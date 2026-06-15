use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::application::ports::{
    ManualImportRequest, ManualImportResult, ManualImportTrigger, QueueSource,
};
use crate::bootstrap::settings::LidarrSettings;
use crate::domain::{FailedImportCandidate, QueueSnapshot};

#[derive(Debug, Clone)]
pub struct LidarrQueueSource {
    base_url: String,
    api_key: String,
    page_size: usize,
    max_pages: usize,
    manual_import_enabled: bool,
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
            manual_import_enabled: settings.manual_import_enabled,
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

impl ManualImportTrigger for LidarrQueueSource {
    async fn trigger_manual_import(
        &self,
        request: ManualImportRequest,
    ) -> Result<ManualImportResult> {
        if !self.manual_import_enabled {
            return Ok(ManualImportResult::Disabled);
        }

        let response = self
            .client
            .get(format!("{}/api/v1/manualimport", self.base_url))
            .query(&[
                ("folder", request.download.output_path.as_str()),
                ("downloadId", request.download.download_id.as_str()),
                ("filterExistingFiles", "true"),
                ("replaceExistingFiles", "true"),
            ])
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .map_err(|err| anyhow!("failed requesting lidarr manual import candidates: {err}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| anyhow!("failed reading lidarr manual import candidates: {err}"))?;

        if !status.is_success() {
            return Err(anyhow!(
                "lidarr returned HTTP {status} for manual import candidates: {body}"
            ));
        }

        let candidates: Vec<ManualImportResource> = serde_json::from_str(&body).map_err(|err| {
            anyhow!("lidarr returned invalid manual import JSON: {err}; body: {body}")
        })?;
        let files = match select_manual_import_files(&request, candidates)? {
            Some(files) => files,
            None => {
                return Ok(ManualImportResult::Skipped {
                    reason: "lidarr did not return one complete album release match".into(),
                });
            }
        };
        let imported_track_count = files.len();
        let command = ManualImportCommand {
            name: "ManualImport",
            import_mode: "Move",
            replace_existing_files: true,
            files,
        };
        let command_body = serde_json::to_string(&command)
            .map_err(|err| anyhow!("failed serializing lidarr manual import command: {err}"))?;
        let response = self
            .client
            .post(format!("{}/api/v1/command", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("content-type", "application/json")
            .body(command_body)
            .send()
            .await
            .map_err(|err| anyhow!("failed starting lidarr manual import command: {err}"))?;
        let status = response.status();
        let body = response.text().await.map_err(|err| {
            anyhow!("failed reading lidarr manual import command response: {err}")
        })?;

        if !status.is_success() {
            return Err(anyhow!(
                "lidarr returned HTTP {status} for manual import command: {body}"
            ));
        }

        Ok(ManualImportResult::Started {
            imported_track_count,
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportResource {
    path: String,
    #[serde(default)]
    artist: Option<ManualImportArtist>,
    #[serde(default)]
    album: Option<ManualImportAlbum>,
    #[serde(default)]
    album_release_id: i64,
    #[serde(default)]
    tracks: Vec<ManualImportTrack>,
    #[serde(default)]
    quality: Option<Value>,
    #[serde(default)]
    indexer_flags: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportArtist {
    id: i64,
    #[serde(default)]
    artist_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportAlbum {
    id: i64,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportTrack {
    id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportCommand {
    name: &'static str,
    import_mode: &'static str,
    replace_existing_files: bool,
    files: Vec<ManualImportFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportFile {
    path: String,
    artist_id: i64,
    album_id: i64,
    album_release_id: i64,
    track_ids: Vec<i64>,
    quality: Value,
    indexer_flags: i64,
    download_id: String,
    disable_release_switching: bool,
}

fn select_manual_import_files(
    request: &ManualImportRequest,
    candidates: Vec<ManualImportResource>,
) -> Result<Option<Vec<ManualImportFile>>> {
    let mut selected = Vec::with_capacity(request.generated_tracks.len());

    for generated_track in &request.generated_tracks {
        let matches = candidates
            .iter()
            .filter(|candidate| paths_match(&candidate.path, generated_track))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Ok(None);
        }

        let Some(file) = manual_import_file(&request.download.download_id, matches[0]) else {
            return Ok(None);
        };
        selected.push(file);
    }

    if selected.is_empty() || !one_album_release(&selected) {
        return Ok(None);
    }
    if !cue_hints_match(request, &candidates, &selected) {
        return Ok(None);
    }

    Ok(Some(selected))
}

fn paths_match(candidate_path: &str, generated_track: &Path) -> bool {
    Path::new(candidate_path) == generated_track
}

fn manual_import_file(download_id: &str, item: &ManualImportResource) -> Option<ManualImportFile> {
    let artist_id = item.artist.as_ref()?.id;
    let album_id = item.album.as_ref()?.id;
    if item.album_release_id <= 0 {
        return None;
    }
    let quality = item.quality.clone()?;
    let track_ids = item.tracks.iter().map(|track| track.id).collect::<Vec<_>>();
    if track_ids.is_empty() {
        return None;
    }

    Some(ManualImportFile {
        path: item.path.clone(),
        artist_id,
        album_id,
        album_release_id: item.album_release_id,
        track_ids,
        quality,
        indexer_flags: item.indexer_flags,
        download_id: download_id.to_owned(),
        disable_release_switching: false,
    })
}

fn one_album_release(files: &[ManualImportFile]) -> bool {
    let first = &files[0];
    files.iter().all(|file| {
        file.artist_id == first.artist_id
            && file.album_id == first.album_id
            && file.album_release_id == first.album_release_id
    })
}

fn cue_hints_match(
    request: &ManualImportRequest,
    candidates: &[ManualImportResource],
    selected: &[ManualImportFile],
) -> bool {
    let expected_track_count = request
        .cue_hints
        .iter()
        .map(|hint| hint.track_count)
        .sum::<usize>();
    if expected_track_count > 0 && expected_track_count != selected.len() {
        return false;
    }

    let album_titles = request
        .cue_hints
        .iter()
        .filter_map(|hint| hint.album_title.as_deref())
        .map(normalize_hint)
        .collect::<HashSet<_>>();
    if album_titles.len() == 1 {
        let selected_album_titles = candidates
            .iter()
            .filter(|candidate| {
                selected
                    .iter()
                    .any(|file| paths_match(&candidate.path, Path::new(&file.path)))
            })
            .filter_map(|candidate| candidate.album.as_ref()?.title.as_deref())
            .map(normalize_hint)
            .collect::<HashSet<_>>();
        if !selected_album_titles.is_empty()
            && selected_album_titles
                .iter()
                .all(|title| !album_titles.contains(title))
        {
            return false;
        }
    }

    true
}

fn normalize_hint(value: &str) -> String {
    value.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::LidarrQueueSource;
    use crate::application::ports::{
        CueMetadataHint, ManualImportRequest, ManualImportResult, ManualImportTrigger, QueueSource,
    };
    use crate::bootstrap::settings::LidarrSettings;
    use crate::domain::{FailedImportCandidate, TrackedDownload};

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
            manual_import_enabled: false,
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
            manual_import_enabled: false,
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
            manual_import_enabled: false,
        });

        let queue = client.queue_snapshot().await.unwrap();
        assert_eq!(queue.pages_fetched, 1);
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn client_stops_when_empty_page_is_returned() {
        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"totalRecords":5,"records":[{"downloadId":"a"}]}"#,
            ),
            ("200 OK", r#"{"totalRecords":5,"records":[]}"#),
        ])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 1,
            queue_max_pages: 100,
            manual_import_enabled: false,
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
            manual_import_enabled: false,
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
            manual_import_enabled: false,
        });

        let err = client.queue_snapshot().await.unwrap_err();

        assert!(err.to_string().contains("invalid queue JSON"));
        assert!(err.to_string().contains("page 1"));
    }

    #[tokio::test]
    async fn client_errors_when_max_pages_is_exceeded() {
        let (url, _) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"totalRecords":999,"records":[{"downloadId":"a"}]}"#,
            ),
            (
                "200 OK",
                r#"{"totalRecords":999,"records":[{"downloadId":"b"}]}"#,
            ),
            (
                "200 OK",
                r#"{"totalRecords":999,"records":[{"downloadId":"c"}]}"#,
            ),
        ])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 1,
            queue_max_pages: 2,
            manual_import_enabled: false,
        });

