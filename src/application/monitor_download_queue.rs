use crate::domain::{QueueSnapshot, TrackedDownload};

pub fn classify_downloads(
    downloads: Vec<TrackedDownload>,
    snapshot: &QueueSnapshot,
) -> (Vec<TrackedDownload>, Vec<TrackedDownload>) {
    let mut to_process = Vec::new();
    let mut to_cleanup = Vec::new();

    for download in downloads {
        if snapshot.active_download_ids.contains(&download.download_id) {
            if download.lifecycle_state.is_ready_for_processing() {
                to_process.push(download);
            }
        } else if !download.lifecycle_state.is_terminal() && download.has_generated_tracks() {
            to_cleanup.push(download);
        }
    }

    (to_process, to_cleanup)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::domain::{DownloadLifecycleState, QueueSnapshot, TrackedDownload};

    use super::classify_downloads;

    #[test]
    fn classifies_downloads_by_queue_presence_and_split_state() {
        let snapshot = QueueSnapshot {
            total_records: 2,
            active_download_ids: HashSet::from([
                "in-queue".to_owned(),
                "awaiting-import".to_owned(),
                "completed".to_owned(),
            ]),
            failed_imports: Vec::new(),
        };
        let awaiting_import = with_state("awaiting-import", DownloadLifecycleState::AwaitingImport);
        let mut completed = with_state("completed", DownloadLifecycleState::Completed);
        completed.cue_sheets = awaiting_import.cue_sheets.clone();
        let gone = with_state("gone", DownloadLifecycleState::AwaitingImport);
        let downloads = vec![download("in-queue"), awaiting_import, completed, gone];

        let (to_process, to_cleanup) = classify_downloads(downloads, &snapshot);

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

    fn download(download_id: &str) -> TrackedDownload {
        let mut download = TrackedDownload::pending(
            download_id.to_owned(),
            download_id.to_owned(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );
        download.cue_sheets = vec![crate::domain::CueSheet {
            id: "cue-1".into(),
            download_id: download_id.to_owned(),
            path: "/downloads/album/album.cue".into(),
            status: crate::domain::CueSheetStatus::Split,
            message: None,
            updated_at: String::new(),
            tracks: vec![crate::domain::GeneratedTrack {
                id: "track-1".into(),
                cue_sheet_id: "cue-1".into(),
                download_id: download_id.to_owned(),
                path: "/downloads/album/01.flac".into(),
                size_bytes: Some(1),
                cleanup_status: crate::domain::TrackCleanupStatus::Pending,
                cleanup_message: None,
                deleted_at: None,
            }],
        }];
        download
    }

    fn with_state(download_id: &str, state: DownloadLifecycleState) -> TrackedDownload {
        let mut download = download(download_id);
        download.lifecycle_state = state;
        download
    }
}
