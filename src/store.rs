use exitfailure::ExitFailure;
use rusqlite::{named_params, params, Connection, Result};
use std::borrow::{Borrow, BorrowMut};
use std::fs;
use uuid::Uuid;

use crate::globals::dirs;

async fn establish_connection() -> Result<Connection, ExitFailure> {
    let dirs = dirs();
    let data_dir = dirs.data_dir();
    fs::create_dir_all(data_dir)?;
    let database_file = data_dir.join("data.db");
    let conn = Connection::open(database_file.to_str().unwrap())?;
    Ok(conn)
}

#[derive(Eq, Hash, Debug)]
pub struct Download {
    pub download_id: String,
    pub title: String,
    pub status: String,
    pub output_path: String,
    pub tracked_download_state: String,
    pub cue_files: Vec<CueFile>,
}

impl PartialEq for Download {
    fn eq(&self, other: &Self) -> bool {
        self.download_id == other.download_id
    }
}

impl Download {
    async fn initialize() -> Result<Connection, ExitFailure> {
        let conn = establish_connection().await?;
        match conn.execute(
            "CREATE TABLE IF NOT EXISTS downloads (
                download_id            TEXT PRIMARY KEY,
                title                  TEXT NOT NULL,
                status                 TEXT NOT NULL,
                output_path            TEXT NOT NULL,
                tracked_download_state TEXT NOT NULL
            )",
            [],
        ) {
            Ok(..) => (),
            Err(err) => println!("Failure: {}", err),
        }
        Ok(conn)
    }

    async fn new(
        download_id: String,
        title: String,
        status: String,
        output_path: String,
        tracked_download_state: String,
    ) -> Download {
        let cue_files = CueFile::find(download_id.borrow()).await.unwrap();
        Download {
            download_id,
            title,
            status,
            output_path,
            tracked_download_state,
            cue_files,
        }
    }

    pub async fn find(download_id: String) -> Result<Download, ExitFailure> {
        let conn = Download::initialize().await.unwrap();
        let result = conn.query_row(
            "SELECT download_id, title, status, output_path, tracked_download_state \
                FROM downloads where download_id = ?",
            [download_id],
            |row| {
                Ok(Download::new(
                    row.get_unwrap(0),
                    row.get_unwrap(1),
                    row.get_unwrap(2),
                    row.get_unwrap(3),
                    row.get_unwrap(4),
                ))
            },
        )?;
        Ok(result.await)
    }

    pub async fn save(&self) -> Result<(), ExitFailure> {
        let conn = Download::initialize().await.unwrap();
        let mut select_statement =
            conn.prepare("SELECT download_id FROM downloads where download_id = ?")?;
        let exists = select_statement.exists(params![self.download_id]).unwrap();
        let mut statement = if exists {
            conn.prepare(
                "UPDATE downloads
                  set
                    title = :title,
                    status = :status,
                    output_path = :output_path,
                    tracked_download_state = :tracked_download_state
                  where download_id = :download_id",
            )
        } else {
            conn.prepare(
                "INSERT INTO
                  downloads (download_id, title, status, output_path, tracked_download_state)
                VALUES
                  (
                    :download_id, :title, :status, :output_path, :tracked_download_state
                  )",
            )
        }?;
        statement.execute(named_params! {
            ":download_id": self.download_id,
            ":title": self.title,
            ":status": self.status,
            ":output_path": self.output_path,
            ":tracked_download_state": self.tracked_download_state
        })?;
        for cue_file in self.cue_files.as_slice() {
            cue_file.save().await?;
        }
        Ok(())
    }

    pub async fn add_cue_file<'a>(&mut self, path: String) -> Result<&mut CueFile, ExitFailure> {
        let cue_file = CueFile {
            id: Uuid::new_v4(),
            download_id: self.download_id.to_owned(),
            path,
            tracks: vec![],
        };
        cue_file.save().await.unwrap();
        self.cue_files.push(cue_file);
        Ok(self.cue_files.last_mut().unwrap())
    }
}

#[derive(Eq, Hash, Debug)]
pub struct CueFile {
    id: Uuid,
    download_id: String,
    pub path: String,
    pub tracks: Vec<Track>,
}

