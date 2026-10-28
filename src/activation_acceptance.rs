//! Actual HTTP initialization and shared production polling; only external services are fixtures.
use crate::{agent_service, artifacts, config, deploy, rcon::RconClient};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::os::unix::process::ExitStatusExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
struct Runtime {
    remote: PathBuf,
    online: u32,
    active: bool,
    actions: Vec<String>,
    snapshots: u64,
}
impl deploy::Runtime for Runtime {
    fn launcher(&mut self, config: &Value) -> Result<Value> {
        Ok(
            json!({"Id":config["deployment"]["service"],"WorkingDirectory":config["paths"]["server"],"ExecStart":"{ path=/usr/bin/java ; argv[]=/usr/bin/java -jar server.jar nogui ; }","LaunchPath":"/usr/bin/java","LaunchArgv":["/usr/bin/java","-Xms1G","-Xmx2G","-jar","server.jar","nogui"],"LaunchFlags":[]}),
        )
    }

    fn online(&mut self) -> Result<u32> {
        Ok(self.online)
    }
    fn command(&mut self, _: &str) -> Result<String> {
        Ok("saved".into())
    }
    fn service(&mut self, action: &str, _: &str) -> Result<Output> {
        self.actions.push(action.into());
        match action {
            "start" => self.active = true,
            "stop" => self.active = false,
            _ => (),
        };
        Ok(Output {
            status: std::process::ExitStatus::from_raw(if self.active { 0 } else { 3 << 8 }),
            stdout: if self.active {
                b"active\n".to_vec()
            } else {
                b"inactive\n".to_vec()
            },
            stderr: vec![],
        })
    }
    fn snapshot(
        &mut self,
        _: &str,
        branch: &str,
        _: &Path,
        staging: &Path,
    ) -> Result<(String, PathBuf)> {
        self.snapshots += 1;
        fs::create_dir_all(staging)?;
        let target = staging.join(format!("snapshot-{}", self.snapshots));
        let output = Command::new("git")
            .args(["clone", "--quiet", "--branch", branch])
            .arg(&self.remote)
            .arg(&target)
            .output()?;
        ensure!(output.status.success(), "Fixture clone failed");
        let commit = git(&target, &["rev-parse", "HEAD"]);
        Ok((commit, target))
    }
    fn healthy(&mut self, _: &str, _: u64) -> Result<()> {
        ensure!(self.active, "Fixture service stopped");
        Ok(())
    }
}
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

