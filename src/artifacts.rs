use anyhow::{Context, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Artifact {
    pub name: String,
    pub destination: String,
    pub url: String,
    pub sha256: String,
    pub version: String,
    pub transfer: bool,
}
pub fn validate_url(url: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(url)?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        bail!("Artifact URL must be HTTPS without credentials");
    }
    Ok(u)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("Missing manifest field {key}"))
}
pub fn load_manifest(repo: &Path) -> Result<Vec<Artifact>> {
    let mut result = Vec::new();
    if repo
        .join("requirements")
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        bail!("Manifest directory symlink forbidden");
    }
    for category in ["engine", "plugins", "mods"] {
        let path = repo.join("requirements").join(format!("{category}.json"));
        if !path.exists() {
            continue;
        }
        if path.symlink_metadata()?.file_type().is_symlink() {
            bail!("Manifest symlink forbidden");
        }
        let data: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let entries = if category == "engine" {
            vec![&data]
        } else {
            data[category]
                .as_array()
                .context("Manifest must contain an array")?
                .iter()
                .collect()
        };
        for e in entries {
            let filename = text(e, "filename")?;
            if !filename.ends_with(".jar")
                || !filename
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
                || !filename
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                bail!("Unsafe artifact filename");
            }
            let pinned = &e["pinned"];
            let digest = text(pinned, "sha256")?;
            if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
                bail!("Full SHA-256 required");
            }
            let transfer = pinned["transfer"].as_bool().unwrap_or(false);
            let url = if transfer {
                if pinned.get("url").is_some() {
                    bail!("Transfer pin cannot include URL");
                }
                ""
            } else {
                let url = text(pinned, "url")?;
                validate_url(url)?;
                url
            };
            let destination = if category == "engine" {
                filename.to_string()
            } else {
                format!("{category}/{filename}")
            };
            if result
                .iter()
                .any(|a: &Artifact| a.destination == destination)
            {
                bail!("Duplicate artifact destination");
            }
            result.push(Artifact {
                name: text(e, "name")?.into(),
                destination,
                url: url.into(),
                sha256: digest.to_lowercase(),
                version: text(pinned, "version")?.into(),
                transfer,
            });
        }
    }
    Ok(result)
}
pub fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub fn download(artifact: &Artifact, cache: &Path) -> Result<PathBuf> {
    if artifact.sha256.len() != 64 || !artifact.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid artifact digest");
    }
    fs::create_dir_all(cache)?;
    if artifact.transfer {
        return verified_transfer(artifact, cache);
    }
    let target = cache.join(format!("{}.jar", artifact.sha256));
    if target.exists()
        && !target.symlink_metadata()?.file_type().is_symlink()
        && sha256(&target)? == artifact.sha256
    {
        return Ok(target);
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() > 10 {
                attempt.error("Too many redirects")
            } else if validate_url(attempt.url().as_str()).is_err() {
                attempt.error("Insecure redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()?;
    let mut response = client
        .get(validate_url(&artifact.url)?)
        .header("User-Agent", "minecraft-gitops/0.1")
        .send()?
        .error_for_status()?;
    let mut temp = tempfile::NamedTempFile::new_in(cache)?;
    let mut total = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > 512 * 1024 * 1024 {
            bail!("Artifact exceeds size limit");
        }
        digest.update(&buffer[..count]);
        temp.write_all(&buffer[..count])?;
    }
    if format!("{:x}", digest.finalize()) != artifact.sha256 {
        bail!("Artifact SHA-256 mismatch");
    }
    temp.as_file().sync_all()?;
    temp.persist(&target)?;
    Ok(target)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_credentials() {
        assert!(validate_url("http://example.org/x.jar").is_err());
        assert!(validate_url("https://u:p@example.org/x.jar").is_err());
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
/// Import bytes through the independent transport channel. A Git pin alone never authorizes unverified local bytes.
pub fn import_local(source: &Path, cache: &Path, expected_sha: &str) -> Result<Value> {
    if expected_sha.len() != 64 || !expected_sha.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("Expected SHA-256 is required");
    }
    if source.symlink_metadata()?.file_type().is_symlink() || !source.is_file() {
        bail!("Local artifact must be a regular non-symlink file");
    }
    fs::create_dir_all(cache)?;
    let expected_sha = expected_sha.to_lowercase();
    let mut incoming = tempfile::NamedTempFile::new_in(cache)?;
    let size = std::io::copy(
        &mut File::open(source)?.take(512 * 1024 * 1024 + 1),
        &mut incoming,
    )?;
    if size > 512 * 1024 * 1024 {
        bail!("Local artifact exceeds size limit");
    }
    incoming.as_file().sync_all()?;
    if sha256(incoming.path())? != expected_sha {
        bail!("Transferred artifact SHA-256 mismatch");
    }
    let target = cache.join(format!("{expected_sha}.jar"));
    incoming.persist(&target)?;
    sync_directory(cache)?;
    let receipt = serde_json::json!({"sha256":expected_sha,"size":size,"cache_file":format!("{expected_sha}.jar"),"verified":true});
    let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
    serde_json::to_writer_pretty(&mut temporary, &receipt)?;
    temporary.as_file().sync_all()?;
    temporary.persist(cache.join(format!("{expected_sha}.receipt.json")))?;
    sync_directory(cache)?;
    Ok(receipt)
}
fn verified_transfer(artifact: &Artifact, cache: &Path) -> Result<PathBuf> {
    let target = cache.join(format!("{}.jar", artifact.sha256));
    let receipt_path = cache.join(format!("{}.receipt.json", artifact.sha256));
    if target.symlink_metadata()?.file_type().is_symlink()
        || receipt_path.symlink_metadata()?.file_type().is_symlink()
    {
        bail!("Transfer cache symlinks forbidden");
    }
    let receipt: Value = serde_json::from_slice(&fs::read(receipt_path)?)?;
    if receipt["verified"] != true
        || receipt["sha256"] != artifact.sha256
        || receipt["cache_file"] != format!("{}.jar", artifact.sha256)
        || receipt["size"].as_u64() != Some(target.metadata()?.len())
        || sha256(&target)? != artifact.sha256
    {
        bail!("Transferred artifact receipt/hash verification failed");
    }
    Ok(target)
}
#[cfg(test)]
mod transfer_tests {
    use super::*;
    #[test]
    fn receipt_and_hash_both_required() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input.jar");
        fs::write(&source, b"known").unwrap();
        let digest = sha256(&source).unwrap();
        let cache = temp.path().join("cache");
        let artifact = Artifact {
            name: "local".into(),
            destination: "server.jar".into(),
            url: String::new(),
            sha256: digest.clone(),
            version: "1".into(),
            transfer: true,
        };
        import_local(&source, &cache, &digest).unwrap();
        assert!(download(&artifact, &cache).is_ok());
        fs::write(cache.join(format!("{digest}.jar")), b"wrong").unwrap();
        assert!(download(&artifact, &cache).is_err());
    }
    #[test]
    fn wrong_import_never_creates_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input.jar");
        fs::write(&source, b"wrong").unwrap();
        let cache = temp.path().join("cache");
        assert!(import_local(&source, &cache, &"0".repeat(64)).is_err());
        assert_eq!(fs::read_dir(cache).unwrap().count(), 0);
    }
}
