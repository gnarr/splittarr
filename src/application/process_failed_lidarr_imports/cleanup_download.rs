use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::application::ports::{
    CueScannerPort, CueSplitterPort, DownloadRepositoryPort, GeneratedTrackCleanerPort,
    LidarrQueuePort,
};
use crate::domain::{CueFile, Download, Track};

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
    pub(super) async fn cleanup_download(&self, download: Download) -> Result<()> {
        let cleaner = self.cleaner.clone();
        let repository = self.repository.clone();
        blocking(move || {
            let mut errors = Vec::new();

            for cue_file in &download.cue_files {
                for track in &cue_file.tracks {
                    let path = cleanup_track_path(cue_file, track);
                    if let Err(err) = cleaner.remove_generated_track(&path) {
                        errors.push(format!("{}: {err}", path.display()));
                    }
                }
            }

            if !errors.is_empty() {
                return Err(anyhow!("cleanup failed: {}", errors.join("; ")));
            }

            repository
                .delete_download(&download.download_id)
                .context("delete tracked download")
        })
        .await
    }
}

pub fn cleanup_track_path(cue_file: &CueFile, track: &Track) -> PathBuf {
    let path = PathBuf::from(&track.path);
    if path.is_absolute() {
        return path;
    }

    Path::new(&cue_file.path)
        .parent()
        .map_or(path.clone(), |cue_dir| cue_dir.join(path))
}
