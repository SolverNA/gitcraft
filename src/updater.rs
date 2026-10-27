//! Resolve stable, compatible releases into reviewed, exact dependency pins.
use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};
use std::{fs, io::Read, path::Path, time::Duration};

const MAX_METADATA: u64 = 16 * 1024 * 1024;
const MAX_ARTIFACT: u64 = 512 * 1024 * 1024;

fn https(url: &str) -> Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url)?;
    ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.fragment().is_none(),
        "dependency URL must be HTTPS without credentials or fragments"
    );
    Ok(parsed)
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        .user_agent("GitCraft/0.1 (Minecraft GitOps)")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || https(attempt.url().as_str()).is_err() {
                attempt.error("unsafe dependency redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .with_context(|| format!("missing {key}"))
}

fn fetch_versions(client: &Client, source: &Value) -> Result<Vec<Value>> {
    let project = required(source, "project")?;
    ensure!(
        project
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
        "invalid Modrinth project"
    );
    let minecraft = required(source, "minecraft")?;
    let loader = required(source, "loader")?;
    ensure!(
        source["channel"].as_str().unwrap_or("release") == "release",
        "only stable updates are supported"
    );
    let mut url = https(&format!(
        "https://api.modrinth.com/v2/project/{project}/version"
    ))?;
    url.query_pairs_mut()
        .append_pair("game_versions", &json!([minecraft]).to_string())
        .append_pair("loaders", &json!([loader]).to_string())
        .append_pair("include_changelog", "false");
    let mut data = Vec::new();
    client
        .get(url)
        .send()?
        .error_for_status()?
        .take(MAX_METADATA + 1)
        .read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 <= MAX_METADATA,
        "Modrinth metadata exceeds limit"
    );
    Ok(serde_json::from_slice(&data)?)
}

fn release_file(release: &Value) -> Result<&Value> {
    let files: Vec<_> = release["files"]
        .as_array()
        .context("release files missing")?
        .iter()
        .filter(|f| {
            f["filename"].as_str().unwrap_or("").ends_with(".jar")
                && !matches!(
                    f["file_type"].as_str(),
                    Some("sources-jar" | "dev-jar" | "javadoc-jar" | "signature")
                )
        })
        .collect();
    let primary: Vec<_> = files
        .iter()
        .copied()
        .filter(|f| f["primary"] == true)
        .collect();
    if primary.len() == 1 {
        Ok(primary[0])
    } else if files.len() == 1 {
        Ok(files[0])
    } else {
        bail!("ambiguous release JAR files; pin manually")
    }
}

fn hash_release(client: &Client, file: &Value) -> Result<String> {
    let expected = required(&file["hashes"], "sha512")?;
    ensure!(
        expected.len() == 128 && expected.bytes().all(|c| c.is_ascii_hexdigit()),
        "missing valid provider SHA-512"
    );
    let declared = file["size"].as_u64().context("missing file size")?;
    ensure!(
        declared > 0 && declared <= MAX_ARTIFACT,
        "artifact exceeds size limit"
    );
    let response = client
        .get(https(required(file, "url")?)?)
        .send()?
        .error_for_status()?;
    hash_reader(response, declared, expected)
}

fn hash_reader(mut reader: impl Read, declared: u64, expected: &str) -> Result<String> {
    let (mut sha256, mut sha512, mut size) = (Sha256::new(), Sha512::new(), 0u64);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        ensure!(
            size <= declared && size <= MAX_ARTIFACT,
            "artifact exceeds declared size"
        );
        sha256.update(&buffer[..count]);
        sha512.update(&buffer[..count]);
    }
    ensure!(
        size == declared && hex::encode(sha512.finalize()).eq_ignore_ascii_case(expected),
        "artifact failed size or SHA-512 verification"
    );
    Ok(hex::encode(sha256.finalize()))
}

fn select_release<'a>(artifact: &Value, versions: &'a [Value]) -> Result<Option<&'a Value>> {
    let source = &artifact["source"];
    let minecraft = required(source, "minecraft")?;
    let loader = required(source, "loader")?;
    let mut releases: Vec<_> = versions
        .iter()
        .filter(|v| {
            v["version_type"] == "release"
                && v["status"] == "listed"
                && v["game_versions"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(minecraft)))
                && v["loaders"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(loader)))
        })
        .collect();
    ensure!(!releases.is_empty(), "no stable compatible release");
    releases.sort_by(|a, b| {
        b["date_published"]
            .as_str()
            .cmp(&a["date_published"].as_str())
    });
    let pinned = &artifact["pinned"];
    let current = releases
        .iter()
        .find(|v| {
            if let Some(id) = pinned["version_id"].as_str() {
                v["id"] == id
            } else {
                v["version_number"] == pinned["version"]
            }
        })
        .context("pinned version absent from compatible releases; refusing possible downgrade")?;
    Ok(if current["id"] == releases[0]["id"] {
        None
    } else {
        Some(releases[0])
    })
}

