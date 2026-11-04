//! The search index: a SQLite database with a row for each picture and video that has been read. Besides its metadata,
//! a row keeps the file's movie hash, which its per-file settings are found by, and its file type, so that files can be
//! found by what they are as well as by their name. Every field of the `ffprobe` output is also a row of its own, so
//! that any key can be searched for without the index knowing it in advance. It is the only store that can search
//! across files.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use super::metadata_store::{MetadataStore, Stored};
use crate::fs::metadata::MediaMetadata;
use crate::search::query::{matches_all, Facts, Token};

/// The version of the database's layout, kept in `PRAGMA user_version`.
const VERSION: i64 = 2;

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
    CREATE TABLE probe_field (
        path BLOB NOT NULL,
        field TEXT NOT NULL,
        name TEXT NOT NULL,
        value TEXT NOT NULL,
        number REAL,
        PRIMARY KEY (path, field)
    );
    CREATE INDEX probe_field_lookup ON probe_field (name, value COLLATE NOCASE);
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
        if version < VERSION {
            // The index is a cache of what is read from the files, so an older layout is dropped. The files are read
            // again as they are needed, and recorded in the new layout.
            connection.execute_batch("DROP TABLE IF EXISTS probe_field; DROP TABLE IF EXISTS media;")?;
            connection.execute_batch(SCHEMA)?;
            connection.pragma_update(None, "user_version", VERSION)?;
        }
        Ok(Self { connection })
    }

    /// The paths of the files with a field called `name`, in the format or any stream, whose value is `value` ignoring
    /// case. `name` is the last part of the field's path, so `title` finds `format.tags.title` and `streams.0.tags.title`.
    // Nothing searches yet; the search bar will.
    #[allow(dead_code)]
    pub fn files_with_field(&self, name: &str, value: &str) -> rusqlite::Result<Vec<PathBuf>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT path FROM probe_field WHERE name = ?1 AND value = ?2 COLLATE NOCASE",
        )?;
        let rows = statement.query_map(params![name.to_lowercase(), value], |row| row.get::<_, Vec<u8>>(0))?;
        rows.map(|row| row.map(|bytes| PathBuf::from(OsString::from_vec(bytes)))).collect()
    }
}

impl MetadataStore for MediaIndex {
    fn lookup(&self, path: &Path, mtime: i64) -> Option<Stored> {
        let row = self
            .connection
            .query_row(
                "SELECT mtime, hash, file_type, width, height, duration, frame_rate, codec, audio_codec,
                        audio_channels, bit_rate
                 FROM media WHERE path = ?1",
                params![path.as_os_str().as_bytes()],
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
                        probe: None,
                    };
                    Ok((row.get::<_, i64>(0)?, stored))
                },
            )
            .optional();
        match row {
            Ok(Some((row_mtime, stored))) if row_mtime == mtime => Some(stored),
            Ok(Some(_)) => {
                self.forget_or_log(path);
                None
            }
            Ok(None) => None,
            Err(err) => {
                eprintln!("rsimgv: could not read the search index: {err}");
                None
            }
        }
    }

    fn record(&self, path: &Path, mtime: i64, size: u64, stored: &Stored) -> io::Result<()> {
        let key = path.as_os_str().as_bytes();
        let metadata = &stored.metadata;
        let transaction = self.connection.unchecked_transaction().map_err(io::Error::other)?;
        transaction.execute("DELETE FROM probe_field WHERE path = ?1", params![key]).map_err(io::Error::other)?;
        transaction
            .execute(
                "INSERT OR REPLACE INTO media (path, mtime, size, hash, file_type, width, height, duration, frame_rate,
                                               codec, audio_codec, audio_channels, bit_rate)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    key,
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
            .map_err(io::Error::other)?;
        if let Some(probe) = &stored.probe {
            let mut fields = Vec::new();
            flatten(probe, "", &mut fields);
            let mut insert = transaction
                .prepare("INSERT INTO probe_field (path, field, name, value, number) VALUES (?1, ?2, ?3, ?4, ?5)")
                .map_err(io::Error::other)?;
            for field in &fields {
                insert
                    .execute(params![key, field.field, field.name, field.value, field.number])
                    .map_err(io::Error::other)?;
            }
        }
        transaction.commit().map_err(io::Error::other)
    }

    fn forget(&self, path: &Path) -> io::Result<()> {
        let key = path.as_os_str().as_bytes();
        let transaction = self.connection.unchecked_transaction().map_err(io::Error::other)?;
        transaction.execute("DELETE FROM probe_field WHERE path = ?1", params![key]).map_err(io::Error::other)?;
        transaction.execute("DELETE FROM media WHERE path = ?1", params![key]).map_err(io::Error::other)?;
        transaction.commit().map_err(io::Error::other)
    }

    fn keeps_hash(&self) -> bool {
        true
    }

    /// Matches each row's values, then keeps only the files that are still as the row says: a file that has gone, or
    /// changed since, is forgotten rather than found.
    #[allow(dead_code)] // Nothing searches yet; the search bar will.
    fn search(&self, tokens: &[Token]) -> Option<Vec<PathBuf>> {
        let rows = self
            .connection
            .prepare("SELECT path, mtime, size, file_type, width, height, duration, frame_rate FROM media")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| {
                        let path: Vec<u8> = row.get(0)?;
                        let facts = Facts {
                            name: String::new(),
                            content_type: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                            size: row.get::<_, i64>(2)? as u64,
                            modified: row.get(1)?,
                            width: row.get::<_, Option<i64>>(4)?.map(|width| width as u32),
                            height: row.get::<_, Option<i64>>(5)?.map(|height| height as u32),
                            duration: row.get(6)?,
                            fps: row.get(7)?,
                        };
                        Ok((PathBuf::from(OsString::from_vec(path)), facts))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
            });
        let rows = match rows {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("rsimgv: could not search the search index: {err}");
                return Some(Vec::new());
            }
        };
        let mut found = Vec::new();
        for (path, facts) in rows {
            let facts = Facts { name: file_name(&path), ..facts };
            if !matches_all(tokens, &facts) {
                continue;
            }
            match current_mtime(&path) {
                Some(mtime) if self.lookup(&path, mtime).is_some() => found.push(path),
                Some(_) => {}
                None => self.forget_or_log(&path),
            }
        }
        Some(found)
    }
}

