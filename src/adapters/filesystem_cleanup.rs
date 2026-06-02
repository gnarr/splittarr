use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::application::ports::TrackCleanup;
use crate::domain::{
    CueSheet, GeneratedTrack, TrackCleanupOutcome, TrackCleanupStatus, TrackedDownload,
};

#[derive(Debug, Clone, Default)]
pub struct FilesystemTrackCleanup;

impl FilesystemTrackCleanup {
    pub fn new() -> Self {
        Self
    }
}

impl TrackCleanup for FilesystemTrackCleanup {
    async fn cleanup_download_tracks(
        &self,
        download: &TrackedDownload,
    ) -> Result<Vec<TrackCleanupOutcome>> {
        let download = download.clone();
        tokio::task::spawn_blocking(move || cleanup_download_tracks(&download))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }
}

fn cleanup_download_tracks(download: &TrackedDownload) -> Result<Vec<TrackCleanupOutcome>> {
    let mut outcomes = Vec::new();

    for cue_sheet in &download.cue_sheets {
        for track in &cue_sheet.tracks {
            let path = cleanup_track_path(cue_sheet, track);
            match fs::remove_file(&path) {
                Ok(()) => outcomes.push(TrackCleanupOutcome {
                    track_id: track.id.clone(),
                    status: TrackCleanupStatus::Deleted,
                    message: None,
                }),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    outcomes.push(TrackCleanupOutcome {
                        track_id: track.id.clone(),
                        status: TrackCleanupStatus::Missing,
                        message: Some(format!("{} was already absent", path.display())),
                    })
                }
                Err(err) => outcomes.push(TrackCleanupOutcome {
                    track_id: track.id.clone(),
                    status: TrackCleanupStatus::DeleteFailed,
                    message: Some(format!("{}: {err}", path.display())),
                }),
            }
        }
    }

    Ok(outcomes)
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

    use crate::domain::{CueSheet, CueSheetStatus, GeneratedTrack, TrackCleanupStatus};

    use super::{cleanup_download_tracks, cleanup_track_path};

    #[test]
    fn cleanup_resolves_legacy_relative_track_paths_from_cue_directory() {
        let cue_sheet = CueSheet {
            id: "cue-1".into(),
            download_id: "download-1".into(),
            path: "/downloads/album/album.cue".into(),
            status: CueSheetStatus::Split,
            message: None,
            updated_at: "2024-01-01 00:00:00".into(),
            tracks: Vec::new(),
        };
        let track = GeneratedTrack {
            id: "track-1".into(),
            cue_sheet_id: "cue-1".into(),
            download_id: "download-1".into(),
            path: "01 - Track.flac".into(),
            size_bytes: None,
            cleanup_status: TrackCleanupStatus::Pending,
            cleanup_message: None,
            deleted_at: None,
        };

        assert_eq!(
            cleanup_track_path(&cue_sheet, &track),
            PathBuf::from("/downloads/album/01 - Track.flac")
        );
    }

    #[test]
    fn cleanup_marks_missing_tracks_without_error() {
        let download = crate::domain::TrackedDownload {
            download_id: "download-1".into(),
            title: "Album".into(),
            status: "completed".into(),
            output_path: "/downloads/album".into(),
            tracked_download_state: "importFailed".into(),
            lifecycle_state: crate::domain::DownloadLifecycleState::CleaningUp,
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
            cue_sheets: vec![CueSheet {
                tracks: vec![GeneratedTrack {
                    id: "track-1".into(),
                    cue_sheet_id: "cue-1".into(),
                    download_id: "download-1".into(),
                    path: "/tmp/not-here.flac".into(),
                    size_bytes: None,
                    cleanup_status: TrackCleanupStatus::Pending,
                    cleanup_message: None,
                    deleted_at: None,
                }],
                id: "cue-1".into(),
                path: "/tmp/album.cue".into(),
                download_id: "download-1".into(),
                status: CueSheetStatus::Split,
                message: None,
                updated_at: String::new(),
            }],
            generated_track_count: 1,
            last_error: None,
        };

        let outcomes = cleanup_download_tracks(&download).unwrap();
        assert_eq!(outcomes[0].status, TrackCleanupStatus::Missing);
    }
}