fn update_artifact(client: &Client, artifact: &mut Value) -> Result<Option<Value>> {
    match artifact["source"]["provider"].as_str().unwrap_or("direct") {
        "direct" => return Ok(None),
        "modrinth" => (),
        _ => bail!("unsupported update provider"),
    }
    let versions = fetch_versions(client, &artifact["source"])?;
    let Some(release) = select_release(artifact, &versions)? else {
        return Ok(None);
    };
    let file = release_file(release)?;
    let hash = hash_release(client, file)?;
    let detail = json!({"name":artifact["name"], "from":artifact["pinned"]["version"],
        "to":release["version_number"], "sha256":hash});
    artifact["pinned"] = json!({"version":required(release,"version_number")?,
        "version_id":required(release,"id")?, "url":required(file,"url")?, "sha256":hash});
    Ok(Some(detail))
}

pub fn update_repository(repository: &Path) -> Result<Value> {
    crate::artifacts::load_manifest(repository)?;
    let client = client()?;
    let (mut pending, mut updates) = (Vec::new(), Vec::new());
    for (filename, key) in [
        ("engine.json", None),
        ("plugins.json", Some("plugins")),
        ("mods.json", Some("mods")),
    ] {
        let path = repository.join("requirements").join(filename);
        if !path.exists() {
            continue;
        }
        ensure!(!path.is_symlink(), "manifest symlinks are forbidden");
        let mut value: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let mut changed = false;
        if let Some(key) = key {
            for artifact in value[key]
                .as_array_mut()
                .context("manifest must contain array")?
            {
                if let Some(detail) = update_artifact(&client, artifact)? {
                    updates.push(detail);
                    changed = true;
                }
            }
        } else if let Some(detail) = update_artifact(&client, &mut value)? {
            updates.push(detail);
            changed = true;
        }
        if changed {
            pending.push((path, value));
        }
    }
    let mut changed = Vec::new();
    // Resolve every release before writing any manifest. Publication is handled by PR workflow.
    for (path, value) in pending {
        let mut temporary =
            tempfile::NamedTempFile::new_in(path.parent().context("manifest parent")?)?;
        use std::io::Write;
        writeln!(temporary, "{}", serde_json::to_string_pretty(&value)?)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path)?;
        changed.push(path.strip_prefix(repository)?.to_string_lossy().to_string());
    }
    Ok(json!({"changed":changed,"updates":updates}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn artifact() -> Value {
        json!({"name":"Test","pinned":{"version":"1"},"source":{"minecraft":"1.21","loader":"paper"}})
    }
    fn release(id: &str, date: &str) -> Value {
        json!({"id":id,"version_number":id,"date_published":date,
        "version_type":"release","status":"listed","game_versions":["1.21"],"loaders":["paper"]})
    }
    #[test]
    fn stable_compatible_selection() {
        let mut beta = release("3", "2024-03-01");
        beta["version_type"] = json!("beta");
        let mut incompatible = release("4", "2024-04-01");
        incompatible["loaders"] = json!(["fabric"]);
        let versions = vec![
            release("1", "2024-01-01"),
            release("2", "2024-02-01"),
            beta,
            incompatible,
        ];
        assert_eq!(
            select_release(&artifact(), &versions).unwrap().unwrap()["id"],
            "2"
        );
    }
    #[test]
    fn unknown_pin_refuses_downgrade() {
        assert!(select_release(&artifact(), &[release("0", "2023-01-01")]).is_err());
    }
    #[test]
    fn same_release_unchanged() {
        assert!(
            select_release(&artifact(), &[release("1", "2023-01-01")])
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn provider_hash_and_size_required() {
        let digest = hex::encode(Sha512::digest(b"jar"));
        assert_eq!(
            hash_reader(&b"jar"[..], 3, &digest).unwrap(),
            hex::encode(Sha256::digest(b"jar"))
        );
        assert!(hash_reader(&b"bad"[..], 3, &digest).is_err());
        assert!(hash_reader(&b"long"[..], 3, &digest).is_err());
        assert!(hash_reader(&b"ja"[..], 3, &digest).is_err());
    }
    #[test]
    fn unsafe_urls_rejected() {
        for url in [
            "http://example.org/a.jar",
            "https://token@example.org/a.jar",
            "https://example.org/a.jar#fragment",
        ] {
            assert!(https(url).is_err());
        }
    }
    #[test]
    fn ambiguous_release_requires_manual_pin() {
        assert!(
            release_file(&json!({"files":[{"filename":"a.jar"},{"filename":"b.jar"}]})).is_err()
        );
    }
}
