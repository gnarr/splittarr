use crate::domain::{CueSheet, InputFile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedDownload {
    pub download_id: String,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub tracked_download_state: String,
    pub lifecycle_state: DownloadLifecycleState,
    pub created_at: String,
    pub updated_at: String,
    pub first_seen_at: Option<String>,
    pub last_seen_in_queue_at: Option<String>,
    pub processing_started_at: Option<String>,
    pub processing_finished_at: Option<String>,
    pub cleanup_started_at: Option<String>,
    pub cleanup_finished_at: Option<String>,
    pub completed_at: Option<String>,
    pub input_files: Vec<InputFile>,
    pub cue_sheets: Vec<CueSheet>,
    pub last_error: Option<String>,
}

impl TrackedDownload {
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
            lifecycle_state: DownloadLifecycleState::Detected,
            created_at: String::new(),
            updated_at: String::new(),
            first_seen_at: None,
            last_seen_in_queue_at: None,
            processing_started_at: None,
            processing_finished_at: None,
            cleanup_started_at: None,
            cleanup_finished_at: None,
            completed_at: None,
            input_files: Vec::new(),
            cue_sheets: Vec::new(),
            last_error: None,
        }
    }

    pub fn generated_track_count(&self) -> usize {
        self.cue_sheets.iter().map(|cue| cue.tracks.len()).sum()
    }

    pub fn has_generated_tracks(&self) -> bool {
        self.generated_track_count() > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadLifecycleState {
    Detected,
    Processing,
    AwaitingImport,
    CleaningUp,
    Completed,
    Failed,
}

impl DownloadLifecycleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::Processing => "processing",
            Self::AwaitingImport => "awaiting_import",
            Self::CleaningUp => "cleaning_up",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "processing" => Self::Processing,
            "awaiting_import" => Self::AwaitingImport,
            "cleaning_up" => Self::CleaningUp,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Detected,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed)
    }

    pub fn is_ready_for_processing(&self) -> bool {
        matches!(self, Self::Detected | Self::Failed | Self::Processing)
    }
}
