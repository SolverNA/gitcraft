//! Forms for the complete operational control plane, sharing CLI/config validation.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn hidden(csrf: &str) -> String {
    format!("<input type=hidden name=csrf value='{csrf}'>")
}
fn revision(path: &Path) -> Result<String> {
    crate::config::revision(path)
}
fn plan_path(config: &Value) -> Result<PathBuf> {
    let state = Path::new(config["paths"]["state"].as_str().context("State missing")?);
    fs::create_dir_all(state)?;
    crate::deploy::safe_path(state, "ui-settings-full.json")
}
pub fn page(path: &Path, csrf: &str) -> Result<String> {
    let config = crate::config::load(path)?;
    let h = hidden(csrf);
    let poller = crate::agent_service::status();
    let mut html = format!(
        "<h2>Операции и полные настройки</h2><p>Значения проверяются до сохранения. Изменение путей не переносит миры, state, кеш или резервные копии. Нельзя менять unit через произвольную shell-команду.</p><form method=post action=/operations-settings-preview>{h}<input type=hidden name=revision value='{}'>",
        revision(path)?
    );
    html.push_str(&poller_panel(&poller, csrf));
    for (section, fields) in config.as_object().context("Config invalid")? {
        html.push_str(&format!("<fieldset><legend>{}</legend>", escape(section)));
        for (key, value) in fields.as_object().context("Section invalid")? {
            let name = format!("field.{section}.{key}");
            let val = if let Some(v) = value.as_str() {
                v.to_owned()
            } else {
                value.to_string()
            };
            html.push_str(&format!(
                "<label>{}.{} <input name='{}' value='{}' {}></label><br>",
                escape(section),
                escape(key),
                escape(&name),
                escape(&val),
                if section == "secrets" {
                    "type=text readonly"
                } else if value.is_u64() {
                    "type=number min=0"
                } else {
                    "type=text"
                }
            ));
        }
        html.push_str("</fieldset>");
    }
    html.push_str("<button>Проверить настройки и показать план</button></form>");
    html.push_str(&format!("<h3>Локальные credentials — только запись</h3><pre>{}</pre><p>Пароли не возвращаются в страницу, preview, JSON или журнал. RCON можно менять только при остановленном Minecraft. Web token сменит login и завершит действующие сессии.</p><form method=post action=/credential-save>{h}<select name=role><option value=rcon>RCON password</option><option value=web>Web login token</option></select><input type=password name=secret autocomplete=new-password required><label><input type=checkbox name=confirmed value=yes required>Подтверждаю смену локального credential</label><button>Записать секрет</button></form>",escape(&crate::secrets::status(&config).to_string())));
    for (action, title, extra) in [
        (
            "agent-activate",
            "Подтвердить и включить автоматическую проверку Git",
            "<p>Только проверенный установленный gitcraft-agent.service. При отказе автоматизация не считается включённой; initialization повторять не нужно.</p>",
        ),
        (
            "bootstrap",
            "Первый запуск — EULA",
            "<label><input type=checkbox name=accept_eula value=yes required>Я прочитал и принимаю Minecraft EULA</label>",
        ),
        (
            "stop",
            "Остановить для обслуживания (только пустой сервер)",
            "",
        ),
        (
            "resume",
            "Запустить установленную сборку",
            "<label><input type=checkbox name=acknowledge_data_risk value=yes required>Я проверил совместимость и игровые данные</label>",
        ),
        (
            "recover",
            "Восстановить прерванный deploy — оставить остановленным",
            "<label><input type=checkbox name=acknowledge_data_risk value=yes>Подтверждаю риск миграции игровых данных</label>",
        ),
        (
            "proposal-retry",
            "Повторить открытие PR для сохранённой ветки без нового commit/push",
            "",
        ),
        (
            "backup",
            "Сделать offline backup игровых данных",
            "<textarea name=include placeholder='OurRealm&#10;plugins/Example/database'></textarea>",
        ),
    ] {
        html.push_str(&format!("<form method=post action='/operation/{action}'>{h}<h3>{title}</h3>{extra}<label><input type=checkbox name=confirmed value=yes required>Подтверждаю операцию</label><button>{title}</button></form>"));
    }
    let pending = crate::recovery::pending(&config)?;
    html.push_str(&format!("<h3>Незавершённые операции</h3><pre>{}</pre><h3>Резервные копии данных</h3><pre>{}</pre><p>Статус операции: <a href='/'>главная панель</a>. Перенос имеет отдельный recovery/cleanup: <a href='/transfer'>сессии переноса</a>.</p>",escape(&json!({"deployment":pending.as_ref().map(|v|json!({"phase":v["phase"],"may_have_started":v["may_have_started"]})),"transfer_recovery_pending":crate::transfer::has_pending(&config)?}).to_string()),escape(&crate::ops::backups(&config)?.to_string())));
    Ok(html)
}
pub fn preview(path: &Path, csrf: &str, form: &HashMap<String, String>) -> Result<String> {
    let current = crate::config::load(path)?;
    ensure!(
        form.get("revision") == Some(&revision(path)?),
        "Configuration changed; reload settings"
    );
    let mut proposed = current.clone();
    for (section, fields) in current.as_object().context("Config invalid")? {
        for (key, value) in fields.as_object().context("Section invalid")? {
            let raw = form
                .get(&format!("field.{section}.{key}"))
                .context("A settings field is missing")?;
            proposed[section][key] = if value.is_u64() {
                json!(raw.parse::<u64>().context("Invalid numeric setting")?)
            } else {
                json!(raw)
            };
        }
    }
    let proposed = crate::config::validate(proposed)?;
    crate::config::operational_change(&current, &proposed)?;
    ensure!(
        crate::secrets::resolve(&proposed, "web").is_ok_and(|v| v.len() >= 24),
        "Proposed Web credential is unavailable; configure write-only credential first"
    );
    let plan = json!({"revision":revision(path)?,"previous":current,"proposed":proposed,"credential_generation":crate::secrets::generation(&current)?});
    let p = plan_path(&current)?;
    crate::recovery::durable_json(&p, &plan)?;
    let digest = crate::artifacts::sha256(&p)?;
    Ok(format!(
        "<h2>Проверьте полный план настроек</h2><pre>{}</pre><p>Web bind/address изменится после подтверждения: потребуется переподключение и вход. Если новый адрес занят, сохранение отклоняется.</p><form method=post action=/operations-settings-apply>{}<input type=hidden name=digest value='{digest}'><label><input type=checkbox name=reviewed value=yes required>Я проверил настройки и понимаю ограничения путей/системных прав</label><button>Применить проверенный план</button></form>",
        escape(&serde_json::to_string_pretty(&plan)?),
        hidden(csrf)
    ))
}
pub fn settings_plan(path: &Path, form: &HashMap<String, String>) -> Result<Value> {
    let current = crate::config::load(path)?;
    let p = plan_path(&current)?;
    ensure!(
        form.get("reviewed").is_some_and(|v| v == "yes"),
        "Review confirmation required"
    );
    ensure!(
        form.get("digest") == Some(&crate::artifacts::sha256(&p)?),
        "Settings plan changed"
    );
    let plan: Value = serde_json::from_slice(&fs::read(p)?)?;
    ensure!(
        plan["previous"] == current && plan["revision"].as_str() == Some(revision(path)?.as_str()),
        "Configuration changed since preview"
    );
    ensure!(
        plan["credential_generation"] == crate::secrets::generation(&current)?,
        "Credentials changed since preview; review a fresh settings plan"
    );
    crate::config::operational_change(&current, &plan["proposed"])?;
    Ok(plan)
}
pub fn record(config: &Value, operation: &str, status: &str, result: &Value) -> Result<()> {
    let state = Path::new(config["paths"]["state"].as_str().context("State missing")?);
    fs::create_dir_all(state)?;
    let path = crate::deploy::safe_path(state, "ui-operation.json")?;
    crate::recovery::durable_json(
        &path,
        &json!({"operation":operation,"status":status,"result":result,"process_id":std::process::id(),"timestamp":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()}),
    )
}
pub fn run_operation(config: &Value, name: &str, form: &HashMap<String, String>) -> Result<Value> {
    validate_operation(name, form)?;
    ensure!(
        form.get("confirmed").is_some_and(|v| v == "yes"),
        "Explicit operation confirmation required"
    );
    match name {
        "agent-activate" => Ok(match crate::agent_service::activate(config) {
            Ok(value) => value,
            Err(error) => {
                json!({"status":"activation_required","message":error.to_string(),"observed":crate::agent_service::status(),"next_action":"agent-activate"})
            }
        }),
        "bootstrap" => {
            ensure!(
                form.get("accept_eula").is_some_and(|v| v == "yes"),
                "Explicit EULA acceptance required"
            );
            crate::deploy::bootstrap(config, true)
        }
        "stop" => crate::ops::stop(config),
        "resume" => crate::ops::resume(
            config,
            form.get("acknowledge_data_risk")
                .is_some_and(|v| v == "yes"),
        ),
        "recover" => crate::recovery::recover(
            config,
            form.get("acknowledge_data_risk")
                .is_some_and(|v| v == "yes"),
        ),
        "backup" => {
            let paths = form
                .get("include")
                .context("Select explicit data paths")?
                .lines()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            crate::recovery::backup_data(config, &paths)
        }
        "proposal-retry" => {
            let repo = crate::repository::authoring_repository(config)?;
            crate::repository::retry_proposal(
                &repo,
                config["repository"]["branch"]
                    .as_str()
                    .context("Branch missing")?,
            )
        }
        _ => anyhow::bail!("Unknown operational command"),
    }
}
pub fn validate_operation(name: &str, form: &HashMap<String, String>) -> Result<()> {
    ensure!(
        [
            "bootstrap",
            "agent-activate",
            "stop",
            "resume",
            "recover",
            "backup",
            "proposal-retry"
        ]
        .contains(&name),
        "Unknown operation"
    );
    ensure!(
        form.get("confirmed").is_some_and(|value| value == "yes"),
        "Explicit operation confirmation required"
    );
    if name == "bootstrap" {
        ensure!(
            form.get("accept_eula").is_some_and(|value| value == "yes"),
            "Explicit EULA acceptance required"
        );
    }
    if name == "resume" {
        ensure!(
            form.get("acknowledge_data_risk")
                .is_some_and(|value| value == "yes"),
            "Game-data acknowledgement required"
        );
    }
    if name == "backup" {
        ensure!(
            form.get("include")
                .is_some_and(|value| !value.trim().is_empty()),
            "Select explicit backup data paths"
        );
    }
    Ok(())
}

