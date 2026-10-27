use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn request(port: u16, path: &str, body: Option<&str>, cookie: Option<&str>) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let data = body.unwrap_or("");
    let method = if body.is_some() { "POST" } else { "GET" };
    let cookie = cookie
        .map(|v| format!("Cookie: {v}\r\n"))
        .unwrap_or_default();
    write!(stream,"{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\n{cookie}Content-Length: {}\r\n\r\n{data}",data.len()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}
#[test]
fn login_csrf_and_secret_redaction() {
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let paths: serde_json::Map<_, _> = [
        "repository",
        "server",
        "state",
        "cache",
        "staging",
        "backups",
    ]
    .into_iter()
    .map(|key| (key.into(), json!(temp.path().join(key))))
    .collect();
    let config = json!({"repository":{"url":"git@github.com:example/config.git"},"paths":paths,"web":{"port":port}});
    let path = temp.path().join("agent.yml");
    fs::write(&path, serde_yaml::to_string(&config).unwrap()).unwrap();
    let token = "integration-secret-token-at-least-24";
    let _server = Server(
        Command::new(env!("CARGO_BIN_EXE_gitcraft"))
            .args(["web", "--config", path.to_str().unwrap()])
            .env("GITCRAFT_WEB_TOKEN", token)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "web did not start");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(request(port, "/deploy", Some("csrf=fake"), None).starts_with("HTTP/1.1 403"));
    assert!(request(port, "/login", Some("token=wrong"), None).starts_with("HTTP/1.1 403"));
    let response = request(port, "/login", Some(&format!("token={token}")), None);
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("HttpOnly; SameSite=Strict"));
    let cookie = response
        .lines()
        .find_map(|line| line.strip_prefix("Set-Cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let page = request(port, "/", None, Some(cookie));
    assert!(page.contains("Локальные настройки"));
    assert!(!page.contains(token));
    assert!(
        request(
            port,
            "/settings",
            Some("settings=%7B%7D&csrf=fake"),
            Some(cookie)
        )
        .starts_with("HTTP/1.1 403")
    );
}
