use std::path::Path;

use anyhow::{anyhow, Result};
use walkdir::WalkDir;

use crate::application::ports::CueScanner;
use crate::domain::DiscoveredCueSheets;

#[derive(Debug, Clone, Default)]
pub struct FilesystemCueScanner;

impl FilesystemCueScanner {
    pub fn new() -> Self {
        Self
    }
}

impl CueScanner for FilesystemCueScanner {
    async fn find_cue_sheets(&self, root: &Path) -> Result<DiscoveredCueSheets> {
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || find_cue_files(&root))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }
}

fn find_cue_files(root: &Path) -> Result<DiscoveredCueSheets> {
    if !root.exists() {
        return Err(anyhow!(
            "download output path does not exist: {}",
            root.display()
        ));
    }
    if !root.is_dir() {
        return Err(anyhow!(
            "download output path is not a directory: {}",
            root.display()
        ));
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
    Ok(DiscoveredCueSheets { cue_files, errors })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::find_cue_files;

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

        assert!(err
            .to_string()
            .contains("download output path does not exist"));
    }
}