pub fn outcome(config: &Value, result: Result<Value>) -> (String, Value) {
    match result {
        Ok(value) => {
            let status = if ["waiting_for_players", "rejected", "initialization_required"]
                .contains(&value["status"].as_str().unwrap_or(""))
            {
                "rejected"
            } else if ["partial_success", "activation_required"]
                .contains(&value["status"].as_str().unwrap_or(""))
                || value["agent_activation"]["status"] == "activation_required"
            {
                "activation_required"
            } else if value["status"] == "recovery_required" {
                "recovery_required"
            } else {
                "completed"
            };
            (status.into(), value)
        }
        Err(error) => {
            let message = error.to_string();
            let recovery = message.contains("recovery_required")
                || crate::recovery::pending(config).ok().flatten().is_some()
                || crate::transfer::has_pending(config).unwrap_or(true);
            let rejected = [
                "required",
                "requires",
                "rejected",
                "online",
                "missing",
                "changed",
                "confirmation",
            ]
            .iter()
            .any(|word| message.to_ascii_lowercase().contains(word));
            (
                if recovery {
                    "recovery_required"
                } else if rejected {
                    "rejected"
                } else {
                    "error"
                }
                .into(),
                json!({"message":message}),
            )
        }
    }
}
pub fn recover_interrupted_ui(path: &Path) -> Result<()> {
    let config = crate::config::load(path)?;
    let file = Path::new(config["paths"]["state"].as_str().context("State missing")?)
        .join("ui-operation.json");
    if file.is_file() {
        let value: Value = serde_json::from_slice(&fs::read(file)?)?;
        if value["status"] == "running"
            && value["process_id"].as_u64() != Some(std::process::id() as u64)
        {
            let operation = value["operation"].as_str().unwrap_or("unknown");
            let status = if ["bootstrap", "deploy", "resume", "recover"].contains(&operation) {
                "recovery_required"
            } else {
                "error"
            };
            record(
                &config,
                operation,
                status,
                &json!({"message":"Web process restarted during an operation. Inspect durable journals and actual service state; no automatic retry was performed."}),
            )?;
        }
    }
    Ok(())
}

