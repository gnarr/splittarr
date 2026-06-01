use std::path::PathBuf;

use crate::domain::GeneratedTrack;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueSheet {
    pub id: String,
    pub download_id: String,
    pub path: String,
    pub status: CueSheetStatus,
    pub message: Option<String>,
    pub updated_at: String,
    pub tracks: Vec<GeneratedTrack>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueSheetStatus {
    Pending,
    Split,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCueSheets {
    pub cue_files: Vec<PathBuf>,
    pub errors: Vec<String>,
}

impl CueSheetStatus {
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
pub struct InputFile {
    pub id: String,
    pub download_id: String,
    pub cue_sheet_id: Option<String>,
    pub path: String,
    pub kind: InputFileKind,
    pub size_bytes: Option<i64>,
    pub captured_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFileKind {
    Cue,
    Audio,
}

impl InputFileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cue => "cue",
            Self::Audio => "audio",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "audio" => Self::Audio,
            _ => Self::Cue,
        }
    }
}
