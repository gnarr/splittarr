use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use rusqlite::{named_params, params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::application::ports::DownloadStore;
use crate::domain::{CueSheet, CueSheetStatus, GeneratedTrack, TrackedDownload};

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
                    split_complete, last_error
             FROM downloads
             ORDER BY title, download_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TrackedDownload {
                download_id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                output_path: row.get(3)?,
                tracked_download_state: row.get(4)?,
                split_complete: row.get(5)?,
                last_error: row.get(6)?,
                cue_sheets: Vec::new(),
            })
        })?;

        let mut downloads = Vec::new();
        for row in rows {
            let mut download = row?;
            download.cue_sheets = cue_sheets_for(&conn, &download.download_id)?;
            downloads.push(download);
        }
        Ok(downloads)
    }

    fn upsert_tracked_download_sync(&self, download: &TrackedDownload) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO downloads (
                download_id, title, status, output_path, tracked_download_state,
                split_complete, last_error, updated_at
             )
             VALUES (
                :download_id, :title, :status, :output_path, :tracked_download_state,
                :split_complete, :last_error, CURRENT_TIMESTAMP
             )
             ON CONFLICT(download_id) DO UPDATE SET
                title = excluded.title,
                status = excluded.status,
                output_path = excluded.output_path,
                tracked_download_state = excluded.tracked_download_state,
                split_complete = excluded.split_complete,
                last_error = excluded.last_error,
                updated_at = CURRENT_TIMESTAMP",
            named_params! {
                ":download_id": &download.download_id,
                ":title": &download.title,
                ":status": &download.status,
                ":output_path": &download.output_path,
                ":tracked_download_state": &download.tracked_download_state,
                ":split_complete": download.split_complete,
                ":last_error": &download.last_error,
            },
        )?;
        Ok(())
    }

    fn mark_download_complete_sync(
        &self,
        download_id: &str,
        split_complete: bool,
        last_error: Option<&str>,
    ) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE downloads
             SET split_complete = :split_complete,
                 last_error = :last_error,
                 updated_at = CURRENT_TIMESTAMP
             WHERE download_id = :download_id",
            named_params! {
                ":download_id": download_id,
                ":split_complete": split_complete,
                ":last_error": last_error,
            },
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

    fn record_cue_result_sync(
        &self,
        cue_sheet: &CueSheet,
        status: CueSheetStatus,
        message: Option<&str>,
        tracks: &[PathBuf],
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
                "INSERT OR IGNORE INTO tracks (id, path, cue_file_id, download_id)
                 VALUES (:id, :path, :cue_file_id, :download_id)",
                named_params! {
                    ":id": Uuid::new_v4().to_string(),
                    ":path": track.to_string_lossy().to_string(),
                    ":cue_file_id": &cue_sheet.id,
                    ":download_id": &cue_sheet.download_id,
                },
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    fn delete_download_sync(&self, download_id: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM downloads WHERE download_id = :download_id",
            named_params! { ":download_id": download_id },
        )?;
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

    async fn upsert_tracked_download(&self, download: &TrackedDownload) -> Result<()> {
        let store = self.clone();
        let download = download.clone();
        tokio::task::spawn_blocking(move || store.upsert_tracked_download_sync(&download))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }

    async fn mark_download_complete(
        &self,
        download_id: &str,
        split_complete: bool,
        last_error: Option<&str>,
    ) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        let last_error = last_error.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            store.mark_download_complete_sync(&download_id, split_complete, last_error.as_deref())
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

    async fn record_cue_result(
        &self,
        cue_sheet: &CueSheet,
        status: CueSheetStatus,
        message: Option<&str>,
        tracks: &[PathBuf],
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

    async fn delete_download(&self, download_id: &str) -> Result<()> {
        let store = self.clone();
        let download_id = download_id.to_owned();
        tokio::task::spawn_blocking(move || store.delete_download_sync(&download_id))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }
}

fn cue_sheets_for(conn: &Connection, download_id: &str) -> Result<Vec<CueSheet>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, download_id, status, message
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
        })
    })?;

    let mut cue_sheets = Vec::new();
    for row in rows {
        cue_sheets.push(row?);
    }
    Ok(cue_sheets)
}

fn cue_sheet_by_download_and_path(
    conn: &Connection,
    download_id: &str,
    path: &str,
) -> Result<Option<CueSheet>> {
    conn.query_row(
        "SELECT id, path, download_id, status, message
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
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn tracks_for(
    conn: &Connection,
    cue_sheet_id: &str,
) -> Result<Vec<GeneratedTrack>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, cue_file_id, download_id, path
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
            split_complete         BOOLEAN NOT NULL DEFAULT FALSE,
            last_error             TEXT,
            updated_at             TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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

        CREATE TABLE IF NOT EXISTS tracks (
            id          TEXT PRIMARY KEY,
            path        TEXT NOT NULL,
            cue_file_id TEXT NOT NULL,
            download_id TEXT NOT NULL,
            FOREIGN KEY(cue_file_id) REFERENCES cue_files(id) ON DELETE CASCADE,
            FOREIGN KEY(download_id) REFERENCES downloads(download_id) ON DELETE CASCADE
        );",
    )?;

    add_column_if_missing(
        &tx,
        "downloads",
        "last_error",
        "ALTER TABLE downloads ADD COLUMN last_error TEXT",
    )?;
    add_column_if_missing(
        &tx,
        "downloads",
        "updated_at",
        "ALTER TABLE downloads ADD COLUMN updated_at TEXT NOT NULL DEFAULT '1970-01-01 00:00:00'",
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

    tx.execute(
        "DELETE FROM cue_files
         WHERE rowid NOT IN (
             SELECT MIN(rowid) FROM cue_files GROUP BY download_id, path
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
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_download_path
         ON tracks(download_id, path)",
        [],
    )?;
    tx.pragma_update(None, "user_version", 1)?;
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
    use std::path::{Path, PathBuf};

    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::SqliteDownloadStore;
    use crate::domain::{CueSheetStatus, TrackedDownload};

    #[test]
    fn repository_saves_updates_and_deletes_download_graph() {
        let tmp = tempdir().unwrap();
        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();
        let mut download = TrackedDownload::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );

        repo.upsert_tracked_download_sync(&download).unwrap();
        download.title = "Album Updated".into();
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
            &[PathBuf::from("/downloads/album/01.flac")],
        )
        .unwrap();
        repo.record_cue_result_sync(
            &cue,
            CueSheetStatus::Split,
            None,
            &[PathBuf::from("/downloads/album/01.flac")],
        )
        .unwrap();

        let downloads = repo.load_tracked_downloads_sync().unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].title, "Album Updated");
        assert_eq!(downloads[0].cue_sheets.len(), 1);
        assert_eq!(downloads[0].cue_sheets[0].tracks.len(), 1);

        repo.delete_download_sync("download-1").unwrap();
        assert!(repo.load_tracked_downloads_sync().unwrap().is_empty());
    }

    #[test]
    fn migration_adds_missing_columns_without_losing_existing_rows() {
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
                split_complete BOOLEAN NOT NULL DEFAULT FALSE
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
            INSERT INTO downloads (
                download_id, title, status, output_path, tracked_download_state, split_complete
            ) VALUES (
                'download-1', 'Album', 'completed', '/downloads/album', 'importFailed', 0
            );",
        )
        .unwrap();
        drop(conn);

        let repo = SqliteDownloadStore::open(tmp.path()).unwrap();
        let downloads = repo.load_tracked_downloads_sync().unwrap();

        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].download_id, "download-1");
    }
}
