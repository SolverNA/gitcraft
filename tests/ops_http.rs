use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn request(
    port: u16,
    path: &str,
    fields: Option<&[(String, String)]>,
    cookie: Option<&str>,
) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let body = fields
        .map(|fields| {
            fields
                .iter()
                .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
                .collect::<Vec<_>>()
                .join("&")
        })
        .unwrap_or_default();
    write!(stream,"{} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\n{}Content-Length: {}\r\n\r\n{body}",if fields.is_some(){"POST"}else{"GET"},cookie.map(|v|format!("Cookie: {v}\r\n")).unwrap_or_default(),body.len()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}
fn login(port: u16, token: &str) -> (String, String) {
    let response = request(
        port,
        "/login",
        Some(&[("token".into(), token.into())]),
        None,
    );
    assert!(response.starts_with("HTTP/1.1 200"));
    let cookie = response
        .lines()
        .find_map(|s| s.strip_prefix("Set-Cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let page = request(port, "/operations", None, Some(&cookie));
    assert!(page.starts_with("HTTP/1.1 200"));
    let csrf = page
        .split("name=csrf value='")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap()
        .to_owned();
    (cookie, csrf)
}
fn settings(path: &Path, csrf: &str, modify: impl FnOnce(&mut Value)) -> Vec<(String, String)> {
    let mut value = gitcraft::config::load(path).unwrap();
    modify(&mut value);
    let mut fields = vec![
        ("csrf".into(), csrf.into()),
        ("revision".into(), gitcraft::config::revision(path).unwrap()),
    ];
    for (section, mapping) in value.as_object().unwrap() {
        for (key, value) in mapping.as_object().unwrap() {
            fields.push((
                format!("field.{section}.{key}"),
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            ));
        }
    }
    fields
}
fn digest(page: &str) -> String {
    page.split("name=digest value='")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap()
        .into()
}
#[test]
fn full_settings_cas_bind_rollback_and_write_only_token_rotation() {
    let root = tempfile::tempdir().unwrap();
    let port_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = port_listener.local_addr().unwrap().port();
    drop(port_listener);
    let path = root.path().join("agent.yml");
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
    gitcraft::config::save(&path,json!({"repository":{"url":"git@github.com:fixture/config.git"},"paths":paths,"web":{"port":port}})).unwrap();
    let old = "old-admin-token-at-least-24-characters";
    let new = "new-admin-token-at-least-24-characters";
    let _process = Process(
        Command::new(env!("CARGO_BIN_EXE_gitcraft"))
            .args(["web", "--config", path.to_str().unwrap()])
            .env("GITCRAFT_WEB_TOKEN", old)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let (cookie, csrf) = login(port, old);
    let blocked = TcpListener::bind("127.0.0.1:0").unwrap();
    let blocked_port = blocked.local_addr().unwrap().port();
    let page = request(
        port,
        "/operations-settings-preview",
        Some(&settings(&path, &csrf, |v| {
            v["web"]["port"] = json!(blocked_port)
        })),
        Some(&cookie),
    );
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    let applied = request(
        port,
        "/operations-settings-apply",
        Some(&[
            ("csrf".into(), csrf.clone()),
            ("digest".into(), digest(&page)),
            ("reviewed".into(), "yes".into()),
        ]),
        Some(&cookie),
    );
    assert!(applied.starts_with("HTTP/1.1 400"));
    assert_eq!(gitcraft::config::load(&path).unwrap()["web"]["port"], port);
    let page = request(
        port,
        "/operations-settings-preview",
        Some(&settings(&path, &csrf, |v| {
            v["repository"]["poll_interval_seconds"] = json!(33)
        })),
        Some(&cookie),
    );
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    let applied = request(
        port,
        "/operations-settings-apply",
        Some(&[
            ("csrf".into(), csrf.clone()),
            ("digest".into(), digest(&page)),
            ("reviewed".into(), "yes".into()),
        ]),
        Some(&cookie),
    );
    assert!(applied.starts_with("HTTP/1.1 200"), "{applied}");
    assert_eq!(
        gitcraft::config::load(&path).unwrap()["repository"]["poll_interval_seconds"],
        33
    );
    let response = request(
        port,
        "/credential-save",
        Some(&[
            ("csrf".into(), csrf.clone()),
            ("role".into(), "web".into()),
            ("secret".into(), new.into()),
            ("confirmed".into(), "yes".into()),
        ]),
        Some(&cookie),
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(!response.contains(new));
    assert!(!request(port, "/", None, Some(&cookie)).contains("/operations>"));
    assert!(
        request(port, "/login", Some(&[("token".into(), old.into())]), None)
            .starts_with("HTTP/1.1 403")
    );
    let (cookie, csrf) = login(port, new);
    let page = request(port, "/operations", None, Some(&cookie));
    assert!(!page.contains(new));
    assert!(!page.contains(old));
    let private = fs::read_to_string(root.path().join("local-secrets.json")).unwrap();
    assert!(private.contains(new));
    let next = TcpListener::bind("127.0.0.1:0").unwrap();
    let next_port = next.local_addr().unwrap().port();
    drop(next);
    let page = request(
        port,
        "/operations-settings-preview",
        Some(&settings(&path, &csrf, |v| {
            v["web"]["port"] = json!(next_port)
        })),
        Some(&cookie),
    );
    assert!(page.starts_with("HTTP/1.1 200"));
    assert!(
        request(
            port,
            "/operations-settings-apply",
            Some(&[
                ("csrf".into(), csrf),
                ("digest".into(), digest(&page)),
                ("reviewed".into(), "yes".into())
            ]),
            Some(&cookie)
        )
        .starts_with("HTTP/1.1 200")
    );
    login(next_port, new);
}
