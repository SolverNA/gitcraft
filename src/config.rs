use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub fn load(path: &Path) -> Result<Value> {
    let value: Value =
        serde_yaml::from_str(&fs::read_to_string(path).context("Read agent configuration")?)?;
    validate(value)
}

pub fn validate(mut value: Value) -> Result<Value> {
    let defaults = json!({
        "repository":{"branch":"main","poll_interval_seconds":60,"authoring_path":""},
        "deployment":{"service":"minecraft.service","empty_grace_seconds":60,"health_timeout_seconds":120},
        "rcon":{"host":"127.0.0.1","port":25575,"password_env":"MINECRAFT_RCON_PASSWORD","timeout_seconds":5},
        "web":{"host":"127.0.0.1","port":8765,"token_env":"GITCRAFT_WEB_TOKEN"},
        "transfer":{"max_session_bytes":107374182400u64,"max_staging_bytes":214748364800u64,"max_files":100000,"min_free_bytes":1073741824,"session_ttl_seconds":604800}
    });
    let object = value
        .as_object_mut()
        .context("Configuration must be a mapping")?;
    for key in object.keys() {
        if ![
            "repository",
            "deployment",
            "rcon",
            "web",
            "paths",
            "transfer",
        ]
        .contains(&key.as_str())
        {
            bail!("Unknown configuration section: {key}");
        }
    }
    for (section, fields) in defaults.as_object().unwrap() {
        let target = object
            .entry(section)
            .or_insert(json!({}))
            .as_object_mut()
            .context("Section must be a mapping")?;
        for key in target.keys() {
            if !(fields.as_object().unwrap().contains_key(key)
                || section == "repository" && key == "url")
            {
                bail!("Unknown setting: {section}.{key}");
            }
        }
        for (key, default) in fields.as_object().unwrap() {
            target.entry(key).or_insert(default.clone());
        }
    }
    let url = value["repository"]["url"]
        .as_str()
        .context("repository.url is required")?;
    if url.is_empty()
        || url.starts_with('-')
        || url.contains('\n')
        || url
            .split_once("://")
            .is_some_and(|(_, rest)| rest.split('/').next().unwrap_or("").contains('@'))
    {
        bail!("Invalid repository URL or embedded credentials");
    }
    let branch = value["repository"]["branch"]
        .as_str()
        .context("Invalid branch")?;
    if branch.is_empty()
        || branch.starts_with('-')
        || branch.contains("..")
        || !branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
    {
        bail!("Invalid branch");
    }
    let paths = value["paths"]
        .as_object()
        .context("paths mapping required")?;
    let keys = [
        "repository",
        "server",
        "state",
        "cache",
        "staging",
        "backups",
    ];
    if paths.len() != keys.len() || keys.iter().any(|key| !paths.contains_key(*key)) {
        bail!("All six managed paths are required");
    }
    let mut resolved: Vec<PathBuf> = Vec::new();
    for key in keys {
        let path = Path::new(paths[key].as_str().context("Path must be a string")?);
        if !path.is_absolute()
            || path == Path::new("/")
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("Managed paths must be absolute non-root paths without ..");
        }
        let mut prefix = PathBuf::new();
        for part in path.components() {
            prefix.push(part);
            if prefix
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                bail!("Managed paths must not contain symlinks");
            }
        }
        let path = path.to_path_buf();
        if resolved
            .iter()
            .any(|other| path.starts_with(other) || other.starts_with(&path))
        {
            bail!("Managed paths must be separate and non-nested");
        }
        resolved.push(path);
    }
    let authoring = value["repository"]["authoring_path"]
        .as_str()
        .context("repository.authoring_path must be a string")?;
    if !authoring.is_empty() {
        let path = Path::new(authoring);
        if !path.is_absolute()
            || path == Path::new("/")
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("Authoring path must be an absolute non-root path without ..");
        }
        if [
            "/etc", "/usr", "/bin", "/sbin", "/proc", "/sys", "/dev", "/run", "/boot",
        ]
        .iter()
        .any(|root| path.starts_with(root))
        {
            bail!("Authoring path must not be in a protected system tree");
        }
        if resolved
            .iter()
            .any(|other| path.starts_with(other) || other.starts_with(path))
        {
            bail!("Authoring clone must be separate from every managed path");
        }
        let mut prefix = PathBuf::new();
        for part in path.components() {
            prefix.push(part);
            if prefix
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                bail!("Authoring path must not contain symlinks");
            }
        }
    }
    for (section, key, min, max) in [
        ("repository", "poll_interval_seconds", 1, 86400),
        ("deployment", "empty_grace_seconds", 0, 86400),
        ("deployment", "health_timeout_seconds", 1, 3600),
        ("rcon", "port", 1, 65535),
        ("rcon", "timeout_seconds", 1, 300),
        ("web", "port", 1, 65535),
        ("transfer", "max_session_bytes", 1, 10995116277760),
        ("transfer", "max_staging_bytes", 1, 21990232555520),
        ("transfer", "max_files", 1, 1000000),
        ("transfer", "min_free_bytes", 1, 1099511627776),
        ("transfer", "session_ttl_seconds", 1, 2592000),
    ] {
        let number = value[section][key]
            .as_u64()
            .context(format!("Invalid {section}.{key}"))?;
        if number < min || number > max {
            bail!("Invalid {section}.{key}");
        }
    }
    if value["transfer"]["max_staging_bytes"].as_u64()
        < value["transfer"]["max_session_bytes"].as_u64()
    {
        bail!("Transfer staging quota must cover one session");
    }
    let service = value["deployment"]["service"]
        .as_str()
        .context("Service must be a string")?;
    if !service.ends_with(".service")
        || service.starts_with('-')
        || !service
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.@-".contains(c))
    {
        bail!("Invalid service name");
    }
    for (section, key) in [("rcon", "password_env"), ("web", "token_env")] {
        let text = value[section][key]
            .as_str()
            .context("Environment reference must be a string")?;
        if text.is_empty()
            || text.chars().next().unwrap().is_ascii_digit()
            || !text
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            bail!("Invalid environment reference");
        }
    }
    for section in ["rcon", "web"] {
        let address: std::net::IpAddr = value[section]["host"]
            .as_str()
            .context("Host required")?
            .parse()?;
        if !address.is_loopback() {
            bail!("{section} must bind/connect to loopback");
        }
    }
    Ok(value)
}

