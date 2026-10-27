//! Create review branches and PRs; never push a deployment branch directly.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn audit_index(repo: &Path) -> Result<String> {
    let tree = command(repo, "git", &["write-tree"])?;
    let tree = tree.trim();
    let listing = command(repo, "git", &["ls-tree", "-rz", "--full-tree", tree])?;
    let snapshot = tempfile::tempdir()?;
    for record in listing.split('\0').filter(|record| !record.is_empty()) {
        let (metadata, path) = record.split_once('\t').context("Malformed staged tree")?;
        crate::policy::relative(path)?;
        let fields: Vec<_> = metadata.split_whitespace().collect();
        ensure!(
            fields.len() == 3 && fields[1] == "blob" && ["100644", "100755"].contains(&fields[0]),
            "Symlinks and special Git objects are forbidden"
        );
        let content = crate::gitrepo::run_bounded(
            Command::new("git")
                .current_dir(repo)
                .args(["cat-file", "blob", fields[2]]),
            Duration::from_secs(30),
        )?;
        ensure!(
            content.status.success() && content.stdout.len() < 1024 * 1024,
            "Staged file exceeds exact-audit limit"
        );
        let target = snapshot.path().join(path);
        fs::create_dir_all(target.parent().context("Snapshot parent")?)?;
        fs::write(target, content.stdout)?;
    }
    command(snapshot.path(), "git", &["init", "--quiet"])?;
    command(
        snapshot.path(),
        "git",
        &["add", "--all", "--force", "--", "."],
    )?;
    crate::policy::validate_repository(snapshot.path())?;
    Ok(tree.to_owned())
}
fn command(repo: &Path, program: &str, args: &[&str]) -> Result<String> {
    let output=crate::gitrepo::run_bounded(Command::new(program).current_dir(repo).args(args).env("GIT_TERMINAL_PROMPT","0").env("GIT_SSH_COMMAND","ssh -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=2"),Duration::from_secs(180))?;
    ensure!(
        output.status.success(),
        "{program} operation failed; inspect local authentication and repository permissions (remote output omitted)"
    );
    ensure!(
        output.stdout.len() < 1024 * 1024,
        "Command output exceeds safe limit"
    );
    Ok(String::from_utf8(output.stdout)?)
}
fn github_name(url: &str) -> Result<String> {
    let value = url
        .trim()
        .strip_prefix("git@github.com:")
        .or_else(|| url.trim().strip_prefix("https://github.com/"))
        .context("Authoring origin must use GitHub SSH or credential-free HTTPS")?;
    let value = value.strip_suffix(".git").unwrap_or(value);
    let parts: Vec<_> = value.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|part| !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))),
        "Invalid GitHub repository identity"
    );
    Ok(value.to_ascii_lowercase())
}
/// The Web editor receives only a preconfigured, separate clone bound to the deployment origin.
pub fn authoring_repository(config: &Value) -> Result<PathBuf> {
    let config = crate::config::validate(config.clone())?;
    let path = config["repository"]["authoring_path"]
        .as_str()
        .context("Authoring clone configuration missing")?;
    ensure!(
        !path.is_empty(),
        "Configure repository.authoring_path before using the editor"
    );
    let path = PathBuf::from(path);
    ensure!(
        path.join(".git").is_dir() && !path.join(".git").is_symlink(),
        "Authoring requires a normal Git clone without a symlink control directory"
    );
    let origin = command(&path, "git", &["config", "--get", "remote.origin.url"])?;
    ensure!(
        github_name(&origin)?
            == github_name(
                config["repository"]["url"]
                    .as_str()
                    .context("Deployment URL missing")?
            )?,
        "Authoring origin differs from configured deployment repository"
    );
    crate::policy::validate_repository(&path)?;
    Ok(path)
}
pub fn propose(repo: &Path, base: &str, title: &str) -> Result<Value> {
    ensure!(
        !title.trim().is_empty() && title.len() <= 200 && !title.contains(['\n', '\r']),
        "PR title must be a single bounded line"
    );
    ensure!(
        !base.is_empty()
            && base.starts_with(|c: char| c.is_ascii_alphanumeric())
            && !base.contains("..")
            && !base.ends_with('/')
            && base
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c)),
        "Invalid deployment branch"
    );
    ensure!(
        repo.join(".git").is_dir() && !repo.join(".git").is_symlink(),
        "Proposals require a normal configuration working clone, not the agent's bare repository"
    );
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(repo.join(".git/gitcraft-propose.lock"))?;
    lock.try_lock_exclusive()
        .context("Another configuration proposal is running")?;
    crate::policy::validate_repository(repo)?;
    command(repo, "gh", &["auth", "status"])?;
    let origin = command(repo, "git", &["config", "--get", "remote.origin.url"])?;
    let origin = origin.trim();
    ensure!(
        origin.starts_with("git@github.com:") || origin.starts_with("https://github.com/"),
        "PR origin must be GitHub SSH or credential-free HTTPS"
    );
    command(
        repo,
        "git",
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("refs/heads/{base}"),
        ],
    )?;
    let expected = command(
        repo,
        "git",
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
    )?;
    let expected = expected.trim();
    let head = command(repo, "git", &["rev-parse", "--verify", "HEAD^{commit}"])?;
    ensure!(
        head.trim() == expected,
        "Working clone HEAD must equal fetched deployment branch before proposing edits; rebase or create a clean editing clone"
    );
    let files = command(
        repo,
        "git",
        &["diff", "--name-only", "--no-renames", "-z", "HEAD"],
    )?;
    let untracked = command(
        repo,
        "git",
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    let paths: BTreeSet<_> = files
        .split('\0')
        .chain(untracked.split('\0'))
        .filter(|v| !v.is_empty())
        .collect();
    ensure!(!paths.is_empty(), "No local edits to propose");
    for path in &paths {
        crate::policy::relative(path)?;
        ensure!(
            crate::policy::MANAGED_ROOTS
                .iter()
                .any(|root| path.starts_with(&format!("{root}/")))
                || [
                    "requirements/engine.json",
                    "requirements/plugins.json",
                    "requirements/mods.json",
                    "export-policy.json"
                ]
                .contains(path),
            "Proposal may change only approved configs, dependency pins and export-policy; workflow/code edits need a separate reviewed process"
        );
    }
    let branch = format!(
        "gitcraft/config-{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        std::process::id()
    );
    command(repo, "git", &["checkout", "-b", &branch])?;
    // Stage only audited changes, including explicit removals. Never git add .
    let mut stage = vec!["add", "--all", "--"];
    stage.extend(paths.iter().copied());
    command(repo, "git", &stage)?;
    crate::policy::validate_repository(repo)?;
    let reviewed_tree = audit_index(repo)?;
    command(repo, "git", &["commit", "-m", title])?;
    ensure!(
        command(repo, "git", &["rev-parse", "HEAD^{tree}"])?.trim() == reviewed_tree,
        "Committed tree changed after validation; refusing to push"
    );
    let commit = command(repo, "git", &["rev-parse", "HEAD"])?;
    command(
        repo,
        "git",
        &[
            "push",
            "origin",
            &format!("{}:refs/heads/{branch}", commit.trim()),
        ],
    )?;
    let mut body = tempfile::NamedTempFile::new()?;
    writeln!(
        body,
        "Configuration changes for deployment branch `{base}`.\n\nBase commit: `{expected}`\nProposed commit: `{}`\n\nOnly merging this PR into the configured deployment branch authorizes deployment. Review configuration values, source provenance, hashes and compatibility. Worlds, JAR bytes and runtime data are excluded.",
        commit.trim()
    )?;
    body.as_file().sync_all()?;
    let url = command(
        repo,
        "gh",
        &[
            "pr",
            "create",
            "--base",
            base,
            "--head",
            &branch,
            "--title",
            title,
            "--body-file",
            body.path().to_str().context("Temporary body path")?,
        ],
    )?;
    Ok(
        json!({"status":"proposed","branch":branch,"base":base,"base_commit":expected,"commit":commit.trim(),"url":url.trim(),"deployment_branch_pushed":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_branch_and_bare_repo_fail_before_network() {
        let temp = tempfile::tempdir().unwrap();
        assert!(propose(temp.path(), "--main", "test").is_err());
        assert!(propose(temp.path(), "main", "test").is_err());
    }
    #[test]
    fn proposal_never_allows_unsafe_files_before_auth() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join("README.md"), "safe").unwrap();
        assert!(propose(temp.path(), "main", "\nunsafe").is_err());
    }
    #[test]
    fn authoring_origin_is_bound_and_control_directory_cannot_be_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("editor");
        fs::create_dir(&repo).unwrap();
        command(&repo, "git", &["init", "--quiet"]).unwrap();
        command(
            &repo,
            "git",
            &["remote", "add", "origin", "git@github.com:other/config.git"],
        )
        .unwrap();
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
        let config = json!({"repository":{"url":"https://github.com/team/config.git","authoring_path":repo},"paths":paths});
        assert!(authoring_repository(&config).is_err());
        command(
            &repo,
            "git",
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:team/config.git",
            ],
        )
        .unwrap();
        assert!(authoring_repository(&config).is_ok());
        let actual = root.path().join("git-control");
        fs::rename(repo.join(".git"), &actual).unwrap();
        std::os::unix::fs::symlink(actual, repo.join(".git")).unwrap();
        assert!(authoring_repository(&config).is_err());
        assert!(propose(&repo, "main", "Unsafe control").is_err());
    }
}
