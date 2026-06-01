use std::time::Duration;

use anyhow::Result;
use chrono::prelude::*;

use crate::application::cleanup_processed_download::cleanup_processed_download;
use crate::application::monitor_download_queue::classify_downloads;
use crate::application::ports::{
    CueScanner, CueSplitter, DownloadStore, QueueSource, TrackCleanup,
};
use crate::application::process_tracked_download::{
    process_tracked_download, register_failed_imports,
};

pub struct MonitorService<Q, S, C, P, X> {
    queue_source: Q,
    download_store: S,
    cue_scanner: C,
    cue_splitter: P,
    track_cleanup: X,
    check_frequency_seconds: u64,
}

impl<Q, S, C, P, X> MonitorService<Q, S, C, P, X> {
    pub fn new(
        queue_source: Q,
        download_store: S,
        cue_scanner: C,
        cue_splitter: P,
        track_cleanup: X,
        check_frequency_seconds: u64,
    ) -> Self {
        Self {
            queue_source,
            download_store,
            cue_scanner,
            cue_splitter,
            track_cleanup,
            check_frequency_seconds,
        }
    }
}

impl<Q, S, C, P, X> MonitorService<Q, S, C, P, X>
where
    Q: QueueSource,
    S: DownloadStore,
    C: CueScanner,
    P: CueSplitter,
    X: TrackCleanup,
{
    pub async fn run(&self) -> Result<()> {
        let interval = Duration::from_secs(self.check_frequency_seconds);

        println!("Splittarr");
        println!("Checking every {} seconds", self.check_frequency_seconds);

        loop {
            println!(
                "Checking Lidarr's download queue at {}",
                Local::now().format("%Y-%m-%d %H:%M:%S %z")
            );

            if let Err(err) = self.run_once().await {
                eprintln!("Splittarr cycle failed: {err:#}");
            }

            tokio::time::sleep(interval).await;
        }
    }

    pub async fn run_once(&self) -> Result<()> {
        let mut downloads = self.download_store.load_tracked_downloads().await?;
        println!("{} downloads registered in Splittarr", downloads.len());

        let snapshot = self.queue_source.queue_snapshot().await?;
        println!(
            "Found {} records in Lidarr's download queue",
            snapshot.total_records
        );

        register_failed_imports(
            &self.download_store,
            &mut downloads,
            &snapshot.failed_imports,
        )
        .await?;

        let (to_process, to_cleanup) = classify_downloads(downloads, &snapshot);

        println!("{} downloads to be processed", to_process.len());

        for download in to_process {
            if let Err(err) = process_tracked_download(
                &self.download_store,
                &self.cue_scanner,
                &self.cue_splitter,
                download.clone(),
            )
            .await
            {
                eprintln!("Failed processing {}: {err:#}", download.title);
                let message = err.to_string();
                self.download_store
                    .mark_download_complete(&download.download_id, false, Some(&message))
                    .await?;
            }
        }

        for download in to_cleanup {
            println!("Cleaning up {}", download.title);
            if let Err(err) =
                cleanup_processed_download(&self.download_store, &self.track_cleanup, &download)
                    .await
            {
                eprintln!("Failed cleaning up {}: {err:#}", download.title);
            }
        }

        Ok(())
    }
}
