use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::prelude::*;

use crate::config::Settings;
use crate::lidarr::{LidarrClient, Queue};
use crate::scanner::find_cue_files;
use crate::splitter::{SplitResult, SplitStatus, Splitter};
use crate::store::{CueFileStatus, Download, Repository};

pub async fn run(settings: Settings) -> Result<()> {
    let repo = Repository::open(&settings.data_dir).context("initialize Splittarr database")?;
    let lidarr = LidarrClient::new(&settings.lidarr);
    let splitter = Splitter::new(&settings.cue, &settings.shnsplit);
    let interval = Duration::from_secs(settings.check_frequency_seconds);

    println!("Splittarr");
    println!(
        "Checking every {} seconds",
        settings.check_frequency_seconds
    );

    loop {
        println!(
            "Checking Lidarr's download queue at {}",
            Local::now().format("%Y-%m-%d %H:%M:%S %z")
        );

        if let Err(err) = run_once(&repo, &lidarr, &splitter).await {
            eprintln!("Splittarr cycle failed: {err:#}");
        }

        tokio::time::sleep(interval).await;
    }
}

async fn run_once(repo: &Repository, lidarr: &LidarrClient, splitter: &Splitter) -> Result<()> {
    let repo_for_load = repo.clone();
    let mut downloads = blocking(move || {
        repo_for_load
            .all_downloads()
            .context("load tracked downloads from database")
    })
    .await?;

    println!("{} downloads registered in Splittarr", downloads.len());

    let queue = lidarr.queue().await.context("load Lidarr queue")?;
    println!(
        "Found {} records in Lidarr's download queue",
        queue.records.len()
    );

    register_new_candidates(repo, &mut downloads, &queue).await?;

    let queue_ids = queue_download_ids(&queue);
    let (to_process, to_cleanup) = classify_downloads(downloads, &queue_ids);

    println!("{} downloads to be processed", to_process.len());

    for download in to_process {
        if let Err(err) = process_download(repo, splitter, download.clone()).await {
            eprintln!("Failed processing {}: {err:#}", download.title);
            let repo_for_mark = repo.clone();
            let download_id = download.download_id.clone();
            let message = err.to_string();
            blocking(move || {
                repo_for_mark
                    .mark_download_complete(&download_id, false, Some(&message))
                    .context("store processing error")
            })
            .await?;
        }
    }

    for download in to_cleanup {
        println!("Cleaning up {}", download.title);
        if let Err(err) = cleanup_download(repo.clone(), download.clone()).await {
            eprintln!("Failed cleaning up {}: {err:#}", download.title);
        }
    }

    Ok(())
}

async fn register_new_candidates(
    repo: &Repository,
    downloads: &mut Vec<Download>,
    queue: &Queue,
) -> Result<()> {
    for record in &queue.records {
        let Some(candidate) = record.as_candidate() else {
            continue;
        };

        if let Some(download) = downloads
            .iter_mut()
            .find(|download| download.download_id == candidate.download_id)
        {
            download.title = candidate.title;
            download.status = candidate.status;
            download.output_path = candidate.output_path;
            download.tracked_download_state = candidate.tracked_download_state;
            let repo_for_save = repo.clone();
            let download = download.clone();
            blocking(move || {
                repo_for_save
                    .upsert_download(&download)
                    .context("update tracked download")
            })
            .await?;
            continue;
        }

        let download = Download::pending(
            candidate.download_id,
            candidate.title,
            candidate.status,
            candidate.output_path,
            candidate.tracked_download_state,
        );
        let repo_for_save = repo.clone();
        let download_for_save = download.clone();
        blocking(move || {
            repo_for_save
                .upsert_download(&download_for_save)
                .context("store new tracked download")
        })
        .await?;
        downloads.push(download);
    }

    Ok(())
}

async fn process_download(
    repo: &Repository,
    splitter: &Splitter,
    download: Download,
) -> Result<()> {
    let output_path = PathBuf::from(&download.output_path);
    let scan = {
        let output_path = output_path.clone();
        blocking(move || find_cue_files(&output_path).context("scan download output path")).await?
    };

    for error in &scan.errors {
        eprintln!("Scan warning for {}: {error}", download.title);
    }

    if scan.cue_files.is_empty() {
        let repo_for_mark = repo.clone();
        let download_id = download.download_id.clone();
        blocking(move || {
            repo_for_mark
                .mark_download_complete(&download_id, false, Some("no cue files found"))
                .context("store no-cue state")
        })
        .await?;
        return Ok(());
    }

    let mut all_cues_complete = true;
    let mut failures = Vec::new();

    for cue_path in scan.cue_files {
        let repo_for_cue = repo.clone();
        let download_id = download.download_id.clone();
        let cue_file = {
            let cue_path = cue_path.clone();
            blocking(move || {
                repo_for_cue
                    .get_or_create_cue_file(&download_id, &cue_path)
                    .context("store cue file")
            })
            .await?
        };

        if cue_file.status.is_terminal_success() {
            continue;
        }

        let split_result = {
            let splitter = splitter.clone();
            let cue_path = cue_path.clone();
            blocking(move || splitter.split_cue(&cue_path).map_err(anyhow::Error::from)).await
        };

        match split_result {
            Ok(result) => {
                store_split_result(repo, &cue_file, result).await?;
            }
            Err(err) => {
                all_cues_complete = false;
                let message = err.to_string();
                failures.push(format!("{}: {message}", cue_path.display()));
                let repo_for_result = repo.clone();
                let cue_file = cue_file.clone();
                blocking(move || {
                    repo_for_result
                        .record_cue_result(&cue_file, CueFileStatus::Failed, Some(&message), &[])
                        .context("store failed cue result")
                })
                .await?;
            }
        }
    }

    let error_message = if failures.is_empty() {
        None
    } else {
        Some(failures.join("; "))
    };
    let repo_for_mark = repo.clone();
    let download_id = download.download_id.clone();
    blocking(move || {
        repo_for_mark
            .mark_download_complete(&download_id, all_cues_complete, error_message.as_deref())
            .context("store download completion state")
    })
    .await?;

    println!("Done processing {}", download.title);
    Ok(())
}

