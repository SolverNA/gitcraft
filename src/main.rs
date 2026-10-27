use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gitcraft::{config, deploy, transfer, updater, web};
use std::{fs, io::Write, path::PathBuf, thread, time::Duration};
#[derive(Parser)]
#[command(version, about = "Minecraft GitOps manager")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Validate {
        #[arg(long)]
        repository: PathBuf,
    },
    Agent {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        once: bool,
    },
    Deploy {
        #[arg(long)]
        config: PathBuf,
    },
    Bootstrap {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        accept_eula: bool,
    },
    Web {
        #[arg(long)]
        config: PathBuf,
    },
    Status {
        #[arg(long)]
        config: PathBuf,
    },
    UpdateDependencies {
        #[arg(long)]
        repository: PathBuf,
    },
    Transfer {
        #[command(subcommand)]
        command: Transfer,
    },
    Export {
        #[command(subcommand)]
        command: Export,
    },
    Propose {
        #[arg(long)]
        repository: PathBuf,
        #[arg(long, default_value = "main")]
        base: String,
        #[arg(long)]
        title: String,
    },
    Adopt {
        #[command(subcommand)]
        command: Adopt,
    },
    Recover {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        acknowledge_data_risk: bool,
    },
    BackupData {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, required = true)]
        include: Vec<String>,
    },
}
#[derive(Subcommand)]
enum Export {
    Plan {
        #[arg(long)]
        server: PathBuf,
        #[arg(long)]
        include: Vec<String>,
        #[arg(long)]
        metadata: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
    Apply {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        reviewed: bool,
    },
}
#[derive(Subcommand)]
enum Adopt {
    Plan {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        server: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    Apply {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        repository: PathBuf,
        #[arg(long)]
        expected_commit: String,
    },
}
#[derive(Subcommand)]
enum Transfer {
    Plan {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        source_consistency: String,
        #[arg(long)]
        output: PathBuf,
    },
    Send {
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        session: PathBuf,
        #[arg(long, default_value = "GITCRAFT_WEB_TOKEN")]
        token_env: String,
    },
    Status {
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        session: PathBuf,
    },
    Verify {
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        session: PathBuf,
    },
    Commit {
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        session: PathBuf,
        #[arg(long)]
        confirm_world_replace: bool,
    },
    Recover {
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        session: PathBuf,
    },
}
fn write_private(path: &std::path::Path, value: &serde_json::Value) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new(".")),
    )?;
    writeln!(file, "{}", serde_json::to_string_pretty(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
fn session_request(
    endpoint: &str,
    path: &std::path::Path,
    method: &str,
    suffix: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let s: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let id = s["id"].as_str().context("session ID missing")?;
    let token = s["token"].as_str().context("session token missing")?;
    transfer::request(
        endpoint,
        method,
        &format!("/api/transfers/{id}{suffix}"),
        token,
        body,
    )
}
fn execute() -> Result<()> {
    match Cli::parse().command {
        Command::Validate { repository } => {
            println!("{}", gitcraft::policy::validate_repository(&repository)?);
        }
        Command::Agent { config: path, once } => loop {
            let value = config::load(&path)?;
            match deploy::run(&value) {
                Ok(result) => println!("{result}"),
                Err(error) => {
                    eprintln!("Deployment failed: {error}");
                    if once {
                        return Err(error);
                    }
                }
            }
            if once {
                break;
            }
            thread::sleep(Duration::from_secs(
                value["repository"]["poll_interval_seconds"]
                    .as_u64()
                    .unwrap_or(60),
            ));
        },
        Command::Deploy { config: path } => println!("{}", deploy::run(&config::load(&path)?)?),
        Command::Bootstrap {
            config: path,
            accept_eula,
        } => println!("{}", deploy::bootstrap(&config::load(&path)?, accept_eula)?),
        Command::Web { config: path } => web::serve(&path)?,
        Command::Status { config: path } => println!("{}", web::status(&config::load(&path)?)),
        Command::UpdateDependencies { repository } => {
            println!("{}", updater::update_repository(&repository)?)
        }
        Command::Recover {
            config: path,
            acknowledge_data_risk,
        } => println!(
            "{}",
            gitcraft::recovery::recover(&config::load(&path)?, acknowledge_data_risk)?
        ),
        Command::BackupData {
            config: path,
            include,
        } => println!(
            "{}",
            gitcraft::recovery::backup_data(&config::load(&path)?, &include)?
        ),
        Command::Propose {
            repository,
            base,
            title,
        } => println!(
            "{}",
            gitcraft::repository::propose(&repository, &base, &title)?
        ),
        Command::Export { command } => match command {
            Export::Plan {
                server,
                include,
                metadata,
                output,
            } => {
                let plan = gitcraft::export::plan(&server, &include, metadata.as_deref())?;
                write_private(&output, &plan)?;
                println!(
                    "{}",
                    serde_json::json!({"plan":output,"review_required":true})
                );
            }
            Export::Apply {
                plan,
                destination,
                reviewed,
            } => println!(
                "{}",
                gitcraft::export::apply(&plan, &destination, reviewed)?
            ),
        },
        Command::Adopt { command } => match command {
            Adopt::Plan {
                config: path,
                server,
                output,
            } => {
                let plan = gitcraft::adopt::plan(&config::load(&path)?, &server)?;
                write_private(&output, &plan)?;
                println!(
                    "{}",
                    serde_json::json!({"plan":output,"review_required":true})
                );
            }
            Adopt::Apply {
                config: path,
                plan,
                repository,
                expected_commit,
            } => {
                let plan = serde_json::from_slice(&fs::read(plan)?)?;
                println!(
                    "{}",
                    gitcraft::adopt::apply(
                        &config::load(&path)?,
                        &plan,
                        &repository,
                        &expected_commit
                    )?
                );
            }
        },
        Command::Transfer { command } => match command {
            Transfer::Plan {
                source,
                kind,
                source_consistency,
                output,
            } => {
                let plan = transfer::plan(&source, &kind, &source_consistency)?;
                write_private(&output, &plan)?;
                println!("{}", serde_json::json!({"plan":output}));
            }
            Transfer::Send {
                endpoint,
                plan,
                session,
                token_env,
            } => println!(
                "{}",
                transfer::send(
                    &endpoint,
                    &std::env::var(token_env).context("transfer admin token env missing")?,
                    &plan,
                    &session
                )?
            ),
            Transfer::Status { endpoint, session } => {
                println!("{}", session_request(&endpoint, &session, "GET", "", None)?)
            }
            Transfer::Verify { endpoint, session } => println!(
                "{}",
                session_request(
                    &endpoint,
                    &session,
                    "POST",
                    "/verify",
                    Some(&serde_json::json!({}))
                )?
            ),
            Transfer::Commit {
                endpoint,
                session,
                confirm_world_replace,
            } => println!(
                "{}",
                session_request(
                    &endpoint,
                    &session,
                    "POST",
                    "/commit",
                    Some(&serde_json::json!({"confirm_world_replace":confirm_world_replace}))
                )?
            ),
            Transfer::Recover { endpoint, session } => println!(
                "{}",
                session_request(
                    &endpoint,
                    &session,
                    "POST",
                    "/recover",
                    Some(&serde_json::json!({}))
                )?
            ),
        },
    }
    Ok(())
}
fn main() {
    if let Err(error) = execute() {
        eprintln!("gitcraft: {error:#}");
        std::process::exit(1);
    }
}
