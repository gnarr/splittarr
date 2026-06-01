use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::application::ports::TrackCleanup;
use crate::domain::{CueSheet, GeneratedTrack, TrackedDownload};

#[derive(Debug, Clone, Default)]
pub struct FilesystemTrackCleanup;

impl FilesystemTrackCleanup {
    pub fn new() -> Self {
        Self
    }
}

impl TrackCleanup for FilesystemTrackCleanup {
    async fn cleanup_download_tracks(&self, download: &TrackedDownload) -> Result<()> {
        let download = download.clone();
        tokio::task::spawn_blocking(move || cleanup_download_tracks(&download))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }
}

fn cleanup_download_tracks(download: &TrackedDownload) -> Result<()> {
    let mut errors = Vec::new();

    for cue_sheet in &download.cue_sheets {
        for track in &cue_sheet.tracks {
            let path = cleanup_track_path(cue_sheet, track);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => errors.push(format!("{}: {err}", path.display())),
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("cleanup failed: {}", errors.join("; ")))
    }
}

fn cleanup_track_path(cue_sheet: &CueSheet, track: &GeneratedTrack) -> PathBuf {
    let path = PathBuf::from(&track.path);
    if path.is_absolute() {
        return path;
    }

    Path::new(&cue_sheet.path)
        .parent()
        .map_or(path.clone(), |cue_dir| cue_dir.join(path))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::domain::{CueSheet, CueSheetStatus, GeneratedTrack};

    use super::cleanup_track_path;

    #[test]
    fn cleanup_resolves_legacy_relative_track_paths_from_cue_directory() {
        let cue_sheet = CueSheet {
            id: "cue-1".into(),
            download_id: "download-1".into(),
            path: "/downloads/album/album.cue".into(),
            status: CueSheetStatus::Split,
            message: None,
            tracks: Vec::new(),
        };
        let track = GeneratedTrack {
            id: "track-1".into(),
            cue_sheet_id: "cue-1".into(),
            download_id: "download-1".into(),
            path: "01 - Track.flac".into(),
        };

        assert_eq!(
            cleanup_track_path(&cue_sheet, &track),
            PathBuf::from("/downloads/album/01 - Track.flac")
        );
    }
}
