use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::application::ports::DownloadRepositoryPort;
use crate::domain::{CueFile, CueFileStatus, Download, Track};
use crate::store::{self, Repository};

#[derive(Debug, Clone)]
pub struct SqliteDownloadRepository {
    inner: Repository,
}

impl SqliteDownloadRepository {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            inner: Repository::open(data_dir)?,
        })
    }
}

impl DownloadRepositoryPort for SqliteDownloadRepository {
    fn all_downloads(&self) -> Result<Vec<Download>> {
        Ok(self
            .inner
            .all_downloads()?
            .into_iter()
            .map(download_from_store)
            .collect())
    }

    fn upsert_download(&self, download: &Download) -> Result<()> {
        self.inner.upsert_download(&download_to_store(download))?;
        Ok(())
    }

    fn mark_download_complete(
        &self,
        download_id: &str,
        split_complete: bool,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.inner
            .mark_download_complete(download_id, split_complete, last_error)?;
        Ok(())
    }

    fn get_or_create_cue_file(&self, download_id: &str, path: &Path) -> Result<CueFile> {
        Ok(cue_file_from_store(
            self.inner.get_or_create_cue_file(download_id, path)?,
        ))
    }

    fn record_cue_result(
        &self,
        cue_file: &CueFile,
        status: CueFileStatus,
        message: Option<&str>,
        tracks: &[PathBuf],
    ) -> Result<()> {
        self.inner.record_cue_result(
            &cue_file_to_store(cue_file),
            cue_status_to_store(status),
            message,
            tracks,
        )?;
        Ok(())
    }

    fn delete_download(&self, download_id: &str) -> Result<()> {
        self.inner.delete_download(download_id)?;
        Ok(())
    }
}

fn download_from_store(download: store::Download) -> Download {
    Download {
        download_id: download.download_id,
        title: download.title,
        status: download.status,
        output_path: download.output_path,
        tracked_download_state: download.tracked_download_state,
        cue_files: download
            .cue_files
            .into_iter()
            .map(cue_file_from_store)
            .collect(),
        split_complete: download.split_complete,
        last_error: download.last_error,
    }
}

fn download_to_store(download: &Download) -> store::Download {
    store::Download {
        download_id: download.download_id.clone(),
        title: download.title.clone(),
        status: download.status.clone(),
        output_path: download.output_path.clone(),
        tracked_download_state: download.tracked_download_state.clone(),
        cue_files: download
            .cue_files
            .iter()
            .map(cue_file_to_store)
            .collect(),
        split_complete: download.split_complete,
        last_error: download.last_error.clone(),
    }
}

fn cue_file_from_store(cue_file: store::CueFile) -> CueFile {
    CueFile {
        id: cue_file.id,
        download_id: cue_file.download_id,
        path: cue_file.path,
        status: cue_status_from_store(cue_file.status),
        message: cue_file.message,
        tracks: cue_file.tracks.into_iter().map(track_from_store).collect(),
    }
}

fn cue_file_to_store(cue_file: &CueFile) -> store::CueFile {
    store::CueFile {
        id: cue_file.id.clone(),
        download_id: cue_file.download_id.clone(),
        path: cue_file.path.clone(),
        status: cue_status_to_store(cue_file.status),
        message: cue_file.message.clone(),
        tracks: cue_file.tracks.iter().map(track_to_store).collect(),
    }
}

fn track_from_store(track: store::Track) -> Track {
    Track {
        id: track.id,
        cue_file_id: track.cue_file_id,
        download_id: track.download_id,
        path: track.path,
    }
}

fn track_to_store(track: &Track) -> store::Track {
    store::Track {
        id: track.id.clone(),
        cue_file_id: track.cue_file_id.clone(),
        download_id: track.download_id.clone(),
        path: track.path.clone(),
    }
}

fn cue_status_from_store(status: store::CueFileStatus) -> CueFileStatus {
    match status {
        store::CueFileStatus::Pending => CueFileStatus::Pending,
        store::CueFileStatus::Split => CueFileStatus::Split,
        store::CueFileStatus::Skipped => CueFileStatus::Skipped,
        store::CueFileStatus::Failed => CueFileStatus::Failed,
    }
}

fn cue_status_to_store(status: CueFileStatus) -> store::CueFileStatus {
    match status {
        CueFileStatus::Pending => store::CueFileStatus::Pending,
        CueFileStatus::Split => store::CueFileStatus::Split,
        CueFileStatus::Skipped => store::CueFileStatus::Skipped,
        CueFileStatus::Failed => store::CueFileStatus::Failed,
    }
}
