//! Socket RCON and a process-local sudo fixture; never invokes the host service manager.
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::Duration,
};
fn packet(stream: &mut TcpStream) -> Option<(i32, String)> {
    let mut n = [0; 4];
    stream.read_exact(&mut n).ok()?;
    let n = i32::from_le_bytes(n);
    if !(10..=65536).contains(&n) {
        return None;
    }
    let mut data = vec![0; n as usize];
    stream.read_exact(&mut data).ok()?;
    Some((
        i32::from_le_bytes(data[..4].try_into().ok()?),
        String::from_utf8(data[8..data.len() - 2].to_vec()).ok()?,
    ))
}
fn reply(stream: &mut TcpStream, id: i32, kind: i32, text: &str) {
    stream
        .write_all(&(text.len() as i32 + 10).to_le_bytes())
        .unwrap();
    stream.write_all(&id.to_le_bytes()).unwrap();
    stream.write_all(&kind.to_le_bytes()).unwrap();
    stream.write_all(text.as_bytes()).unwrap();
    stream.write_all(&[0, 0]).unwrap();
}
#[test]
fn maintenance_stop_refuses_players_and_saves_before_explicit_empty_stop() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let online = Arc::new(AtomicU32::new(2));
    let stopped = Arc::new(AtomicBool::new(false));
    let peers = online.clone();
    let done = stopped.clone();
    let server = thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    if let Some((id, password)) = packet(&mut stream) {
                        if password != "fixture-rcon-password" {
                            reply(&mut stream, -1, 2, "");
                            continue;
                        }
                        reply(&mut stream, id, 2, "");
                        if let Some((id, command)) = packet(&mut stream) {
                            let text = if command == "minecraft:list" {
                                format!(
                                    "There are {} of a max of 20 players online:",
                                    peers.load(Ordering::SeqCst)
                                )
                            } else {
                                assert_eq!(command, "save-all flush");
                                "Saved".into()
                            };
                            reply(&mut stream, id, 0, &text);
                        }
                    }
                }
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
    });
    let tools = root.path().join("tools");
    fs::create_dir(&tools).unwrap();
    let sudo = tools.join("sudo");
    fs::write(&sudo,"#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_SERVICE_LOG\"\ncase \"$3\" in stop) exit 0;; is-active) printf 'inactive\\n'; exit 3;; *) exit 1;; esac\n").unwrap();
    fs::set_permissions(&sudo, fs::Permissions::from_mode(0o755)).unwrap();
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
    let path = root.path().join("agent.yml");
    gitcraft::config::save(&path,json!({"repository":{"url":"git@github.com:fixture/config.git"},"paths":paths,"rcon":{"port":port,"password_env":"TEST_RCON_PASSWORD"}})).unwrap();
    let log = root.path().join("service.log");
    let command = || {
        Command::new(env!("CARGO_BIN_EXE_gitcraft"))
            .args([
                "maintenance-stop",
                "--config",
                path.to_str().unwrap(),
                "--confirmed",
            ])
            .env(
                "PATH",
                format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
            )
            .env("TEST_SERVICE_LOG", &log)
            .env("TEST_RCON_PASSWORD", "fixture-rcon-password")
            .output()
            .unwrap()
    };
    let rejected = command();
    assert!(!rejected.status.success());
    assert!(!log.exists());
    online.store(0, Ordering::SeqCst);
    let successful = command();
    assert!(
        successful.status.success(),
        "{}",
        String::from_utf8_lossy(&successful.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&successful.stdout).unwrap();
    assert_eq!(value["status"], "completed");
    let actions = fs::read_to_string(log).unwrap();
    assert!(actions.contains("stop minecraft.service"));
    assert!(actions.contains("is-active minecraft.service"));
    stopped.store(true, Ordering::Relaxed);
    server.join().unwrap();
}
