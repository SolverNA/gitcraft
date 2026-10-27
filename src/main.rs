use anyhow::Result;
use clap::{Parser, Subcommand};
use gitcraft::{artifacts, config, deploy, updater, web};
use std::{path::PathBuf, thread, time::Duration};
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
}
fn execute() -> Result<()> {
    match Cli::parse().command {
        Command::Validate { repository } => {
            artifacts::load_manifest(&repository)?;
            validate_yaml(&repository.join("server-settings"))?;
            validate_yaml(&repository.join("plugins-configs"))?;
            println!("{{\"valid\":true}}");
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
    }
    Ok(())
}
fn validate_yaml(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        anyhow::ensure!(!path.is_symlink(), "Config symlinks forbidden");
        if path.is_dir() {
            validate_yaml(&path)?;
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yml" | "yaml")
        ) {
            let _: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(path)?)?;
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = execute() {
        eprintln!("gitcraft: {error:#}");
        std::process::exit(1);
    }
}
