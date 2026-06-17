use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use rusqlite::{named_params, params, params_from_iter, Connection, OptionalExtension};
use uuid::Uuid;

use crate::application::ports::{
    DownloadHistoryRow, DownloadReadStore, DownloadStats, DownloadStore,
};
use crate::domain::{
    CueSheet, CueSheetStatus, DownloadLifecycleState, GeneratedTrack, InputFile, InputFileKind,
    RecordedTrack, TrackCleanupOutcome, TrackCleanupStatus, TrackedDownload,
};

#[derive(Debug, Clone)]
pub struct SqliteDownloadStore {
    db_path: PathBuf,
}

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

impl SqliteDownloadStore {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(data_dir.as_ref())?;
        let db_path = data_dir.as_ref().join("data.db");
        let mut conn = Connection::open(&db_path)?;
        configure_connection(&conn)?;
        migrate(&mut conn)?;
        Ok(Self { db_path })
    }

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path)?;
        configure_connection(&conn)?;
        Ok(conn)
    }

    fn load_tracked_downloads_sync(&self) -> Result<Vec<TrackedDownload>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT download_id, title, status, output_path, tracked_download_state,
                    lifecycle_state, created_at, updated_at, first_seen_at, last_seen_in_queue_at,
                    processing_started_at, processing_finished_at, cleanup_started_at,
                    cleanup_finished_at, completed_at, last_error
             FROM downloads
             ORDER BY updated_at DESC, download_id DESC",
        )?;
        let rows = stmt.query_map([], |row| map_download_row(&conn, row))?;

        let mut downloads = Vec::new();
        for row in rows {
            downloads.push(row?);
        }
        Ok(downloads)
    }

    fn load_tracked_download_summaries_sync(&self) -> Result<Vec<TrackedDownload>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT d.download_id, d.title, d.status, d.output_path, d.tracked_download_state,
                    d.lifecycle_state, d.created_at, d.updated_at, d.first_seen_at,
                    d.last_seen_in_queue_at, d.processing_started_at, d.processing_finished_at,
                    d.cleanup_started_at, d.cleanup_finished_at, d.completed_at, d.last_error,
                    COUNT(t.id) AS generated_track_count
             FROM downloads d
             LEFT JOIN cue_files c ON c.download_id = d.download_id
             LEFT JOIN tracks t ON t.cue_file_id = c.id
             GROUP BY d.download_id, d.title, d.status, d.output_path, d.tracked_download_state,
                      d.lifecycle_state, d.created_at, d.updated_at, d.first_seen_at,
                      d.last_seen_in_queue_at, d.processing_started_at, d.processing_finished_at,
                      d.cleanup_started_at, d.cleanup_finished_at, d.completed_at, d.last_error
             ORDER BY d.updated_at DESC, d.download_id DESC",
        )?;
        let rows = stmt.query_map([], map_download_summary_row)?;

        let mut downloads = Vec::new();
        for row in rows {
            downloads.push(row?);
        }
        Ok(downloads)
    }

    fn load_download_rows_sync(&self) -> Result<Vec<DownloadHistoryRow>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT d.download_id, d.title, d.status, d.output_path, d.tracked_download_state,
                    d.lifecycle_state, d.updated_at, d.completed_at, COUNT(t.id) AS generated_track_count
             FROM downloads d
             LEFT JOIN cue_files c ON c.download_id = d.download_id
             LEFT JOIN tracks t ON t.cue_file_id = c.id
             GROUP BY d.download_id, d.title, d.status, d.output_path, d.tracked_download_state,
                      d.lifecycle_state, d.updated_at, d.completed_at
             ORDER BY d.updated_at DESC, d.download_id DESC",
        )?;
        let rows = stmt.query_map([], map_download_history_row)?;

        let mut download_rows = Vec::new();
        for row in rows {
            download_rows.push(row?);
        }
        Ok(download_rows)
    }

    fn load_download_row_sync(&self, download_id: &str) -> Result<Option<DownloadHistoryRow>> {
        let conn = self.connect()?;
        Ok(conn
            .query_row(
                "SELECT d.download_id, d.title, d.status, d.output_path, d.tracked_download_state,
                        d.lifecycle_state, d.updated_at, d.completed_at, COUNT(t.id) AS generated_track_count
                 FROM downloads d
                 LEFT JOIN cue_files c ON c.download_id = d.download_id
                 LEFT JOIN tracks t ON t.cue_file_id = c.id
                 WHERE d.download_id = ?
                 GROUP BY d.download_id, d.title, d.status, d.output_path, d.tracked_download_state,
                          d.lifecycle_state, d.updated_at, d.completed_at",
                [download_id],
                map_download_history_row,
            )
            .optional()?)
    }

    fn get_tracked_download_sync(&self, download_id: &str) -> Result<Option<TrackedDownload>> {
        let conn = self.connect()?;
        Ok(conn
            .query_row(
                "SELECT download_id, title, status, output_path, tracked_download_state,
                    lifecycle_state, created_at, updated_at, first_seen_at, last_seen_in_queue_at,
                    processing_started_at, processing_finished_at, cleanup_started_at,
                    cleanup_finished_at, completed_at, last_error
             FROM downloads
             WHERE download_id = ?",
                [download_id],
                |row| map_download_row(&conn, row),
            )
            .optional()?)
    }

    fn get_tracked_downloads_sync(&self, download_ids: &[String]) -> Result<Vec<TrackedDownload>> {
        use std::collections::HashMap;

        if download_ids.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.connect()?;
        let placeholders = std::iter::repeat_n("?", download_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT download_id, title, status, output_path, tracked_download_state,
                    lifecycle_state, created_at, updated_at, first_seen_at, last_seen_in_queue_at,
                    processing_started_at, processing_finished_at, cleanup_started_at,
                    cleanup_finished_at, completed_at, last_error
             FROM downloads
             WHERE download_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&query)?;
        let rows = stmt.query_map(params_from_iter(download_ids.iter()), |row| {
            map_download_row(&conn, row)
        })?;

        let mut by_id = HashMap::new();
        for row in rows {
            let download = row?;
            by_id.insert(download.download_id.clone(), download);
        }

        let mut downloads = Vec::new();
        for download_id in download_ids {
            if let Some(download) = by_id.remove(download_id) {
                downloads.push(download);
            }
        }
        Ok(downloads)
    }

    fn load_download_stats_sync(&self) -> Result<DownloadStats> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(lifecycle_state, 'detected'), COUNT(*) FROM downloads GROUP BY lifecycle_state",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut stats = DownloadStats::default();
        for row in rows {
            let (state, count) = row?;
            let count = count as usize;
            stats.total += count;
            match download_lifecycle_state_from_db(&state) {
                DownloadLifecycleState::Completed => stats.completed += count,
                DownloadLifecycleState::Failed => stats.failed += count,
                DownloadLifecycleState::AwaitingImport => stats.awaiting_import += count,
                DownloadLifecycleState::Detected
                | DownloadLifecycleState::Processing
                | DownloadLifecycleState::CleaningUp => stats.in_progress += count,
            }
        }
        Ok(stats)
    }

    fn upsert_tracked_download_sync(&self, download: &TrackedDownload) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO downloads (
                download_id, title, status, output_path, tracked_download_state, lifecycle_state,
                created_at, updated_at, first_seen_at, last_seen_in_queue_at, last_error
             )
             VALUES (
                :download_id, :title, :status, :output_path, :tracked_download_state, :lifecycle_state,
                CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, :last_error
             )
             ON CONFLICT(download_id) DO UPDATE SET
                title = excluded.title,
                status = excluded.status,
                output_path = excluded.output_path,
                tracked_download_state = excluded.tracked_download_state,
                lifecycle_state = excluded.lifecycle_state,
                last_seen_in_queue_at = CURRENT_TIMESTAMP,
                last_error = excluded.last_error,
                updated_at = CURRENT_TIMESTAMP",
            named_params! {
                ":download_id": &download.download_id,
                ":title": &download.title,
                ":status": &download.status,
                ":output_path": &download.output_path,
                ":tracked_download_state": &download.tracked_download_state,
                ":lifecycle_state": download_lifecycle_state_to_db(&download.lifecycle_state),
                ":last_error": &download.last_error,
            },
        )?;
        Ok(())
    }

    fn mark_download_processing_sync(&self, download_id: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE downloads
             SET lifecycle_state = 'processing',
                 processing_started_at = COALESCE(processing_started_at, CURRENT_TIMESTAMP),
                 last_error = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?",
            [download_id],
        )?;
        Ok(())
    }

    fn mark_download_awaiting_import_sync(&self, download_id: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE downloads
             SET lifecycle_state = 'awaiting_import',
                 processing_finished_at = COALESCE(processing_finished_at, CURRENT_TIMESTAMP),
                 last_error = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?",
            [download_id],
        )?;
        Ok(())
    }

    fn mark_download_cleanup_started_sync(&self, download_id: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE downloads
             SET lifecycle_state = 'cleaning_up',
                 cleanup_started_at = COALESCE(cleanup_started_at, CURRENT_TIMESTAMP),
                 last_error = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?",
            [download_id],
        )?;
        Ok(())
    }

    fn mark_download_completed_sync(&self, download_id: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE downloads
             SET lifecycle_state = 'completed',
                 cleanup_finished_at = CURRENT_TIMESTAMP,
                 completed_at = CURRENT_TIMESTAMP,
                 last_error = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?",
            [download_id],
        )?;
        Ok(())
    }

    fn mark_download_failed_sync(&self, download_id: &str, last_error: Option<&str>) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE downloads
             SET lifecycle_state = 'failed',
                 processing_finished_at = COALESCE(processing_finished_at, CURRENT_TIMESTAMP),
                 cleanup_finished_at = CASE
                     WHEN lifecycle_state = 'cleaning_up' THEN CURRENT_TIMESTAMP
                     ELSE cleanup_finished_at
                 END,
                 last_error = ?2,
                 updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?1",
            params![download_id, last_error],
        )?;
        Ok(())
    }

    fn record_download_warning_sync(&self, download_id: &str, message: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE downloads
             SET last_error = ?2,
                 updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?1",
            params![download_id, message],
        )?;
        Ok(())
    }

    fn get_or_create_cue_sheet_sync(&self, download_id: &str, path: &Path) -> Result<CueSheet> {
        let conn = self.connect()?;
        let path = path.to_string_lossy().to_string();
        let id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT OR IGNORE INTO cue_files (id, path, download_id, status, message, updated_at)
             VALUES (:id, :path, :download_id, :status, NULL, CURRENT_TIMESTAMP)",
            named_params! {
                ":id": id,
                ":path": &path,
                ":download_id": download_id,
                ":status": cue_sheet_status_to_db(CueSheetStatus::Pending),
            },
        )?;

        cue_sheet_by_download_and_path(&conn, download_id, &path)?
            .ok_or_else(|| anyhow!("cue file row disappeared after insert: {path}"))
    }

    fn record_input_file_sync(
        &self,
        download_id: &str,
        cue_sheet_id: Option<&str>,
        path: &Path,
        kind: InputFileKind,
        size_bytes: Option<i64>,
    ) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO input_files (id, download_id, cue_file_id, path, kind, size_bytes, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)
             ON CONFLICT(download_id, path) DO UPDATE SET
                cue_file_id = excluded.cue_file_id,
                kind = excluded.kind,
                size_bytes = excluded.size_bytes,
                captured_at = CURRENT_TIMESTAMP",
            params![
                Uuid::new_v4().to_string(),
                download_id,
                cue_sheet_id,
                path.to_string_lossy().to_string(),
                input_file_kind_to_db(kind),
                size_bytes,
            ],
        )?;
        Ok(())
    }

    fn record_cue_result_sync(
        &self,
        cue_sheet: &CueSheet,
        status: CueSheetStatus,
        message: Option<&str>,
        tracks: &[RecordedTrack],
    ) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE cue_files
             SET status = :status,
                 message = :message,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = :id",
            named_params! {
                ":id": &cue_sheet.id,
                ":status": cue_sheet_status_to_db(status),
                ":message": message,
            },
        )?;

        for track in tracks {
            tx.execute(
                "INSERT INTO tracks (
                    id, path, cue_file_id, download_id, size_bytes, cleanup_status, cleanup_message, deleted_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, NULL)
                 ON CONFLICT(download_id, path) DO UPDATE SET
                    cue_file_id = excluded.cue_file_id,
                    size_bytes = excluded.size_bytes,
                    cleanup_status = excluded.cleanup_status,
                    cleanup_message = excluded.cleanup_message,
                    deleted_at = excluded.deleted_at",
                params![
                    Uuid::new_v4().to_string(),
                    &track.path,
                    &cue_sheet.id,
                    &cue_sheet.download_id,
                    track.size_bytes,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    fn record_track_cleanup_sync(
        &self,
        download_id: &str,
        track_id: &str,
        status: TrackCleanupStatus,
        message: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE tracks
             SET cleanup_status = ?2,
                 cleanup_message = ?3,
                 deleted_at = CASE
                     WHEN ?2 IN ('deleted', 'missing') THEN CURRENT_TIMESTAMP
                     ELSE NULL
                 END
             WHERE id = ?1",
            params![track_id, track_cleanup_status_to_db(status), message],
        )?;
        tx.execute(
            "UPDATE downloads
             SET updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?",
            [download_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn record_track_cleanups_sync(
        &self,
        download_id: &str,
        outcomes: &[TrackCleanupOutcome],
    ) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        for outcome in outcomes {
            tx.execute(
                "UPDATE tracks
                 SET cleanup_status = ?2,
                     cleanup_message = ?3,
                     deleted_at = CASE
                         WHEN ?2 IN ('deleted', 'missing') THEN CURRENT_TIMESTAMP
                         ELSE NULL
                     END
                 WHERE id = ?1",
                params![
                    &outcome.track_id,
                    track_cleanup_status_to_db(outcome.status),
                    outcome.message.as_deref()
                ],
            )?;
        }
        tx.execute(
            "UPDATE downloads
             SET updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?",
            [download_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}

impl DownloadStore for SqliteDownloadStore {
    async fn load_tracked_downloads(&self) -> Result<Vec<TrackedDownload>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.load_tracked_downloads_sync())
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn load_tracked_download_summaries(&self) -> Result<Vec<TrackedDownload>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.load_tracked_download_summaries_sync())
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn get_tracked_download(&self, download_id: &str) -> Result<Option<TrackedDownload>> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.get_tracked_download_sync(&download_id))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn get_tracked_downloads(&self, download_ids: &[String]) -> Result<Vec<TrackedDownload>> {
        let store = self.clone();
        let download_ids = download_ids.to_vec();
        tokio::task::spawn_blocking(move || store.get_tracked_downloads_sync(&download_ids))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn upsert_tracked_download(&self, download: &TrackedDownload) -> Result<()> {
        let store = self.clone();
        let download = download.clone();
        tokio::task::spawn_blocking(move || store.upsert_tracked_download_sync(&download))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn mark_download_processing(&self, download_id: &str) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.mark_download_processing_sync(&download_id))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn mark_download_awaiting_import(&self, download_id: &str) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.mark_download_awaiting_import_sync(&download_id))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn mark_download_cleanup_started(&self, download_id: &str) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.mark_download_cleanup_started_sync(&download_id))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn mark_download_completed(&self, download_id: &str) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.mark_download_completed_sync(&download_id))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn mark_download_failed(
        &self,
        download_id: &str,
        last_error: Option<&str>,
    ) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        let last_error = last_error.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            store.mark_download_failed_sync(&download_id, last_error.as_deref())
        })
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn record_download_warning(&self, download_id: &str, message: &str) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        let message = message.to_owned();
        tokio::task::spawn_blocking(move || {
            store.record_download_warning_sync(&download_id, &message)
        })
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn get_or_create_cue_sheet(&self, download_id: &str, path: &Path) -> Result<CueSheet> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || store.get_or_create_cue_sheet_sync(&download_id, &path))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn record_input_file(
        &self,
        download_id: &str,
        cue_sheet_id: Option<&str>,
        path: &Path,
        kind: InputFileKind,
        size_bytes: Option<i64>,
    ) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        let cue_sheet_id = cue_sheet_id.map(str::to_owned);
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            store.record_input_file_sync(
                &download_id,
                cue_sheet_id.as_deref(),
                &path,
                kind,
                size_bytes,
            )
        })
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn record_cue_result(
        &self,
        cue_sheet: &CueSheet,
        status: CueSheetStatus,
        message: Option<&str>,
        tracks: &[RecordedTrack],
    ) -> Result<()> {
        let store = self.clone();
        let cue_sheet = cue_sheet.clone();
        let message = message.map(str::to_owned);
        let tracks = tracks.to_vec();
        tokio::task::spawn_blocking(move || {
            store.record_cue_result_sync(&cue_sheet, status, message.as_deref(), &tracks)
        })
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn record_track_cleanup(
        &self,
        download_id: &str,
        track_id: &str,
        status: TrackCleanupStatus,
        message: Option<&str>,
    ) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        let track_id = track_id.to_owned();
        let message = message.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            store.record_track_cleanup_sync(&download_id, &track_id, status, message.as_deref())
        })
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn record_track_cleanups(
        &self,
        download_id: &str,
        outcomes: &[TrackCleanupOutcome],
    ) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        let outcomes = outcomes.to_vec();
        tokio::task::spawn_blocking(move || {
            store.record_track_cleanups_sync(&download_id, &outcomes)
        })
        .await
        .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }
}

