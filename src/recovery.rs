//! Durable managed-file rollback journal. Worlds and plugin databases are deliberately outside rollback.
use crate::{artifacts, deploy};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn durable_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("Journal needs parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn journal_path(config: &Value) -> Result<PathBuf> {
    Ok(Path::new(
        config["paths"]["state"]
            .as_str()
            .context("State path required")?,
    )
    .join("transaction.json"))
}
pub fn pending(config: &Value) -> Result<Option<Value>> {
    let path = journal_path(config)?;
    if !path.exists() {
        return Ok(None);
    }
    deploy::safe_path(path.parent().unwrap(), "transaction.json")?;
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}
pub(crate) fn save(config: &Value, journal: &Value) -> Result<()> {
    durable_json(&journal_path(config)?, journal)
}
pub(crate) fn phase(config: &Value, journal: &mut Value, phase: &str) -> Result<()> {
    journal["phase"] = json!(phase);
    if matches!(phase, "starting" | "healthy" | "committed") {
        journal["may_have_started"] = json!(true);
    }
    save(config, journal)
}
pub(crate) fn finish(config: &Value, journal: &Value) -> Result<()> {
    let path = journal_path(config)?;
    durable_json(
        &path.parent().unwrap().join("last-transaction.json"),
        journal,
    )?;
    fs::remove_file(&path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn copy_durable(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination.parent().context("Destination needs parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut File::open(source)?, &mut temporary)?;
    temporary.as_file().sync_all()?;
    temporary.persist(destination)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
/// Caller must hold deploy.lock and establish the service is stopped.
pub(crate) fn restore_stopped(
    config: &Value,
    journal: &Value,
    acknowledge_data_risk: bool,
) -> Result<Value> {
    let server = Path::new(
        config["paths"]["server"]
            .as_str()
            .context("Server path required")?,
    );
    if journal["server"].as_str() != server.to_str() {
        bail!("Journal belongs to a different server");
    }
    if journal["may_have_started"] == true && !acknowledge_data_risk {
        bail!(
            "Interrupted deployment may have started Java and migrated world/plugin data; inspect a consistent data backup, then explicitly recover --acknowledge-data-risk. Service remains stopped."
        );
    }
    let phase = journal["phase"].as_str().context("Missing journal phase")?;
    let mutation_possible = matches!(
        phase,
        "applying" | "starting" | "healthy" | "committed" | "rolling_back"
    );
    if mutation_possible {
        let backup = Path::new(journal["backup"].as_str().context("Missing backup path")?);
        let backup_root = Path::new(
            config["paths"]["backups"]
                .as_str()
                .context("Backup root required")?,
        );
        let relative = backup
            .strip_prefix(backup_root)
            .context("Backup outside configured root")?
            .to_str()
            .context("Invalid backup path")?;
        deploy::safe_path(backup_root, relative)?;
        let owned: Vec<String> = serde_json::from_value(journal["owned"].clone())?;
        let hashes: BTreeMap<String, String> =
            serde_json::from_value(journal["backup_hashes"].clone())?;
        // Validate every backup before changing any target, including corruption from a partial disk write.
        for (relative, expected) in &hashes {
            let source = deploy::safe_path(backup, relative)?;
            if artifacts::sha256(&source)? != *expected {
                bail!("Managed backup hash mismatch; recovery stopped before mutation");
            }
        }
        for relative in &owned {
            let destination = deploy::safe_path(server, relative)?;
            if let Some(_) = hashes.get(relative) {
                copy_durable(&deploy::safe_path(backup, relative)?, &destination)?;
            } else if destination.exists() {
                if !destination.is_file() {
                    bail!("Recovery destination is not a file");
                }
                fs::remove_file(&destination)?;
                File::open(destination.parent().unwrap())?.sync_all()?;
            }
        }
        let state = Path::new(config["paths"]["state"].as_str().unwrap()).join("deployment.json");
        if journal["previous_state"].is_null() {
            if state.exists() {
                fs::remove_file(&state)?;
                File::open(state.parent().unwrap())?.sync_all()?;
            }
        } else {
            durable_json(&state, &journal["previous_state"])?;
        }
    }
    let mut completed = journal.clone();
    completed["phase"] = json!("recovered_stopped");
    finish(config, &completed)?;
    let result = json!({"status":"recovered_stopped","service_restarted":false,"managed_files_only":true,"data_risk_acknowledged":acknowledge_data_risk,"message":"Managed files restored; service remains stopped. World/plugin database changes were not restored. Inspect data and start the existing unit explicitly."});
    durable_json(
        &Path::new(config["paths"]["state"].as_str().unwrap()).join("status.json"),
        &result,
    )?;
    Ok(result)
}
pub fn recover(config: &Value, acknowledge_data_risk: bool) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    let state = Path::new(config["paths"]["state"].as_str().unwrap());
    fs::create_dir_all(state)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(deploy::safe_path(state, "deploy.lock")?)?;
    lock.try_lock_exclusive()?;
    deploy::ensure_stopped(&config)?;
    let journal = pending(&config)?.context("No interrupted deployment journal")?;
    restore_stopped(&config, &journal, acknowledge_data_risk)
}
fn copy_tree(
    source: &Path,
    dest: &Path,
    base: &Path,
    hashes: &mut BTreeMap<String, String>,
) -> Result<()> {
    let meta = source.symlink_metadata()?;
    if meta.file_type().is_symlink() {
        bail!("Data backup symlinks forbidden");
    }
    if meta.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dest.join(entry.file_name()), base, hashes)?;
        }
        File::open(dest)?.sync_all()?;
    } else if meta.is_file() {
        copy_durable(source, dest)?;
        hashes.insert(
            source.strip_prefix(base)?.to_string_lossy().into(),
            artifacts::sha256(dest)?,
        );
    } else {
        bail!("Unsupported data backup file");
    }
    Ok(())
}
/// Offline local-data backup. External databases need their own coordinated snapshot procedure.
pub fn backup_data(config: &Value, includes: &[String]) -> Result<Value> {
    let config = crate::config::validate(config.clone())?;
    if includes.is_empty() {
        bail!("Explicit data paths are required");
    }
    let state = Path::new(config["paths"]["state"].as_str().unwrap());
    fs::create_dir_all(state)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(deploy::safe_path(state, "deploy.lock")?)?;
    lock.try_lock_exclusive()?;
    deploy::ensure_stopped(&config)?;
    backup_data_stopped(&config, includes, || deploy::ensure_stopped(&config))
}
fn backup_data_stopped(
    config: &Value,
    includes: &[String],
    verify_stopped: impl Fn() -> Result<()>,
) -> Result<Value> {
    let server = Path::new(
        config["paths"]["server"]
            .as_str()
            .context("Server path required")?,
    );
    let backup_root = Path::new(
        config["paths"]["backups"]
            .as_str()
            .context("Backup root required")?,
    );
    fs::create_dir_all(backup_root)?;
    let destination = backup_root.join(format!(
        "data-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&destination)?;
    let mut hashes = BTreeMap::new();
    for relative in includes {
        if relative == "." || relative.starts_with('.') || relative.ends_with(".jar") {
            bail!("Select explicit world or plugin-data paths");
        }
        let source = deploy::safe_path(server, relative)?;
        let target = deploy::safe_path(&destination, relative)?;
        copy_tree(&source, &target, server, &mut hashes)?;
    }
    verify_stopped()?;
    let receipt = json!({"status":"offline_data_backup","path":destination,"files":hashes,"includes":includes,"scope":"local files only; external databases require separate coordinated snapshots","service_restarted":false});
    durable_json(&destination.join("data-backup.json"), &receipt)?;
    Ok(receipt)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_backup_keeps_data_outside_server() {
        let root = tempfile::tempdir().unwrap();
        let server = root.path().join("server");
        fs::create_dir_all(server.join("world")).unwrap();
        fs::write(server.join("world/level.dat"), "world").unwrap();
        let config = json!({"paths":{"server":server,"backups":root.path().join("backups")}});
        let receipt = backup_data_stopped(&config, &["world".into()], || Ok(())).unwrap();
        assert_eq!(
            fs::read_to_string(
                Path::new(receipt["path"].as_str().unwrap()).join("world/level.dat")
            )
            .unwrap(),
            "world"
        );
        assert_eq!(
            fs::read_to_string(server.join("world/level.dat")).unwrap(),
            "world"
        );
    }
    #[test]
    fn changed_service_state_rejects_backup_receipt() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("server/world")).unwrap();
        let config = json!({"paths":{"server":root.path().join("server"),"backups":root.path().join("backups")}});
        assert!(
            backup_data_stopped(&config, &["world".into()], || anyhow::bail!(
                "service started"
            ))
            .is_err()
        );
    }
}
