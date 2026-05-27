use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use rcue::parser::parse_from_file;
use regex::Regex;
use thiserror::Error;

use crate::config::{Cue, Shnsplit};

#[derive(Debug, Clone)]
pub struct Splitter {
    cue_strict: bool,
    shnsplit_path: PathBuf,
    overwrite: bool,
    format: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitResult {
    pub status: SplitStatus,
    pub tracks: Vec<PathBuf>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitStatus {
    Split,
    Skipped,
}

#[derive(Debug, Error)]
pub enum SplitError {
    #[error("cue path is not valid UTF-8: {0}")]
    NonUtf8CuePath(PathBuf),
    #[error("cue file has no parent directory: {0}")]
    MissingCueDirectory(PathBuf),
    #[error("cue file name is not valid UTF-8: {0}")]
    NonUtf8CueFileName(PathBuf),
    #[error("failed to parse cue file {path}: {message}")]
    CueParse { path: PathBuf, message: String },
    #[error("failed to run shnsplit for {path}: {source}")]
    CommandStart {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("shnsplit failed for {path} with status {status}: {stderr}")]
    CommandFailed {
        path: PathBuf,
        status: String,
        stderr: String,
    },
    #[error("shnsplit succeeded for {path} but no generated tracks were detected")]
    NoGeneratedTracks { path: PathBuf, stderr: String },
    #[error("shnsplit reported output that does not exist for {path}: {output}")]
    MissingGeneratedTrack { path: PathBuf, output: PathBuf },
}

impl Splitter {
    pub fn new(cue: &Cue, shnsplit: &Shnsplit) -> Self {
        Self {
            cue_strict: cue.strict,
            shnsplit_path: shnsplit.path.clone(),
            overwrite: shnsplit.overwrite,
            format: shnsplit.format.clone(),
        }
    }

    pub fn split_cue(&self, cue_path: &Path) -> Result<SplitResult, SplitError> {
        let cue_path_str = cue_path
            .to_str()
            .ok_or_else(|| SplitError::NonUtf8CuePath(cue_path.to_path_buf()))?;
        let cue_dir = cue_path
            .parent()
            .ok_or_else(|| SplitError::MissingCueDirectory(cue_path.to_path_buf()))?;
        let cue_file_name = cue_path
            .file_name()
            .ok_or_else(|| SplitError::NonUtf8CueFileName(cue_path.to_path_buf()))?;

        let cue = parse_from_file(cue_path_str, self.cue_strict).map_err(|source| {
            SplitError::CueParse {
                path: cue_path.to_path_buf(),
                message: source.to_string(),
            }
        })?;

        let referenced_files = cue
            .files
            .iter()
            .map(|file| file.file.as_str())
            .filter(|file| cue_dir.join(file).exists())
            .collect::<BTreeSet<_>>();

        if referenced_files.is_empty() {
            return Ok(SplitResult {
                status: SplitStatus::Skipped,
                tracks: Vec::new(),
                message: Some("cue file does not reference an audio file in its directory".into()),
            });
        }

        let overwrite = if self.overwrite { "always" } else { "never" };
        let output = {
            let mut command = Command::new(&self.shnsplit_path);
            command
                .current_dir(cue_dir)
                .arg("-f")
                .arg(cue_file_name)
                .arg("-d")
                .arg(cue_dir)
                .arg("-t")
                .arg(&self.format)
                .arg("-O")
                .arg(overwrite)
                .arg("-o")
                .arg("flac flac -s -8 -o %f -");

            for file in referenced_files {
                command.arg(file);
            }

            command
                .output()
                .map_err(|source| SplitError::CommandStart {
                    path: cue_path.to_path_buf(),
                    source,
                })?
        };

        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Err(SplitError::CommandFailed {
                path: cue_path.to_path_buf(),
                status: output.status.code().map_or_else(
                    || "terminated by signal".to_owned(),
                    |code| code.to_string(),
                ),
                stderr,
            });
        }

        let tracks = parse_generated_tracks(cue_dir, &stderr);
        if tracks.is_empty() {
            return Err(SplitError::NoGeneratedTracks {
                path: cue_path.to_path_buf(),
                stderr,
            });
        }

        for track in &tracks {
            if !track.exists() {
                return Err(SplitError::MissingGeneratedTrack {
                    path: cue_path.to_path_buf(),
                    output: track.clone(),
                });
            }
        }

        Ok(SplitResult {
            status: SplitStatus::Split,
            tracks,
            message: None,
        })
    }
}

fn parse_generated_tracks(cue_dir: &Path, stderr: &str) -> Vec<PathBuf> {
    let re = Regex::new(
        r"Splitting \[(?P<input_file>[^]]+)] \((?P<input_length>[^)]+)\) --> \[(?P<output_file>[^]]+)] \((?P<output_length>[^)]+)\) :",
    )
    .expect("generated-track regex must compile");

    re.captures_iter(stderr)
        .map(|cap| PathBuf::from(&cap["output_file"]))
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cue_dir.join(path)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn splits_with_fake_shnsplit_and_records_absolute_tracks() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), true);
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"touch "Artist - Album - 01 - Track One.flac"
echo "Splitting [album.flac] (0:01.00) --> [Artist - Album - 01 - Track One.flac] (0:01.00) :" >&2
exit 0
"#,
        );
        let splitter = test_splitter(fake);

        let result = splitter.split_cue(&cue_path).unwrap();

        assert_eq!(result.status, SplitStatus::Split);
        assert_eq!(
            result.tracks,
            vec![tmp.path().join("Artist - Album - 01 - Track One.flac")]
        );
    }

    #[test]
    fn skips_when_cue_references_no_existing_audio() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), false);
        let fake = write_fake_shnsplit(tmp.path(), "exit 1\n");
        let splitter = test_splitter(fake);

        let result = splitter.split_cue(&cue_path).unwrap();

        assert_eq!(result.status, SplitStatus::Skipped);
        assert!(result.tracks.is_empty());
        assert!(result.message.unwrap().contains("does not reference"));
    }

    #[test]
    fn command_failure_is_an_error() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), true);
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"echo "split failed" >&2
exit 2
"#,
        );
        let splitter = test_splitter(fake);

        let err = splitter.split_cue(&cue_path).unwrap_err();

        assert!(matches!(err, SplitError::CommandFailed { .. }));
    }

    #[test]
    fn parses_generated_tracks_relative_to_cue_directory() {
        let tmp = tempdir().unwrap();
        let stderr = "Splitting [album.flac] (0:01.00) --> [01 - Track.flac] (0:01.00) :";

        let tracks = parse_generated_tracks(tmp.path(), stderr);

        assert_eq!(tracks, vec![tmp.path().join("01 - Track.flac")]);
    }

    fn test_splitter(shnsplit_path: PathBuf) -> Splitter {
        Splitter::new(
            &Cue { strict: false },
            &Shnsplit {
                path: shnsplit_path,
                overwrite: true,
                format: "%p - %a - %n - %t".into(),
            },
        )
    }

    fn write_fixture_album(dir: &Path, with_audio: bool) -> PathBuf {
        let cue_path = dir.join("album.cue");
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
        if with_audio {
            fs::write(dir.join("album.flac"), "").unwrap();
        }
        cue_path
    }

    fn write_fake_shnsplit(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-shnsplit");
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).unwrap();
        }
        path
    }
}
