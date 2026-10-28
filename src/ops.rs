//! Explicit maintenance operations; no arbitrary shell or forced player eviction.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
fn state(config: &Value, name: &str) -> Result<PathBuf> {
    crate::deploy::safe_path(
        Path::new(config["paths"]["state"].as_str().context("State missing")?),
        name,
    )
}
pub fn stop(config: &Value) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    let _lock = crate::deploy::lock_state(&config)?;
    let client = crate::deploy::client(&config)?;
    ensure!(
        client.online()? == 0,
        "Players are online; maintenance stop rejected"
    );
    client.command("save-all flush")?;
    ensure!(client.online()? == 0, "Player joined; stop rejected");
    crate::deploy::service("stop", config["deployment"]["service"].as_str().unwrap())?;
    crate::deploy::ensure_stopped(&config)?;
    Ok(json!({"status":"completed","operation":"maintenance_stop","players_kicked":false}))
}
fn properties(text: &str) -> BTreeMap<String, String> {
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
                Some((key.to_owned(), value.trim().to_owned()))
            }
        })
        .collect()
}
pub fn resume(config: &Value, acknowledge: bool) -> Result<Value> {
    ensure!(
        acknowledge,
        "Explicit acknowledgement of game-data compatibility is required"
    );
    let config = crate::config::validate(config.clone())?;
    let _lock = crate::deploy::lock_state(&config)?;
    crate::deploy::ensure_stopped(&config)?;
    ensure!(
        crate::recovery::pending(&config)?.is_none() && !crate::transfer::has_pending(&config)?,
        "Recovery is pending; resume rejected"
    );
    let installed: Value = serde_json::from_slice(
        &fs::read(state(&config, "deployment.json")?)
            .context("No installed state; use explicit EULA bootstrap or adoption")?,
    )?;
    let commit = installed["commit"]
        .as_str()
        .context("Installed commit missing")?;
    ensure!(
        commit.len() == 40 && commit.bytes().all(|c| c.is_ascii_hexdigit()),
        "Installed commit invalid"
    );
    let snapshot = tempfile::tempdir_in(
        config["paths"]["staging"]
            .as_str()
            .context("Staging missing")?,
    )?;
    let git = |args: &[&str]| -> Result<()> {
        let result =
            crate::gitrepo::run_bounded(Command::new("git").args(args), Duration::from_secs(60))?;
        ensure!(
            result.status.success(),
            "Cannot verify installed revision; resume rejected"
        );
        Ok(())
    };
    git(&[
        "clone",
        "--quiet",
        "--no-checkout",
        config["paths"]["repository"].as_str().unwrap(),
        snapshot.path().to_str().unwrap(),
    ])?;
    git(&[
        "-C",
        snapshot.path().to_str().unwrap(),
        "checkout",
        "--quiet",
        "--detach",
        commit,
    ])?;
    crate::policy::validate_repository(snapshot.path())?;
    let server = Path::new(config["paths"]["server"].as_str().unwrap());
    let manifest = crate::artifacts::load_manifest(snapshot.path())?;
    let engine = manifest
        .iter()
        .find(|a| !a.destination.contains('/'))
        .context("Installed engine missing")?;
    crate::adopt::verify_launcher(
        &config,
        &crate::adopt::show_unit(&config)?,
        &engine.destination,
    )?;
    for artifact in &manifest {
        ensure!(
            crate::artifacts::sha256(&crate::deploy::safe_path(server, &artifact.destination)?)?
                == artifact.sha256,
            "Installed JAR hash differs; resume rejected"
        );
    }
    for (root, prefix) in [
        ("server-settings", ""),
        ("plugins-configs", "plugins/"),
        ("config", "config/"),
        ("defaultconfigs", "defaultconfigs/"),
    ] {
        fn walk(base: &Path, p: &Path, prefix: &str, server: &Path) -> Result<()> {
            if !p.exists() {
                return Ok(());
            }
            for entry in fs::read_dir(p)? {
                let source = entry?.path();
                ensure!(!source.is_symlink(), "Config symlink rejected");
                if source.is_dir() {
                    walk(base, &source, prefix, server)?;
                } else {
                    let relative = source
                        .strip_prefix(base)?
                        .to_str()
                        .context("Config path invalid")?;
                    let target = crate::deploy::safe_path(server, &format!("{prefix}{relative}"))?;
                    if relative == "server.properties" && prefix.is_empty() {
                        ensure!(
                            properties(&fs::read_to_string(&source)?)
                                == properties(&fs::read_to_string(target)?),
                            "Runtime configuration differs from installed revision"
                        );
                    } else {
                        ensure!(
                            crate::artifacts::sha256(&source)?
                                == crate::artifacts::sha256(&target)?,
                            "Runtime configuration differs from installed revision"
                        );
                    }
                }
            }
            Ok(())
        }
        walk(
            &snapshot.path().join(root),
            &snapshot.path().join(root),
            prefix,
            server,
        )?;
    }
    let client = crate::deploy::client(&config)?;
    let original = snapshot.path().join("server-settings/server.properties");
    crate::deploy::ensure_stopped(&config)?;
    ensure!(
        crate::recovery::pending(&config)?.is_none() && !crate::transfer::has_pending(&config)?,
        "Recovery appeared during preflight"
    );
    crate::adopt::verify_launcher(
        &config,
        &crate::adopt::show_unit(&config)?,
        &engine.destination,
    )?;
    let properties_path = crate::deploy::safe_path(server, "server.properties")?;
    crate::deploy::write_properties(
        if original.is_file() {
            Some(original.as_path())
        } else if properties_path.is_file() {
            Some(properties_path.as_path())
        } else {
            None
        },
        &properties_path,
        &client,
    )?;
    let service = config["deployment"]["service"].as_str().unwrap();
    let started = crate::deploy::service("start", service);
    let deadline = std::time::Instant::now()
        + Duration::from_secs(
            config["deployment"]["health_timeout_seconds"]
                .as_u64()
                .unwrap(),
        );
    loop {
        let healthy = started.is_ok()
            && crate::deploy::service("is-active", service)
                .is_ok_and(|status| status.status.success())
            && client.online().is_ok();
        if healthy {
            break;
        }
        if started.is_err() || std::time::Instant::now() >= deadline {
            let stopped = crate::deploy::service("stop", service).is_ok()
                && crate::deploy::ensure_stopped(&config).is_ok();
            crate::recovery::durable_json(
                &state(&config, "status.json")?,
                &json!({"status":"recovery_required","operation":"resume","service_stopped":stopped,"message":"Start/readiness failed. Inspect service state and game-data compatibility before another acknowledged start."}),
            )?;
            anyhow::bail!(
                "recovery_required: start/readiness failed; service_stopped={stopped}; inspection required"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(json!({"status":"completed","operation":"resume","commit":commit}))
}
pub fn backups(config: &Value) -> Result<Value> {
    let root = Path::new(
        config["paths"]["backups"]
            .as_str()
            .context("Backup path missing")?,
    );
    let mut records = Vec::new();
    if root.exists() {
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            ensure!(!path.is_symlink(), "Backup symlink refused");
            if path.is_dir() {
                let receipt = path.join("data-backup.json");
                if receipt.exists() {
                    let bytes = fs::read(&receipt)?;
                    ensure!(
                        bytes.len() < 4 * 1024 * 1024,
                        "Backup receipt exceeds list limit"
                    );
                    let value: Value = serde_json::from_slice(&bytes)?;
                    records.push(json!({"path":path,"status":value["status"],"includes":value["includes"],"files":value["files"].as_object().map(|v|v.len())}));
                }
            }
        }
    }
    Ok(json!({"backups":records,"automatic_retention":false}))
}

pub fn set_credential(config: &Value, role: &str, value: &str, confirmed: bool) -> Result<Value> {
    ensure!(confirmed, "Credential confirmation required");
    let config = crate::config::validate(config.clone())?;
    let _lock = crate::deploy::lock_state(&config)?;
    if role == "rcon" {
        crate::deploy::ensure_stopped(&config)?;
    }
    crate::secrets::write(&config, role, value)
}
