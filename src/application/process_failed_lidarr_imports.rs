use std::time::Duration;

use anyhow::{Context, Result};
use chrono::prelude::*;

use crate::adapters::filesystem::{CueFileScanner, GeneratedTrackCleaner};
use crate::adapters::lidarr::LidarrClient;
use crate::adapters::shnsplit::ShnsplitCueSplitter;
use crate::adapters::sqlite::SqliteDownloadRepository;
use crate::application::ports::{
    CueScannerPort, CueSplitterPort, DownloadRepositoryPort, GeneratedTrackCleanerPort,
    LidarrQueuePort,
};
use crate::config::Settings;

mod cleanup_download;
mod process_download;
mod register_candidates;
mod run_cycle;

pub use cleanup_download::cleanup_track_path;
pub use run_cycle::classify_downloads;

pub async fn process_failed_lidarr_imports(settings: Settings) -> Result<()> {
    let repository =
        SqliteDownloadRepository::open(&settings.data_dir).context("initialize Splittarr database")?;
    let lidarr = LidarrClient::new(&settings.lidarr);
    let scanner = CueFileScanner;
    let splitter = ShnsplitCueSplitter::new(&settings.cue, &settings.shnsplit);
    let cleaner = GeneratedTrackCleaner;
    let interval = Duration::from_secs(settings.check_frequency_seconds);

    FailedLidarrImportProcessor::new(repository, lidarr, scanner, splitter, cleaner)
        .run_forever(interval, settings.check_frequency_seconds)
        .await
}

#[derive(Clone)]
pub struct FailedLidarrImportProcessor<Repository, Lidarr, Scanner, Splitter, Cleaner> {
    pub(super) repository: Repository,
    pub(super) lidarr: Lidarr,
    pub(super) scanner: Scanner,
    pub(super) splitter: Splitter,
    pub(super) cleaner: Cleaner,
}

impl<Repository, Lidarr, Scanner, Splitter, Cleaner>
    FailedLidarrImportProcessor<Repository, Lidarr, Scanner, Splitter, Cleaner>
where
    Repository: DownloadRepositoryPort,
    Lidarr: LidarrQueuePort,
    Scanner: CueScannerPort,
    Splitter: CueSplitterPort,
    Cleaner: GeneratedTrackCleanerPort,
{
    pub fn new(
        repository: Repository,
        lidarr: Lidarr,
        scanner: Scanner,
        splitter: Splitter,
        cleaner: Cleaner,
    ) -> Self {
        Self {
            repository,
            lidarr,
            scanner,
            splitter,
            cleaner,
        }
    }

    pub async fn run_forever(
        &self,
        interval: Duration,
        check_frequency_seconds: u64,
    ) -> Result<()> {
        println!("Splittarr");
        println!("Checking every {check_frequency_seconds} seconds");

        loop {
            println!(
                "Checking Lidarr's download queue at {}",
                Local::now().format("%Y-%m-%d %H:%M:%S %z")
            );

            if let Err(err) = self.run_once().await {
                eprintln!("Splittarr cycle failed: {err:#}");
            }

            tokio::time::sleep(interval).await;
        }
    }
}

pub(super) async fn blocking<T>(operation: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .context("blocking task failed to join")?
}
