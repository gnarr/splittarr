use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::application::ports::{DownloadHistoryRow, DownloadReadStore};
use crate::domain::{
    CueSheet, CueSheetStatus, DownloadLifecycleState, GeneratedTrack, InputFile, InputFileKind,
    TrackCleanupStatus, TrackedDownload,
};

#[derive(Clone)]
struct WebState<S> {
    store: S,
}

pub fn router<S>(store: S) -> Router
where
    S: DownloadReadStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(index::<S>))
        .route("/healthz", get(healthz))
        .route("/downloads/{download_id}", get(download_detail::<S>))
        .route(
            "/downloads/{download_id}/content",
            get(download_detail_content::<S>),
        )
        .route("/downloads/{download_id}/row", get(download_row_route::<S>))
        .route("/downloads/rows", get(download_rows_route::<S>))
        .with_state(WebState { store })
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn index<S>(State(state): State<WebState<S>>) -> Response
where
    S: DownloadReadStore,
{
    match state.store.load_download_rows().await {
        Ok(downloads) => Html(page(
            "Splittarr",
            html! {
                h1 { "Splittarr" }
                section class="panel" {
                    h2 { "Download History" }
                    p id="downloads-empty" class="muted" hidden[!downloads.is_empty()] {
                        "No downloads have been tracked yet."
                    }
                    table id="downloads-table" hidden[downloads.is_empty()] {
                        thead {
                            tr {
                                th { "Download" }
                                th { "Lifecycle" }
                                th { "Lidarr" }
                                th { "Output Path" }
                                th { "Tracks" }
                                th { "Updated" }
                                th { "Completed" }
                            }
                        }
                        tbody id="downloads-rows" {
                            @for download in &downloads {
                                (download_row(download))
                            }
                        }
                    }
                }
                script { (PreEscaped(HISTORY_SCRIPT)) }
            },
        ))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load downloads: {error}"),
        )
            .into_response(),
    }
}

async fn download_row_route<S>(
    State(state): State<WebState<S>>,
    Path(download_id): Path<String>,
) -> impl IntoResponse
where
    S: DownloadReadStore,
{
    match state.store.load_download_row(&download_id).await {
        Ok(Some(download)) => download_row(&download).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "download not found").into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load download row: {error}"),
        )
            .into_response(),
    }
}

async fn download_rows_route<S>(State(state): State<WebState<S>>) -> impl IntoResponse
where
    S: DownloadReadStore,
{
    match state.store.load_download_rows().await {
        Ok(downloads) => download_rows(&downloads).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load download rows: {error}"),
        )
            .into_response(),
    }
}

async fn download_detail<S>(
    State(state): State<WebState<S>>,
    Path(download_id): Path<String>,
) -> Response
where
    S: DownloadReadStore,
{
    match state.store.get_tracked_download(&download_id).await {
        Ok(Some(download)) => Html(page(
            &download.title,
            html! {
                nav { a href="/" { "Download History" } }
                div id="download-detail-content" data-download-id=(&download.download_id) {
                    (download_content(&download))
                }
                script { (PreEscaped(DETAIL_SCRIPT)) }
            },
        ))
        .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "download not found").into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load download detail: {error}"),
        )
            .into_response(),
    }
}

async fn download_detail_content<S>(
    State(state): State<WebState<S>>,
    Path(download_id): Path<String>,
) -> impl IntoResponse
where
    S: DownloadReadStore,
{
    match state.store.get_tracked_download(&download_id).await {
        Ok(Some(download)) => download_content(&download).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "download not found").into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load download detail content: {error}"),
        )
            .into_response(),
    }
}

fn page(title: &str, body: Markup) -> String {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                style { (STYLE) }
            }
            body {
                main { (body) }
            }
        }
    }
    .into_string()
}

fn download_row(download: &DownloadHistoryRow) -> Markup {
    html! {
        tr id=(format!("download-row-{}", download.download_id)) data-download-id=(&download.download_id) {
            td {
                a href=(format!("/downloads/{}", download.download_id)) {
                    (&download.title)
                }
            }
            td { span class=(lifecycle_class(&download.lifecycle_state)) { (download.lifecycle_state.as_str()) } }
            td { (&download.status) " / " (&download.tracked_download_state) }
            td class="path" { (&download.output_path) }
            td { (download.generated_track_count) }
            td { (&download.updated_at) }
            td { (download.completed_at.as_deref().unwrap_or("-")) }
        }
    }
}