fn client() -> RconClient {
    RconClient {
        host: "127.0.0.1".into(),
        port: 25575,
        password: "fixture-private".into(),
        timeout: Duration::from_secs(1),
    }
}
fn transaction(config: &Value, bootstrap: bool, runtime: &Arc<Mutex<Runtime>>) -> Result<Value> {
    deploy::execute_with(
        config,
        |config| {
            deploy::run_transaction(config, bootstrap, &client(), &mut *runtime.lock().unwrap())
        },
        |_| Ok(()),
    )
}
struct Poller {
    config_path: PathBuf,
    runtime: Arc<Mutex<Runtime>>,
    active: AtomicBool,
    fail: AtomicBool,
    stopped: Arc<AtomicBool>,
    attempts: AtomicUsize,
    reconciles: Arc<AtomicUsize>,
    worker: Mutex<Option<JoinHandle<Result<()>>>>,
}
impl Drop for Poller {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.get_mut().unwrap().take() {
            worker.join().unwrap().unwrap();
        }
    }
}
struct Activation(Arc<Poller>);
impl agent_service::Runtime for Activation {
    fn inspect(&mut self) -> Result<Value> {
        let active = self.0.active.load(Ordering::SeqCst);
        Ok(
            json!({"Id":agent_service::UNIT,"FragmentPath":agent_service::UNIT_FILE,"User":"minecraft","WorkingDirectory":"/var/lib/minecraft","LaunchPath":agent_service::BINARY,"LaunchArgv":[agent_service::BINARY,"agent","--config",agent_service::CONFIG],"LaunchFlags":[],"EnvironmentFiles":[[agent_service::ENV_FILE,false]],"DropInPaths":[],"ActiveState":if active {"active"} else {"inactive"},"UnitFileState":if active {"enabled"} else {"disabled"}}),
        )
    }
    fn installed_config(&mut self) -> Result<Value> {
        config::load(&self.0.config_path)
    }
    fn trusted_files(&mut self, _: &Value) -> Result<()> {
        Ok(())
    }
    fn minecraft_user(&mut self, _: &Value) -> Result<String> {
        Ok("minecraft".into())
    }
    fn enable_now(&mut self) -> Result<()> {
        self.0.attempts.fetch_add(1, Ordering::SeqCst);
        let config = config::load(&self.0.config_path)?;
        // Actual lock acquisition proves the initialization transaction released it before activation.
        drop(deploy::lock_state(&config)?);
        ensure!(
            Path::new(config["paths"]["state"].as_str().unwrap())
                .join("deployment.json")
                .is_file(),
            "Activation preceded durable initialization"
        );
        ensure!(
            !self.0.fail.load(Ordering::SeqCst),
            "Fixture activation denied"
        );
        let path = self.0.config_path.clone();
        let stopped = self.0.stopped.clone();
        let runtime = self.0.runtime.clone();
        let reconciles = self.0.reconciles.clone();
        let mut worker = self.0.worker.lock().unwrap();
        ensure!(worker.is_none(), "Duplicate poller spawn");
        *worker = Some(thread::spawn(move || {
            crate::agent_loop::run_with(
                &path,
                false,
                |config| {
                    reconciles.fetch_add(1, Ordering::SeqCst);
                    transaction(config, false, &runtime)
                },
                || stopped.load(Ordering::SeqCst),
            )
        }));
        self.0.active.store(true, Ordering::SeqCst);
        Ok(())
    }
}
struct Adoption {
    runtime: Arc<Mutex<Runtime>>,
    unit: Value,
}
impl crate::adopt::AdoptionRuntime for Adoption {
    fn snapshot(&mut self, config: &Value) -> Result<(String, PathBuf)> {
        use deploy::Runtime as _;
        self.runtime.lock().unwrap().snapshot(
            config["repository"]["url"].as_str().unwrap(),
            config["repository"]["branch"].as_str().unwrap(),
            Path::new(config["paths"]["repository"].as_str().unwrap()),
            Path::new(config["paths"]["staging"].as_str().unwrap()),
        )
    }
    fn unit(&mut self, _: &Value) -> Result<Value> {
        Ok(self.unit.clone())
    }
    fn online(&mut self, _: &Value) -> Result<u32> {
        Ok(0)
    }
}
struct HttpFixture {
    endpoint: String,
    client: reqwest::blocking::Client,
    cookie: String,
    csrf: String,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
}
impl HttpFixture {
    fn start(path: &Path, runner: Arc<crate::web::OperationRunner>) -> Self {
        let config = config::load(path).unwrap();
        let endpoint = format!("http://127.0.0.1:{}", config["web"]["port"]);
        let stopped = Arc::new(AtomicBool::new(false));
        let worker = {
            let path = path.to_owned();
            let stopped = stopped.clone();
            thread::spawn(move || crate::web::serve_fixture(&path, runner, stopped))
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        wait(|| client.get(&endpoint).send().is_ok(), "HTTP listener");
        let response = client
            .post(format!("{endpoint}/login"))
            .form(&[("token", "acceptance-private-web-token-12345")])
            .send()
            .unwrap();
        assert!(response.status().is_success());
        let cookie = response.headers()[reqwest::header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let page = client
            .get(format!("{endpoint}/onboarding"))
            .header("Cookie", &cookie)
            .send()
            .unwrap()
            .text()
            .unwrap();
        let csrf = page
            .split("name=csrf value='")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap()
            .to_owned();
        Self {
            endpoint,
            client,
            cookie,
            csrf,
            stopped,
            worker: Some(worker),
        }
    }
    fn post(&self, route: &str, fields: &[(&str, &str)]) -> (u16, String) {
        let mut fields = fields.to_vec();
        fields.push(("csrf", &self.csrf));
        let response = self
            .client
            .post(format!("{}{route}", self.endpoint))
            .header("Cookie", &self.cookie)
            .form(&fields)
            .send()
            .unwrap();
        (response.status().as_u16(), response.text().unwrap())
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.client.get(&self.endpoint).send();
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap().unwrap();
        }
    }
}
fn wait(mut condition: impl FnMut() -> bool, description: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < deadline, "Timed out: {description}");
        thread::sleep(Duration::from_millis(25));
    }
}
fn read_json(path: &Path) -> Value {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

fn scenario(adopt: bool, activation_fails: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("authoring");
    fs::create_dir_all(repo.join("requirements")).unwrap();
    fs::create_dir_all(repo.join("server-settings")).unwrap();
    let jar = root.path().join("fixture.jar");
    fs::write(&jar, b"synthetic fixture only").unwrap();
    let sha = artifacts::sha256(&jar).unwrap();
    fs::write(repo.join("requirements/engine.json"),serde_json::to_vec(&json!({"name":"fixture","filename":"server.jar","pinned":{"transfer":true,"sha256":sha,"version":"1"},"source":{"provider":"transfer"}})).unwrap()).unwrap();
    fs::write(
        repo.join("server-settings/server.properties"),
        "motd=first\n",
    )
    .unwrap();
    git(&repo, &["init", "-b", "release"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(
        &repo,
        &[
            "add",
            "requirements/engine.json",
            "server-settings/server.properties",
        ],
    );
    git(&repo, &["commit", "-m", "Initial reviewed state"]);
    let initial = git(&repo, &["rev-parse", "HEAD"]);
    let remote = root.path().join("remote.git");
    assert!(
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote)
            .output()
            .unwrap()
            .status
            .success()
    );
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "origin", "release"]);
    let paths: serde_json::Map<_, _> = [
        "repository",
        "server",
        "state",
        "cache",
        "staging",
        "backups",
    ]
    .into_iter()
    .map(|key| (key.into(), json!(root.path().join("vps").join(key))))
    .collect();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let path = root.path().join("agent.yml");
    fs::write(&path,serde_json::to_vec(&json!({"repository":{"url":"git@github.com:fixture/configs.git","branch":"release","poll_interval_seconds":1},"paths":paths,"deployment":{"empty_grace_seconds":0},"web":{"port":port}})).unwrap()).unwrap();
    let config = config::load(&path).unwrap();
    crate::secrets::write(&config, "web", "acceptance-private-web-token-12345").unwrap();
    crate::secrets::write(&config, "rcon", "fixture-private").unwrap();
    let server = Path::new(config["paths"]["server"].as_str().unwrap());
    fs::create_dir_all(server).unwrap();
    artifacts::import_local(
        &jar,
        Path::new(config["paths"]["cache"].as_str().unwrap()),
        &sha,
    )
    .unwrap();
    let fragment = root.path().join("minecraft.service");
    fs::write(
        &fragment,
        "[Service]\nUser=minecraft\nExecStart=/usr/bin/java -jar server.jar nogui\n",
    )
    .unwrap();
    let unit = json!({"Id":"minecraft.service","User":"minecraft","Group":"minecraft","WorkingDirectory":server,"ExecStart":"{ path=/usr/bin/java ; argv[]=/usr/bin/java -jar server.jar nogui ; }","LaunchPath":"/usr/bin/java","LaunchArgv":["/usr/bin/java","-jar","server.jar","nogui"],"LaunchFlags":[],"FragmentPath":fragment,"MainPID":"123","ActiveState":"active"});
    if adopt {
        fs::copy(&jar, server.join("server.jar")).unwrap();
        fs::write(
            server.join("server.properties"),
            "motd=first\nenable-rcon=true\nrcon.password=fixture-private\n",
        )
        .unwrap();
    }
    let runtime = Arc::new(Mutex::new(Runtime {
        remote,
        online: 0,
        active: adopt,
        actions: vec![],
        snapshots: 0,
    }));
    let poller = Arc::new(Poller {
        config_path: path.clone(),
        runtime: runtime.clone(),
        active: AtomicBool::new(false),
        fail: AtomicBool::new(activation_fails),
        stopped: Arc::new(AtomicBool::new(false)),
        attempts: AtomicUsize::new(0),
        reconciles: Arc::new(AtomicUsize::new(0)),
        worker: Mutex::new(None),
    });
    let enabled = Arc::new(AtomicBool::new(adopt));
    let runner: Arc<crate::web::OperationRunner> = {
        let enabled = enabled.clone();
        let poller = poller.clone();
        let unit = unit.clone();
        Arc::new(move |config, name, form: &HashMap<String, String>| {
            let mut activation = Activation(poller.clone());
            match name {
                "minecraft-autostart" | "stop" | "resume" => boot_operation(
                    config,
                    name,
                    form,
                    &mut BootRuntime {
                        runtime: poller.runtime.clone(),
                        enabled: enabled.clone(),
                        unit: unit.clone(),
                    },
                ),
                "bootstrap" => {
                    ensure!(
                        form.get("accept_eula").is_some_and(|v| v == "yes"),
                        "Explicit EULA required"
                    );
                    agent_service::initialize_with(config, &mut activation, || {
                        transaction(config, true, &poller.runtime)
                    })
                }
                "agent-activate" => agent_service::activate_with(config, &mut activation),
                "adopt-plan" => crate::adopt::build_plan(config, Path::new(&form["server"]), &unit),
                "adopt" => crate::adopt::apply_with(
                    config,
                    &serde_json::from_str(&form["plan"])?,
                    Path::new(&form["repository"]),
                    &form["expected_commit"],
                    &mut Adoption {
                        runtime: poller.runtime.clone(),
                        unit: unit.clone(),
                    },
                    &mut activation,
                ),
                _ => anyhow::bail!("Unexpected fixture operation"),
            }
        })
    };
    let http = HttpFixture::start(&path, runner);
    if !adopt {
        assert_eq!(
            http.post("/operation/bootstrap", &[("confirmed", "yes")]).0,
            400
        );
        assert_eq!(poller.attempts.load(Ordering::SeqCst), 0);
        assert!(
            !Path::new(config["paths"]["state"].as_str().unwrap())
                .join("deployment.json")
                .exists()
        );
    }
    if adopt {
        let (code, html) = http.post("/adopt-plan", &[("server", server.to_str().unwrap())]);
        assert_eq!(code, 200, "{html}");
        let digest = html
            .split("name=digest value='")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap();
        let (code, html) = http.post(
            "/adopt-apply",
            &[
                ("digest", digest),
                ("reviewed", "yes"),
                ("repository", repo.to_str().unwrap()),
                ("expected_commit", &initial),
            ],
        );
        assert_eq!(code, 200, "{html}");
    } else {
        let (code, _) = http.post(
            "/operation/bootstrap",
            &[("confirmed", "yes"), ("accept_eula", "yes")],
        );
        assert_eq!(code, 202);
    }
    let state = Path::new(config["paths"]["state"].as_str().unwrap());
    wait(
        || {
            ["completed", "activation_required", "setup_required"].contains(
                &read_json(&state.join("ui-operation.json"))["status"]
                    .as_str()
                    .unwrap_or(""),
            )
        },
        "initialization HTTP outcome",
    );
    let outcome = read_json(&state.join("ui-operation.json"));
    assert_eq!(
        enabled.load(Ordering::SeqCst),
        adopt,
        "Initialization must preserve existing boot policy"
    );
    assert_eq!(read_json(&state.join("deployment.json"))["commit"], initial);
    assert_eq!(
        outcome["status"],
        if activation_fails {
            "activation_required"
        } else {
            "setup_required"
        },
        "{outcome}"
    );
    assert_eq!(outcome["result"]["initialized"], true);
    // Only a merge into the reviewed branch is visible to the actual production scheduler.
    git(&repo, &["checkout", "-b", "petya/config"]);
    fs::write(
        repo.join("server-settings/server.properties"),
        "motd=approved-second\n",
    )
    .unwrap();
    git(&repo, &["add", "server-settings/server.properties"]);
    git(&repo, &["commit", "-m", "Proposed change"]);
    git(&repo, &["checkout", "release"]);
    git(
        &repo,
        &["merge", "--no-ff", "petya/config", "-m", "Approved merge"],
    );
    git(&repo, &["push", "origin", "release"]);
    let next = git(&repo, &["rev-parse", "HEAD"]);
    if activation_fails {
        thread::sleep(Duration::from_millis(1200));
        assert_eq!(poller.reconciles.load(Ordering::SeqCst), 0);
        assert_eq!(read_json(&state.join("deployment.json"))["commit"], initial);
        poller.fail.store(false, Ordering::SeqCst);
        assert_eq!(
            http.post("/operation/agent-activate", &[("confirmed", "yes")])
                .0,
            202
        );
    }
    wait(
        || read_json(&state.join("deployment.json"))["commit"] == next,
        "automatic polling of next approved merge",
    );
    assert!(
        fs::read_to_string(server.join("server.properties"))
            .unwrap()
            .contains("motd=approved-second")
    );
    if activation_fails {
        wait(
            || read_json(&state.join("ui-operation.json"))["status"] == "completed",
            "activation completion",
        );
    }
    let attempts = poller.attempts.load(Ordering::SeqCst);
    assert_eq!(
        http.post("/operation/agent-activate", &[("confirmed", "yes")])
            .0,
        202
    );
    wait(
        || read_json(&state.join("ui-operation.json"))["status"] == "completed",
        "idempotent retry",
    );
    assert_eq!(poller.attempts.load(Ordering::SeqCst), attempts);
    // Changed configuration is loaded on the next actual scheduler reconciliation.
    let mut changed = config::load(&path).unwrap();
    changed["repository"]["poll_interval_seconds"] = json!(2);
    fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    let count = poller.reconciles.load(Ordering::SeqCst);
    wait(
        || poller.reconciles.load(Ordering::SeqCst) > count,
        "scheduler config reload",
    );
    if !adopt && !activation_fails {
        boot_policy_and_maintenance_acceptance(
            &http,
            &config,
            &repo,
            &runtime,
            &enabled,
            &poller.reconciles,
        );
    }
    drop(http);
    drop(poller);
}
#[test]
fn http_bootstrap_and_adoption_activate_shared_poller_and_apply_next_merge() {
    for adopt in [false, true] {
        scenario(adopt, false);
    }
}
#[test]
fn http_activation_failure_preserves_initialization_and_retry_starts_polling() {
    for adopt in [false, true] {
        scenario(adopt, true);
    }
}

