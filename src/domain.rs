use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub record_count: usize,
    pub download_ids: HashSet<String>,
    pub candidates: Vec<DownloadCandidate>,
}

impl QueueSnapshot {
    pub fn empty() -> Self {
        Self {
            record_count: 0,
            download_ids: HashSet::new(),
            candidates: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadCandidate {
    pub download_id: String,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub tracked_download_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub download_id: String,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub tracked_download_state: String,
    pub cue_files: Vec<CueFile>,
    pub split_complete: bool,
    pub last_error: Option<String>,
}

impl Download {
    pub fn pending(
        download_id: String,
        title: String,
        status: String,
        output_path: String,
        tracked_download_state: String,
    ) -> Self {
        Self {
            download_id,
            title,
            status,
            output_path,
            tracked_download_state,
            cue_files: Vec::new(),
            split_complete: false,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueFile {
    pub id: String,
    pub download_id: String,
    pub path: String,
    pub status: CueFileStatus,
    pub message: Option<String>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueFileStatus {
    Pending,
    Split,
    Skipped,
    Failed,
}

impl CueFileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Split => "split",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "split" => Self::Split,
            "skipped" => Self::Skipped,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }

    pub fn is_terminal_success(self) -> bool {
        matches!(self, Self::Split | Self::Skipped)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub cue_file_id: String,
    pub download_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueScan {
    pub cue_files: Vec<PathBuf>,
    pub errors: Vec<String>,
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
