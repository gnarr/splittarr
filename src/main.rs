mod settings;

mod globals;
mod lidarr;
mod shnsplit;
mod store;

use crate::settings::settings::Lidarr;
use crate::shnsplit::shnsplit::split;
use crate::store::Download;
use exitfailure::ExitFailure;
use lidarr::Queue;
use std::borrow::{Borrow, BorrowMut};
use std::collections::HashSet;
use walkdir::WalkDir;

#[tokio::main]
async fn main() -> Result<(), ExitFailure> {
    println!("Splittarr");

    let queue = Queue::get().await?;
    let mut downloads = HashSet::new();
    for record in queue.records {
        if record.status == "completed" && record.tracked_download_state == "importFailed" {
            let download = Download {
                title: record.title,
                status: record.status,
                output_path: record.output_path,
                download_id: record.download_id,
                tracked_download_state: record.tracked_download_state,
                cue_files: vec![],
            };
            download.save().await?;
            downloads.insert(download);
        }
    }

    for mut download in downloads {
        let download_id = download.download_id.to_owned();
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
        dbg!(download);

        let ddd = Download::find(download_id).await.unwrap();
        dbg!(ddd);
    }

    Ok(())
}
