//! Loopback UI. Session cookies, CSRF tokens and no arbitrary paths or shell commands.
use crate::{config, deploy, transfer};
use anyhow::{Context, Result, bail};
use rand::{RngCore, rngs::OsRng};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tiny_http::{Header, Request, Response, Server};

pub fn status(value: &Value) -> Value {
    let mut result = json!({"service":value["deployment"]["service"],"poller":crate::agent_service::status(),"minecraft_autostart":crate::minecraft_autostart::status(value)});
    if let Some(state) = value["paths"]["state"].as_str() {
        for name in ["status", "deployment", "ui-operation"] {
            if let Ok(text) = fs::read_to_string(Path::new(state).join(format!("{name}.json")))
                && let Ok(v) = serde_json::from_str::<Value>(&text)
            {
                result[name] = v;
            }
        }
    }
    result
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn equal(left: &str, right: &str) -> bool {
    let a = Sha256::digest(left.as_bytes());
    let b = Sha256::digest(right.as_bytes());
    a.iter().zip(b).fold(0u8, |v, (x, y)| v | (x ^ y)) == 0
}
fn respond(request: Request, code: u16, body: String, cookie: Option<String>) {
    let mut response=Response::from_string(format!("<!doctype html><html lang=ru><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>GitCraft</title><style>body{{font:16px system-ui;max-width:900px;margin:40px auto;padding:20px;background:#111827;color:#eee}}pre,textarea{{white-space:pre-wrap;background:#1f2937;color:#eee;padding:16px}}textarea{{width:90%;height:180px}}button,input{{padding:10px;margin:8px}}a{{color:#93c5fd}}</style><h1>GitCraft</h1>{body}</html>")).with_status_code(code);
    for (name, value) in [
        ("Content-Type", "text/html; charset=utf-8"),
        ("Cache-Control", "no-store"),
        ("X-Frame-Options", "DENY"),
        ("X-Content-Type-Options", "nosniff"),
        (
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'",
        ),
    ] {
        response.add_header(Header::from_bytes(name, value).unwrap());
    }
    if let Some(cookie) = cookie {
        response.add_header(Header::from_bytes("Set-Cookie", cookie).unwrap());
    }
    let _ = request.respond(response);
}
fn header(request: &Request, key: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(key))
        .map(|h| h.value.as_str().to_owned())
}
fn decode(text: &str) -> Result<String> {
    let text = text.replace('+', " ");
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            anyhow::ensure!(index + 2 < bytes.len(), "Invalid encoding");
            let encoded = std::str::from_utf8(&bytes[index + 1..index + 3])?;
            out.push(u8::from_str_radix(encoded, 16)?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Ok(String::from_utf8(out)?)
}
fn fields(body: &str) -> Result<HashMap<String, String>> {
    body.split('&')
        .map(|field| {
            let (k, v) = field.split_once('=').context("Invalid form")?;
            Ok((decode(k)?, decode(v)?))
        })
        .collect()
}

fn json_response(request: Request, code: u16, value: Value) {
    let mut response = Response::from_string(value.to_string()).with_status_code(code);
    for (k, v) in [
        ("Content-Type", "application/json"),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
    ] {
        response.add_header(Header::from_bytes(k, v).unwrap());
    }
    let _ = request.respond(response);
}
fn read_json(request: &mut Request, max: usize) -> Result<Value> {
    anyhow::ensure!(
        request.body_length().is_some_and(|n| n <= max),
        "request body exceeds size limit"
    );
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= max, "request body exceeds size limit");
    Ok(serde_json::from_slice(&bytes)?)
}
fn transfer_api(mut request: Request, path: &Path, admin: &str, session_csrf: Option<&str>) {
    let result = (|| -> Result<Value> {
        let config = config::load(path)?;
        let route = request.url().to_owned();
        let method = request.method().as_str().to_owned();
        if let Some(origin) = header(&request, "Origin") {
            anyhow::ensure!(
                Some(origin) == header(&request, "Host").map(|h| format!("http://{h}")),
                "forbidden request origin"
            );
        }
        let bearer = header(&request, "Authorization")
            .and_then(|s| s.strip_prefix("Bearer ").map(str::to_owned))
            .unwrap_or_default();
        if route.starts_with("/api/transfer-sessions") {
            let csrf = header(&request, "X-CSRF-Token");
            anyhow::ensure!(
                equal(admin, &bearer)
                    || session_csrf
                        .zip(csrf.as_deref())
                        .is_some_and(|(a, b)| equal(a, b)),
                "Authentication required"
            );
            if route == "/api/transfer-sessions" && method == "GET" {
                return transfer::list_sessions(&config);
            }
            let body = read_json(&mut request, 16384)?;
            return match route.as_str() {
                "/api/transfer-sessions/cleanup-plan" => {
                    let ids = body["ids"]
                        .as_array()
                        .context("IDs must be an array")?
                        .iter()
                        .map(|id| id.as_str().map(str::to_owned).context("Invalid session ID"))
                        .collect::<Result<Vec<_>>>()?;
                    transfer::cleanup_plan(&config, &ids)
                }
                "/api/transfer-sessions/cleanup-apply" => transfer::cleanup_apply(
                    &config,
                    body["digest"].as_str().context("Cleanup digest missing")?,
                    body["reviewed"] == true,
                ),
                "/api/transfer-sessions/cancel" => transfer::cancel_admin(
                    &config,
                    body["id"].as_str().context("Session ID missing")?,
                ),
                _ => bail!("Unknown lifecycle route"),
            };
        }
        if route == "/api/transfers" && method == "POST" {
            let csrf = header(&request, "X-CSRF-Token");
            anyhow::ensure!(
                equal(admin, &bearer)
                    || session_csrf
                        .zip(csrf.as_deref())
                        .is_some_and(|(a, b)| equal(a, b)),
                "authentication required"
            );
            let manifest: transfer::Manifest =
                serde_json::from_value(read_json(&mut request, 16 * 1024 * 1024)?)?;
            if !equal(admin, &bearer) {
                anyhow::ensure!(
                    manifest
                        .files
                        .iter()
                        .all(|f| f.size <= transfer::BROWSER_FILE_BYTES),
                    "browser file exceeds 64 MiB; use gitcraft transfer plan/send streaming CLI"
                );
            }
            return transfer::create(&config, manifest);
        }
        let pieces: Vec<_> = route.trim_start_matches('/').split('/').collect();
        anyhow::ensure!(
            pieces.len() >= 3 && pieces[0] == "api" && pieces[1] == "transfers",
            "unknown transfer route"
        );
        let id = pieces[2];
        anyhow::ensure!(!bearer.is_empty(), "scoped transfer token required");
        if method == "GET" && pieces.len() == 3 {
            return transfer::status(&config, id, &bearer);
        }
        if method == "PUT" && pieces.len() == 6 && pieces[3] == "files" {
            let index = pieces[4].parse()?;
            let offset = pieces[5].parse()?;
            let digest =
                header(&request, "X-Chunk-SHA256").context("chunk SHA-256 header missing")?;
            // Authenticate before reading a potentially large request body.
            anyhow::ensure!(
                request
                    .body_length()
                    .is_some_and(|n| n <= transfer::CHUNK_BYTES),
                "chunk exceeds byte limit"
            );
            let length = request.body_length().context("Chunk length missing")?;
            return transfer::receive_chunk(
                &config,
                id,
                &bearer,
                index,
                offset,
                &digest,
                request.as_reader(),
                length,
            );
        }
        if method == "POST" && pieces.len() == 4 {
            let body = read_json(&mut request, 1024)?;
            return match pieces[3] {
                "verify" => transfer::verify(&config, id, &bearer),
                "commit" => {
                    transfer::commit(&config, id, &bearer, body["confirm_world_replace"] == true)
                }
                "recover" => transfer::recover(&config, id, &bearer),
                "cancel" => transfer::cancel(&config, id, &bearer),
                _ => bail!("unknown transfer operation"),
            };
        }
        bail!("unknown transfer route")
    })();
    match result {
        Ok(value) => json_response(request, 200, value),
        Err(error) => json_response(request, 400, json!({"error":error.to_string()})),
    }
}

