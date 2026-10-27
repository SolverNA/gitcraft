//! Explicit, reviewable export of text configuration. Game data and JAR bytes never enter Git.
use crate::{artifacts, policy};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("Missing export field {key}"))
}
fn source_path(server: &Path, relative: &str) -> Result<PathBuf> {
    policy::relative(relative)?;
    let mut path = server.to_path_buf();
    ensure!(!path.is_symlink(), "Server root cannot be a symlink");
    for part in Path::new(relative).components() {
        path.push(part);
        ensure!(!path.is_symlink(), "Symlink source is forbidden");
    }
    ensure!(
        path.is_file(),
        "Selected source must be a regular file: {relative}"
    );
    Ok(path)
}
fn mapping(relative: &str) -> Result<String> {
    policy::relative(relative)?;
    let target = if let Some(tail) = relative.strip_prefix("plugins/") {
        format!("plugins-configs/{tail}")
    } else if relative.starts_with("config/") || relative.starts_with("defaultconfigs/") {
        relative.into()
    } else if !relative.contains('/') {
        format!("server-settings/{relative}")
    } else {
        bail!("No supported configuration mapping for {relative}");
    };
    policy::config_path(&target)?;
    Ok(target)
}
fn output_content(relative: &str, source: &Path) -> Result<(String, Vec<String>)> {
    ensure!(
        source.metadata()?.len() <= 2 * 1024 * 1024,
        "Configuration file too large"
    );
    let content = fs::read_to_string(source).context("Config must be UTF-8 text")?;
    Ok(if relative == "server.properties" {
        policy::sanitize_properties(&content)
    } else {
        (content, Vec::new())
    })
}
fn world_roots(server: &Path) -> Result<BTreeSet<String>> {
    let properties = fs::read_to_string(server.join("server.properties")).unwrap_or_default();
    let mut worlds = policy::configured_worlds(&properties);
    fn visit(
        root: &Path,
        current: &Path,
        worlds: &mut BTreeSet<String>,
        depth: usize,
    ) -> Result<()> {
        ensure!(depth <= 32, "Server tree exceeds maximum traversal depth");
        for entry in fs::read_dir(current)? {
            let path = entry?.path();
            if path.is_symlink() {
                continue;
            }
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("Non UTF-8 path")?
                .replace('\\', "/");
            if path.is_dir() {
                if path.join("level.dat").is_file() {
                    worlds.insert(relative);
                    continue;
                }
                if relative == ".git"
                    || policy::runtime_path(&format!("{relative}/placeholder.yml"))
                {
                    continue;
                }
                visit(root, &path, worlds, depth + 1)?;
            }
        }
        Ok(())
    }
    visit(server, server, &mut worlds, 0)?;
    Ok(worlds)
}
fn in_world(relative: &str, worlds: &BTreeSet<String>) -> bool {
    worlds
        .iter()
        .any(|world| relative == *world || relative.starts_with(&format!("{world}/")))
}
fn candidates(
    server: &Path,
    worlds: &BTreeSet<String>,
) -> Result<(Vec<Value>, Vec<String>, Vec<String>)> {
    let mut files = Vec::new();
    let mut jars = Vec::new();
    let mut excluded = Vec::new();
    fn walk(
        root: &Path,
        current: &Path,
        worlds: &BTreeSet<String>,
        files: &mut Vec<Value>,
        jars: &mut Vec<String>,
        excluded: &mut Vec<String>,
        depth: usize,
    ) -> Result<()> {
        ensure!(
            depth <= 32 && files.len() + jars.len() + excluded.len() < 100000,
            "Export candidate limit exceeded"
        );
        for entry in fs::read_dir(current)? {
            let path = entry?.path();
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("Non UTF-8 path")?
                .replace('\\', "/");
            if path.is_symlink() || in_world(&relative, worlds) {
                excluded.push(relative);
                continue;
            }
            if path.is_dir() {
                if policy::runtime_path(&format!("{relative}/placeholder.yml")) {
                    excluded.push(relative);
                    continue;
                }
                if !relative.contains('/')
                    && !["plugins", "config", "defaultconfigs", "mods"].contains(&relative.as_str())
                {
                    excluded.push(relative);
                    continue;
                }
                walk(root, &path, worlds, files, jars, excluded, depth + 1)?;
            } else if path.is_file() {
                let jar = path.extension().and_then(|s| s.to_str()) == Some("jar");
                if jar
                    && (!relative.contains('/')
                        || relative
                            .strip_prefix("plugins/")
                            .is_some_and(|tail| !tail.contains('/'))
                        || relative
                            .strip_prefix("mods/")
                            .is_some_and(|tail| !tail.contains('/')))
                {
                    jars.push(relative);
                    continue;
                }
                if let Ok(destination) = mapping(&relative) {
                    files.push(
                        json!({"source":relative,"destination":destination,"selected":false}),
                    );
                } else {
                    excluded.push(relative);
                }
            }
        }
        Ok(())
    }
    walk(
        server,
        server,
        worlds,
        &mut files,
        &mut jars,
        &mut excluded,
        0,
    )?;
    files.sort_by_key(|v| v["source"].as_str().unwrap_or("").to_owned());
    jars.sort();
    excluded.sort();
    Ok((files, jars, excluded))
}
fn jar_manifest(
    server: &Path,
    jars: &[String],
    metadata: Option<&Path>,
) -> Result<(Value, Vec<String>, Vec<Value>)> {
    let entries: Vec<Value> = if let Some(path) = metadata {
        let value: Value = serde_json::from_slice(&fs::read(path)?)?;
        policy::validate_metadata(&value)?;
        value["artifacts"]
            .as_array()
            .context("Metadata requires artifacts array")?
            .clone()
    } else {
        Vec::new()
    };
    let mut engine = None;
    let mut plugins = Vec::new();
    let mut mods = Vec::new();
    let mut unresolved = Vec::new();
    let mut receipts = Vec::new();
    let mut used = BTreeSet::new();
    for entry in &entries {
        let path = text(entry, "path")?;
        ensure!(
            jars.iter().any(|jar| jar == path),
            "Metadata refers to undiscovered or unsafe JAR: {path}"
        );
        ensure!(used.insert(path.to_owned()), "Duplicate metadata JAR");
        let category = entry["category"].as_str().unwrap_or_else(|| {
            if path.starts_with("plugins/") {
                "plugins"
            } else if path.starts_with("mods/") {
                "mods"
            } else {
                "engine"
            }
        });
        ensure!(
            ["engine", "plugins", "mods"].contains(&category),
            "Invalid artifact category"
        );
        ensure!(
            (category == "engine" && !path.contains('/'))
                || (category == "plugins" && path.starts_with("plugins/"))
                || (category == "mods" && path.starts_with("mods/")),
            "Artifact category must match source layout"
        );
        let version = text(entry, "version")?;
        let name = text(entry, "name")?;
        let filename = entry["filename"]
            .as_str()
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path));
        ensure!(
            filename.ends_with(".jar")
                && filename
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
                && filename.starts_with(|c: char| c.is_ascii_alphanumeric()),
            "Unsafe JAR filename"
        );
        let source = entry["source"]
            .as_object()
            .context("Explicit source provider required; use transfer for unknown local origins")?;
        ensure!(
            source.keys().all(
                |key| ["provider", "project", "minecraft", "loader", "channel"]
                    .contains(&key.as_str())
            ),
            "Unsupported source metadata field"
        );
        let provider = text(&entry["source"], "provider")?;
        ensure!(
            ["transfer", "direct", "modrinth"].contains(&provider),
            "Unsupported source provider"
        );
        let hash = artifacts::sha256(&source_path(server, path)?)?;
        let mut pinned = json!({"version":version,"sha256":hash});
        if provider == "transfer" {
            pinned["transfer"] = json!(true);
        } else {
            let url = text(entry, "url")?;
            artifacts::validate_url(url)?;
            pinned["url"] = json!(url);
        }
        let artifact =
            json!({"name":name,"filename":filename,"pinned":pinned,"source":entry["source"]});
        match category {
            "engine" => {
                ensure!(engine.is_none(), "Select exactly one engine JAR");
                engine = Some(artifact);
            }
            "plugins" => plugins.push(artifact),
            _ => mods.push(artifact),
        }
        receipts.push(
            json!({"source":path,"sha256":hash,"size":fs::metadata(server.join(path))?.len()}),
        );
    }
    for path in jars {
        if !used.contains(path) && (path.starts_with("plugins/") || path.starts_with("mods/")) {
            unresolved.push(path.clone());
        }
    }
    if engine.is_none() {
        unresolved.push("Select one engine JAR with explicit name/version/source metadata".into());
    }
    Ok((
        json!({"engine":engine,"plugins":{"plugins":plugins},"mods":{"mods":mods}}),
        unresolved,
        receipts,
    ))
}

