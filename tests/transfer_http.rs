//! Real loopback HTTP and process restart; all service calls hit a fixture executable.
use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::Path,
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
fn start(config: &Path, port: u16, bin: &Path) -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_gitcraft"))
        .args(["web", "--config", config.to_str().unwrap()])
        .env("GITCRAFT_WEB_TOKEN", "http-test-admin-token-long-enough")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let server = Server(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "web did not start");
        thread::sleep(Duration::from_millis(20));
    }
    server
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn manifest(path: &str, kind: &str, bytes: &[u8]) -> Value {
    json!({"files":[{"path":path,"kind":kind,"size":bytes.len(),"sha256":sha(bytes)}],"source_consistency":"stopped"})
}
#[test]
fn browser_auth_restart_resume_integrity_and_stopped_commit() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let service_state = temp.path().join("service-state");
    fs::write(&service_state, "inactive").unwrap();
    let script = format!(
        "#!/bin/sh\nif [ \"$3\" != is-active ]; then exit 99; fi\nstate=$(cat '{}')\nprintf '%s\\n' \"$state\"\nif [ \"$state\" = active ]; then exit 0; else exit 3; fi\n",
        service_state.display()
    );
    fs::write(bin.join("sudo"), script).unwrap();
    fs::set_permissions(bin.join("sudo"), fs::Permissions::from_mode(0o755)).unwrap();
    let paths: serde_json::Map<_, _> = [
        "repository",
        "server",
        "state",
        "cache",
        "staging",
        "backups",
    ]
    .into_iter()
    .map(|key| {
        let path = temp.path().join(key);
        fs::create_dir(&path).unwrap();
        (key.into(), json!(path))
    })
    .collect();
    let config = json!({"repository":{"url":"git@github.com:example/config.git"},"paths":paths,"web":{"port":port},"transfer":{"min_free_bytes":1,"max_session_bytes":134217728,"max_staging_bytes":268435456}});
    let config_path = temp.path().join("agent.yml");
    fs::write(&config_path, serde_yaml::to_string(&config).unwrap()).unwrap();
    let server = start(&config_path, port, &bin);
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let body = manifest("world/region/r.0.0.mca", "world", b"test");
    assert!(
        !client
            .post(format!("{base}/api/transfers"))
            .json(&body)
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    let login = client
        .post(format!("{base}/login"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("token=http-test-admin-token-long-enough")
        .send()
        .unwrap();
    assert!(login.status().is_success());
    let cookie = login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let page = client
        .get(format!("{base}/transfer"))
        .header("Cookie", &cookie)
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(page.contains("webkitdirectory"));
    assert!(page.contains("64 MiB"));
    assert!(!page.contains("http-test-admin-token-long-enough"));
    let csrf = page
        .split("id=csrf type=hidden value='")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    let mut oversized = body.clone();
    oversized["files"][0]["size"] = json!(64 * 1024 * 1024 + 1);
    let response = client
        .post(format!("{base}/api/transfers"))
        .header("Cookie", &cookie)
        .header("X-CSRF-Token", csrf)
        .json(&oversized)
        .send()
        .unwrap();
    assert!(!response.status().is_success());
    assert!(response.text().unwrap().contains("64 MiB"));
    let mut overquota = body.clone();
    overquota["files"][0]["size"] = json!(134217729u64);
    assert!(
        !client
            .post(format!("{base}/api/transfers"))
            .bearer_auth("http-test-admin-token-long-enough")
            .json(&overquota)
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    let mut bytes = vec![b'x'; gitcraft::transfer::CHUNK_BYTES];
    bytes.extend_from_slice(b"remainder");
    let manifest = manifest("world/region/r.0.0.mca", "world", &bytes);
    let session: Value = client
        .post(format!("{base}/api/transfers"))
        .header("Cookie", &cookie)
        .header("X-CSRF-Token", csrf)
        .json(&manifest)
        .send()
        .unwrap()
        .json()
        .unwrap();
    let id = session["id"].as_str().unwrap();
    let token = session["token"].as_str().unwrap();
    let route = format!("{base}/api/transfers/{id}");
    assert!(
        !client
            .get(&route)
            .bearer_auth("wrong")
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    let first = &bytes[..gitcraft::transfer::CHUNK_BYTES];
    assert!(
        client
            .put(format!("{route}/files/0/0"))
            .bearer_auth(token)
            .header("X-Chunk-SHA256", sha(first))
            .body(first.to_vec())
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    drop(server);
    let _server = start(&config_path, port, &bin);
    let status: Value = client
        .get(&route)
        .bearer_auth(token)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(status["offsets"][0], gitcraft::transfer::CHUNK_BYTES);
    assert!(status.get("token").is_none());
    let conflicting = vec![b'z'; first.len()];
    assert!(
        !client
            .put(format!("{route}/files/0/0"))
            .bearer_auth(token)
            .header("X-Chunk-SHA256", sha(&conflicting))
            .body(conflicting)
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    let tail = &bytes[gitcraft::transfer::CHUNK_BYTES..];
    assert!(
        client
            .put(format!(
                "{route}/files/0/{}",
                gitcraft::transfer::CHUNK_BYTES
            ))
            .bearer_auth(token)
            .header("X-Chunk-SHA256", sha(tail))
            .body(tail.to_vec())
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    assert!(
        client
            .post(format!("{route}/verify"))
            .bearer_auth(token)
            .json(&json!({}))
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    // Verify refuses post-upload corruption, then succeeds after the original data is restored.
    let data = temp.path().join(format!("state/transfers/{id}/data/0"));
    fs::write(&data, b"corrupt").unwrap();
    assert!(
        !client
            .post(format!("{route}/verify"))
            .bearer_auth(token)
            .json(&json!({}))
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    fs::write(&data, &bytes).unwrap();
    fs::create_dir_all(temp.path().join("server/world/region")).unwrap();
    fs::write(
        temp.path().join("server/world/region/r.0.0.mca"),
        b"old-world",
    )
    .unwrap();
    assert!(
        !client
            .post(format!("{route}/commit"))
            .bearer_auth(token)
            .json(&json!({"confirm_world_replace":false}))
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    fs::write(&service_state, "active").unwrap();
    assert!(
        !client
            .post(format!("{route}/commit"))
            .bearer_auth(token)
            .json(&json!({"confirm_world_replace":true}))
            .send()
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        fs::read(temp.path().join("server/world/region/r.0.0.mca")).unwrap(),
        b"old-world"
    );
    fs::write(&service_state, "inactive").unwrap();
    let committed: Value = client
        .post(format!("{route}/commit"))
        .bearer_auth(token)
        .json(&json!({"confirm_world_replace":true}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(committed["status"], "committed", "{committed}");
    assert_eq!(
        fs::read(temp.path().join("server/world/region/r.0.0.mca")).unwrap(),
        bytes
    );
    assert_eq!(
        fs::read(temp.path().join(format!(
            "backups/world-transfer-{id}/world/region/r.0.0.mca"
        )))
        .unwrap(),
        b"old-world"
    );
    let journal = fs::read_to_string(
        temp.path()
            .join(format!("state/transfers/{id}/session.json")),
    )
    .unwrap();
    assert!(!journal.contains(token));
}
