use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedImportCandidate {
    pub download_id: String,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub tracked_download_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub total_records: usize,
    pub pages_fetched: usize,
    pub active_download_ids: HashSet<String>,
    pub failed_imports: Vec<FailedImportCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOutcome {
    pub status: SplitStatus,
    pub tracks: Vec<PathBuf>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitStatus {
    Split,
    Skipped,
}