pub fn plan(server: &Path, includes: &[String], metadata: Option<&Path>) -> Result<Value> {
    ensure!(
        server.is_dir() && !server.is_symlink(),
        "Server must be an existing real directory"
    );
    let server = server.canonicalize()?;
    let worlds = world_roots(&server)?;
    let (mut candidates, jars, excluded) = candidates(&server, &worlds)?;
    let mut selected = Vec::new();
    let mut blocked = Vec::new();
    let mut targets = BTreeSet::new();
    for relative in includes {
        let attempt = (|| -> Result<Value> {
            ensure!(
                !in_world(relative, &worlds),
                "World configuration/data is excluded"
            );
            let source = source_path(&server, relative)?;
            let destination = mapping(relative)?;
            ensure!(
                targets.insert(destination.clone()),
                "Duplicate export destination"
            );
            let (content, redactions) = output_content(relative, &source)?;
            policy::validate_config_file(&destination, &content)?;
            Ok(
                json!({"source":relative,"destination":destination,"source_sha256":artifacts::sha256(&source)?,"export_sha256":hex::encode(sha2_digest(content.as_bytes())),"redacted_keys":redactions}),
            )
        })();
        match attempt {
            Ok(value) => selected.push(value),
            Err(error) => blocked.push(json!({"source":relative,"reason":error.to_string()})),
        };
    }
    for candidate in &mut candidates {
        candidate["selected"] = json!(selected.iter().any(|s| s["source"] == candidate["source"]));
    }
    let (manifests, unresolved, artifact_sources) = jar_manifest(&server, &jars, metadata)?;
    Ok(
        json!({"schema":1,"server":server,"selected":selected,"candidates":candidates,"excluded":excluded,"world_roots":worlds,"jar_candidates":jars,"artifact_sources":artifact_sources,"manifests":manifests,"unresolved_artifacts":unresolved,"blocked":blocked,"review_required":true,"secret_detection":"Heuristic only; inspect every explicitly selected configuration before approval. Plan contains no config values."}),
    )
}
fn sha2_digest(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).to_vec()
}

