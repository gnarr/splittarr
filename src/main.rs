mod adapters;
mod application;
mod bootstrap;
mod domain;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use crate::adapters::filesystem_cleanup::FilesystemTrackCleanup;
use crate::adapters::filesystem_cue_input_inspector::FilesystemCueInputInspector;
use crate::adapters::filesystem_cue_scanner::FilesystemCueScanner;
use crate::adapters::filesystem_download_log::FilesystemDownloadLog;
use crate::adapters::gnudb_api::GnudbDiscReleaseLookup;
use crate::adapters::lidarr_api::LidarrQueueSource;
use crate::adapters::musicbrainz_api::FilesystemMusicBrainzDiscReleaseLookup;
use crate::adapters::shnsplit_splitter::ShnsplitCueSplitter;
use crate::adapters::sqlite_download_store::SqliteDownloadStore;
use crate::adapters::web;
use crate::application::service::{MonitorService, ProcessingAdapters};
use crate::bootstrap::settings::{Cli, Settings};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let settings = Settings::load(cli.config).context("load settings")?;
    let disc_release_lookup = Arc::new(GnudbDiscReleaseLookup::new(&settings.gnudb));
    let musicbrainz_lookup = Arc::new(FilesystemMusicBrainzDiscReleaseLookup::new(
        &settings.musicbrainz,
    ));
    let queue_source = LidarrQueueSource::new(&settings.lidarr)
        .with_musicbrainz_disc_release_lookup(musicbrainz_lookup)
        .with_musicbrainz_trust_disc_lookup(settings.musicbrainz.trust_disc_lookup)
        .with_musicbrainz_add_missing_release_group(
            settings.musicbrainz.add_missing_release_group_enabled,
        )
        .with_disc_release_lookup(disc_release_lookup);
    let manual_import = queue_source.clone();
    let download_store =
        SqliteDownloadStore::open(&settings.data_dir).context("initialize Splittarr database")?;
    let web_store = download_store.clone();
    let status_config = web::StatusConfig {
        version: env!("CARGO_PKG_VERSION"),
        data_dir: settings.data_dir.to_string_lossy().into_owned(),
        check_frequency_seconds: settings.check_frequency_seconds,
        download_log_enabled: settings.logging.download_log_enabled,
        lidarr_url: settings.lidarr.url.clone(),
        manual_import_enabled: settings.lidarr.manual_import_enabled,
        musicbrainz_enabled: settings.musicbrainz.disc_lookup_enabled,
        musicbrainz_base_url: settings.musicbrainz.base_url.clone(),
        musicbrainz_trust_disc_lookup: settings.musicbrainz.trust_disc_lookup,
        musicbrainz_add_missing_release_group: settings.musicbrainz.add_missing_release_group_enabled,
        gnudb_enabled: settings.gnudb.disc_lookup_enabled,
        gnudb_server: settings.gnudb.server.clone(),
        cue_strict: settings.cue.strict,
        shnsplit_path: settings.shnsplit.path.to_string_lossy().into_owned(),
        shnsplit_overwrite: settings.shnsplit.overwrite,
        shnsplit_format: settings.shnsplit.format.clone(),
    };
    let cue_scanner = FilesystemCueScanner::new();
    let cue_input_inspector = FilesystemCueInputInspector::new();
    let download_log = FilesystemDownloadLog::new(settings.logging.download_log_enabled);
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
        ProcessingAdapters {
            cue_scanner,
            cue_input_inspector,
            cue_splitter,
            manual_import,
            download_log,
            track_cleanup,
        },
        settings.check_frequency_seconds,
    );
    let listener = tokio::net::TcpListener::bind(&settings.server.bind_address)
        .await
        .with_context(|| format!("bind {}", settings.server.bind_address))?;
    let app = web::router(web_store, status_config);

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
