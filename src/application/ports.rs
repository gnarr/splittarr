use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{
    CueSheet, CueSheetStatus, DiscoveredCueSheets, DownloadLifecycleState, InputFileKind,
    QueueSnapshot, RecordedTrack, SplitOutcome, TrackCleanupOutcome, TrackCleanupStatus,
    TrackedDownload,
};

pub trait QueueSource {
    async fn queue_snapshot(&self) -> Result<QueueSnapshot>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadHistoryRow {
    pub download_id: String,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub tracked_download_state: String,
    pub lifecycle_state: DownloadLifecycleState,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub generated_track_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct DownloadStats {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub awaiting_import: usize,
    pub in_progress: usize,
}

#[async_trait]
pub trait DownloadReadStore {
    async fn load_download_rows(&self) -> Result<Vec<DownloadHistoryRow>>;
    async fn load_download_row(&self, download_id: &str) -> Result<Option<DownloadHistoryRow>>;
    async fn get_tracked_download(&self, download_id: &str) -> Result<Option<TrackedDownload>>;
    async fn load_download_stats(&self) -> Result<DownloadStats>;
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
    async fn record_download_warning(&self, _download_id: &str, _message: &str) -> Result<()> {
        Ok(())
    }
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
    async fn file_size(&self, path: &Path) -> Result<Option<i64>>;
    async fn snapshot_inputs(&self, cue_path: &Path) -> Result<CueInputSnapshot>;
    async fn cue_references_audio_file(&self, cue_path: &Path, audio_path: &Path) -> Result<bool>;
    async fn filter_cue_files_for_audio(
        &self,
        cue_files: Vec<PathBuf>,
        audio_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        let mut matching = Vec::new();
        for cue_path in cue_files {
            if self
                .cue_references_audio_file(&cue_path, audio_path)
                .await?
            {
                matching.push(cue_path);
            }
        }
        Ok(matching)
    }
}

pub trait CueSplitter {
    async fn split_cue(&self, cue_path: &Path) -> Result<SplitOutcome>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualImportRequest {
    pub download: TrackedDownload,
    pub generated_tracks: Vec<PathBuf>,
    pub cue_hints: Vec<CueMetadataHint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueMetadataHint {
    pub path: PathBuf,
    pub album_title: Option<String>,
    pub performer: Option<String>,
    pub catalog: Option<String>,
    pub disc_id: Option<String>,
    pub comments: Vec<(String, String)>,
    pub track_count: usize,
    pub tracks: Vec<CueTrackHint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueTrackHint {
    pub number: String,
    pub title: Option<String>,
    pub performer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualImportResult {
    Disabled,
    Started {
        imported_track_count: usize,
        diagnostic: String,
    },
    Skipped {
        reason: String,
        diagnostic: String,
    },
}

pub trait ManualImportTrigger {
    async fn trigger_manual_import(
        &self,
        request: ManualImportRequest,
    ) -> Result<ManualImportResult>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicBrainzDiscLookupRequest {
    pub cue_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MusicBrainzDiscLookupResult {
    Disabled {
        diagnostic: String,
    },
    Found {
        releases: Vec<MusicBrainzDiscRelease>,
        diagnostic: String,
    },
    NotFound {
        diagnostic: String,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MusicBrainzDiscRelease {
    pub id: String,
    pub title: Option<String>,
    pub date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    pub barcode: Option<String>,
    pub quality: Option<String>,
    pub media_count: usize,
    pub media_formats: Vec<String>,
    pub media_track_counts: Vec<usize>,
    pub label_count: usize,
    pub release_group_id: Option<String>,
    pub release_group_title: Option<String>,
    pub release_group_first_release_date: Option<String>,
}

#[async_trait]
pub trait MusicBrainzDiscReleaseLookup: Send + Sync {
    async fn lookup_musicbrainz_disc_releases(
        &self,
        request: MusicBrainzDiscLookupRequest,
    ) -> Result<MusicBrainzDiscLookupResult>;
}

#[derive(Debug, Default, Clone)]
pub struct NoopMusicBrainzDiscReleaseLookup;

#[async_trait]
impl MusicBrainzDiscReleaseLookup for NoopMusicBrainzDiscReleaseLookup {
    async fn lookup_musicbrainz_disc_releases(
        &self,
        _request: MusicBrainzDiscLookupRequest,
    ) -> Result<MusicBrainzDiscLookupResult> {
        Ok(MusicBrainzDiscLookupResult::Disabled {
            diagnostic: "MusicBrainz lookup: disabled\n".into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscReleaseLookupRequest {
    pub disc_id: String,
    pub artist: Option<String>,
    pub album_title: Option<String>,
    pub year: Option<i32>,
    pub track_count: usize,
    pub track_titles_by_number: Vec<(i64, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscReleaseLookupResult {
    Disabled {
        diagnostic: String,
    },
    Found {
        candidates: Vec<DiscReleaseCandidate>,
        diagnostic: String,
    },
    NotFound {
        diagnostic: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscReleaseCandidate {
    pub category: String,
    pub entry_id: String,
    pub disc_id: String,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub year: Option<i32>,
    pub track_titles: Vec<String>,
    pub art_ids: Vec<String>,
}

#[async_trait]
pub trait DiscReleaseLookup: Send + Sync {
    async fn lookup_disc_release(
        &self,
        request: DiscReleaseLookupRequest,
    ) -> Result<DiscReleaseLookupResult>;
}

#[derive(Debug, Default, Clone)]
pub struct NoopDiscReleaseLookup;

#[async_trait]
impl DiscReleaseLookup for NoopDiscReleaseLookup {
    async fn lookup_disc_release(
        &self,
        _request: DiscReleaseLookupRequest,
    ) -> Result<DiscReleaseLookupResult> {
        Ok(DiscReleaseLookupResult::Disabled {
            diagnostic: "GnuDB lookup: disabled\n".into(),
        })
    }
}

pub trait TrackCleanup {
    async fn cleanup_download_tracks(
        &self,
        download: &TrackedDownload,
    ) -> Result<Vec<TrackCleanupOutcome>>;
}

pub trait DownloadLog {
    async fn write_download_log(&self, download: &TrackedDownload, content: &str) -> Result<()>;
    async fn delete_download_log(&self, download: &TrackedDownload) -> Result<()>;
}
