use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !output.status.success() {
        bail!("Git operation failed (credentials and remote output omitted)");
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
pub fn snapshot(
    url: &str,
    branch: &str,
    repository: &Path,
    staging: &Path,
) -> Result<(String, PathBuf)> {
    if !(url.starts_with("https://") || url.starts_with("ssh://") || url.starts_with("git@")) {
        bail!("Git remote must use HTTPS or SSH");
    }
    if branch.is_empty()
        || !branch.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
        || branch.contains("..")
        || branch.ends_with('/')
    {
        bail!("Invalid Git branch");
    }
    fs::create_dir_all(repository.parent().context("Repository needs parent")?)?;
    let repository = repository.to_str().context("Invalid repository path")?;
    if !Path::new(repository).join("HEAD").exists() {
        git(&["init", "--bare", repository])?;
    }
    git(&[
        "-C",
        repository,
        "fetch",
        "--no-tags",
        "--",
        url,
        &format!("refs/heads/{branch}"),
    ])?;
    let commit = git(&[
        "-C",
        repository,
        "rev-parse",
        "--verify",
        "FETCH_HEAD^{commit}",
    ])?;
    fs::create_dir_all(staging)?;
    let target = staging.join(&commit);
    if target.symlink_metadata().is_ok() {
        if target.symlink_metadata()?.file_type().is_symlink() {
            bail!("Staging symlink forbidden");
        }
        fs::remove_dir_all(&target)?;
    }
    let dest = target.to_str().context("Invalid staging path")?;
    git(&["clone", "--no-checkout", "--", repository, dest])?;
    git(&["-C", dest, "checkout", "--detach", &commit])?;
    Ok((commit, target))
}
