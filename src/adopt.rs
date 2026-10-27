//! Adopt an existing unit without rewriting its launch command, user, Java or credentials.
use crate::{artifacts, deploy, gitrepo, recovery};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path, process::Command, time::Duration};
pub(crate) fn show_unit(config: &Value) -> Result<Value> {
    let name = config["deployment"]["service"]
        .as_str()
        .context("Service required")?;
    let output = gitrepo::run_bounded(
        Command::new("/usr/bin/systemctl").args([
            "show",
            "--no-pager",
            "--property=Id,User,Group,WorkingDirectory,ExecStart,FragmentPath,MainPID,ActiveState",
            name,
        ]),
        Duration::from_secs(30),
    )?;
    if !output.status.success() {
        bail!("Cannot discover existing unit");
    }
    let mut unit = parse_show(&String::from_utf8(output.stdout)?)?;
    let object = bus_json(&[
        "call",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "GetUnit",
        "s",
        name,
    ])?;
    let object = object["data"][0]
        .as_str()
        .context("Cannot resolve structured systemd unit")?;
    let commands = bus_json(&[
        "get-property",
        "org.freedesktop.systemd1",
        object,
        "org.freedesktop.systemd1.Service",
        "ExecStartEx",
    ])?;
    attach_structured_launcher(&mut unit, &commands)?;
    Ok(unit)
}
fn attach_structured_launcher(unit: &mut Value, commands: &Value) -> Result<()> {
    if commands["type"] != "a(sasasttttuii)" {
        bail!("Unsupported structured ExecStartEx signature; no fallback to ambiguous text");
    }
    let records = commands["data"]
        .as_array()
        .context("Structured launcher unavailable; explicit direct Java launch required")?;
    if records.len() != 1 {
        bail!("Exactly one structured ExecStart is required");
    }
    let record = records[0]
        .as_array()
        .context("Malformed structured launcher")?;
    if record.len() != 10 {
        bail!("Malformed structured ExecStartEx record");
    }
    unit["LaunchPath"] = record[0].clone();
    unit["LaunchArgv"] = record[1].clone();
    unit["LaunchFlags"] = record[2].clone();
    Ok(())
}