#[async_trait]
impl DownloadReadStore for SqliteDownloadStore {
    async fn load_download_rows(&self) -> Result<Vec<DownloadHistoryRow>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.load_download_rows_sync())
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn load_download_row(&self, download_id: &str) -> Result<Option<DownloadHistoryRow>> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.load_download_row_sync(&download_id))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn get_tracked_download(&self, download_id: &str) -> Result<Option<TrackedDownload>> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.get_tracked_download_sync(&download_id))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn load_download_stats(&self) -> Result<DownloadStats> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.load_download_stats_sync())
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }
}

fn map_download_row(
    conn: &Connection,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<TrackedDownload> {
    let download_id: String = row.get(0)?;
    let cue_sheets = cue_sheets_for(conn, &download_id)?;
    let generated_track_count = cue_sheets.iter().map(|cue| cue.tracks.len()).sum();
    Ok(TrackedDownload {
        input_files: input_files_for(conn, &download_id)?,
        cue_sheets,
        download_id,
        title: row.get(1)?,
        status: row.get(2)?,
        output_path: row.get(3)?,
        tracked_download_state: row.get(4)?,
        lifecycle_state: lifecycle_state_from_row(row, 5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        first_seen_at: row.get(8)?,
        last_seen_in_queue_at: row.get(9)?,
        processing_started_at: row.get(10)?,
        processing_finished_at: row.get(11)?,
        cleanup_started_at: row.get(12)?,
        cleanup_finished_at: row.get(13)?,
        completed_at: row.get(14)?,
        generated_track_count,
        last_error: row.get(15)?,
    })
}

fn map_download_history_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DownloadHistoryRow> {
    Ok(DownloadHistoryRow {
        download_id: row.get(0)?,
        title: row.get(1)?,
        status: row.get(2)?,
        output_path: row.get(3)?,
        tracked_download_state: row.get(4)?,
        lifecycle_state: lifecycle_state_from_row(row, 5)?,
        updated_at: row.get(6)?,
        completed_at: row.get(7)?,
        generated_track_count: row.get::<_, i64>(8)? as usize,
    })
}

fn map_download_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackedDownload> {
    Ok(TrackedDownload {
        input_files: Vec::new(),
        cue_sheets: Vec::new(),
        download_id: row.get(0)?,
        title: row.get(1)?,
        status: row.get(2)?,
        output_path: row.get(3)?,
        tracked_download_state: row.get(4)?,
        lifecycle_state: lifecycle_state_from_row(row, 5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        first_seen_at: row.get(8)?,
        last_seen_in_queue_at: row.get(9)?,
        processing_started_at: row.get(10)?,
        processing_finished_at: row.get(11)?,
        cleanup_started_at: row.get(12)?,
        cleanup_finished_at: row.get(13)?,
        completed_at: row.get(14)?,
        generated_track_count: row.get::<_, i64>(16)? as usize,
        last_error: row.get(15)?,
    })
}

fn download_lifecycle_state_to_db(state: &DownloadLifecycleState) -> &'static str {
    match state {
        DownloadLifecycleState::Detected => "detected",
        DownloadLifecycleState::Processing => "processing",
        DownloadLifecycleState::AwaitingImport => "awaiting_import",
        DownloadLifecycleState::CleaningUp => "cleaning_up",
        DownloadLifecycleState::Completed => "completed",
        DownloadLifecycleState::Failed => "failed",
    }
}

fn download_lifecycle_state_from_db(value: &str) -> DownloadLifecycleState {
    match value {
        "processing" => DownloadLifecycleState::Processing,
        "awaiting_import" => DownloadLifecycleState::AwaitingImport,
        "cleaning_up" => DownloadLifecycleState::CleaningUp,
        "completed" => DownloadLifecycleState::Completed,
        "failed" => DownloadLifecycleState::Failed,
        _ => DownloadLifecycleState::Detected,
    }
}

fn cue_sheet_status_to_db(status: CueSheetStatus) -> &'static str {
    match status {
        CueSheetStatus::Pending => "pending",
        CueSheetStatus::Split => "split",
        CueSheetStatus::Skipped => "skipped",
        CueSheetStatus::Failed => "failed",
    }
}

fn cue_sheet_status_from_db(value: &str) -> CueSheetStatus {
    match value {
        "split" => CueSheetStatus::Split,
        "skipped" => CueSheetStatus::Skipped,
        "failed" => CueSheetStatus::Failed,
        _ => CueSheetStatus::Pending,
    }
}

fn input_file_kind_to_db(kind: InputFileKind) -> &'static str {
    match kind {
        InputFileKind::Cue => "cue",
        InputFileKind::Audio => "audio",
    }
}