fn poller_panel(status: &Value, _csrf: &str) -> String {
    format!(
        "<section id=poller-status><h3>Автоматическая проверка Git</h3><p>Фактическое состояние установленного фонового сервиса: active и enabled проверяются отдельно. Unknown означает, что состояние не подтверждено. Успешный старт Minecraft сам по себе не означает включение автообновлений.</p><pre>{}</pre><p>При activation_required проверьте причину ниже и используйте подтверждённое включение агента. Не повторяйте bootstrap/adopt только ради запуска poller.</p></section>",
        escape(&serde_json::to_string_pretty(status).unwrap_or_else(|_| "unknown".into()))
    )
}

#[cfg(test)]
mod activation_ui_tests {
    use super::*;

    #[test]
    fn initialized_without_enabled_poller_is_not_completed() {
        let result = json!({"status":"partial_success","initialized":true,"initialization_status":"adopted","agent_activation":{"status":"activation_required","observed":{"active":"inactive","enabled":"disabled"}}});
        let (status, details) = outcome(&json!({}), Ok(result.clone()));
        assert_eq!(status, "activation_required");
        assert_eq!(details, result);
        let (status, _) = outcome(
            &json!({}),
            Ok(
                json!({"status":"deployed","agent_activation":{"status":"activated","active":"active","enabled":"enabled"}}),
            ),
        );
        assert_eq!(status, "completed");
    }

    #[test]
    fn poller_retry_requires_confirmation_and_unknown_is_not_false() {
        assert!(validate_operation("agent-activate", &HashMap::new()).is_err());
        assert!(
            validate_operation(
                "agent-activate",
                &HashMap::from([("confirmed".into(), "yes".into())])
            )
            .is_ok()
        );
        let html = poller_panel(
            &json!({"active":"unknown","enabled":"unknown","message":"<script>"}),
            "csrf",
        );
        assert!(html.contains("unknown"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
