use std::collections::HashSet;

use anyhow::{Context, Result};

use crate::application::ports::{
    CueScannerPort, CueSplitterPort, DownloadRepositoryPort, GeneratedTrackCleanerPort,
    LidarrQueuePort,
};
use crate::domain::Download;

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
    pub async fn run_once(&self) -> Result<()> {
        let repository = self.repository.clone();
        let mut downloads = blocking(move || {
            repository
                .all_downloads()
                .context("load tracked downloads from database")
        })
        .await?;

        println!("{} downloads registered in Splittarr", downloads.len());

        let queue = self
            .lidarr
            .queue_snapshot()
            .await
            .context("load Lidarr queue")?;
        println!("Found {} records in Lidarr's download queue", queue.record_count);

        self.register_new_candidates(&mut downloads, queue.candidates)
            .await?;

        let (to_process, to_cleanup) = classify_downloads(downloads, &queue.download_ids);

        println!("{} downloads to be processed", to_process.len());

        for download in to_process {
            if let Err(err) = self.process_download(download.clone()).await {
                eprintln!("Failed processing {}: {err:#}", download.title);
                self.mark_processing_failed(&download.download_id, err.to_string())
                    .await?;
            }
        }

        for download in to_cleanup {
            println!("Cleaning up {}", download.title);
            if let Err(err) = self.cleanup_download(download.clone()).await {
                eprintln!("Failed cleaning up {}: {err:#}", download.title);
            }
        }

        Ok(())
    }
}

pub fn classify_downloads(
    downloads: Vec<Download>,
    queue_ids: &HashSet<String>,
) -> (Vec<Download>, Vec<Download>) {
    let mut to_process = Vec::new();
    let mut to_cleanup = Vec::new();

    for download in downloads {
        if queue_ids.contains(&download.download_id) {
            if !download.split_complete {
                to_process.push(download);
            }
        } else {
            to_cleanup.push(download);
        }
    }

    (to_process, to_cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_downloads_by_queue_presence_and_split_state() {
        let queue_ids = HashSet::from(["in-queue".to_owned(), "processed".to_owned()]);
        let mut processed = download("processed");
        processed.split_complete = true;
        let downloads = vec![download("in-queue"), processed, download("gone")];

        let (to_process, to_cleanup) = classify_downloads(downloads, &queue_ids);

        assert_eq!(
            to_process
                .iter()
                .map(|download| download.download_id.as_str())
                .collect::<Vec<_>>(),
            vec!["in-queue"]
        );
        assert_eq!(
            to_cleanup
                .iter()
                .map(|download| download.download_id.as_str())
                .collect::<Vec<_>>(),
            vec!["gone"]
        );
    }

    fn download(download_id: &str) -> Download {
        Download::pending(
            download_id.to_owned(),
            download_id.to_owned(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        )
    }
}
