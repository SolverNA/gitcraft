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
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
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
        if let Ok(bytes) = fs::read(path.join("session.json")) {
            let s: Session = serde_json::from_slice(&bytes)?;
            if s.status != "committed" {
                for f in s.manifest.files {
                    reserved = reserved.checked_add(f.size).context("quota overflow")?;
                }
            }
        }
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
pub fn create(config: &Value, manifest: Manifest) -> Result<Value> {
    let bytes = validate_manifest(config, &manifest)?;
    let base = root(config)?;
    let _guard = lock(&base.join("create.lock"))?;
    capacity(config, &base, bytes)?;
    let id = random();
    let token = random();
    let dir = session_dir(config, &id)?;
    fs::create_dir(&dir)?;
    fs::create_dir(dir.join("data"))?;
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
    };
    for index in 0..count {
        File::create(data(&dir, index)?)?.sync_all()?;
    }
    save(&dir, &s)?;
    Ok(json!({"id":id,"token":token,"chunk_bytes":CHUNK_BYTES,"status":"receiving"}))
}
pub fn status(config: &Value, id: &str, token: &str) -> Result<Value> {
    let (dir, s) = load(config, id, token)?;
    let _guard = lock(&dir.join("session.lock"))?;
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
    ensure!(
        !bytes.is_empty() && bytes.len() <= CHUNK_BYTES,
        "chunk outside byte limit"
    );
    ensure!(
        valid_hash(digest) && hash(bytes).eq_ignore_ascii_case(digest),
        "chunk SHA-256 mismatch"
    );
    let dir = session_dir(config, id)?;
    let _guard = lock(&dir.join("session.lock"))?;
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
    let mut journal = json!({"phase":"prepared","worlds":worlds,"existing":existing,"backup":backup,"stage":stage,"artifacts":receipts});
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
    let dir = session_dir(config, id)?;
    let _session = lock(&dir.join("session.lock"))?;
    // Recovery credentials remain valid for an interrupted journal even after upload TTL.
    let (_, mut s) = load_inner(config, id, token, true)?;
    let state = Path::new(config["paths"]["state"].as_str().context("state missing")?);
    let _deploy = lock(&state.join("deploy.lock"))?;
    crate::deploy::ensure_stopped(config)?;
    let journal: Value = serde_json::from_slice(&fs::read(dir.join("journal.json"))?)?;
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
}
