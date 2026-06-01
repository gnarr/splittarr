use std::collections::HashSet;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::application::ports::QueueSource;
use crate::bootstrap::settings::LidarrSettings;
use crate::domain::{FailedImportCandidate, QueueSnapshot};

#[derive(Debug, Clone)]
pub struct LidarrQueueSource {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueResponse {
    #[serde(default)]
    records: Vec<QueueRecord>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueRecord {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tracked_download_state: Option<String>,
    #[serde(default)]
    download_id: Option<String>,
    #[serde(default)]
    output_path: Option<String>,
}

impl LidarrQueueSource {
    pub fn new(settings: &LidarrSettings) -> Self {
        Self {
            base_url: settings.url.trim_end_matches('/').to_owned(),
            api_key: settings.api_key.clone(),
            client: reqwest::Client::new(),
        }
    }
}

impl QueueSource for LidarrQueueSource {
    async fn queue_snapshot(&self) -> Result<QueueSnapshot> {
        let response = self
            .client
            .get(format!("{}/api/v1/queue", self.base_url))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(anyhow!("lidarr returned HTTP {status}: {body}"));
        }

        let queue: QueueResponse = serde_json::from_str(&body)
            .map_err(|err| anyhow!("lidarr returned invalid queue JSON: {err}; body: {body}"))?;

        let active_download_ids = queue
            .records
            .iter()
            .filter_map(QueueRecord::download_id)
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let failed_imports = queue
            .records
            .iter()
            .filter_map(QueueRecord::as_candidate)
            .collect::<Vec<_>>();

        Ok(QueueSnapshot {
            total_records: queue.records.len(),
            active_download_ids,
            failed_imports,
        })
    }
}

impl QueueRecord {
    fn download_id(&self) -> Option<&str> {
        self.download_id
            .as_deref()
            .filter(|value| !value.is_empty())
    }

    fn as_candidate(&self) -> Option<FailedImportCandidate> {
        let status = self.status.as_deref()?;
        let tracked_download_state = self.tracked_download_state.as_deref()?;
        if status != "completed" || tracked_download_state != "importFailed" {
            return None;
        }

        let download_id = self.download_id.as_ref()?.trim();
        let output_path = self.output_path.as_ref()?.trim();
        if download_id.is_empty() || output_path.is_empty() {
            return None;
        }

        Some(FailedImportCandidate {
            download_id: download_id.to_owned(),
            title: self
                .title
                .clone()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| download_id.to_owned()),
            status: status.to_owned(),
            output_path: output_path.to_owned(),
            tracked_download_state: tracked_download_state.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::LidarrQueueSource;
    use crate::application::ports::QueueSource;
    use crate::bootstrap::settings::LidarrSettings;
    use crate::domain::FailedImportCandidate;

    #[test]
    fn queue_record_candidate_requires_import_failed_completed_with_path() {
        let record = super::QueueRecord {
            title: Some("Album".to_owned()),
            status: Some("completed".to_owned()),
            tracked_download_state: Some("importFailed".to_owned()),
            download_id: Some("abc".to_owned()),
            output_path: Some("/downloads/album".to_owned()),
        };

        assert_eq!(
            record.as_candidate(),
            Some(FailedImportCandidate {
                download_id: "abc".to_owned(),
                title: "Album".to_owned(),
                status: "completed".to_owned(),
                output_path: "/downloads/album".to_owned(),
                tracked_download_state: "importFailed".to_owned(),
            })
        );

        let mut missing_path = record;
        missing_path.output_path = None;
        assert_eq!(missing_path.as_candidate(), None);
    }

    #[tokio::test]
    async fn client_parses_successful_queue_response() {
        let body = r#"{"records":[{"title":"Album","status":"completed","trackedDownloadState":"importFailed","downloadId":"abc","outputPath":"/downloads/album"}]}"#;
        let url = serve_once("200 OK", body).await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
        });

        let queue = client.queue_snapshot().await.unwrap();

        assert_eq!(queue.total_records, 1);
        assert!(queue.active_download_ids.contains("abc"));
    }

    #[tokio::test]
    async fn client_reports_non_success_status() {
        let url = serve_once("500 Internal Server Error", "boom").await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
        });

        let err = client.queue_snapshot().await.unwrap_err();

        assert!(err.to_string().contains("HTTP 500"));
    }

    #[tokio::test]
    async fn client_reports_malformed_json() {
        let url = serve_once("200 OK", "not-json").await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
        });

        let err = client.queue_snapshot().await.unwrap_err();

        assert!(err.to_string().contains("invalid queue JSON"));
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
