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
    fs::write(
        &gh,
        r#"#!/bin/sh
if [ "$1" = auth ]; then exit 0; fi
if [ "$2" = list ]; then printf "[]\n"; exit 0; fi
printf "%s\n" https://github.com/fixture/config/pull/1
"#,
    )
    .unwrap();
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

fn retry_fixture(
    gh_script: &str,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
) {
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
    fs::write(&gh, gh_script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    (temp, repo, remote, tools, approved)
}
#[test]
fn pushed_proposal_can_retry_pr_without_new_commit_push_or_duplicate() {
    let script = r#"#!/bin/sh
if [ "$1" = auth ]; then exit 0; fi
if [ "$2" = list ]; then
 if [ -f .git/mock-pr-created ]; then
  sha=$(/usr/bin/git rev-parse HEAD)
  state=OPEN; if [ -f .git/mock-pr-state ]; then state=$(cat .git/mock-pr-state); fi
  printf '[{"url":"https://github.com/fixture/config/pull/2","state":"%s","baseRefName":"release","headRefOid":"%s"}]\n' "$state" "$sha"
 else printf '[]\n'; fi
 exit 0
fi
if [ "$2" = create ]; then
 printf 'create\n' >> .git/mock-pr-create-count
 if [ ! -f .git/mock-pr-attempt ]; then touch .git/mock-pr-attempt; printf 'secret-output-must-not-leak\n' >&2; exit 1; fi
 touch .git/mock-pr-created
 printf '%s\n' https://github.com/fixture/config/pull/2
 exit 0
fi
exit 2
"#;
    let (_temp, repo, remote, tools, approved) = retry_fixture(script);
    use std::os::unix::fs::PermissionsExt;
    fs::write(tools.join("git"),"#!/bin/sh\ncase \"$1\" in commit|push) printf '%s\\n' \"$1\" >> .git/mock-mutating-git ;; esac\nexec /usr/bin/git \"$@\"\n").unwrap();
    fs::set_permissions(tools.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_gitcraft"))
            .args(args)
            .env(
                "PATH",
                format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap()
    };
    let first = run(&[
        "propose",
        "--repository",
        repo.to_str().unwrap(),
        "--base",
        "release",
        "--title",
        "Retry fixture",
    ]);
    assert!(!first.status.success());
    assert!(!String::from_utf8_lossy(&first.stderr).contains("secret-output-must-not-leak"));
    let status: Value = serde_json::from_slice(
        &run(&["propose-status", "--repository", repo.to_str().unwrap()]).stdout,
    )
    .unwrap();
    assert_eq!(status["status"], "pr_pending");
    assert_eq!(status["pushed"], true);
    let branch = status["branch"].as_str().unwrap();
    let commit = status["commit"].as_str().unwrap();
    assert_eq!(
        git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]),
        commit
    );
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/release"]), approved);
    let before = fs::read(repo.join(".git/mock-mutating-git")).unwrap();
    let count = git(&repo, &["rev-list", "--all", "--count"]);
    fs::write(
        repo.join("server-settings/server.properties"),
        "motd=uncommitted-later\n",
    )
    .unwrap();
    let retry = run(&[
        "propose-retry",
        "--repository",
        repo.to_str().unwrap(),
        "--base",
        "release",
    ]);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let result: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(result["status"], "proposed");
    assert_eq!(result["commit"], commit);
    assert_eq!(
        before,
        fs::read(repo.join(".git/mock-mutating-git")).unwrap()
    );
    assert_eq!(count, git(&repo, &["rev-list", "--all", "--count"]));
    assert!(
        fs::read_to_string(repo.join("server-settings/server.properties"))
            .unwrap()
            .contains("uncommitted-later")
    );
    let repeated = run(&[
        "propose-retry",
        "--repository",
        repo.to_str().unwrap(),
        "--base",
        "release",
    ]);
    assert!(repeated.status.success());
    assert_eq!(
        before,
        fs::read(repo.join(".git/mock-mutating-git")).unwrap()
    );
    let creates = fs::read(repo.join(".git/mock-pr-create-count")).unwrap();
    for (state, status) in [("CLOSED", "pr_closed"), ("MERGED", "pr_merged")] {
        fs::write(repo.join(".git/mock-pr-state"), state).unwrap();
        let output = run(&[
            "propose-retry",
            "--repository",
            repo.to_str().unwrap(),
            "--base",
            "release",
        ]);
        assert!(output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["status"], status);
        assert_eq!(
            creates,
            fs::read(repo.join(".git/mock-pr-create-count")).unwrap()
        );
    }
    assert!(
        !run(&[
            "propose-retry",
            "--repository",
            repo.to_str().unwrap(),
            "--base",
            "main"
        ])
        .status
        .success()
    );
    git(
        &remote,
        &["update-ref", &format!("refs/heads/{branch}"), &approved],
    );
    assert!(
        !run(&[
            "propose-retry",
            "--repository",
            repo.to_str().unwrap(),
            "--base",
            "release"
        ])
        .status
        .success()
    );
    assert_eq!(
        before,
        fs::read(repo.join(".git/mock-mutating-git")).unwrap()
    );
}

#[test]
fn rejected_push_retains_verified_local_operation_and_retry_never_pushes_missing_ref() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, repo, remote, tools, _approved) =
        retry_fixture("#!/bin/sh\nif [ \"$1\" = auth ]; then exit 0; fi\nprintf '[]\\n'\n");
    let hook = remote.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_gitcraft"))
            .args(args)
            .env(
                "PATH",
                format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap()
    };
    assert!(
        !run(&[
            "propose",
            "--repository",
            repo.to_str().unwrap(),
            "--base",
            "release",
            "--title",
            "Rejected push"
        ])
        .status
        .success()
    );
    let status: Value = serde_json::from_slice(
        &run(&["propose-status", "--repository", repo.to_str().unwrap()]).stdout,
    )
    .unwrap();
    assert_eq!(status["status"], "local_verified");
    assert_eq!(status["pushed"], false);
    let commit = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(status["commit"], commit);
    assert!(
        !run(&[
            "propose-retry",
            "--repository",
            repo.to_str().unwrap(),
            "--base",
            "release"
        ])
        .status
        .success()
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), commit);
    assert!(
        git(
            &remote,
            &["for-each-ref", "--format=%(refname)", "refs/heads/gitcraft"]
        )
        .is_empty()
    );
}
