use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use thiserror::Error;
use walkdir::WalkDir;

use crate::application::ports::{CueScannerPort, GeneratedTrackCleanerPort};
use crate::domain::CueScan;

#[derive(Debug, Clone, Copy)]
pub struct CueFileScanner;

#[derive(Debug, Clone, Copy)]
pub struct GeneratedTrackCleaner;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("download output path does not exist: {0}")]
    MissingRoot(PathBuf),
    #[error("download output path is not a directory: {0}")]
    NotDirectory(PathBuf),
}

impl CueScannerPort for CueFileScanner {
    fn find_cue_files(&self, root: &Path) -> Result<CueScan> {
        find_cue_files(root).map_err(Into::into)
    }
}

impl GeneratedTrackCleanerPort for GeneratedTrackCleaner {
    fn remove_generated_track(&self, path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

pub fn find_cue_files(root: &Path) -> Result<CueScan, ScanError> {
    if !root.exists() {
        return Err(ScanError::MissingRoot(root.to_path_buf()));
    }
    if !root.is_dir() {
        return Err(ScanError::NotDirectory(root.to_path_buf()));
    }

    let mut cue_files = Vec::new();
    let mut errors = Vec::new();

    for entry in WalkDir::new(root) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                errors.push(err.to_string());
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let is_cue = entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"));

        if is_cue {
            cue_files.push(entry.path().to_path_buf());
        }
    }

    cue_files.sort();
    Ok(CueScan { cue_files, errors })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn finds_cue_files_case_insensitively() {
        let tmp = tempdir().unwrap();
        let nested = tmp.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let cue = nested.join("album.CUE");
        fs::write(&cue, "").unwrap();
        fs::write(tmp.path().join("album.flac"), "").unwrap();

        let scan = find_cue_files(tmp.path()).unwrap();

        assert_eq!(scan.cue_files, vec![cue]);
        assert!(scan.errors.is_empty());
    }

    #[test]
    fn missing_root_is_an_error() {
        let tmp = tempdir().unwrap();
        let err = find_cue_files(&tmp.path().join("missing")).unwrap_err();

        assert!(matches!(err, ScanError::MissingRoot(_)));
    }
}
