use crate::domain::CueSheet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedDownload {
    pub download_id: String,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub tracked_download_state: String,
    pub cue_sheets: Vec<CueSheet>,
    pub split_complete: bool,
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
            cue_sheets: Vec::new(),
            split_complete: false,
            last_error: None,
        }
    }
}
