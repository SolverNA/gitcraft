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
            if category == "engine" && filename != "server.jar" {
                bail!("Engine filename must be server.jar");
            }
            let pinned = &e["pinned"];
            let digest = text(pinned, "sha256")?;
            if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
                bail!("Full SHA-256 required");
            }
            let url = text(pinned, "url")?;
            validate_url(url)?;
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
    fs::create_dir_all(cache)?;
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