pub fn apply(plan_path: &Path, destination: &Path, reviewed: bool) -> Result<Value> {
    ensure!(
        reviewed,
        "Explicit --reviewed approval is required after inspecting the plan and source configs"
    );
    let value: Value = serde_json::from_slice(&fs::read(plan_path)?)?;
    ensure!(value["schema"] == 1, "Unsupported export plan");
    ensure!(
        value["blocked"].as_array().is_some_and(Vec::is_empty),
        "Export plan contains blocked selections"
    );
    ensure!(
        value["unresolved_artifacts"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "Every plugin/mod and one engine need explicit metadata"
    );
    let server = Path::new(text(&value, "server")?);
    ensure!(
        server.is_absolute() && server.is_dir() && !server.is_symlink(),
        "Plan source unavailable"
    );
    let worlds = world_roots(server)?;
    ensure!(
        !destination.exists(),
        "Destination must not exist; export never overwrites an existing repository"
    );
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure!(parent.is_dir(), "Destination parent must exist");
    let temp = tempfile::tempdir_in(parent)?;
    let mut approved = Vec::new();
    for entry in value["selected"]
        .as_array()
        .context("Plan selected files missing")?
    {
        let relative = text(entry, "source")?;
        ensure!(
            !in_world(relative, &worlds),
            "Source became a world directory"
        );
        let source = source_path(server, relative)?;
        ensure!(
            artifacts::sha256(&source)? == text(entry, "source_sha256")?,
            "Selected source changed since plan; create a new plan"
        );
        let destination_relative = mapping(relative)?;
        ensure!(
            destination_relative == text(entry, "destination")?,
            "Plan mapping was altered"
        );
        let (content, _) = output_content(relative, &source)?;
        ensure!(
            hex::encode(sha2_digest(content.as_bytes())) == text(entry, "export_sha256")?,
            "Export content changed since preview"
        );
        policy::validate_config_file(&destination_relative, &content)?;
        let target = temp.path().join(&destination_relative);
        fs::create_dir_all(target.parent().context("Target parent")?)?;
        fs::write(target, content)?;
        approved.push(destination_relative);
    }
    for entry in value["artifact_sources"]
        .as_array()
        .context("Artifact source receipts missing")?
    {
        let source = source_path(server, text(entry, "source")?)?;
        ensure!(
            artifacts::sha256(&source)? == text(entry, "sha256")?,
            "Local JAR changed since preview"
        );
    }
    let requirements = temp.path().join("requirements");
    fs::create_dir(&requirements)?;
    for category in ["engine", "plugins", "mods"] {
        let manifest = &value["manifests"][category];
        ensure!(!manifest.is_null(), "Manifest missing");
        fs::write(
            requirements.join(format!("{category}.json")),
            serde_json::to_vec_pretty(manifest)?,
        )?;
    }
    fs::write(
        temp.path().join("export-policy.json"),
        serde_json::to_vec_pretty(&json!({"schema":1,"approved_config_paths":approved}))?,
    )?;
    fs::write(temp.path().join(".gitignore"), policy::IGNORE)?;
    fs::write(
        temp.path().join("README.md"),
        "# Minecraft desired state\n\nExplicitly reviewed configuration only. Worlds, JARs, runtime data and secrets stay outside Git. See export-policy.json for approved config paths. Use GitCraft propose for edits; only a merged deployment branch is installed.\n",
    )?;
    policy::validate_repository(temp.path())?;
    let output = crate::gitrepo::run_bounded(
        Command::new("git")
            .arg("init")
            .arg("--initial-branch=main")
            .arg(temp.path()),
        Duration::from_secs(30),
    )?;
    ensure!(output.status.success(), "Git initialization failed");
    // No implicit Git commit/push: identity, remote and PR approval belong to the operator.
    fs::rename(temp.path(), destination)?;
    Ok(
        json!({"status":"exported","repository":destination,"files":approved,"jar_bytes_copied":false,"world_bytes_copied":false,"next":"Review git diff, commit initial config, configure origin and protected deployment branch; add Actions via bootstrap-repo.sh"}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let server = temp.path().join("server");
        fs::create_dir(&server).unwrap();
        fs::write(server.join("server.jar"), b"fake jar").unwrap();
        fs::write(
            server.join("server.properties"),
            "level-name=OurRealm\nrcon.password=never-in-plan\nmotd=Hello\n",
        )
        .unwrap();
        fs::create_dir(server.join("OurRealm")).unwrap();
        fs::write(server.join("OurRealm/level.dat"), b"world").unwrap();
        let metadata = temp.path().join("metadata.json");
        fs::write(&metadata,serde_json::to_vec(&json!({"artifacts":[{"path":"server.jar","name":"Local engine","version":"unversioned-local","source":{"provider":"transfer"}}]})).unwrap()).unwrap();
        (temp, server, metadata)
    }
    #[test]
    fn plan_apply_keeps_worlds_jars_secrets_out() {
        let (temp, server, metadata) = fixture();
        let value = plan(&server, &["server.properties".into()], Some(&metadata)).unwrap();
        assert!(!value.to_string().contains("never-in-plan"));
        let file = temp.path().join("plan.json");
        fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        let destination = temp.path().join("repo");
        assert!(apply(&file, &destination, false).is_err());
        apply(&file, &destination, true).unwrap();
        assert!(!destination.join("server.jar").exists());
        assert!(!destination.join("OurRealm").exists());
        assert!(
            !fs::read_to_string(destination.join("server-settings/server.properties"))
                .unwrap()
                .contains("rcon.password")
        );
        assert!(policy::validate_repository(&destination).is_ok());
        assert!(apply(&file, &destination, true).is_err());
    }
    #[test]
    fn changed_sources_and_world_config_are_blocked() {
        let (temp, server, metadata) = fixture();
        fs::write(server.join("OurRealm/config.yml"), "safe: true").unwrap();
        let blocked = plan(&server, &["OurRealm/config.yml".into()], Some(&metadata)).unwrap();
        assert_eq!(blocked["blocked"].as_array().unwrap().len(), 1);
        let value = plan(&server, &["server.properties".into()], Some(&metadata)).unwrap();
        let file = temp.path().join("plan.json");
        fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::write(server.join("server.properties"), "motd=changed").unwrap();
        assert!(apply(&file, &temp.path().join("repo"), true).is_err());
    }
    #[test]
    fn plugin_runtime_and_nested_secrets_are_denied() {
        let (_temp, server, metadata) = fixture();
        fs::create_dir_all(server.join("plugins/Essentials/userdata")).unwrap();
        fs::write(
            server.join("plugins/Essentials/userdata/player.yml"),
            "data: player",
        )
        .unwrap();
        fs::write(
            server.join("plugins/Essentials/config.yml"),
            "nested:\n  - api-key: dont-print-me\n",
        )
        .unwrap();
        let value = plan(
            &server,
            &[
                "plugins/Essentials/userdata/player.yml".into(),
                "plugins/Essentials/config.yml".into(),
            ],
            Some(&metadata),
        )
        .unwrap();
        assert_eq!(value["blocked"].as_array().unwrap().len(), 2);
        assert!(!value.to_string().contains("dont-print-me"));
    }
}