#[test]
fn shared_scheduler_reloads_configured_interval_without_restart() {
    let root = tempfile::tempdir().unwrap();
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
    let mut config = config::validate(json!({"repository":{"url":"git@github.com:fixture/configs.git","poll_interval_seconds":1},"paths":paths})).unwrap();
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let stop = AtomicBool::new(false);
    let mut observed = Vec::new();
    crate::agent_loop::run_with(
        &path,
        false,
        |current| {
            observed.push((
                Instant::now(),
                current["repository"]["poll_interval_seconds"]
                    .as_u64()
                    .unwrap(),
            ));
            if observed.len() == 1 {
                config["repository"]["poll_interval_seconds"] = json!(2);
                fs::write(&path, serde_json::to_vec(&config)?)?;
            }
            if observed.len() == 3 {
                stop.store(true, Ordering::SeqCst);
            }
            Ok(json!({"status":"scheduler-fixture"}))
        },
        || stop.load(Ordering::SeqCst),
    )
    .unwrap();
    assert_eq!(
        observed
            .iter()
            .map(|(_, interval)| *interval)
            .collect::<Vec<_>>(),
        [1, 2, 2]
    );
    assert!(observed[1].0.duration_since(observed[0].0) >= Duration::from_secs(1));
    assert!(observed[2].0.duration_since(observed[1].0) >= Duration::from_secs(2));
}

