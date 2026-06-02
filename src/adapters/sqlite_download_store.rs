use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use rusqlite::{named_params, params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::application::ports::DownloadStore;
use crate::domain::{
    CueSheet, CueSheetStatus, DownloadLifecycleState, GeneratedTrack, InputFile, InputFileKind,
    RecordedTrack, TrackCleanupStatus, TrackedDownload,
};

#[derive(Debug, Clone)]
pub struct SqliteDownloadStore {
    db_path: PathBuf,
}

impl SqliteDownloadStore {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(data_dir.as_ref())?;
        let db_path = data_dir.as_ref().join("data.db");
        let mut conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        Ok(Self { db_path })
    }

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
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
            "SELECT download_id, title, status, output_path, tracked_download_state,
                    lifecycle_state, created_at, updated_at, first_seen_at, last_seen_in_queue_at,
                    processing_started_at, processing_finished_at, cleanup_started_at,
                    cleanup_finished_at, completed_at, last_error
             FROM downloads
             ORDER BY updated_at DESC, download_id DESC",
        )?;
        let rows = stmt.query_map([], map_download_summary_row)?;

        let mut downloads = Vec::new();
        for row in rows {
            downloads.push(row?);
        }
        Ok(downloads)
    }

    fn get_tracked_download_sync(&self, download_id: &str) -> Result<Option<TrackedDownload>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT download_id, title, status, output_path, tracked_download_state,
                    lifecycle_state, created_at, updated_at, first_seen_at, last_seen_in_queue_at,
                    processing_started_at, processing_finished_at, cleanup_started_at,
                    cleanup_finished_at, completed_at, last_error
             FROM downloads
             WHERE download_id = ?",
            [download_id],
            |row| map_download_row(&conn, row),
        )
        .optional()
        .map_err(Into::into)
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
                ":lifecycle_state": download.lifecycle_state.as_str(),
                ":last_error": &download.last_error,
            },
        )?;
        Ok(())
    }

    fn touch_download_queue_presence_sync(&self, download_id: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE downloads
             SET last_seen_in_queue_at = CURRENT_TIMESTAMP,
                 updated_at = CURRENT_TIMESTAMP
             WHERE download_id = ?",
            [download_id],
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
                 processing_finished_at = CURRENT_TIMESTAMP,
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
                ":status": CueSheetStatus::Pending.as_str(),
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
                kind.as_str(),
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
                ":status": status.as_str(),
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
                    size_bytes = excluded.size_bytes",
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
            params![track_id, status.as_str(), message],
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

    async fn upsert_tracked_download(&self, download: &TrackedDownload) -> Result<()> {
        let store = self.clone();
        let download = download.clone();
        tokio::task::spawn_blocking(move || store.upsert_tracked_download_sync(&download))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn touch_download_queue_presence(&self, download_id: &str) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.touch_download_queue_presence_sync(&download_id))
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

    async fn mark_download_failed(&self, download_id: &str, last_error: Option<&str>) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        let last_error = last_error.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            store.mark_download_failed_sync(&download_id, last_error.as_deref())
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
}

