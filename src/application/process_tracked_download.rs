use std::path::PathBuf;

use anyhow::Result;

use crate::application::ports::{CueScanner, CueSplitter, DownloadStore};
use crate::domain::{
    CueSheet, CueSheetStatus, FailedImportCandidate, SplitOutcome, SplitStatus, TrackedDownload,
};

pub async fn register_failed_imports<S: DownloadStore>(
    store: &S,
    downloads: &mut Vec<TrackedDownload>,
    candidates: &[FailedImportCandidate],
) -> Result<()> {
    for candidate in candidates {
        if let Some(download) = downloads
            .iter_mut()
            .find(|download| download.download_id == candidate.download_id)
        {
            download.title = candidate.title.clone();
            download.status = candidate.status.clone();
            download.output_path = candidate.output_path.clone();
            download.tracked_download_state = candidate.tracked_download_state.clone();
            store.upsert_tracked_download(download).await?;
            continue;
        }

        let download = TrackedDownload::pending(
            candidate.download_id.clone(),
            candidate.title.clone(),
            candidate.status.clone(),
            candidate.output_path.clone(),
            candidate.tracked_download_state.clone(),
        );
        store.upsert_tracked_download(&download).await?;
        downloads.push(download);
    }

    Ok(())
}

pub async fn process_tracked_download<S, C, P>(
    store: &S,
    scanner: &C,
    splitter: &P,
    download: TrackedDownload,
) -> Result<()>
where
    S: DownloadStore,
    C: CueScanner,
    P: CueSplitter,
{
    let output_path = PathBuf::from(&download.output_path);
    let scan = scanner.find_cue_sheets(&output_path).await?;

    for error in &scan.errors {
        eprintln!("Scan warning for {}: {error}", download.title);
    }

    if scan.cue_files.is_empty() {
        store
            .mark_download_complete(&download.download_id, false, Some("no cue files found"))
            .await?;
        return Ok(());
    }

    let mut all_cues_complete = true;
    let mut failures = Vec::new();

    for cue_path in scan.cue_files {
        let cue_sheet = store
            .get_or_create_cue_sheet(&download.download_id, &cue_path)
            .await?;

        if cue_sheet.status.is_terminal_success() {
            continue;
        }

        match splitter.split_cue(&cue_path).await {
            Ok(result) => {
                store_split_result(store, &cue_sheet, result).await?;
            }
            Err(err) => {
                all_cues_complete = false;
                let message = err.to_string();
                failures.push(format!("{}: {message}", cue_path.display()));
                store
                    .record_cue_result(&cue_sheet, CueSheetStatus::Failed, Some(&message), &[])
                    .await?;
            }
        }
    }

    let error_message = if failures.is_empty() {
        None
    } else {
        Some(failures.join("; "))
    };
    store
        .mark_download_complete(
            &download.download_id,
            all_cues_complete,
            error_message.as_deref(),
        )
        .await?;

    println!("Done processing {}", download.title);
    Ok(())
}

async fn store_split_result<S: DownloadStore>(
    store: &S,
    cue_sheet: &CueSheet,
    result: SplitOutcome,
) -> Result<()> {
    let status = match result.status {
        SplitStatus::Split => CueSheetStatus::Split,
        SplitStatus::Skipped => CueSheetStatus::Skipped,
    };
    store
        .record_cue_result(cue_sheet, status, result.message.as_deref(), &result.tracks)
        .await
}
