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
use std::collections::HashSet;
use walkdir::WalkDir;

#[tokio::main]
async fn main() -> Result<(), ExitFailure> {
    println!("Splittarr");

    let queue = Queue::get().await?;
    let mut paths = HashSet::new();
    for record in queue.records {
        if record.status == "completed" && record.tracked_download_state == "importFailed" {
            let download = Download {
                id: record.id,
                title: record.title,
                status: record.status,
                output_path: record.output_path,
                download_id: record.download_id,
                tracked_download_state: record.tracked_download_state,
            };
            download.save().await?;
            paths.insert(download.output_path);
        }
    }

    for path in paths {
        for file in WalkDir::new(path).into_iter().filter_map(|file| file.ok()) {
            if file.metadata().unwrap().is_file() {
                if file.file_name().to_str().unwrap().ends_with("cue") {
                    split(file);
                }
            }
        }
    }
    Ok(())
}
