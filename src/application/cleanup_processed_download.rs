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
    let outcomes = cleanup
        .cleanup_download_tracks(download)
        .await
        .with_context(|| format!("cleanup failed for {}", download.title))?;

    let mut failures = Vec::new();
    for outcome in &outcomes {
        if outcome.status == TrackCleanupStatus::DeleteFailed {
            if let Some(message) = &outcome.message {
                failures.push(message.clone());
            }
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
