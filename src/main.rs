mod settings;

mod globals;
mod lidarr;
mod shnsplit;
mod store;

use crate::settings::settings::{Lidarr, Settings};
use crate::shnsplit::shnsplit::split;
use crate::store::Download;
use chrono::prelude::*;
use exitfailure::ExitFailure;
use lidarr::Queue;
use std::borrow::BorrowMut;
use std::thread::sleep;
use std::{fs, time};
use walkdir::WalkDir;

#[tokio::main]
async fn main() -> Result<(), ExitFailure> {
    println!("Splittarr");
    let settings = Settings::new();
    let check_frequency_seconds = settings.get::<u64>("check_frequency_seconds").unwrap();
    println!("Checking every {} seconds", check_frequency_seconds);
    loop {
        println!(
            "Checking Lidarr's queue at {}",
            Local::now().format("%Y-%m-%d %H:%M:%S %z")
        );

        // get all downloads in database
        let downloads = Download::all().await?;

        // get queue from lidarr
        let queue = Queue::get().await?;

        // partition downloads by whether they are found in lidarr's queue or not
        let (in_queue, to_delete): (Vec<Download>, Vec<Download>) =
            downloads.into_iter().partition(|download| {
                queue
                    .records
                    .iter()
                    .find(|&record| record.download_id == download.download_id)
                    .is_some()
            });

        // dbg!(&in_queue);
        // dbg!(&to_delete);

        // partition downloads found in queue by whether they where completely split or not
        let (processed, mut process_queue): (Vec<Download>, Vec<Download>) = in_queue
            .into_iter()
            .partition(|download| download.split_complete);

        // dbg!(&process_queue);

        // partition records by whether they are in process_queue or not
        let (_, un_processed_records): (Vec<_>, Vec<_>) =
            queue.records.into_iter().partition(|record| {
                processed
                    .iter()
                    .find(|&download| download.download_id == record.download_id)
                    .is_some()
            });

        // dbg!(&un_processed_records);

        for record in un_processed_records {
            let in_process_queue = process_queue
                .iter()
                .find(|&download| download.download_id == record.download_id)
                .is_some();
            if !in_process_queue
                && record.status == "completed"
                && record.tracked_download_state == "importFailed"
            {
                let download = Download {
                    title: record.title,
                    status: record.status,
                    output_path: record.output_path,
                    download_id: record.download_id,
                    tracked_download_state: record.tracked_download_state,
                    cue_files: vec![],
                    split_complete: false,
                };
                download.save().await?;
                process_queue.push(download);
            }
        }

        // dbg!(&process_queue);

        for mut download in process_queue {
            for file in WalkDir::new(download.output_path.as_str())
                .into_iter()
                .filter_map(|file| file.ok())
            {
                if file.metadata().unwrap().is_file() {
                    if file.file_name().to_str().unwrap().ends_with("cue") {
                        split(download.borrow_mut(), file).await;
                    }
                }
            }
            download.split_complete = true;
            download.save().await.expect("TODO: panic message");
        }

        for mut download in to_delete {
            println!("Cleaning up {}", download.title);
            for cue_file in download.cue_files.iter_mut() {
                for track in cue_file.tracks.iter_mut() {
                    dbg!(track.path.as_str());
                    fs::remove_file(track.path.as_str()).unwrap();
                    track.delete().await;
                }
                cue_file.delete().await;
            }
            download.delete().await;
        }

        sleep(time::Duration::from_secs(check_frequency_seconds));
    }
}
