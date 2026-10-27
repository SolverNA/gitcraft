//! Conservative shared export, CI and deployment policy. Not a complete secret scanner.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
    process::Command,
    time::Duration,
};

pub const MANAGED_ROOTS: [&str; 4] = [
    "server-settings",
    "plugins-configs",
    "config",
    "defaultconfigs",
];
pub const IGNORE: &str = "# Defense in depth: validation also scans tracked files.\n*.jar\n*.log\n*.db\n*.sqlite\n*.sqlite3\n*.mca\n*.mcr\n*.dat\n*.dat_old\n*.lock\n*.zip\n*.tar\n*.gz\n.env\n.env.*\nsecrets.env\n**/logs/\n**/cache/\n**/caches/\n**/backups/\n**/playerdata/\n**/advancements/\n**/stats/\n**/userdata/\n**/database/\n**/databases/\n**/storage/\n**/data/\n**/region/\n**/entities/\n**/poi/\n**/level.dat\n**/session.lock\n/world*/\n/eula.txt\n/usercache.json\n/ops.json\n/whitelist.json\n/banned-*.json\n";
const MAX_CONFIG: u64 = 2 * 1024 * 1024;

pub fn relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.contains('\\')
            && !path.contains('\0')
            && !path.contains('\n')
            && !path.contains('\r'),
        "Unsafe relative path"
    );
    ensure!(
        Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_))),
        "Unsafe relative path"
    );
    Ok(())
}
pub fn runtime_path(path: &str) -> bool {
    if relative(path).is_err() {
        return true;
    }
    let lowered = path.to_ascii_lowercase();
    let pieces: Vec<_> = lowered.split('/').collect();
    if pieces.iter().any(|p| {
        [
            "logs",
            "cache",
            "caches",
            "backups",
            "backup",
            "playerdata",
            "advancements",
            "stats",
            "userdata",
            "database",
            "databases",
            "storage",
            "data",
            "region",
            "entities",
            "poi",
            ".git",
            ".ssh",
        ]
        .contains(p)
    }) {
        return true;
    }
    let name = pieces.last().copied().unwrap_or("");
    name == ".env"
        || name.starts_with(".env.")
        || name.contains("secret")
        || name == "eula.txt"
        || name == "ops.json"
        || name == "whitelist.json"
        || name == "usercache.json"
        || name.starts_with("banned-")
        || name == "level.dat"
        || name == "session.lock"
        || [
            "jar", "log", "db", "sqlite", "sqlite3", "mca", "mcr", "dat", "dat_old", "lock", "zip",
            "tar", "gz", "key", "pem", "p12", "keystore",
        ]
        .iter()
        .any(|extension| name.ends_with(&format!(".{extension}")))
}
pub fn config_path(path: &str) -> Result<()> {
    relative(path)?;
    ensure!(
        !runtime_path(path),
        "Runtime, world, binary or secret path is forbidden: {path}"
    );
    let (root, tail) = path
        .split_once('/')
        .context("Config must be under a managed root")?;
    ensure!(
        MANAGED_ROOTS.contains(&root) && !tail.is_empty(),
        "Unsupported config mapping: {path}"
    );
    if root == "server-settings" {
        ensure!(
            !tail.contains('/'),
            "Server settings must be top-level files"
        );
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    ensure!(
        [
            "yml",
            "yaml",
            "json",
            "properties",
            "toml",
            "conf",
            "cfg",
            "txt"
        ]
        .contains(&extension),
        "Unsupported config extension: {path}"
    );
    Ok(())
}
fn sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_', '.'], "");
    [
        "password",
        "passwd",
        "token",
        "secret",
        "apikey",
        "privatekey",
        "authorization",
        "webhook",
        "connectionstring",
    ]
    .iter()
    .any(|s| key.contains(s))
}
fn empty(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(|v| v.trim().is_empty())
}
fn inspect_value(value: &Value) -> Result<()> {
    match value {
        Value::Object(mapping) => {
            for (key, value) in mapping {
                ensure!(
                    !sensitive(key) || empty(value),
                    "Potential secret value; sanitize manually before export (value omitted)"
                );
                inspect_value(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                inspect_value(value)?;
            }
        }
        Value::String(value) => inspect_text(value)?,
        _ => (),
    }
    Ok(())
}
pub fn validate_metadata(value: &Value) -> Result<()> {
    inspect_value(value)
}
fn inspect_text(text: &str) -> Result<()> {
    for part in text.split_whitespace() {
        if let Some((_, rest)) = part.split_once("://") {
            ensure!(
                !rest.split('/').next().unwrap_or("").contains('@'),
                "Credential-bearing URL is forbidden (value omitted)"
            );
            let url_text = part
                .find("https://")
                .or_else(|| part.find("http://"))
                .map(|offset| &part[offset..])
                .unwrap_or(part)
                .trim_matches(['\'', '"', ',']);
            if let Ok(url) = reqwest::Url::parse(url_text) {
                ensure!(
                    !url.query_pairs().any(|(key, _)| sensitive(&key)),
                    "Credential-like URL parameter is forbidden (value omitted)"
                );
            }
        }
    }
    ensure!(
        !text.contains("-----BEGIN") || !text.contains("PRIVATE KEY-----"),
        "Private key material is forbidden"
    );
    for prefix in ["ghp_", "gho_", "github_pat_", "AKIA"] {
        ensure!(
            !text.contains(prefix),
            "Potential access credential is forbidden (value omitted)"
        );
    }
    Ok(())
}
pub fn validate_config_file(path: &str, content: &str) -> Result<()> {
    config_path(path)?;
    ensure!(
        content.len() as u64 <= MAX_CONFIG && !content.contains('\0'),
        "Config must be bounded text: {path}"
    );
    inspect_text(content)?;
    match Path::new(path).extension().and_then(|s| s.to_str()) {
        Some("json") => inspect_value(
            &serde_json::from_str::<Value>(content).context("Invalid JSON configuration")?,
        )?,
        Some("yaml" | "yml") => inspect_value(
            &serde_yaml::from_str::<Value>(content).context("Invalid YAML configuration")?,
        )?,
        Some("toml") => inspect_value(&serde_json::to_value(
            toml::from_str::<toml::Value>(content).context("Invalid TOML configuration")?,
        )?)?,
        _ => {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with(['#', ';']) {
                    continue;
                }
                if let Some((key, value)) = line.split_once(['=', ':']) {
                    ensure!(
                        !sensitive(key) || value.trim().trim_matches(['\'', '"']).is_empty(),
                        "Potential secret setting; sanitize manually before export (value omitted)"
                    );
                }
            }
        }
    }
    Ok(())
}
pub fn sanitize_properties(content: &str) -> (String, Vec<String>) {
    let mut output = String::new();
    let mut removed = Vec::new();
    for line in content.lines() {
        let key = line
            .trim_start()
            .split(['=', ':', ' ', '\t'])
            .next()
            .unwrap_or("");
        if key == "rcon.password" {
            removed.push("rcon.password".into());
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
    (output, removed)
}
pub fn configured_worlds(content: &str) -> BTreeSet<String> {
    let mut worlds: BTreeSet<String> = ["world", "world_nether", "world_the_end"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "level-name"
        {
            let value = value.trim();
            if relative(value).is_ok() {
                for suffix in ["", "_nether", "_the_end"] {
                    worlds.insert(format!("{value}{suffix}"));
                }
            }
        }
    }
    worlds
}
fn known_config(path: &str) -> bool {
    matches!(
        path,
        "server-settings/server.properties"
            | "server-settings/bukkit.yml"
            | "server-settings/spigot.yml"
            | "server-settings/paper.yml"
            | "server-settings/purpur.yml"
            | "server-settings/pufferfish.yml"
    )
}
/// Existing approved text configs only. Metadata/workflow edits use a separate reviewed process.
pub fn editable_paths(repo: &Path) -> Result<Vec<String>> {
    validate_repository(repo)?;
    let mut approved = BTreeSet::new();
    let path = repo.join("export-policy.json");
    if path.exists() {
        let value: Value = serde_json::from_slice(&fs::read(path)?)?;
        for path in value["approved_config_paths"]
            .as_array()
            .context("Missing config approvals")?
        {
            approved.insert(path.as_str().context("Invalid approval")?.to_owned());
        }
    }
    Ok(tracked_files(repo)?
        .into_iter()
        .filter(|path| {
            config_path(path).is_ok()
                && (known_config(path) || approved.contains(path))
                && repo.join(path).is_file()
        })
        .collect())
}
fn collect(base: &Path, current: &Path, output: &mut BTreeSet<String>) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }
    ensure!(
        !current.is_symlink(),
        "Symlink forbidden in managed configuration"
    );
    for entry in fs::read_dir(current)? {
        let path = entry?.path();
        ensure!(
            !path.is_symlink(),
            "Symlink forbidden in managed configuration"
        );
        if path.is_dir() {
            collect(base, &path, output)?;
        } else {
            ensure!(path.is_file(), "Special file forbidden");
            output.insert(
                path.strip_prefix(base)?
                    .to_str()
                    .context("Non-UTF8 file path")?
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}
pub fn tracked_files(repo: &Path) -> Result<BTreeSet<String>> {
    if !repo.join(".git").exists() {
        return Ok(BTreeSet::new());
    }
    let output = crate::gitrepo::run_bounded(
        Command::new("git").arg("-C").arg(repo).args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]),
        Duration::from_secs(30),
    )?;
    ensure!(output.status.success(), "Cannot audit tracked files");
    // run_bounded caps output at 1MiB. Reject at the boundary rather than silently truncate.
    ensure!(
        output.stdout.len() < 1024 * 1024,
        "Tracked file listing exceeds audit limit"
    );
    let text = String::from_utf8(output.stdout)?;
    Ok(text
        .split('\0')
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect())
}
pub fn validate_repository(repo: &Path) -> Result<Value> {
    ensure!(repo.is_dir(), "Repository directory missing");
    let mut files = tracked_files(repo)?;
    for root in MANAGED_ROOTS {
        collect(repo, &repo.join(root), &mut files)?;
    }
    let policy = repo.join("export-policy.json");
    let approved: BTreeSet<String> = if policy.exists() {
        ensure!(!policy.is_symlink(), "Policy symlink forbidden");
        let value: Value = serde_json::from_slice(&fs::read(&policy)?)?;
        value["approved_config_paths"]
            .as_array()
            .context("Policy requires approved_config_paths")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .context("Approved path must be a string")
            })
            .collect::<Result<_>>()?
    } else {
        BTreeSet::new()
    };
    for path in &approved {
        config_path(path)?;
    }
    let properties =
        fs::read_to_string(repo.join("server-settings/server.properties")).unwrap_or_default();
    let worlds = configured_worlds(&properties);
    let mut configs = 0;
    for path in files {
        relative(&path)?;
        ensure!(
            !runtime_path(&path),
            "Forbidden tracked or managed file: {path}"
        );
        ensure!(
            !worlds
                .iter()
                .any(|world| path == *world || path.starts_with(&format!("{world}/"))),
            "World content is forbidden in Git: {path}"
        );
        let source = repo.join(&path);
        ensure!(!source.is_symlink(), "Tracked symlinks forbidden: {path}");
        // Deleted tracked configs are validated as removals, never read through another path.
        if !source.exists() {
            continue;
        }
        if MANAGED_ROOTS
            .iter()
            .any(|root| path.starts_with(&format!("{root}/")))
        {
            ensure!(
                known_config(&path) || approved.contains(&path),
                "Arbitrary config needs explicit approval in export-policy.json: {path}"
            );
            let content =
                fs::read_to_string(&source).context("Managed config must be UTF-8 text")?;
            validate_config_file(&path, &content)?;
            configs += 1;
        } else {
            ensure!(
                path == ".gitignore"
                    || path == "README.md"
                    || path == "export-policy.json"
                    || [
                        "requirements/engine.json",
                        "requirements/plugins.json",
                        "requirements/mods.json"
                    ]
                    .contains(&path.as_str())
                    || path.starts_with(".github/")
                    || path.starts_with("docs/")
                    || path == "CODEOWNERS",
                "Unmanaged file is forbidden in configuration Git: {path}"
            );
        }
    }
    for category in ["engine", "plugins", "mods"] {
        let path = repo.join("requirements").join(format!("{category}.json"));
        if path.exists() {
            inspect_value(&serde_json::from_slice::<Value>(&fs::read(path)?)?)?;
        }
    }
    crate::artifacts::load_manifest(repo)?;
    Ok(
        json!({"valid":true,"configs":configs,"secret_detection":"conservative heuristic; manual review required"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignore_template_matches_shared_policy() {
        assert_eq!(
            include_str!("../templates/config-repository/.gitignore"),
            IGNORE
        );
    }
    #[test]
    fn secret_values_are_never_in_errors() {
        let err = validate_config_file("plugins-configs/Foo/config.yml", "api-token: do-not-leak")
            .unwrap_err()
            .to_string();
        assert!(!err.contains("do-not-leak"));
        assert!(validate_config_file("config/foo.toml", "password = 'hidden'").is_err());
        assert!(
            validate_config_file(
                "server-settings/server.properties",
                "resource-pack=https://example.invalid/pack?token=do-not-leak"
            )
            .is_err()
        );
    }
    #[test]
    fn custom_world_names_and_markers() {
        assert!(configured_worlds("level-name=MyRealm\n").contains("MyRealm"));
        assert!(runtime_path("plugins-configs/Map/MyRealm/level.dat"));
        assert!(runtime_path("plugins-configs/Test/database/state.json"));
    }
    #[test]
    fn tracked_ignored_files_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(root)
                .output()
                .unwrap()
                .status
                .success()
        );
        fs::write(root.join("leaked.jar"), b"jar").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "--", "leaked.jar"])
            .output()
            .unwrap();
        assert!(validate_repository(root).is_err());
    }
    #[test]
    fn arbitrary_config_requires_approval() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("config")).unwrap();
        fs::write(temp.path().join("config/mod.toml"), "enabled=true").unwrap();
        assert!(validate_repository(temp.path()).is_err());
        fs::write(
            temp.path().join("export-policy.json"),
            r#"{"approved_config_paths":["config/mod.toml"]}"#,
        )
        .unwrap();
        assert!(validate_repository(temp.path()).is_ok());
    }
}
