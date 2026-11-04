//! The search index: a SQLite database of the pictures and videos that have been read. Each file has a row in `file`,
//! with its path, modification time and size, the metadata derived from its probe, and its movie hash and file type.
//! Its `ffprobe` output is a tree, kept in `probe_node`: a row for each dict, list and value, linked to the row above it,
//! so any key at any depth can be searched for. It is the only store that can search across files.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;

use super::metadata_store::{MetadataStore, Stored};
use crate::fs::metadata::MediaMetadata;
use crate::search::query::{matches_all, Facts, Token};

/// The version of the database's layout, kept in `PRAGMA user_version`.
const VERSION: i64 = 3;

const SCHEMA: &str = "
    CREATE TABLE file (
        id INTEGER PRIMARY KEY,
        path BLOB NOT NULL UNIQUE,
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
    CREATE INDEX file_hash ON file (hash);
    CREATE TABLE probe_node (
        id INTEGER PRIMARY KEY,
        file_id INTEGER NOT NULL REFERENCES file (id) ON DELETE CASCADE,
        parent_id INTEGER REFERENCES probe_node (id) ON DELETE CASCADE,
        key TEXT,
        position INTEGER,
        kind TEXT NOT NULL,
        value TEXT,
        number REAL
    );
    CREATE INDEX probe_node_file ON probe_node (file_id);
    CREATE INDEX probe_node_parent ON probe_node (parent_id);
    CREATE INDEX probe_node_key_value ON probe_node (key COLLATE NOCASE, value COLLATE NOCASE);
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
        // Deleting a file removes its probe too, which needs the foreign key to be enforced.
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < VERSION {
            // The index is a cache of what is read from the files, so an older layout is dropped. The files are read
            // again as they are needed, and recorded in the new layout.
            connection.execute_batch(
                "DROP TABLE IF EXISTS probe_node; DROP TABLE IF EXISTS file; \
                 DROP TABLE IF EXISTS probe_field; DROP TABLE IF EXISTS media;",
            )?;
            connection.execute_batch(SCHEMA)?;
            connection.pragma_update(None, "user_version", VERSION)?;
        }
        Ok(Self { connection })
    }

    /// The paths of the files with a key called `key`, at any depth of the probe, whose value is `value` ignoring case.
    /// So `title` finds `format.tags.title` and `streams.0.tags.title` alike.
    // Nothing searches yet; the search bar will.
    #[allow(dead_code)]
    pub fn files_with_key(&self, key: &str, value: &str) -> rusqlite::Result<Vec<PathBuf>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT file.path FROM probe_node JOIN file ON file.id = probe_node.file_id
             WHERE probe_node.key = ?1 COLLATE NOCASE AND probe_node.value = ?2 COLLATE NOCASE",
        )?;
        let rows = statement.query_map(params![key, value], |row| row.get::<_, Vec<u8>>(0))?;
        rows.map(|row| row.map(|bytes| PathBuf::from(OsString::from_vec(bytes)))).collect()
    }

    /// The probe recorded for the file at `path`, rebuilt from its rows, or `None` if there is none.
    // Nothing reads the probe back yet; the search reads the file's row.
    #[allow(dead_code)]
    pub fn probe_of(&self, path: &Path) -> rusqlite::Result<Option<Value>> {
        let root: Option<i64> = self
            .connection
            .query_row(
                "SELECT probe_node.id FROM probe_node JOIN file ON file.id = probe_node.file_id
                 WHERE file.path = ?1 AND probe_node.parent_id IS NULL",
                params![path.as_os_str().as_bytes()],
                |row| row.get(0),
            )
            .optional()?;
        root.map(|id| self.node_value(id)).transpose()
    }

    /// The value of the node `id`: its children rebuilt in the order they were recorded, or its scalar.
    fn node_value(&self, id: i64) -> rusqlite::Result<Value> {
        let (kind, text): (String, Option<String>) = self.connection.query_row(
            "SELECT kind, value FROM probe_node WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = self
            .connection
            .prepare("SELECT id, key FROM probe_node WHERE parent_id = ?1 ORDER BY id")?;
        let children: Vec<(i64, Option<String>)> =
            statement.query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(match kind.as_str() {
            "object" => Value::Object(
                children
                    .into_iter()
                    .map(|(child, key)| Ok((key.unwrap_or_default(), self.node_value(child)?)))
                    .collect::<rusqlite::Result<_>>()?,
            ),
            "array" => Value::Array(
                children.into_iter().map(|(child, _)| self.node_value(child)).collect::<rusqlite::Result<_>>()?,
            ),
            "string" => Value::String(text.unwrap_or_default()),
            "bool" => Value::Bool(text.as_deref() == Some("true")),
            _ => {
                let text = text.unwrap_or_default();
                match text.parse::<i64>() {
                    Ok(integer) => Value::from(integer),
                    Err(_) => text.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number),
                }
            }
        })
    }
}

