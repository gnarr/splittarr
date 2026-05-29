use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::application::ports::{
    CueScannerPort, CueSplitterPort, DownloadRepositoryPort, GeneratedTrackCleanerPort,
    LidarrQueuePort,
};
use crate::domain::{CueFile, CueFileStatus, Download, SplitResult, SplitStatus};

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
    pub(super) async fn process_download(&self, download: Download) -> Result<()> {
        let output_path = PathBuf::from(&download.output_path);
        let scan = {
            let scanner = self.scanner.clone();
            let output_path = output_path.clone();
            blocking(move || scanner.find_cue_files(&output_path).context("scan download output path"))
                .await?
        };

        for error in &scan.errors {
            eprintln!("Scan warning for {}: {error}", download.title);
        }

        if scan.cue_files.is_empty() {
            self.mark_download_complete(
                download.download_id.clone(),
                false,
                Some("no cue files found".to_owned()),
            )
            .await?;
            return Ok(());
        }

        let mut all_cues_complete = true;
        let mut failures = Vec::new();

        for cue_path in scan.cue_files {
            let cue_file = self
                .get_or_create_cue_file(download.download_id.clone(), cue_path.clone())
                .await?;

            if cue_file.status.is_terminal_success() {
                continue;
            }

            let split_result = {
                let splitter = self.splitter.clone();
                let cue_path = cue_path.clone();
                blocking(move || splitter.split_cue(&cue_path)).await
            };

            match split_result {
                Ok(result) => {
                    self.store_split_result(cue_file, result).await?;
                }
                Err(err) => {
                    all_cues_complete = false;
                    let message = err.to_string();
                    failures.push(format!("{}: {message}", cue_path.display()));
                    self.record_cue_failure(cue_file, message).await?;
                }
            }
        }

        let error_message = if failures.is_empty() {
            None
        } else {
            Some(failures.join("; "))
        };
        self.mark_download_complete(download.download_id.clone(), all_cues_complete, error_message)
            .await?;

        println!("Done processing {}", download.title);
        Ok(())
    }

    pub(super) async fn mark_processing_failed(&self, download_id: &str, message: String) -> Result<()> {
        self.mark_download_complete(download_id.to_owned(), false, Some(message))
            .await
    }

    async fn mark_download_complete(
        &self,
        download_id: String,
        split_complete: bool,
        last_error: Option<String>,
    ) -> Result<()> {
        let repository = self.repository.clone();
        blocking(move || {
            repository
                .mark_download_complete(&download_id, split_complete, last_error.as_deref())
                .context("store download completion state")
        })
        .await
    }

    async fn get_or_create_cue_file(&self, download_id: String, cue_path: PathBuf) -> Result<CueFile> {
        let repository = self.repository.clone();
        blocking(move || {
            repository
                .get_or_create_cue_file(&download_id, &cue_path)
                .context("store cue file")
        })
        .await
    }

    async fn store_split_result(&self, cue_file: CueFile, result: SplitResult) -> Result<()> {
        let status = match result.status {
            SplitStatus::Split => CueFileStatus::Split,
            SplitStatus::Skipped => CueFileStatus::Skipped,
        };
        let repository = self.repository.clone();
        blocking(move || {
            repository
                .record_cue_result(&cue_file, status, result.message.as_deref(), &result.tracks)
                .context("store cue result")
        })
        .await
    }

    async fn record_cue_failure(&self, cue_file: CueFile, message: String) -> Result<()> {
        let repository = self.repository.clone();
        blocking(move || {
            repository
                .record_cue_result(&cue_file, CueFileStatus::Failed, Some(&message), &[])
                .context("store failed cue result")
        })
        .await
    }
}
