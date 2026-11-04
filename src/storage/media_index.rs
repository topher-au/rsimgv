//! The search index: a SQLite database with a row for each picture and video that has been read. Besides its metadata,
//! a row keeps the file's movie hash, which its per-file settings are found by, and its file type, so that files can be
//! found by what they are as well as by their name.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};

use super::metadata_store::{MetadataStore, Stored};
use crate::fs::metadata::MediaMetadata;

/// The version of the database's layout, kept in `PRAGMA user_version`.
const VERSION: i64 = 1;

const SCHEMA: &str = "
    CREATE TABLE media (
        path BLOB PRIMARY KEY,
        mtime INTEGER NOT NULL,
        size INTEGER NOT NULL,
        hash TEXT,
        file_type TEXT,
        width INTEGER,
        height INTEGER,
        duration REAL,
        frame_rate REAL,
        codec TEXT,
        audio_codec TEXT,
        audio_channels INTEGER,
        bit_rate INTEGER
    );
    CREATE INDEX media_hash ON media (hash);
";

pub struct MediaIndex {
    connection: Connection,
}

impl MediaIndex {
    /// Opens the index in `file`, creating it if it does not exist. Several windows, or processes, may share it.
    pub fn open(file: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let connection = Connection::open(file).with_context(|| format!("opening {}", file.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            connection.execute_batch(SCHEMA)?;
            connection.pragma_update(None, "user_version", VERSION)?;
        }
        Ok(Self { connection })
    }
}

impl MetadataStore for MediaIndex {
    fn load(&self, path: &Path, mtime: i64) -> Option<Stored> {
        let row = self
            .connection
            .query_row(
                "SELECT mtime, hash, file_type, width, height, duration, frame_rate, codec, audio_codec,
                        audio_channels, bit_rate
                 FROM media WHERE path = ?1",
                params![path.as_os_str().as_encoded_bytes()],
                |row| {
                    let stored = Stored {
                        metadata: MediaMetadata {
                            width: row.get::<_, Option<i64>>(3)?.map(|width| width as u32),
                            height: row.get::<_, Option<i64>>(4)?.map(|height| height as u32),
                            duration: row.get(5)?,
                            frame_rate: row.get(6)?,
                            codec: row.get(7)?,
                            audio_codec: row.get(8)?,
                            audio_channels: row.get::<_, Option<i64>>(9)?.map(|channels| channels as u32),
                            bit_rate: row.get::<_, Option<i64>>(10)?.map(|bits| bits as u64),
                        },
                        hash: row.get(1)?,
                        file_type: row.get(2)?,
                    };
                    Ok((row.get::<_, i64>(0)?, stored))
                },
            )
            .optional();
        match row {
            Ok(Some((row_mtime, stored))) if row_mtime == mtime => Some(stored),
            Ok(Some(_)) => {
                // Out of date: the row goes, so the file is read again and saved with its new modification time.
                if let Err(err) = self.connection.execute("DELETE FROM media WHERE path = ?1", params![path.as_os_str().as_encoded_bytes()]) {
                    eprintln!("rsimgv: could not remove an out-of-date search index row: {err}");
                }
                None
            }
            Ok(None) => None,
            Err(err) => {
                eprintln!("rsimgv: could not read the search index: {err}");
                None
            }
        }
    }

    fn save(&self, path: &Path, mtime: i64, size: u64, stored: &Stored) -> std::io::Result<()> {
        let metadata = &stored.metadata;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO media (path, mtime, size, hash, file_type, width, height, duration, frame_rate,
                                               codec, audio_codec, audio_channels, bit_rate)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    path.as_os_str().as_encoded_bytes(),
                    mtime,
                    size as i64,
                    stored.hash,
                    stored.file_type,
                    metadata.width.map(i64::from),
                    metadata.height.map(i64::from),
                    metadata.duration,
                    metadata.frame_rate,
                    metadata.codec,
                    metadata.audio_codec,
                    metadata.audio_channels.map(i64::from),
                    metadata.bit_rate.map(|bits| bits as i64),
                ],
            )
            .map(|_| ())
            .map_err(std::io::Error::other)
    }

    fn keeps_hash(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_record_is_removed_when_looked_up() {
        let dir = std::env::temp_dir().join(format!("rsimgv-index-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let index = MediaIndex::open(&dir.join("index.sqlite3")).unwrap();
        let path = dir.join("clip.mp4");
        let stored = Stored {
            metadata: MediaMetadata { width: Some(1920), height: Some(1080), duration: Some(12.5), ..MediaMetadata::default() },
            hash: Some("0123456789abcdef".into()),
            file_type: Some("video/mp4".into()),
        };
        index.save(&path, 100, 4096, &stored).unwrap();
        assert_eq!(index.load(&path, 100), Some(stored), "the record is current");
        assert_eq!(index.load(&path, 101), None, "a changed modification time is out of date");
        assert_eq!(index.load(&path, 100), None, "the out-of-date record was removed, so it is gone for good");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
