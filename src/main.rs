mod app;
mod config;
mod lidarr;
mod scanner;
mod splitter;
mod store;

use anyhow::{Context, Result};
use clap::Parser;

use crate::config::{Cli, Settings};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let settings = Settings::load(cli.config).context("load settings")?;
    app::run(settings).await
}
