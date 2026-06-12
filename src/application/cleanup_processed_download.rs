use anyhow::{Context, Result};

use crate::application::ports::{DownloadStore, TrackCleanup};
use crate::domain::{TrackCleanupStatus, TrackedDownload};

pub async fn cleanup_processed_download<S: DownloadStore, C: TrackCleanup>(
    store: &S,
    cleanup: &C,
    download: &TrackedDownload,
) -> Result<()> {
    store
        .mark_download_cleanup_started(&download.download_id)
        .await
        .with_context(|| format!("mark cleanup started for {}", download.title))?;
    let outcomes = match cleanup.cleanup_download_tracks(download).await {
        Ok(outcomes) => outcomes,
        Err(err) => {
            let message = format!("cleanup failed for {}: {err}", download.title);
            store
                .mark_download_failed(&download.download_id, Some(&message))
                .await
                .context("mark download failed after cleanup error")?;
            return Err(err).with_context(|| format!("cleanup failed for {}", download.title));
        }
    };

    let mut failures = Vec::new();
    for outcome in &outcomes {
        if outcome.status == TrackCleanupStatus::DeleteFailed {
            failures.push(
                outcome
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("track cleanup failed: {}", outcome.track_id)),
            );
        }
    }
    store
        .record_track_cleanups(&download.download_id, &outcomes)
        .await
        .with_context(|| format!("record cleanup results for {}", download.title))?;

    if failures.is_empty() {
        store
            .mark_download_completed(&download.download_id)
            .await
            .context("mark download completed")?;
    } else {
        store
            .mark_download_failed(&download.download_id, Some(&failures.join("; ")))
            .await
            .context("mark download failed after cleanup")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use anyhow::Result;

    use super::cleanup_processed_download;
    use crate::application::ports::{DownloadStore, TrackCleanup};
    use crate::domain::{
        CueSheet, CueSheetStatus, DownloadLifecycleState, InputFileKind, RecordedTrack,
        TrackCleanupOutcome, TrackCleanupStatus, TrackedDownload,
    };

    #[derive(Default)]
    struct FakeStore {
        states: Mutex<Vec<String>>,
        last_error: Mutex<Option<String>>,
    }

    impl DownloadStore for FakeStore {
        async fn load_tracked_downloads(&self) -> Result<Vec<TrackedDownload>> {
            Ok(Vec::new())
        }

        async fn get_tracked_download(
            &self,
            _download_id: &str,
        ) -> Result<Option<TrackedDownload>> {
            Ok(None)
        }

        async fn upsert_tracked_download(&self, _download: &TrackedDownload) -> Result<()> {
            Ok(())
        }

        async fn mark_download_processing(&self, _download_id: &str) -> Result<()> {
            Ok(())
        }

        async fn mark_download_awaiting_import(&self, _download_id: &str) -> Result<()> {
            Ok(())
        }

        async fn mark_download_cleanup_started(&self, _download_id: &str) -> Result<()> {
            self.states.lock().unwrap().push("cleaning_up".into());
            Ok(())
        }

        async fn mark_download_completed(&self, _download_id: &str) -> Result<()> {
            self.states.lock().unwrap().push("completed".into());
            Ok(())
        }

        async fn mark_download_failed(
            &self,
            _download_id: &str,
            last_error: Option<&str>,
        ) -> Result<()> {
            self.states.lock().unwrap().push("failed".into());
            *self.last_error.lock().unwrap() = last_error.map(str::to_owned);
            Ok(())
        }

        async fn get_or_create_cue_sheet(
            &self,
            _download_id: &str,
            _path: &Path,
        ) -> Result<CueSheet> {
            unreachable!()
        }

        async fn record_input_file(
            &self,
            _download_id: &str,
            _cue_sheet_id: Option<&str>,
            _path: &Path,
            _kind: InputFileKind,
            _size_bytes: Option<i64>,
        ) -> Result<()> {
            Ok(())
        }

        async fn record_cue_result(
            &self,
            _cue_sheet: &CueSheet,
            _status: CueSheetStatus,
            _message: Option<&str>,
            _tracks: &[RecordedTrack],
        ) -> Result<()> {
            Ok(())
        }

        async fn record_track_cleanup(
            &self,
            _download_id: &str,
            _track_id: &str,
            _status: TrackCleanupStatus,
            _message: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }
    }

    struct FakeCleanup;

    impl TrackCleanup for FakeCleanup {
        async fn cleanup_download_tracks(
            &self,
            _download: &TrackedDownload,
        ) -> Result<Vec<TrackCleanupOutcome>> {
            Ok(vec![TrackCleanupOutcome {
                track_id: "track-1".into(),
                status: TrackCleanupStatus::DeleteFailed,
                message: None,
            }])
        }
    }

    struct FailingCleanup;

    impl TrackCleanup for FailingCleanup {
        async fn cleanup_download_tracks(
            &self,
            _download: &TrackedDownload,
        ) -> Result<Vec<TrackCleanupOutcome>> {
            anyhow::bail!("filesystem unavailable");
        }
    }

    #[tokio::test]
    async fn delete_failed_without_message_marks_download_failed() {
        let store = FakeStore::default();
        let cleanup = FakeCleanup;
        let download = TrackedDownload {
            download_id: "download-1".into(),
            title: "Album".into(),
            status: "completed".into(),
            output_path: "/downloads/album".into(),
            tracked_download_state: "importFailed".into(),
            lifecycle_state: DownloadLifecycleState::AwaitingImport,
            created_at: String::new(),
            updated_at: String::new(),
            first_seen_at: None,
            last_seen_in_queue_at: None,
            processing_started_at: None,
            processing_finished_at: None,
            cleanup_started_at: None,
            cleanup_finished_at: None,
            completed_at: None,
            input_files: Vec::new(),
            cue_sheets: Vec::new(),
            generated_track_count: 0,
            last_error: None,
        };

        cleanup_processed_download(&store, &cleanup, &download)
            .await
            .unwrap();

        assert!(store.states.lock().unwrap().contains(&"failed".to_string()));
        assert_eq!(
            store.last_error.lock().unwrap().as_deref(),
            Some("track cleanup failed: track-1")
        );
    }

    #[tokio::test]
    async fn cleanup_error_marks_download_failed() {
        let store = FakeStore::default();
        let cleanup = FailingCleanup;
        let download = TrackedDownload {
            download_id: "download-1".into(),
            title: "Album".into(),
            status: "completed".into(),
            output_path: "/downloads/album".into(),
            tracked_download_state: "importFailed".into(),
            lifecycle_state: DownloadLifecycleState::AwaitingImport,
            created_at: String::new(),
            updated_at: String::new(),
            first_seen_at: None,
            last_seen_in_queue_at: None,
            processing_started_at: None,
            processing_finished_at: None,
            cleanup_started_at: None,
            cleanup_finished_at: None,
            completed_at: None,
            input_files: Vec::new(),
            cue_sheets: Vec::new(),
            generated_track_count: 0,
            last_error: None,
        };

        let err = cleanup_processed_download(&store, &cleanup, &download)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("cleanup failed for Album"));
        assert!(store.states.lock().unwrap().contains(&"failed".to_string()));
        assert_eq!(
            store.last_error.lock().unwrap().as_deref(),
            Some("cleanup failed for Album: filesystem unavailable")
        );
    }
}
