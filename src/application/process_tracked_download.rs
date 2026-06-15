use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use rcue::parser::parse_from_file;

use crate::application::ports::{
    CueInputInspector, CueMetadataHint, CueScanner, CueSplitter, DownloadStore,
    ManualImportRequest, ManualImportTrigger,
};
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

pub async fn process_tracked_download<S, C, I, P, M>(
    store: &S,
    scanner: &C,
    inspector: &I,
    splitter: &P,
    manual_import: &M,
    download: TrackedDownload,
) -> Result<()>
where
    S: DownloadStore,
    C: CueScanner,
    I: CueInputInspector,
    P: CueSplitter,
    M: ManualImportTrigger,
{
    store
        .mark_download_processing(&download.download_id)
        .await?;
    let output_path = PathBuf::from(&download.output_path);
    let scan_root = scan_root_for(&output_path)?;
    let mut scan = scanner.find_cue_sheets(&scan_root).await?;

    if output_path.is_file() {
        scan.cue_files = inspector
            .filter_cue_files_for_audio(scan.cue_files, &output_path)
            .await?;
    }

    for error in &scan.errors {
        eprintln!("Scan warning for {}: {error}", download.title);
    }

    if scan.cue_files.is_empty() {
        eprintln!("Failed processing {}: no cue files found", download.title);
        store
            .mark_download_failed(&download.download_id, Some("no cue files found"))
            .await?;
        return Ok(());
    }

    let mut all_cues_complete = true;
    let mut failures = Vec::new();
    let mut generated_tracks = Vec::new();
    let mut cue_hints = Vec::new();

    for cue_path in scan.cue_files {
        cue_hints.push(cue_metadata_hint(&cue_path));
        let cue_sheet = store
            .get_or_create_cue_sheet(&download.download_id, &cue_path)
            .await?;
        snapshot_input_files(
            store,
            inspector,
            &download.download_id,
            &cue_sheet,
            &cue_path,
        )
        .await?;

        if cue_sheet.status.is_terminal_success() {
            continue;
        }

        match splitter.split_cue(&cue_path).await {
            Ok(result) => {
                let tracks = result.tracks.clone();
                store_split_result(store, inspector, &cue_sheet, result).await?;
                generated_tracks.extend(tracks);
            }
            Err(err) => {
                all_cues_complete = false;
                let message = err.to_string();
                eprintln!(
                    "Failed splitting cue for {} at {}: {err:#}",
                    download.title,
                    cue_path.display()
                );
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
        if !generated_tracks.is_empty() {
            let request = ManualImportRequest {
                download: download.clone(),
                generated_tracks,
                cue_hints,
            };
            match manual_import.trigger_manual_import(request).await {
                Ok(result) => println!("Manual import for {}: {result:?}", download.title),
                Err(err) => {
                    let message = format!("manual import trigger failed: {err}");
                    eprintln!(
                        "Manual import trigger failed for {}: {err:#}",
                        download.title
                    );
                    store
                        .record_download_warning(&download.download_id, &message)
                        .await?;
                }
            }
        }
    } else {
        store
            .mark_download_failed(&download.download_id, error_message.as_deref())
            .await?;
    }

    println!("Done processing {}", download.title);
    Ok(())
}

fn cue_metadata_hint(cue_path: &Path) -> CueMetadataHint {
    let cue = cue_path
        .to_str()
        .and_then(|path| parse_from_file(path, false).ok());
    let Some(cue) = cue else {
        return CueMetadataHint {
            path: cue_path.to_path_buf(),
            album_title: None,
            performer: None,
            catalog: None,
            comments: Vec::new(),
            track_count: 0,
        };
    };

    CueMetadataHint {
        path: cue_path.to_path_buf(),
        album_title: cue.title,
        performer: cue.performer,
        catalog: cue.catalog,
        comments: cue.comments,
        track_count: cue.files.iter().map(|file| file.tracks.len()).sum(),
    }
}

async fn store_split_result<S: DownloadStore, I: CueInputInspector>(
    store: &S,
    inspector: &I,
    cue_sheet: &CueSheet,
    result: SplitOutcome,
) -> Result<()> {
    let status = match result.status {
        SplitStatus::Split => CueSheetStatus::Split,
        SplitStatus::Skipped => CueSheetStatus::Skipped,
    };
    let mut tracks = Vec::with_capacity(result.tracks.len());
    for path in &result.tracks {
        tracks.push(RecordedTrack {
            path: path.to_string_lossy().to_string(),
            size_bytes: inspector.file_size(path).await?,
        });
    }
    store
        .record_cue_result(cue_sheet, status, result.message.as_deref(), &tracks)
        .await
}

async fn snapshot_input_files<S: DownloadStore, I: CueInputInspector>(
    store: &S,
    inspector: &I,
    download_id: &str,
    cue_sheet: &CueSheet,
    cue_path: &Path,
) -> Result<()> {
    let snapshot = inspector.snapshot_inputs(cue_path).await?;
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use anyhow::Result;
    use tempfile::tempdir;

    use super::process_tracked_download;
    use crate::application::ports::{
        CueInputInspector, CueInputSnapshot, CueReferencedAudioInput, CueScanner, CueSplitter,
        DownloadStore, ManualImportRequest, ManualImportResult, ManualImportTrigger,
    };
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
        warnings: Mutex<Vec<String>>,
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

        async fn record_download_warning(&self, _download_id: &str, message: &str) -> Result<()> {
            self.warnings.lock().unwrap().push(message.to_owned());
            *self.last_error.lock().unwrap() = Some(message.to_owned());
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

    struct FakeInspector {
        matches: Mutex<Vec<(PathBuf, PathBuf)>>,
    }

    impl CueInputInspector for FakeInspector {
        async fn file_size(&self, path: &Path) -> Result<Option<i64>> {
            Ok(fs::metadata(path)
                .ok()
                .and_then(|metadata| i64::try_from(metadata.len()).ok()))
        }

        async fn snapshot_inputs(&self, cue_path: &Path) -> Result<CueInputSnapshot> {
            let cue_size_bytes = fs::metadata(cue_path)
                .ok()
                .and_then(|metadata| i64::try_from(metadata.len()).ok());
            let audio_path = cue_path.with_extension("flac");
            let audio_inputs = if audio_path.exists() {
                vec![CueReferencedAudioInput {
                    path: audio_path,
                    size_bytes: Some(5),
                }]
            } else {
                Vec::new()
            };
            Ok(CueInputSnapshot {
                cue_size_bytes,
                audio_inputs,
            })
        }

        async fn cue_references_audio_file(
            &self,
            cue_path: &Path,
            audio_path: &Path,
        ) -> Result<bool> {
            self.matches
                .lock()
                .unwrap()
                .push((cue_path.to_path_buf(), audio_path.to_path_buf()));
            Ok(cue_path.file_stem() == audio_path.file_stem())
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

    #[derive(Default)]
    struct FakeManualImport {
        calls: Mutex<Vec<ManualImportRequest>>,
        fail: bool,
    }

    impl ManualImportTrigger for FakeManualImport {
        async fn trigger_manual_import(
            &self,
            request: ManualImportRequest,
        ) -> Result<ManualImportResult> {
            self.calls.lock().unwrap().push(request);
            if self.fail {
                return Err(anyhow::anyhow!("lidarr is unavailable"));
            }
            Ok(ManualImportResult::Started {
                imported_track_count: 1,
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
        let inspector = FakeInspector {
            matches: Mutex::new(Vec::new()),
        };
        let splitter = FakeSplitter {
            calls: Mutex::new(Vec::new()),
        };
        let manual_import = FakeManualImport::default();
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Single".into(),
            "completed".into(),
            audio_path.to_string_lossy().to_string(),
            "importFailed".into(),
        );

        process_tracked_download(
            &store,
            &scanner,
            &inspector,
            &splitter,
            &manual_import,
            download,
        )
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
        assert_eq!(manual_import.calls.lock().unwrap().len(), 1);
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
        let inspector = FakeInspector {
            matches: Mutex::new(Vec::new()),
        };
        let splitter = FakeSplitter {
            calls: Mutex::new(Vec::new()),
        };
        let manual_import = FakeManualImport::default();
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Single".into(),
            "completed".into(),
            audio_path.to_string_lossy().to_string(),
            "importFailed".into(),
        );

        process_tracked_download(
            &store,
            &scanner,
            &inspector,
            &splitter,
            &manual_import,
            download,
        )
        .await
        .unwrap();

        assert_eq!(splitter.calls.lock().unwrap().len(), 0);
        assert_eq!(manual_import.calls.lock().unwrap().len(), 0);
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
        let inspector = FakeInspector {
            matches: Mutex::new(Vec::new()),
        };
        let splitter = FakeSplitter {
            calls: Mutex::new(Vec::new()),
        };
        let manual_import = FakeManualImport::default();
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Single".into(),
            "completed".into(),
            target_audio.to_string_lossy().to_string(),
            "importFailed".into(),
        );

        process_tracked_download(
            &store,
            &scanner,
            &inspector,
            &splitter,
            &manual_import,
            download,
        )
        .await
        .unwrap();

        assert_eq!(splitter.calls.lock().unwrap().as_slice(), &[target_cue]);
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
        let inspector = FakeInspector {
            matches: Mutex::new(Vec::new()),
        };
        let splitter = FakeSplitter {
            calls: Mutex::new(Vec::new()),
        };
        let manual_import = FakeManualImport {
            fail: true,
            ..Default::default()
        };
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Broken".into(),
            "completed".into(),
            tmp.path().to_string_lossy().to_string(),
            "importFailed".into(),
        );

        process_tracked_download(
            &store,
            &scanner,
            &inspector,
            &splitter,
            &manual_import,
            download,
        )
        .await
        .unwrap();

        assert_eq!(
            splitter.calls.lock().unwrap().as_slice(),
            &[cue_path.clone()]
        );
        assert_eq!(
            store.recorded_input_files.lock().unwrap().as_slice(),
            &[(cue_path.to_string_lossy().to_string(), InputFileKind::Cue)]
        );
        assert!(store
            .states
            .lock()
            .unwrap()
            .contains(&"awaiting_import".to_string()));
        assert_eq!(
            store.warnings.lock().unwrap().as_slice(),
            &["manual import trigger failed: lidarr is unavailable"]
        );
    }
}
