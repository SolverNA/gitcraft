//! Real CLI refusal path: no host service manager is invoked without explicit confirmation.
use serde_json::json;
use std::{fs, process::Command};
#[test]
fn minecraft_boot_policy_cli_requires_confirmation_and_supported_policy() {
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
    let path = root.path().join("agent.yml");
    fs::write(
        &path,
        serde_json::to_vec(
            &json!({"repository":{"url":"git@github.com:fixture/config.git"},"paths":paths}),
        )
        .unwrap(),
    )
    .unwrap();
    for args in [
        vec!["--policy", "enabled"],
        vec!["--policy", "unsupported", "--confirmed"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_gitcraft"))
            .args(["minecraft-autostart", "--config"])
            .arg(&path)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!root.path().join("state/minecraft-autostart.json").exists());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("confirm") || error.contains("policy") || error.contains("Policy"),
            "{error}"
        );
    }
}
