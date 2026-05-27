use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{named_params, params, Connection, OptionalExtension};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Repository {
    db_path: PathBuf,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("cue file row disappeared after insert: {0}")]
    MissingCueFile(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub download_id: String,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub tracked_download_state: String,
    pub cue_files: Vec<CueFile>,
    pub split_complete: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueFile {
    pub id: String,
    pub download_id: String,
    pub path: String,
    pub status: CueFileStatus,
    pub message: Option<String>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueFileStatus {
    Pending,
    Split,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub cue_file_id: String,
    pub download_id: String,
    pub path: String,
}

impl Repository {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        fs::create_dir_all(data_dir.as_ref())?;
        let db_path = data_dir.as_ref().join("data.db");
        let mut conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        Ok(Self { db_path })
    }

    pub fn all_downloads(&self) -> Result<Vec<Download>, StoreError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT download_id, title, status, output_path, tracked_download_state,
                    split_complete, last_error
             FROM downloads
             ORDER BY title, download_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Download {
                download_id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                output_path: row.get(3)?,
                tracked_download_state: row.get(4)?,
                split_complete: row.get(5)?,
                last_error: row.get(6)?,
                cue_files: Vec::new(),
            })
        })?;

        let mut downloads = Vec::new();
        for row in rows {
            let mut download = row?;
            download.cue_files = Self::cue_files_for(&conn, &download.download_id)?;
            downloads.push(download);
        }
        Ok(downloads)
    }

    pub fn upsert_download(&self, download: &Download) -> Result<(), StoreError> {
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
                ":download_id": download.download_id,
                ":title": download.title,
                ":status": download.status,
                ":output_path": download.output_path,
                ":tracked_download_state": download.tracked_download_state,
                ":split_complete": download.split_complete,
                ":last_error": download.last_error,
            },
        )?;
        Ok(())
    }

    pub fn mark_download_complete(
        &self,
        download_id: &str,
        split_complete: bool,
        last_error: Option<&str>,
    ) -> Result<(), StoreError> {
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

    pub fn get_or_create_cue_file(
        &self,
        download_id: &str,
        path: &Path,
    ) -> Result<CueFile, StoreError> {
        let conn = self.connect()?;
        let path = path.to_string_lossy().to_string();
        let id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT OR IGNORE INTO cue_files (id, path, download_id, status, message, updated_at)
             VALUES (:id, :path, :download_id, :status, NULL, CURRENT_TIMESTAMP)",
            named_params! {
                ":id": id,
                ":path": path,
                ":download_id": download_id,
                ":status": CueFileStatus::Pending.as_str(),
            },
        )?;

        Self::cue_file_by_download_and_path(&conn, download_id, &path)?
            .ok_or(StoreError::MissingCueFile(path))
    }

    pub fn record_cue_result(
        &self,
        cue_file: &CueFile,
        status: CueFileStatus,
        message: Option<&str>,
        tracks: &[PathBuf],
    ) -> Result<(), StoreError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE cue_files
             SET status = :status,
                 message = :message,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = :id",
            named_params! {
                ":id": cue_file.id,
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
                    ":path": track.to_string_lossy(),
                    ":cue_file_id": cue_file.id,
                    ":download_id": cue_file.download_id,
                },
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn delete_download(&self, download_id: &str) -> Result<(), StoreError> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM downloads WHERE download_id = :download_id",
            named_params! { ":download_id": download_id },
        )?;
        Ok(())
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        let conn = Connection::open(&self.db_path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    fn cue_files_for(conn: &Connection, download_id: &str) -> Result<Vec<CueFile>, StoreError> {
        let mut stmt = conn.prepare(
            "SELECT id, path, download_id, status, message
             FROM cue_files
             WHERE download_id = ?
             ORDER BY path",
        )?;
        let rows = stmt.query_map([download_id], |row| {
            let id: String = row.get(0)?;
            Ok(CueFile {
                tracks: Self::tracks_for(conn, &id)?,
                id,
                path: row.get(1)?,
                download_id: row.get(2)?,
                status: CueFileStatus::from_db(row.get::<_, String>(3)?.as_str()),
                message: row.get(4)?,
            })
        })?;

        let mut cue_files = Vec::new();
        for row in rows {
            cue_files.push(row?);
        }
        Ok(cue_files)
    }

    fn cue_file_by_download_and_path(
        conn: &Connection,
        download_id: &str,
        path: &str,
    ) -> Result<Option<CueFile>, StoreError> {
        conn.query_row(
            "SELECT id, path, download_id, status, message
             FROM cue_files
             WHERE download_id = ? AND path = ?",
            params![download_id, path],
            |row| {
                let id: String = row.get(0)?;
                Ok(CueFile {
                    tracks: Self::tracks_for(conn, &id)?,
                    id,
                    path: row.get(1)?,
                    download_id: row.get(2)?,
                    status: CueFileStatus::from_db(row.get::<_, String>(3)?.as_str()),
                    message: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
    }

    fn tracks_for(conn: &Connection, cue_file_id: &str) -> Result<Vec<Track>, rusqlite::Error> {
        let mut stmt = conn.prepare(
            "SELECT id, cue_file_id, download_id, path
             FROM tracks
             WHERE cue_file_id = ?
             ORDER BY path",
        )?;
        let rows = stmt.query_map([cue_file_id], |row| {
            Ok(Track {
                id: row.get(0)?,
                cue_file_id: row.get(1)?,
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
}

impl Download {
    pub fn pending(
        download_id: String,
        title: String,
        status: String,
        output_path: String,
        tracked_download_state: String,
    ) -> Self {
        Self {
            download_id,
            title,
            status,
            output_path,
            tracked_download_state,
            cue_files: Vec::new(),
            split_complete: false,
            last_error: None,
        }
    }
}

impl CueFileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Split => "split",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "split" => Self::Split,
            "skipped" => Self::Skipped,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }

    pub fn is_terminal_success(self) -> bool {
        matches!(self, Self::Split | Self::Skipped)
    }
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
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
) -> Result<(), StoreError> {
    if !column_exists(conn, table, column)? {
        conn.execute(statement, [])?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, StoreError> {
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
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn repository_saves_updates_and_deletes_download_graph() {
        let tmp = tempdir().unwrap();
        let repo = Repository::open(tmp.path()).unwrap();
        let mut download = Download::pending(
            "download-1".into(),
            "Album".into(),
            "completed".into(),
            "/downloads/album".into(),
            "importFailed".into(),
        );

        repo.upsert_download(&download).unwrap();
        download.title = "Album Updated".into();
        repo.upsert_download(&download).unwrap();

        let cue = repo
            .get_or_create_cue_file(
                &download.download_id,
                Path::new("/downloads/album/album.cue"),
            )
            .unwrap();
        repo.record_cue_result(
            &cue,
            CueFileStatus::Split,
            None,
            &[PathBuf::from("/downloads/album/01.flac")],
        )
        .unwrap();
        repo.record_cue_result(
            &cue,
            CueFileStatus::Split,
            None,
            &[PathBuf::from("/downloads/album/01.flac")],
        )
        .unwrap();

        let downloads = repo.all_downloads().unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].title, "Album Updated");
        assert_eq!(downloads[0].cue_files.len(), 1);
        assert_eq!(downloads[0].cue_files[0].tracks.len(), 1);

        repo.delete_download("download-1").unwrap();
        assert!(repo.all_downloads().unwrap().is_empty());
    }

    #[test]
    fn repository_migrates_old_schema_without_losing_downloads() {
        let tmp = tempdir().unwrap();
        let db_path = tmp.path().join("data.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE downloads (
                download_id            TEXT PRIMARY KEY,
                title                  TEXT NOT NULL,
                status                 TEXT NOT NULL,
                output_path            TEXT NOT NULL,
                tracked_download_state TEXT NOT NULL,
                split_complete         BOOLEAN DEFAULT FALSE
            );
            CREATE TABLE cue_files (
                id          TEXT PRIMARY KEY,
                path        TEXT NOT NULL,
                download_id TEXT NOT NULL,
                FOREIGN KEY(download_id) REFERENCES downloads(download_id)
            );
            CREATE TABLE tracks (
                id          TEXT PRIMARY KEY,
                path        TEXT NOT NULL,
                cue_file_id TEXT NOT NULL,
                download_id TEXT NOT NULL,
                FOREIGN KEY(cue_file_id) REFERENCES cue_files(id),
                FOREIGN KEY(download_id) REFERENCES downloads(download_id)
            );
            INSERT INTO downloads VALUES (
                'download-1', 'Album', 'completed', '/downloads/album', 'importFailed', 0
            );",
        )
        .unwrap();
        drop(conn);

        let repo = Repository::open(tmp.path()).unwrap();
        let downloads = repo.all_downloads().unwrap();

        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].last_error, None);
        assert!(!downloads[0].split_complete);
    }
}
