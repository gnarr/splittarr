use serde::Deserialize;
use serde::Serialize;

use crate::settings::{get_settings, Lidarr};
use exitfailure::ExitFailure;

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    pub page: i64,
    pub page_size: i64,
    pub sort_key: String,
    pub sort_direction: String,
    pub total_records: i64,
    pub records: Vec<Record>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub artist_id: i64,
    pub album_id: i64,
    pub quality: Quality,
    pub size: f64,
    pub title: String,
    pub sizeleft: f64,
    pub status: String,
    pub tracked_download_status: String,
    pub tracked_download_state: String,
    pub status_messages: Vec<StatusMessage>,
    pub error_message: Option<String>,
    pub download_id: String,
    pub protocol: String,
    pub download_client: String,
    pub indexer: String,
    pub output_path: String,
    pub download_forced: bool,
    pub id: i64,
    pub timeleft: Option<String>,
    pub estimated_completion_time: Option<String>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    pub quality: Quality2,
    pub revision: Revision,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality2 {
    pub id: i64,
    pub name: String,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub version: i64,
    pub real: i64,
    pub is_repack: bool,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusMessage {
    pub title: String,
    pub messages: Vec<String>,
}

impl Queue {
    pub async fn get() -> Result<Self, ExitFailure> {
        let settings = get_settings();
        let client = reqwest::Client::new();
        let lidarr = settings.get::<Lidarr>("lidarr").expect(
            "Lidarr settings not found in config or environment. \
            Please create a config.toml file with [lidarr] url and api_key or set environment \
            variables SPLITTARR_LIDARR.URL and SPLITTARR_LIDARR.API_KEY.\nERROR",
        );
        let mut lidarr_queue = lidarr.url.to_owned();
        lidarr_queue.push_str("/api/v1/queue");
        let response = client
            .get(lidarr_queue)
            .header("x-api-key", lidarr.api_key)
            .send()
            .await?
            .text()
            .await?;
        let queue = serde_json::from_str(&response).unwrap();
        Ok(queue)
    }
}
