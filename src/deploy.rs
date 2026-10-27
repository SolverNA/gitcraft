use crate::{artifacts, gitrepo, rcon::RconClient};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn text<'a>(v: &'a Value, section: &str, key: &str) -> Result<&'a str> {
    v[section][key]
        .as_str()
        .with_context(|| format!("Missing configuration {section}.{key}"))
}
fn seconds(v: &Value, key: &str, default: u64) -> u64 {
    v["deployment"][key].as_u64().unwrap_or(default)
}
fn timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
pub(crate) fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("State needs parent")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut candidate = root.to_path_buf();
    if relative.is_empty() {
        bail!("Empty managed path");
    }
    // Check all ancestors too: a symlinked server root must never redirect writes.
    let mut ancestor = PathBuf::new();
    for component in root.components() {
        ancestor.push(component);
        if ancestor
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            bail!("Symlink in managed root");
        }
    }
    for component in Path::new(relative).components() {
        if let Component::Normal(part) = component {
            candidate.push(part);
        } else {
            bail!("Unsafe managed path");
        }
        if candidate
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            bail!("Symlink in managed destination");
        }
    }
    Ok(candidate)
}
fn config_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| ["yml", "yaml", "properties", "json", "txt", "toml", "conf"].contains(&s))
}
pub(crate) fn collect_configs(
    base: &Path,
    current: &Path,
    prefix: &str,
    top: bool,
    result: &mut BTreeMap<String, PathBuf>,
) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }
    if current.symlink_metadata()?.file_type().is_symlink() {
        bail!("Config symlinks forbidden");
    }
    for entry in fs::read_dir(current)? {
        let path = entry?.path();
        let meta = path.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            bail!("Config symlinks forbidden");
        }
        if meta.is_dir() {
            if top {
                bail!("Server settings must be top-level files");
            }
            collect_configs(base, &path, prefix, false, result)?;
        } else {
            if !meta.is_file() || !config_extension(&path) {
                bail!("Unsupported configuration file");
            }
            match path.extension().and_then(|s| s.to_str()) {
                Some("yml" | "yaml") => {
                    let _: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&path)?)
                        .context("Invalid YAML configuration")?;
                }
                Some("json") => {
                    let _: Value = serde_json::from_slice(&fs::read(&path)?)
                        .context("Invalid JSON configuration")?;
                }
                _ => {}
            }
            let relative = path
                .strip_prefix(base)?
                .to_str()
                .context("Invalid config filename")?;
            if top && relative == "eula.txt" {
                bail!("EULA is a local decision");
            }
            result.insert(format!("{prefix}{relative}"), path);
        }
    }
    Ok(())
}
fn service(action: &str, name: &str) -> Result<Output> {
    if !name.ends_with(".service")
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.@-".contains(c))
        || !name.starts_with(|c: char| c.is_ascii_alphanumeric())
    {
        bail!("Invalid service name");
    }
    let output = gitrepo::run_bounded(
        Command::new("sudo").args(["-n", "/usr/bin/systemctl", action, name]),
        Duration::from_secs(180),
    )?;
    if action != "is-active" && !output.status.success() {
        bail!("systemctl {action} failed");
    }
    Ok(output)
}
pub fn ensure_stopped(config: &Value) -> Result<()> {
    let status = service("is-active", text(config, "deployment", "service")?)?;
    let output = String::from_utf8_lossy(&status.stdout);
    if status.status.success() || !["inactive", "failed"].contains(&output.trim()) {
        bail!(
            "Operation requires explicitly stopped Minecraft service; unknown service status is unsafe"
        );
    }
    Ok(())
}
fn copy_atomic(source: &Path, dest: &Path) -> Result<()> {
    let parent = dest.parent().context("Destination needs parent")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    let mut input = File::open(source)?;
    std::io::copy(&mut input, &mut temp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o640))?;
    }
    temp.as_file().sync_all()?;
    temp.persist(dest)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn write_properties(source: Option<&Path>, dest: &Path, client: &RconClient) -> Result<()> {
    if client
        .password
        .chars()
        .any(|c| c == '\n' || c == '\r' || c == '\0')
    {
        bail!("RCON password contains forbidden characters");
    }
    let mut output = String::new();
    if let Some(source) = source {
        for line in fs::read_to_string(source)?.lines() {
            let key = line
                .trim_start()
                .split(['=', ':', ' ', '\t'])
                .next()
                .unwrap_or("");
            if ![
                "enable-rcon",
                "rcon.password",
                "rcon.port",
                "broadcast-rcon-to-ops",
            ]
            .contains(&key)
            {
                output.push_str(line);
                output.push('\n');
            }
        }
    }
    // Escape Java properties metacharacters; this secret overlay stays local.
    let password = client
        .password
        .replace('\\', "\\\\")
        .replace(' ', "\\ ")
        .replace(':', "\\:")
        .replace('=', "\\=")
        .replace('#', "\\#")
        .replace('!', "\\!");
    output.push_str(&format!(
        "enable-rcon=true\nrcon.password={password}\nrcon.port={}\nbroadcast-rcon-to-ops=false\n",
        client.port
    ));
    let parent = dest.parent().context("Properties need parent")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(output.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.as_file().sync_all()?;
    temp.persist(dest)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
trait Runtime {
    fn checkpoint(&mut self, _phase: &str) -> Result<()> {
        Ok(())
    }
    fn online(&mut self) -> Result<u32>;
    fn command(&mut self, command: &str) -> Result<String>;
    fn service(&mut self, action: &str, name: &str) -> Result<Output>;
    fn snapshot(
        &mut self,
        url: &str,
        branch: &str,
        repository: &Path,
        staging: &Path,
    ) -> Result<(String, PathBuf)> {
        gitrepo::snapshot(url, branch, repository, staging)
    }
    fn download(&mut self, artifact: &artifacts::Artifact, cache: &Path) -> Result<PathBuf> {
        artifacts::download(artifact, cache)
    }
    fn healthy(&mut self, name: &str, timeout: u64) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(timeout);
        loop {
            if self.service("is-active", name)?.status.success() && self.online().is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("Minecraft readiness timed out");
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
}
struct LiveRuntime<'a> {
    client: &'a RconClient,
}
impl Runtime for LiveRuntime<'_> {
    fn online(&mut self) -> Result<u32> {
        self.client.online()
    }
    fn command(&mut self, command: &str) -> Result<String> {
        self.client.command(command)
    }
    fn service(&mut self, action: &str, name: &str) -> Result<Output> {
        service(action, name)
    }
}
fn paths(config: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(text(config, "paths", key)?))
}
pub(crate) fn client(config: &Value) -> Result<RconClient> {
    let env = text(config, "rcon", "password_env")?;
    let password = std::env::var(env).context("RCON secret environment variable missing")?;
    if password.is_empty() {
        bail!("RCON password cannot be empty");
    }
    Ok(RconClient {
        host: config["rcon"]["host"]
            .as_str()
            .unwrap_or("127.0.0.1")
            .into(),
        port: config["rcon"]["port"]
            .as_u64()
            .unwrap_or(25575)
            .try_into()?,
        password,
        timeout: Duration::from_secs(config["rcon"]["timeout_seconds"].as_u64().unwrap_or(5)),
    })
}
fn run_inner(config: &Value, bootstrap: bool) -> Result<Value> {
    let client = client(config)?;
    run_transaction(
        config,
        bootstrap,
        &client,
        &mut LiveRuntime { client: &client },
    )
}
fn run_transaction(
    config: &Value,
    bootstrap: bool,
    client: &RconClient,
    runtime: &mut impl Runtime,
) -> Result<Value> {
    let server = paths(config, "server")?;
    let state_dir = paths(config, "state")?;
    let backup_root = paths(config, "backups")?;
    for key in [
        "server",
        "state",
        "cache",
        "staging",
        "backups",
        "repository",
    ] {
        let path = paths(config, key)?;
        safe_path(&path, ".path-check")?;
        fs::create_dir_all(path)?;
    }
    let state_file = state_dir.join("deployment.json");
    let state: Value = if state_file.exists() {
        serde_json::from_slice(&fs::read(&state_file)?)?
    } else {
        json!({})
    };
    let name = text(config, "deployment", "service")?;
    if bootstrap {
        if state_file.exists() {
            bail!("Bootstrap forbidden after a successful deployment");
        }
        let status = runtime.service("is-active", name)?;
        let status_text = String::from_utf8_lossy(&status.stdout);
        if status.status.success() || !["inactive", "failed"].contains(&status_text.trim()) {
            bail!("Bootstrap requires explicitly inactive or failed Minecraft service");
        }
    }
    let (commit, repo) = runtime.snapshot(
        text(config, "repository", "url")?,
        config["repository"]["branch"].as_str().unwrap_or("main"),
        &paths(config, "repository")?,
        &paths(config, "staging")?,
    )?;
    if state["commit"].as_str() == Some(&commit) {
        return Ok(json!({"status":"unchanged","commit":commit}));
    }
    crate::policy::validate_repository(&repo)?;
    let mut sources = BTreeMap::new();
    for (folder, prefix, top) in [
        ("server-settings", "", true),
        ("plugins-configs", "plugins/", false),
        ("config", "config/", false),
        ("defaultconfigs", "defaultconfigs/", false),
    ] {
        let base = repo.join(folder);
        collect_configs(&base, &base, prefix, top, &mut sources)?;
    }
    let manifest = artifacts::load_manifest(&repo)?;
    if !manifest.iter().any(|a| !a.destination.contains('/')) {
        bail!("An engine manifest is required");
    }
    for artifact in &manifest {
        let cached = runtime.download(artifact, &paths(config, "cache")?)?;
        if sources
            .insert(artifact.destination.clone(), cached)
            .is_some()
        {
            bail!("Duplicate managed destination");
        }
    }
    let previous: Vec<String> = if state.get("managed_files").is_some() {
        serde_json::from_value(state["managed_files"].clone())?
    } else {
        Vec::new()
    };
    let mut owned: BTreeSet<String> = previous
        .into_iter()
        .chain(sources.keys().cloned())
        .collect();
    owned.insert("server.properties".into());
    if bootstrap {
        owned.insert("eula.txt".into());
    }
    for relative in &owned {
        let parts: Vec<_> = Path::new(relative).components().collect();
        if parts.len() > 1
            && !relative.starts_with("plugins/")
            && !relative.starts_with("mods/")
            && !relative.starts_with("config/")
            && !relative.starts_with("defaultconfigs/")
        {
            bail!("Invalid ownership state");
        }
        if parts.len() == 1 && !relative.ends_with(".jar") && !config_extension(Path::new(relative))
        {
            bail!("Invalid ownership extension");
        }
        let dest = safe_path(&server, relative)?;
        if dest.exists() && !dest.is_file() {
            bail!("Managed destination is not a regular file");
        }
    }
    if !bootstrap
        && !wait_empty(
            || runtime.online(),
            Duration::from_secs(seconds(config, "empty_grace_seconds", 60)),
        )?
    {
        return Ok(json!({"status":"waiting_for_players","commit":commit}));
    }
    // Journal is durable before stop or any managed mutation; recovery never touches worlds.
    let backup = backup_root.join(format!("{commit}-{}", timestamp()));
    fs::create_dir_all(&backup)?;
    let mut journal = json!({"phase":"prepared","server":server,"backup":backup,"commit":commit,"owned":owned,"previous_state":if state_file.exists(){state.clone()}else{Value::Null},"backup_hashes":{},"may_have_started":false,"bootstrap":bootstrap});
    crate::recovery::save(config, &journal)?;
    runtime.checkpoint("prepared")?;
    // Grace does not prevent a new connection between this check and service stop.
    if !bootstrap {
        runtime.command("save-all flush")?;
        runtime.service("stop", name)?;
    }
    crate::recovery::phase(config, &mut journal, "stopped")?;
    runtime.checkpoint("stopped")?;
    let mut existed = BTreeSet::new();
    let backup_result = (|| -> Result<()> {
        for relative in &owned {
            let dest = safe_path(&server, relative)?;
            if dest.exists() {
                let target = safe_path(&backup, relative)?;
                fs::create_dir_all(target.parent().unwrap())?;
                copy_atomic(&dest, &target)?;
                existed.insert(relative.clone());
            }
        }
        atomic_json(
            &backup.join("backup.json"),
            &json!({"commit":state["commit"],"files":existed}),
        )
    })();
    if let Err(error) = backup_result {
        if !bootstrap {
            runtime.service("start", name)?;
        }
        crate::recovery::finish(config, &journal)?;
        return Err(error.context("Backup failed before mutation"));
    }
    let mut backup_hashes = BTreeMap::new();
    for relative in &existed {
        backup_hashes.insert(
            relative.clone(),
            artifacts::sha256(&safe_path(&backup, relative)?)?,
        );
    }
    journal["backup_hashes"] = json!(backup_hashes);
    crate::recovery::phase(config, &mut journal, "backed_up")?;
    runtime.checkpoint("backed_up")?;
    crate::recovery::phase(config, &mut journal, "applying")?;
    runtime.checkpoint("applying")?;
    let applied = (|| -> Result<()> {
        for relative in &owned {
            let dest = safe_path(&server, relative)?;
            if relative == "server.properties" {
                write_properties(sources.get(relative).map(PathBuf::as_path), &dest, client)?;
            } else if relative == "eula.txt" && !bootstrap {
                continue;
            } else if relative == "eula.txt" && bootstrap {
                fs::write(&dest, "eula=true\n")?;
            } else if let Some(source) = sources.get(relative) {
                copy_atomic(source, &dest)?;
            } else if dest.exists() {
                fs::remove_file(&dest)?;
                File::open(dest.parent().unwrap())?.sync_all()?;
            }
            runtime.checkpoint("file_applied")?;
        }
        crate::recovery::phase(config, &mut journal, "starting")?;
        runtime.checkpoint("starting")?;
        runtime.service("start", name)?;
        runtime.healthy(name, seconds(config, "health_timeout_seconds", 120))?;
        crate::recovery::phase(config, &mut journal, "healthy")?;
        runtime.checkpoint("healthy")?;
        Ok(())
    })();
    if let Err(error) = applied {
        crate::recovery::phase(config, &mut journal, "rolling_back")?;
        let rollback = (|| -> Result<()> {
            runtime.service("stop", name)?;
            restore_owned(&server, &backup, &owned, &existed)?;
            if !bootstrap {
                runtime.service("start", name)?;
                runtime.healthy(name, seconds(config, "health_timeout_seconds", 120))?;
            }
            Ok(())
        })();
        atomic_json(
            &state_dir.join("rollback.json"),
            &json!({"status":if rollback.is_ok(){"restored"}else{"failed"},"backup":backup,"timestamp":timestamp()}),
        )?;
        if rollback.is_err() {
            bail!("Deployment failed and rollback failed; inspect local journal and backup");
        }
        crate::recovery::finish(config, &journal)?;
        return Err(error.context("Deployment failed; managed files restored (world/plugin data migrations are not rolled back)"));
    }
    let mut managed: BTreeSet<String> = sources.keys().cloned().collect();
    managed.insert("server.properties".into());
    if bootstrap || owned.contains("eula.txt") {
        managed.insert("eula.txt".into());
    }
    let versions: BTreeMap<_, _> = manifest.iter().map(|a| (&a.name, &a.version)).collect();
    let result = json!({"status":"deployed","commit":commit,"managed_files":managed,"backup":backup,"versions":versions});
    atomic_json(&state_file, &result)?;
    crate::recovery::phase(config, &mut journal, "committed")?;
    runtime.checkpoint("committed")?;
    crate::recovery::finish(config, &journal)?;
    Ok(result)
}
fn execute(config: &Value, bootstrap: bool) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    let config = &config;
    let state = paths(config, "state")?;
    safe_path(&state, "deploy.lock")?;
    fs::create_dir_all(&state)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("deploy.lock"))?;
    lock.try_lock_exclusive()
        .context("Another deployment is running")?;
    let result = if let Some(journal) = crate::recovery::pending(config)? {
        ensure_stopped(config)?;
        crate::recovery::restore_stopped(config, &journal, false)
    } else {
        run_inner(config, bootstrap)
    };
    match &result {
        Ok(value) => atomic_json(&state.join("status.json"), value)?,
        Err(_) => atomic_json(
            &state.join("status.json"),
            &json!({"status":"error","message":"Deployment failed; inspect local journal","timestamp":timestamp()}),
        )?,
    }
    FileExt::unlock(&lock)?;
    result
}
pub fn run(config: &Value) -> Result<Value> {
    execute(config, false)
}
pub fn bootstrap(config: &Value, accept_eula: bool) -> Result<Value> {
    if !accept_eula {
        bail!("Bootstrap requires explicit --accept-eula");
    }
    execute(config, true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_rejected() {
        let root = tempfile::tempdir().unwrap();
        assert!(safe_path(root.path(), "../world/level.dat").is_err());
        assert!(safe_path(root.path(), "/tmp/outside").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_rejected() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/tmp", root.path().join("plugins")).unwrap();
        assert!(safe_path(root.path(), "plugins/test.jar").is_err());
    }
    #[test]
    fn bootstrap_requires_eula() {
        assert!(bootstrap(&json!({}), false).is_err());
    }
    #[test]
    fn local_rcon_overlay() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(
            &source,
            "motd=hello\nrcon.password=unsafe\nenable-rcon=false\n",
        )
        .unwrap();
        let dest = root.path().join("properties");
        let client = RconClient {
            host: "localhost".into(),
            port: 25575,
            password: "secret".into(),
            timeout: Duration::from_secs(1),
        };
        write_properties(Some(&source), &dest, &client).unwrap();
        let output = fs::read_to_string(dest).unwrap();
        assert!(output.contains("motd=hello"));
        assert!(!output.contains("unsafe"));
        assert!(output.contains("rcon.password=secret"));
    }
}

fn wait_empty(mut online: impl FnMut() -> Result<u32>, grace: Duration) -> Result<bool> {
    if online()? != 0 {
        return Ok(false);
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        thread::sleep(
            Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now())),
        );
        if online()? != 0 {
            return Ok(false);
        }
    }
    Ok(online()? == 0)
}
fn restore_owned(
    server: &Path,
    backup: &Path,
    owned: &BTreeSet<String>,
    existed: &BTreeSet<String>,
) -> Result<()> {
    for relative in owned {
        let dest = safe_path(server, relative)?;
        if existed.contains(relative) {
            copy_atomic(&safe_path(backup, relative)?, &dest)?;
        } else if dest.exists() {
            fs::remove_file(&dest)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod transaction_tests {
    use super::*;
    #[test]
    fn online_failure_and_player_arrival_defer() {
        assert!(wait_empty(|| anyhow::bail!("unavailable"), Duration::ZERO).is_err());
        assert!(!wait_empty(|| Ok(2), Duration::ZERO).unwrap());
        let mut counts = vec![0, 1].into_iter();
        assert!(!wait_empty(|| Ok(counts.next().unwrap()), Duration::ZERO).unwrap());
    }
    #[test]
    fn rollback_restores_only_owned_files() {
        let root = tempfile::tempdir().unwrap();
        let server = root.path().join("server");
        let backup = root.path().join("backup");
        fs::create_dir_all(server.join("plugins")).unwrap();
        fs::create_dir_all(backup.join("plugins")).unwrap();
        fs::write(server.join("plugins/old.jar"), "new").unwrap();
        fs::write(backup.join("plugins/old.jar"), "old").unwrap();
        fs::write(server.join("plugins/new.jar"), "new").unwrap();
        fs::write(server.join("plugins/foreign.jar"), "foreign").unwrap();
        fs::create_dir_all(server.join("world")).unwrap();
        fs::write(server.join("world/level.dat"), "world").unwrap();
        let owned = BTreeSet::from(["plugins/old.jar".into(), "plugins/new.jar".into()]);
        let existed = BTreeSet::from(["plugins/old.jar".into()]);
        restore_owned(&server, &backup, &owned, &existed).unwrap();
        assert_eq!(
            fs::read_to_string(server.join("plugins/old.jar")).unwrap(),
            "old"
        );
        assert!(!server.join("plugins/new.jar").exists());
        assert_eq!(
            fs::read_to_string(server.join("plugins/foreign.jar")).unwrap(),
            "foreign"
        );
        assert_eq!(
            fs::read_to_string(server.join("world/level.dat")).unwrap(),
            "world"
        );
    }
    #[test]
    fn concurrent_lock_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lock");
        let one = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        let two = OpenOptions::new().write(true).open(&path).unwrap();
        one.try_lock_exclusive().unwrap();
        assert!(two.try_lock_exclusive().is_err());
        FileExt::unlock(&one).unwrap();
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    struct FakeRuntime {
        repo: PathBuf,
        commit: String,
        online: Option<u32>,
        actions: Vec<String>,
        crash_after: Option<String>,
        fail_first_readiness: bool,
        readiness_calls: usize,
        state: PathBuf,
    }
    impl Runtime for FakeRuntime {
        fn checkpoint(&mut self, phase: &str) -> Result<()> {
            if self.crash_after.as_deref() == Some(phase) {
                panic!("simulated process interruption at {phase}");
            }
            Ok(())
        }
        fn online(&mut self) -> Result<u32> {
            self.online.context("RCON unavailable")
        }
        fn command(&mut self, _: &str) -> Result<String> {
            Ok("Saved".into())
        }
        fn service(&mut self, action: &str, _: &str) -> Result<Output> {
            self.actions.push(action.into());
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: b"active\n".to_vec(),
                stderr: vec![],
            })
        }
        fn snapshot(
            &mut self,
            _: &str,
            _: &str,
            _: &Path,
            staging: &Path,
        ) -> Result<(String, PathBuf)> {
            let dest = staging.join(&self.commit);
            let status = Command::new("git")
                .arg("clone")
                .arg("--quiet")
                .arg("--no-local")
                .arg(&self.repo)
                .arg(&dest)
                .status()?;
            if !status.success() {
                bail!("Fixture clone failed");
            }
            Ok((self.commit.clone(), dest))
        }
        fn healthy(&mut self, _: &str, _: u64) -> Result<()> {
            // Installed state must still describe the previous deployment during readiness.
            let current: Value =
                serde_json::from_slice(&fs::read(self.state.join("deployment.json"))?)?;
            assert_eq!(current["commit"], "previous");
            self.readiness_calls += 1;
            if self.fail_first_readiness && self.readiness_calls == 1 {
                bail!("Readiness failed");
            }
            Ok(())
        }
    }
    struct Fixture {
        _root: tempfile::TempDir,
        config: Value,
        client: RconClient,
        runtime: FakeRuntime,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("source");
            fs::create_dir_all(repo.join("requirements")).unwrap();
            fs::create_dir_all(repo.join("server-settings")).unwrap();
            fs::write(repo.join("server-settings/server.properties"), "motd=new\n").unwrap();
            let digest = format!("{:x}", sha2::Sha256::digest(b"new jar"));
            fs::write(repo.join("requirements/engine.json"),serde_json::to_vec(&json!({"name":"engine","filename":"server.jar","pinned":{"url":"https://example.org/server.jar","sha256":digest,"version":"2"}})).unwrap()).unwrap();
            for args in [
                vec!["init", "--quiet"],
                vec!["add", "."],
                vec![
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.org",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
            ] {
                assert!(
                    Command::new("git")
                        .arg("-C")
                        .arg(&repo)
                        .args(args)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let commit = String::from_utf8(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(["rev-parse", "HEAD"])
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_string();
            let mut path_values = serde_json::Map::new();
            for key in [
                "server",
                "state",
                "cache",
                "staging",
                "backups",
                "repository",
            ] {
                let path = root.path().join(key);
                fs::create_dir_all(&path).unwrap();
                path_values.insert(key.into(), json!(path));
            }
            let config = json!({"paths":path_values,"repository":{"url":"https://example.org/repo.git","branch":"main"},"deployment":{"service":"minecraft.service","empty_grace_seconds":0,"health_timeout_seconds":1},"rcon":{"host":"127.0.0.1","port":25575,"password_env":"UNUSED","timeout_seconds":1}});
            let server = paths(&config, "server").unwrap();
            fs::create_dir_all(server.join("plugins")).unwrap();
            fs::create_dir_all(server.join("world")).unwrap();
            fs::write(server.join("server.jar"), "old jar").unwrap();
            fs::write(server.join("server.properties"), "motd=old\n").unwrap();
            fs::write(server.join("plugins/old.jar"), "owned").unwrap();
            fs::write(server.join("plugins/foreign.jar"), "foreign").unwrap();
            fs::write(server.join("world/level.dat"), "world").unwrap();
            fs::write(server.join("eula.txt"), "eula=true\n").unwrap();
            let state = paths(&config, "state").unwrap();
            atomic_json(&state.join("deployment.json"),&json!({"commit":"previous","managed_files":["server.jar","server.properties","plugins/old.jar","eula.txt"]})).unwrap();
            fs::write(
                paths(&config, "cache")
                    .unwrap()
                    .join(format!("{digest}.jar")),
                "new jar",
            )
            .unwrap();
            let client = RconClient {
                host: "127.0.0.1".into(),
                port: 25575,
                password: "local-secret".into(),
                timeout: Duration::from_secs(1),
            };
            Self {
                _root: root,
                config,
                client,
                runtime: FakeRuntime {
                    repo,
                    commit,
                    online: Some(0),
                    actions: vec![],
                    crash_after: None,
                    fail_first_readiness: false,
                    readiness_calls: 0,
                    state,
                },
            }
        }
    }
    use sha2::Digest;
    #[test]
    fn durable_journal_recovers_every_interruption_phase_without_restarting() {
        for phase in [
            "prepared",
            "stopped",
            "backed_up",
            "applying",
            "file_applied",
            "starting",
            "healthy",
            "committed",
        ] {
            let mut fixture = Fixture::new();
            fixture.runtime.crash_after = Some(phase.into());
            let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_transaction(
                    &fixture.config,
                    false,
                    &fixture.client,
                    &mut fixture.runtime,
                )
            }));
            assert!(interrupted.is_err());
            // Reconstruct solely from durable disk state, as a new process would.
            let journal = crate::recovery::pending(&fixture.config).unwrap().unwrap();
            if ["starting", "healthy", "committed"].contains(&phase) {
                assert!(
                    crate::recovery::restore_stopped(&fixture.config, &journal, false).is_err()
                );
            }
            let result = crate::recovery::restore_stopped(&fixture.config, &journal, true).unwrap();
            assert_eq!(result["service_restarted"], false);
            assert!(crate::recovery::pending(&fixture.config).unwrap().is_none());
            let server = paths(&fixture.config, "server").unwrap();
            assert_eq!(
                fs::read_to_string(server.join("server.jar")).unwrap(),
                "old jar"
            );
            assert_eq!(
                fs::read_to_string(server.join("server.properties")).unwrap(),
                "motd=old\n"
            );
            assert_eq!(
                fs::read_to_string(server.join("world/level.dat")).unwrap(),
                "world"
            );
            let state: Value = serde_json::from_slice(
                &fs::read(fixture.runtime.state.join("deployment.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(state["commit"], "previous");
        }
    }
    #[test]
    fn corrupt_backup_refuses_recovery_before_any_restore() {
        let mut fixture = Fixture::new();
        fixture.runtime.crash_after = Some("starting".into());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_transaction(
                &fixture.config,
                false,
                &fixture.client,
                &mut fixture.runtime,
            )
        }));
        let journal = crate::recovery::pending(&fixture.config).unwrap().unwrap();
        fs::write(
            Path::new(journal["backup"].as_str().unwrap()).join("server.jar"),
            "corrupted",
        )
        .unwrap();
        assert!(crate::recovery::restore_stopped(&fixture.config, &journal, true).is_err());
        assert_eq!(
            fs::read_to_string(paths(&fixture.config, "server").unwrap().join("server.jar"))
                .unwrap(),
            "new jar"
        );
        assert!(crate::recovery::pending(&fixture.config).unwrap().is_some());
    }
    #[test]
    fn successful_transaction_commits_after_readiness_and_retains_eula() {
        let mut fixture = Fixture::new();
        let result = run_transaction(
            &fixture.config,
            false,
            &fixture.client,
            &mut fixture.runtime,
        )
        .unwrap();
        assert_eq!(result["status"], "deployed");
        assert_eq!(fixture.runtime.readiness_calls, 1);
        let server = paths(&fixture.config, "server").unwrap();
        assert_eq!(
            fs::read_to_string(server.join("server.jar")).unwrap(),
            "new jar"
        );
        assert!(
            fs::read_to_string(server.join("server.properties"))
                .unwrap()
                .contains("local-secret")
        );
        assert!(server.join("eula.txt").exists());
        assert!(!server.join("plugins/old.jar").exists());
        assert_eq!(
            fs::read_to_string(server.join("plugins/foreign.jar")).unwrap(),
            "foreign"
        );
        assert_eq!(
            fs::read_to_string(server.join("world/level.dat")).unwrap(),
            "world"
        );
        let state: Value = serde_json::from_slice(
            &fs::read(fixture.runtime.state.join("deployment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["commit"], fixture.runtime.commit);
    }
    #[test]
    fn failed_readiness_restores_jar_configs_and_previous_commit() {
        let mut fixture = Fixture::new();
        fixture.runtime.fail_first_readiness = true;
        assert!(
            run_transaction(
                &fixture.config,
                false,
                &fixture.client,
                &mut fixture.runtime
            )
            .is_err()
        );
        let server = paths(&fixture.config, "server").unwrap();
        assert_eq!(
            fs::read_to_string(server.join("server.jar")).unwrap(),
            "old jar"
        );
        assert_eq!(
            fs::read_to_string(server.join("server.properties")).unwrap(),
            "motd=old\n"
        );
        assert!(server.join("plugins/old.jar").exists());
        assert_eq!(fixture.runtime.readiness_calls, 2);
        assert_eq!(
            fixture.runtime.actions,
            vec!["stop", "start", "stop", "start"]
        );
        let state: Value = serde_json::from_slice(
            &fs::read(fixture.runtime.state.join("deployment.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["commit"], "previous");
    }
    #[test]
    fn malformed_yaml_never_stops_service() {
        let mut fixture = Fixture::new();
        fs::write(
            fixture.runtime.repo.join("server-settings/spigot.yml"),
            "broken: [\n",
        )
        .unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&fixture.runtime.repo)
                .args(["add", "."])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&fixture.runtime.repo)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.org",
                    "commit",
                    "--quiet",
                    "-m",
                    "invalid fixture"
                ])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            run_transaction(
                &fixture.config,
                false,
                &fixture.client,
                &mut fixture.runtime
            )
            .is_err()
        );
        assert!(fixture.runtime.actions.is_empty());
    }
    #[test]
    fn missing_properties_template_still_writes_local_rcon_overlay() {
        let mut fixture = Fixture::new();
        fs::remove_file(
            fixture
                .runtime
                .repo
                .join("server-settings/server.properties"),
        )
        .unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&fixture.runtime.repo)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.org",
                    "commit",
                    "--quiet",
                    "-am",
                    "remove properties fixture"
                ])
                .status()
                .unwrap()
                .success()
        );
        run_transaction(
            &fixture.config,
            false,
            &fixture.client,
            &mut fixture.runtime,
        )
        .unwrap();
        let properties = paths(&fixture.config, "server")
            .unwrap()
            .join("server.properties");
        assert!(
            fs::read_to_string(properties)
                .unwrap()
                .contains("rcon.password=local-secret")
        );
    }
    #[test]
    fn unavailable_rcon_never_stops_service() {
        let mut fixture = Fixture::new();
        fixture.runtime.online = None;
        assert!(
            run_transaction(
                &fixture.config,
                false,
                &fixture.client,
                &mut fixture.runtime
            )
            .is_err()
        );
        assert!(fixture.runtime.actions.is_empty());
    }
    #[test]
    fn online_players_never_stop_service() {
        let mut fixture = Fixture::new();
        fixture.runtime.online = Some(1);
        assert_eq!(
            run_transaction(
                &fixture.config,
                false,
                &fixture.client,
                &mut fixture.runtime
            )
            .unwrap()["status"],
            "waiting_for_players"
        );
        assert!(fixture.runtime.actions.is_empty());
    }
}
