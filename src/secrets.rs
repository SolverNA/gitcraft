//! Local write-only credential provider. Values never belong in config or operation previews.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
fn store(config: &Value) -> Result<Option<PathBuf>> {
    let path = config["secrets"]["store_path"].as_str().unwrap_or("");
    if path.is_empty() {
        Ok(None)
    } else {
        let path = PathBuf::from(path);
        crate::deploy::safe_path(
            path.parent().context("Secret store parent missing")?,
            path.file_name()
                .and_then(|v| v.to_str())
                .context("Secret filename invalid")?,
        )?;
        Ok(Some(path))
    }
}
fn load(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let metadata = path.symlink_metadata()?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.permissions().mode() & 0o077 == 0
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1,
        "Credential store must be a private owned regular file"
    );
    ensure!(metadata.len() <= 16384, "Credential store too large");
    let value: Value = serde_json::from_slice(&fs::read(path)?)
        .map_err(|_| anyhow::anyhow!("Invalid local credential store; values omitted"))?;
    ensure!(
        value.as_object().is_some_and(|m| m.keys().all(|key| [
            "rcon_password",
            "web_token",
            "generation"
        ]
        .contains(&key.as_str()))),
        "Invalid credential roles"
    );
    for (key, value) in value.as_object().unwrap() {
        let secret = value
            .as_str()
            .context("Invalid credential data; values omitted")?;
        ensure!(
            !secret.is_empty() && secret.len() <= 4096 && !secret.contains(['\n', '\r', '\0']),
            "Invalid credential data; values omitted"
        );
        if key == "web_token" {
            ensure!(secret.len() >= 24, "Invalid Web credential; values omitted");
        }
        if key == "generation" {
            ensure!(
                secret.len() == 64 && secret.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Invalid credential generation"
            );
        }
    }
    Ok(value)
}
pub fn resolve(config: &Value, role: &str) -> Result<String> {
    let (key, section, reference) = match role {
        "rcon" => ("rcon_password", "rcon", "password_env"),
        "web" => ("web_token", "web", "token_env"),
        _ => anyhow::bail!("Unsupported credential role"),
    };
    if let Some(path) = store(config)?
        && let Some(value) = load(&path)?[key].as_str()
    {
        return Ok(value.to_owned());
    }
    let name = config[section][reference]
        .as_str()
        .context("Credential reference missing")?;
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!(
            "Credential unavailable for role {role}; configure the local write-only provider"
        )
    })
}
pub fn status(config: &Value) -> Value {
    json!({"rcon_configured":resolve(config,"rcon").is_ok_and(|v|!v.is_empty()),"web_configured":resolve(config,"web").is_ok_and(|v|v.len()>=24),"provider":"local private store, then environment; values are never returned"})
}
pub fn write(config: &Value, role: &str, value: &str) -> Result<Value> {
    let key = match role {
        "rcon" => "rcon_password",
        "web" => "web_token",
        _ => anyhow::bail!("Unsupported credential role"),
    };
    ensure!(
        !value.is_empty() && value.len() <= 4096 && !value.contains(['\n', '\r', '\0']),
        "Invalid credential; values omitted"
    );
    if role == "web" {
        ensure!(
            value.len() >= 24,
            "Web credential needs at least 24 characters"
        );
    }
    let path = store(config)?.context("Local credential store path is not configured")?;
    let parent = path.parent().context("Credential parent missing")?;
    ensure!(parent.is_dir(), "Credential directory missing");
    let lock_path = parent.join(".credentials.lock");
    crate::deploy::safe_path(parent, ".credentials.lock")?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)?;
    lock.try_lock_exclusive()
        .context("Credential update is already running")?;
    let mut current = load(&path)?;
    current[key] = json!(value);
    use rand::RngCore;
    let mut revision = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut revision);
    current["generation"] = json!(hex::encode(revision));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec(&current)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path)?;
    fs::File::open(parent)?.sync_all()?;
    FileExt::unlock(&lock)?;
    Ok(json!({"status":"completed","role":role,"stored":true,"values_returned":false}))
}
pub fn generation(config: &Value) -> Result<String> {
    if let Some(path) = store(config)? {
        let value = load(&path)?;
        if let Some(generation) = value["generation"].as_str() {
            return Ok(generation.to_owned());
        }
        if path.exists() {
            let metadata = path.metadata()?;
            return Ok(format!(
                "legacy-{}-{}-{}-{}",
                metadata.ino(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.len()
            ));
        }
    }
    Ok("environment".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_write_only_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let value = json!({"secrets":{"store_path":root.path().join("credentials.json")}});
        let result = write(&value, "rcon", "not-returned-secret").unwrap();
        assert!(!result.to_string().contains("not-returned-secret"));
        assert_eq!(resolve(&value, "rcon").unwrap(), "not-returned-secret");
        assert_eq!(
            fs::metadata(root.path().join("credentials.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!status(&value).to_string().contains("not-returned-secret"));
    }
    #[test]
    fn symlink_and_public_permissions_refused() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("secret.json");
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let config = json!({"secrets":{"store_path":path}});
        assert!(write(&config, "rcon", "secret").is_err());
    }
    #[test]
    fn hardlinks_and_malformed_private_provider_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store.json");
        let config = json!({"secrets":{"store_path":path},"rcon":{"password_env":"PATH"}});
        fs::write(&path, r#"{"rcon_password":42}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(resolve(&config, "rcon").is_err());
        fs::write(&path, "{}").unwrap();
        fs::hard_link(&path, root.path().join("linked.json")).unwrap();
        assert!(write(&config, "rcon", "private-value").is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.path().join("linked.json"), &path).unwrap();
        assert!(write(&config, "rcon", "private-value").is_err());
    }
}
