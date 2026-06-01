use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::domain::{
    CueSheet, CueSheetStatus, DiscoveredCueSheets, QueueSnapshot, SplitOutcome, TrackedDownload,
};

pub trait QueueSource {
    async fn queue_snapshot(&self) -> Result<QueueSnapshot>;
}

pub trait DownloadStore {
    async fn load_tracked_downloads(&self) -> Result<Vec<TrackedDownload>>;
    async fn upsert_tracked_download(&self, download: &TrackedDownload) -> Result<()>;
    async fn mark_download_complete(
        &self,
        download_id: &str,
        split_complete: bool,
        last_error: Option<&str>,
    ) -> Result<()>;
    async fn get_or_create_cue_sheet(&self, download_id: &str, path: &Path) -> Result<CueSheet>;
    async fn record_cue_result(
        &self,
        cue_sheet: &CueSheet,
        status: CueSheetStatus,
        message: Option<&str>,
        tracks: &[PathBuf],
    ) -> Result<()>;
    async fn delete_download(&self, download_id: &str) -> Result<()>;
}

pub trait CueScanner {
    async fn find_cue_sheets(&self, root: &Path) -> Result<DiscoveredCueSheets>;
}

pub trait CueSplitter {
    async fn split_cue(&self, cue_path: &Path) -> Result<SplitOutcome>;
}

pub trait TrackCleanup {
    async fn cleanup_download_tracks(&self, download: &TrackedDownload) -> Result<()>;
}
