use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use anyhow::Result;

use crate::domain::{CueFile, CueFileStatus, CueScan, Download, QueueSnapshot, SplitResult};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait LidarrQueuePort: Send + Sync {
    fn queue_snapshot(&self) -> BoxFuture<'_, Result<QueueSnapshot>>;
}

pub trait DownloadRepositoryPort: Clone + Send + 'static {
    fn all_downloads(&self) -> Result<Vec<Download>>;
    fn upsert_download(&self, download: &Download) -> Result<()>;
    fn mark_download_complete(
        &self,
        download_id: &str,
        split_complete: bool,
        last_error: Option<&str>,
    ) -> Result<()>;
    fn get_or_create_cue_file(&self, download_id: &str, path: &Path) -> Result<CueFile>;
    fn record_cue_result(
        &self,
        cue_file: &CueFile,
        status: CueFileStatus,
        message: Option<&str>,
        tracks: &[PathBuf],
    ) -> Result<()>;
    fn delete_download(&self, download_id: &str) -> Result<()>;
}

pub trait CueScannerPort: Clone + Send + 'static {
    fn find_cue_files(&self, root: &Path) -> Result<CueScan>;
}

pub trait CueSplitterPort: Clone + Send + 'static {
    fn split_cue(&self, cue_path: &Path) -> Result<SplitResult>;
}

pub trait GeneratedTrackCleanerPort: Clone + Send + 'static {
    fn remove_generated_track(&self, path: &Path) -> Result<()>;
}