fn map_download_row(conn: &Connection, row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackedDownload> {
    let download_id: String = row.get(0)?;
    Ok(TrackedDownload {
        input_files: input_files_for(conn, &download_id)?,
        cue_sheets: cue_sheets_for(conn, &download_id)?,
        download_id,
        title: row.get(1)?,
        status: row.get(2)?,
        output_path: row.get(3)?,
        tracked_download_state: row.get(4)?,
        lifecycle_state: DownloadLifecycleState::from_db(row.get::<_, String>(5)?.as_str()),
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        first_seen_at: row.get(8)?,
        last_seen_in_queue_at: row.get(9)?,
        processing_started_at: row.get(10)?,
        processing_finished_at: row.get(11)?,
        cleanup_started_at: row.get(12)?,
        cleanup_finished_at: row.get(13)?,
        completed_at: row.get(14)?,
        last_error: row.get(15)?,
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
        lifecycle_state: DownloadLifecycleState::from_db(row.get::<_, String>(5)?.as_str()),
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        first_seen_at: row.get(8)?,
        last_seen_in_queue_at: row.get(9)?,
        processing_started_at: row.get(10)?,
        processing_finished_at: row.get(11)?,
        cleanup_started_at: row.get(12)?,
        cleanup_finished_at: row.get(13)?,
        completed_at: row.get(14)?,
        last_error: row.get(15)?,
    })
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
            status: CueSheetStatus::from_db(row.get::<_, String>(3)?.as_str()),
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
            kind: InputFileKind::from_db(row.get::<_, String>(4)?.as_str()),
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
                status: CueSheetStatus::from_db(row.get::<_, String>(3)?.as_str()),
                message: row.get(4)?,
                updated_at: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn tracks_for(conn: &Connection, cue_sheet_id: &str) -> Result<Vec<GeneratedTrack>, rusqlite::Error> {
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
            cleanup_status: TrackCleanupStatus::from_db(row.get::<_, String>(5)?.as_str()),
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

    add_column_if_missing(&tx, "downloads", "lifecycle_state", "ALTER TABLE downloads ADD COLUMN lifecycle_state TEXT")?;
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
    add_column_if_missing(&tx, "downloads", "first_seen_at", "ALTER TABLE downloads ADD COLUMN first_seen_at TEXT")?;
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
    add_column_if_missing(&tx, "downloads", "completed_at", "ALTER TABLE downloads ADD COLUMN completed_at TEXT")?;
    add_column_if_missing(&tx, "downloads", "last_error", "ALTER TABLE downloads ADD COLUMN last_error TEXT")?;
    add_column_if_missing(&tx, "cue_files", "status", "ALTER TABLE cue_files ADD COLUMN status TEXT NOT NULL DEFAULT 'pending'")?;
    add_column_if_missing(&tx, "cue_files", "message", "ALTER TABLE cue_files ADD COLUMN message TEXT")?;
    add_column_if_missing(
        &tx,
        "cue_files",
        "updated_at",
        "ALTER TABLE cue_files ADD COLUMN updated_at TEXT NOT NULL DEFAULT '1970-01-01 00:00:00'",
    )?;
    add_column_if_missing(&tx, "tracks", "size_bytes", "ALTER TABLE tracks ADD COLUMN size_bytes INTEGER")?;
    add_column_if_missing(
        &tx,
        "tracks",
        "cleanup_status",
        "ALTER TABLE tracks ADD COLUMN cleanup_status TEXT NOT NULL DEFAULT 'pending'",
    )?;
    add_column_if_missing(&tx, "tracks", "cleanup_message", "ALTER TABLE tracks ADD COLUMN cleanup_message TEXT")?;
    add_column_if_missing(&tx, "tracks", "deleted_at", "ALTER TABLE tracks ADD COLUMN deleted_at TEXT")?;

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

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, statement: &str) -> Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute(statement, [])?;
    }
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
    use crate::domain::{CueSheetStatus, DownloadLifecycleState, InputFileKind, RecordedTrack, TrackCleanupStatus, TrackedDownload};

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
        repo.mark_download_awaiting_import_sync("download-1").unwrap();
        let stored = repo.get_tracked_download_sync("download-1").unwrap().unwrap();
        let track = &stored.cue_sheets[0].tracks[0];
        repo.record_track_cleanup_sync(
            "download-1",
            &track.id,
            TrackCleanupStatus::Deleted,
            None,
        )
        .unwrap();
        repo.mark_download_completed_sync("download-1").unwrap();

        let downloads = repo.load_tracked_downloads_sync().unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].lifecycle_state, DownloadLifecycleState::Completed);
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
        let done = downloads.iter().find(|download| download.download_id == "done").unwrap();
        let bad = downloads.iter().find(|download| download.download_id == "bad").unwrap();
        assert_eq!(done.lifecycle_state, DownloadLifecycleState::AwaitingImport);
        assert_eq!(bad.lifecycle_state, DownloadLifecycleState::Failed);
    }
}
