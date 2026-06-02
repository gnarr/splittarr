use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use rcue::parser::parse_from_file;
use tokio::task;

use crate::application::ports::{CueScanner, CueSplitter, DownloadStore};
use crate::domain::{
    CueSheet, CueSheetStatus, FailedImportCandidate, InputFileKind, RecordedTrack, SplitOutcome,
    SplitStatus, TrackedDownload,
};

pub async fn register_failed_imports<S: DownloadStore>(
    store: &S,
    downloads: &mut Vec<TrackedDownload>,
    candidates: &[FailedImportCandidate],
) -> Result<()> {
    for candidate in candidates {
        if let Some(download) = downloads
            .iter_mut()
            .find(|download| download.download_id == candidate.download_id)
        {
            download.title = candidate.title.clone();
            download.status = candidate.status.clone();
            download.output_path = candidate.output_path.clone();
            download.tracked_download_state = candidate.tracked_download_state.clone();
            store.upsert_tracked_download(download).await?;
            continue;
        }

        let download = TrackedDownload::pending(
            candidate.download_id.clone(),
            candidate.title.clone(),
            candidate.status.clone(),
            candidate.output_path.clone(),
            candidate.tracked_download_state.clone(),
        );
        store.upsert_tracked_download(&download).await?;
        downloads.push(download);
    }

    Ok(())
}

pub async fn process_tracked_download<S, C, P>(
    store: &S,
    scanner: &C,
    splitter: &P,
    download: TrackedDownload,
) -> Result<()>
where
    S: DownloadStore,
    C: CueScanner,
    P: CueSplitter,
{
    store
        .mark_download_processing(&download.download_id)
        .await?;
    let output_path = PathBuf::from(&download.output_path);
    let scan_root = scan_root_for(&output_path)?;
    let mut scan = scanner.find_cue_sheets(&scan_root).await?;

    if output_path.is_file() {
        scan.cue_files = filter_cue_files_for_audio(scan.cue_files, output_path.clone()).await?;
    }

    for error in &scan.errors {
        eprintln!("Scan warning for {}: {error}", download.title);
    }

    if scan.cue_files.is_empty() {
        store
            .mark_download_failed(&download.download_id, Some("no cue files found"))
            .await?;
        return Ok(());
    }

    let mut all_cues_complete = true;
    let mut failures = Vec::new();

    for cue_path in scan.cue_files {
        let cue_sheet = store
            .get_or_create_cue_sheet(&download.download_id, &cue_path)
            .await?;
        snapshot_input_files(store, &download.download_id, &cue_sheet, &cue_path).await?;

        if cue_sheet.status.is_terminal_success() {
            continue;
        }

        match splitter.split_cue(&cue_path).await {
            Ok(result) => {
                store_split_result(store, &cue_sheet, result).await?;
            }
            Err(err) => {
                all_cues_complete = false;
                let message = err.to_string();
                failures.push(format!("{}: {message}", cue_path.display()));
                store
                    .record_cue_result(&cue_sheet, CueSheetStatus::Failed, Some(&message), &[])
                    .await?;
            }
        }
    }

    let error_message = if failures.is_empty() {
        None
    } else {
        Some(failures.join("; "))
    };
    if all_cues_complete {
        store
            .mark_download_awaiting_import(&download.download_id)
            .await?;
    } else {
        store
            .mark_download_failed(&download.download_id, error_message.as_deref())
            .await?;
    }

    println!("Done processing {}", download.title);
    Ok(())
}

async fn store_split_result<S: DownloadStore>(
    store: &S,
    cue_sheet: &CueSheet,
    result: SplitOutcome,
) -> Result<()> {
    let status = match result.status {
        SplitStatus::Split => CueSheetStatus::Split,
        SplitStatus::Skipped => CueSheetStatus::Skipped,
    };
    let tracks = result
        .tracks
        .iter()
        .map(|path| RecordedTrack {
            path: path.to_string_lossy().to_string(),
            size_bytes: file_size(path),
        })
        .collect::<Vec<_>>();
    store
        .record_cue_result(cue_sheet, status, result.message.as_deref(), &tracks)
        .await
}

