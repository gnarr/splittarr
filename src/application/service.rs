use std::time::Duration;

use anyhow::Result;
use chrono::prelude::*;

use crate::application::cleanup_processed_download::cleanup_processed_download;
use crate::application::monitor_download_queue::classify_downloads;
use crate::application::ports::{
    CueInputInspector, CueScanner, CueSplitter, DownloadStore, QueueSource, TrackCleanup,
};
use crate::application::process_tracked_download::{
    process_tracked_download, register_failed_imports,
};

pub struct MonitorService<Q, S, C, I, P, X> {
    queue_source: Q,
    download_store: S,
    cue_scanner: C,
    cue_input_inspector: I,
    cue_splitter: P,
    track_cleanup: X,
    check_frequency_seconds: u64,
}

impl<Q, S, C, I, P, X> MonitorService<Q, S, C, I, P, X> {
    pub fn new(
        queue_source: Q,
        download_store: S,
        cue_scanner: C,
        cue_input_inspector: I,
        cue_splitter: P,
        track_cleanup: X,
        check_frequency_seconds: u64,
    ) -> Self {
        Self {
            queue_source,
            download_store,
            cue_scanner,
            cue_input_inspector,
            cue_splitter,
            track_cleanup,
            check_frequency_seconds,
        }
    }
}

