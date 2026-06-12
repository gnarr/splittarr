#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTrack {
    pub id: String,
    pub cue_sheet_id: String,
    pub download_id: String,
    pub path: String,
    pub size_bytes: Option<i64>,
    pub cleanup_status: TrackCleanupStatus,
    pub cleanup_message: Option<String>,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackCleanupStatus {
    Pending,
    Deleted,
    DeleteFailed,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedTrack {
    pub path: String,
    pub size_bytes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackCleanupOutcome {
    pub track_id: String,
    pub status: TrackCleanupStatus,
    pub message: Option<String>,
}
