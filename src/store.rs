use exitfailure::ExitFailure;
use rusqlite::{named_params, params, Connection, Result};
use std::fs;

use crate::globals::dirs;

pub struct Download {
    pub id: i64,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub download_id: String,
    pub tracked_download_state: String,
}

impl Download {
    async fn initialize() -> Result<Connection, ExitFailure> {
        let dirs = dirs();
        let data_dir = dirs.data_dir();
        fs::create_dir_all(data_dir)?;
        let database_file = data_dir.join("data.db");
        let conn = Connection::open(database_file.to_str().unwrap())?;
        match conn.execute(
            "CREATE TABLE IF NOT EXISTS downloads (
                id                     INTEGER PRIMARY KEY,
                title                  TEXT NOT NULL,
                status                 TEXT NOT NULL,
                output_path            TEXT NOT NULL,
                download_id            TEXT NOT NULL,
                tracked_download_state TEXT NOT NULL
            )",
            [],
        ) {
            Ok(..) => (),
            Err(err) => println!("Failure: {}", err),
        }
        Ok(conn)
    }

    pub async fn find(id: i64) -> Result<Download, ExitFailure> {
        let conn = Download::initialize().await.unwrap();
        let result = conn.query_row(
            "SELECT id, title, status, output_path, download_id, tracked_download_state \
                FROM downloads where id = ?",
            [id],
            |row| {
                Ok(Download {
                    id: row.get_unwrap(0),
                    title: row.get_unwrap(1),
                    status: row.get_unwrap(2),
                    output_path: row.get_unwrap(3),
                    download_id: row.get_unwrap(4),
                    tracked_download_state: row.get_unwrap(5),
                })
            },
        )?;
        Ok(result)
    }

    pub async fn save(&self) -> Result<(), ExitFailure> {
        let conn = Download::initialize().await.unwrap();
        let mut select_statement = conn.prepare("SELECT id FROM downloads where id = ?")?;
        let exists = select_statement.exists(params![self.id]).unwrap();
        let mut statement = if exists {
            conn.prepare(
                "UPDATE downloads
                  set
                    title = :title,
                    status = :status,
                    output_path = :output_path,
                    download_id = :download_id,
                    tracked_download_state = :tracked_download_state
                  where id = :id",
            )
        } else {
            conn.prepare(
                "INSERT INTO
                  downloads (id, title, status, output_path, download_id, tracked_download_state)
                VALUES
                  (
                    :id, :title, :status, :output_path, :download_id, :tracked_download_state
                  )",
            )
        }?;
        statement.execute(named_params! {
            ":id": self.id,
            ":title": self.title,
            ":status": self.status,
            ":output_path": self.output_path,
            ":download_id": self.download_id,
            ":tracked_download_state": self.tracked_download_state
        })?;
        Ok(())
    }
}
