use anyhow::{Context, Result};

use crate::application::ports::{
    CueScannerPort, CueSplitterPort, DownloadRepositoryPort, GeneratedTrackCleanerPort,
    LidarrQueuePort,
};
use crate::domain::{Download, DownloadCandidate};

use super::{blocking, FailedLidarrImportProcessor};

impl<Repository, Lidarr, Scanner, Splitter, Cleaner>
    FailedLidarrImportProcessor<Repository, Lidarr, Scanner, Splitter, Cleaner>
where
    Repository: DownloadRepositoryPort,
    Lidarr: LidarrQueuePort,
    Scanner: CueScannerPort,
    Splitter: CueSplitterPort,
    Cleaner: GeneratedTrackCleanerPort,
{
    pub(super) async fn register_new_candidates(
        &self,
        downloads: &mut Vec<Download>,
        candidates: Vec<DownloadCandidate>,
    ) -> Result<()> {
        for candidate in candidates {
            if let Some(download) = downloads
                .iter_mut()
                .find(|download| download.download_id == candidate.download_id)
            {
                download.title = candidate.title;
                download.status = candidate.status;
                download.output_path = candidate.output_path;
                download.tracked_download_state = candidate.tracked_download_state;
                self.upsert_download(download.clone(), "update tracked download")
                    .await?;
                continue;
            }

            let download = Download::pending(
                candidate.download_id,
                candidate.title,
                candidate.status,
                candidate.output_path,
                candidate.tracked_download_state,
            );
            self.upsert_download(download.clone(), "store new tracked download")
                .await?;
            downloads.push(download);
        }

        Ok(())
    }

    async fn upsert_download(&self, download: Download, context: &'static str) -> Result<()> {
        let repository = self.repository.clone();
        blocking(move || repository.upsert_download(&download).context(context)).await
    }
}
