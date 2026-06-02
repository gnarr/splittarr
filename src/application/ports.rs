use std::path::Path;

use anyhow::Result;

use crate::domain::{
    CueSheet, CueSheetStatus, DiscoveredCueSheets, InputFileKind, QueueSnapshot, RecordedTrack,
    SplitOutcome, TrackCleanupOutcome, TrackCleanupStatus, TrackedDownload,
};

pub trait QueueSource {
    async fn queue_snapshot(&self) -> Result<QueueSnapshot>;
}

pub trait DownloadStore {
    async fn load_tracked_downloads(&self) -> Result<Vec<TrackedDownload>>;
    async fn load_tracked_download_summaries(&self) -> Result<Vec<TrackedDownload>> {
        self.load_tracked_downloads().await
    }
    async fn get_tracked_download(&self, download_id: &str) -> Result<Option<TrackedDownload>>;
    async fn upsert_tracked_download(&self, download: &TrackedDownload) -> Result<()>;
    async fn mark_download_processing(&self, download_id: &str) -> Result<()>;
    async fn mark_download_awaiting_import(&self, download_id: &str) -> Result<()>;
    async fn mark_download_cleanup_started(&self, download_id: &str) -> Result<()>;
    async fn mark_download_completed(&self, download_id: &str) -> Result<()>;
    async fn mark_download_failed(&self, download_id: &str, last_error: Option<&str>) -> Result<()>;
    async fn get_or_create_cue_sheet(&self, download_id: &str, path: &Path) -> Result<CueSheet>;
    async fn record_input_file(
        &self,
        download_id: &str,
        cue_sheet_id: Option<&str>,
        path: &Path,
        kind: InputFileKind,
        size_bytes: Option<i64>,
    ) -> Result<()>;
    async fn record_cue_result(
        &self,
        cue_sheet: &CueSheet,
        status: CueSheetStatus,
        message: Option<&str>,
        tracks: &[RecordedTrack],
    ) -> Result<()>;
    async fn record_track_cleanup(
        &self,
        download_id: &str,
        track_id: &str,
        status: TrackCleanupStatus,
        message: Option<&str>,
    ) -> Result<()>;
}

pub trait CueScanner {
    async fn find_cue_sheets(&self, root: &Path) -> Result<DiscoveredCueSheets>;
}

pub trait CueSplitter {
    async fn split_cue(&self, cue_path: &Path) -> Result<SplitOutcome>;
}

pub trait TrackCleanup {
    async fn cleanup_download_tracks(&self, download: &TrackedDownload)
    -> Result<Vec<TrackCleanupOutcome>>;
}