fn download_rows(downloads: &[DownloadHistoryRow]) -> Markup {
    html! {
        @for download in downloads {
            (download_row(download))
        }
    }
}

fn download_content(download: &TrackedDownload) -> Markup {
    html! {
        h1 { (&download.title) }
        section class="panel grid" {
            div { strong { "Lifecycle" } span class=(lifecycle_class(&download.lifecycle_state)) { (download.lifecycle_state.as_str()) } }
            div { strong { "Lidarr status" } span { (&download.status) } }
            div { strong { "Tracked state" } span { (&download.tracked_download_state) } }
            div { strong { "Generated tracks" } span { (download.generated_track_count()) } }
            div { strong { "First seen" } span { (download.first_seen_at.as_deref().unwrap_or("-")) } }
            div { strong { "Last seen in queue" } span { (download.last_seen_in_queue_at.as_deref().unwrap_or("-")) } }
            div { strong { "Processing started" } span { (download.processing_started_at.as_deref().unwrap_or("-")) } }
            div { strong { "Processing finished" } span { (download.processing_finished_at.as_deref().unwrap_or("-")) } }
            div { strong { "Cleanup started" } span { (download.cleanup_started_at.as_deref().unwrap_or("-")) } }
            div { strong { "Cleanup finished" } span { (download.cleanup_finished_at.as_deref().unwrap_or("-")) } }
            div { strong { "Completed at" } span { (download.completed_at.as_deref().unwrap_or("-")) } }
            div class="wide" { strong { "Output path" } span class="path" { (&download.output_path) } }
        }
        section class="panel" {
            h2 { "Last Error" }
            @if let Some(error) = &download.last_error {
                pre class="error-block" { (error) }
            } @else {
                p class="muted" { "No error recorded." }
            }
        }
        section class="panel" {
            h2 { "Input Files" }
            @if download.input_files.is_empty() {
                p class="muted" { "No input files have been recorded yet." }
            } @else {
                table {
                    thead {
                        tr {
                            th { "Kind" }
                            th { "Path" }
                            th { "Size" }
                            th { "Captured" }
                        }
                    }
                    tbody {
                        @for input in &download.input_files {
                            (input_row(input))
                        }
                    }
                }
            }
        }
        section class="panel" {
            h2 { "Cue Sheets" }
            @if download.cue_sheets.is_empty() {
                p class="muted" { "No cue sheets have been recorded yet." }
            } @else {
                @for cue in &download.cue_sheets {
                    (cue_card(cue))
                }
            }
        }
        section class="panel" {
            h2 { "Output Files" }
            @if download.generated_track_count() == 0 {
                p class="muted" { "No generated tracks have been recorded yet." }
            } @else {
                table {
                    thead {
                        tr {
                            th { "Path" }
                            th { "Size" }
                            th { "Cleanup" }
                            th { "Deleted At" }
                        }
                    }
                    tbody {
                        @for cue in &download.cue_sheets {
                            @for track in &cue.tracks {
                                (track_row(track))
                            }
                        }
                    }
                }
            }
        }
    }
}

fn input_row(input: &InputFile) -> Markup {
    html! {
        tr {
            td { (input_kind_label(input.kind)) }
            td class="path" { (&input.path) }
            td { (format_size(input.size_bytes)) }
            td { (&input.captured_at) }
        }
    }
}

fn cue_card(cue: &CueSheet) -> Markup {
    html! {
        article class="card" {
            h3 class="path" { (&cue.path) }
            p {
                span class=(cue_status_class(cue.status)) { (cue.status.as_str()) }
                " "
                span class="muted" { (&cue.updated_at) }
            }
            @if let Some(message) = &cue.message {
                pre { (message) }
            }
        }
    }
}

fn track_row(track: &GeneratedTrack) -> Markup {
    html! {
        tr {
            td class="path" { (&track.path) }
            td { (format_size(track.size_bytes)) }
            td {
                span class=(cleanup_class(track.cleanup_status)) { (track.cleanup_status.as_str()) }
                @if let Some(message) = &track.cleanup_message {
                    div class="muted" { (message) }
                }
            }
            td { (track.deleted_at.as_deref().unwrap_or("-")) }
        }
    }
}

