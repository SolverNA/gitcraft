//! Direct, bounded and resumable client -> VPS transport. No archives are extracted.
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const CHUNK_BYTES: usize = 1024 * 1024;
pub const BROWSER_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferFile {
    pub kind: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub files: Vec<TransferFile>,
    pub source_consistency: String,
}
#[derive(Serialize, Deserialize)]
struct Session {
    id: String,
    token_hash: String,
    created: u64,
    manifest: Manifest,
    offsets: Vec<u64>,
    chunks: Vec<Vec<String>>,
    verified: bool,
    status: String,
    #[serde(default)]
    destination: Option<Value>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn random() -> String {
    let mut b = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
fn limit(config: &Value, key: &str, default: u64) -> u64 {
    config["transfer"][key].as_u64().unwrap_or(default)
}
fn atom(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut t = tempfile::NamedTempFile::new_in(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    serde_json::to_writer_pretty(&mut t, value)?;
    t.as_file().sync_all()?;
    t.persist(path)?;
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}
fn root(config: &Value) -> Result<PathBuf> {
    let state = Path::new(
        config["paths"]["state"]
            .as_str()
            .context("state path missing")?,
    );
    let p = crate::deploy::safe_path(state, "transfers")?;
    fs::create_dir_all(&p)?;
    Ok(p)
}
fn session_dir(config: &Value, id: &str) -> Result<PathBuf> {
    ensure!(valid_hash(id), "invalid transfer ID");
    crate::deploy::safe_path(&root(config)?, id)
}
struct LockGuard(File);
impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
fn lock(path: &Path) -> Result<LockGuard> {
    let path = crate::deploy::safe_path(
        path.parent().context("lock parent missing")?,
        path.file_name()
            .and_then(|n| n.to_str())
            .context("invalid lock filename")?,
    )?;
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    f.try_lock_exclusive()
        .context("transfer operation already running")?;
    // Explicit unlock on scope exit: a parallel fork can transiently inherit the
    // same open-file description until exec, so close alone need not release it.
    Ok(LockGuard(f))
}
fn load(config: &Value, id: &str, token: &str) -> Result<(PathBuf, Session)> {
    load_inner(config, id, token, false)
}
fn load_inner(
    config: &Value,
    id: &str,
    token: &str,
    allow_expired: bool,
) -> Result<(PathBuf, Session)> {
    let dir = session_dir(config, id)?;
    let s: Session =
        serde_json::from_slice(&fs::read(crate::deploy::safe_path(&dir, "session.json")?)?)?;
    ensure!(
        s.token_hash == hash(token.as_bytes()),
        "invalid scoped transfer token"
    );
    ensure!(
        allow_expired
            || now().saturating_sub(s.created) <= limit(config, "session_ttl_seconds", 604800),
        "transfer session expired"
    );
    Ok((dir, s))
}
fn data(dir: &Path, index: usize) -> Result<PathBuf> {
    crate::deploy::safe_path(dir, &format!("data/{index}"))
}
fn save(dir: &Path, s: &Session) -> Result<()> {
    atom(&dir.join("session.json"), s)
}
fn destination_binding(config: &Value) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for key in ["server", "cache", "backups", "state"] {
        let path = Path::new(
            config["paths"][key]
                .as_str()
                .context("destination path missing")?,
        );
        ensure!(
            path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir)),
            "destination must be a validated absolute path"
        );
        crate::deploy::safe_path(path, ".binding-check")?;
        let normalized: PathBuf = path.components().collect();
        result.insert(key.into(), json!(normalized));
    }
    result.insert("service".into(), config["deployment"]["service"].clone());
    result.insert(
        "transfers".into(),
        json!(root(config)?.components().collect::<PathBuf>()),
    );
    Ok(Value::Object(result))
}
fn check_destination(config: &Value, session: &Session) -> Result<()> {
    let expected=session.destination.as_ref().context("legacy transfer session has no destination binding; create a new session or clean it up after any required recovery")?;
    ensure!(
        *expected == destination_binding(config)?,
        "transfer destination or service changed; restore original settings for recovery or create a new session"
    );
    Ok(())
}
pub fn has_active_sessions(config: &Value) -> Result<bool> {
    let base = root(config)?;
    for dir in session_entries(&base)? {
        let s = match raw_session(&dir) {
            Ok(s) => s,
            Err(_) => return Ok(true),
        };
        if ["receiving", "verified", "recovery_required"].contains(&s.status.as_str())
            || (dir.join("journal.json").exists() && s.status != "committed")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub fn has_pending(config: &Value) -> Result<bool> {
    for entry in fs::read_dir(root(config)?)? {
        let dir = entry?.path();
        if dir.is_symlink() {
            bail!("transfer session symlink forbidden");
        }
        if dir.is_dir() && dir.join("journal.json").exists() {
            let s: Session = serde_json::from_slice(&fs::read(dir.join("session.json"))?)?;
            if s.status != "committed" {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn sync_tree(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        ensure!(!path.is_symlink(), "world tree symlink forbidden");
        if path.is_dir() {
            sync_tree(&path)?;
        } else {
            File::open(path)?.sync_all()?;
        }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}
fn plain_component(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}
fn validate_file(f: &TransferFile) -> Result<()> {
    ensure!(valid_hash(&f.sha256), "full file SHA-256 is required");
    ensure!(
        f.path.len() <= 1024 && !f.path.contains('\\') && !f.path.contains('\0'),
        "invalid transfer path"
    );
    let parts: Vec<_> = Path::new(&f.path).components().collect();
    ensure!(
        !parts.is_empty() && parts.iter().all(|p| matches!(p, Component::Normal(_))),
        "unsafe transfer path"
    );
    match f.kind.as_str() {
        "engine" | "plugin" | "mod" => {
            ensure!(
                parts.len() == 1 && plain_component(&f.path) && f.path.ends_with(".jar"),
                "artifact must be a JAR basename"
            );
        }
        "world" => {
            let first = parts[0]
                .as_os_str()
                .to_str()
                .context("non UTF-8 world root")?;
            ensure!(
                parts.len() > 1 && plain_component(first),
                "world path must include a plain world directory"
            );
            ensure!(
                ![
                    "plugins",
                    "mods",
                    "logs",
                    "cache",
                    "libraries",
                    "state",
                    "config",
                    "defaultconfigs",
                    "backups",
                    "repository"
                ]
                .contains(&first),
                "reserved world destination"
            );
        }
        _ => bail!("unsupported transfer kind"),
    }
    Ok(())
}
fn validate_manifest(config: &Value, manifest: &Manifest) -> Result<u64> {
    ensure!(
        !manifest.files.is_empty()
            && manifest.files.len() as u64 <= limit(config, "max_files", 100000),
        "transfer file count outside limit"
    );
    ensure!(
        ["stopped", "snapshot"].contains(&manifest.source_consistency.as_str()),
        "source must be stopped or a consistent snapshot"
    );
    let mut paths = BTreeSet::new();
    let mut bytes = 0u64;
    for f in &manifest.files {
        validate_file(f)?;
        ensure!(
            paths.insert(format!("{}/{}", f.kind, f.path)),
            "duplicate transfer path"
        );
        bytes = bytes
            .checked_add(f.size)
            .context("transfer size overflow")?;
    }
    ensure!(
        bytes <= limit(config, "max_session_bytes", 100 * 1024 * 1024 * 1024),
        "transfer exceeds session byte quota"
    );
    Ok(bytes)
}
fn capacity(config: &Value, root: &Path, additional: u64) -> Result<()> {
    let mut reserved = 0u64;
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if !path.is_dir() || path.is_symlink() {
            continue;
        }
        let staged = staged_bytes(&path.join("data"))?;
        let declared = match fs::read(path.join("session.json")) {
            Ok(bytes) => {
                let s: Session = serde_json::from_slice(&bytes)?;
                if s.status == "committed" {
                    0
                } else {
                    s.manifest
                        .files
                        .iter()
                        .try_fold(0u64, |n, f| n.checked_add(f.size).context("quota overflow"))?
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        // Completed sessions retain their verified data. Count those actual bytes,
        // as well as orphan/partial chunks, instead of freeing a fictional quota.
        reserved = reserved
            .checked_add(staged.max(declared))
            .context("quota overflow")?;
    }
    ensure!(
        reserved.checked_add(additional).context("quota overflow")?
            <= limit(config, "max_staging_bytes", 200 * 1024 * 1024 * 1024),
        "staging quota exhausted; remove expired sessions explicitly"
    );
    ensure!(
        fs2::available_space(root)?
            >= additional.saturating_add(limit(config, "min_free_bytes", 1024 * 1024 * 1024)),
        "insufficient free staging space"
    );
    Ok(())
}
fn staged_bytes(path: &Path) -> Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        !metadata.file_type().is_symlink(),
        "staging symlink forbidden"
    );
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    ensure!(metadata.is_dir(), "special staging file forbidden");
    fs::read_dir(path)?.try_fold(0u64, |n, entry| {
        n.checked_add(staged_bytes(&entry?.path())?)
            .context("quota overflow")
    })
}
pub fn create(config: &Value, manifest: Manifest) -> Result<Value> {
    let bytes = validate_manifest(config, &manifest)?;
    let base = root(config)?;
    let destination = Some(destination_binding(config)?);
    let _guard = lock(&base.join("create.lock"))?;
    capacity(config, &base, bytes)?;
    let id = random();
    let token = random();
    let dir = session_dir(config, &id)?;
    fs::create_dir(&dir)?;
    fs::create_dir(dir.join("data"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(dir.join("data"), fs::Permissions::from_mode(0o700))?;
    }
    let count = manifest.files.len();
    let s = Session {
        id: id.clone(),
        token_hash: hash(token.as_bytes()),
        created: now(),
        manifest,
        offsets: vec![0; count],
        chunks: vec![vec![]; count],
        verified: false,
        status: "receiving".into(),
        destination,
    };
    for index in 0..count {
        File::create(data(&dir, index)?)?.sync_all()?;
    }
    save(&dir, &s)?;
    Ok(json!({"id":id,"token":token,"chunk_bytes":CHUNK_BYTES,"status":"receiving"}))
}
pub fn status(config: &Value, id: &str, token: &str) -> Result<Value> {
    let dir = session_dir(config, id)?;
    let _guard = lock(&dir.join("session.lock"))?;
    let (_, s) = load(config, id, token)?;
    Ok(
        json!({"id":s.id,"manifest":s.manifest,"offsets":s.offsets,"verified":s.verified,"status":s.status,"chunk_bytes":CHUNK_BYTES}),
    )
}
pub fn chunk(
    config: &Value,
    id: &str,
    token: &str,
    index: usize,
    offset: u64,
    digest: &str,
    bytes: &[u8],
) -> Result<Value> {
    let dir = session_dir(config, id)?;
    let _guard = lock(&dir.join("session.lock"))?;
    chunk_locked(config, id, token, index, offset, digest, bytes)
}
// Wire fields stay explicit so the HTTP layer cannot acquire a lock after reading.
#[allow(clippy::too_many_arguments)]
pub fn receive_chunk(
    config: &Value,
    id: &str,
    token: &str,
    index: usize,
    offset: u64,
    digest: &str,
    reader: &mut dyn Read,
    length: usize,
) -> Result<Value> {
    ensure!(
        length > 0 && length <= CHUNK_BYTES,
        "chunk outside byte limit"
    );
    let dir = session_dir(config, id)?;
    let _guard = lock(&dir.join("session.lock"))?;
    let (_, s) = load(config, id, token)?;
    ensure!(s.status == "receiving", "session no longer accepts chunks");
    ensure!(index < s.manifest.files.len(), "unknown file index");
    let mut bytes = Vec::with_capacity(length);
    reader.take(length as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() == length, "incomplete or oversized chunk body");
    chunk_locked(config, id, token, index, offset, digest, &bytes)
}
fn chunk_locked(
    config: &Value,
    id: &str,
    token: &str,
    index: usize,
    offset: u64,
    digest: &str,
    bytes: &[u8],
) -> Result<Value> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= CHUNK_BYTES,
        "chunk outside byte limit"
    );
    ensure!(
        valid_hash(digest) && hash(bytes).eq_ignore_ascii_case(digest),
        "chunk SHA-256 mismatch"
    );
    let dir = session_dir(config, id)?;
    let (_, mut s) = load(config, id, token)?;
    ensure!(s.status == "receiving", "session no longer accepts chunks");
    let f = s.manifest.files.get(index).context("unknown file index")?;
    let current = s.offsets[index];
    ensure!(
        offset.is_multiple_of(CHUNK_BYTES as u64),
        "chunk offset must be aligned"
    );
    let end = offset
        .checked_add(bytes.len() as u64)
        .context("offset overflow")?;
    ensure!(end <= f.size, "chunk exceeds declared file size");
    ensure!(
        bytes.len() == CHUNK_BYTES || end == f.size,
        "short non-final chunk"
    );
    if offset < current {
        ensure!(
            end <= current
                && s.chunks[index]
                    .get((offset / CHUNK_BYTES as u64) as usize)
                    .is_some_and(|d| d.eq_ignore_ascii_case(digest)),
            "conflicting replay"
        );
        return Ok(json!({"offset":current,"replayed":true}));
    }
    ensure!(
        offset == current,
        "out-of-order chunk; query status and resume at committed offset"
    );
    ensure!(
        fs2::available_space(&dir)?
            >= bytes.len() as u64 + limit(config, "min_free_bytes", 1024 * 1024 * 1024),
        "insufficient free disk space"
    );
    let mut output = OpenOptions::new().write(true).open(data(&dir, index)?)?;
    ensure!(
        output.metadata()?.len() >= current,
        "staged file truncated; session is corrupt"
    );
    output.set_len(current)?;
    output.seek(SeekFrom::Start(current))?;
    output.write_all(bytes)?;
    output.sync_all()?;
    s.offsets[index] = end;
    s.chunks[index].push(digest.to_ascii_lowercase());
    s.verified = false;
    save(&dir, &s)?;
    Ok(json!({"offset":end,"replayed":false}))
}
pub fn verify(config: &Value, id: &str, token: &str) -> Result<Value> {
    let dir = session_dir(config, id)?;
    let _guard = lock(&dir.join("session.lock"))?;
    let (_, mut s) = load(config, id, token)?;
    ensure!(
        s.status == "receiving" || s.status == "verified",
        "invalid verification phase"
    );
    for (i, f) in s.manifest.files.iter().enumerate() {
        ensure!(s.offsets[i] == f.size, "file upload incomplete");
        let p = data(&dir, i)?;
        ensure!(
            fs::metadata(&p)?.len() == f.size
                && crate::artifacts::sha256(&p)?.eq_ignore_ascii_case(&f.sha256),
            "whole-file SHA-256 mismatch"
        );
    }
    s.verified = true;
    s.status = "verified".into();
    save(&dir, &s)?;
    Ok(json!({"id":id,"verified":true,"status":"verified"}))
}

pub fn commit(config: &Value, id: &str, token: &str, confirm_replace: bool) -> Result<Value> {
    commit_with(config, id, token, confirm_replace, || {
        crate::deploy::ensure_stopped(config)
    })
}
pub(crate) fn commit_with(
    config: &Value,
    id: &str,
    token: &str,
    confirm_replace: bool,
    stopped: impl Fn() -> Result<()>,
) -> Result<Value> {
    let dir = session_dir(config, id)?;
    let _session = lock(&dir.join("session.lock"))?;
    let (_, mut s) = load(config, id, token)?;
    check_destination(config, &s)?;
    if s.status == "committed" {
        return Ok(
            json!({"id":id,"status":"committed","receipt":serde_json::from_slice::<Value>(&fs::read(dir.join("receipt.json"))?)?}),
        );
    }
    ensure!(
        s.verified && s.status == "verified",
        "explicit whole-file verification required before commit"
    );
    let state = Path::new(config["paths"]["state"].as_str().context("state missing")?);
    let _deploy = lock(&crate::deploy::safe_path(state, "deploy.lock")?)?;
    ensure!(
        crate::recovery::pending(config)?.is_none(),
        "deployment recovery is pending; recover it before world transfer commit"
    );
    stopped()?;
    let server = Path::new(
        config["paths"]["server"]
            .as_str()
            .context("server missing")?,
    );
    let backups = Path::new(
        config["paths"]["backups"]
            .as_str()
            .context("backups missing")?,
    );
    let cache = Path::new(config["paths"]["cache"].as_str().context("cache missing")?);
    let mut worlds = BTreeSet::new();
    for f in &s.manifest.files {
        validate_file(f)?;
        if f.kind == "world" {
            worlds.insert(f.path.split('/').next().unwrap().to_owned());
        }
    }
    ensure!(
        worlds.is_empty() || confirm_replace,
        "world replacement requires explicit confirmation"
    );
    // Never continue a partially applied world transaction automatically.
    ensure!(
        !dir.join("journal.json").exists(),
        "world transaction journal exists; recover it explicitly before retry"
    );
    for (i, f) in s.manifest.files.iter().enumerate() {
        let p = data(&dir, i)?;
        ensure!(
            fs::metadata(&p)?.len() == f.size
                && crate::artifacts::sha256(&p)?.eq_ignore_ascii_case(&f.sha256),
            "staged file integrity changed"
        );
    }
    let stage = crate::deploy::safe_path(server, &format!("gitcraft-transfer-{id}"))?;
    ensure!(!stage.exists(), "world staging directory already exists");
    let world_bytes = s
        .manifest
        .files
        .iter()
        .filter(|f| f.kind == "world")
        .try_fold(0u64, |n, f| {
            n.checked_add(f.size).context("world size overflow")
        })?;
    ensure!(
        fs2::available_space(server)?
            >= world_bytes.saturating_add(limit(config, "min_free_bytes", 1024 * 1024 * 1024)),
        "insufficient world staging disk space"
    );
    fs::create_dir(&stage)?;
    let backup = crate::deploy::safe_path(backups, &format!("world-transfer-{id}"))?;
    fs::create_dir_all(&backup)?;
    let mut receipts = Vec::new();
    let preparation = (|| -> Result<()> {
        for (i, f) in s.manifest.files.iter().enumerate() {
            if f.kind == "world" {
                let dest = crate::deploy::safe_path(&stage, &f.path)?;
                fs::create_dir_all(dest.parent().unwrap())?;
                fs::copy(data(&dir, i)?, &dest)?;
                File::open(dest)?.sync_all()?;
            } else {
                receipts.push(crate::artifacts::import_local(
                    &data(&dir, i)?,
                    cache,
                    &f.sha256,
                )?);
            }
        }
        sync_tree(&stage)?;
        Ok(())
    })();
    if let Err(error) = preparation {
        let _ = fs::remove_dir_all(&stage);
        return Err(error.context("transfer preparation failed before world mutation"));
    }
    for world in &worlds {
        let dest = crate::deploy::safe_path(server, world)?;
        ensure!(
            !dest.exists() || dest.is_dir(),
            "world destination is not a directory"
        );
    }
    let existing: Vec<_> = worlds
        .iter()
        .filter(|w| server.join(w).exists())
        .cloned()
        .collect();
    File::open(server)?.sync_all()?;
    File::open(backups)?.sync_all()?;
    let mut journal = json!({"phase":"prepared","worlds":worlds,"existing":existing,"backup":backup,"stage":stage,"artifacts":receipts,"destination":s.destination});
    atom(&dir.join("journal.json"), &journal)?;
    stopped()?;
    let applied = (|| -> Result<()> {
        for world in &worlds {
            let dest = crate::deploy::safe_path(server, world)?;
            if dest.exists() {
                fs::rename(&dest, crate::deploy::safe_path(&backup, world)?)?;
                File::open(server)?.sync_all()?;
                File::open(&backup)?.sync_all()?;
            }
            fs::rename(crate::deploy::safe_path(&stage, world)?, &dest)?;
            File::open(&stage)?.sync_all()?;
            File::open(server)?.sync_all()?;
        }
        stopped()?;
        Ok(())
    })();
    if let Err(error) = applied {
        s.status = "recovery_required".into();
        save(&dir, &s)?;
        return Err(error.context(
            "world commit interrupted; run transfer recover while Minecraft remains stopped",
        ));
    }
    journal["phase"] = json!("committed");
    atom(&dir.join("journal.json"), &journal)?;
    let receipt = json!({"worlds":worlds,"backup":backup,"artifacts":receipts,"files":s.manifest.files,"committed":now()});
    atom(&dir.join("receipt.json"), &receipt)?;
    s.status = "committed".into();
    save(&dir, &s)?;
    let _ = fs::remove_dir(&stage);
    Ok(json!({"id":id,"status":"committed","receipt":receipt}))
}

pub fn recover(config: &Value, id: &str, token: &str) -> Result<Value> {
    recover_with(config, id, token, || crate::deploy::ensure_stopped(config))
}
fn recover_with(
    config: &Value,
    id: &str,
    token: &str,
    stopped: impl Fn() -> Result<()>,
) -> Result<Value> {
    let dir = session_dir(config, id)?;
    let _session = lock(&dir.join("session.lock"))?;
    // Recovery credentials remain valid for an interrupted journal even after upload TTL.
    let (_, mut s) = load_inner(config, id, token, true)?;
    if s.destination.is_some() {
        check_destination(config, &s)?;
    }
    let state = Path::new(config["paths"]["state"].as_str().context("state missing")?);
    let _deploy = lock(&state.join("deploy.lock"))?;
    let journal: Value = serde_json::from_slice(&fs::read(dir.join("journal.json"))?)?;
    // Legacy journals have no session binding. Their exact recorded paths are
    // the only safe migration proof; never guess a target from new settings.
    let expected_stage = crate::deploy::safe_path(
        Path::new(
            config["paths"]["server"]
                .as_str()
                .context("server missing")?,
        ),
        &format!("gitcraft-transfer-{id}"),
    )?;
    let expected_backup = crate::deploy::safe_path(
        Path::new(
            config["paths"]["backups"]
                .as_str()
                .context("backups missing")?,
        ),
        &format!("world-transfer-{id}"),
    )?;
    ensure!(
        journal["stage"]
            .as_str()
            .is_some_and(|p| Path::new(p) == expected_stage)
            && journal["backup"]
                .as_str()
                .is_some_and(|p| Path::new(p) == expected_backup),
        "recovery journal paths do not match the original destination; restore original settings or inspect the recorded backups manually; no files were restored"
    );
    if let Some(binding) = journal.get("destination").filter(|v| !v.is_null()) {
        ensure!(
            *binding == destination_binding(config)?,
            "world journal destination differs from current settings; restore original settings before recovery"
        );
    }
    stopped()?;
    if journal["phase"] == "committed" {
        let receipt = json!({"worlds":journal["worlds"],"backup":journal["backup"],"artifacts":journal["artifacts"],"files":s.manifest.files,"committed":now()});
        atom(&dir.join("receipt.json"), &receipt)?;
        s.status = "committed".into();
        save(&dir, &s)?;
        return Ok(json!({"id":id,"status":"committed","minecraft_restarted":false}));
    }
    ensure!(
        journal["phase"] == "prepared",
        "unknown transfer recovery phase"
    );
    let server = Path::new(
        config["paths"]["server"]
            .as_str()
            .context("server missing")?,
    );
    let backup = Path::new(
        config["paths"]["backups"]
            .as_str()
            .context("backup missing")?,
    )
    .join(format!("world-transfer-{id}"));
    let existing: BTreeSet<String> = serde_json::from_value(journal["existing"].clone())?;
    let worlds: BTreeSet<String> = serde_json::from_value(journal["worlds"].clone())?;
    // Preserve backups while recovery is incomplete; repeated recovery is safe.
    for world in worlds {
        ensure!(plain_component(&world), "invalid recovery world");
        let dest = crate::deploy::safe_path(server, &world)?;
        let old = crate::deploy::safe_path(&backup, &world)?;
        if old.exists() {
            if dest.exists() {
                fs::remove_dir_all(&dest)?;
            }
            copy_tree(&old, &dest)?;
            File::open(server)?.sync_all()?;
        } else if !existing.contains(&world) && dest.exists() {
            fs::remove_dir_all(dest)?;
        }
    }
    let stage = crate::deploy::safe_path(server, &format!("gitcraft-transfer-{id}"))?;
    if stage.exists() {
        fs::remove_dir_all(stage)?;
    }
    if backup.exists() {
        let archive = backup.with_file_name(format!("recovered-world-transfer-{id}-{}", random()));
        fs::rename(&backup, &archive)?;
        File::open(archive.parent().context("archive parent")?)?.sync_all()?;
    }
    s.status = "verified".into();
    save(&dir, &s)?;
    fs::remove_file(dir.join("journal.json"))?;
    File::open(&dir)?.sync_all()?;
    Ok(json!({"id":id,"status":"restored","minecraft_restarted":false}))
}
fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    ensure!(!source.is_symlink(), "backup symlink forbidden");
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let path = entry?.path();
        ensure!(!path.is_symlink(), "backup symlink forbidden");
        let to = dest.join(path.file_name().context("backup entry missing name")?);
        if path.is_dir() {
            copy_tree(&path, &to)?;
        } else {
            fs::copy(&path, &to)?;
            File::open(&to)?.sync_all()?;
        }
    }
    File::open(dest)?.sync_all()?;
    Ok(())
}

/// Build a transport plan from a stopped source or a consistent, user-created snapshot.
pub fn plan(source: &Path, kind: &str, consistency: &str) -> Result<Value> {
    ensure!(
        ["stopped", "snapshot"].contains(&consistency),
        "source must be stopped or snapshot"
    );
    ensure!(!source.is_symlink(), "source symlink forbidden");
    let mut files = Vec::new();
    fn visit(root: &Path, p: &Path, kind: &str, files: &mut Vec<TransferFile>) -> Result<()> {
        ensure!(!p.is_symlink(), "source symlinks forbidden");
        if p.is_dir() {
            for e in fs::read_dir(p)? {
                visit(root, &e?.path(), kind, files)?;
            }
        } else {
            ensure!(p.is_file(), "source must contain regular files");
            let relative = p
                .strip_prefix(root)?
                .to_str()
                .context("non UTF-8 filename")?
                .replace('\\', "/");
            let f = TransferFile {
                kind: kind.into(),
                path: relative,
                size: fs::metadata(p)?.len(),
                sha256: crate::artifacts::sha256(p)?,
            };
            validate_file(&f)?;
            files.push(f);
        }
        Ok(())
    }
    let base = source.parent().context("source needs parent")?;
    visit(base, source, kind, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(
        json!({"source":base.canonicalize()?,"manifest":Manifest{files,source_consistency:consistency.into()}}),
    )
}

fn endpoint(base: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(base)?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "credentials do not belong in endpoint URLs"
    );
    let loopback = url.host_str().is_some_and(|h| {
        h == "localhost"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "plain HTTP allowed only through a loopback SSH tunnel; use HTTPS remotely"
    );
    Ok(url)
}
pub fn request(
    base: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let base = endpoint(base)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut r = client
        .request(method.parse()?, base.join(path)?)
        .bearer_auth(token);
    if let Some(body) = body {
        r = r.json(body);
    }
    let response = r.send()?;
    let code = response.status();
    let value: Value = response.json()?;
    ensure!(
        code.is_success(),
        "transfer rejected: {}",
        value["error"].as_str().unwrap_or("request failed")
    );
    Ok(value)
}
pub fn send(base: &str, admin_token: &str, plan_path: &Path, resume_path: &Path) -> Result<Value> {
    let value: Value = serde_json::from_slice(&fs::read(plan_path)?)?;
    let manifest: Manifest = serde_json::from_value(value["manifest"].clone())?;
    let source = Path::new(value["source"].as_str().context("source path missing")?);
    let session: Value = if resume_path.exists() {
        serde_json::from_slice(&fs::read(resume_path)?)?
    } else {
        let s = request(
            base,
            "POST",
            "/api/transfers",
            admin_token,
            Some(&serde_json::to_value(&manifest)?),
        )?;
        atom(resume_path, &s)?;
        s
    };
    let id = session["id"].as_str().context("session id missing")?;
    let token = session["token"].as_str().context("session token missing")?;
    let current = request(base, "GET", &format!("/api/transfers/{id}"), token, None)?;
    ensure!(
        current["manifest"] == serde_json::to_value(&manifest)?,
        "resume session manifest differs from transfer plan"
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let url = endpoint(base)?;
    for (i, f) in manifest.files.iter().enumerate() {
        let p = crate::deploy::safe_path(source, &f.path)?;
        ensure!(
            fs::metadata(&p)?.len() == f.size
                && crate::artifacts::sha256(&p)?.eq_ignore_ascii_case(&f.sha256),
            "source changed since planning"
        );
        let mut input = File::open(p)?;
        let mut offset = current["offsets"][i]
            .as_u64()
            .context("missing remote offset")?;
        input.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0; CHUNK_BYTES];
        while offset < f.size {
            let want = (f.size - offset).min(CHUNK_BYTES as u64) as usize;
            input.read_exact(&mut buffer[..want])?;
            let response = client
                .put(url.join(&format!("/api/transfers/{id}/files/{i}/{offset}"))?)
                .bearer_auth(token)
                .header("X-Chunk-SHA256", hash(&buffer[..want]))
                .body(buffer[..want].to_vec())
                .send()?;
            ensure!(
                response.status().is_success(),
                "chunk upload failed; rerun send with the same resume file"
            );
            offset += want as u64;
            eprintln!("{}: {}/{} bytes", f.path, offset, f.size);
        }
    }
    request(
        base,
        "POST",
        &format!("/api/transfers/{id}/verify"),
        token,
        Some(&json!({})),
    )
}

// Lifecycle lock order: create.lock -> session locks sorted by ID. Chunk/commit
// only hold their own session lock; cleanup never waits for a deployment lock.
fn raw_session(dir: &Path) -> Result<Session> {
    Ok(serde_json::from_slice(&fs::read(
        crate::deploy::safe_path(dir, "session.json")?,
    )?)?)
}
fn session_summary(config: &Value, dir: &Path, s: &Session, busy: bool) -> Result<Value> {
    let expires = s
        .created
        .saturating_add(limit(config, "session_ttl_seconds", 604800));
    let expired = now() > expires;
    let staged = staged_bytes(&dir.join("data"))?;
    let disk = staged_bytes(dir)?;
    let declared = s
        .manifest
        .files
        .iter()
        .try_fold(0u64, |n, f| n.checked_add(f.size).context("quota overflow"))?;
    let journal = dir.join("journal.json");
    let pending = if journal.exists() {
        let value: Value =
            serde_json::from_slice(&fs::read(crate::deploy::safe_path(dir, "journal.json")?)?)?;
        s.status != "committed" || value["phase"] != "committed"
    } else {
        false
    };
    let reason = if busy {
        Some("active operation holds session lock")
    } else if pending || s.status == "recovery_required" {
        Some("pending world commit or recovery")
    } else if s.status == "committed" && !dir.join("receipt.json").is_file() {
        Some("committed receipt missing; inspect before cleanup")
    } else if !["committed", "cancelled", "receiving", "verified"].contains(&s.status.as_str()) {
        Some("unknown session state")
    } else if !["committed", "cancelled"].contains(&s.status.as_str()) && !expired {
        Some("active unexpired session; cancel explicitly first")
    } else {
        None
    };
    Ok(
        json!({"id":s.id,"status":s.status,"created":s.created,"expires":expires,"expired":expired,"staged_bytes":staged,"disk_bytes":disk,"reserved_bytes":if s.status=="committed"{staged}else{declared.max(staged)},"eligible_cleanup":reason.is_none(),"protected_reason":reason}),
    )
}
fn session_fingerprint(config: &Value, dir: &Path, s: &Session) -> Result<String> {
    let summary = session_summary(config, dir, s, false)?;
    Ok(hash(&serde_json::to_vec(
        &json!({"session_hash":hash(&fs::read(dir.join("session.json"))?),"created":summary["created"],"expires":summary["expires"],"status":summary["status"],"disk_bytes":summary["disk_bytes"],"staged_bytes":summary["staged_bytes"],"receipt":if dir.join("receipt.json").exists(){Some(hash(&fs::read(crate::deploy::safe_path(dir,"receipt.json")?)?))}else{None}}),
    )?))
}
fn session_entries(base: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(base)? {
        let path = entry?.path();
        ensure!(!path.is_symlink(), "transfer root symlink forbidden");
        if path.is_dir()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(valid_hash)
        {
            entries.push(path);
        }
    }
    entries.sort();
    Ok(entries)
}
fn unavailable_summary(dir: &Path) -> Result<Value> {
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("invalid session directory")?;
    let staged = staged_bytes(&dir.join("data"))?;
    Ok(
        json!({"id":id,"status":"unavailable","created":null,"expires":null,"expired":null,"staged_bytes":staged,"reserved_bytes":staged,"disk_bytes":staged_bytes(dir)?,"eligible_cleanup":false,"protected_reason":"session metadata missing or malformed; inspect manually, never auto-delete"}),
    )
}
pub fn list_sessions(config: &Value) -> Result<Value> {
    let base = root(config)?;
    let _global = lock(&base.join("create.lock"))?;
    let mut sessions = Vec::new();
    let (mut staged, mut reserved) = (0u64, 0u64);
    for dir in session_entries(&base)? {
        let guard = lock(&crate::deploy::safe_path(&dir, "session.lock")?);
        let value = match raw_session(&dir) {
            Ok(s) => session_summary(config, &dir, &s, guard.is_err())?,
            Err(_) => unavailable_summary(&dir)?,
        };
        staged = staged
            .checked_add(
                value["staged_bytes"]
                    .as_u64()
                    .context("staged size missing")?,
            )
            .context("size overflow")?;
        reserved = reserved
            .checked_add(
                value["reserved_bytes"]
                    .as_u64()
                    .context("reserved size missing")?,
            )
            .context("size overflow")?;
        sessions.push(value);
    }
    for entry in fs::read_dir(&base)? {
        let dir = entry?.path();
        let Some(id) = dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("deleting-"))
            .filter(|id| valid_hash(id))
        else {
            continue;
        };
        ensure!(!dir.is_symlink(), "cleanup tombstone symlink forbidden");
        if !dir.is_dir() {
            continue;
        }
        let bytes = staged_bytes(&dir.join("data"))?;
        let reserve = match raw_session(&dir) {
            Ok(s) if s.status != "committed" => s
                .manifest
                .files
                .iter()
                .try_fold(0u64, |n, f| n.checked_add(f.size).context("size overflow"))?
                .max(bytes),
            _ => bytes,
        };
        staged = staged.checked_add(bytes).context("size overflow")?;
        reserved = reserved.checked_add(reserve).context("size overflow")?;
        sessions.push(json!({"id":id,"status":"cleanup_in_progress","staged_bytes":bytes,"reserved_bytes":reserve,"disk_bytes":staged_bytes(&dir)?,"eligible_cleanup":false,"protected_reason":"resume the reviewed cleanup plan"}));
    }
    Ok(
        json!({"sessions":sessions,"staged_bytes":staged,"reserved_bytes":reserved,"max_staging_bytes":limit(config,"max_staging_bytes",200*1024*1024*1024),"cleanup_pending":base.join("cleanup-journal.json").exists()}),
    )
}
pub fn cancel(config: &Value, id: &str, token: &str) -> Result<Value> {
    cancel_session(config, id, Some(token))
}
pub fn cancel_admin(config: &Value, id: &str) -> Result<Value> {
    cancel_session(config, id, None)
}
fn cancel_session(config: &Value, id: &str, token: Option<&str>) -> Result<Value> {
    let dir = session_dir(config, id)?;
    let _guard = lock(&crate::deploy::safe_path(&dir, "session.lock")?)?;
    let mut s = if let Some(token) = token {
        load_inner(config, id, token, true)?.1
    } else {
        raw_session(&dir)?
    };
    ensure!(
        !dir.join("journal.json").exists() && s.status != "recovery_required",
        "pending world commit/recovery cannot be cancelled"
    );
    if s.status == "cancelled" {
        return Ok(json!({"id":id,"status":"cancelled"}));
    }
    ensure!(
        ["receiving", "verified"].contains(&s.status.as_str()),
        "only receiving or verified sessions can be cancelled"
    );
    s.status = "cancelled".into();
    s.verified = false;
    save(&dir, &s)?;
    Ok(json!({"id":id,"status":"cancelled","data_deleted":false}))
}
fn lifecycle_plan_file(base: &Path) -> PathBuf {
    base.join("cleanup-plan.json")
}
fn safe_lifecycle_files(base: &Path) -> Result<()> {
    for name in [
        "cleanup-plan.json",
        "cleanup-journal.json",
        "cleanup-last.json",
    ] {
        crate::deploy::safe_path(base, name)?;
    }
    Ok(())
}
pub fn cleanup_plan(config: &Value, ids: &[String]) -> Result<Value> {
    let base = root(config)?;
    safe_lifecycle_files(&base)?;
    let _global = lock(&base.join("create.lock"))?;
    let journal = base.join("cleanup-journal.json");
    if journal.exists() {
        let value: Value = serde_json::from_slice(&fs::read(journal)?)?;
        return Ok(json!({"digest":value["digest"],"plan":value["plan"],"resume":true}));
    }
    let selected: BTreeSet<String> = ids
        .iter()
        .map(|id| {
            ensure!(valid_hash(id), "invalid transfer ID");
            Ok(id.clone())
        })
        .collect::<Result<_>>()?;
    let (mut candidates, mut protected, mut seen) = (Vec::new(), Vec::new(), BTreeSet::new());
    let mut bytes = 0u64;
    for dir in session_entries(&base)? {
        let id = dir
            .file_name()
            .and_then(|n| n.to_str())
            .context("invalid session directory")?;
        if !selected.is_empty() && !selected.contains(id) {
            continue;
        }
        seen.insert(id.to_owned());
        let guard = lock(&crate::deploy::safe_path(&dir, "session.lock")?);
        let s = match raw_session(&dir) {
            Ok(s) => s,
            Err(_) => {
                protected.push(unavailable_summary(&dir)?);
                continue;
            }
        };
        let summary = session_summary(config, &dir, &s, guard.is_err())?;
        if summary["eligible_cleanup"] == true {
            let size = summary["disk_bytes"]
                .as_u64()
                .context("disk size missing")?;
            bytes = bytes.checked_add(size).context("cleanup size overflow")?;
            candidates.push(json!({"id":id,"fingerprint":session_fingerprint(config,&dir,&s)?,"disk_bytes":size,"status":s.status,"expires":summary["expires"]}));
        } else {
            protected.push(summary);
        }
    }
    ensure!(selected.is_subset(&seen), "unknown session ID requested");
    let plan = json!({"schema":1,"root":base,"candidates":candidates,"protected":protected,"reclaim_bytes":bytes});
    let digest = hash(&serde_json::to_vec(&plan)?);
    atom(
        &lifecycle_plan_file(&base),
        &json!({"digest":digest,"plan":plan}),
    )?;
    Ok(json!({"digest":digest,"plan":plan,"resume":false}))
}
fn archive_session(config: &Value, dir: &Path, s: &Session) -> Result<()> {
    let state = Path::new(config["paths"]["state"].as_str().context("state missing")?);
    let archive = crate::deploy::safe_path(state, "transfer-receipts")?;
    fs::create_dir_all(&archive)?;
    let receipt = if dir.join("receipt.json").exists() {
        Some(serde_json::from_slice::<Value>(&fs::read(
            crate::deploy::safe_path(dir, "receipt.json")?,
        )?)?)
    } else {
        None
    };
    atom(
        &crate::deploy::safe_path(&archive, &format!("{}.json", s.id))?,
        &json!({"id":s.id,"status":s.status,"created":s.created,"manifest":s.manifest,"receipt":receipt,"cleaned_at":now(),"cleanup_fingerprint":session_fingerprint(config,dir,s)?}),
    )
}
pub fn cleanup_apply(config: &Value, digest: &str, reviewed: bool) -> Result<Value> {
    cleanup_apply_with(config, digest, reviewed, || Ok(()))
}
fn cleanup_apply_with(
    config: &Value,
    digest: &str,
    reviewed: bool,
    after_rename: impl Fn() -> Result<()>,
) -> Result<Value> {
    ensure!(reviewed, "cleanup requires explicit reviewed confirmation");
    ensure!(valid_hash(digest), "invalid cleanup plan digest");
    let base = root(config)?;
    safe_lifecycle_files(&base)?;
    let _global = lock(&base.join("create.lock"))?;
    let journal_path = base.join("cleanup-journal.json");
    let last = base.join("cleanup-last.json");
    if !journal_path.exists() && last.exists() {
        let value: Value = serde_json::from_slice(&fs::read(&last)?)?;
        if value["digest"] == digest {
            return Ok(value["result"].clone());
        }
    }
    let plan_path = lifecycle_plan_file(&base);
    let saved: Value = serde_json::from_slice(&fs::read(if journal_path.exists() {
        &journal_path
    } else {
        &plan_path
    })?)?;
    ensure!(
        saved["digest"] == digest && hash(&serde_json::to_vec(&saved["plan"])?) == digest,
        "cleanup plan changed; preview again"
    );
    let plan = &saved["plan"];
    ensure!(
        plan["root"].as_str() == base.to_str(),
        "cleanup plan belongs to another state root"
    );
    let entries = plan["candidates"]
        .as_array()
        .context("cleanup candidates missing")?;
    ensure!(!entries.is_empty(), "no eligible sessions in cleanup plan");
    let recovering = journal_path.exists();
    let mut guards = Vec::new();
    // Validate and lock every selected session before deleting any. Protected
    // sessions remain intact when one candidate has changed since preview.
    for entry in entries {
        let id = entry["id"].as_str().context("session ID missing")?;
        let dir = session_dir(config, id)?;
        let tomb = crate::deploy::safe_path(&base, &format!("deleting-{id}"))?;
        ensure!(
            !(dir.exists() && tomb.exists()),
            "ambiguous cleanup tombstone"
        );
        if recovering && !dir.exists() {
            // remove_dir_all can be interrupted after deleting session.json.
            // A durable journal + archived fingerprint authorize only this exact
            // detached tombstone; never infer ownership from its filename alone.
            let state = Path::new(config["paths"]["state"].as_str().context("state missing")?);
            let archive = crate::deploy::safe_path(state, &format!("transfer-receipts/{id}.json"))?;
            let audit: Value = serde_json::from_slice(&fs::read(archive)?)?;
            ensure!(
                audit["id"] == id && audit["cleanup_fingerprint"] == entry["fingerprint"],
                "cleanup audit fingerprint mismatch"
            );
            if tomb.exists() {
                guards.push(lock(&crate::deploy::safe_path(&tomb, "session.lock")?)?);
                staged_bytes(&tomb)?;
            }
            continue;
        }
        if dir.exists() {
            let path = dir;
            guards.push(lock(&crate::deploy::safe_path(&path, "session.lock")?)?);
            let s = raw_session(&path)?;
            ensure!(s.id == id, "session identity mismatch");
            let summary = session_summary(config, &path, &s, false)?;
            ensure!(
                summary["eligible_cleanup"] == true,
                "session is active or needs recovery; cleanup refused"
            );
            ensure!(
                entry["fingerprint"] == session_fingerprint(config, &path, &s)?,
                "session changed; preview cleanup again"
            );
            archive_session(config, &path, &s)?;
        } else {
            bail!("session disappeared; preview cleanup again");
        }
    }
    if !recovering {
        atom(&journal_path, &json!({"digest":digest,"plan":plan}))?;
    }
    let mut removed = Vec::new();
    for entry in entries {
        let id = entry["id"].as_str().context("session ID missing")?;
        let dir = session_dir(config, id)?;
        let tomb = crate::deploy::safe_path(&base, &format!("deleting-{id}"))?;
        if dir.exists() {
            fs::rename(&dir, &tomb)?;
            File::open(&base)?.sync_all()?;
            after_rename()?;
        }
        if tomb.exists() {
            // Full-tree preflight forbids following any symlink during removal.
            staged_bytes(&tomb)?;
            fs::remove_dir_all(&tomb)?;
            File::open(&base)?.sync_all()?;
        }
        removed.push(id.to_owned());
    }
    let result = json!({"status":"cleaned","removed":removed,"reclaimed_bytes":plan["reclaim_bytes"],"imported_cache_preserved":true,"receipts_archived":true});
    atom(&last, &json!({"digest":digest,"result":result}))?;
    fs::remove_file(&journal_path)?;
    File::open(&base)?.sync_all()?;
    drop(guards);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(root: &Path) -> Value {
        json!({"paths":{"state":root.join("state"),"server":root.join("server"),"backups":root.join("backups"),"cache":root.join("cache")},"transfer":{"min_free_bytes":0}})
    }
    fn manifest(bytes: &[u8]) -> Manifest {
        Manifest {
            files: vec![TransferFile {
                kind: "world".into(),
                path: "world/level.dat".into(),
                size: bytes.len() as u64,
                sha256: hash(bytes),
            }],
            source_consistency: "stopped".into(),
        }
    }
    #[test]
    fn resume_replay_and_integrity() {
        let d = tempfile::tempdir().unwrap();
        let c = config(d.path());
        let s = create(&c, manifest(b"hello")).unwrap();
        let id = s["id"].as_str().unwrap();
        let token = s["token"].as_str().unwrap();
        assert!(chunk(&c, id, "wrong", 0, 0, &hash(b"hello"), b"hello").is_err());
        assert!(chunk(&c, id, token, 0, 1, &hash(b"hello"), b"hello").is_err());
        assert!(chunk(&c, id, token, 0, 0, &hash(b"wrong"), b"hello").is_err());
        chunk(&c, id, token, 0, 0, &hash(b"hello"), b"hello").unwrap();
        assert_eq!(status(&c, id, token).unwrap()["offsets"][0], 5);
        assert_eq!(
            chunk(&c, id, token, 0, 0, &hash(b"hello"), b"hello").unwrap()["replayed"],
            true
        );
        verify(&c, id, token).unwrap();
        assert!(chunk(&c, id, token, 0, 0, &hash(b"hello"), b"hello").is_err());
    }
    #[test]
    fn traversal_and_quotas() {
        let d = tempfile::tempdir().unwrap();
        let c = config(d.path());
        for path in ["../world/x", "/world/x", "world/../x", "plugins/x"] {
            let mut m = manifest(b"x");
            m.files[0].path = path.into();
            assert!(create(&c, m).is_err(), "{path}");
        }
        let mut c = c;
        c["transfer"]["max_session_bytes"] = json!(1);
        assert!(create(&c, manifest(b"too big")).is_err());
    }
    #[test]
    fn world_commit_requires_stopped_confirmation_and_preserves_backup() {
        let d = tempfile::tempdir().unwrap();
        let c = config(d.path());
        fs::create_dir_all(d.path().join("server/world")).unwrap();
        fs::create_dir_all(d.path().join("backups")).unwrap();
        fs::write(d.path().join("server/world/level.dat"), b"old").unwrap();
        let s = create(&c, manifest(b"new")).unwrap();
        let id = s["id"].as_str().unwrap();
        let token = s["token"].as_str().unwrap();
        chunk(&c, id, token, 0, 0, &hash(b"new"), b"new").unwrap();
        assert!(commit_with(&c, id, token, true, || Ok(())).is_err());
        verify(&c, id, token).unwrap();
        assert!(commit_with(&c, id, token, false, || Ok(())).is_err());
        assert!(commit_with(&c, id, token, true, || bail!("active")).is_err());
        commit_with(&c, id, token, true, || Ok(())).unwrap();
        assert_eq!(
            fs::read(d.path().join("server/world/level.dat")).unwrap(),
            b"new"
        );
        assert_eq!(
            fs::read(
                d.path()
                    .join(format!("backups/world-transfer-{id}/world/level.dat"))
            )
            .unwrap(),
            b"old"
        );
    }
    #[test]
    fn plain_public_http_forbidden() {
        assert!(endpoint("http://example.org:8765").is_err());
        assert!(endpoint("http://127.0.0.1:8765").is_ok());
        assert!(endpoint("https://example.org").is_ok());
    }
    #[test]
    fn explicit_unlock_releases_duplicated_open_description() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let guard = lock(&path).unwrap();
        let inherited = guard.0.try_clone().unwrap();
        drop(guard);
        let second = lock(&path).unwrap();
        drop(second);
        drop(inherited);
    }
    #[test]
    fn retained_committed_and_partial_bytes_still_consume_staging_quota() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config["transfer"]["max_session_bytes"] = json!(10);
        config["transfer"]["max_staging_bytes"] = json!(6);
        let session = create(&config, manifest(b"abc")).unwrap();
        let id = session["id"].as_str().unwrap();
        let token = session["token"].as_str().unwrap();
        chunk(&config, id, token, 0, 0, &hash(b"abc"), b"abc").unwrap();
        let (path, mut saved) = load(&config, id, token).unwrap();
        saved.status = "committed".into();
        save(&path, &saved).unwrap();
        assert!(create(&config, manifest(b"four")).is_err());
        // Simulate extra persisted bytes after an interrupted write: actual disk
        // usage is counted even if the session metadata no longer reserves it.
        fs::write(path.join("data/partial.tmp"), b"1234").unwrap();
        assert!(create(&config, manifest(b"x")).is_err());
    }
    #[test]
    fn cleanup_frees_quota_but_active_session_remains_usable() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(dir.path());
        c["transfer"]["max_session_bytes"] = json!(6);
        c["transfer"]["max_staging_bytes"] = json!(6);
        let a = create(&c, manifest(b"aaa")).unwrap();
        let b = create(&c, manifest(b"bbb")).unwrap();
        let bid = b["id"].as_str().unwrap();
        let bt = b["token"].as_str().unwrap();
        chunk(&c, bid, bt, 0, 0, &hash(b"bbb"), b"bbb").unwrap();
        assert!(create(&c, manifest(b"x")).is_err());
        assert!(cancel(&c, bid, "wrong").is_err());
        cancel(&c, bid, bt).unwrap();
        assert!(chunk(&c, bid, bt, 0, 0, &hash(b"bbb"), b"bbb").is_err());
        let listing = list_sessions(&c).unwrap();
        assert!(!listing.to_string().contains(bt));
        let plan = cleanup_plan(&c, &[]).unwrap();
        assert_eq!(plan["plan"]["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(plan["plan"]["protected"].as_array().unwrap().len(), 1);
        let digest = plan["digest"].as_str().unwrap();
        assert!(cleanup_apply(&c, digest, false).is_err());
        cleanup_apply(&c, digest, true).unwrap();
        assert!(!session_dir(&c, bid).unwrap().exists());
        assert!(
            dir.path()
                .join(format!("state/transfer-receipts/{bid}.json"))
                .exists()
        );
        create(&c, manifest(b"ccc")).unwrap();
        chunk(
            &c,
            a["id"].as_str().unwrap(),
            a["token"].as_str().unwrap(),
            0,
            0,
            &hash(b"aaa"),
            b"aaa",
        )
        .unwrap();
    }
    #[test]
    fn cleanup_protects_pending_worlds_and_revalidates_changed_plans() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        let a = create(&c, manifest(b"a")).unwrap();
        let id = a["id"].as_str().unwrap();
        let token = a["token"].as_str().unwrap();
        let (path, mut s) = load(&c, id, token).unwrap();
        s.created = 0;
        s.status = "recovery_required".into();
        save(&path, &s).unwrap();
        atom(&path.join("journal.json"), &json!({"phase":"prepared"})).unwrap();
        assert!(cancel_admin(&c, id).is_err());
        let plan = cleanup_plan(&c, &[]).unwrap();
        assert!(plan["plan"]["candidates"].as_array().unwrap().is_empty());
        assert!(path.exists());
        let b = create(&c, manifest(b"b")).unwrap();
        let bid = b["id"].as_str().unwrap();
        cancel_admin(&c, bid).unwrap();
        let plan = cleanup_plan(&c, &[bid.to_owned()]).unwrap();
        let bdir = session_dir(&c, bid).unwrap();
        let mut s = raw_session(&bdir).unwrap();
        s.status = "receiving".into();
        save(&bdir, &s).unwrap();
        assert!(cleanup_apply(&c, plan["digest"].as_str().unwrap(), true).is_err());
        assert!(bdir.exists());
        assert!(path.join("journal.json").exists());
    }
    #[test]
    fn cleanup_preserves_imported_jar_and_durable_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        fs::create_dir_all(dir.path().join("server")).unwrap();
        fs::create_dir_all(dir.path().join("backups")).unwrap();
        let mut m = manifest(b"jar");
        m.files[0].kind = "plugin".into();
        m.files[0].path = "Local.jar".into();
        let s = create(&c, m).unwrap();
        let id = s["id"].as_str().unwrap();
        let token = s["token"].as_str().unwrap();
        chunk(&c, id, token, 0, 0, &hash(b"jar"), b"jar").unwrap();
        verify(&c, id, token).unwrap();
        commit_with(&c, id, token, false, || Ok(())).unwrap();
        let plan = cleanup_plan(&c, &[id.to_owned()]).unwrap();
        cleanup_apply(&c, plan["digest"].as_str().unwrap(), true).unwrap();
        assert!(
            dir.path()
                .join(format!("cache/{}.jar", hash(b"jar")))
                .exists()
        );
        assert!(
            dir.path()
                .join(format!("cache/{}.receipt.json", hash(b"jar")))
                .exists()
        );
        let receipt: Value = serde_json::from_slice(
            &fs::read(
                dir.path()
                    .join(format!("state/transfer-receipts/{id}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            receipt["receipt"]["artifacts"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        );
        assert!(!receipt.to_string().contains(token));
        fs::create_dir_all(dir.path().join("requirements")).unwrap();
        fs::write(dir.path().join("requirements/plugins.json"),json!({"plugins":[{"name":"Local","filename":"Local.jar","pinned":{"transfer":true,"version":"fixture","sha256":hash(b"jar")},"source":{"provider":"transfer"}}]}).to_string()).unwrap();
        let pins = crate::artifacts::load_manifest(dir.path()).unwrap();
        let verified = crate::artifacts::download(&pins[0], &dir.path().join("cache")).unwrap();
        assert_eq!(fs::read(verified).unwrap(), b"jar");
    }
    #[test]
    fn cleanup_resumes_after_partial_tombstone_removal_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        let s = create(&c, manifest(b"partial")).unwrap();
        let id = s["id"].as_str().unwrap();
        let token = s["token"].as_str().unwrap();
        chunk(&c, id, token, 0, 0, &hash(b"partial"), b"partial").unwrap();
        cancel(&c, id, token).unwrap();
        let plan = cleanup_plan(&c, &[]).unwrap();
        let digest = plan["digest"].as_str().unwrap();
        assert!(
            cleanup_apply_with(&c, digest, true, || bail!("simulated crash after rename")).is_err()
        );
        let tomb = root(&c).unwrap().join(format!("deleting-{id}"));
        assert!(tomb.exists());
        fs::remove_file(tomb.join("session.json")).unwrap();
        fs::remove_file(tomb.join("data/0")).unwrap();
        assert_eq!(cleanup_plan(&c, &[]).unwrap()["resume"], true);
        let result = cleanup_apply(&c, digest, true).unwrap();
        assert!(!tomb.exists());
        assert_eq!(cleanup_apply(&c, digest, true).unwrap(), result);
        assert_eq!(list_sessions(&c).unwrap()["cleanup_pending"], false);
    }
    #[test]
    fn body_reader_holds_lock_even_if_session_expires_mid_request() {
        struct Reader<'a> {
            c: &'a Value,
            id: &'a str,
            read: bool,
        }
        impl Read for Reader<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.read {
                    return Ok(0);
                }
                self.read = true;
                assert!(cancel_admin(self.c, self.id).is_err());
                let path = session_dir(self.c, self.id).unwrap();
                let mut s = raw_session(&path).unwrap();
                s.created = 0;
                save(&path, &s).unwrap();
                let plan = cleanup_plan(self.c, &[]).unwrap();
                assert!(plan["plan"]["candidates"].as_array().unwrap().is_empty());
                assert!(
                    plan["plan"]["protected"][0]["protected_reason"]
                        .as_str()
                        .unwrap()
                        .contains("active operation")
                );
                out[..3].copy_from_slice(b"abc");
                Ok(3)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        let s = create(&c, manifest(b"abc")).unwrap();
        let id = s["id"].as_str().unwrap();
        let token = s["token"].as_str().unwrap();
        let mut reader = Reader {
            c: &c,
            id,
            read: false,
        };
        assert!(receive_chunk(&c, id, token, 0, 0, &hash(b"abc"), &mut reader, 3).is_err());
        let plan = cleanup_plan(&c, &[]).unwrap();
        assert_eq!(plan["plan"]["candidates"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn orphan_metadata_stays_protected_without_blocking_healthy_session_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        let s = create(&c, manifest(b"known")).unwrap();
        cancel_admin(&c, s["id"].as_str().unwrap()).unwrap();
        let orphan = root(&c).unwrap().join("a".repeat(64));
        fs::create_dir_all(orphan.join("data")).unwrap();
        fs::write(orphan.join("data/0"), b"orphan").unwrap();
        let listing = list_sessions(&c).unwrap();
        assert_eq!(listing["sessions"].as_array().unwrap().len(), 2);
        assert!(
            listing["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["status"] == "unavailable" && s["eligible_cleanup"] == false)
        );
        let plan = cleanup_plan(&c, &[]).unwrap();
        assert_eq!(plan["plan"]["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(plan["plan"]["protected"].as_array().unwrap().len(), 1);
        cleanup_apply(&c, plan["digest"].as_str().unwrap(), true).unwrap();
        assert!(orphan.join("data/0").exists());
    }
    #[test]
    fn destination_change_rejects_commit_and_original_binding_still_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        fs::create_dir_all(dir.path().join("server")).unwrap();
        fs::create_dir_all(dir.path().join("backups")).unwrap();
        let s = create(&c, manifest(b"new")).unwrap();
        let id = s["id"].as_str().unwrap();
        let token = s["token"].as_str().unwrap();
        chunk(&c, id, token, 0, 0, &hash(b"new"), b"new").unwrap();
        verify(&c, id, token).unwrap();
        assert!(has_active_sessions(&c).unwrap());
        let mut changed = c.clone();
        changed["paths"]["server"] = json!(dir.path().join("different-server"));
        assert!(commit_with(&changed, id, token, true, || Ok(())).is_err());
        assert!(!dir.path().join("different-server").exists());
        assert!(!dir.path().join("server/world").exists());
        let mut changed = c.clone();
        changed["deployment"]["service"] = json!("different.service");
        assert!(commit_with(&changed, id, token, true, || Ok(())).is_err());
        commit_with(&c, id, token, true, || Ok(())).unwrap();
        assert_eq!(
            fs::read(dir.path().join("server/world/level.dat")).unwrap(),
            b"new"
        );
        assert!(!has_active_sessions(&c).unwrap());
    }
    #[test]
    fn recovery_rejects_changed_destination_and_legacy_recovery_requires_exact_paths() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        fs::create_dir_all(dir.path().join("server/world")).unwrap();
        fs::create_dir_all(dir.path().join("backups")).unwrap();
        fs::write(dir.path().join("server/world/level.dat"), b"old").unwrap();
        let s = create(&c, manifest(b"new")).unwrap();
        let id = s["id"].as_str().unwrap();
        let token = s["token"].as_str().unwrap();
        chunk(&c, id, token, 0, 0, &hash(b"new"), b"new").unwrap();
        verify(&c, id, token).unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(
            commit_with(&c, id, token, true, || {
                calls.set(calls.get() + 1);
                if calls.get() == 3 {
                    bail!("crash after replacing worlds");
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(
            fs::read(dir.path().join("server/world/level.dat")).unwrap(),
            b"new"
        );
        let mut changed = c.clone();
        changed["paths"]["server"] = json!(dir.path().join("wrong-server"));
        assert!(recover_with(&changed, id, token, || Ok(())).is_err());
        assert!(!dir.path().join("wrong-server").exists());
        let sessiondir = session_dir(&c, id).unwrap();
        let mut state = raw_session(&sessiondir).unwrap();
        state.destination = None;
        save(&sessiondir, &state).unwrap();
        assert!(commit_with(&c, id, token, true, || Ok(())).is_err());
        let journalpath = sessiondir.join("journal.json");
        let mut journal: Value = serde_json::from_slice(&fs::read(&journalpath).unwrap()).unwrap();
        journal.as_object_mut().unwrap().remove("destination");
        let original = journal.clone();
        journal["stage"] = json!(dir.path().join("wrong-stage"));
        atom(&journalpath, &journal).unwrap();
        assert!(recover_with(&c, id, token, || Ok(())).is_err());
        assert_eq!(
            fs::read(dir.path().join("server/world/level.dat")).unwrap(),
            b"new"
        );
        atom(&journalpath, &original).unwrap();
        recover_with(&c, id, token, || Ok(())).unwrap();
        assert_eq!(
            fs::read(dir.path().join("server/world/level.dat")).unwrap(),
            b"old"
        );
    }
}
