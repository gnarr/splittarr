use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use rcue::parser::parse_from_file;

use crate::application::ports::{CueInputInspector, CueInputSnapshot, CueReferencedAudioInput};

#[derive(Debug, Clone, Copy, Default)]
pub struct FilesystemCueInputInspector;

impl FilesystemCueInputInspector {
    pub fn new() -> Self {
        Self
    }
}

impl CueInputInspector for FilesystemCueInputInspector {
    async fn file_size(&self, path: &Path) -> Result<Option<i64>> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || file_size(&path))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))
    }

    async fn snapshot_inputs(&self, cue_path: &Path) -> Result<CueInputSnapshot> {
        let cue_path = cue_path.to_path_buf();
        tokio::task::spawn_blocking(move || snapshot_inputs_sync(&cue_path))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn cue_references_audio_file(&self, cue_path: &Path, audio_path: &Path) -> Result<bool> {
        let cue_path = cue_path.to_path_buf();
        let audio_path = audio_path.to_path_buf();
        tokio::task::spawn_blocking(move || cue_references_audio_file_sync(&cue_path, &audio_path))
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))
    }

    async fn filter_cue_files_for_audio(
        &self,
        cue_files: Vec<PathBuf>,
        audio_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        let audio_path = audio_path.to_path_buf();
        tokio::task::spawn_blocking(move || filter_cue_files_for_audio_sync(cue_files, &audio_path))
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))
    }
}

fn snapshot_inputs_sync(cue_path: &Path) -> Result<CueInputSnapshot> {
    let cue_size_bytes = file_size(cue_path);
    let cue = match parse_from_file(&cue_path.to_string_lossy(), false) {
        Ok(cue) => cue,
        Err(err) => {
            eprintln!(
                "Unable to parse cue file for input snapshot {}: {err}",
                cue_path.display()
            );
            return Ok(CueInputSnapshot {
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
        .filter_map(existing_audio_input)
        .collect();

    Ok(CueInputSnapshot {
        cue_size_bytes,
        audio_inputs,
    })
}

fn cue_references_audio_file_sync(cue_path: &Path, audio_path: &Path) -> bool {
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

fn filter_cue_files_for_audio_sync(cue_files: Vec<PathBuf>, audio_path: &Path) -> Vec<PathBuf> {
    cue_files
        .into_iter()
        .filter(|cue_path| cue_references_audio_file_sync(cue_path, audio_path))
        .collect()
}

fn existing_audio_input(path: PathBuf) -> Option<CueReferencedAudioInput> {
    let metadata = fs::metadata(&path).ok()?;
    Some(CueReferencedAudioInput {
        path,
        size_bytes: i64::try_from(metadata.len()).ok(),
    })
}

fn file_size(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| i64::try_from(metadata.len()).ok())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::FilesystemCueInputInspector;
    use crate::application::ports::{CueInputInspector, CueReferencedAudioInput};

    #[tokio::test]
    async fn snapshot_inputs_returns_cue_size_and_existing_audio_inputs() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("album.cue");
        let audio_path = tmp.path().join("album.flac");
        fs::write(
            &cue_path,
            "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        fs::write(&audio_path, b"audio").unwrap();

        let snapshot = FilesystemCueInputInspector::new()
            .snapshot_inputs(&cue_path)
            .await
            .unwrap();

        assert!(snapshot.cue_size_bytes.is_some());
        assert_eq!(
            snapshot.audio_inputs,
            vec![CueReferencedAudioInput {
                path: audio_path,
                size_bytes: Some(5),
            }]
        );
    }

    #[tokio::test]
    async fn snapshot_inputs_skips_missing_referenced_audio() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("album.cue");
        fs::write(
            &cue_path,
            "FILE \"missing.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let snapshot = FilesystemCueInputInspector::new()
            .snapshot_inputs(&cue_path)
            .await
            .unwrap();

        assert!(snapshot.cue_size_bytes.is_some());
        assert!(snapshot.audio_inputs.is_empty());
    }

    #[tokio::test]
    async fn snapshot_inputs_returns_empty_audio_inputs_for_invalid_cue() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("broken.cue");
        fs::write(&cue_path, "not a cue").unwrap();

        let snapshot = FilesystemCueInputInspector::new()
            .snapshot_inputs(&cue_path)
            .await
            .unwrap();

        assert!(snapshot.cue_size_bytes.is_some());
        assert!(snapshot.audio_inputs.is_empty());
    }

    #[tokio::test]
    async fn cue_matching_checks_exact_referenced_audio_file() {
        let tmp = tempdir().unwrap();
        let target_audio = tmp.path().join("target.flac");
        let other_audio = tmp.path().join("other.flac");
        let cue_path = tmp.path().join("target.cue");
        fs::write(&target_audio, b"audio").unwrap();
        fs::write(
            &cue_path,
            "FILE \"target.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let inspector = FilesystemCueInputInspector::new();
        assert!(inspector
            .cue_references_audio_file(&cue_path, &target_audio)
            .await
            .unwrap());
        assert!(!inspector
            .cue_references_audio_file(&cue_path, &other_audio)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn invalid_cue_matching_returns_false() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("broken.cue");
        let audio_path = tmp.path().join("target.flac");
        fs::write(&cue_path, "not a cue").unwrap();

        let matches = FilesystemCueInputInspector::new()
            .cue_references_audio_file(&cue_path, &audio_path)
            .await
            .unwrap();

        assert!(!matches);
    }
}
