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
    let mut result = json!({"service":value["deployment"]["service"]});
    if let Some(state) = value["paths"]["state"].as_str() {
        for name in ["status", "deployment"] {
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
        if route == "/api/transfers" && method == "POST" {
            let csrf = header(&request, "X-CSRF-Token");
            anyhow::ensure!(
                equal(admin, &bearer)
                    || session_csrf
                        .zip(csrf.as_deref())
                        .is_some_and(|(a, b)| equal(a, b)),
                "authentication required"
            );
            let manifest = serde_json::from_value(read_json(&mut request, 16 * 1024 * 1024)?)?;
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
            transfer::status(&config, id, &bearer)?;
            anyhow::ensure!(
                request
                    .body_length()
                    .is_some_and(|n| n <= transfer::CHUNK_BYTES),
                "chunk exceeds byte limit"
            );
            let mut bytes = Vec::new();
            request
                .as_reader()
                .take(transfer::CHUNK_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            return transfer::chunk(&config, id, &bearer, index, offset, &digest, &bytes);
        }
        if method == "POST" && pieces.len() == 4 {
            let body = read_json(&mut request, 1024)?;
            return match pieces[3] {
                "verify" => transfer::verify(&config, id, &bearer),
                "commit" => {
                    transfer::commit(&config, id, &bearer, body["confirm_world_replace"] == true)
                }
                "recover" => transfer::recover(&config, id, &bearer),
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
        "<h2>Прямой перенос local → VPS</h2><p>Данные идут прямо на этот сервер через SSH-туннель. Миры и JAR не попадают в Git или стороннее хранилище.</p><p>Браузер: максимум 64 MiB на файл, размер проверяется до чтения. Для больших файлов: <code>gitcraft transfer plan --source PATH --kind world --source-consistency stopped --output plan.json</code>, затем <code>gitcraft transfer send --endpoint http://127.0.0.1:8765 --plan plan.json --session session.json</code>. Для JAR укажите kind engine/plugin/mod.</p><input id=csrf type=hidden value='{csrf}'><label>Тип <select id=kind><option value=world>Мир (папка)</option><option value=engine>Ядро</option><option value=plugin>Плагины</option><option value=mod>Моды</option></select></label><p><input id=files type=file multiple><input id=folder type=file webkitdirectory multiple></p><label><input id=consistent type=checkbox>Локальный Minecraft остановлен или выбрана заранее сделанная согласованная snapshot-копия</label><p><select id=consistency><option value=stopped>Источник остановлен</option><option value=snapshot>Готовый snapshot</option></select></p><button id=send>Передать / продолжить</button><button id=verify>Проверить целые файлы</button><p><label><input id=confirm type=checkbox>Подтверждаю замену выбранных миров на VPS с резервной копией. Minecraft на VPS остановлен.</label></p><button id=commit>Применить проверенный перенос</button><button id=status>Статус</button><button id=recover>Восстановить прерванную замену (сервер остановлен)</button><progress id=progress max=100 value=0></progress><pre id=output>Выберите файлы/папку. При возобновлении выберите тот же источник.</pre><p>Session token хранится только в localStorage этого браузера; не передавайте его другим. После рестарта сервер сохранит смещения. Окончательное применение никогда не запускает Minecraft автоматически.</p><a href='/'>Панель</a><script src='/assets/transfer.js'></script>"
    )
}

pub fn serve(path: &Path) -> Result<()> {
    let value = config::load(path)?;
    let env = value["web"]["token_env"]
        .as_str()
        .context("Missing token reference")?;
    let token = std::env::var(env).context("Web token environment variable missing")?;
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
    let server = Server::http(&address).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut sessions: HashMap<String, (String, Instant)> = HashMap::new();
    let busy = Arc::new(AtomicBool::new(false));
    let settings_lock = Arc::new(Mutex::new(()));
    for mut request in server.incoming_requests() {
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
        if url.starts_with("/api/transfers") {
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
        if method == "GET" && url == "/" {
            if let Some(csrf) = csrf {
                let current = config::load(path)?;
                let editable = json!({"poll_interval_seconds":current["repository"]["poll_interval_seconds"],"empty_grace_seconds":current["deployment"]["empty_grace_seconds"],"health_timeout_seconds":current["deployment"]["health_timeout_seconds"]});
                let hidden = format!("<input type=hidden name=csrf value='{csrf}'>");
                respond(
                    request,
                    200,
                    format!(
                        "<p><a href=/transfer>Перенести миры и JAR напрямую</a> · <a href=/onboarding>Репозиторий / экспорт / adopt</a></p><h2>Состояние</h2><pre>{}</pre><form method=post action=/deploy>{hidden}<button>Проверить и применить, когда сервер пуст</button></form><h2>Локальные настройки</h2><p>Можно менять вручную в agent.yml. Конфиги Minecraft меняются через PR.</p><form method=post action=/settings>{hidden}<textarea name=settings>{}</textarea><button>Сохранить</button></form><form method=post action=/logout>{hidden}<button>Выйти</button></form>",
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
        if request.body_length().is_none_or(|n| n > 16384) {
            respond(request, 413, "Invalid body length".into(), None);
            continue;
        }
        let mut body = String::new();
        if request
            .as_reader()
            .take(16385)
            .read_to_string(&mut body)
            .is_err()
            || body.len() > 16384
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
        match url.as_str() {
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
                        let _ = deploy::run(&value);
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
                    config::save(path, value)?;
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
    Ok(())
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
}
