mod adapters;
mod application;
mod bootstrap;
mod domain;

use anyhow::{Context, Result};
use clap::Parser;

use crate::adapters::filesystem_cleanup::FilesystemTrackCleanup;
use crate::adapters::filesystem_cue_scanner::FilesystemCueScanner;
use crate::adapters::lidarr_api::LidarrQueueSource;
use crate::adapters::shnsplit_splitter::ShnsplitCueSplitter;
use crate::adapters::sqlite_download_store::SqliteDownloadStore;
use crate::adapters::web;
use crate::application::service::MonitorService;
use crate::bootstrap::settings::{Cli, Settings};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let settings = Settings::load(cli.config).context("load settings")?;
    let queue_source = LidarrQueueSource::new(&settings.lidarr);
    let download_store =
        SqliteDownloadStore::open(&settings.data_dir).context("initialize Splittarr database")?;
    let web_store = download_store.clone();
    let cue_scanner = FilesystemCueScanner::new();
    let cue_splitter = ShnsplitCueSplitter::new(
        settings.cue.strict,
        settings.shnsplit.path.clone(),
        settings.shnsplit.overwrite,
        settings.shnsplit.format.clone(),
    );
    let track_cleanup = FilesystemTrackCleanup::new();
    let service = MonitorService::new(
        queue_source,
        download_store,
        cue_scanner,
        cue_splitter,
        track_cleanup,
        settings.check_frequency_seconds,
    );
    let listener = tokio::net::TcpListener::bind(&settings.server.bind_address)
        .await
        .with_context(|| format!("bind {}", settings.server.bind_address))?;
    let app = web::router(web_store);

    println!(
        "Web UI listening on http://{}",
        settings.server.bind_address
    );

    let local = tokio::task::LocalSet::new();
    local.spawn_local(async move {
        if let Err(err) = service.run().await {
            eprintln!("monitor exited: {err:#}");
        }
    });

    local
        .run_until(async move { axum::serve(listener, app).await.context("run web server") })
        .await
}