impl<Q, S, C, I, P, X> MonitorService<Q, S, C, I, P, X>
where
    Q: QueueSource,
    S: DownloadStore,
    C: CueScanner,
    I: CueInputInspector,
    P: CueSplitter,
    X: TrackCleanup,
{
    pub async fn run(&self) -> Result<()> {
        let interval = Duration::from_secs(self.check_frequency_seconds);

        println!("Splittarr");
        println!("Checking every {} seconds", self.check_frequency_seconds);

        loop {
            println!(
                "Checking Lidarr's download queue at {}",
                Local::now().format("%Y-%m-%d %H:%M:%S %z")
            );

            if let Err(err) = self.run_once().await {
                eprintln!("Splittarr cycle failed: {err:#}");
            }

            tokio::time::sleep(interval).await;
        }
    }

    pub async fn run_once(&self) -> Result<()> {
        let mut downloads = self
            .download_store
            .load_tracked_download_summaries()
            .await?;
        println!("{} downloads registered in Splittarr", downloads.len());

        let snapshot = self.queue_source.queue_snapshot().await?;
        println!(
            "Found {} records in Lidarr's download queue",
            snapshot.total_records
        );
        println!(
            "Fetched {} queue page(s) from Lidarr",
            snapshot.pages_fetched
        );

        register_failed_imports(
            &self.download_store,
            &mut downloads,
            &snapshot.failed_imports,
        )
        .await?;

        let (to_process, to_cleanup_candidates) = classify_downloads(downloads, &snapshot);
        let to_cleanup_ids = to_cleanup_candidates
            .into_iter()
            .map(|download| download.download_id)
            .collect::<Vec<_>>();
        let to_cleanup = self
            .download_store
            .get_tracked_downloads(&to_cleanup_ids)
            .await?;

        println!("{} downloads to be processed", to_process.len());

        for download in to_process {
            if let Err(err) = process_tracked_download(
                &self.download_store,
                &self.cue_scanner,
                &self.cue_input_inspector,
                &self.cue_splitter,
                download.clone(),
            )
            .await
            {
                eprintln!("Failed processing {}: {err:#}", download.title);
                let message = err.to_string();
                self.download_store
                    .mark_download_failed(&download.download_id, Some(&message))
                    .await?;
            }
        }

        for download in to_cleanup {
            println!("Cleaning up {}", download.title);
            if let Err(err) =
                cleanup_processed_download(&self.download_store, &self.track_cleanup, &download)
                    .await
            {
                eprintln!("Failed cleaning up {}: {err:#}", download.title);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use tempfile::tempdir;

    use super::MonitorService;
    use crate::adapters::sqlite_download_store::SqliteDownloadStore;
    use crate::application::ports::{
        CueInputInspector, CueInputSnapshot, CueReferencedAudioInput, CueScanner, CueSplitter,
        DownloadStore, QueueSource, TrackCleanup,
    };
    use crate::domain::{
        DiscoveredCueSheets, DownloadLifecycleState, FailedImportCandidate, QueueSnapshot,
        SplitOutcome, SplitStatus, TrackCleanupOutcome, TrackCleanupStatus,
    };

    struct FakeQueue {
        snapshots: Mutex<Vec<QueueSnapshot>>,
    }

    impl QueueSource for FakeQueue {
        async fn queue_snapshot(&self) -> anyhow::Result<QueueSnapshot> {
            let mut snapshots = self.snapshots.lock().unwrap();
            Ok(if snapshots.len() > 1 {
                snapshots.remove(0)
            } else {
                snapshots[0].clone()
            })
        }
    }

    struct FakeScanner {
        cue_path: PathBuf,
    }

    impl CueScanner for FakeScanner {
        async fn find_cue_sheets(&self, _root: &Path) -> anyhow::Result<DiscoveredCueSheets> {
            Ok(DiscoveredCueSheets {
                cue_files: vec![self.cue_path.clone()],
                errors: Vec::new(),
            })
        }
    }

    struct FakeInspector;

    impl CueInputInspector for FakeInspector {
        async fn file_size(&self, path: &Path) -> anyhow::Result<Option<i64>> {
            Ok(fs::metadata(path)
                .ok()
                .and_then(|metadata| i64::try_from(metadata.len()).ok()))
        }

        async fn snapshot_inputs(&self, cue_path: &Path) -> anyhow::Result<CueInputSnapshot> {
            Ok(CueInputSnapshot {
                cue_size_bytes: fs::metadata(cue_path)
                    .ok()
                    .and_then(|metadata| i64::try_from(metadata.len()).ok()),
                audio_inputs: vec![CueReferencedAudioInput {
                    path: cue_path.with_extension("flac"),
                    size_bytes: Some(5),
                }],
            })
        }

        async fn cue_references_audio_file(
            &self,
            cue_path: &Path,
            audio_path: &Path,
        ) -> anyhow::Result<bool> {
            Ok(cue_path.file_stem() == audio_path.file_stem())
        }
    }

    struct FakeSplitter {
        output_track: PathBuf,
    }

    impl CueSplitter for FakeSplitter {
        async fn split_cue(&self, _cue_path: &Path) -> anyhow::Result<SplitOutcome> {
            fs::write(&self.output_track, b"track").unwrap();
            Ok(SplitOutcome {
                status: SplitStatus::Split,
                tracks: vec![self.output_track.clone()],
                message: None,
            })
        }
    }

    struct FakeCleanup;

    impl TrackCleanup for FakeCleanup {
        async fn cleanup_download_tracks(
            &self,
            download: &crate::domain::TrackedDownload,
        ) -> anyhow::Result<Vec<TrackCleanupOutcome>> {
            Ok(download
                .cue_sheets
                .iter()
                .flat_map(|cue| cue.tracks.iter())
                .map(|track| TrackCleanupOutcome {
                    track_id: track.id.clone(),
                    status: TrackCleanupStatus::Deleted,
                    message: None,
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn run_once_processes_then_completes_without_deleting_history() {
        let tmp = tempdir().unwrap();
        let album_dir = tmp.path().join("album");
        fs::create_dir_all(&album_dir).unwrap();
        let cue_path = album_dir.join("album.cue");
        let audio_path = album_dir.join("album.flac");
        let split_track = album_dir.join("01 - Track.flac");
        fs::write(
            &cue_path,
            r#"PERFORMER "Artist"
TITLE "Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Track One"
    PERFORMER "Artist"
    INDEX 01 00:00:00
"#,
        )
        .unwrap();
        fs::write(&audio_path, b"audio").unwrap();

        let snapshot_active = QueueSnapshot {
            total_records: 1,
            pages_fetched: 1,
            active_download_ids: HashSet::from(["download-1".to_owned()]),
            failed_imports: vec![FailedImportCandidate {
                download_id: "download-1".into(),
                title: "Album".into(),
                status: "completed".into(),
                output_path: album_dir.to_string_lossy().to_string(),
                tracked_download_state: "importFailed".into(),
            }],
        };
        let snapshot_gone = QueueSnapshot {
            total_records: 0,
            pages_fetched: 1,
            active_download_ids: HashSet::new(),
            failed_imports: Vec::new(),
        };
        let queue = FakeQueue {
            snapshots: Mutex::new(vec![snapshot_active, snapshot_gone]),
        };
        let store = SqliteDownloadStore::open(tmp.path()).unwrap();
        let service = MonitorService::new(
            queue,
            store.clone(),
            FakeScanner {
                cue_path: cue_path.clone(),
            },
            FakeInspector,
            FakeSplitter {
                output_track: split_track.clone(),
            },
            FakeCleanup,
            60,
        );

        service.run_once().await.unwrap();
        let after_split = store
            .get_tracked_download("download-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after_split.lifecycle_state,
            DownloadLifecycleState::AwaitingImport
        );
        assert_eq!(after_split.generated_track_count(), 1);
        assert_eq!(after_split.input_files.len(), 2);

        service.run_once().await.unwrap();
        let completed = store
            .get_tracked_download("download-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.lifecycle_state, DownloadLifecycleState::Completed);
        assert_eq!(
            completed.cue_sheets[0].tracks[0].cleanup_status,
            TrackCleanupStatus::Deleted
        );
        assert!(completed.completed_at.is_some());
    }
}