#[derive(Clone)]
struct BootRuntime {
    runtime: Arc<Mutex<Runtime>>,
    enabled: Arc<AtomicBool>,
    unit: Value,
}
impl crate::minecraft_autostart::Runtime for BootRuntime {
    fn inspect(&mut self, _: &Value) -> Result<Value> {
        let mut unit = self.unit.clone();
        unit["ActiveState"] = json!(if self.runtime.lock().unwrap().active {
            "active"
        } else {
            "inactive"
        });
        unit["UnitFileState"] = json!(if self.enabled.load(Ordering::SeqCst) {
            "enabled"
        } else {
            "disabled"
        });
        Ok(unit)
    }
    fn trusted_installation(&mut self, _: &Value, _: &Value) -> Result<()> {
        Ok(())
    }
    fn change(&mut self, _: &Value, policy: &str) -> Result<()> {
        ensure!(
            ["enabled", "disabled"].contains(&policy),
            "Invalid boot policy"
        );
        self.enabled.store(policy == "enabled", Ordering::SeqCst);
        Ok(())
    }
}
fn boot_operation(
    config: &Value,
    name: &str,
    form: &HashMap<String, String>,
    boot: &mut BootRuntime,
) -> Result<Value> {
    use deploy::Runtime as _;
    match name {
        "minecraft-autostart" => crate::minecraft_autostart::set_with(
            config,
            &form["policy"],
            form.get("confirmed").is_some_and(|v| v == "yes"),
            boot,
        ),
        "stop" => {
            let runtime = boot.runtime.clone();
            let stopruntime = runtime.clone();
            crate::minecraft_autostart::maintenance_with(
                config,
                boot,
                move || {
                    let mut runtime = runtime.lock().unwrap();
                    ensure!(runtime.online()? == 0, "Players online");
                    runtime.command("save-all flush")?;
                    Ok(())
                },
                move || {
                    stopruntime
                        .lock()
                        .unwrap()
                        .service("stop", "minecraft.service")?;
                    Ok(())
                },
            )
        }
        "resume" => {
            let runtime = boot.runtime.clone();
            crate::minecraft_autostart::resume_with(config, boot, move || {
                let mut runtime = runtime.lock().unwrap();
                runtime.service("start", "minecraft.service")?;
                runtime.healthy("minecraft.service", 1)?;
                Ok(json!({"status":"completed"}))
            })
        }
        _ => anyhow::bail!("Unsupported boot fixture operation"),
    }
}
fn completed_http_operation(
    http: &HttpFixture,
    state: &Path,
    route: &str,
    fields: &[(&str, &str)],
) {
    // The live poller legitimately owns the nonblocking deploy lock sometimes.
    // Retry only that explicit conflict through HTTP; never hide policy/safety failures.
    for _ in 0..10 {
        assert_eq!(http.post(route, fields).0, 202);
        wait(
            || read_json(&state.join("ui-operation.json"))["status"] != "running",
            "boot operation outcome",
        );
        let outcome = read_json(&state.join("ui-operation.json"));
        if outcome["status"] == "completed" {
            return;
        }
        assert_eq!(
            outcome["result"]["message"], "Another deployment is running",
            "{outcome}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    panic!("Live reconciliation did not release the operation lock");
}
fn boot_policy_and_maintenance_acceptance(
    http: &HttpFixture,
    config: &Value,
    repo: &Path,
    runtime: &Arc<Mutex<Runtime>>,
    enabled: &Arc<AtomicBool>,
    reconciles: &AtomicUsize,
) {
    let state = Path::new(config["paths"]["state"].as_str().unwrap());
    runtime.lock().unwrap().active = false;
    runtime.lock().unwrap().actions.clear();
    assert_eq!(
        http.post("/operation/minecraft-autostart", &[("policy", "enabled")])
            .0,
        400
    );
    completed_http_operation(
        http,
        state,
        "/operation/minecraft-autostart",
        &[("policy", "enabled"), ("confirmed", "yes")],
    );
    assert!(enabled.load(Ordering::SeqCst));
    assert!(
        !runtime.lock().unwrap().active,
        "Boot enable must not start the current runtime"
    );
    assert!(
        !runtime
            .lock()
            .unwrap()
            .actions
            .iter()
            .any(|action| action == "start")
    );
    // Model only systemd's reboot decision: enabled units start, disabled ones do not.
    runtime.lock().unwrap().active = enabled.load(Ordering::SeqCst);
    assert!(runtime.lock().unwrap().active);
    completed_http_operation(http, state, "/operation/stop", &[("confirmed", "yes")]);
    assert!(!enabled.load(Ordering::SeqCst));
    assert!(!runtime.lock().unwrap().active);
    runtime.lock().unwrap().active = enabled.load(Ordering::SeqCst);
    assert!(!runtime.lock().unwrap().active);
    runtime.lock().unwrap().actions.clear();
    let old_commit = read_json(&state.join("deployment.json"))["commit"].clone();
    let count = reconciles.load(Ordering::SeqCst);
    wait(
        || reconciles.load(Ordering::SeqCst) >= count + 2,
        "unchanged scheduler preserves maintenance hold",
    );
    assert!(!runtime.lock().unwrap().active);
    git(
        repo,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "Approved maintenance-time change",
        ],
    );
    git(repo, &["push", "origin", "release"]);
    let count = reconciles.load(Ordering::SeqCst);
    wait(
        || reconciles.load(Ordering::SeqCst) >= count + 2,
        "pending merge scheduler preserves maintenance hold",
    );
    assert_eq!(
        read_json(&state.join("deployment.json"))["commit"],
        old_commit
    );
    assert!(!runtime.lock().unwrap().active);
    assert!(!runtime.lock().unwrap().actions.iter().any(|a| a == "start"));
    completed_http_operation(
        http,
        state,
        "/operation/resume",
        &[("confirmed", "yes"), ("acknowledge_data_risk", "yes")],
    );
    assert!(enabled.load(Ordering::SeqCst));
    assert!(runtime.lock().unwrap().active);
    completed_http_operation(
        http,
        state,
        "/operation/minecraft-autostart",
        &[("policy", "disabled"), ("confirmed", "yes")],
    );
    assert!(!enabled.load(Ordering::SeqCst));
    completed_http_operation(http, state, "/operation/stop", &[("confirmed", "yes")]);
    completed_http_operation(
        http,
        state,
        "/operation/resume",
        &[("confirmed", "yes"), ("acknowledge_data_risk", "yes")],
    );
    assert!(!enabled.load(Ordering::SeqCst));
    runtime.lock().unwrap().active = enabled.load(Ordering::SeqCst);
    assert!(
        !runtime.lock().unwrap().active,
        "Explicit disabled policy must remain disabled after resume/reboot"
    );
}