async fn snapshot_input_files<S: DownloadStore>(
    store: &S,
    download_id: &str,
    cue_sheet: &CueSheet,
    cue_path: &Path,
) -> Result<()> {
    let snapshot = snapshot_input_file_details(cue_path.to_path_buf()).await?;
    store
        .record_input_file(
            download_id,
            Some(&cue_sheet.id),
            cue_path,
            InputFileKind::Cue,
            snapshot.cue_size_bytes,
        )
        .await?;

    for input in snapshot.audio_inputs {
        store
            .record_input_file(
                download_id,
                Some(&cue_sheet.id),
                &input.path,
                InputFileKind::Audio,
                input.size_bytes,
            )
            .await?;
    }

    Ok(())
}

struct SnapshotInputDetails {
    cue_size_bytes: Option<i64>,
    audio_inputs: Vec<SnapshotAudioInput>,
}

struct SnapshotAudioInput {
    path: PathBuf,
    size_bytes: Option<i64>,
}

async fn snapshot_input_file_details(cue_path: PathBuf) -> Result<SnapshotInputDetails> {
    task::spawn_blocking(move || snapshot_input_file_details_sync(&cue_path))
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
}

fn snapshot_input_file_details_sync(cue_path: &Path) -> Result<SnapshotInputDetails> {
    let cue_size_bytes = file_size(cue_path);
    let cue_path_str = cue_path.to_string_lossy();
    let cue = match parse_from_file(&cue_path_str, false) {
        Ok(cue) => cue,
        Err(err) => {
            eprintln!(
                "Unable to parse cue file for input snapshot {}: {err}",
                cue_path.display()
            );
            return Ok(SnapshotInputDetails {
                cue_size_bytes,
                audio_inputs: Vec::new(),
            });
        }
    };
    let cue_dir = cue_path.parent().unwrap_or_else(|| Path::new("."));
    let audio_inputs = cue
        .files
        .iter()
        .map(|file| cue_dir.join(&file.file))
        .filter_map(|path| {
            path.exists().then(|| SnapshotAudioInput {
                size_bytes: file_size(&path),
                path,
            })
        })
        .collect();

    Ok(SnapshotInputDetails {
        cue_size_bytes,
        audio_inputs,
    })
}

fn file_size(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| i64::try_from(metadata.len()).ok())
}

fn scan_root_for(output_path: &Path) -> Result<PathBuf> {
    if output_path.is_dir() {
        return Ok(output_path.to_path_buf());
    }
    if output_path.is_file() {
        return output_path.parent().map(Path::to_path_buf).ok_or_else(|| {
            anyhow!(
                "download output file has no parent directory: {}",
                output_path.display()
            )
        });
    }
    Ok(output_path.to_path_buf())
}

fn cue_references_audio_file(cue_path: &Path, audio_path: &Path) -> bool {
    let cue = match parse_from_file(&cue_path.to_string_lossy(), false) {
        Ok(cue) => cue,
        Err(err) => {
            eprintln!(
                "Unable to parse cue file while matching audio {}: {err}",
                cue_path.display()
            );
            return false;
        }
    };
    let cue_dir = cue_path.parent().unwrap_or_else(|| Path::new("."));

    cue.files
        .iter()
        .map(|file| cue_dir.join(&file.file))
        .any(|candidate| candidate == audio_path)
}

