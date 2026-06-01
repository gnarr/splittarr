use crate::domain::{QueueSnapshot, TrackedDownload};

pub fn classify_downloads(
    downloads: Vec<TrackedDownload>,
    snapshot: &QueueSnapshot,
) -> (Vec<TrackedDownload>, Vec<TrackedDownload>) {
    let mut to_process = Vec::new();
    let mut to_cleanup = Vec::new();

    for download in downloads {
        if snapshot.active_download_ids.contains(&download.download_id) {
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
    use std::collections::HashSet;

    use crate::domain::{QueueSnapshot, TrackedDownload};

    use super::classify_downloads;

    #[test]
    fn classifies_downloads_by_queue_presence_and_split_state() {
        let snapshot = QueueSnapshot {
            total_records: 2,
            active_download_ids: HashSet::from(["in-queue".to_owned(), "processed".to_owned()]),
            failed_imports: Vec::new(),
        };
        let mut processed = download("processed");
        processed.split_complete = true;
        let downloads = vec![download("in-queue"), processed, download("gone")];

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
        TrackedDownload::pending(
            download_id.to_owned(),
            download_id.to_owned(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        )
    }
}
