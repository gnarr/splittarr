use std::path::{Path, PathBuf};

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
    async fn get_tracked_downloads(&self, download_ids: &[String]) -> Result<Vec<TrackedDownload>> {
        let mut downloads = Vec::new();
        for download_id in download_ids {
            if let Some(download) = self.get_tracked_download(download_id).await? {
                downloads.push(download);
            }
        }
        Ok(downloads)
    }
    async fn upsert_tracked_download(&self, download: &TrackedDownload) -> Result<()>;
    async fn mark_download_processing(&self, download_id: &str) -> Result<()>;
    async fn mark_download_awaiting_import(&self, download_id: &str) -> Result<()>;
    async fn mark_download_cleanup_started(&self, download_id: &str) -> Result<()>;
    async fn mark_download_completed(&self, download_id: &str) -> Result<()>;
    async fn mark_download_failed(&self, download_id: &str, last_error: Option<&str>)
        -> Result<()>;
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
    async fn record_track_cleanups(
        &self,
        download_id: &str,
        outcomes: &[TrackCleanupOutcome],
    ) -> Result<()> {
        for outcome in outcomes {
            self.record_track_cleanup(
                download_id,
                &outcome.track_id,
                outcome.status,
                outcome.message.as_deref(),
            )
            .await?;
        }
        Ok(())
    }
}

pub trait CueScanner {
    async fn find_cue_sheets(&self, root: &Path) -> Result<DiscoveredCueSheets>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueInputSnapshot {
    pub cue_size_bytes: Option<i64>,
    pub audio_inputs: Vec<CueReferencedAudioInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueReferencedAudioInput {
    pub path: PathBuf,
    pub size_bytes: Option<i64>,
}

pub trait CueInputInspector {
    async fn snapshot_inputs(&self, cue_path: &Path) -> Result<CueInputSnapshot>;
    async fn cue_references_audio_file(&self, cue_path: &Path, audio_path: &Path) -> Result<bool>;
}

pub trait CueSplitter {
    async fn split_cue(&self, cue_path: &Path) -> Result<SplitOutcome>;
}

pub trait TrackCleanup {
    async fn cleanup_download_tracks(
        &self,
        download: &TrackedDownload,
    ) -> Result<Vec<TrackCleanupOutcome>>;
}
