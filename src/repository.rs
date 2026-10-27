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
    audit_tree(repo, tree)?;
    Ok(tree.to_owned())
}
fn audit_tree(repo: &Path, tree: &str) -> Result<()> {
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
    Ok(())
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
struct ProposalLock(std::fs::File);
impl Drop for ProposalLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
fn proposal_lock(repo: &Path) -> Result<ProposalLock> {
    ensure!(
        repo.join(".git").is_dir() && !repo.join(".git").is_symlink(),
        "Normal Git clone required"
    );
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(crate::deploy::safe_path(
            repo,
            ".git/gitcraft-propose.lock",
        )?)?;
    file.try_lock_exclusive()
        .context("Another configuration proposal is running")?;
    Ok(ProposalLock(file))
}
fn operation_path(repo: &Path) -> Result<PathBuf> {
    crate::deploy::safe_path(repo, ".git/gitcraft-proposal.json")
}
fn save_operation(repo: &Path, operation: &Value) -> Result<()> {
    crate::recovery::durable_json(&operation_path(repo)?, operation)
}
pub fn proposal_status(repo: &Path) -> Result<Value> {
    let path = operation_path(repo)?;
    if !path.exists() {
        return Ok(json!({"status":"none"}));
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        value["kind"] == "gitcraft-proposal-v1",
        "Unknown proposal journal"
    );
    Ok(value)
}
fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !branch.contains("..")
        && !branch.ends_with('/')
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}
fn remote_sha(repo: &Path, branch: &str) -> Result<String> {
    let output = command(
        repo,
        "git",
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let rows: Vec<_> = output.lines().collect();
    ensure!(
        rows.len() == 1,
        "Expected review branch is missing or ambiguous on origin; retry never pushes"
    );
    let (sha, reference) = rows[0].split_once('\t').context("Malformed remote ref")?;
    ensure!(
        reference == format!("refs/heads/{branch}"),
        "Unexpected remote reference"
    );
    Ok(sha.into())
}
fn finish_pr(repo: &Path, operation: &mut Value) -> Result<Value> {
    let branch = operation["branch"]
        .as_str()
        .context("Review branch missing")?
        .to_owned();
    let base = operation["base"]
        .as_str()
        .context("Base missing")?
        .to_owned();
    let origin = operation["origin"]
        .as_str()
        .context("Origin missing")?
        .to_owned();
    let commit = operation["commit"]
        .as_str()
        .context("Commit missing")?
        .to_owned();
    let existing = command(
        repo,
        "gh",
        &[
            "pr",
            "list",
            "--repo",
            &origin,
            "--head",
            &branch,
            "--state",
            "all",
            "--json",
            "url,state,baseRefName,headRefOid",
            "--limit",
            "100",
        ],
    );
    let rows: Vec<Value> = match existing.and_then(|raw| Ok(serde_json::from_str(&raw)?)) {
        Ok(rows) => rows,
        Err(_) => {
            operation["status"] = json!("pr_pending");
            operation["error"] = json!("Could not check existing GitHub PRs; output omitted");
            save_operation(repo, operation)?;
            anyhow::bail!(
                "Review branch is pushed; existing-PR lookup failed. Use propose-retry after fixing GitHub access"
            );
        }
    };
    ensure!(
        rows.len() <= 1,
        "Multiple PRs reference this review branch; inspect manually before retry"
    );
    if let Some(pr) = rows.first() {
        ensure!(
            pr["baseRefName"] == base && pr["headRefOid"] == commit,
            "Existing PR target or immutable commit differs; no duplicate was created"
        );
        let status = match pr["state"].as_str() {
            Some("OPEN") => "proposed",
            Some("CLOSED") => "pr_closed",
            Some("MERGED") => "pr_merged",
            _ => anyhow::bail!("Unknown existing PR state"),
        };
        let url = checked_pr_url(pr["url"].as_str().context("PR URL missing")?, &origin)?;
        operation["status"] = json!(status);
        operation["url"] = json!(url);
        operation["error"] = Value::Null;
        save_operation(repo, operation)?;
        return Ok(operation.clone());
    }
    let mut body = tempfile::NamedTempFile::new()?;
    writeln!(
        body,
        "Configuration changes for deployment branch `{base}`.\n\nBase commit: `{}`\nProposed commit: `{commit}`\n\nOnly an approved merge into the configured deployment branch authorizes deployment. Worlds and JAR bytes stay outside Git.",
        operation["base_commit"].as_str().unwrap_or("")
    )?;
    body.as_file().sync_all()?;
    let title = operation["title"].as_str().context("Title missing")?;
    match command(
        repo,
        "gh",
        &[
            "pr",
            "create",
            "--repo",
            &origin,
            "--base",
            &base,
            "--head",
            &branch,
            "--title",
            title,
            "--body-file",
            body.path().to_str().context("Body path")?,
        ],
    ) {
        Ok(raw) => {
            let url = checked_pr_url(raw.trim(), &origin)?;
            operation["status"] = json!("proposed");
            operation["url"] = json!(url);
            operation["error"] = Value::Null;
            save_operation(repo, operation)?;
            Ok(operation.clone())
        }
        Err(_) => {
            operation["status"] = json!("pr_pending");
            operation["error"] =
                json!("GitHub PR creation failed after verified push; remote output omitted");
            save_operation(repo, operation)?;
            anyhow::bail!(
                "Review branch is pushed; PR creation failed. Use propose-retry; no new commit or push is required"
            )
        }
    }
}
fn checked_pr_url(url: &str, origin: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url)?;
    let prefix = format!("/{origin}/pull/");
    ensure!(
        parsed.scheme() == "https"
            && parsed.host_str() == Some("github.com")
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed
                .path()
                .to_ascii_lowercase()
                .strip_prefix(&prefix)
                .is_some_and(
                    |number| !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit())
                ),
        "Unexpected PR URL; remote output omitted"
    );
    Ok(url.into())
}
pub fn retry_proposal(repo: &Path, expected_base: &str) -> Result<Value> {
    let _lock = proposal_lock(repo)?;
    let mut operation = proposal_status(repo)?;
    let result = retry_checked(repo, expected_base, &mut operation);
    if result.is_err()
        && operation["kind"] == "gitcraft-proposal-v1"
        && operation["error"].is_null()
    {
        operation["status"] = json!("retry_blocked");
        operation["error"] = json!(
            "Retry validation failed; inspect origin, review SHA, target branch and GitHub access. No commit or push was performed; output omitted."
        );
        save_operation(repo, &operation)?;
    }
    result
}
fn retry_checked(repo: &Path, expected_base: &str, operation: &mut Value) -> Result<Value> {
    operation["error"] = Value::Null;
    ensure!(
        operation["kind"] == "gitcraft-proposal-v1",
        "No proposal operation to retry"
    );
    let branch = operation["branch"]
        .as_str()
        .context("Review branch missing")?;
    let base = operation["base"].as_str().context("Base missing")?;
    ensure!(
        valid_branch(expected_base)
            && base == expected_base
            && valid_branch(branch)
            && branch.starts_with("gitcraft/config-")
            && branch != base,
        "Retry target or review branch differs from expected deployment policy"
    );
    let origin = command(repo, "git", &["config", "--get", "remote.origin.url"])?;
    ensure!(
        github_name(&origin)? == operation["origin"].as_str().context("Origin missing")?,
        "Origin changed since verified proposal"
    );
    let commit = operation["commit"].as_str().context("Commit missing")?;
    ensure!(
        commit.len() == 40 && commit.bytes().all(|c| c.is_ascii_hexdigit()),
        "Invalid recorded commit"
    );
    let tree = command(
        repo,
        "git",
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
    )?;
    ensure!(
        tree.trim() == operation["tree"].as_str().context("Tree missing")?,
        "Recorded local commit tree differs"
    );
    audit_tree(repo, tree.trim())?;
    ensure!(
        remote_sha(repo, branch)? == commit,
        "Pushed review branch SHA differs; no new push or PR was attempted"
    );
    command(repo, "gh", &["auth", "status"])?;
    operation["pushed"] = json!(true);
    operation["status"] = json!("pushed");
    operation["error"] = Value::Null;
    save_operation(repo, operation)?;
    finish_pr(repo, operation)
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
    let _lock = proposal_lock(repo)?;
    let previous = proposal_status(repo)?;
    ensure!(
        !["local_verified", "pushed", "pr_pending", "retry_blocked"]
            .contains(&previous["status"].as_str().unwrap_or("")),
        "An earlier verified proposal is pending; inspect proposal-status and use propose-retry rather than creating another commit"
    );
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
    let mut operation = json!({"kind":"gitcraft-proposal-v1","status":"local_verified","origin":github_name(origin)?,"branch":branch,"base":base,"base_commit":expected,"commit":commit.trim(),"tree":reviewed_tree,"title":title,"pushed":false,"deployment_branch_pushed":false,"error":null});
    save_operation(repo, &operation)?;
    if command(
        repo,
        "git",
        &[
            "push",
            "origin",
            &format!("{}:refs/heads/{branch}", commit.trim()),
        ],
    )
    .is_err()
    {
        operation["error"] =
            json!("Push failed; remote output omitted. Retry never pushes a missing branch.");
        save_operation(repo, &operation)?;
        anyhow::bail!(
            "Verified local proposal retained; push failed. Inspect proposal-status before any manual recovery"
        );
    }
    ensure!(
        remote_sha(repo, &branch)? == commit.trim(),
        "Pushed review SHA verification failed; operation retained"
    );
    operation["pushed"] = json!(true);
    operation["status"] = json!("pushed");
    save_operation(repo, &operation)?;
    let result = finish_pr(repo, &mut operation);
    if result.is_err() && operation["error"].is_null() {
        operation["status"] = json!("pr_pending");
        operation["error"] = json!(
            "PR response could not be verified; inspect existing PR before retry. Output omitted."
        );
        save_operation(repo, &operation)?;
    }
    result
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