fn input_kind_label(kind: InputFileKind) -> &'static str {
    match kind {
        InputFileKind::Cue => "cue",
        InputFileKind::Audio => "audio",
    }
}

fn lifecycle_class(state: &DownloadLifecycleState) -> &'static str {
    match state {
        DownloadLifecycleState::Completed => "status status-ok",
        DownloadLifecycleState::Failed => "status status-error",
        DownloadLifecycleState::AwaitingImport => "status status-warn",
        DownloadLifecycleState::Detected
        | DownloadLifecycleState::Processing
        | DownloadLifecycleState::CleaningUp => "status status-active",
    }
}

fn cue_status_class(state: CueSheetStatus) -> &'static str {
    match state {
        CueSheetStatus::Split => "status status-ok",
        CueSheetStatus::Failed => "status status-error",
        CueSheetStatus::Skipped => "status status-warn",
        CueSheetStatus::Pending => "status status-active",
    }
}

fn cleanup_class(status: TrackCleanupStatus) -> &'static str {
    match status {
        TrackCleanupStatus::Deleted => "status status-ok",
        TrackCleanupStatus::Missing => "status status-warn",
        TrackCleanupStatus::DeleteFailed => "status status-error",
        TrackCleanupStatus::Pending => "status status-active",
    }
}

fn format_size(size: Option<i64>) -> String {
    let Some(size) = size else {
        return "-".into();
    };
    let mut value = size as f64;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut index = 0;
    while value >= 1024.0 && index < units.len() - 1 {
        value /= 1024.0;
        index += 1;
    }
    format!("{value:.1} {}", units[index])
}

const HISTORY_SCRIPT: &str = r#"
const rows = document.getElementById("downloads-rows");
if (rows) {
  const table = document.getElementById("downloads-table");
  const empty = document.getElementById("downloads-empty");
  const refreshRows = async () => {
    const response = await fetch("/downloads/rows", { headers: { "x-requested-with": "fetch" } });
    if (!response.ok) return;
    const body = await response.text();
    rows.innerHTML = body;
    const hasRows = body.trim().length > 0;
    if (table) table.hidden = !hasRows;
    if (empty) empty.hidden = hasRows;
  };
  refreshRows();
  setInterval(refreshRows, 10000);
}
"#;

const DETAIL_SCRIPT: &str = r#"
const detailContent = document.getElementById("download-detail-content");
if (detailContent) {
  const id = detailContent.dataset.downloadId;
  const refreshDetail = async () => {
    if (!id) return;
    const response = await fetch(`/downloads/${id}/content`, { headers: { "x-requested-with": "fetch" } });
    if (!response.ok) return;
    detailContent.innerHTML = await response.text();
  };
  setInterval(refreshDetail, 10000);
}
"#;