impl MediaIndex {
    /// Forgets `path`, logging a failure, since a lookup has no way to report one.
    fn forget_or_log(&self, path: &Path) {
        if let Err(err) = self.forget(path) {
            eprintln!("rsimgv: could not remove an out-of-date search index row: {err}");
        }
    }
}

/// One value of the `ffprobe` output, as a row of the probe table.
struct ProbeField {
    /// Its path in the output, such as `streams.0.tags.title`.
    field: String,
    /// The last part of the path that is not a position in an array, lower case, such as `title`.
    name: String,
    value: String,
    /// The value as a number, when it reads as one.
    number: Option<f64>,
}

/// Adds every value under `value` to `out`, named by its path below `field`. Objects and arrays are walked, so keys
/// that ffprobe did not name in advance are included, and null values are left out.
fn flatten(value: &Value, field: &str, out: &mut Vec<ProbeField>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                flatten(child, &join(field, key), out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                flatten(child, &join(field, &index.to_string()), out);
            }
        }
        Value::Null => {}
        scalar => {
            let value = match scalar {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            out.push(ProbeField {
                field: field.to_owned(),
                name: name_of(field),
                number: value.parse().ok(),
                value,
            });
        }
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

/// The last part of `field` that is not a position in an array, in lower case.
fn name_of(field: &str) -> String {
    field
        .rsplit('.')
        .find(|part| !part.bytes().all(|byte| byte.is_ascii_digit()))
        .unwrap_or_default()
        .to_lowercase()
}

/// The name of the file at `path`, as its last component, lossily.
#[allow(dead_code)] // Used by the search, which nothing calls yet.
fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The modification time of the file at `path` in Unix seconds, if it can be read.
#[allow(dead_code)] // Used by the search, which nothing calls yet.
fn current_mtime(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn index() -> (MediaIndex, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rsimgv-index-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        (MediaIndex::open(&dir.join("index.sqlite3")).unwrap(), dir.join("clip.mp4"))
    }

    #[test]
    fn a_stale_record_is_removed_when_looked_up() {
        let (index, path) = index();
        let stored = Stored {
            metadata: MediaMetadata { width: Some(1920), height: Some(1080), duration: Some(12.5), ..MediaMetadata::default() },
            hash: Some("0123456789abcdef".into()),
            file_type: Some("video/mp4".into()),
            probe: None,
        };
        index.record(&path, 100, 4096, &stored).unwrap();
        assert_eq!(index.lookup(&path, 100), Some(stored), "the record is current");
        assert_eq!(index.lookup(&path, 101), None, "a changed modification time is out of date");
        assert_eq!(index.lookup(&path, 100), None, "the out-of-date record was removed, so it is gone for good");
    }

    #[test]
    fn any_probe_field_is_searchable_by_its_name_and_value() {
        let (index, path) = index();
        let probe = json!({
            "format": {"tags": {"title": "Holiday"}, "duration": "12.5"},
            "streams": [{"codec_type": "video", "tags": {"encoder": "Lavf"}, "disposition": {"default": 1}}],
        });
        let stored = Stored { probe: Some(probe), ..Stored::default() };
        index.record(&path, 100, 4096, &stored).unwrap();
        assert_eq!(index.files_with_field("title", "holiday").unwrap(), vec![path.clone()], "any case matches");
        assert_eq!(index.files_with_field("encoder", "Lavf").unwrap(), vec![path.clone()], "a stream's tag is found");
        assert_eq!(index.files_with_field("default", "1").unwrap(), vec![path.clone()], "a disposition is found");
        assert!(index.files_with_field("title", "Summer").unwrap().is_empty());
        index.forget(&path).unwrap();
        assert!(index.files_with_field("title", "Holiday").unwrap().is_empty(), "forgetting removes the fields too");
    }

    #[test]
    fn probe_values_keep_their_numbers() {
        let mut fields = Vec::new();
        flatten(&json!({"format": {"duration": "12.5", "format_name": "mov"}}), "", &mut fields);
        let duration = fields.iter().find(|field| field.name == "duration").unwrap();
        assert_eq!(duration.number, Some(12.5));
        let format_name = fields.iter().find(|field| field.name == "format_name").unwrap();
        assert_eq!(format_name.number, None);
    }
}
