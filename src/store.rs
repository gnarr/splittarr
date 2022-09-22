use exitfailure::ExitFailure;
use rusqlite::{named_params, params, Connection, Result};
use std::borrow::Borrow;
use std::fs;
use std::path::Path;
use uuid::Uuid;

use crate::Settings;

async fn establish_connection() -> Result<Connection, ExitFailure> {
    let settings = Settings::new();
    let data_dir = settings
        .get::<String>("data_dir")
        .expect("Could not find data_dir");
    let data_dir_path = Path::new(data_dir.as_str());
    fs::create_dir_all(data_dir_path)?;
    let database_file = data_dir_path.join("data.db");
    let initialized = database_file.exists();
    let conn = Connection::open(database_file.to_str().unwrap())?;
    if !initialized {
        match conn.execute(
            "CREATE TABLE IF NOT EXISTS downloads (
                download_id            TEXT PRIMARY KEY,
                title                  TEXT NOT NULL,
                status                 TEXT NOT NULL,
                output_path            TEXT NOT NULL,
                tracked_download_state TEXT NOT NULL,
                split_complete         BOOLEAN DEFAULT FALSE
            )",
            [],
        ) {
            Ok(..) => (),
            Err(err) => println!("Failure: {}", err),
        }
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
    }
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
    pub split_complete: bool,
}

impl PartialEq for Download {
    fn eq(&self, other: &Self) -> bool {
        self.download_id == other.download_id
    }
}

impl Download {
    async fn new(
        download_id: String,
        title: String,
        status: String,
        output_path: String,
        tracked_download_state: String,
        split_complete: bool,
    ) -> Download {
        let cue_files = CueFile::find(download_id.borrow()).await.unwrap();
        Download {
            download_id,
            title,
            status,
            output_path,
            tracked_download_state,
            cue_files,
            split_complete,
        }
    }

    // pub async fn find(download_id: String) -> Result<Download, ExitFailure> {
    //     let conn = establish_connection().await?;
    //     let result = conn.query_row(
    //         "SELECT download_id, title, status, output_path, tracked_download_state, split_complete \
    //             FROM downloads where download_id = ?",
    //         [download_id],
    //         |row| {
    //             Ok(Download::new(
    //                 row.get_unwrap(0),
    //                 row.get_unwrap(1),
    //                 row.get_unwrap(2),
    //                 row.get_unwrap(3),
    //                 row.get_unwrap(4),
    //                 row.get_unwrap(5)
    //             ))
    //         },
    //     )?;
    //     Ok(result.await)
    // }

    pub async fn all() -> Result<Vec<Download>, ExitFailure> {
        let conn = establish_connection().await?;
        let mut stmt = conn.prepare("SELECT download_id, title, status, output_path, tracked_download_state, split_complete \
                FROM downloads")?;
        let iter = stmt.query_map([], |row| {
            Ok(Download::new(
                row.get_unwrap(0),
                row.get_unwrap(1),
                row.get_unwrap(2),
                row.get_unwrap(3),
                row.get_unwrap(4),
                row.get_unwrap(5),
            ))
        })?;
        let mut downloads = vec![];
        for download in iter {
            downloads.push(download.unwrap().await);
        }
        Ok(downloads)
    }

    pub async fn save(&self) -> Result<(), ExitFailure> {
        let conn = establish_connection().await?;
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
                    tracked_download_state = :tracked_download_state,
                    split_complete = :split_complete
                  where download_id = :download_id",
            )
        } else {
            conn.prepare(
                "INSERT INTO
                  downloads (download_id, title, status, output_path, tracked_download_state, split_complete)
                VALUES
                  (
                    :download_id, :title, :status, :output_path, :tracked_download_state, :split_complete
                  )",
            )
        }?;
        statement.execute(named_params! {
            ":download_id": self.download_id,
            ":title": self.title,
            ":status": self.status,
            ":output_path": self.output_path,
            ":tracked_download_state": self.tracked_download_state,
            ":split_complete": self.split_complete,
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

    pub async fn delete(&mut self) {
        let conn = establish_connection().await.unwrap();
        let _ = conn.execute(
            "DELETE FROM tracks WHERE download_id = :download_id",
            named_params! { ":download_id": self.download_id},
        );
        let _ = conn.execute(
            "DELETE FROM cue_files WHERE download_id = :download_id",
            named_params! { ":download_id": self.download_id},
        );
        self.cue_files = vec![];
        let _ = conn.execute(
            "DELETE FROM downloads WHERE download_id = :download_id",
            named_params! { ":download_id": self.download_id},
        );
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
        let conn = establish_connection().await?;
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
        let conn = establish_connection().await?;
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

    pub async fn delete(&mut self) {
        let conn = establish_connection().await.unwrap();
        let _ = conn.execute(
            "DELETE FROM tracks WHERE cue_file_id = :cue_file_id",
            named_params! { ":cue_file_id": self.id},
        );
        self.tracks = vec![];
        let _ = conn.execute(
            "DELETE FROM cue_files WHERE id = :id",
            named_params! { ":id": self.id},
        );
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
    async fn find(cue_file_id: Uuid) -> Result<Vec<Track>, ExitFailure> {
        let conn = establish_connection().await?;
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
        let conn = establish_connection().await?;
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

    pub async fn delete(&self) {
        let conn = establish_connection().await.unwrap();
        let _ = conn.execute(
            "DELETE FROM tracks WHERE id = :id",
            named_params! { ":id": self.id},
        );
    }
}
