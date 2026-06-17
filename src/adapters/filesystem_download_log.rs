use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::application::ports::DownloadLog;
use crate::domain::TrackedDownload;

#[derive(Debug, Clone)]
pub struct FilesystemDownloadLog {
    enabled: bool,
}

impl FilesystemDownloadLog {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl DownloadLog for FilesystemDownloadLog {
    async fn write_download_log(&self, download: &TrackedDownload, content: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let path = download_log_path(download);
        let display_path = path.display().to_string();
        let content = content.to_owned();
        tokio::task::spawn_blocking(move || fs::write(&path, content))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
            .map_err(|err| anyhow!("failed writing {display_path}: {err}"))
    }

    async fn delete_download_log(&self, download: &TrackedDownload) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let path = download_log_path(download);
        tokio::task::spawn_blocking(move || match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(anyhow!("failed deleting {}: {err}", path.display())),
        })
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }
}

fn download_log_path(download: &TrackedDownload) -> PathBuf {
    download_root_path(&PathBuf::from(&download.output_path)).join("splittarr.log")
}

fn download_root_path(output_path: &Path) -> PathBuf {
    if output_path.is_file() {
        return output_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| output_path.to_path_buf());
    }

    output_path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{download_log_path, FilesystemDownloadLog};
    use crate::application::ports::DownloadLog;
    use crate::domain::TrackedDownload;

    #[tokio::test]
    async fn writes_log_to_download_directory() {
        let tmp = tempdir().unwrap();
        let download = download(tmp.path().to_string_lossy().to_string());
        let logger = FilesystemDownloadLog::new(true);

        logger.write_download_log(&download, "hello").await.unwrap();

        assert_eq!(
            fs::read_to_string(tmp.path().join("splittarr.log")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn writes_log_to_parent_when_output_path_is_a_file() {
        let tmp = tempdir().unwrap();
        let audio_path = tmp.path().join("album.flac");
        fs::write(&audio_path, b"audio").unwrap();
        let download = download(audio_path.to_string_lossy().to_string());
        let logger = FilesystemDownloadLog::new(true);

        logger.write_download_log(&download, "hello").await.unwrap();

        assert_eq!(
            fs::read_to_string(tmp.path().join("splittarr.log")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn disabled_logger_does_not_write() {
        let tmp = tempdir().unwrap();
        let download = download(tmp.path().to_string_lossy().to_string());
        let logger = FilesystemDownloadLog::new(false);

        logger.write_download_log(&download, "hello").await.unwrap();

        assert!(!tmp.path().join("splittarr.log").exists());
    }

    #[tokio::test]
    async fn deletes_existing_log_and_ignores_missing_log() {
        let tmp = tempdir().unwrap();
        let download = download(tmp.path().to_string_lossy().to_string());
        fs::write(download_log_path(&download), "hello").unwrap();
        let logger = FilesystemDownloadLog::new(true);

        logger.delete_download_log(&download).await.unwrap();
        logger.delete_download_log(&download).await.unwrap();

        assert!(!tmp.path().join("splittarr.log").exists());
    }

    fn download(output_path: String) -> TrackedDownload {
        TrackedDownload::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            output_path,
            "importFailed".into(),
        )
    }
}
