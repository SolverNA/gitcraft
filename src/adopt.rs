//! Adopt an existing unit without rewriting its launch command, user, Java or credentials.
use crate::{artifacts, deploy, gitrepo, recovery};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path, process::Command, time::Duration};
fn show_unit(config: &Value) -> Result<Value> {
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
    parse_show(&String::from_utf8(output.stdout)?)
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
    let exec = unit["ExecStart"].as_str().context("ExecStart required")?;
    let executable = exec
        .split("path=")
        .nth(1)
        .and_then(|s| s.split(" ;").next())
        .context("Cannot resolve executable from systemd")?;
    if Path::new(executable).file_name().and_then(|s| s.to_str()) != Some("java") {
        bail!(
            "Unsupported launcher: adoption currently requires direct java -jar; shell scripts and Forge @args need a reviewed launcher adapter"
        );
    }
    let args = exec
        .split("argv[]=")
        .nth(1)
        .and_then(|s| s.split(" ;").next())
        .context("Cannot resolve launch arguments")?;
    let words: Vec<_> = args.split_whitespace().collect();
    let jar=words.windows(2).find(|w|w[0]=="-jar").map(|w|w[1]).context("Unsupported launcher: direct -jar is required; @args and shell launchers are not inferred")?;
    let jar_path = Path::new(jar);
    let full = if jar_path.is_absolute() {
        jar_path.to_path_buf()
    } else {
        server.join(jar_path)
    };
    let canonical = fs::canonicalize(&full).context("Launched JAR does not exist")?;
    let server = fs::canonicalize(server)?;
    let relative = canonical
        .strip_prefix(&server)
        .context("Launched JAR is outside server directory")?;
    if relative.components().count() != 1 {
        bail!("Launched engine must be a top-level pinned JAR");
    }
    let relative = relative.to_str().context("Non-UTF8 launched JAR")?;
    deploy::safe_path(&server, relative)?;
    if fs::symlink_metadata(&full)?.file_type().is_symlink() {
        bail!("Launched engine symlink is unsupported");
    }
    Ok(relative.into())
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
        let unit = json!({"Id":"custom.service","User":"custom","Group":"custom","WorkingDirectory":server,"ExecStart":"{ path=/custom/java ; argv[]=/custom/java -Xmx7G -jar custom-paper.jar ; }","FragmentPath":fragment,"ActiveState":"active","MainPID":"123"});
        let plan = build_plan(&config, &server, &unit).unwrap();
        assert!(plan["launch_supported"].as_bool().unwrap());
        assert!(!serde_json::to_string(&plan).unwrap().contains("-Xmx7G"));
        let mut unsupported = unit.clone();
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
