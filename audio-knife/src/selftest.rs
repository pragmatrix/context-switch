//! Startup self-test: verifies deployment configuration that the server itself does not
//! depend on, so problems are surfaced early without preventing startup.

use std::env;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::warn;

/// Runs the startup self-tests. Failures are logged as warnings with context about what will
/// not work, but never prevent startup.
pub fn run() {
    if let Err(err) = google_credentials() {
        warn!("The `google-transcribe` service will not work: {err:#}");
    }
}

/// Checks `GOOGLE_APPLICATION_CREDENTIALS`: the variable must be set and point to a file
/// containing valid JSON. Returns the first problem found as an error.
fn google_credentials() -> Result<()> {
    const VAR: &str = "GOOGLE_APPLICATION_CREDENTIALS";

    let path = env::var(VAR).context(format!("{VAR} is not set"))?;

    let json = fs::read_to_string(Path::new(&path))
        .with_context(|| format!("{VAR}={path}: cannot read file"))?;

    serde_json::from_str::<serde_json::Value>(&json)
        .with_context(|| format!("{VAR}={path}: not valid JSON"))?;

    Ok(())
}