        let err = client.queue_snapshot().await.unwrap_err();
        assert!(err.to_string().contains("exceeded max pages"));
    }

    #[tokio::test]
    async fn manual_import_disabled_makes_no_request() {
        let (url, requests) = serve_sequence(Vec::new()).await;
        let client = lidarr_client(url, false);

        let result = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap();

        assert_eq!(result, ManualImportResult::Disabled);
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn manual_import_starts_command_for_one_album_release() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1,"artistName":"Artist"},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}],"quality":{"quality":{"id":6,"name":"FLAC"},"revision":{"version":1,"real":0}},"indexerFlags":0},
            {"path":"/downloads/album/02.flac","artist":{"id":1,"artistName":"Artist"},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":12}],"quality":{"quality":{"id":6,"name":"FLAC"},"revision":{"version":1,"real":0}},"indexerFlags":0}
        ]"#;
        let (url, requests) =
            serve_sequence(vec![("200 OK", candidates), ("201 Created", r#"{"id":7}"#)]).await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request(
                vec!["/downloads/album/01.flac", "/downloads/album/02.flac"],
                2,
            ))
            .await
            .unwrap();

        assert_eq!(
            result,
            ManualImportResult::Started {
                imported_track_count: 2
            }
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /api/v1/manualimport?"));
        assert!(requests[0].contains("downloadId=download-1"));
        assert!(requests[1].starts_with("POST /api/v1/command "));

        let body = request_body(&requests[1]);
        let command: Value = serde_json::from_str(body).unwrap();
        assert_eq!(command["name"], "ManualImport");
        assert_eq!(command["importMode"], "Move");
        assert_eq!(command["replaceExistingFiles"], true);
        assert_eq!(command["files"].as_array().unwrap().len(), 2);
        assert_eq!(command["files"][0]["albumReleaseId"], 3);
        assert_eq!(command["files"][0]["trackIds"], serde_json::json!([11]));
        assert_eq!(command["files"][0]["downloadId"], "download-1");
    }

    #[tokio::test]
    async fn manual_import_skips_multiple_album_releases() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}],"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/02.flac","artist":{"id":1},"album":{"id":2,"title":"Album"},"albumReleaseId":4,"tracks":[{"id":12}],"quality":{"quality":{"id":6}}}
        ]"#;
        let (url, requests) = serve_sequence(vec![("200 OK", candidates)]).await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request(
                vec!["/downloads/album/01.flac", "/downloads/album/02.flac"],
                2,
            ))
            .await
            .unwrap();

        assert!(matches!(result, ManualImportResult::Skipped { .. }));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn manual_import_skips_incomplete_candidates() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}]}
        ]"#;
        let (url, requests) = serve_sequence(vec![("200 OK", candidates)]).await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap();

        assert!(matches!(result, ManualImportResult::Skipped { .. }));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn manual_import_reports_api_errors() {
        let url = serve_once("500 Internal Server Error", "boom").await;
        let client = lidarr_client(url, true);

        let err = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("HTTP 500"));
        assert!(err.to_string().contains("manual import candidates"));
    }

    fn lidarr_client(url: String, manual_import_enabled: bool) -> LidarrQueueSource {
        LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
            manual_import_enabled,
        })
    }

    fn manual_import_request(paths: Vec<&str>, track_count: usize) -> ManualImportRequest {
        ManualImportRequest {
            download: TrackedDownload::pending(
                "download-1".into(),
                "Artist - Album".into(),
                "completed".into(),
                "/downloads/album".into(),
                "importFailed".into(),
            ),
            generated_tracks: paths.into_iter().map(PathBuf::from).collect(),
            cue_hints: vec![CueMetadataHint {
                path: PathBuf::from("/downloads/album/album.cue"),
                album_title: Some("Album".into()),
                performer: Some("Artist".into()),
                catalog: None,
                comments: Vec::new(),
                track_count,
            }],
        }
    }

    fn request_body(request: &str) -> &str {
        request.split("\r\n\r\n").nth(1).unwrap_or_default()
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
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shared_requests = Arc::clone(&requests);
        let shared_responses = Arc::new(Mutex::new(
            responses
                .into_iter()
                .collect::<VecDeque<(&'static str, &'static str)>>(),
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
                shared_requests
                    .lock()
                    .unwrap()
                    .push(request_text.into_owned());

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        (format!("http://{addr}"), requests)
    }
}