fn bus_json(args: &[&str]) -> Result<Value> {
    let output = gitrepo::run_bounded(
        Command::new("/usr/bin/busctl")
            .args(["--system", "--json=short"])
            .args(args),
        Duration::from_secs(30),
    ).context("busctl discovery unavailable; install a supported systemd/busctl package; deployment was not applied")?;
    if !output.status.success() {
        bail!(
            "Structured systemd launcher discovery failed; supported system bus and ExecStartEx are required; no deployment was applied"
        );
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| anyhow::anyhow!("Malformed structured systemd response; values omitted"))
}
fn parse_show(output: &str) -> Result<Value> {
    let mut props = serde_json::Map::new();
    for line in output.lines() {
        if let Some((key, value)) = line.split_once('=') {
            props.insert(key.into(), json!(value));
        }
    }
    for required in [
        "Id",
        "WorkingDirectory",
        "ExecStart",
        "FragmentPath",
        "User",
    ] {
        if props[required].as_str().is_none_or(str::is_empty) {
            bail!("Existing unit must explicitly define {required}; no unsafe launch inference");
        }
    }
    Ok(Value::Object(props))
}
fn fingerprint(unit: &Value) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(unit)?);
    let path = Path::new(unit["FragmentPath"].as_str().context("Missing unit path")?);
    hash.update(fs::read(path).context("Cannot read existing unit fragment")?);
    Ok(format!("{:x}", hash.finalize()))
}
fn stable_unit(unit: &Value) -> Value {
    let mut unit = unit.clone();
    if let Some(object) = unit.as_object_mut() {
        object.remove("MainPID");
        object.remove("ActiveState");
    }
    unit
}
fn launch_engine(server: &Path, unit: &Value) -> Result<String> {
    launch_engine_path(server, unit, true)
}
fn launch_engine_path(server: &Path, unit: &Value, require_existing: bool) -> Result<String> {
    let executable = unit["LaunchPath"]
        .as_str()
        .context("Exact structured launcher path is required")?;
    if !Path::new(executable).is_absolute()
        || Path::new(executable).file_name().and_then(|s| s.to_str()) != Some("java")
    {
        bail!(
            "Unsupported launcher: direct absolute java -jar required; shell/Forge @args launchers are not inferred"
        );
    }
    if !unit["LaunchFlags"]
        .as_array()
        .is_some_and(|flags| flags.is_empty())
    {
        bail!(
            "Prefixed systemd executable flags are unsupported; review an explicit unprefixed Java launch"
        );
    }
    let values = unit["LaunchArgv"]
        .as_array()
        .context("Exact structured launcher argv required")?;
    let words: Vec<&str> = values
        .iter()
        .map(|v| v.as_str().context("Malformed launcher argv"))
        .collect::<Result<_>>()?;
    if words.first().copied() != Some(executable)
        || words
            .iter()
            .any(|w| w.starts_with('@') || w.contains(['$', '%', '\\', '\'', '"']))
        || words.iter().filter(|w| **w == "-jar").count() != 1
    {
        bail!(
            "Ambiguous launcher substitutions, escaped arguments or argument files unsupported; use reviewed explicit Java flags and one -jar target"
        );
    }
    let jar = words
        .windows(2)
        .find(|w| w[0] == "-jar")
        .map(|w| w[1])
        .context("Unsupported launcher: direct -jar required")?;
    let jar_path = Path::new(jar);
    let server = fs::canonicalize(server)?;
    let relative = if jar_path.is_absolute() {
        jar_path
            .strip_prefix(&server)
            .context("Launched JAR is outside server directory")?
            .to_path_buf()
    } else {
        jar_path.to_path_buf()
    };
    let mut components = relative.components();
    let first = components.next().context("Missing engine filename")?;
    let filename = match first {
        std::path::Component::CurDir => components.next(),
        std::path::Component::Normal(_) => Some(first),
        _ => None,
    }
    .context("Unsafe engine path")?;
    if components.next().is_some() {
        bail!("Engine must be a top-level JAR without parent traversal");
    }
    let filename = filename
        .as_os_str()
        .to_str()
        .context("Non-UTF8 engine filename")?;
    if !filename.ends_with(".jar")
        || !filename
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || !filename
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        bail!("Ambiguous or escaped engine filename is unsupported");
    }
    let target = deploy::safe_path(&server, filename)?;
    if require_existing && !target.is_file() {
        bail!("Launched JAR does not exist");
    }
    Ok(filename.into())
}
/// Validate the effective launcher without rewriting it or requiring a bootstrap JAR to exist.
pub(crate) fn verify_launcher(config: &Value, unit: &Value, destination: &str) -> Result<()> {
    if unit["Id"] != config["deployment"]["service"] {
        bail!("Discovered unit identity differs from configured service");
    }
    let server = Path::new(
        config["paths"]["server"]
            .as_str()
            .context("Server path required")?,
    );
    let working = unit["WorkingDirectory"]
        .as_str()
        .context("Unit WorkingDirectory required")?;
    if fs::canonicalize(working)? != fs::canonicalize(server)? {
        bail!("Unit WorkingDirectory differs from configured server");
    }
    if launch_engine_path(server, unit, false)? != destination {
        bail!(
            "Pinned engine destination differs from actual systemd -jar target; unit was not rewritten and deployment was not applied"
        );
    }
    Ok(())
}
fn build_plan(config: &Value, server: &Path, unit: &Value) -> Result<Value> {
    deploy::safe_path(server, "server.properties")?;
    let configured = Path::new(
        config["paths"]["server"]
            .as_str()
            .context("Server path required")?,
    );
    if fs::canonicalize(server)? != fs::canonicalize(configured)? {
        bail!("Explicit server path differs from configuration");
    }
    if fs::canonicalize(
        unit["WorkingDirectory"]
            .as_str()
            .context("Unit working directory required")?,
    )? != fs::canonicalize(server)?
    {
        bail!("Existing unit WorkingDirectory differs from explicit server path");
    }
    let stable = stable_unit(unit);
    let exec = unit["ExecStart"].as_str().context("ExecStart required")?;
    let executable = exec
        .split("path=")
        .nth(1)
        .and_then(|s| s.split(" ;").next())
        .unwrap_or("unresolved");
    let launch = launch_engine(server, unit);
    let (supported, engine, reason) = match launch {
        Ok(engine) => (true, Some(engine), None),
        Err(error) => (false, None, Some(error.to_string())),
    };
    let mut display = stable.clone();
    display["ExecStart"] = json!("[redacted: exact effective command is fingerprinted]");
    display["LaunchArgv"] = json!("[redacted: exact arguments are fingerprinted]");
    Ok(
        json!({"kind":"adopt-plan-v1","server":server,"service":config["deployment"]["service"],"unit":display,"unit_sha256":fingerprint(&stable)?,"exec_start_sha256":format!("{:x}",Sha256::digest(exec.as_bytes())),"discovered_executable":executable,"launch_supported":supported,"engine_filename":engine,"unsupported_reason":reason,"active_state":unit["ActiveState"],"actions":["verify approved repository commit and runtime hashes","verify existing RCON credentials without resetting","seed initial managed ownership and verified local artifact receipts"],"preserves":["existing systemd unit","launch command and Java","worlds and plugin databases","existing RCON credentials"],"requires_review":true}),
    )
}
pub fn plan(config: &Value, server: &Path) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    build_plan(&config, server, &show_unit(&config)?)
}
fn git_head(repo: &Path) -> Result<String> {
    let output = gitrepo::run_bounded(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", "HEAD^{commit}"]),
        Duration::from_secs(30),
    )?;
    if !output.status.success() {
        bail!("Cannot resolve local reviewed commit");
    }
    let dirty = gitrepo::run_bounded(
        Command::new("git").arg("-C").arg(repo).args([
            "status",
            "--porcelain",
            "--untracked-files=all",
        ]),
        Duration::from_secs(30),
    )?;
    if !dirty.status.success() || !dirty.stdout.is_empty() {
        bail!("Reviewed repository must have a clean worktree");
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn properties_without_local(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with(['#', '!']) {
                return None;
            }
            let (key, value) = line.split_once(['=', ':']).unwrap_or((line, ""));
            let key = key.trim();
            if [
                "enable-rcon",
                "rcon.password",
                "rcon.port",
                "broadcast-rcon-to-ops",
            ]
            .contains(&key)
            {
                None
            } else {
                Some((key.into(), value.trim().into()))
            }
        })
        .collect()
}
fn apply_verified(
    config: &Value,
    plan: &Value,
    repo: &Path,
    commit: &str,
    unit: &Value,
    verify_rcon: impl Fn() -> Result<u32>,
) -> Result<Value> {
    if plan["kind"] != "adopt-plan-v1"
        || plan["service"] != config["deployment"]["service"]
        || plan["server"] != config["paths"]["server"]
    {
        bail!("Adopt plan/config mismatch");
    }
    if fingerprint(&stable_unit(unit))?
        != plan["unit_sha256"]
            .as_str()
            .context("Plan unit fingerprint missing")?
    {
        bail!("Existing unit changed since plan; inspect a new plan");
    }
    let server = Path::new(config["paths"]["server"].as_str().unwrap());
    let state = Path::new(
        config["paths"]["state"]
            .as_str()
            .context("State required")?,
    );
    if state.join("deployment.json").exists() || recovery::pending(config)?.is_some() {
        bail!("Server already managed or recovery pending");
    }
    let launched_engine = launch_engine(server, unit)?;
    let manifest = artifacts::load_manifest(repo)?;
    if !manifest.iter().any(|a| a.destination == launched_engine) {
        bail!("Adoption engine pin must match the existing unit direct -jar target");
    }
    let mut sources = BTreeMap::new();
    for (folder, prefix, top) in [
        ("server-settings", "", true),
        ("plugins-configs", "plugins/", false),
        ("config", "config/", false),
        ("defaultconfigs", "defaultconfigs/", false),
    ] {
        let base = repo.join(folder);
        deploy::collect_configs(&base, &base, prefix, top, &mut sources)?;
    }
    let mut hashes = BTreeMap::new();
    for (relative, source) in &sources {
        let target = deploy::safe_path(server, relative)?;
        let equal = if relative == "server.properties" {
            properties_without_local(&fs::read_to_string(source)?)
                == properties_without_local(&fs::read_to_string(&target)?)
        } else {
            artifacts::sha256(source)? == artifacts::sha256(&target)?
        };
        if !equal {
            bail!("Runtime config differs from reviewed repository: {relative}");
        }
        hashes.insert(relative.clone(), artifacts::sha256(&target)?);
    }
    for artifact in &manifest {
        let target = deploy::safe_path(server, &artifact.destination)?;
        if artifacts::sha256(&target)? != artifact.sha256 {
            bail!(
                "Runtime JAR differs from reviewed pin: {}",
                artifact.destination
            );
        }
        hashes.insert(artifact.destination.clone(), artifact.sha256.clone());
    }
    // Existing credentials are tested, never rewritten. Players may remain online: adoption changes no runtime files.
    let online = verify_rcon()?;
    for artifact in &manifest {
        artifacts::import_local(
            &deploy::safe_path(server, &artifact.destination)?,
            Path::new(
                config["paths"]["cache"]
                    .as_str()
                    .context("Cache required")?,
            ),
            &artifact.sha256,
        )?;
    }
    let result = json!({"status":"adopted","commit":commit,"managed_files":hashes.keys().collect::<Vec<_>>(),"managed_hashes":hashes,"unit_sha256":plan["unit_sha256"],"existing_unit":true,"online":online,"rcon_credentials_preserved":true});
    recovery::durable_json(&state.join("deployment.json"), &result)?;
    recovery::durable_json(&state.join("status.json"), &result)?;
    Ok(result)
}
pub fn apply(config: &Value, plan: &Value, repo: &Path, expected_commit: &str) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    if git_head(repo)? != expected_commit {
        bail!("Reviewed HEAD differs from expected commit");
    }
    crate::policy::validate_repository(repo)?;
    let state = Path::new(config["paths"]["state"].as_str().unwrap());
    fs::create_dir_all(state)?;
    let _lock = deploy::lock_state(&config)?;
    if crate::transfer::has_pending(&config)? {
        bail!("Pending transfer recovery blocks adoption");
    }
    let (approved, staged) = gitrepo::snapshot(
        config["repository"]["url"].as_str().unwrap(),
        config["repository"]["branch"].as_str().unwrap(),
        Path::new(config["paths"]["repository"].as_str().unwrap()),
        Path::new(config["paths"]["staging"].as_str().unwrap()),
    )?;
    if approved != expected_commit {
        bail!("Expected adoption commit is not the current approved branch HEAD");
    }
    crate::policy::validate_repository(&staged)?;
    let client = deploy::client(&config)?;
    apply_verified(
        &config,
        plan,
        &staged,
        &approved,
        &show_unit(&config)?,
        || client.online(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adoption_preserves_custom_unit_world_and_running_players() {
        let root = tempfile::tempdir().unwrap();
        let server = root.path().join("existing");
        let repo = root.path().join("repo");
        let state = root.path().join("state");
        fs::create_dir_all(server.join("world")).unwrap();
        fs::create_dir_all(repo.join("requirements")).unwrap();
        fs::create_dir_all(repo.join("server-settings")).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(server.join("world/level.dat"), "valuable world").unwrap();
        fs::write(server.join("custom-paper.jar"), "jar").unwrap();
        fs::write(
            server.join("server.properties"),
            "motd=hello\nenable-rcon=true\nrcon.password=existing-secret\n",
        )
        .unwrap();
        fs::write(
            repo.join("server-settings/server.properties"),
            "motd=hello\n",
        )
        .unwrap();
        let sha = artifacts::sha256(&server.join("custom-paper.jar")).unwrap();
        fs::write(repo.join("requirements/engine.json"),serde_json::to_vec(&json!({"name":"custom","filename":"custom-paper.jar","pinned":{"transfer":true,"sha256":sha,"version":"reviewed"}})).unwrap()).unwrap();
        let fragment = root.path().join("custom.service");
        let contents =
            "[Service]\nUser=custom\nExecStart=/custom/java -Xmx7G -jar custom-paper.jar\n";
        fs::write(&fragment, contents).unwrap();
        let config = json!({"paths":{"server":server,"state":state,"cache":root.path().join("cache")},"deployment":{"service":"custom.service"}});
        let unit = json!({"Id":"custom.service","User":"custom","Group":"custom","WorkingDirectory":server,"ExecStart":"{ path=/custom/java ; argv[]=/custom/java -Xmx7G -jar custom-paper.jar ; }","FragmentPath":fragment,"ActiveState":"active","MainPID":"123","LaunchPath":"/custom/java","LaunchArgv":["/custom/java","-Xmx7G","-jar","custom-paper.jar"],"LaunchFlags":[]});
        let plan = build_plan(&config, &server, &unit).unwrap();
        assert!(plan["launch_supported"].as_bool().unwrap());
        assert!(!serde_json::to_string(&plan).unwrap().contains("-Xmx7G"));
        let mut unsupported = unit.clone();
        unsupported["LaunchPath"] = json!("/bin/sh");
        unsupported["LaunchArgv"] = json!(["/bin/sh", "launch.sh", "--password=hidden"]);
        unsupported["ExecStart"] =
            json!("{ path=/bin/sh ; argv[]=/bin/sh launch.sh --password=hidden ; }");
        let preview = build_plan(&config, &server, &unsupported).unwrap();
        assert_eq!(preview["launch_supported"], false);
        assert!(!serde_json::to_string(&preview).unwrap().contains("hidden"));
        assert!(
            apply_verified(
                &config,
                &preview,
                &repo,
                "approved-commit",
                &unsupported,
                || Ok(3)
            )
            .is_err()
        );
        fs::write(&fragment, "changed unit").unwrap();
        assert!(apply_verified(&config, &plan, &repo, "approved-commit", &unit, || Ok(3)).is_err());
        fs::write(&fragment, contents).unwrap();
        fs::write(server.join("custom-paper.jar"), "wrong jar").unwrap();
        assert!(apply_verified(&config, &plan, &repo, "approved-commit", &unit, || Ok(3)).is_err());
        fs::write(server.join("custom-paper.jar"), "jar").unwrap();
        fs::write(
            repo.join("server-settings/server.properties"),
            "motd=unexpected\n",
        )
        .unwrap();
        assert!(apply_verified(&config, &plan, &repo, "approved-commit", &unit, || Ok(3)).is_err());
        fs::write(
            repo.join("server-settings/server.properties"),
            "motd=hello\n",
        )
        .unwrap();
        assert!(
            apply_verified(
                &config,
                &plan,
                &repo,
                "approved-commit",
                &unit,
                || anyhow::bail!("RCON unavailable")
            )
            .is_err()
        );
        assert!(!state.join("deployment.json").exists());
        let result =
            apply_verified(&config, &plan, &repo, "approved-commit", &unit, || Ok(3)).unwrap();
        assert_eq!(result["online"], 3);
        assert_eq!(fs::read_to_string(&fragment).unwrap(), contents);
        assert_eq!(
            fs::read_to_string(server.join("world/level.dat")).unwrap(),
            "valuable world"
        );
        assert!(
            fs::read_to_string(server.join("server.properties"))
                .unwrap()
                .contains("existing-secret")
        );
        assert!(server.join("custom-paper.jar").exists());
    }
}

#[cfg(test)]
mod launcher_tests {
    use super::*;
    #[test]
    fn exact_dbus_argv_flags_and_missing_bootstrap_jar() {
        let root = tempfile::tempdir().unwrap();
        let config =
            json!({"paths":{"server":root.path()},"deployment":{"service":"minecraft.service"}});
        let mut unit = json!({"Id":"minecraft.service","WorkingDirectory":root.path()});
        let record = json!([
            "/usr/bin/java",
            ["/usr/bin/java", "-Xmx2G", "-jar", "server.jar", "nogui"],
            [],
            0,
            0,
            0,
            0,
            0,
            0,
            0
        ]);
        let structured = json!({"type":"a(sasasttttuii)","data":[record.clone()]});
        attach_structured_launcher(&mut unit, &structured).unwrap();
        assert!(verify_launcher(&config, &unit, "server.jar").is_ok());
        assert!(verify_launcher(&config, &unit, "other.jar").is_err());
        unit["LaunchArgv"] = json!(["/usr/bin/java", "-jar", "server.jar nogui"]);
        assert!(verify_launcher(&config, &unit, "server.jar").is_err());
        unit["LaunchArgv"] = json!(["/usr/bin/java", "$JAVA_OPTS", "-jar", "server.jar"]);
        assert!(verify_launcher(&config, &unit, "server.jar").is_err());
        unit["LaunchArgv"] = json!(["/usr/bin/java", "-jar", "server.jar"]);
        unit["LaunchFlags"] = json!(["fully-privileged"]);
        assert!(verify_launcher(&config, &unit, "server.jar").is_err());
        assert!(
            attach_structured_launcher(
                &mut unit,
                &json!({"type":"a(sasasttttuii)","data":[record.clone(),record]})
            )
            .is_err()
        );
    }
}