pub fn save(path: &Path, value: Value) -> Result<Value> {
    let value = validate(value)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("Config parent missing")?)?;
    temporary.write_all(serde_yaml::to_string(&value)?.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|e| anyhow::anyhow!(e.error))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Value {
        json!({"repository":{"url":"git@github.com:example/repo.git"},"paths":{"repository":"/tmp/gc/repo","server":"/tmp/gc/server","state":"/tmp/gc/state","cache":"/tmp/gc/cache","staging":"/tmp/gc/staging","backups":"/tmp/gc/backups"}})
    }
    #[test]
    fn defaults_and_invalid_settings() {
        let mut value = sample();
        assert_eq!(validate(value.clone()).unwrap()["web"]["host"], "127.0.0.1");
        value["web"] = json!({"host":"0.0.0.0"});
        assert!(validate(value).is_err());
    }
    #[test]
    fn reject_nested_paths_and_credentials() {
        let mut value = sample();
        value["paths"]["state"] = json!("/tmp/gc/server/state");
        assert!(validate(value).is_err());
        let mut value = sample();
        value["repository"]["url"] = json!("https://token@github.com/org/repo");
        assert!(validate(value).is_err());
    }
    #[test]
    fn atomic_private_roundtrip() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent.yml");
        save(&file, sample()).unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(load(&file).is_ok());
    }
    #[test]
    fn authoring_must_be_separate_and_without_symlinks() {
        let mut value = sample();
        value["repository"]["authoring_path"] = json!("/tmp/gc/server/editor");
        assert!(validate(value).is_err());
        let mut value = sample();
        value["repository"]["authoring_path"] = json!("/etc/gitcraft/editor");
        assert!(validate(value).is_err());
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, root.path().join("alias")).unwrap();
        let mut value = sample();
        value["repository"]["authoring_path"] = json!(root.path().join("alias/clone"));
        assert!(validate(value).is_err());
    }
    #[test]
    fn managed_symlink_cannot_alias_the_authoring_clone() {
        let root = tempfile::tempdir().unwrap();
        let authoring = root.path().join("authoring");
        fs::create_dir(&authoring).unwrap();
        let alias = root.path().join("live-alias");
        std::os::unix::fs::symlink(&authoring, &alias).unwrap();
        let mut value = sample();
        value["repository"]["authoring_path"] = json!(authoring);
        value["paths"]["server"] = json!(alias);
        assert!(validate(value).is_err());
    }
}
