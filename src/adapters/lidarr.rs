use std::collections::HashSet;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::application::ports::{BoxFuture, LidarrQueuePort};
use crate::config::Lidarr;
use crate::domain::{DownloadCandidate, QueueSnapshot};

#[derive(Debug, Clone)]
pub struct LidarrClient {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

#[derive(Debug, Error)]
pub enum LidarrError {
    #[error("lidarr request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("lidarr returned HTTP {status}: {body}")]
    Http {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("lidarr returned invalid queue JSON: {source}")]
    Json {
        source: serde_json::Error,
        body: String,
    },
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub page_size: Option<i64>,
    #[serde(default)]
    pub sort_key: Option<String>,
    #[serde(default)]
    pub sort_direction: Option<String>,
    #[serde(default)]
    pub total_records: Option<i64>,
    #[serde(default)]
    pub records: Vec<Record>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub tracked_download_status: Option<String>,
    #[serde(default)]
    pub tracked_download_state: Option<String>,
    #[serde(default)]
    pub download_id: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
}

impl LidarrClient {
    pub fn new(settings: &Lidarr) -> Self {
        Self {
            base_url: settings.url.trim_end_matches('/').to_owned(),
            api_key: settings.api_key.clone(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn queue(&self) -> Result<Queue, LidarrError> {
        let response = self
            .client
            .get(format!("{}/api/v1/queue", self.base_url))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(LidarrError::Http { status, body });
        }

        serde_json::from_str::<Queue>(&body).map_err(|source| LidarrError::Json { source, body })
    }
}

impl LidarrQueuePort for LidarrClient {
    fn queue_snapshot(&self) -> BoxFuture<'_, Result<QueueSnapshot>> {
        Box::pin(async move { Ok(self.queue().await?.into_snapshot()) })
    }
}

impl Queue {
    fn into_snapshot(self) -> QueueSnapshot {
        QueueSnapshot {
            record_count: self.records.len(),
            download_ids: self
                .records
                .iter()
                .filter_map(|record| record.download_id().map(str::to_owned))
                .collect::<HashSet<_>>(),
            candidates: self
                .records
                .into_iter()
                .filter_map(|record| record.into_candidate())
                .collect(),
        }
    }
}

impl Record {
    pub fn download_id(&self) -> Option<&str> {
        self.download_id.as_deref().filter(|value| !value.is_empty())
    }

    pub fn into_candidate(self) -> Option<DownloadCandidate> {
        let status = self.status?;
        let tracked_download_state = self.tracked_download_state?;
        if status != "completed" || tracked_download_state != "importFailed" {
            return None;
        }

        let download_id = self.download_id?.trim().to_owned();
        let output_path = self.output_path?.trim().to_owned();
        if download_id.is_empty() || output_path.is_empty() {
            return None;
        }

        let title = self
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| download_id.clone());

        Some(DownloadCandidate {
            download_id,
            title,
            status,
            output_path,
            tracked_download_state,
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn queue_record_candidate_requires_import_failed_completed_with_path() {
        let record = Record {
            title: Some("Album".to_owned()),
            status: Some("completed".to_owned()),
            tracked_download_state: Some("importFailed".to_owned()),
            download_id: Some("abc".to_owned()),
            output_path: Some("/downloads/album".to_owned()),
            ..Record::default()
        };

        assert_eq!(
            record.into_candidate(),
            Some(DownloadCandidate {
                download_id: "abc".to_owned(),
                title: "Album".to_owned(),
                status: "completed".to_owned(),
                output_path: "/downloads/album".to_owned(),
                tracked_download_state: "importFailed".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn client_parses_successful_queue_response() {
        let body = r#"{"records":[{"title":"Album","status":"completed","trackedDownloadState":"importFailed","downloadId":"abc","outputPath":"/downloads/album"}]}"#;
        let url = serve_once("200 OK", body).await;
        let client = LidarrClient::new(&Lidarr {
            url,
            api_key: "secret".to_owned(),
        });

        let queue = client.queue().await.unwrap();

        assert_eq!(queue.records.len(), 1);
        assert_eq!(queue.records[0].download_id(), Some("abc"));
    }

    async fn serve_once(status: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}")
    }
}
