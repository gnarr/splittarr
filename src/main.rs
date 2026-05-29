mod adapters;
mod application;
mod config;
mod domain;

use anyhow::{Context, Result};
use clap::Parser;

use crate::config::{Cli, Settings};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let settings = Settings::load(cli.config).context("load settings")?;
    application::process_failed_lidarr_imports(settings).await
}
