use anyhow::{Context, Result};

use crate::application::ports::{DownloadStore, TrackCleanup};
use crate::domain::TrackedDownload;

pub async fn cleanup_processed_download<S: DownloadStore, C: TrackCleanup>(
    store: &S,
    cleanup: &C,
    download: &TrackedDownload,
) -> Result<()> {
    cleanup
        .cleanup_download_tracks(download)
        .await
        .with_context(|| format!("cleanup failed for {}", download.title))?;
    store
        .delete_download(&download.download_id)
        .await
        .context("delete tracked download")
}