async fn filter_cue_files_for_audio(
    cue_files: Vec<PathBuf>,
    audio_path: PathBuf,
) -> Result<Vec<PathBuf>> {
    task::spawn_blocking(move || {
        Ok(cue_files
            .into_iter()
            .filter(|cue_path| cue_references_audio_file(cue_path, &audio_path))
            .collect())
    })
    .await
    .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use anyhow::Result;
    use tempfile::tempdir;

    use super::{cue_references_audio_file, process_tracked_download};
    use crate::application::ports::{CueScanner, CueSplitter, DownloadStore};
    use crate::domain::{
        CueSheet, CueSheetStatus, DiscoveredCueSheets, InputFileKind, RecordedTrack, SplitOutcome,
        SplitStatus, TrackCleanupStatus, TrackedDownload,
    };

    #[derive(Default)]
    struct FakeStore {
        states: Mutex<Vec<String>>,
        last_error: Mutex<Option<String>>,
        recorded_cues: Mutex<Vec<String>>,
        recorded_input_files: Mutex<Vec<(String, InputFileKind)>>,
        recorded_tracks: Mutex<Vec<String>>,
    }

    impl DownloadStore for FakeStore {
        async fn load_tracked_downloads(&self) -> Result<Vec<TrackedDownload>> {
            Ok(Vec::new())
        }

        async fn get_tracked_download(
            &self,
            _download_id: &str,
        ) -> Result<Option<TrackedDownload>> {
            Ok(None)
        }

        async fn upsert_tracked_download(&self, _download: &TrackedDownload) -> Result<()> {
            Ok(())
        }

        async fn mark_download_processing(&self, _download_id: &str) -> Result<()> {
            self.states.lock().unwrap().push("processing".into());
            Ok(())
        }

        async fn mark_download_awaiting_import(&self, _download_id: &str) -> Result<()> {
            self.states.lock().unwrap().push("awaiting_import".into());
            Ok(())
        }

        async fn mark_download_cleanup_started(&self, _download_id: &str) -> Result<()> {
            Ok(())
        }

        async fn mark_download_completed(&self, _download_id: &str) -> Result<()> {
            Ok(())
        }

        async fn mark_download_failed(
            &self,
            _download_id: &str,
            last_error: Option<&str>,
        ) -> Result<()> {
            self.states.lock().unwrap().push("failed".into());
            *self.last_error.lock().unwrap() = last_error.map(str::to_owned);
            Ok(())
        }

        async fn get_or_create_cue_sheet(
            &self,
            download_id: &str,
            path: &Path,
        ) -> Result<CueSheet> {
            self.recorded_cues
                .lock()
                .unwrap()
                .push(path.to_string_lossy().to_string());
            Ok(CueSheet {
                id: format!("cue:{}", path.display()),
                download_id: download_id.to_owned(),
                path: path.to_string_lossy().to_string(),
                status: CueSheetStatus::Pending,
                message: None,
                updated_at: "now".into(),
                tracks: Vec::new(),
            })
        }

        async fn record_input_file(
            &self,
            _download_id: &str,
            _cue_sheet_id: Option<&str>,
            path: &Path,
            kind: InputFileKind,
            _size_bytes: Option<i64>,
        ) -> Result<()> {
            self.recorded_input_files
                .lock()
                .unwrap()
                .push((path.to_string_lossy().to_string(), kind));
            Ok(())
        }

        async fn record_cue_result(
            &self,
            _cue_sheet: &CueSheet,
            status: CueSheetStatus,
            message: Option<&str>,
            tracks: &[RecordedTrack],
        ) -> Result<()> {
            if status == CueSheetStatus::Split {
                self.recorded_tracks
                    .lock()
                    .unwrap()
                    .extend(tracks.iter().map(|track| track.path.clone()));
            }
            if status == CueSheetStatus::Failed {
                *self.last_error.lock().unwrap() = message.map(str::to_owned);
            }
            Ok(())
        }

        async fn record_track_cleanup(
            &self,
            _download_id: &str,
            _track_id: &str,
            _status: TrackCleanupStatus,
            _message: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }
    }

    struct FakeScanner {
        roots: Mutex<Vec<PathBuf>>,
        cue_files: Vec<PathBuf>,
    }

    impl CueScanner for FakeScanner {
        async fn find_cue_sheets(&self, root: &Path) -> Result<DiscoveredCueSheets> {
            self.roots.lock().unwrap().push(root.to_path_buf());
            Ok(DiscoveredCueSheets {
                cue_files: self.cue_files.clone(),
                errors: Vec::new(),
            })
        }
    }

    struct FakeSplitter {
        calls: Mutex<Vec<PathBuf>>,
    }

    impl CueSplitter for FakeSplitter {
        async fn split_cue(&self, cue_path: &Path) -> Result<SplitOutcome> {
            self.calls.lock().unwrap().push(cue_path.to_path_buf());
            Ok(SplitOutcome {
                status: SplitStatus::Split,
                tracks: vec![cue_path.with_file_name("01 - Track.flac")],
                message: None,
            })
        }
    }

    #[tokio::test]
    async fn processes_file_output_path_using_parent_directory_and_matching_cue() {
        let tmp = tempdir().unwrap();
        let audio_path = tmp.path().join("single.flac");
        let cue_path = tmp.path().join("single.cue");
        fs::write(&audio_path, b"audio").unwrap();
        fs::write(
            &cue_path,
            "FILE \"single.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let store = FakeStore::default();
        let scanner = FakeScanner {
            roots: Mutex::new(Vec::new()),
            cue_files: vec![cue_path.clone()],
        };
        let splitter = FakeSplitter {
            calls: Mutex::new(Vec::new()),
        };
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Single".into(),
            "completed".into(),
            audio_path.to_string_lossy().to_string(),
            "importFailed".into(),
        );

        process_tracked_download(&store, &scanner, &splitter, download)
            .await
            .unwrap();

        assert_eq!(
            scanner.roots.lock().unwrap().as_slice(),
            &[tmp.path().to_path_buf()]
        );
        assert_eq!(splitter.calls.lock().unwrap().as_slice(), &[cue_path]);
        assert!(store
            .states
            .lock()
            .unwrap()
            .contains(&"awaiting_import".to_string()));
    }

    #[tokio::test]
    async fn file_output_path_without_matching_cue_fails_with_no_cue_files_found() {
        let tmp = tempdir().unwrap();
        let audio_path = tmp.path().join("single.flac");
        let cue_path = tmp.path().join("other.cue");
        fs::write(&audio_path, b"audio").unwrap();
        fs::write(
            &cue_path,
            "FILE \"other.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let store = FakeStore::default();
        let scanner = FakeScanner {
            roots: Mutex::new(Vec::new()),
            cue_files: vec![cue_path],
        };
        let splitter = FakeSplitter {
            calls: Mutex::new(Vec::new()),
        };
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Single".into(),
            "completed".into(),
            audio_path.to_string_lossy().to_string(),
            "importFailed".into(),
        );

        process_tracked_download(&store, &scanner, &splitter, download)
            .await
            .unwrap();

        assert_eq!(splitter.calls.lock().unwrap().len(), 0);
        assert_eq!(
            store.last_error.lock().unwrap().as_deref(),
            Some("no cue files found")
        );
    }

    #[tokio::test]
    async fn file_output_path_ignores_unrelated_cues_in_same_directory() {
        let tmp = tempdir().unwrap();
        let target_audio = tmp.path().join("target.flac");
        let other_audio = tmp.path().join("other.flac");
        let target_cue = tmp.path().join("target.cue");
        let other_cue = tmp.path().join("other.cue");
        fs::write(&target_audio, b"audio").unwrap();
        fs::write(&other_audio, b"audio").unwrap();
        fs::write(
            &target_cue,
            "FILE \"target.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        fs::write(
            &other_cue,
            "FILE \"other.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let store = FakeStore::default();
        let scanner = FakeScanner {
            roots: Mutex::new(Vec::new()),
            cue_files: vec![other_cue, target_cue.clone()],
        };
        let splitter = FakeSplitter {
            calls: Mutex::new(Vec::new()),
        };
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Single".into(),
            "completed".into(),
            target_audio.to_string_lossy().to_string(),
            "importFailed".into(),
        );

        process_tracked_download(&store, &scanner, &splitter, download)
            .await
            .unwrap();

        assert_eq!(splitter.calls.lock().unwrap().as_slice(), &[target_cue]);
    }

    #[test]
    fn cue_matching_checks_exact_referenced_audio_file() {
        let tmp = tempdir().unwrap();
        let target_audio = tmp.path().join("target.flac");
        let target_cue = tmp.path().join("target.cue");
        let other_audio = tmp.path().join("other.flac");
        fs::write(&target_audio, b"audio").unwrap();
        fs::write(
            &target_cue,
            "FILE \"target.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        assert!(cue_references_audio_file(&target_cue, &target_audio));
        assert!(!cue_references_audio_file(&target_cue, &other_audio));
    }

    #[tokio::test]
    async fn invalid_cue_snapshot_records_cue_file_and_continues_processing() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("broken.cue");
        fs::write(&cue_path, "this is not a valid cue sheet").unwrap();

        let store = FakeStore::default();
        let scanner = FakeScanner {
            roots: Mutex::new(Vec::new()),
            cue_files: vec![cue_path.clone()],
        };
        let splitter = FakeSplitter {
            calls: Mutex::new(Vec::new()),
        };
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Broken".into(),
            "completed".into(),
            tmp.path().to_string_lossy().to_string(),
            "importFailed".into(),
        );

        process_tracked_download(&store, &scanner, &splitter, download)
            .await
            .unwrap();

        assert_eq!(splitter.calls.lock().unwrap().as_slice(), &[cue_path.clone()]);
        assert_eq!(
            store.recorded_input_files.lock().unwrap().as_slice(),
            &[(cue_path.to_string_lossy().to_string(), InputFileKind::Cue)]
        );
        assert!(store
            .states
            .lock()
            .unwrap()
            .contains(&"awaiting_import".to_string()));
    }
}
