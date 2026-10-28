//! Explicit Minecraft boot policy, separate from poller activation and current runtime state.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
pub(crate) const SCOPE: &str = "/etc/gitcraft/minecraft-unit.json";
pub(crate) trait Runtime {
    fn inspect(&mut self, config: &Value) -> Result<Value>;
    fn trusted_installation(&mut self, config: &Value, unit: &Value) -> Result<()>;
    fn change(&mut self, config: &Value, policy: &str) -> Result<()>;
}
pub(crate) struct SystemRuntime;
impl Runtime for SystemRuntime {
    fn inspect(&mut self, config: &Value) -> Result<Value> {
        crate::adopt::show_unit(config)
    }
    fn trusted_installation(&mut self, config: &Value, unit: &Value) -> Result<()> {
        crate::agent_service::trusted(Path::new(SCOPE))?;
        let scope: Value = serde_json::from_slice(&fs::read(SCOPE)?)?;
        ensure!(
            scope["service"] == config["deployment"]["service"]
                && scope["server"] == config["paths"]["server"],
            "Minecraft unit is outside the root-approved installation scope; update the reviewed installer permissions"
        );
        ensure!(
            scope["fragment"] == unit["FragmentPath"],
            "Loaded Minecraft unit differs from installed scope"
        );
        let path = Path::new(
            unit["FragmentPath"]
                .as_str()
                .context("Minecraft fragment missing")?,
        );
        crate::agent_service::trusted(path).context(
            "Minecraft original unit fragment requires protected ownership and no symlink path",
        )?;
        for dropin in unit["DropInPaths"]
            .as_array()
            .context("Minecraft drop-in proof unavailable")?
        {
            crate::agent_service::trusted(Path::new(
                dropin.as_str().context("Invalid Minecraft drop-in path")?,
            ))
            .context("Minecraft drop-in requires protected ownership and no symlink path")?;
        }
        let installed = crate::config::load(Path::new(crate::agent_service::CONFIG))?;
        ensure!(
            installed == *config,
            "Minecraft boot policy requires the reviewed installed configuration; no custom-config unit was enabled"
        );
        Ok(())
    }
    fn change(&mut self, config: &Value, policy: &str) -> Result<()> {
        ensure!(
            ["enabled", "disabled"].contains(&policy),
            "Unsupported boot policy"
        );
        let action = if policy == "enabled" {
            "enable"
        } else {
            "disable"
        };
        let unit = config["deployment"]["service"]
            .as_str()
            .context("Minecraft service missing")?;
        let output = crate::gitrepo::run_bounded(
            Command::new("sudo").args(["-n", "/usr/bin/systemctl", action, unit]),
            Duration::from_secs(60),
        )?;
        ensure!(
            output.status.success(),
            "Minecraft boot policy change failed or lacks the narrowly scoped installer permission; runtime was not started"
        );
        Ok(())
    }
}
fn file(config: &Value, name: &str) -> Result<PathBuf> {
    crate::deploy::safe_path(
        Path::new(config["paths"]["state"].as_str().context("State missing")?),
        name,
    )
}
fn read(config: &Value, name: &str) -> Result<Option<Value>> {
    let path = file(config, name)?;
    if !path.exists() {
        return Ok(None);
    }
    ensure!(
        fs::metadata(&path)?.len() < 1024 * 1024,
        "Boot policy record exceeds safety limit"
    );
    let bytes = fs::read(path)?;
    ensure!(
        bytes.len() < 1024 * 1024,
        "Boot policy record exceeds safety limit"
    );
    Ok(Some(serde_json::from_slice(&bytes)?))
}
fn binding(config: &Value) -> Value {
    json!({"service":config["deployment"]["service"],"server":config["paths"]["server"],"state":config["paths"]["state"]})
}
fn validate_record(config: &Value, record: &Value) -> Result<()> {
    ensure!(
        record["binding"] == binding(config),
        "Boot policy/maintenance belongs to another server; restore original settings"
    );
    Ok(())
}
pub fn pending(config: &Value) -> Result<bool> {
    if read(config, "maintenance.json")?.is_some() {
        return Ok(true);
    }
    Ok(read(config, "boot-policy.json")?.is_some_and(|v| v["phase"] != "applied"))
}
pub(crate) fn ensure_not_held(config: &Value) -> Result<()> {
    ensure!(
        read(config, "maintenance.json")?.is_none(),
        "Persistent maintenance hold is active; use explicit acknowledged resume, not deployment or initialization"
    );
    Ok(())
}
pub fn guard_configuration_change(current: &Value, proposed: &Value) -> Result<()> {
    if read(current, "maintenance.json")?.is_some() || read(current, "boot-policy.json")?.is_some()
    {
        ensure!(
            binding(current) == binding(proposed),
            "Existing boot policy/maintenance cannot be orphaned by changing service/server/state; use a separately reviewed installation"
        );
    }
    Ok(())
}
fn unit_policy(unit: &Value) -> &str {
    unit["UnitFileState"].as_str().unwrap_or("unknown")
}
fn initialized(config: &Value) -> Result<Value> {
    let value = read(config, "deployment.json")?.context(
        "Explicit bootstrap or adoption is required before Minecraft boot policy changes",
    )?;
    ensure!(
        ["deployed", "adopted"].contains(&value["status"].as_str().unwrap_or(""))
            && value["commit"]
                .as_str()
                .is_some_and(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())),
        "Initialized Minecraft ownership is invalid"
    );
    Ok(value)
}
fn checked_unit(config: &Value, runtime: &mut impl Runtime) -> Result<Value> {
    let installed = initialized(config)?;
    let unit = runtime.inspect(config)?;
    let jars: Vec<&str> = installed["managed_files"]
        .as_array()
        .context("Managed file proof missing")?
        .iter()
        .filter_map(Value::as_str)
        .filter(|path| !path.contains('/') && path.ends_with(".jar"))
        .collect();
    ensure!(
        jars.len() == 1,
        "Exactly one initialized engine is required"
    );
    crate::adopt::verify_launcher(config, &unit, jars[0])?;
    ensure!(
        unit["User"]
            .as_str()
            .is_some_and(|u| !u.is_empty() && u != "root"),
        "Minecraft must use a non-root service user"
    );
    runtime.trusted_installation(config, &unit)?;
    ensure!(
        ["enabled", "disabled"].contains(&unit_policy(&unit)),
        "Minecraft unit is masked, static or unknown; no automatic unmask or unit rewrite is permitted"
    );
    Ok(unit)
}
fn show(config: &Value, unit: Option<&Value>) -> Result<Value> {
    let policy = read(config, "boot-policy.json")?;
    let hold = read(config, "maintenance.json")?;
    if let Some(record) = &policy {
        validate_record(config, record)?;
    }
    if let Some(record) = &hold {
        validate_record(config, record)?;
    }
    Ok(
        json!({"service":config["deployment"]["service"],"active":unit.and_then(|u|u["ActiveState"].as_str()).unwrap_or("unknown"),"actual_boot_policy":unit.map(unit_policy).unwrap_or("unknown"),"desired_boot_policy":policy.as_ref().and_then(|p|p["desired"].as_str()).unwrap_or("unselected"),"choice_required":policy.as_ref().is_none_or(|p|p["phase"]!="applied"),"policy_phase":policy.as_ref().map(|p|&p["phase"]),"policy_pending":policy.as_ref().is_some_and(|p|p["phase"]!="applied"),"maintenance_hold":hold.is_some(),"maintenance_phase":hold.as_ref().map(|h|&h["phase"]),"effective_suspended":hold.is_some()&&unit.is_some_and(|u|unit_policy(u)=="disabled"),"message":"Boot policy does not start Minecraft now. Persistent maintenance temporarily disables boot and only explicit ready resume restores the desired policy."}),
    )
}
pub fn status(config: &Value) -> Value {
    let unit = SystemRuntime.inspect(config).ok();
    show(config,unit.as_ref()).unwrap_or_else(|_|json!({"service":config["deployment"]["service"],"active":"unknown","actual_boot_policy":"unknown","desired_boot_policy":"unknown","maintenance_hold":true,"choice_required":true,"message":"Boot-policy records require operator inspection; no automatic change was made"}))
}
pub fn set(config: &Value, policy: &str, confirmed: bool) -> Result<Value> {
    set_with(config, policy, confirmed, &mut SystemRuntime)
}
pub(crate) fn set_with(
    config: &Value,
    policy: &str,
    confirmed: bool,
    runtime: &mut impl Runtime,
) -> Result<Value> {
    ensure!(
        confirmed,
        "Explicit Minecraft boot policy confirmation is required"
    );
    ensure!(
        ["enabled", "disabled"].contains(&policy),
        "Unsupported Minecraft boot policy"
    );
    let config = crate::config::validate(config.clone())?;
    let _lock = crate::deploy::lock_state(&config)?;
    ensure!(
        crate::recovery::pending(&config)?.is_none() && !crate::transfer::has_pending(&config)?,
        "Recovery must finish before boot policy changes"
    );
    let before = checked_unit(&config, runtime)?;
    let hold = read(&config, "maintenance.json")?;
    if let Some(record) = &hold {
        validate_record(&config, record)?;
    }
    let desired = json!({"desired":policy,"phase":"preparing","binding":binding(&config)});
    crate::recovery::durable_json(&file(&config, "boot-policy.json")?, &desired)?;
    let effective = if hold.is_some() { "disabled" } else { policy };
    if unit_policy(&before) != effective {
        runtime.change(&config, effective)?;
    }
    let after = checked_unit(&config, runtime)?;
    ensure!(
        unit_policy(&after) == effective,
        "Minecraft boot policy was not verified; the recorded intent remains pending"
    );
    let mut desired = desired;
    desired["phase"] = json!("applied");
    crate::recovery::durable_json(&file(&config, "boot-policy.json")?, &desired)?;
    if let Some(mut hold) = hold {
        hold["desired"] = json!(policy);
        crate::recovery::durable_json(&file(&config, "maintenance.json")?, &hold)?;
    }
    let mut result = show(&config, Some(&after))?;
    result["status"] = json!("completed");
    result["operation"] = json!("minecraft_autostart");
    result["runtime_started"] = json!(false);
    Ok(result)
}
pub(crate) fn maintenance_with(
    config: &Value,
    runtime: &mut impl Runtime,
    before: impl FnOnce() -> Result<()>,
    stop: impl FnOnce() -> Result<()>,
) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    let _lock = crate::deploy::lock_state(&config)?;
    before()?;
    let unit = checked_unit(&config, runtime)?;
    let existing = read(&config, "maintenance.json")?;
    let mut hold = if let Some(record) = existing {
        validate_record(&config, &record)?;
        record
    } else {
        let policy = read(&config, "boot-policy.json")?;
        if let Some(record) = &policy {
            validate_record(&config, record)?;
            ensure!(
                record["phase"] == "applied",
                "Resolve the pending boot policy before maintenance"
            );
        }
        json!({"phase":"preparing","desired":policy.as_ref().and_then(|p|p["desired"].as_str()).unwrap_or(unit_policy(&unit)),"binding":binding(&config)})
    };
    crate::recovery::durable_json(&file(&config, "maintenance.json")?, &hold)?;
    if unit_policy(&unit) != "disabled" {
        runtime.change(&config, "disabled")?;
    }
    let unit = checked_unit(&config, runtime)?;
    ensure!(
        unit_policy(&unit) == "disabled",
        "Persistent maintenance is not established: boot disable failed; Minecraft was not stopped"
    );
    hold["phase"] = json!("boot_disabled");
    crate::recovery::durable_json(&file(&config, "maintenance.json")?, &hold)?;
    if let Err(error) = stop() {
        hold["phase"] = json!("stop_failed");
        crate::recovery::durable_json(&file(&config, "maintenance.json")?, &hold)?;
        return Err(error.context("Boot is disabled and maintenance intent retained; runtime stop failed, inspect actual service state"));
    }
    hold["phase"] = json!("held");
    crate::recovery::durable_json(&file(&config, "maintenance.json")?, &hold)?;
    Ok(
        json!({"status":"completed","operation":"maintenance_stop","maintenance_hold":true,"desired_boot_policy":hold["desired"],"actual_boot_policy":"disabled","players_kicked":false}),
    )
}
pub(crate) fn resume_with(
    config: &Value,
    runtime: &mut impl Runtime,
    start_ready: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    let _lock = crate::deploy::lock_state(&config)?;
    let hold = read(&config, "maintenance.json")?;
    let Some(mut hold) = hold else {
        return start_ready();
    };
    validate_record(&config, &hold)?;
    ensure!(
        crate::recovery::pending(&config)?.is_none() && !crate::transfer::has_pending(&config)?,
        "Recovery must finish before maintenance resume"
    );
    let unit = checked_unit(&config, runtime)?;
    ensure!(
        unit_policy(&unit) == "disabled",
        "Maintenance boot suspension changed externally; restore it before resume"
    );
    if let Some(policy) = read(&config, "boot-policy.json")? {
        validate_record(&config, &policy)?;
        ensure!(
            policy["phase"] == "applied",
            "Resolve pending boot policy before resume"
        );
        hold["desired"] = policy["desired"].clone();
    }
    hold["phase"] = json!("resuming");
    crate::recovery::durable_json(&file(&config, "maintenance.json")?, &hold)?;
    let mut result = match start_ready() {
        Ok(value) => value,
        Err(error) => {
            hold["phase"] = json!("resume_failed");
            crate::recovery::durable_json(&file(&config, "maintenance.json")?, &hold)?;
            return Err(error.context("Persistent maintenance remains held and boot disabled"));
        }
    };
    let desired = hold["desired"]
        .as_str()
        .context("Maintenance desired policy missing")?;
    let restored = (|| -> Result<Value> {
        if desired != "disabled" {
            runtime.change(&config, desired)?;
        }
        let unit = checked_unit(&config, runtime)?;
        ensure!(
            unit_policy(&unit) == desired,
            "Desired boot policy restoration was not verified"
        );
        Ok(unit)
    })();
    let unit = match restored {
        Ok(unit) => unit,
        Err(error) => {
            let suspended = runtime.change(&config, "disabled").is_ok()
                && runtime
                    .inspect(&config)
                    .is_ok_and(|u| unit_policy(&u) == "disabled");
            hold["phase"] = json!("restore_failed");
            hold["boot_suspension_verified"] = json!(suspended);
            crate::recovery::durable_json(&file(&config, "maintenance.json")?, &hold)?;
            return Err(error.context(format!("Minecraft is ready but boot restoration failed; maintenance intent retained, boot_suspension_verified={suspended}. Inspect actual state; stop explicitly before retrying ready resume")));
        }
    };
    let path = file(&config, "maintenance.json")?;
    fs::remove_file(&path)?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    result["minecraft_boot"] = show(&config, Some(&unit))?;
    result["maintenance_hold"] = json!(false);
    Ok(result)
}
pub(crate) fn initial_metadata(config: &Value, result: &mut Value) {
    let boot = status(config);
    result["boot_policy_required"] = boot["choice_required"].clone();
    result["minecraft_boot"] = boot;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Fake {
        policy: String,
        active: Rc<Cell<bool>>,
        deny: Option<String>,
        calls: Vec<String>,
        config: Value,
    }
    impl Runtime for Fake {
        fn inspect(&mut self, _: &Value) -> Result<Value> {
            Ok(
                json!({"Id":self.config["deployment"]["service"],"User":"minecraft","WorkingDirectory":self.config["paths"]["server"],"FragmentPath":"/etc/systemd/system/minecraft.service","ActiveState":if self.active.get(){"active"}else{"inactive"},"UnitFileState":self.policy,"LaunchPath":"/usr/bin/java","LaunchArgv":["/usr/bin/java","-jar","server.jar"],"LaunchFlags":[]}),
            )
        }
        fn trusted_installation(&mut self, _: &Value, _: &Value) -> Result<()> {
            Ok(())
        }
        fn change(&mut self, _: &Value, policy: &str) -> Result<()> {
            self.calls.push(policy.into());
            ensure!(
                self.deny.as_deref() != Some(policy),
                "scoped boot permission denied"
            );
            self.policy = policy.into();
            Ok(())
        }
    }
    fn fixture() -> (tempfile::TempDir, Value, Fake) {
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
        .map(|k| (k.into(), json!(root.path().join(k))))
        .collect();
        let config = crate::config::validate(
            json!({"repository":{"url":"git@github.com:team/config.git"},"paths":paths}),
        )
        .unwrap();
        fs::create_dir_all(root.path().join("server")).unwrap();
        crate::recovery::durable_json(
            &file(&config, "deployment.json").unwrap(),
            &json!({"status":"adopted","commit":"a".repeat(40),"managed_files":["server.jar"]}),
        )
        .unwrap();
        let fake = Fake {
            policy: "enabled".into(),
            active: Rc::new(Cell::new(true)),
            deny: None,
            calls: vec![],
            config: config.clone(),
        };
        (root, config, fake)
    }
    #[test]
    fn adoption_preserves_policy_until_explicit_choice_and_setting_never_starts() {
        let (_root, c, mut r) = fixture();
        let unit = r.inspect(&c).unwrap();
        let view = show(&c, Some(&unit)).unwrap();
        assert_eq!(view["actual_boot_policy"], "enabled");
        assert_eq!(view["choice_required"], true);
        assert!(r.calls.is_empty());
        assert!(set_with(&c, "disabled", false, &mut r).is_err());
        set_with(&c, "disabled", true, &mut r).unwrap();
        assert!(r.active.get());
        assert_eq!(r.policy, "disabled");
        assert_eq!(r.calls, vec!["disabled"]);
    }
    #[test]
    fn maintenance_survives_reboot_and_only_ready_resume_restores_desired() {
        let (_root, c, mut r) = fixture();
        set_with(&c, "enabled", true, &mut r).unwrap();
        let active = r.active.clone();
        maintenance_with(
            &c,
            &mut r,
            || Ok(()),
            || {
                active.set(false);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(r.policy, "disabled");
        r.active.set(r.policy == "enabled");
        assert!(!r.active.get());
        assert!(ensure_not_held(&c).is_err());
        let active = r.active.clone();
        resume_with(&c, &mut r, || {
            active.set(true);
            Ok(json!({"status":"completed"}))
        })
        .unwrap();
        assert!(r.active.get());
        assert_eq!(r.policy, "enabled");
        assert!(!pending(&c).unwrap());
    }
    #[test]
    fn failures_keep_real_boot_suspension_and_never_stop_before_disable() {
        let (_root, c, mut r) = fixture();
        r.deny = Some("disabled".into());
        let stopped = Cell::new(false);
        assert!(
            maintenance_with(
                &c,
                &mut r,
                || Ok(()),
                || {
                    stopped.set(true);
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(!stopped.get());
        let view = show(&c, Some(&r.inspect(&c).unwrap())).unwrap();
        assert_eq!(view["maintenance_hold"], true);
        assert_eq!(view["effective_suspended"], false);
        r.deny = None;
        let active = r.active.clone();
        maintenance_with(
            &c,
            &mut r,
            || Ok(()),
            || {
                active.set(false);
                Ok(())
            },
        )
        .unwrap();
        assert!(resume_with(&c, &mut r, || anyhow::bail!("readiness failed")).is_err());
        assert_eq!(r.policy, "disabled");
        assert!(pending(&c).unwrap());
        r.deny = Some("enabled".into());
        let active = r.active.clone();
        assert!(
            resume_with(&c, &mut r, || {
                active.set(true);
                Ok(json!({"status":"completed"}))
            })
            .is_err()
        );
        assert_eq!(r.policy, "disabled");
        assert!(pending(&c).unwrap());
        assert_eq!(
            read(&c, "maintenance.json").unwrap().unwrap()["phase"],
            "restore_failed"
        );
    }
    #[test]
    fn desired_change_while_held_never_unsuspends_and_disabled_survives_resume() {
        let (_root, c, mut r) = fixture();
        let active = r.active.clone();
        maintenance_with(
            &c,
            &mut r,
            || Ok(()),
            || {
                active.set(false);
                Ok(())
            },
        )
        .unwrap();
        set_with(&c, "enabled", true, &mut r).unwrap();
        assert_eq!(r.policy, "disabled");
        set_with(&c, "disabled", true, &mut r).unwrap();
        let active = r.active.clone();
        resume_with(&c, &mut r, || {
            active.set(true);
            Ok(json!({"status":"completed"}))
        })
        .unwrap();
        assert_eq!(r.policy, "disabled");
        assert!(!pending(&c).unwrap());
    }
    #[test]
    fn online_refusal_makes_no_hold_and_failed_policy_stays_required() {
        let (_root, c, mut r) = fixture();
        assert!(
            maintenance_with(
                &c,
                &mut r,
                || anyhow::bail!("players online"),
                || panic!("must not stop")
            )
            .is_err()
        );
        assert!(!pending(&c).unwrap());
        r.deny = Some("disabled".into());
        assert!(set_with(&c, "disabled", true, &mut r).is_err());
        let unit = r.inspect(&c).unwrap();
        let view = show(&c, Some(&unit)).unwrap();
        assert_eq!(view["choice_required"], true);
        assert_eq!(view["policy_pending"], true);
    }
}
