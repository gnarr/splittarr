use serde::Deserialize;
use serde::Serialize;

use crate::settings::Settings;
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
    pub album_id: Option<i64>,
    pub quality: Quality,
    pub custom_formats: Vec<CustomFormat>,
    pub custom_format_score: i64,
    pub size: i64,
    pub title: String,
    pub sizeleft: i64,
    pub status: String,
    pub tracked_download_status: String,
    pub tracked_download_state: String,
    pub status_messages: Vec<StatusMessage>,
    pub error_message: Option<String>,
    pub download_id: String,
    pub protocol: String,
    pub download_client: String,
    pub indexer: Option<String>,
    pub output_path: Option<String>,
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

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomFormat {
    pub id: i64,
    pub name: String,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusMessage {
    pub title: String,
    pub messages: Vec<String>,
}

impl Queue {
    pub async fn get() -> Result<Self, ExitFailure> {
        let settings = Settings::new()?;
        let client = reqwest::Client::new();
        let response = client
            .get(format!("{}/api/v1/queue", settings.lidarr.url))
            .header("x-api-key", settings.lidarr.api_key)
            .send()
            .await?
            .text()
            .await?;
        let queue = serde_json::from_str(&response).unwrap();
        Ok(queue)
    }
}
