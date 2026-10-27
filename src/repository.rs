//! Create review branches and PRs; never push a deployment branch directly.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Write,
    path::Path,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
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
        repo.join(".git").is_dir(),
        "Proposals require a normal configuration working clone, not the agent's bare repository"
    );
    crate::policy::validate_repository(repo)?;
    command(repo, "gh", &["auth", "status"])?;
    let origin = command(repo, "git", &["remote", "get-url", "origin"])?;
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
    command(repo, "git", &["commit", "-m", title])?;
    let commit = command(repo, "git", &["rev-parse", "HEAD"])?;
    command(
        repo,
        "git",
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
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
    use std::fs;
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
}