fn transfer_page(csrf: &str) -> String {
    format!(
        "<h2>Прямой перенос local → VPS</h2><p>Данные идут прямо на этот сервер через SSH-туннель. Миры и JAR не попадают в Git или стороннее хранилище.</p><p>Браузер: максимум 64 MiB на файл, размер проверяется до чтения. Для больших файлов: <code>gitcraft transfer plan --source PATH --kind world --source-consistency stopped --output plan.json</code>, затем <code>gitcraft transfer send --endpoint http://127.0.0.1:8765 --plan plan.json --session session.json</code>. Для JAR укажите kind engine/plugin/mod.</p><input id=csrf type=hidden value='{csrf}'><label>Тип <select id=kind><option value=world>Мир (папка)</option><option value=engine>Ядро</option><option value=plugin>Плагины</option><option value=mod>Моды</option></select></label><p><input id=files type=file multiple><input id=folder type=file webkitdirectory multiple></p><label><input id=consistent type=checkbox>Локальный Minecraft остановлен или выбрана заранее сделанная согласованная snapshot-копия</label><p><select id=consistency><option value=stopped>Источник остановлен</option><option value=snapshot>Готовый snapshot</option></select></p><button id=send>Передать / продолжить</button><button id=verify>Проверить целые файлы</button><p><label><input id=confirm type=checkbox>Подтверждаю замену выбранных миров на VPS с резервной копией. Minecraft на VPS остановлен.</label></p><button id=commit>Применить проверенный перенос</button><button id=status>Статус</button><button id=recover>Восстановить прерванную замену (сервер остановлен)</button><progress id=progress max=100 value=0></progress><pre id=output>Выберите файлы/папку. При возобновлении выберите тот же источник.</pre><p>Session token хранится только в localStorage этого браузера; не передавайте его другим. После рестарта сервер сохранит смещения. Окончательное применение никогда не запускает Minecraft автоматически.</p><section><button id=sessions>Сессии и занятое место</button><pre id=sessionlist></pre><input id=cleanupids placeholder='Session IDs; пусто — все допустимые'><button id=cleanupplan>Показать план очистки</button><label><input id=cleanupreviewed type=checkbox>Я проверил план очистки</label><button id=cleanupapply>Применить очистку</button><button id=cancel>Отменить текущую сессию</button></section><a href='/'>Панель</a><script src='/assets/transfer.js'></script>"
    )
}
fn onboarding_page(csrf: &str, current: &Value) -> String {
    let hidden = format!("<input type=hidden name=csrf value='{csrf}'>");
    format!(
        "<h2>Подключение и проверка перед изменением</h2><p>Настройки подключения сначала сохраняются как план. Git-конфиги редактируются в отдельном рабочем клоне и отправляются через PR; bare-кэш агента для этого не подходит. GitHub write credentials нужны только локальному gh пользователя, серверу деплоя достаточно read-only ключа.</p><h3>Репозиторий / ветка</h3><form method=post action=/onboarding-plan>{hidden}<input name=url required value='{}'><input name=branch required value='{}'><button>Показать план</button></form><h3>Экспорт настроек из доступной этому процессу папки</h3><p>Для локального Minecraft запустите эту панель локально или используйте gitcraft export plan/apply. Браузер не открывает произвольные пути на другой машине.</p><form method=post action=/export-plan>{hidden}<input name=server required placeholder='/path/to/stopped-local-server'><textarea name=include placeholder='Явно выбранные относительные пути, по одному на строку'></textarea><input name=metadata placeholder='/path/to/artifacts-metadata.json'><button>Сканировать и показать план</button></form><p><a href=/editor>Просмотреть diff и редактировать одобренные конфиги</a></p><h3>Предложить проверенные правки через PR</h3><form method=post action=/propose>{hidden}<p>Используется только repository.authoring_path из локального agent.yml.</p><p>Целевая ветка PR берётся из repository.branch.</p><input name=title value='Minecraft configuration update' required><label><input type=checkbox name=reviewed value=yes required>Я проверил локальные изменения</label><button>Создать ветку и PR</button></form><h3>Подключить существующий сервер без перезаписи unit/Java/RCON</h3><form method=post action=/adopt-plan>{hidden}<input name=server required value='{}'><button>Показать adopt-план</button></form><a href='/'>Панель</a>",
        escape(current["repository"]["url"].as_str().unwrap_or("")),
        escape(current["repository"]["branch"].as_str().unwrap_or("main")),
        escape(current["paths"]["server"].as_str().unwrap_or(""))
    )
}
fn ui_plan_path(current: &Value, name: &str) -> Result<PathBuf> {
    let state = Path::new(
        current["paths"]["state"]
            .as_str()
            .context("state missing")?,
    );
    fs::create_dir_all(state)?;
    crate::deploy::safe_path(state, name)
}
fn write_ui_plan(path: &Path, value: &Value) -> Result<String> {
    use std::io::Write;
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().context("plan parent")?)?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
fn reviewed_plan(path: &Path, form: &HashMap<String, String>) -> Result<Value> {
    anyhow::ensure!(
        form.get("reviewed").is_some_and(|v| v == "yes"),
        "explicit review confirmation required"
    );
    let bytes = fs::read(path)?;
    anyhow::ensure!(
        form.get("digest") == Some(&hex::encode(Sha256::digest(&bytes))),
        "plan changed; review it again"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn review_html(
    csrf: &str,
    action: &str,
    digest: &str,
    plan: &Value,
    extra: &str,
) -> Result<String> {
    Ok(format!(
        "<h2>План — проверьте перед применением</h2><pre>{}</pre><form method=post action='{action}'><input type=hidden name=csrf value='{csrf}'><input type=hidden name=digest value='{digest}'>{extra}<label><input type=checkbox name=reviewed value=yes required>Я проверил план</label><button>Применить этот план</button></form><a href='/onboarding'>Назад</a>",
        escape(&serde_json::to_string_pretty(plan)?)
    ))
}
const EDIT_BYTES: u64 = 64 * 1024;
struct AuthoringLock(std::fs::File);
impl Drop for AuthoringLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn authoring_lock(repo: &Path) -> Result<AuthoringLock> {
    let path = crate::deploy::safe_path(repo, ".git/gitcraft-propose.lock")?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("authoring clone is busy with another edit or proposal")?;
    Ok(AuthoringLock(file))
}
fn authoring_file(repo: &Path, relative: &str) -> Result<(PathBuf, String, String)> {
    let approved = crate::policy::editable_paths(repo)?;
    anyhow::ensure!(
        approved.iter().any(|p| p == relative),
        "file is not an explicitly approved editable configuration"
    );
    crate::policy::config_path(relative)?;
    let path = crate::deploy::safe_path(repo, relative)?;
    anyhow::ensure!(
        path.is_file() && fs::metadata(&path)?.len() <= EDIT_BYTES,
        "editor accepts regular UTF-8 files up to 64 KiB; use CLI for larger configs"
    );
    let content = fs::read_to_string(&path)?;
    crate::policy::validate_config_file(relative, &content).map_err(|_| {
        anyhow::anyhow!("configuration rejected by shared policy; contents omitted")
    })?;
    let hash = hex::encode(Sha256::digest(content.as_bytes()));
    Ok((path, content, hash))
}
fn head_content(repo: &Path, relative: &str) -> Result<String> {
    let output = crate::gitrepo::run_bounded(
        std::process::Command::new("git").current_dir(repo).args([
            "-c",
            "core.hooksPath=/dev/null",
            "show",
            "--no-ext-diff",
            "--no-textconv",
            &format!("HEAD:{relative}"),
        ]),
        Duration::from_secs(10),
    )?;
    anyhow::ensure!(
        output.status.success() && output.stdout.len() as u64 <= EDIT_BYTES,
        "tracked HEAD config unavailable or too large for editor"
    );
    let content = String::from_utf8(output.stdout)?;
    crate::policy::validate_config_file(relative, &content).map_err(|_| {
        anyhow::anyhow!("historical configuration rejected by policy; diff and values omitted")
    })?;
    Ok(content)
}
fn authoring_head(repo: &Path) -> Result<String> {
    let output = crate::gitrepo::run_bounded(
        std::process::Command::new("git").current_dir(repo).args([
            "rev-parse",
            "--verify",
            "HEAD^{commit}",
        ]),
        Duration::from_secs(10),
    )?;
    anyhow::ensure!(output.status.success(), "authoring HEAD unavailable");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
fn authoring_connection(current: &Value) -> Value {
    json!({"url":current["repository"]["url"],"branch":current["repository"]["branch"],"authoring_path":current["repository"]["authoring_path"]})
}
fn text_diff(relative: &str, before: &str, after: &str) -> String {
    let mut diff = format!(
        "--- HEAD/{relative}\n+++ proposed/{relative}\n@@ -1,{} +1,{} @@\n",
        before.lines().count(),
        after.lines().count()
    );
    for line in before.lines() {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in after.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}
fn editor_list(current: &Value, csrf: &str) -> Result<String> {
    let repo = crate::repository::authoring_repository(current)?;
    let _guard = authoring_lock(&repo)?;
    let paths = crate::policy::editable_paths(&repo)?;
    let mut html = format!(
        "<h2>Редактор конфигов через отдельный рабочий клон</h2><p>{}</p><p>До 64 KiB на файл. Только существующие явно одобренные конфиги. Сохранение меняет рабочий клон; production обновится после PR и merge.</p>",
        escape(&repo.to_string_lossy())
    );
    for relative in paths {
        html.push_str(&format!("<form method=post action=/editor-open><input type=hidden name=csrf value='{csrf}'><input type=hidden name=file value='{}'><button>{}</button></form>",escape(&relative),escape(&relative)));
    }
    html.push_str("<a href='/onboarding'>PR и подключение</a>");
    Ok(html)
}
fn editor_open(current: &Value, csrf: &str, relative: &str) -> Result<String> {
    let repo = crate::repository::authoring_repository(current)?;
    let _guard = authoring_lock(&repo)?;
    let (_, content, hash) = authoring_file(&repo, relative)?;
    let original = head_content(&repo, relative)?;
    Ok(format!(
        "<h2>{}</h2><h3>Текущие изменения относительно HEAD</h3><pre>{}</pre><form method=post action=/editor-preview><input type=hidden name=csrf value='{csrf}'><input type=hidden name=file value='{}'><input type=hidden name=oldhash value='{hash}'><textarea name=content maxlength=65536>{}</textarea><button>Проверить и показать diff</button></form><a href='/editor'>Файлы</a>",
        escape(relative),
        escape(&text_diff(relative, &original, &content)),
        escape(relative),
        escape(&content)
    ))
}
fn editor_preview(current: &Value, csrf: &str, form: &HashMap<String, String>) -> Result<String> {
    let repo = crate::repository::authoring_repository(current)?;
    let _guard = authoring_lock(&repo)?;
    let relative = form.get("file").context("file missing")?;
    let (_, _, oldhash) = authoring_file(&repo, relative)?;
    anyhow::ensure!(
        form.get("oldhash") == Some(&oldhash),
        "file changed; reopen before editing"
    );
    let content = form.get("content").context("content missing")?;
    anyhow::ensure!(
        content.len() as u64 <= EDIT_BYTES,
        "editor content exceeds 64 KiB"
    );
    crate::policy::validate_config_file(relative, content).map_err(|_| {
        anyhow::anyhow!("proposed configuration rejected by policy; contents omitted")
    })?;
    let original = head_content(&repo, relative)?;
    let plan = json!({"repository":repo,"connection":authoring_connection(current),"head":authoring_head(&repo)?,"file":relative,"oldhash":oldhash,"content":content});
    let digest = write_ui_plan(&ui_plan_path(current, "ui-edit.json")?, &plan)?;
    Ok(format!(
        "<h2>Проверьте изменения</h2><pre>{}</pre><form method=post action=/editor-save><input type=hidden name=csrf value='{csrf}'><input type=hidden name=digest value='{digest}'><label><input type=checkbox name=reviewed value=yes required>Я проверил diff</label><button>Сохранить в рабочем клоне</button></form>",
        escape(&text_diff(relative, &original, content))
    ))
}
fn editor_save(current: &Value, form: &HashMap<String, String>) -> Result<String> {
    use std::io::Write;
    let plan_path = ui_plan_path(current, "ui-edit.json")?;
    let plan = reviewed_plan(&plan_path, form)?;
    let repo = crate::repository::authoring_repository(current)?;
    anyhow::ensure!(
        plan["repository"].as_str() == repo.to_str(),
        "authoring path changed; review a fresh plan"
    );
    let _guard = authoring_lock(&repo)?;
    anyhow::ensure!(
        plan["connection"] == authoring_connection(current)
            && plan["head"] == authoring_head(&repo)?,
        "repository connection or HEAD changed; review a fresh plan"
    );
    let relative = plan["file"].as_str().context("planned file missing")?;
    let (path, before, oldhash) = authoring_file(&repo, relative)?;
    anyhow::ensure!(
        plan["oldhash"] == oldhash,
        "file changed after preview; no overwrite performed"
    );
    let content = plan["content"]
        .as_str()
        .context("planned content missing")?;
    anyhow::ensure!(
        content.len() as u64 <= EDIT_BYTES,
        "planned content exceeds editor limit"
    );
    crate::policy::validate_config_file(relative, content)
        .map_err(|_| anyhow::anyhow!("planned content rejected; values omitted"))?;
    let persist = |bytes: &[u8]| -> Result<()> {
        let mut tmp = tempfile::NamedTempFile::new_in(path.parent().context("file parent")?)?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(&path)?;
        std::fs::File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    };
    persist(content.as_bytes())?;
    if crate::policy::validate_repository(&repo).is_err() {
        persist(before.as_bytes())?;
        bail!("repository policy rejected save; previous content restored and values omitted");
    }
    fs::remove_file(plan_path)?;
    Ok("<p>Сохранено только в отдельном рабочем клоне. Проверьте diff и создайте PR.</p><a href='/editor'>Проверить изменения</a> · <a href='/onboarding'>Создать PR</a>".into())
}

#[cfg(test)]
pub(crate) type OperationRunner =
    dyn Fn(&Value, &str, &HashMap<String, String>) -> Result<Value> + Send + Sync;
#[derive(Clone, Default)]
struct RuntimeHooks {
    #[cfg(test)]
    runner: Option<Arc<OperationRunner>>,
    #[cfg(test)]
    stopped: Option<Arc<AtomicBool>>,
}
impl RuntimeHooks {
    fn operation(
        &self,
        config: &Value,
        name: &str,
        form: &HashMap<String, String>,
    ) -> Result<Value> {
        #[cfg(test)]
        if let Some(runner) = &self.runner {
            return runner(config, name, form);
        }
        crate::ops_ui::run_operation(config, name, form)
    }
    fn adopt_plan(&self, config: &Value, server: &Path) -> Result<Value> {
        #[cfg(test)]
        if let Some(runner) = &self.runner {
            return runner(
                config,
                "adopt-plan",
                &HashMap::from([("server".into(), server.to_string_lossy().into_owned())]),
            );
        }
        crate::adopt::plan(config, server)
    }
    fn adopt(&self, config: &Value, plan: &Value, repo: &Path, expected: &str) -> Result<Value> {
        #[cfg(test)]
        if let Some(runner) = &self.runner {
            let form = HashMap::from([
                ("plan".into(), serde_json::to_string(plan)?),
                ("repository".into(), repo.to_string_lossy().into_owned()),
                ("expected_commit".into(), expected.into()),
            ]);
            return runner(config, "adopt", &form);
        }
        crate::adopt::apply(config, plan, repo, expected)
    }
    fn stopped(&self) -> bool {
        #[cfg(test)]
        if let Some(stopped) = &self.stopped {
            return stopped.load(Ordering::SeqCst);
        }
        false
    }
}
#[cfg(test)]
pub(crate) fn serve_fixture(
    path: &Path,
    runner: Arc<OperationRunner>,
    stopped: Arc<AtomicBool>,
) -> Result<()> {
    let mut listener = None;
    let hooks = RuntimeHooks {
        runner: Some(runner),
        stopped: Some(stopped),
    };
    while !hooks.stopped() {
        listener = serve_instance(path, listener, hooks.clone())?;
    }
    Ok(())
}
pub fn serve(path: &Path) -> Result<()> {
    crate::ops_ui::recover_interrupted_ui(path)?;
    let mut listener = None;
    loop {
        listener = serve_instance(path, listener, RuntimeHooks::default())?;
    }
}
fn serve_instance(
    path: &Path,
    listener: Option<Server>,
    hooks: RuntimeHooks,
) -> Result<Option<Server>> {
    let value = config::load(path)?;
    let env = value["web"]["token_env"]
        .as_str()
        .context("Missing token reference")?;
    let _ = env;
    let mut token = crate::secrets::resolve(&value, "web")?;
    anyhow::ensure!(
        token.len() >= 24,
        "Web token must contain at least 24 characters"
    );
    let host = value["web"]["host"].as_str().context("Host missing")?;
    let port = value["web"]["port"].as_u64().context("Port missing")?;
    let address = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let server = match listener {
        Some(server) => server,
        None => Server::http(&address).map_err(|e| anyhow::anyhow!(e.to_string()))?,
    };
    let mut sessions: HashMap<String, (String, Instant)> = HashMap::new();
    let busy = Arc::new(AtomicBool::new(false));
    let settings_lock = Arc::new(Mutex::new(()));
    for mut request in server.incoming_requests() {
        if hooks.stopped() {
            respond(request, 503, "Fixture stopped".into(), None);
            return Ok(None);
        }
        let auth = crate::secrets::resolve(&config::load(path)?, "web")?;
        if !equal(&auth, &token) {
            token = auth;
            sessions.clear();
        }
        sessions.retain(|_, (_, expiry)| *expiry > Instant::now());
        let session_id = header(&request, "Cookie").and_then(|cookie| {
            cookie.split(';').find_map(|piece| {
                piece
                    .trim()
                    .strip_prefix("gitcraft_session=")
                    .map(str::to_owned)
            })
        });
        let csrf = session_id
            .as_ref()
            .and_then(|id| sessions.get(id))
            .map(|(csrf, _)| csrf.clone());
        let method = request.method().as_str().to_owned();
        let url = request.url().to_owned();
        if url.starts_with("/api/transfers") || url.starts_with("/api/transfer-sessions") {
            transfer_api(request, path, &token, csrf.as_deref());
            continue;
        }
        if method == "GET" && url == "/assets/transfer.js" {
            let mut response = Response::from_string(include_str!("transfer.js"));
            response.add_header(
                Header::from_bytes("Content-Type", "text/javascript; charset=utf-8").unwrap(),
            );
            response.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
            let _ = request.respond(response);
            continue;
        }
        if method == "GET" && url == "/transfer" {
            if let Some(csrf) = csrf {
                respond(request, 200, transfer_page(&csrf), None);
            } else {
                respond(
                    request,
                    403,
                    "Войдите через <a href='/'>панель</a>.".into(),
                    None,
                );
            }
            continue;
        }
        if method == "GET" && url == "/operations" {
            if let Some(csrf) = csrf {
                match crate::ops_ui::page(path, &csrf) {
                    Ok(html) => respond(request, 200, html, None),
                    Err(error) => respond(request, 400, escape(&error.to_string()), None),
                }
            } else {
                respond(request, 403, "Login required".into(), None);
            }
            continue;
        }
        if method == "GET" && url == "/onboarding" {
            if let Some(csrf) = csrf {
                respond(
                    request,
                    200,
                    onboarding_page(&csrf, &config::load(path)?),
                    None,
                );
            } else {
                respond(request, 403, "Login required".into(), None);
            }
            continue;
        }
        if method == "GET" && url == "/editor" {
            if let Some(csrf) = csrf {
                match editor_list(&config::load(path)?, &csrf) {
                    Ok(html) => respond(request, 200, html, None),
                    Err(error) => respond(
                        request,
                        400,
                        format!(
                            "<p>{}</p><a href='/onboarding'>Подключение</a>",
                            escape(&error.to_string())
                        ),
                        None,
                    ),
                }
            } else {
                respond(request, 403, "Login required".into(), None);
            }
            continue;
        }
        if method == "GET" && url == "/" {
            if let Some(csrf) = csrf {
                let current = config::load(path)?;
                let editable = json!({"poll_interval_seconds":current["repository"]["poll_interval_seconds"],"empty_grace_seconds":current["deployment"]["empty_grace_seconds"],"health_timeout_seconds":current["deployment"]["health_timeout_seconds"]});
                let hidden = format!("<input type=hidden name=csrf value='{csrf}'>");
                respond(
                    request,
                    200,
                    format!(
                        "<p><a href=/operations>Полные настройки / bootstrap / backup / recovery</a> · <a href=/transfer>Перенести миры и JAR напрямую</a> · <a href=/onboarding>Репозиторий / экспорт / adopt</a></p><h2>Состояние</h2><pre>{}</pre><form method=post action=/deploy>{hidden}<button>Проверить и применить, когда сервер пуст</button></form><h2>Локальные настройки</h2><p>Можно менять вручную в agent.yml. Конфиги Minecraft меняются через PR.</p><form method=post action=/settings>{hidden}<textarea name=settings>{}</textarea><button>Сохранить</button></form><form method=post action=/logout>{hidden}<button>Выйти</button></form>",
                        escape(&serde_json::to_string_pretty(&status(&current))?),
                        escape(&serde_json::to_string_pretty(&editable)?)
                    ),
                    None,
                );
            } else {
                respond(request,200,"<p>Локальная панель управления</p><form method=post action=/login><input type=password name=token required autocomplete=current-password><button>Войти</button></form>".into(),None);
            }
            continue;
        }
        if method != "POST" {
            respond(request, 404, "Not found".into(), None);
            continue;
        }
        if let Some(origin) = header(&request, "Origin")
            && Some(origin) != header(&request, "Host").map(|host| format!("http://{host}"))
        {
            respond(request, 403, "Forbidden origin".into(), None);
            continue;
        }
        let body_limit = if url == "/editor-preview" || url.starts_with("/operations-settings") {
            256 * 1024
        } else {
            16384
        };
        if request.body_length().is_none_or(|n| n > body_limit) {
            respond(request, 413, "Invalid body length".into(), None);
            continue;
        }
        let mut body = String::new();
        if request
            .as_reader()
            .take(body_limit as u64 + 1)
            .read_to_string(&mut body)
            .is_err()
            || body.len() > body_limit
        {
            respond(request, 400, "Invalid body".into(), None);
            continue;
        }
        let form = match fields(&body) {
            Ok(v) => v,
            Err(_) => {
                respond(request, 400, "Invalid form".into(), None);
                continue;
            }
        };
        if url == "/login" {
            if !equal(&token, form.get("token").map(String::as_str).unwrap_or("")) {
                respond(request, 403, "Invalid login".into(), None);
                continue;
            }
            if sessions.len() >= 100 {
                sessions.clear();
            }
            let id = random();
            sessions.insert(
                id.clone(),
                (random(), Instant::now() + Duration::from_secs(3600)),
            );
            respond(
                request,
                200,
                "<a href='/'>Открыть панель</a>".into(),
                Some(format!(
                    "gitcraft_session={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=3600"
                )),
            );
            continue;
        }
        if !csrf
            .as_ref()
            .is_some_and(|csrf| form.get("csrf").is_some_and(|given| equal(csrf, given)))
        {
            respond(request, 403, "Invalid session or CSRF token".into(), None);
            continue;
        }
        if url.starts_with("/operation/") {
            let operation = url.trim_start_matches("/operation/").to_owned();
            if busy.swap(true, Ordering::SeqCst) {
                respond(request, 409, "Operation already running".into(), None);
                continue;
            }
            let current = config::load(path)?;
            if let Err(error) = crate::ops_ui::validate_operation(&operation, &form) {
                busy.store(false, Ordering::SeqCst);
                crate::ops_ui::record(
                    &current,
                    &operation,
                    "rejected",
                    &json!({"message":error.to_string()}),
                )?;
                respond(request, 400, escape(&error.to_string()), None);
                continue;
            }
            crate::ops_ui::record(&current, &operation, "running", &json!({}))?;
            let guard = busy.clone();
            let arguments = form.clone();
            let hooks = hooks.clone();
            std::thread::spawn(move || {
                let result = hooks.operation(&current, &operation, &arguments);
                let (status, value) = crate::ops_ui::outcome(&current, result);
                let _ = crate::ops_ui::record(&current, &operation, &status, &value);
                guard.store(false, Ordering::SeqCst);
            });
            respond(
                request,
                202,
                "<p>Операция запущена. Итог появится в статусе.</p><a href='/'>Панель</a>".into(),
                None,
            );
            continue;
        }
        match url.as_str() {
            "/operations-settings-preview" => {
                match crate::ops_ui::preview(path, csrf.as_deref().unwrap(), &form) {
                    Ok(html) => respond(request, 200, html, None),
                    Err(error) => respond(request, 400, escape(&error.to_string()), None),
                }
            }
            "/operations-settings-apply" => {
                let outcome = (|| -> Result<Option<Server>> {
                    anyhow::ensure!(
                        !busy.load(Ordering::SeqCst),
                        "An operation is running; settings rejected"
                    );
                    let _guard = settings_lock
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Settings lock unavailable"))?;
                    let current = config::load(path)?;
                    let _deployment = crate::deploy::lock_state(&current)?;
                    let plan = crate::ops_ui::settings_plan(path, &form)?;
                    let proposed = &plan["proposed"];
                    let newserver = if current["web"]["host"] != proposed["web"]["host"]
                        || current["web"]["port"] != proposed["web"]["port"]
                    {
                        let host = proposed["web"]["host"]
                            .as_str()
                            .context("Bind address missing")?;
                        let port = proposed["web"]["port"]
                            .as_u64()
                            .context("Bind port missing")?;
                        let address = if host.contains(':') {
                            format!("[{host}]:{port}")
                        } else {
                            format!("{host}:{port}")
                        };
                        Some(Server::http(&address).map_err(|_| {
                            anyhow::anyhow!("New Web address cannot be bound; settings unchanged")
                        })?)
                    } else {
                        None
                    };
                    config::save_checked(
                        path,
                        proposed.clone(),
                        plan["revision"].as_str().context("Revision missing")?,
                    )?;
                    Ok(newserver)
                })();
                match outcome {
                    Ok(next) => {
                        respond(request,200,"<p>Настройки сохранены. Если изменён Web адрес/token — переподключитесь и войдите заново.</p><a href='/operations'>Операции</a>".into(),None);
                        if let Some(next) = next {
                            return Ok(Some(next));
                        }
                    }
                    Err(error) => respond(request, 400, escape(&error.to_string()), None),
                }
            }
            "/credential-save" => {
                let outcome = (|| -> Result<Value> {
                    anyhow::ensure!(!busy.load(Ordering::SeqCst), "An operation is running");
                    anyhow::ensure!(
                        form.get("confirmed").is_some_and(|v| v == "yes"),
                        "Credential confirmation required"
                    );
                    let current = config::load(path)?;
                    let _lock = crate::deploy::lock_state(&current)?;
                    let role = form.get("role").context("Credential role missing")?;
                    if role == "rcon" {
                        crate::deploy::ensure_stopped(&current)?;
                    }
                    crate::secrets::write(
                        &current,
                        role,
                        form.get("secret").context("Credential missing")?,
                    )
                })();
                match outcome{Ok(value)=>respond(request,200,format!("<pre>{}</pre><a href='/'>Login / панель</a>",escape(&value.to_string())),None),Err(_)=>respond(request,400,"Credential update rejected. Check confirmation, stopped service and private provider permissions; values omitted.".into(),None)}
            }
            "/editor-open" | "/editor-preview" | "/editor-save" => {
                let result = (|| -> Result<String> {
                    let current = config::load(path)?;
                    let csrf = csrf.as_deref().context("session missing")?;
                    match url.as_str() {
                        "/editor-open" => {
                            editor_open(&current, csrf, form.get("file").context("file missing")?)
                        }
                        "/editor-preview" => editor_preview(&current, csrf, &form),
                        "/editor-save" => editor_save(&current, &form),
                        _ => bail!("unknown edit action"),
                    }
                })();
                match result {
                    Ok(html) => respond(request, 200, html, None),
                    Err(error) => respond(
                        request,
                        400,
                        format!(
                            "<p>Ошибка: {}</p><a href='/editor'>Назад</a>",
                            escape(&error.to_string())
                        ),
                        None,
                    ),
                }
            }
            "/onboarding-plan" | "/onboarding-apply" | "/export-plan" | "/export-apply"
            | "/adopt-plan" | "/adopt-apply" => {
                let outcome = (|| -> Result<String> {
                    let _guard = settings_lock
                        .lock()
                        .map_err(|_| anyhow::anyhow!("settings lock unavailable"))?;
                    let current = config::load(path)?;
                    let csrf = csrf.as_deref().context("session missing")?;
                    match url.as_str() {
                        "/onboarding-plan" => {
                            let mut proposed = current.clone();
                            proposed["repository"]["url"] =
                                json!(form.get("url").context("repository URL missing")?);
                            proposed["repository"]["branch"] =
                                json!(form.get("branch").context("branch missing")?);
                            let proposed = config::validate(proposed)?;
                            let plan = json!({"previous":current,"proposed":proposed,"revision":config::revision(path)?});
                            let digest = write_ui_plan(
                                &ui_plan_path(&current, "ui-onboarding.json")?,
                                &plan,
                            )?;
                            review_html(csrf, "/onboarding-apply", &digest, &plan, "")
                        }
                        "/onboarding-apply" => {
                            let p = ui_plan_path(&current, "ui-onboarding.json")?;
                            let plan = reviewed_plan(&p, &form)?;
                            anyhow::ensure!(
                                plan["previous"] == current,
                                "configuration changed; create a fresh plan"
                            );
                            anyhow::ensure!(
                                !busy.load(Ordering::SeqCst),
                                "An operation is running"
                            );
                            config::save_operational_checked(
                                path,
                                plan["proposed"].clone(),
                                plan["revision"].as_str().context("Revision missing")?,
                            )?;
                            fs::remove_file(p)?;
                            Ok("<p>Подключение сохранено. Следующая проверка агента перечитает конфигурацию.</p><a href='/'>Панель</a>".into())
                        }
                        "/export-plan" => {
                            let source =
                                PathBuf::from(form.get("server").context("source missing")?);
                            let include: Vec<String> = form
                                .get("include")
                                .map(String::as_str)
                                .unwrap_or("")
                                .lines()
                                .map(str::trim)
                                .filter(|p| !p.is_empty())
                                .map(str::to_owned)
                                .collect();
                            let metadata = form
                                .get("metadata")
                                .filter(|p| !p.trim().is_empty())
                                .map(PathBuf::from);
                            let plan = crate::export::plan(&source, &include, metadata.as_deref())?;
                            let digest =
                                write_ui_plan(&ui_plan_path(&current, "ui-export.json")?, &plan)?;
                            review_html(
                                csrf,
                                "/export-apply",
                                &digest,
                                &plan,
                                "<input name=destination required placeholder='/path/to/separate-working-clone'>",
                            )
                        }
                        "/export-apply" => {
                            let p = ui_plan_path(&current, "ui-export.json")?;
                            reviewed_plan(&p, &form)?;
                            let destination = PathBuf::from(
                                form.get("destination").context("destination missing")?,
                            );
                            let result = crate::export::apply(&p, &destination, true)?;
                            Ok(format!(
                                "<pre>{}</pre><p>Изменения подготовлены локально. Проверьте diff и отправьте PR через onboarding.</p><a href='/onboarding'>Назад</a>",
                                escape(&result.to_string())
                            ))
                        }
                        "/adopt-plan" => {
                            let server =
                                PathBuf::from(form.get("server").context("server missing")?);
                            let plan = hooks.adopt_plan(&current, &server)?;
                            let digest =
                                write_ui_plan(&ui_plan_path(&current, "ui-adopt.json")?, &plan)?;
                            review_html(
                                csrf,
                                "/adopt-apply",
                                &digest,
                                &plan,
                                "<input name=repository required placeholder='/path/to/reviewed-config-checkout'><input name=expected_commit required placeholder='Full approved commit SHA'>",
                            )
                        }
                        "/adopt-apply" => {
                            let plan =
                                reviewed_plan(&ui_plan_path(&current, "ui-adopt.json")?, &form)?;
                            let repo = PathBuf::from(
                                form.get("repository").context("repository missing")?,
                            );
                            let expected = form
                                .get("expected_commit")
                                .context("approved commit missing")?;
                            let result = hooks.adopt(&current, &plan, &repo, expected)?;
                            let (operation_status, details) =
                                crate::ops_ui::outcome(&current, Ok(result.clone()));
                            crate::ops_ui::record(&current, "adopt", &operation_status, &details)?;
                            Ok(format!(
                                "<pre>{}</pre><a href='/operations'>Проверить автоматизацию / повторить включение агента</a> · <a href='/'>Панель</a>",
                                escape(&result.to_string())
                            ))
                        }
                        _ => bail!("unknown onboarding action"),
                    }
                })();
                match outcome {
                    Ok(html) => respond(request, 200, html, None),
                    Err(error) => respond(
                        request,
                        400,
                        format!(
                            "<p>Ошибка: {}</p><a href='/onboarding'>Назад</a>",
                            escape(&error.to_string())
                        ),
                        None,
                    ),
                }
            }
            "/propose" => {
                if busy.swap(true, Ordering::SeqCst) {
                    respond(request, 409, "Operation already running".into(), None);
                    continue;
                }
                if form.get("reviewed").is_none_or(|v| v != "yes") {
                    busy.store(false, Ordering::SeqCst);
                    respond(request, 400, "Review confirmation required".into(), None);
                    continue;
                }
                let busy = busy.clone();
                let path = path.to_path_buf();
                let title = form.get("title").cloned().unwrap_or_default();
                std::thread::spawn(move || {
                    let outcome = (|| -> Result<()> {
                        let current = config::load(&path)?;
                        let target = ui_plan_path(&current, "ui-operation.json")?;
                        write_ui_plan(&target, &json!({"status":"running","operation":"propose"}))?;
                        let base = current["repository"]["branch"]
                            .as_str()
                            .context("configured branch missing")?;
                        let result = crate::repository::authoring_repository(&current)
                            .and_then(|repo| crate::repository::propose(&repo, base, &title));
                        let value = match result {
                            Ok(result) => json!({"status":"completed","result":result}),
                            Err(error) => json!({"status":"error","error":error.to_string()}),
                        };
                        write_ui_plan(&target, &value)?;
                        Ok(())
                    })();
                    if outcome.is_err() {
                        eprintln!(
                            "UI proposal failed; inspect local configuration and credentials"
                        );
                    }
                    busy.store(false, Ordering::SeqCst);
                });
                respond(request,202,"<p>Создание PR запущено. Результат или ошибка появится в состоянии панели; успех пока не подтверждён.</p><a href='/'>Панель</a>".into(),None);
            }
            "/logout" => {
                if let Some(id) = session_id {
                    sessions.remove(&id);
                }
                respond(
                    request,
                    200,
                    "<a href='/'>Войти</a>".into(),
                    Some("gitcraft_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0".into()),
                );
            }
            "/deploy" => {
                if busy.swap(true, Ordering::SeqCst) {
                    respond(request, 409, "Operation already running".into(), None);
                    continue;
                }
                let busy = busy.clone();
                let path = PathBuf::from(path);
                std::thread::spawn(move || {
                    if let Ok(value) = config::load(&path) {
                        let _ = crate::ops_ui::record(&value, "deploy", "running", &json!({}));
                        let (status, result) = crate::ops_ui::outcome(&value, deploy::run(&value));
                        let _ = crate::ops_ui::record(&value, "deploy", &status, &result);
                    }
                    busy.store(false, Ordering::SeqCst);
                });
                respond(request,202,"<p>Проверка запущена. Результат появится в состоянии.</p><a href='/'>Вернуться</a>".into(),None);
            }
            "/settings" => {
                let result = (|| -> Result<()> {
                    let _guard = settings_lock
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Settings lock unavailable"))?;
                    let edit: Value =
                        serde_json::from_str(form.get("settings").context("Settings missing")?)?;
                    let object = edit.as_object().context("Settings mapping required")?;
                    if object.len() != 3
                        || object.keys().any(|key| {
                            ![
                                "poll_interval_seconds",
                                "empty_grace_seconds",
                                "health_timeout_seconds",
                            ]
                            .contains(&key.as_str())
                        })
                    {
                        bail!("Unsupported settings");
                    }
                    let mut value = config::load(path)?;
                    value["repository"]["poll_interval_seconds"] =
                        edit["poll_interval_seconds"].clone();
                    for key in ["empty_grace_seconds", "health_timeout_seconds"] {
                        value["deployment"][key] = edit[key].clone();
                    }
                    let revision = config::revision(path)?;
                    config::save_operational_checked(path, value, &revision)?;
                    Ok(())
                })();
                respond(
                    request,
                    if result.is_ok() { 200 } else { 400 },
                    if result.is_ok() {
                        "<p>Сохранено.</p><a href='/'>Вернуться</a>"
                    } else {
                        "Invalid settings; check service journal."
                    }
                    .into(),
                    None,
                );
            }
            _ => respond(request, 404, "Not found".into(), None),
        }
    }
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_decode() {
        assert_eq!(
            fields("settings=%7B%22a%22%3A1%7D").unwrap()["settings"],
            "{\"a\":1}"
        );
        assert!(fields("token=%QZ").is_err());
    }
    #[test]
    fn html_and_tokens() {
        assert_eq!(escape("<script>"), "&lt;script&gt;");
        assert!(equal("abc", "abc"));
        assert!(!equal("abc", "abd"));
        assert_ne!(random(), random());
    }
    fn edit_fixture(content: &str) -> (tempfile::TempDir, Value, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("authoring");
        fs::create_dir_all(repo.join("plugins-configs/Foo")).unwrap();
        fs::write(repo.join("plugins-configs/Foo/config.yml"), content).unwrap();
        fs::write(
            repo.join("export-policy.json"),
            json!({"approved_config_paths":["plugins-configs/Foo/config.yml"]}).to_string(),
        )
        .unwrap();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec![
                "config",
                "remote.origin.url",
                "git@github.com:team/config.git",
            ],
            vec![
                "add",
                "plugins-configs/Foo/config.yml",
                "export-policy.json",
            ],
            vec![
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.org",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "fixture",
            ],
        ] {
            let status = std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        }
        let paths: serde_json::Map<_, _> = [
            "repository",
            "server",
            "state",
            "cache",
            "staging",
            "backups",
        ]
        .into_iter()
        .map(|key| (key.into(), json!(root.path().join(key))))
        .collect();
        let config=crate::config::validate(json!({"repository":{"url":"git@github.com:team/config.git","authoring_path":repo},"paths":paths})).unwrap();
        (root, config, repo)
    }
    fn edit_plan(config: &Value, repo: &Path, content: &str) -> (HashMap<String, String>, String) {
        let oldhash = hex::encode(Sha256::digest(
            fs::read(repo.join("plugins-configs/Foo/config.yml")).unwrap(),
        ));
        let form = HashMap::from([
            ("file".into(), "plugins-configs/Foo/config.yml".into()),
            ("oldhash".into(), oldhash),
            ("content".into(), content.into()),
        ]);
        let html = editor_preview(config, "csrf", &form).unwrap();
        let bytes = fs::read(ui_plan_path(config, "ui-edit.json").unwrap()).unwrap();
        let digest = hex::encode(Sha256::digest(bytes));
        (
            HashMap::from([("reviewed".into(), "yes".into()), ("digest".into(), digest)]),
            html,
        )
    }
    #[test]
    fn editor_escapes_preview_saves_only_reviewed_clone_and_guards_stale_hash() {
        let (_root, config, repo) = edit_fixture("message: hello\n");
        let (form, html) = edit_plan(
            &config,
            &repo,
            "message: '</textarea><script>alert(1)</script>'\n",
        );
        assert!(!html.contains("</textarea><script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(editor_save(&config, &HashMap::new()).is_err());
        editor_save(&config, &form).unwrap();
        assert!(
            fs::read_to_string(repo.join("plugins-configs/Foo/config.yml"))
                .unwrap()
                .contains("<script>")
        );
        let (form, _) = edit_plan(&config, &repo, "message: new\n");
        fs::write(
            repo.join("plugins-configs/Foo/config.yml"),
            "message: concurrent\n",
        )
        .unwrap();
        assert!(editor_save(&config, &form).is_err());
        assert!(
            fs::read_to_string(repo.join("plugins-configs/Foo/config.yml"))
                .unwrap()
                .contains("concurrent")
        );
    }
    #[test]
    fn editor_rechecks_policy_authoring_path_and_secret_history() {
        let (_root, config, repo) = edit_fixture("message: hello\n");
        let (form, _) = edit_plan(&config, &repo, "message: new\n");
        fs::write(
            repo.join("export-policy.json"),
            json!({"approved_config_paths":[]}).to_string(),
        )
        .unwrap();
        assert!(editor_save(&config, &form).is_err());
        let (_root, config, repo) = edit_fixture("api-token: historical-secret\n");
        fs::write(
            repo.join("plugins-configs/Foo/config.yml"),
            "message: sanitized\n",
        )
        .unwrap();
        let error = editor_open(&config, "csrf", "plugins-configs/Foo/config.yml")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("historical-secret"));
        assert!(error.contains("historical"));
        let (_root, config, repo) = edit_fixture("message: hello\n");
        let (form, _) = edit_plan(&config, &repo, "message: new\n");
        let mut changed = config.clone();
        changed["repository"]["authoring_path"] = json!(repo.parent().unwrap().join("other"));
        assert!(editor_save(&changed, &form).is_err());
    }
    #[test]
    fn editor_plan_is_bound_to_connection_and_git_head() {
        let (_root, config, repo) = edit_fixture("message: hello\n");
        let (form, _) = edit_plan(&config, &repo, "message: new\n");
        let mut changed = config.clone();
        changed["repository"]["branch"] = json!("release");
        assert!(editor_save(&changed, &form).is_err());
        let result = std::process::Command::new("git")
            .current_dir(&repo)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.org",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "other checkout state",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(result.success());
        assert!(editor_save(&config, &form).is_err());
    }
    #[test]
    fn editor_rejects_traversal_symlinks_unapproved_paths_oversize_and_secrets() {
        let (_root, config, repo) = edit_fixture("message: hello\n");
        for path in [
            "../outside.yml",
            ".github/workflows/ci.yml",
            "requirements/engine.json",
            "plugins-configs/Unknown/config.yml",
        ] {
            assert!(editor_open(&config, "csrf", path).is_err());
        }
        let (_, _, oldhash) = authoring_file(&repo, "plugins-configs/Foo/config.yml").unwrap();
        for content in ["a".repeat(65537), "api-token: proposed-secret".into()] {
            let form = HashMap::from([
                ("file".into(), "plugins-configs/Foo/config.yml".into()),
                ("oldhash".into(), oldhash.clone()),
                ("content".into(), content),
            ]);
            let error = editor_preview(&config, "csrf", &form)
                .unwrap_err()
                .to_string();
            assert!(!error.contains("proposed-secret"));
        }
        fs::remove_file(repo.join("plugins-configs/Foo/config.yml")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", repo.join("plugins-configs/Foo/config.yml"))
            .unwrap();
        assert!(editor_open(&config, "csrf", "plugins-configs/Foo/config.yml").is_err());
        let mut overlap = config.clone();
        overlap["repository"]["authoring_path"] = config["paths"]["server"].clone();
        assert!(crate::config::validate(overlap).is_err());
    }
}
