use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
/// Bound external tools without pipe backpressure; kill the entire process group on timeout.
pub(crate) fn run_bounded(command: &mut Command, timeout: Duration) -> Result<Output> {
    let mut stdout = tempfile::tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            bail!("External command timed out");
        }
        thread::sleep(Duration::from_millis(20));
    };
    stdout.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    stdout.take(1024 * 1024).read_to_end(&mut bytes)?;
    Ok(Output {
        status,
        stdout: bytes,
        stderr: Vec::new(),
    })
}
fn ssh_command(config: Option<&str>) -> Result<String> {
    let extra = if let Some(path) = config {
        if !path.starts_with('/')
            || path.contains("..")
            || !path
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/._-".contains(&c))
        {
            bail!("SSH configuration path must be absolute with no shell metacharacters");
        }
        format!(" -F {path}")
    } else {
        String::new()
    };
    Ok(format!(
        "ssh{extra} -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=2"
    ))
}
fn git(args: &[&str]) -> Result<String> {
    let config = std::env::var("GITCRAFT_SSH_CONFIG").ok();
    let output = run_bounded(
        Command::new("git")
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", ssh_command(config.as_deref())?),
        Duration::from_secs(180),
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeout_kills_and_reaps() {
        let start = Instant::now();
        assert!(
            run_bounded(
                Command::new("sh").args(["-c", "sleep 10"]),
                Duration::from_millis(50)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn large_output_has_no_pipe_deadlock() {
        let result = run_bounded(
            Command::new("sh").args(["-c", "yes x | head -c 2000000"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout.len(), 1024 * 1024);
    }
}

#[cfg(test)]
mod ssh_tests {
    use super::*;
    #[test]
    fn configured_identity_route_cannot_inject_shell() {
        assert!(
            ssh_command(Some("/etc/gitcraft/ssh.conf"))
                .unwrap()
                .contains(" -F /etc/gitcraft/ssh.conf ")
        );
        assert!(ssh_command(Some("/tmp/a;curl attacker")).is_err());
        assert!(ssh_command(Some("relative.conf")).is_err());
    }
}
