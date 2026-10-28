//! Shared production polling loop. Tests inject only the external reconciliation boundary.
use anyhow::Result;
use serde_json::Value;
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};
pub fn run(path: &Path, once: bool) -> Result<()> {
    run_with(path, once, crate::deploy::run, || false)
}
pub(crate) fn run_with(
    path: &Path,
    once: bool,
    mut reconcile: impl FnMut(&Value) -> Result<Value>,
    mut stopped: impl FnMut() -> bool,
) -> Result<()> {
    loop {
        if stopped() {
            return Ok(());
        }
        let config = crate::config::load(path)?;
        match reconcile(&config) {
            Ok(value) => println!("{value}"),
            Err(error) => {
                eprintln!("Deployment failed: {error}");
                if once {
                    return Err(error);
                }
            }
        }
        if once {
            return Ok(());
        }
        let deadline = Instant::now()
            + Duration::from_secs(
                config["repository"]["poll_interval_seconds"]
                    .as_u64()
                    .unwrap_or(60),
            );
        while Instant::now() < deadline {
            if stopped() {
                return Ok(());
            }
            thread::sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}
