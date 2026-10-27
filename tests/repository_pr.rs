use serde_json::Value;
use std::{fs, path::Path, process::Command};
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
#[test]
fn proposal_pushes_only_review_branch_and_keeps_deployment_head() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=release"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    fs::create_dir(repo.join("server-settings")).unwrap();
    fs::write(
        repo.join("server-settings/server.properties"),
        "motd=before\n",
    )
    .unwrap();
    git(&repo, &["add", "server-settings/server.properties"]);
    git(&repo, &["commit", "-m", "initial"]);
    let approved = git(&repo, &["rev-parse", "HEAD"]);
    let output = Command::new("git")
        .args(["init", "--bare"])
        .arg(&remote)
        .output()
        .unwrap();
    assert!(output.status.success());
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:fixture/config.git",
        ],
    );
    git(
        &repo,
        &[
            "config",
            &format!("url.file://{}.insteadOf", remote.to_str().unwrap()),
            "git@github.com:fixture/config.git",
        ],
    );
    git(&repo, &["push", "origin", "release"]);
    fs::write(
        repo.join("server-settings/server.properties"),
        "motd=after\n",
    )
    .unwrap();
    let tools = temp.path().join("tools");
    fs::create_dir(&tools).unwrap();
    let gh = tools.join("gh");
    fs::write(&gh,"#!/bin/sh\nif [ \"$1\" = auth ]; then exit 0; fi\nprintf '%s\\n' https://github.com/fixture/config/pull/1\n").unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_gitcraft"))
        .args([
            "propose",
            "--repository",
            repo.to_str().unwrap(),
            "--base",
            "release",
            "--title",
            "Change MOTD",
        ])
        .env(
            "PATH",
            format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["base"], "release");
    assert_eq!(result["base_commit"], approved);
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/release"]), approved);
    let branch = result["branch"].as_str().unwrap();
    assert!(branch.starts_with("gitcraft/config-"));
    assert_ne!(
        git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]),
        approved
    );
    assert_eq!(git(&repo, &["rev-parse", "release"]), approved);
    assert_eq!(result["deployment_branch_pushed"], false);
}