fn input_file_kind_from_db(value: &str) -> InputFileKind {
    match value {
        "audio" => InputFileKind::Audio,
        _ => InputFileKind::Cue,
    }
}

fn track_cleanup_status_to_db(status: TrackCleanupStatus) -> &'static str {
    match status {
        TrackCleanupStatus::Pending => "pending",
        TrackCleanupStatus::Deleted => "deleted",
        TrackCleanupStatus::DeleteFailed => "delete_failed",
        TrackCleanupStatus::Missing => "missing",
    }
}

fn track_cleanup_status_from_db(value: &str) -> TrackCleanupStatus {
    match value {
        "deleted" => TrackCleanupStatus::Deleted,
        "delete_failed" => TrackCleanupStatus::DeleteFailed,
        "missing" => TrackCleanupStatus::Missing,
        _ => TrackCleanupStatus::Pending,
    }
}

fn lifecycle_state_from_row(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<DownloadLifecycleState> {
    Ok(download_lifecycle_state_from_db(
        row.get::<_, Option<String>>(index)?
            .as_deref()
            .unwrap_or("detected"),
    ))
}

fn cue_sheets_for(conn: &Connection, download_id: &str) -> rusqlite::Result<Vec<CueSheet>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, download_id, status, message, updated_at
         FROM cue_files
         WHERE download_id = ?
         ORDER BY path",
    )?;
    let rows = stmt.query_map([download_id], |row| {
        let id: String = row.get(0)?;
        Ok(CueSheet {
            tracks: tracks_for(conn, &id)?,
            id,
            path: row.get(1)?,
            download_id: row.get(2)?,
            status: cue_sheet_status_from_db(row.get::<_, String>(3)?.as_str()),
            message: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;

    let mut cue_sheets = Vec::new();
    for row in rows {
        cue_sheets.push(row?);
    }
    Ok(cue_sheets)
}

fn input_files_for(conn: &Connection, download_id: &str) -> rusqlite::Result<Vec<InputFile>> {
    let mut stmt = conn.prepare(
        "SELECT id, download_id, cue_file_id, path, kind, size_bytes, captured_at
         FROM input_files
         WHERE download_id = ?
         ORDER BY kind, path",
    )?;
    let rows = stmt.query_map([download_id], |row| {
        Ok(InputFile {
            id: row.get(0)?,
            download_id: row.get(1)?,
            cue_sheet_id: row.get(2)?,
            path: row.get(3)?,
            kind: input_file_kind_from_db(row.get::<_, String>(4)?.as_str()),
            size_bytes: row.get(5)?,
            captured_at: row.get(6)?,
        })
    })?;

    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

fn cue_sheet_by_download_and_path(
    conn: &Connection,
    download_id: &str,
    path: &str,
) -> rusqlite::Result<Option<CueSheet>> {
    conn.query_row(
        "SELECT id, path, download_id, status, message, updated_at
         FROM cue_files
         WHERE download_id = ? AND path = ?",
        params![download_id, path],
        |row| {
            let id: String = row.get(0)?;
            Ok(CueSheet {
                tracks: tracks_for(conn, &id)?,
                id,
                path: row.get(1)?,
                download_id: row.get(2)?,
                status: cue_sheet_status_from_db(row.get::<_, String>(3)?.as_str()),
                message: row.get(4)?,
                updated_at: row.get(5)?,
            })
        },
    )
    .optional()
}

fn tracks_for(
    conn: &Connection,
    cue_sheet_id: &str,
) -> Result<Vec<GeneratedTrack>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, cue_file_id, download_id, path, size_bytes, cleanup_status, cleanup_message, deleted_at
         FROM tracks
         WHERE cue_file_id = ?
         ORDER BY path",
    )?;
    let rows = stmt.query_map([cue_sheet_id], |row| {
        Ok(GeneratedTrack {
            id: row.get(0)?,
            cue_sheet_id: row.get(1)?,
            download_id: row.get(2)?,
            path: row.get(3)?,
            size_bytes: row.get(4)?,
            cleanup_status: track_cleanup_status_from_db(row.get::<_, String>(5)?.as_str()),
            cleanup_message: row.get(6)?,
            deleted_at: row.get(7)?,
        })
    })?;

    let mut tracks = Vec::new();
    for row in rows {
        tracks.push(row?);
    }
    Ok(tracks)
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS downloads (
            download_id            TEXT PRIMARY KEY,
            title                  TEXT NOT NULL,
            status                 TEXT NOT NULL,
            output_path            TEXT NOT NULL,
            tracked_download_state TEXT NOT NULL,
            lifecycle_state        TEXT,
            split_complete         BOOLEAN NOT NULL DEFAULT FALSE,
            created_at             TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at             TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            first_seen_at          TEXT,
            last_seen_in_queue_at  TEXT,
            processing_started_at  TEXT,
            processing_finished_at TEXT,
            cleanup_started_at     TEXT,
            cleanup_finished_at    TEXT,
            completed_at           TEXT,
            last_error             TEXT
        );

        CREATE TABLE IF NOT EXISTS cue_files (
            id          TEXT PRIMARY KEY,
            path        TEXT NOT NULL,
            download_id TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'pending',
            message     TEXT,
            updated_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY(download_id) REFERENCES downloads(download_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS input_files (
            id          TEXT PRIMARY KEY,
            download_id TEXT NOT NULL,
            cue_file_id TEXT,
            path        TEXT NOT NULL,
            kind        TEXT NOT NULL,
            size_bytes  INTEGER,
            captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY(download_id) REFERENCES downloads(download_id) ON DELETE CASCADE,
            FOREIGN KEY(cue_file_id) REFERENCES cue_files(id) ON DELETE SET NULL
        );

        CREATE TABLE IF NOT EXISTS tracks (
            id              TEXT PRIMARY KEY,
            path            TEXT NOT NULL,
            cue_file_id     TEXT NOT NULL,
            download_id     TEXT NOT NULL,
            size_bytes      INTEGER,
            cleanup_status  TEXT NOT NULL DEFAULT 'pending',
            cleanup_message TEXT,
            deleted_at      TEXT,
            FOREIGN KEY(cue_file_id) REFERENCES cue_files(id) ON DELETE CASCADE,
            FOREIGN KEY(download_id) REFERENCES downloads(download_id) ON DELETE CASCADE
        );",
    )?;

    add_column_if_missing(
        &tx,
        "downloads",
        "lifecycle_state",
        "ALTER TABLE downloads ADD COLUMN lifecycle_state TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "created_at",
        "ALTER TABLE downloads ADD COLUMN created_at TEXT NOT NULL DEFAULT '1970-01-01 00:00:00'",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "updated_at",
        "ALTER TABLE downloads ADD COLUMN updated_at TEXT NOT NULL DEFAULT '1970-01-01 00:00:00'",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "first_seen_at",
        "ALTER TABLE downloads ADD COLUMN first_seen_at TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "last_seen_in_queue_at",
        "ALTER TABLE downloads ADD COLUMN last_seen_in_queue_at TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "processing_started_at",
        "ALTER TABLE downloads ADD COLUMN processing_started_at TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "processing_finished_at",
        "ALTER TABLE downloads ADD COLUMN processing_finished_at TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "cleanup_started_at",
        "ALTER TABLE downloads ADD COLUMN cleanup_started_at TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "cleanup_finished_at",
        "ALTER TABLE downloads ADD COLUMN cleanup_finished_at TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "completed_at",
        "ALTER TABLE downloads ADD COLUMN completed_at TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "last_error",
        "ALTER TABLE downloads ADD COLUMN last_error TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "cue_files",
        "status",
        "ALTER TABLE cue_files ADD COLUMN status TEXT NOT NULL DEFAULT 'pending'",
    )?;
    add_column_if_missing(
        &tx,
        "cue_files",
        "message",
        "ALTER TABLE cue_files ADD COLUMN message TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "cue_files",
        "updated_at",
        "ALTER TABLE cue_files ADD COLUMN updated_at TEXT NOT NULL DEFAULT '1970-01-01 00:00:00'",
    )?;
    add_column_if_missing(
        &tx,
        "tracks",
        "size_bytes",
        "ALTER TABLE tracks ADD COLUMN size_bytes INTEGER",
    )?;
    add_column_if_missing(
        &tx,
        "tracks",
        "cleanup_status",
        "ALTER TABLE tracks ADD COLUMN cleanup_status TEXT NOT NULL DEFAULT 'pending'",
    )?;
    add_column_if_missing(
        &tx,
        "tracks",
        "cleanup_message",
        "ALTER TABLE tracks ADD COLUMN cleanup_message TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "tracks",
        "deleted_at",
        "ALTER TABLE tracks ADD COLUMN deleted_at TEXT",
    )?;

    tx.execute(
        "UPDATE downloads
         SET created_at = CASE
                WHEN created_at IS NULL OR created_at = '1970-01-01 00:00:00' THEN COALESCE(updated_at, CURRENT_TIMESTAMP)
                ELSE created_at
             END,
             updated_at = COALESCE(updated_at, CURRENT_TIMESTAMP),
             first_seen_at = COALESCE(first_seen_at, created_at, updated_at, CURRENT_TIMESTAMP),
             last_seen_in_queue_at = COALESCE(last_seen_in_queue_at, updated_at)",
        [],
    )?;

    if column_exists(&tx, "downloads", "split_complete")? {
        tx.execute(
            "UPDATE downloads
             SET lifecycle_state = CASE
                 WHEN lifecycle_state IS NOT NULL AND lifecycle_state <> '' THEN lifecycle_state
                 WHEN split_complete = 1 THEN 'awaiting_import'
                 WHEN COALESCE(last_error, '') <> '' THEN 'failed'
                 ELSE 'detected'
             END",
            [],
        )?;
    } else {
        tx.execute(
            "UPDATE downloads
             SET lifecycle_state = COALESCE(NULLIF(lifecycle_state, ''), 'detected')",
            [],
        )?;
    }

    tx.execute(
        "DELETE FROM cue_files
         WHERE rowid NOT IN (
             SELECT MIN(rowid) FROM cue_files GROUP BY download_id, path
         )",
        [],
    )?;
    tx.execute(
        "DELETE FROM input_files
         WHERE rowid NOT IN (
             SELECT MIN(rowid) FROM input_files GROUP BY download_id, path
         )",
        [],
    )?;
    tx.execute(
        "DELETE FROM tracks
         WHERE rowid NOT IN (
             SELECT MIN(rowid) FROM tracks GROUP BY download_id, path
         )",
        [],
    )?;
    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_cue_files_download_path
         ON cue_files(download_id, path)",
        [],
    )?;
    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_input_files_download_path
         ON input_files(download_id, path)",
        [],
    )?;
    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_download_path
         ON tracks(download_id, path)",
        [],
    )?;
    tx.pragma_update(None, "user_version", 2)?;
    tx.commit()?;
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    statement: &str,
) -> Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute(statement, [])?;
    }
    Ok(())
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::SqliteDownloadStore;
    use crate::domain::{
        CueSheetStatus, DownloadLifecycleState, InputFileKind, RecordedTrack, TrackCleanupOutcome,
        TrackCleanupStatus, TrackedDownload,
    };

    #[test]
    fn enum_database_mappings_preserve_existing_values() {
        assert_eq!(
            super::download_lifecycle_state_to_db(&DownloadLifecycleState::Detected),
            "detected"
        );
        assert_eq!(
            super::download_lifecycle_state_to_db(&DownloadLifecycleState::Processing),
            "processing"
        );
        assert_eq!(
            super::download_lifecycle_state_to_db(&DownloadLifecycleState::AwaitingImport),
            "awaiting_import"
        );
        assert_eq!(
            super::download_lifecycle_state_to_db(&DownloadLifecycleState::CleaningUp),
            "cleaning_up"
        );
        assert_eq!(
            super::download_lifecycle_state_to_db(&DownloadLifecycleState::Completed),
            "completed"
        );
        assert_eq!(
            super::download_lifecycle_state_to_db(&DownloadLifecycleState::Failed),
            "failed"
        );
        assert_eq!(
            super::download_lifecycle_state_from_db("processing"),
            DownloadLifecycleState::Processing
        );
        assert_eq!(
            super::download_lifecycle_state_from_db("awaiting_import"),
            DownloadLifecycleState::AwaitingImport
        );
        assert_eq!(
            super::download_lifecycle_state_from_db("cleaning_up"),
            DownloadLifecycleState::CleaningUp
        );
        assert_eq!(
            super::download_lifecycle_state_from_db("completed"),
            DownloadLifecycleState::Completed
        );
        assert_eq!(
            super::download_lifecycle_state_from_db("failed"),
            DownloadLifecycleState::Failed
        );
        assert_eq!(
            super::download_lifecycle_state_from_db("unexpected"),
            DownloadLifecycleState::Detected
        );

        assert_eq!(
            super::cue_sheet_status_to_db(CueSheetStatus::Pending),
            "pending"
        );
        assert_eq!(
            super::cue_sheet_status_to_db(CueSheetStatus::Split),
            "split"
        );
        assert_eq!(
            super::cue_sheet_status_to_db(CueSheetStatus::Skipped),
            "skipped"
        );
        assert_eq!(
            super::cue_sheet_status_to_db(CueSheetStatus::Failed),
            "failed"
        );
        assert_eq!(
            super::cue_sheet_status_from_db("split"),
            CueSheetStatus::Split
        );
        assert_eq!(
            super::cue_sheet_status_from_db("skipped"),
            CueSheetStatus::Skipped
        );
        assert_eq!(
            super::cue_sheet_status_from_db("failed"),
            CueSheetStatus::Failed
        );
        assert_eq!(
            super::cue_sheet_status_from_db("unexpected"),
            CueSheetStatus::Pending
        );

        assert_eq!(super::input_file_kind_to_db(InputFileKind::Cue), "cue");
        assert_eq!(super::input_file_kind_to_db(InputFileKind::Audio), "audio");
        assert_eq!(
            super::input_file_kind_from_db("audio"),
            InputFileKind::Audio
        );
        assert_eq!(
            super::input_file_kind_from_db("unexpected"),
            InputFileKind::Cue
        );

        assert_eq!(
            super::track_cleanup_status_to_db(TrackCleanupStatus::Pending),
            "pending"
        );
        assert_eq!(
            super::track_cleanup_status_to_db(TrackCleanupStatus::Deleted),
            "deleted"
        );
        assert_eq!(
            super::track_cleanup_status_to_db(TrackCleanupStatus::DeleteFailed),
            "delete_failed"
        );
        assert_eq!(
            super::track_cleanup_status_to_db(TrackCleanupStatus::Missing),
            "missing"
        );
        assert_eq!(
            super::track_cleanup_status_from_db("deleted"),
            TrackCleanupStatus::Deleted
        );
        assert_eq!(
            super::track_cleanup_status_from_db("delete_failed"),
            TrackCleanupStatus::DeleteFailed
        );
        assert_eq!(
            super::track_cleanup_status_from_db("missing"),
            TrackCleanupStatus::Missing
        );
        assert_eq!(
            super::track_cleanup_status_from_db("unexpected"),
            TrackCleanupStatus::Pending
        );
    }

    #[test]
    fn repository_persists_history_and_file_snapshots() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );

        repo.upsert_tracked_download_sync(&download).unwrap();
        repo.mark_download_processing_sync("download-1").unwrap();
        let cue = repo
            .get_or_create_cue_sheet_sync(
                &download.download_id,
                Path::new("/downloads/album/album.cue"),
            )
            .unwrap();
        repo.record_input_file_sync(
            &download.download_id,
            Some(&cue.id),
            Path::new("/downloads/album/album.cue"),
            InputFileKind::Cue,
            Some(123),
        )
        .unwrap();
        repo.record_cue_result_sync(
            &cue,
            CueSheetStatus::Split,
            None,
            &[RecordedTrack {
                path: "/downloads/album/01.flac".into(),
                size_bytes: Some(456),
            }],
        )
        .unwrap();
        repo.mark_download_awaiting_import_sync("download-1")
            .unwrap();
        let stored = repo
            .get_tracked_download_sync("download-1")
            .unwrap()
            .unwrap();
        let track = &stored.cue_sheets[0].tracks[0];
        repo.record_track_cleanup_sync("download-1", &track.id, TrackCleanupStatus::Deleted, None)
            .unwrap();
        repo.mark_download_completed_sync("download-1").unwrap();

        let downloads = repo.load_tracked_downloads_sync().unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(
            downloads[0].lifecycle_state,
            DownloadLifecycleState::Completed
        );
        assert_eq!(downloads[0].input_files.len(), 1);
        assert_eq!(downloads[0].cue_sheets.len(), 1);
        assert_eq!(downloads[0].cue_sheets[0].tracks.len(), 1);
        assert_eq!(downloads[0].cue_sheets[0].tracks[0].size_bytes, Some(456));
        assert_eq!(
            downloads[0].cue_sheets[0].tracks[0].cleanup_status,
            TrackCleanupStatus::Deleted
        );
    }

    #[test]
    fn awaiting_import_preserves_first_processing_finished_timestamp() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );

        repo.upsert_tracked_download_sync(&download).unwrap();
        repo.mark_download_awaiting_import_sync("download-1")
            .unwrap();

        let conn = Connection::open(&repo.db_path).unwrap();
        conn.execute(
            "UPDATE downloads
             SET processing_finished_at = '2001-02-03 04:05:06'
             WHERE download_id = ?",
            ["download-1"],
        )
        .unwrap();
        drop(conn);

        repo.mark_download_awaiting_import_sync("download-1")
            .unwrap();

        let stored = repo
            .get_tracked_download_sync("download-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.processing_finished_at.as_deref(),
            Some("2001-02-03 04:05:06")
        );
    }

    #[test]
    fn re_recorded_track_resets_stale_cleanup_state() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );

        repo.upsert_tracked_download_sync(&download).unwrap();
        let cue = repo
            .get_or_create_cue_sheet_sync(
                &download.download_id,
                Path::new("/downloads/album/album.cue"),
            )
            .unwrap();
        repo.record_cue_result_sync(
            &cue,
            CueSheetStatus::Split,
            None,
            &[RecordedTrack {
                path: "/downloads/album/01.flac".into(),
                size_bytes: Some(456),
            }],
        )
        .unwrap();

        let stored = repo
            .get_tracked_download_sync("download-1")
            .unwrap()
            .unwrap();
        let track = &stored.cue_sheets[0].tracks[0];
        repo.record_track_cleanup_sync(
            "download-1",
            &track.id,
            TrackCleanupStatus::Deleted,
            Some("already removed"),
        )
        .unwrap();

        repo.record_cue_result_sync(
            &cue,
            CueSheetStatus::Split,
            None,
            &[RecordedTrack {
                path: "/downloads/album/01.flac".into(),
                size_bytes: Some(789),
            }],
        )
        .unwrap();

        let refreshed = repo
            .get_tracked_download_sync("download-1")
            .unwrap()
            .unwrap();
        let track = &refreshed.cue_sheets[0].tracks[0];
        assert_eq!(track.size_bytes, Some(789));
        assert_eq!(track.cleanup_status, TrackCleanupStatus::Pending);
        assert_eq!(track.cleanup_message, None);
        assert_eq!(track.deleted_at, None);
    }

    #[test]
    fn load_download_row_fetches_only_requested_history_row() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();

        let album = TrackedDownload::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );
        let single = TrackedDownload::pending(
            "download-2".into(),
            "Single".into(),
            "completed".into(),
            "/downloads/single".into(),
            "importFailed".into(),
        );

        repo.upsert_tracked_download_sync(&album).unwrap();
        repo.upsert_tracked_download_sync(&single).unwrap();

        let cue = repo
            .get_or_create_cue_sheet_sync(
                &album.download_id,
                Path::new("/downloads/album/album.cue"),
            )
            .unwrap();
        repo.record_cue_result_sync(
            &cue,
            CueSheetStatus::Split,
            None,
            &[RecordedTrack {
                path: "/downloads/album/01.flac".into(),
                size_bytes: Some(456),
            }],
        )
        .unwrap();

        let row = repo.load_download_row_sync("download-1").unwrap().unwrap();
        assert_eq!(row.download_id, "download-1");
        assert_eq!(row.title, "Album");
        assert_eq!(row.generated_track_count, 1);
        assert_eq!(repo.load_download_row_sync("missing").unwrap(), None);
    }

    #[test]
    fn bulk_get_tracked_downloads_preserves_requested_order() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();

        let first = TrackedDownload::pending(
            "download-1".into(),
            "First".into(),
            "completed".into(),
            "/downloads/first".into(),
            "importFailed".into(),
        );
        let second = TrackedDownload::pending(
            "download-2".into(),
            "Second".into(),
            "completed".into(),
            "/downloads/second".into(),
            "importFailed".into(),
        );

        repo.upsert_tracked_download_sync(&first).unwrap();
        repo.upsert_tracked_download_sync(&second).unwrap();

        let downloads = repo
            .get_tracked_downloads_sync(&[
                "download-2".to_string(),
                "missing".to_string(),
                "download-1".to_string(),
            ])
            .unwrap();

        assert_eq!(downloads.len(), 2);
        assert_eq!(downloads[0].download_id, "download-2");
        assert_eq!(downloads[1].download_id, "download-1");
    }

    #[test]
    fn connections_set_busy_timeout() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();

        let conn = repo.connect().unwrap();
        let busy_timeout_ms = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))
            .unwrap();

        assert_eq!(
            busy_timeout_ms,
            super::SQLITE_BUSY_TIMEOUT.as_millis() as i64
        );
    }

    #[test]
    fn record_track_cleanups_updates_multiple_tracks_in_one_call() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );

        repo.upsert_tracked_download_sync(&download).unwrap();
        let cue = repo
            .get_or_create_cue_sheet_sync(
                &download.download_id,
                Path::new("/downloads/album/album.cue"),
            )
            .unwrap();
        repo.record_cue_result_sync(
            &cue,
            CueSheetStatus::Split,
            None,
            &[
                RecordedTrack {
                    path: "/downloads/album/01.flac".into(),
                    size_bytes: Some(111),
                },
                RecordedTrack {
                    path: "/downloads/album/02.flac".into(),
                    size_bytes: Some(222),
                },
            ],
        )
        .unwrap();

        let stored = repo
            .get_tracked_download_sync("download-1")
            .unwrap()
            .unwrap();
        let outcomes = stored.cue_sheets[0]
            .tracks
            .iter()
            .map(|track| TrackCleanupOutcome {
                track_id: track.id.clone(),
                status: TrackCleanupStatus::Deleted,
                message: None,
            })
            .collect::<Vec<_>>();

        repo.record_track_cleanups_sync("download-1", &outcomes)
            .unwrap();

        let refreshed = repo
            .get_tracked_download_sync("download-1")
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.cue_sheets[0].tracks.len(), 2);
        assert!(refreshed.cue_sheets[0]
            .tracks
            .iter()
            .all(|track| track.cleanup_status == TrackCleanupStatus::Deleted));
    }

    #[test]
    fn null_lifecycle_state_defaults_to_detected() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();
        let download = TrackedDownload::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );
        repo.upsert_tracked_download_sync(&download).unwrap();
        let conn = Connection::open(&repo.db_path).unwrap();
        conn.execute(
            "UPDATE downloads SET lifecycle_state = NULL WHERE download_id = ?",
            ["download-1"],
        )
        .unwrap();
        drop(conn);

        let summary = repo
            .load_tracked_download_summaries_sync()
            .unwrap()
            .pop()
            .unwrap();
        let full = repo
            .get_tracked_download_sync("download-1")
            .unwrap()
            .unwrap();
        let row = repo.load_download_row_sync("download-1").unwrap().unwrap();

        assert_eq!(summary.lifecycle_state, DownloadLifecycleState::Detected);
        assert_eq!(full.lifecycle_state, DownloadLifecycleState::Detected);
        assert_eq!(row.lifecycle_state, DownloadLifecycleState::Detected);
    }

    #[test]
    fn migration_backfills_lifecycle_state_from_legacy_rows() {
        let tmp = tempdir().unwrap();
        let db_path = tmp.path().join("data.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE downloads (
                download_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                status TEXT NOT NULL,
                output_path TEXT NOT NULL,
                tracked_download_state TEXT NOT NULL,
                split_complete BOOLEAN NOT NULL DEFAULT FALSE,
                last_error TEXT,
                updated_at TEXT NOT NULL DEFAULT '2024-01-01 00:00:00'
            );
            CREATE TABLE cue_files (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                download_id TEXT NOT NULL
            );
            CREATE TABLE tracks (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                cue_file_id TEXT NOT NULL,
                download_id TEXT NOT NULL
            );
            INSERT INTO downloads(download_id, title, status, output_path, tracked_download_state, split_complete, last_error)
            VALUES
                ('done', 'Done', 'completed', '/downloads/done', 'importFailed', 1, NULL),
                ('bad', 'Bad', 'completed', '/downloads/bad', 'importFailed', 0, 'split failed');",
        )
        .unwrap();
        drop(conn);

        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();
        let downloads = repo.load_tracked_downloads_sync().unwrap();

        assert_eq!(downloads.len(), 2);
        let done = downloads
            .iter()
            .find(|download| download.download_id == "done")
            .unwrap();
        let bad = downloads
            .iter()
            .find(|download| download.download_id == "bad")
            .unwrap();
        assert_eq!(done.lifecycle_state, DownloadLifecycleState::AwaitingImport);
        assert_eq!(bad.lifecycle_state, DownloadLifecycleState::Failed);
    }
}
