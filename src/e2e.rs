//! Full local scenario. Real Git, export, hashes and transfer bytes; service/RCON are fixtures.
use crate::{artifacts, config, deploy, export, rcon::RconClient, transfer};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::os::unix::process::ExitStatusExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
struct Runtime {
    remote: PathBuf,
    online: u32,
    active: bool,
    actions: Vec<String>,
    snapshots: u64,
}
impl deploy::Runtime for Runtime {
    fn online(&mut self) -> Result<u32> {
        Ok(self.online)
    }
    fn command(&mut self, _: &str) -> Result<String> {
        Ok("saved".into())
    }
    fn service(&mut self, action: &str, _: &str) -> Result<Output> {
        self.actions.push(action.into());
        match action {
            "start" => self.active = true,
            "stop" => self.active = false,
            _ => (),
        };
        Ok(Output {
            status: std::process::ExitStatus::from_raw(if self.active { 0 } else { 3 << 8 }),
            stdout: if self.active {
                b"active\n".to_vec()
            } else {
                b"inactive\n".to_vec()
            },
            stderr: vec![],
        })
    }
    fn snapshot(
        &mut self,
        _: &str,
        branch: &str,
        _: &Path,
        staging: &Path,
    ) -> Result<(String, PathBuf)> {
        self.snapshots += 1;
        fs::create_dir_all(staging)?;
        let target = staging.join(format!("snapshot-{}", self.snapshots));
        let output = Command::new("git")
            .args(["clone", "--quiet", "--branch", branch])
            .arg(&self.remote)
            .arg(&target)
            .output()?;
        ensure!(output.status.success(), "Fixture clone failed");
        let commit = git(&target, &["rev-parse", "HEAD"]);
        Ok((commit, target))
    }
    fn healthy(&mut self, _: &str, _: u64) -> Result<()> {
        ensure!(self.active, "Fixture service stopped");
        Ok(())
    }
}
fn transport(config: &Value, source: &Path, kind: &str) {
    use sha2::{Digest, Sha256};
    let plan = transfer::plan(source, kind, "stopped").unwrap();
    let manifest: transfer::Manifest = serde_json::from_value(plan["manifest"].clone()).unwrap();
    let session = transfer::create(config, manifest.clone()).unwrap();
    let id = session["id"].as_str().unwrap();
    let token = session["token"].as_str().unwrap();
    let base = Path::new(plan["source"].as_str().unwrap());
    for (index, file) in manifest.files.iter().enumerate() {
        let bytes = fs::read(base.join(&file.path)).unwrap();
        for (offset, chunk) in bytes.chunks(transfer::CHUNK_BYTES).enumerate() {
            let offset = (offset * transfer::CHUNK_BYTES) as u64;
            let hash = hex::encode(Sha256::digest(chunk));
            transfer::chunk(config, id, token, index, offset, &hash, chunk).unwrap();
            if offset == 0 {
                assert_eq!(
                    transfer::chunk(config, id, token, index, offset, &hash, chunk).unwrap()["replayed"],
                    true
                );
            }
        }
    }
    transfer::verify(config, id, token).unwrap();
    transfer::commit_with(config, id, token, true, || Ok(())).unwrap();
}
#[test]
fn local_export_transfer_pr_merge_and_exact_sha_deploy() {
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("local");
    fs::create_dir_all(local.join("plugins/Example/userdata")).unwrap();
    fs::create_dir_all(local.join("mods")).unwrap();
    fs::create_dir_all(local.join("config")).unwrap();
    fs::create_dir_all(local.join("defaultconfigs")).unwrap();
    fs::create_dir_all(local.join("OurRealm")).unwrap();
    for (path, content) in [
        (
            "server.properties",
            "level-name=OurRealm\nmotd=first\nrcon.password=local-secret\n",
        ),
        ("plugins/Example/config.yml", "enabled: true\n"),
        ("plugins/Example/userdata/player.yml", "private: player\n"),
        ("config/example.cfg", "enabled=true\n"),
        ("defaultconfigs/example.toml", "enabled=true\n"),
    ] {
        fs::write(local.join(path), content).unwrap();
    }
    for path in ["server.jar", "plugins/Example.jar", "mods/ExampleMod.jar"] {
        fs::write(local.join(path), format!("fixture bytes {path}")).unwrap();
    }
    fs::write(local.join("OurRealm/level.dat"), "migrated world").unwrap();
    let metadata = root.path().join("metadata.json");
    fs::write(&metadata,serde_json::to_vec(&json!({"artifacts":[{"path":"server.jar","name":"Engine","version":"local-1","source":{"provider":"transfer"}},{"path":"plugins/Example.jar","name":"Example","version":"local-1","source":{"provider":"transfer"}},{"path":"mods/ExampleMod.jar","name":"ExampleMod","version":"local-1","source":{"provider":"transfer"}}]})).unwrap()).unwrap();
    let plan = export::plan(
        &local,
        &[
            "server.properties".into(),
            "plugins/Example/config.yml".into(),
            "config/example.cfg".into(),
            "defaultconfigs/example.toml".into(),
        ],
        Some(&metadata),
    )
    .unwrap();
    assert!(plan["blocked"].as_array().unwrap().is_empty());
    assert!(!plan.to_string().contains("local-secret"));
    let plan_path = root.path().join("export.json");
    fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
    let repo = root.path().join("configs");
    export::apply(&plan_path, &repo, true).unwrap();
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "-m", "Initial approved configuration"]);
    git(&repo, &["branch", "-m", "release"]);
    let initial = git(&repo, &["rev-parse", "HEAD"]);
    let remote = root.path().join("remote.git");
    assert!(
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote)
            .output()
            .unwrap()
            .status
            .success()
    );
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "origin", "release"]);
    let paths: serde_json::Map<_, _> = [
        "repository",
        "server",
        "state",
        "cache",
        "staging",
        "backups",
    ]
    .into_iter()
    .map(|key| (key.into(), json!(root.path().join("vps").join(key))))
    .collect();
    let config=config::validate(json!({"repository":{"url":"git@github.com:fixture/configs.git","branch":"release"},"paths":paths,"deployment":{"empty_grace_seconds":0}})).unwrap();
    for key in ["server", "state", "backups"] {
        fs::create_dir_all(Path::new(config["paths"][key].as_str().unwrap())).unwrap();
    }
    transport(&config, &local.join("OurRealm"), "world");
    for (path, kind) in [
        ("server.jar", "engine"),
        ("plugins/Example.jar", "plugin"),
        ("mods/ExampleMod.jar", "mod"),
    ] {
        transport(&config, &local.join(path), kind);
    }
    let server = Path::new(config["paths"]["server"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(server.join("OurRealm/level.dat")).unwrap(),
        "migrated world"
    );
    for artifact in artifacts::load_manifest(&repo).unwrap() {
        assert!(
            artifacts::download(
                &artifact,
                Path::new(config["paths"]["cache"].as_str().unwrap())
            )
            .is_ok()
        );
    }
    let client = RconClient {
        host: "127.0.0.1".into(),
        port: 25575,
        password: "vps-secret".into(),
        timeout: Duration::from_secs(1),
    };
    let mut runtime = Runtime {
        remote,
        online: 0,
        active: false,
        actions: vec![],
        snapshots: 0,
    };
    let result = deploy::run_transaction(&config, true, &client, &mut runtime).unwrap();
    assert_eq!(result["commit"], initial);
    assert!(server.join("config/example.cfg").exists());
    assert!(server.join("defaultconfigs/example.toml").exists());
    assert!(!server.join("plugins/Example/userdata").exists());
    git(&repo, &["checkout", "-b", "petya/change-motd"]);
    fs::write(
        repo.join("server-settings/server.properties"),
        "level-name=OurRealm\nmotd=approved-second\n",
    )
    .unwrap();
    git(&repo, &["add", "server-settings/server.properties"]);
    git(&repo, &["commit", "-m", "Proposed MOTD"]);
    git(&repo, &["push", "origin", "petya/change-motd"]);
    let unchanged = deploy::run_transaction(&config, false, &client, &mut runtime).unwrap();
    assert_eq!(unchanged["status"], "unchanged");
    assert!(
        fs::read_to_string(server.join("server.properties"))
            .unwrap()
            .contains("motd=first")
    );
    git(&repo, &["checkout", "release"]);
    git(
        &repo,
        &[
            "merge",
            "--no-ff",
            "petya/change-motd",
            "-m",
            "Approved merge",
        ],
    );
    git(&repo, &["push", "origin", "release"]);
    let approved = git(&repo, &["rev-parse", "HEAD"]);
    runtime.online = 2;
    runtime.actions.clear();
    let waiting = deploy::run_transaction(&config, false, &client, &mut runtime).unwrap();
    assert_eq!(waiting["status"], "waiting_for_players");
    assert!(!runtime.actions.iter().any(|v| v == "stop"));
    runtime.online = 0;
    let deployed = deploy::run_transaction(&config, false, &client, &mut runtime).unwrap();
    assert_eq!(deployed["commit"], approved);
    assert_eq!(git(&repo, &["rev-parse", "release"]), approved);
    assert!(
        fs::read_to_string(server.join("server.properties"))
            .unwrap()
            .contains("motd=approved-second")
    );
    assert_eq!(
        fs::read_to_string(server.join("OurRealm/level.dat")).unwrap(),
        "migrated world"
    );
    let cache = Path::new(config["paths"]["cache"].as_str().unwrap());
    let engine = artifacts::load_manifest(&repo)
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Engine")
        .unwrap();
    fs::write(cache.join(format!("{}.jar", engine.sha256)), b"corrupted").unwrap();
    git(
        &repo,
        &["commit", "--allow-empty", "-m", "Trigger verification"],
    );
    git(&repo, &["push", "origin", "release"]);
    runtime.actions.clear();
    assert!(deploy::run_transaction(&config, false, &client, &mut runtime).is_err());
    assert!(!runtime.actions.iter().any(|v| v == "stop"));
    assert_eq!(
        fs::read_to_string(server.join("OurRealm/level.dat")).unwrap(),
        "migrated world"
    );
    let installed: Value = serde_json::from_slice(
        &fs::read(Path::new(config["paths"]["state"].as_str().unwrap()).join("deployment.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["commit"], approved);
}