const STYLE: &str = r#"
:root {
  color-scheme: light dark;
  --bg: #f7f7f4;
  --panel: #ffffff;
  --text: #1f2428;
  --muted: #667076;
  --border: #d7d9d7;
  --accent: #2f6f73;
  --ok: #1c7c54;
  --warn: #946200;
  --error: #a83232;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #171918;
    --panel: #202322;
    --text: #edf0ee;
    --muted: #aab2ae;
    --border: #3a403d;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--text);
  font: 15px/1.45 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
main {
  width: min(1160px, calc(100vw - 32px));
  margin: 32px auto;
}
h1 { margin: 0 0 18px; font-size: 30px; }
h2 { margin: 0 0 14px; font-size: 18px; }
h3 { margin: 0 0 8px; font-size: 15px; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
nav { margin-bottom: 14px; }
.panel {
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 18px;
  margin: 14px 0;
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(230px, 1fr));
  gap: 12px;
}
.grid div {
  display: grid;
  gap: 4px;
}
.grid .wide {
  grid-column: 1 / -1;
}
.grid strong {
  color: var(--muted);
  font-size: 12px;
  text-transform: uppercase;
}
table {
  width: 100%;
  border-collapse: collapse;
}
th, td {
  padding: 10px 8px;
  border-bottom: 1px solid var(--border);
  text-align: left;
  vertical-align: top;
}
th {
  color: var(--muted);
  font-size: 12px;
  text-transform: uppercase;
}
.status {
  display: inline-block;
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 2px 8px;
  font-size: 12px;
}
.status-ok { color: var(--ok); border-color: var(--ok); }
.status-warn { color: var(--warn); border-color: var(--warn); }
.status-error { color: var(--error); border-color: var(--error); }
.status-active { color: var(--accent); border-color: var(--accent); }
.muted { color: var(--muted); }
.path { word-break: break-all; font-family: "SFMono-Regular", Consolas, monospace; }
.card {
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 14px;
  margin-top: 12px;
}
pre {
  overflow: auto;
  max-height: 520px;
  padding: 12px;
  border-radius: 6px;
  border: 1px solid var(--border);
  background: color-mix(in srgb, var(--bg), var(--panel) 35%);
}
.error-block { color: var(--error); }
"#;

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::router;
    use crate::application::ports::{DownloadHistoryRow, DownloadReadStore};
    use crate::domain::{
        CueSheet, CueSheetStatus, DownloadLifecycleState, GeneratedTrack, InputFile, InputFileKind,
        TrackCleanupStatus, TrackedDownload,
    };

    #[derive(Clone, Default)]
    struct FakeReadStore {
        rows: Vec<DownloadHistoryRow>,
        detail: Option<TrackedDownload>,
    }

    impl DownloadReadStore for FakeReadStore {
        async fn load_download_rows(&self) -> anyhow::Result<Vec<DownloadHistoryRow>> {
            Ok(self.rows.clone())
        }

        async fn load_download_row(
            &self,
            download_id: &str,
        ) -> anyhow::Result<Option<DownloadHistoryRow>> {
            Ok(self
                .rows
                .iter()
                .find(|row| row.download_id == download_id)
                .cloned())
        }

        async fn get_tracked_download(
            &self,
            download_id: &str,
        ) -> anyhow::Result<Option<TrackedDownload>> {
            Ok(self
                .detail
                .as_ref()
                .filter(|download| download.download_id == download_id)
                .cloned())
        }
    }

    #[tokio::test]
    async fn index_renders_empty_state_with_fake_read_store() {
        let app = router(FakeReadStore::default());

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rendered = String::from_utf8(body.to_vec()).unwrap();
        assert!(rendered.contains("No downloads have been tracked yet."));
    }

    #[tokio::test]
    async fn detail_renders_recorded_files() {
        let mut download = TrackedDownload::pending(
            "abc".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );
        download.input_files = vec![InputFile {
            id: "input-1".into(),
            download_id: "abc".into(),
            cue_sheet_id: Some("cue-1".into()),
            path: "/downloads/album/album.cue".into(),
            kind: InputFileKind::Cue,
            size_bytes: Some(12),
            captured_at: "2026-06-12 12:00:00".into(),
        }];
        download.cue_sheets = vec![CueSheet {
            id: "cue-1".into(),
            download_id: "abc".into(),
            path: "/downloads/album/album.cue".into(),
            status: CueSheetStatus::Split,
            message: None,
            updated_at: "2026-06-12 12:00:00".into(),
            tracks: vec![GeneratedTrack {
                id: "track-1".into(),
                cue_sheet_id: "cue-1".into(),
                download_id: "abc".into(),
                path: "/downloads/album/01.flac".into(),
                size_bytes: Some(64),
                cleanup_status: TrackCleanupStatus::Pending,
                cleanup_message: None,
                deleted_at: None,
            }],
        }];

        let app = router(FakeReadStore {
            detail: Some(download),
            ..FakeReadStore::default()
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/downloads/abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rendered = String::from_utf8(body.to_vec()).unwrap();
        assert!(rendered.contains("Input Files"));
        assert!(rendered.contains("/downloads/album/album.cue"));
        assert!(rendered.contains("/downloads/album/01.flac"));
    }

    #[tokio::test]
    async fn rows_endpoint_renders_all_rows_in_one_response() {
        let app = router(FakeReadStore {
            rows: vec![DownloadHistoryRow {
                download_id: "abc".into(),
                title: "Album".into(),
                status: "completed".into(),
                output_path: "/downloads/album".into(),
                tracked_download_state: "importFailed".into(),
                lifecycle_state: DownloadLifecycleState::Detected,
                updated_at: "2026-06-12 12:00:00".into(),
                completed_at: None,
                generated_track_count: 0,
            }],
            detail: None,
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/downloads/rows")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rendered = String::from_utf8(body.to_vec()).unwrap();

        assert!(rendered.contains("download-row-abc"));
        assert!(rendered.contains("/downloads/abc"));
        assert!(rendered.contains("0"));
    }
}