async fn store_split_result(
    repo: &Repository,
    cue_file: &crate::store::CueFile,
    result: SplitResult,
) -> Result<()> {
    let status = match result.status {
        SplitStatus::Split => CueFileStatus::Split,
        SplitStatus::Skipped => CueFileStatus::Skipped,
    };
    let repo_for_result = repo.clone();
    let cue_file = cue_file.clone();
    blocking(move || {
        repo_for_result
            .record_cue_result(&cue_file, status, result.message.as_deref(), &result.tracks)
            .context("store cue result")
    })
    .await
}

async fn cleanup_download(repo: Repository, download: Download) -> Result<()> {
    blocking(move || {
        let mut errors = Vec::new();

        for cue_file in &download.cue_files {
            for track in &cue_file.tracks {
                let path = cleanup_track_path(cue_file, track);
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => errors.push(format!("{}: {err}", path.display())),
                }
            }
        }

        if !errors.is_empty() {
            return Err(anyhow!("cleanup failed: {}", errors.join("; ")));
        }

        repo.delete_download(&download.download_id)
            .context("delete tracked download")
    })
    .await
}

fn cleanup_track_path(cue_file: &crate::store::CueFile, track: &crate::store::Track) -> PathBuf {
    let path = PathBuf::from(&track.path);
    if path.is_absolute() {
        return path;
    }

    Path::new(&cue_file.path)
        .parent()
        .map_or(path.clone(), |cue_dir| cue_dir.join(path))
}

fn queue_download_ids(queue: &Queue) -> HashSet<String> {
    queue
        .records
        .iter()
        .filter_map(|record| record.download_id().map(str::to_owned))
        .collect()
}

fn classify_downloads(
    downloads: Vec<Download>,
    queue_ids: &HashSet<String>,
) -> (Vec<Download>, Vec<Download>) {
    let mut to_process = Vec::new();
    let mut to_cleanup = Vec::new();

    for download in downloads {
        if queue_ids.contains(&download.download_id) {
            if !download.split_complete {
                to_process.push(download);
            }
        } else {
            to_cleanup.push(download);
        }
    }

    (to_process, to_cleanup)
}

async fn blocking<T>(operation: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .context("blocking task failed to join")?
}

#[cfg(test)]
mod tests {
    use crate::lidarr::Record;

    use super::*;

    #[test]
    fn classifies_downloads_by_queue_presence_and_split_state() {
        let queue_ids = HashSet::from(["in-queue".to_owned(), "processed".to_owned()]);
        let mut processed = download("processed");
        processed.split_complete = true;
        let downloads = vec![download("in-queue"), processed, download("gone")];

        let (to_process, to_cleanup) = classify_downloads(downloads, &queue_ids);

        assert_eq!(
            to_process
                .iter()
                .map(|download| download.download_id.as_str())
                .collect::<Vec<_>>(),
            vec!["in-queue"]
        );
        assert_eq!(
            to_cleanup
                .iter()
                .map(|download| download.download_id.as_str())
                .collect::<Vec<_>>(),
            vec!["gone"]
        );
    }

    #[test]
    fn extracts_queue_download_ids_from_partial_records() {
        let queue = Queue {
            records: vec![
                Record {
                    download_id: Some("abc".into()),
                    ..Record::default()
                },
                Record::default(),
            ],
            ..Queue::default()
        };

        assert_eq!(
            queue_download_ids(&queue),
            HashSet::from(["abc".to_owned()])
        );
    }

    #[test]
    fn cleanup_resolves_legacy_relative_track_paths_from_cue_directory() {
        let cue_file = crate::store::CueFile {
            id: "cue-1".into(),
            download_id: "download-1".into(),
            path: "/downloads/album/album.cue".into(),
            status: CueFileStatus::Split,
            message: None,
            tracks: Vec::new(),
        };
        let track = crate::store::Track {
            id: "track-1".into(),
            cue_file_id: "cue-1".into(),
            download_id: "download-1".into(),
            path: "01 - Track.flac".into(),
        };

        assert_eq!(
            cleanup_track_path(&cue_file, &track),
            PathBuf::from("/downloads/album/01 - Track.flac")
        );
    }

    fn download(download_id: &str) -> Download {
        Download::pending(
            download_id.to_owned(),
            download_id.to_owned(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        )
    }
}