impl PartialEq for CueFile {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl CueFile {
    async fn initialize() -> Result<Connection, ExitFailure> {
        let conn = establish_connection().await?;
        match conn.execute(
            "CREATE TABLE IF NOT EXISTS cue_files (
                id                     TEXT PRIMARY KEY,
                path                   TEXT NOT NULL,
                download_id            TEXT NOT NULL,
                FOREIGN KEY(download_id) REFERENCES downloads(download_id)
            )",
            [],
        ) {
            Ok(..) => (),
            Err(err) => println!("Failure: {}", err),
        }
        Ok(conn)
    }

    async fn new(id: Uuid, download_id: String, path: String) -> CueFile {
        let tracks = Track::find(id).await.unwrap();
        CueFile {
            id,
            download_id,
            path,
            tracks,
        }
    }

    async fn find(download_id: &str) -> Result<Vec<CueFile>, ExitFailure> {
        let conn = CueFile::initialize().await.unwrap();
        let mut stmt = conn.prepare("SELECT id, path FROM cue_files where download_id = ?")?;
        let iter = stmt.query_map([download_id], |row| {
            Ok(CueFile::new(
                row.get_unwrap(0),
                download_id.to_owned(),
                row.get_unwrap(1),
            ))
        })?;
        let mut files = vec![];
        for file in iter {
            files.push(file.unwrap().await);
        }
        Ok(files)
    }

    async fn save(&self) -> Result<(), ExitFailure> {
        let conn = CueFile::initialize().await.unwrap();
        let mut select_statement = conn.prepare("SELECT path FROM cue_files where path = ?")?;
        let exists = select_statement.exists(params![self.path]).unwrap();
        if !exists {
            let mut statement = conn.prepare(
                "INSERT INTO
                  cue_files (id, path, download_id)
                VALUES
                  (
                    :id, :path, :download_id
                  )",
            )?;
            statement.execute(named_params! {
                ":id": self.id,
                ":path": self.path,
                ":download_id": self.download_id,
            })?;
        }
        Ok(())
    }

    pub async fn add_track(&mut self, path: String) {
        let track = Track {
            id: Uuid::new_v4(),
            cue_file_id: self.id,
            download_id: self.download_id.to_owned(),
            path,
        };
        track.save().await.unwrap();
        self.tracks.push(track);
    }
}

#[derive(Eq, Hash, Debug)]
pub struct Track {
    id: Uuid,
    cue_file_id: Uuid,
    download_id: String,
    pub path: String,
}

impl PartialEq for Track {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.download_id == other.download_id
    }
}

impl Track {
    async fn initialize() -> Result<Connection, ExitFailure> {
        let conn = establish_connection().await?;
        match conn.execute(
            "CREATE TABLE IF NOT EXISTS tracks (
                id                     TEXT PRIMARY KEY,
                path                   TEXT NOT NULL,
                cue_file_id            TEXT NOT NULL,
                download_id            TEXT NOT NULL,
                FOREIGN KEY(cue_file_id) REFERENCES cue_files(id),
                FOREIGN KEY(download_id) REFERENCES downloads(download_id)
            )",
            [],
        ) {
            Ok(..) => (),
            Err(err) => println!("Failure: {}", err),
        }
        Ok(conn)
    }

    async fn find(cue_file_id: Uuid) -> Result<Vec<Track>, ExitFailure> {
        let conn = Track::initialize().await.unwrap();
        let mut stmt =
            conn.prepare("SELECT id, download_id, path FROM tracks where cue_file_id = ?")?;
        let iter = stmt.query_map([cue_file_id], |row| {
            Ok(Track {
                id: row.get_unwrap(0),
                cue_file_id,
                download_id: row.get_unwrap(1),
                path: row.get_unwrap(2),
            })
        })?;
        let mut tracks = vec![];
        for track in iter {
            tracks.push(track.unwrap());
        }
        Ok(tracks)
    }

    async fn save(&self) -> Result<(), ExitFailure> {
        let conn = Track::initialize().await.unwrap();
        let mut select_statement = conn.prepare("SELECT path FROM tracks where path = ?")?;
        let exists = select_statement.exists(params![self.path]).unwrap();
        if !exists {
            let mut statement = conn.prepare(
                "INSERT INTO
                  tracks (id, path, cue_file_id, download_id)
                VALUES
                  (
                    :id, :path, :cue_file_id, :download_id
                  )",
            )?;
            dbg!(self.path.to_owned());
            dbg!(self.cue_file_id.to_owned());
            dbg!(self.download_id.to_owned());
            statement.execute(named_params! {
                ":id": self.id,
                ":path": self.path,
                ":cue_file_id": self.cue_file_id ,
                ":download_id": self.download_id,
            })?;
        }
        Ok(())
    }
}
