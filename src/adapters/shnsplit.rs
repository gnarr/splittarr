use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use rcue::parser::parse_from_file;
use regex::Regex;
use thiserror::Error;

use crate::application::ports::CueSplitterPort;
use crate::config::{Cue, Shnsplit};
use crate::domain::{SplitResult, SplitStatus};

#[derive(Debug, Clone)]
pub struct ShnsplitCueSplitter {
    cue_strict: bool,
    shnsplit_path: PathBuf,
    overwrite: bool,
    format: String,
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

impl ShnsplitCueSplitter {
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

            command.output().map_err(|source| SplitError::CommandStart {
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

impl CueSplitterPort for ShnsplitCueSplitter {
    fn split_cue(&self, cue_path: &Path) -> Result<SplitResult> {
        ShnsplitCueSplitter::split_cue(self, cue_path).map_err(Into::into)
    }
}

fn parse_generated_tracks(cue_dir: &Path, stderr: &str) -> Vec<PathBuf> {
    let re = Regex::new(
        r"Splitting \[(?P<input_file>[^]]+)] \((?P<input_length>[^)]+)\) --> \[(?P<output_file>[^]]+)] \((?P<output_length>[^)]+)\) :",
    )
    .expect("generated-track regex must compile");

    re.captures_iter(stderr)
        .map(|cap| PathBuf::from(&cap["output_file"]))
        .map(|path| if path.is_absolute() { path } else { cue_dir.join(path) })
        .collect()
}
