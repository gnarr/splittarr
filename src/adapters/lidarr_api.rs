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
    page_size: usize,
    max_pages: usize,
    client: reqwest::Client,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueResponse {
    #[serde(default)]
    total_records: Option<usize>,
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
            page_size: settings.queue_page_size.max(1),
            max_pages: settings.queue_max_pages.max(1),
            client: reqwest::Client::new(),
        }
    }
}

impl QueueSource for LidarrQueueSource {
    async fn queue_snapshot(&self) -> Result<QueueSnapshot> {
        let mut page = 1_usize;
        let mut pages_fetched = 0_usize;
        let mut all_records = Vec::new();
        let mut expected_total_records = None;

        loop {
            if page > self.max_pages {
                return Err(anyhow!(
                    "lidarr queue pagination exceeded max pages ({}) while fetching page {}",
                    self.max_pages,
                    page
                ));
            }

            let response = self
                .client
                .get(format!("{}/api/v1/queue", self.base_url))
                .query(&[("page", page), ("pageSize", self.page_size)])
                .header("x-api-key", &self.api_key)
                .send()
                .await
                .map_err(|err| anyhow!("failed requesting lidarr queue page {page}: {err}"))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|err| anyhow!("failed reading lidarr queue page {page}: {err}"))?;

            if !status.is_success() {
                return Err(anyhow!(
                    "lidarr returned HTTP {status} for queue page {page}: {body}"
                ));
            }

            let queue: QueueResponse = serde_json::from_str(&body).map_err(|err| {
                anyhow!("lidarr returned invalid queue JSON for page {page}: {err}; body: {body}")
            })?;

            pages_fetched += 1;
            expected_total_records = expected_total_records.or(queue.total_records);
            let current_page_count = queue.records.len();
            all_records.extend(queue.records);

            if current_page_count == 0 {
                break;
            }
            if let Some(total) = expected_total_records {
                if all_records.len() >= total {
                    break;
                }
            }
            if current_page_count < self.page_size {
                break;
            }

            page += 1;
        }

        let active_download_ids = all_records
            .iter()
            .filter_map(QueueRecord::download_id)
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let failed_imports = all_records
            .iter()
            .filter_map(QueueRecord::as_candidate)
            .collect::<Vec<_>>();

        Ok(QueueSnapshot {
            total_records: all_records.len(),
            pages_fetched,
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
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

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
            queue_page_size: 100,
            queue_max_pages: 100,
        });

        let queue = client.queue_snapshot().await.unwrap();

        assert_eq!(queue.total_records, 1);
        assert_eq!(queue.pages_fetched, 1);
        assert!(queue.active_download_ids.contains("abc"));
    }

    #[tokio::test]
    async fn client_fetches_multiple_pages_and_collects_failed_imports() {
        let (url, _) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"totalRecords":2,"records":[{"title":"Downloading","status":"warning","trackedDownloadState":"downloading","downloadId":"down-1","outputPath":"/downloads/down-1"}]}"#,
            ),
            (
                "200 OK",
                r#"{"totalRecords":2,"records":[{"title":"Failed","status":"completed","trackedDownloadState":"importFailed","downloadId":"fail-1","outputPath":"/downloads/fail-1"}]}"#,
            ),
        ])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 1,
            queue_max_pages: 100,
        });

        let queue = client.queue_snapshot().await.unwrap();

        assert_eq!(queue.total_records, 2);
        assert_eq!(queue.pages_fetched, 2);
        assert!(queue.active_download_ids.contains("down-1"));
        assert!(queue.active_download_ids.contains("fail-1"));
        assert_eq!(queue.failed_imports.len(), 1);
    }

    #[tokio::test]
    async fn client_stops_when_short_page_is_returned() {
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            r#"{"records":[{"title":"Album","status":"completed","trackedDownloadState":"importFailed","downloadId":"abc","outputPath":"/downloads/album"}]}"#,
        )])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
        });

        let queue = client.queue_snapshot().await.unwrap();
        assert_eq!(queue.pages_fetched, 1);
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn client_stops_when_empty_page_is_returned() {
        let (url, requests) = serve_sequence(vec![
            ("200 OK", r#"{"totalRecords":5,"records":[{"downloadId":"a"}]}"#),
            ("200 OK", r#"{"totalRecords":5,"records":[]}"#),
        ])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 1,
            queue_max_pages: 100,
        });

        let queue = client.queue_snapshot().await.unwrap();
        assert_eq!(queue.pages_fetched, 2);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn client_reports_non_success_status() {
        let url = serve_once("500 Internal Server Error", "boom").await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
        });

        let err = client.queue_snapshot().await.unwrap_err();

        assert!(err.to_string().contains("HTTP 500"));
        assert!(err.to_string().contains("page 1"));
    }

    #[tokio::test]
    async fn client_reports_malformed_json() {
        let url = serve_once("200 OK", "not-json").await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
        });

        let err = client.queue_snapshot().await.unwrap_err();

        assert!(err.to_string().contains("invalid queue JSON"));
        assert!(err.to_string().contains("page 1"));
    }

    #[tokio::test]
    async fn client_errors_when_max_pages_is_exceeded() {
        let (url, _) = serve_sequence(vec![
            ("200 OK", r#"{"totalRecords":999,"records":[{"downloadId":"a"}]}"#),
            ("200 OK", r#"{"totalRecords":999,"records":[{"downloadId":"b"}]}"#),
            ("200 OK", r#"{"totalRecords":999,"records":[{"downloadId":"c"}]}"#),
        ])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 1,
            queue_max_pages: 2,
        });

        let err = client.queue_snapshot().await.unwrap_err();
        assert!(err.to_string().contains("exceeded max pages"));
    }

    async fn serve_once(status: &'static str, body: &'static str) -> String {
        let (url, _) = serve_sequence(vec![(status, body)]).await;
        url
    }

    async fn serve_sequence(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let request_lines = Arc::new(Mutex::new(Vec::new()));
        let shared_lines = Arc::clone(&request_lines);
        let shared_responses = Arc::new(Mutex::new(
            responses.into_iter().collect::<VecDeque<(&'static str, &'static str)>>(),
        ));
        let queue = Arc::clone(&shared_responses);

        tokio::spawn(async move {
            loop {
                let next = { queue.lock().unwrap().pop_front() };
                let Some((status, body)) = next else {
                    break;
                };

                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 4096];
                let bytes_read = socket.read(&mut request).await.unwrap();
                let request_text = String::from_utf8_lossy(&request[..bytes_read]);
                if let Some(line) = request_text.lines().next() {
                    shared_lines.lock().unwrap().push(line.to_owned());
                }

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        (format!("http://{addr}"), request_lines)
    }
}
