//! Activate only the installed, root-owned GitCraft poller after durable initialization.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::os::unix::fs::MetadataExt;
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
    process::Command,
    time::Duration,
};

pub(crate) const UNIT: &str = "gitcraft-agent.service";
pub(crate) const UNIT_FILE: &str = "/etc/systemd/system/gitcraft-agent.service";
pub(crate) const BINARY: &str = "/opt/gitcraft/bin/gitcraft";
pub(crate) const CONFIG: &str = "/etc/gitcraft/config/agent.yml";
pub(crate) const ENV_FILE: &str = "/etc/gitcraft/secrets.env";

pub(crate) trait Runtime {
    fn inspect(&mut self) -> Result<Value>;
    fn installed_config(&mut self) -> Result<Value>;
    fn trusted_files(&mut self, unit: &Value) -> Result<()>;
    fn minecraft_user(&mut self, config: &Value) -> Result<String>;
    fn enable_now(&mut self) -> Result<()>;
}
pub(crate) struct SystemRuntime;
fn bus(args: &[&str]) -> Result<Value> {
    let result = crate::gitrepo::run_bounded(
        Command::new("/usr/bin/busctl")
            .args(["--system", "--json=short"])
            .args(args),
        Duration::from_secs(15),
    )
    .context("Poller discovery unavailable; install the supported systemd/busctl tools")?;
    ensure!(
        result.status.success(),
        "Poller system-bus discovery failed; output omitted"
    );
    serde_json::from_slice(&result.stdout)
        .map_err(|_| anyhow::anyhow!("Malformed poller discovery response; values omitted"))
}
fn trusted(path: &Path) -> Result<()> {
    let mut prefix = std::path::PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        let meta = prefix
            .symlink_metadata()
            .context("Required root-owned installation file is missing")?;
        ensure!(
            !meta.file_type().is_symlink() && meta.uid() == 0 && meta.mode() & 0o022 == 0,
            "Poller installation paths must be root-owned and not writable by group/others"
        );
    }
    ensure!(path.is_file(), "Poller installation file must be regular");
    Ok(())
}
impl Runtime for SystemRuntime {
    fn inspect(&mut self) -> Result<Value> {
        let output = crate::gitrepo::run_bounded(
            Command::new("/usr/bin/systemctl").args([
                "show",
                "--no-pager",
                "--property=Id,ActiveState,UnitFileState,FragmentPath,User,WorkingDirectory",
                UNIT,
            ]),
            Duration::from_secs(15),
        )?;
        ensure!(
            output.status.success(),
            "Installed poller unit cannot be inspected; output omitted"
        );
        let mut unit = json!({});
        for line in String::from_utf8(output.stdout)?.lines() {
            if let Some((key, value)) = line.split_once('=') {
                unit[key] = json!(value);
            }
        }
        let object = bus(&[
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "GetUnit",
            "s",
            UNIT,
        ])?;
        let object = object["data"][0]
            .as_str()
            .context("Installed poller object missing")?;
        let exec = bus(&[
            "get-property",
            "org.freedesktop.systemd1",
            object,
            "org.freedesktop.systemd1.Service",
            "ExecStartEx",
        ])?;
        ensure!(
            exec["type"] == "a(sasasttttuii)",
            "Unsupported poller ExecStartEx signature"
        );
        let commands = exec["data"].as_array().context("Poller argv unavailable")?;
        ensure!(commands.len() == 1, "Poller must have one exact ExecStart");
        let record = commands[0].as_array().context("Invalid poller ExecStart")?;
        ensure!(record.len() == 10, "Invalid poller ExecStart record");
        unit["LaunchPath"] = record[0].clone();
        unit["LaunchArgv"] = record[1].clone();
        unit["LaunchFlags"] = record[2].clone();
        let environment = bus(&[
            "get-property",
            "org.freedesktop.systemd1",
            object,
            "org.freedesktop.systemd1.Service",
            "EnvironmentFiles",
        ])?;
        ensure!(
            environment["type"] == "a(sb)",
            "Unsupported poller environment-file signature"
        );
        unit["EnvironmentFiles"] = environment["data"].clone();
        let dropins = bus(&[
            "get-property",
            "org.freedesktop.systemd1",
            object,
            "org.freedesktop.systemd1.Unit",
            "DropInPaths",
        ])?;
        ensure!(
            dropins["type"] == "as",
            "Unsupported poller drop-in signature"
        );
        unit["DropInPaths"] = dropins["data"].clone();
        Ok(unit)
    }
    fn installed_config(&mut self) -> Result<Value> {
        crate::config::load(Path::new(CONFIG))
    }
    fn trusted_files(&mut self, unit: &Value) -> Result<()> {
        for path in [UNIT_FILE, BINARY, ENV_FILE] {
            trusted(Path::new(path))?;
        }
        for path in unit["DropInPaths"]
            .as_array()
            .context("Poller drop-in list unavailable")?
        {
            trusted(Path::new(
                path.as_str().context("Invalid poller drop-in path")?,
            ))?;
        }
        Ok(())
    }
    fn minecraft_user(&mut self, config: &Value) -> Result<String> {
        Ok(crate::adopt::show_unit(config)?["User"]
            .as_str()
            .context("Minecraft unit user unavailable")?
            .into())
    }
    fn enable_now(&mut self) -> Result<()> {
        let result = crate::gitrepo::run_bounded(
            Command::new("sudo").args(["-n", "/usr/bin/systemctl", "enable", "--now", UNIT]),
            Duration::from_secs(60),
        )?;
        ensure!(
            result.status.success(),
            "Poller activation was denied or failed; update the installed narrowly scoped sudoers rule and retry agent-activate"
        );
        Ok(())
    }
}
fn validate_unit(unit: &Value) -> Result<()> {
    ensure!(
        unit["Id"] == UNIT && unit["FragmentPath"] == UNIT_FILE,
        "Loaded poller is not the expected installed unit"
    );
    ensure!(
        unit["LaunchPath"] == BINARY
            && unit["LaunchArgv"] == json!([BINARY, "agent", "--config", CONFIG])
            && unit["LaunchFlags"] == json!([]),
        "Loaded poller launcher differs from the standard long-running agent/config; no unit was rewritten"
    );
    ensure!(
        unit["EnvironmentFiles"] == json!([[ENV_FILE, false]]),
        "Poller environment files differ from the supported installation"
    );
    ensure!(
        unit["User"]
            .as_str()
            .is_some_and(|name| !name.is_empty() && name != "root"),
        "Poller must run as the non-root Minecraft user"
    );
    ensure!(
        unit["WorkingDirectory"]
            .as_str()
            .is_some_and(|path| ["", "/", "/var/lib/minecraft"].contains(&path)),
        "Unexpected poller working directory; review the installed unit before activation"
    );
    Ok(())
}
fn observed(unit: &Value, standard: bool) -> Value {
    json!({"unit":UNIT,"active":unit["ActiveState"].as_str().unwrap_or("unknown"),"enabled":unit["UnitFileState"].as_str().unwrap_or("unknown"),"installed_standard":standard})
}
pub fn status() -> Value {
    status_with(&mut SystemRuntime)
}
fn status_with(runtime: &mut impl Runtime) -> Value {
    match runtime.inspect() {
        Ok(unit) => {
            let standard = validate_unit(&unit)
                .and_then(|_| runtime.trusted_files(&unit))
                .is_ok();
            let mut result = observed(&unit, standard);
            result["message"] = json!(if standard {
                "Actual installed poller state"
            } else {
                "Poller installation requires operator review; activation will not rewrite units"
            });
            result
        }
        Err(_) => {
            json!({"unit":UNIT,"active":"unknown","enabled":"unknown","installed_standard":false,"message":"Installed poller unavailable; install the standard service and retry activation"})
        }
    }
}
struct ActivationLock(File);
impl Drop for ActivationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
pub fn activate(config: &Value) -> Result<Value> {
    activate_with(config, &mut SystemRuntime)
}
pub(crate) fn activate_with(config: &Value, runtime: &mut impl Runtime) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    let state = Path::new(config["paths"]["state"].as_str().context("State missing")?);
    fs::create_dir_all(state)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(crate::deploy::safe_path(state, "agent-activation.lock")?)?;
    file.try_lock_exclusive()
        .context("Poller activation is already running")?;
    let _activation = ActivationLock(file);
    {
        let _deployment = crate::deploy::lock_state(&config)?;
        ensure!(
            crate::recovery::pending(&config)?.is_none() && !crate::transfer::has_pending(&config)?,
            "Recovery is pending; resolve it before activating the poller"
        );
        let bytes = fs::read(crate::deploy::safe_path(state, "deployment.json")?).context(
            "No durable initialized state; complete explicit bootstrap or adoption first",
        )?;
        let installed: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            ["deployed", "adopted"].contains(&installed["status"].as_str().unwrap_or(""))
                && installed["commit"].as_str().is_some_and(
                    |sha| sha.len() == 40 && sha.bytes().all(|c| c.is_ascii_hexdigit())
                )
                && installed["managed_files"].is_array(),
            "Initialized ownership proof is invalid; poller was not activated"
        );
        ensure!(
            runtime.installed_config()? == config,
            "Initialization configuration differs from the installed agent configuration; update the reviewed standard installation, then use agent-activate (do not repeat initialization)"
        );
        let unit = runtime.inspect()?;
        validate_unit(&unit)?;
        runtime.trusted_files(&unit)?;
        ensure!(
            unit["User"].as_str() == Some(runtime.minecraft_user(&config)?.as_str()),
            "Installed poller user differs from the Minecraft unit user"
        );
    }
    // Release deploy.lock BEFORE systemd starts the poller: its first reconciliation must not collide with initialization.
    let before = runtime.inspect()?;
    validate_unit(&before)?;
    runtime.trusted_files(&before)?;
    if before["ActiveState"] != "active" || before["UnitFileState"] != "enabled" {
        runtime.enable_now()?;
    }
    let after = runtime.inspect()?;
    validate_unit(&after)?;
    runtime.trusted_files(&after)?;
    ensure!(
        after["ActiveState"] == "active" && after["UnitFileState"] == "enabled",
        "Initialization succeeded, but the poller is not both active and persistently enabled; retry agent-activate after inspecting the service"
    );
    let mut result = observed(&after, true);
    result["status"] = json!("activated");
    crate::recovery::durable_json(&state.join("agent-activation.json"), &result)?;
    Ok(result)
}
/// The same post-initialization orchestration is used by CLI, Web and acceptance tests.
pub(crate) fn initialize_with(
    config: &Value,
    runtime: &mut impl Runtime,
    operation: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    let mut result = operation()?;
    ensure!(
        ["deployed", "adopted"].contains(&result["status"].as_str().unwrap_or("")),
        "Initialization did not complete; poller activation was not attempted"
    );
    let initialization_status = result["status"].clone();
    result["initialized"] = json!(true);
    result["initialization_status"] = initialization_status;
    result["agent_activation"] = match activate_with(config, runtime) {
        Ok(value) => value,
        Err(error) => {
            result["status"] = json!("partial_success");
            result["next_action"] = json!("agent-activate");
            json!({"status":"activation_required","unit":UNIT,"message":error.to_string(),"observed":status_with(runtime),"retry_without_reinitializing":true})
        }
    };
    if let Some(state) = config["paths"]["state"].as_str()
        && crate::recovery::durable_json(&Path::new(state).join("status.json"), &result).is_err()
    {
        result["status_recorded"] = json!(false);
    }
    Ok(result)
}
pub(crate) fn initialize(
    config: &Value,
    operation: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    initialize_with(config, &mut SystemRuntime, operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        config: Value,
        unit: Value,
        enables: usize,
        denied: bool,
        trusted: bool,
    }
    impl Runtime for Fake {
        fn inspect(&mut self) -> Result<Value> {
            Ok(self.unit.clone())
        }
        fn installed_config(&mut self) -> Result<Value> {
            Ok(self.config.clone())
        }
        fn trusted_files(&mut self, _: &Value) -> Result<()> {
            ensure!(self.trusted, "Installation proof failed");
            Ok(())
        }
        fn minecraft_user(&mut self, _: &Value) -> Result<String> {
            Ok("minecraft".into())
        }
        fn enable_now(&mut self) -> Result<()> {
            let _proof = crate::deploy::lock_state(&self.config)?;
            ensure!(
                !self.denied,
                "Narrow service activation was denied; output omitted"
            );
            self.enables += 1;
            self.unit["ActiveState"] = json!("active");
            self.unit["UnitFileState"] = json!("enabled");
            Ok(())
        }
    }
    pub(super) fn standard_unit() -> Value {
        json!({"Id":UNIT,"ActiveState":"inactive","UnitFileState":"disabled","FragmentPath":UNIT_FILE,"User":"minecraft","WorkingDirectory":"","LaunchPath":BINARY,"LaunchArgv":[BINARY,"agent","--config",CONFIG],"LaunchFlags":[],"EnvironmentFiles":[[ENV_FILE,false]],"DropInPaths":[]})
    }
    fn fixture() -> (tempfile::TempDir, Fake) {
        let root = tempfile::tempdir().unwrap();
        let paths: serde_json::Map<_, _> = [
            "repository",
            "server",
            "state",
            "cache",
            "staging",
            "backups",
        ]
        .into_iter()
        .map(|key| (key.into(), json!(root.path().join(key))))
        .collect();
        let config = crate::config::validate(
            json!({"repository":{"url":"git@github.com:team/config.git"},"paths":paths}),
        )
        .unwrap();
        (
            root,
            Fake {
                config,
                unit: standard_unit(),
                enables: 0,
                denied: false,
                trusted: true,
            },
        )
    }
    fn initialize_state(config: &Value, status: &str) -> Result<Value> {
        let _lock = crate::deploy::lock_state(config)?;
        let value = json!({"status":status,"commit":"a".repeat(40),"managed_files":["server.jar"]});
        crate::recovery::durable_json(
            &Path::new(config["paths"]["state"].as_str().unwrap()).join("deployment.json"),
            &value,
        )?;
        Ok(value)
    }
    #[test]
    fn successful_init_activates_after_commit_and_unlock_only_once() {
        for status in ["deployed", "adopted"] {
            let (_root, mut runtime) = fixture();
            let config = runtime.config.clone();
            let result =
                initialize_with(&config, &mut runtime, || initialize_state(&config, status))
                    .unwrap();
            assert_eq!(result["status"], status);
            assert_eq!(result["agent_activation"]["status"], "activated");
            assert_eq!(runtime.enables, 1);
            activate_with(&config, &mut runtime).unwrap();
            assert_eq!(runtime.enables, 1);
        }
    }
    #[test]
    fn activation_denial_is_partial_success_and_retry_preserves_initial_state() {
        let (_root, mut runtime) = fixture();
        let config = runtime.config.clone();
        runtime.denied = true;
        let result = initialize_with(&config, &mut runtime, || {
            initialize_state(&config, "deployed")
        })
        .unwrap();
        assert_eq!(result["status"], "partial_success");
        assert_eq!(result["agent_activation"]["status"], "activation_required");
        assert_eq!(runtime.enables, 0);
        let path = Path::new(config["paths"]["state"].as_str().unwrap()).join("deployment.json");
        let before = fs::read(&path).unwrap();
        runtime.denied = false;
        activate_with(&config, &mut runtime).unwrap();
        assert_eq!(before, fs::read(path).unwrap());
        assert_eq!(runtime.enables, 1);
    }
    #[test]
    fn failed_initialization_and_pending_recovery_never_enable() {
        let (_root, mut runtime) = fixture();
        let config = runtime.config.clone();
        assert!(
            initialize_with(&config, &mut runtime, || anyhow::bail!(
                "initialization failed"
            ))
            .is_err()
        );
        assert_eq!(runtime.enables, 0);
        assert!(activate_with(&config, &mut runtime).is_err());
        initialize_state(&config, "deployed").unwrap();
        crate::recovery::save(&config, &json!({"phase":"applying"})).unwrap();
        assert!(activate_with(&config, &mut runtime).is_err());
        assert_eq!(runtime.enables, 0);
    }
    #[test]
    fn effective_unit_config_and_file_proofs_fail_closed() {
        for case in 0..5 {
            let (_root, mut runtime) = fixture();
            let config = runtime.config.clone();
            initialize_state(&config, "adopted").unwrap();
            match case {
                0 => {
                    runtime.unit["LaunchArgv"] =
                        json!([BINARY, "agent", "--config", "/tmp/different.yml"])
                }
                1 => runtime.unit["User"] = json!("root"),
                2 => runtime.unit["EnvironmentFiles"] = json!([["/tmp/untrusted", false]]),
                3 => runtime.trusted = false,
                _ => runtime.config["repository"]["branch"] = json!("other"),
            };
            assert!(activate_with(&config, &mut runtime).is_err());
            assert_eq!(runtime.enables, 0);
        }
    }
}