/// Records the node for `value` under the file `file_id`, then its children, which are linked to it. The root has no
/// parent or key. A null is not recorded, as it says nothing.
fn insert_node(
    transaction: &Transaction,
    file_id: i64,
    parent: Option<i64>,
    key: Option<&str>,
    position: Option<i64>,
    value: &Value,
) -> rusqlite::Result<()> {
    let (kind, text, number) = match value {
        Value::Null => return Ok(()),
        Value::Object(_) => ("object", None, None),
        Value::Array(_) => ("array", None, None),
        Value::String(text) => ("string", Some(text.clone()), text.parse::<f64>().ok()),
        Value::Number(number) => ("number", Some(number.to_string()), number.as_f64()),
        Value::Bool(flag) => ("bool", Some(flag.to_string()), None),
    };
    transaction.execute(
        "INSERT INTO probe_node (file_id, parent_id, key, position, kind, value, number)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![file_id, parent, key, position, kind, text, number],
    )?;
    let id = transaction.last_insert_rowid();
    match value {
        Value::Object(map) => {
            for (child_key, child) in map {
                insert_node(transaction, file_id, Some(id), Some(child_key), None, child)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                insert_node(transaction, file_id, Some(id), None, Some(index as i64), child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

impl MetadataStore for MediaIndex {
    fn lookup(&self, path: &Path, mtime: i64) -> Option<Stored> {
        let row = self
            .connection
            .query_row(
                "SELECT mtime, hash, file_type, width, height, duration, frame_rate, codec, audio_codec,
                        audio_channels, bit_rate
                 FROM file WHERE path = ?1",
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
        // Replacing the file's row removes its probe nodes with it.
        transaction.execute("DELETE FROM file WHERE path = ?1", params![key]).map_err(io::Error::other)?;
        transaction
            .execute(
                "INSERT INTO file (path, mtime, size, hash, file_type, width, height, duration, frame_rate, codec,
                                   audio_codec, audio_channels, bit_rate)
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
            let file_id = transaction.last_insert_rowid();
            insert_node(&transaction, file_id, None, None, None, probe).map_err(io::Error::other)?;
        }
        transaction.commit().map_err(io::Error::other)
    }

    fn forget(&self, path: &Path) -> io::Result<()> {
        self.connection
            .execute("DELETE FROM file WHERE path = ?1", params![path.as_os_str().as_bytes()])
            .map(|_| ())
            .map_err(io::Error::other)
    }

    fn keeps_hash(&self) -> bool {
        true
    }

    /// Matches each file's row, then keeps only the files that are still as the row says: a file that has gone, or
    /// changed since, is forgotten rather than found.
    // Nothing searches yet; the search bar will.
    #[allow(dead_code)]
    fn search(&self, tokens: &[Token]) -> Option<Vec<PathBuf>> {
        let rows = self
            .connection
            .prepare("SELECT path, mtime, size, file_type, width, height, duration, frame_rate FROM file")
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
    fn a_probe_is_kept_as_a_tree_and_rebuilt_as_it_was() {
        let (index, path) = index();
        let probe = json!({
            "streams": [
                {"index": 0, "codec_type": "video", "width": 1920, "tags": {"encoder": "Lavf"}},
                {"index": 1, "codec_type": "audio", "disposition": {"default": 1, "forced": false}},
            ],
            "format": {"format_name": "mov", "duration": "12.5", "tags": {"title": "Holiday"}},
        });
        let stored = Stored { probe: Some(probe.clone()), ..Stored::default() };
        index.record(&path, 100, 4096, &stored).unwrap();
        assert_eq!(index.probe_of(&path).unwrap(), Some(probe));
    }

    #[test]
    fn any_key_at_any_depth_is_searchable_by_its_value() {
        let (index, path) = index();
        let probe = json!({
            "format": {"tags": {"title": "Holiday"}, "duration": "12.5"},
            "streams": [{"codec_type": "video", "tags": {"encoder": "Lavf"}, "disposition": {"default": 1}}],
        });
        let stored = Stored { probe: Some(probe), ..Stored::default() };
        index.record(&path, 100, 4096, &stored).unwrap();
        assert_eq!(index.files_with_key("title", "holiday").unwrap(), vec![path.clone()], "any case matches");
        assert_eq!(index.files_with_key("encoder", "Lavf").unwrap(), vec![path.clone()], "a stream's tag is found");
        assert_eq!(index.files_with_key("default", "1").unwrap(), vec![path.clone()], "a disposition is found");
        assert!(index.files_with_key("title", "Summer").unwrap().is_empty());
        index.forget(&path).unwrap();
        assert!(index.files_with_key("title", "Holiday").unwrap().is_empty(), "forgetting removes the probe too");
    }
}
