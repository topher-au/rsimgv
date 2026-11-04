//! The search index: a SQLite database of the pictures and videos that have been read. Each file has a row in `file`,
//! with its path, modification time and size, the metadata derived from its probe, and its movie hash and file type.
//! Its `ffprobe` output is a tree, kept in `probe_node`: a row for each dict, list and value, linked to the row above it,
//! so any key at any depth can be searched for. Each key's name and each string value is kept once, in `probe_word`,
//! and the nodes refer to it by its id. It is the only store that can search across files.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Value as SqlValue, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;

use super::metadata_store::{MetadataStore, Stored};
use crate::fs::metadata::MediaMetadata;
use crate::search::query::{matches_all, Facts, Token};

/// The version of the database's layout, kept in `PRAGMA user_version`.
const VERSION: i64 = 12;

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
        bit_rate INTEGER
    );
    CREATE TABLE probe_word (
        id INTEGER PRIMARY KEY,
        text TEXT NOT NULL UNIQUE
    );
    CREATE INDEX probe_word_search ON probe_word (text COLLATE NOCASE);
    CREATE TABLE probe_node (
        id INTEGER PRIMARY KEY,
        file_id INTEGER NOT NULL REFERENCES file (id) ON DELETE CASCADE,
        parent_id INTEGER REFERENCES probe_node (id) ON DELETE CASCADE,
        key_id INTEGER REFERENCES probe_word (id),
        kind INTEGER NOT NULL,
        value
    );
    CREATE INDEX probe_node_file ON probe_node (file_id);
    CREATE INDEX probe_node_parent ON probe_node (parent_id);
    CREATE INDEX probe_node_key_value ON probe_node (key_id, value COLLATE NOCASE);
    -- Each node with its file's path, its key's name and its value as text, for reading the index by hand. The
    -- search and lookups query the tables directly.
    CREATE VIEW probe_text AS
        SELECT probe_node.id, probe_node.file_id, file.path, probe_node.parent_id, probe_word.text AS key,
               probe_node.kind,
               CASE probe_node.kind
                   WHEN 2 THEN (SELECT text FROM probe_word WHERE id = probe_node.value)
                   ELSE probe_node.value
               END AS value
        FROM probe_node
        JOIN file ON file.id = probe_node.file_id
        LEFT JOIN probe_word ON probe_word.id = probe_node.key_id;
    -- Each scalar with its file's path, its name in the probe (`probe.streams[0].width`) and its value. A name is built
    -- from the root down. An array item's position is its place among its siblings, since they are recorded in order.
    CREATE VIEW probe_property AS
        WITH RECURSIVE
        step AS (
            SELECT probe_node.id, probe_node.file_id, probe_node.parent_id, probe_node.kind, probe_node.value,
                   CASE WHEN probe_node.key_id IS NOT NULL THEN '.' || probe_word.text
                        ELSE '[' || (ROW_NUMBER() OVER (PARTITION BY probe_node.parent_id ORDER BY probe_node.id) - 1) || ']'
                   END AS text
            FROM probe_node
            LEFT JOIN probe_word ON probe_word.id = probe_node.key_id
        ),
        name(id, file_id, kind, value, text) AS (
            SELECT id, file_id, kind, value, 'probe' FROM step WHERE parent_id IS NULL
            UNION ALL
            SELECT step.id, step.file_id, step.kind, step.value, name.text || step.text
            FROM step JOIN name ON step.parent_id = name.id
        )
        SELECT file.path AS file_path, name.text AS name,
               CASE name.kind
                   WHEN 2 THEN (SELECT text FROM probe_word WHERE id = name.value)
                   ELSE name.value
               END AS value
        FROM name
        JOIN file ON file.id = name.file_id
        WHERE name.kind NOT IN (0, 1);
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
        // WAL is durable enough with NORMAL, and each commit is cheaper than with FULL.
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        // Deleting a file removes its probe too, which needs the foreign key to be enforced.
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < VERSION {
            // The index is a cache of what is read from the files, so an older layout is dropped. The files are read
            // again as they are needed, and recorded in the new layout.
            connection.execute_batch(
                "DROP VIEW IF EXISTS probe_property; DROP VIEW IF EXISTS probe_text; DROP TABLE IF EXISTS probe_node; DROP TABLE IF EXISTS probe_word; \
                 DROP TABLE IF EXISTS probe_key; \
                 DROP TABLE IF EXISTS file; DROP TABLE IF EXISTS probe_field; DROP TABLE IF EXISTS media;",
            )?;
            // Freed pages are given back to the file as they are freed, rather than kept for the next write. The setting
            // applies to a file only when it is rebuilt, so the VACUUM comes first, and before any tables are made.
            connection.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
            connection.execute_batch("VACUUM")?;
            connection.execute_batch(SCHEMA)?;
            connection.pragma_update(None, "user_version", VERSION)?;
        }
        connection.execute_batch("PRAGMA incremental_vacuum")?;
        Ok(Self { connection })
    }

    /// The paths of the files with a key called `key`, at any depth of the probe, whose value is `value` ignoring case.
    /// So `title` finds `format.tags.title` and `streams.0.tags.title` alike.
    // Nothing searches yet; the search bar will.
    #[allow(dead_code)]
    pub fn files_with_key(&self, key: &str, value: &str) -> rusqlite::Result<Vec<PathBuf>> {
        // A string value is stored as the id of its word, and any other value as its own number, so the text is matched
        // as a word if the value is a string, and as a number if it reads as one.
        let number = value
            .parse::<i64>()
            .map(SqlValue::Integer)
            .or_else(|_| value.parse::<f64>().map(SqlValue::Real))
            .unwrap_or(SqlValue::Null);
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT file.path FROM probe_node JOIN file ON file.id = probe_node.file_id
             WHERE probe_node.key_id IN (SELECT id FROM probe_word WHERE text = ?1 COLLATE NOCASE)
               AND ((probe_node.kind = ?2 AND probe_node.value IN
                        (SELECT id FROM probe_word WHERE text = ?3 COLLATE NOCASE))
                 OR (probe_node.kind <> ?2 AND probe_node.value IN (?4)))",
        )?;
        let rows = statement.query_map(params![key, Kind::String, value, number], |row| row.get::<_, Vec<u8>>(0))?;
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
        let (kind, stored): (Kind, SqlValue) = self.connection.query_row(
            "SELECT kind, value FROM probe_node WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT probe_node.id, probe_word.text FROM probe_node
             LEFT JOIN probe_word ON probe_word.id = probe_node.key_id
             WHERE probe_node.parent_id = ?1 ORDER BY probe_node.id",
        )?;
        let children: Vec<(i64, Option<String>)> =
            statement.query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(match kind {
            Kind::Object => Value::Object(
                children
                    .into_iter()
                    .map(|(child, key)| Ok((key.unwrap_or_default(), self.node_value(child)?)))
                    .collect::<rusqlite::Result<_>>()?,
            ),
            Kind::Array => Value::Array(
                children.into_iter().map(|(child, _)| self.node_value(child)).collect::<rusqlite::Result<_>>()?,
            ),
            Kind::String => Value::String(match stored {
                SqlValue::Integer(word) => self.connection.query_row(
                    "SELECT text FROM probe_word WHERE id = ?1",
                    params![word],
                    |row| row.get(0),
                )?,
                _ => String::new(),
            }),
            Kind::Bool => Value::Bool(matches!(stored, SqlValue::Integer(flag) if flag != 0)),
            Kind::Number => match stored {
                SqlValue::Integer(integer) => Value::from(integer),
                SqlValue::Real(real) => serde_json::Number::from_f64(real).map_or(Value::Null, Value::Number),
                _ => Value::Null,
            },
        })
    }
}

/// What a probe node is. It is stored as its number, so the table does not repeat its name.
#[derive(Clone, Copy)]
enum Kind {
    Object = 0,
    Array = 1,
    String = 2,
    Number = 3,
    Bool = 4,
}

impl ToSql for Kind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(*self as i64))
    }
}

impl FromSql for Kind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match i64::column_result(value)? {
            0 => Ok(Kind::Object),
            1 => Ok(Kind::Array),
            2 => Ok(Kind::String),
            3 => Ok(Kind::Number),
            4 => Ok(Kind::Bool),
            code => Err(FromSqlError::OutOfRange(code)),
        }
    }
}

/// Where a node sits: the key it is under in an object, or its position in an array, and the node above it.
struct Slot<'a> {
    parent: Option<i64>,
    parent_label: &'a str,
    key: Option<&'a str>,
    position: Option<i64>,
}

/// A JSON number as an integer when it is one, otherwise as a real.
fn number_value(number: &serde_json::Number) -> SqlValue {
    number
        .as_i64()
        .map(SqlValue::Integer)
        .or_else(|| number.as_f64().map(SqlValue::Real))
        .unwrap_or(SqlValue::Null)
}

/// The id of the word `text`, which is a key's name or a string value. It is added the first time it is seen.
fn intern_word(transaction: &Transaction, text: &str) -> rusqlite::Result<i64> {
    transaction.query_row(
        "INSERT INTO probe_word (text) VALUES (?1) ON CONFLICT (text) DO UPDATE SET text = text RETURNING id",
        params![text],
        |row| row.get(0),
    )
}

/// Records the node for `value` under the file `file_id`, then its children, which are linked to it. The root has no
/// parent or key. A null is not recorded, as it says nothing. Each scalar is logged as it is recorded.
fn insert_node(transaction: &Transaction, file: &str, file_id: i64, slot: Slot, value: &Value) -> rusqlite::Result<()> {
    // A number or bool is stored as its own type, so a number is compared as one. A string is stored as the id of its
    // word, and is looked up again by that id.
    let (kind, stored, shown) = match value {
        Value::Null => return Ok(()),
        Value::Object(_) => (Kind::Object, SqlValue::Null, None),
        Value::Array(_) => (Kind::Array, SqlValue::Null, None),
        Value::String(text) => (Kind::String, SqlValue::Integer(intern_word(transaction, text)?), Some(text.clone())),
        Value::Number(number) => (Kind::Number, number_value(number), Some(number.to_string())),
        Value::Bool(flag) => (Kind::Bool, SqlValue::Integer(i64::from(*flag)), Some(flag.to_string())),
    };
    let key_id = slot.key.map(|key| intern_word(transaction, key)).transpose()?;
    transaction.execute(
        "INSERT INTO probe_node (file_id, parent_id, key_id, kind, value) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![file_id, slot.parent, key_id, kind, stored],
    )?;
    let id = transaction.last_insert_rowid();
    let label = slot.key.map_or_else(|| slot.position.map(|index| index.to_string()).unwrap_or_default(), str::to_owned);
    if let Some(text) = &shown {
        eprintln!("index-update: {file} {label}={text},{}=[{label}={{{label}={text}}}]", slot.parent_label);
    }
    match value {
        Value::Object(map) => {
            for (child_key, child) in map {
                let slot = Slot { parent: Some(id), parent_label: &label, key: Some(child_key), position: None };
                insert_node(transaction, file, file_id, slot, child)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let slot = Slot { parent: Some(id), parent_label: &label, key: None, position: Some(index as i64) };
                insert_node(transaction, file, file_id, slot, child)?;
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
                "SELECT mtime, hash, file_type, width, height, duration, frame_rate, codec, audio_codec, bit_rate
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
                            bit_rate: row.get::<_, Option<i64>>(9)?.map(|bits| bits as u64),
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
        let name = file_name(path);
        let transaction = self.connection.unchecked_transaction().map_err(io::Error::other)?;
        // Replacing the file's row removes its probe nodes with it.
        if transaction.execute("DELETE FROM file WHERE path = ?1", params![key]).map_err(io::Error::other)? > 0 {
            eprintln!("index-update: {name} removed");
        }
        transaction
            .execute(
                "INSERT INTO file (path, mtime, size, hash, file_type, width, height, duration, frame_rate, codec,
                                   audio_codec, bit_rate)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                    metadata.bit_rate.map(|bits| bits as i64),
                ],
            )
            .map_err(io::Error::other)?;
        if let Some(probe) = &stored.probe {
            let file_id = transaction.last_insert_rowid();
            let slot = Slot { parent: None, parent_label: "", key: None, position: None };
            insert_node(&transaction, &name, file_id, slot, probe).map_err(io::Error::other)?;
        }
        transaction.commit().map_err(io::Error::other)
    }

    fn forget(&self, path: &Path) -> io::Result<()> {
        let removed = self
            .connection
            .execute("DELETE FROM file WHERE path = ?1", params![path.as_os_str().as_bytes()])
            .map_err(io::Error::other)?;
        if removed > 0 {
            eprintln!("index-update: {} removed", file_name(path));
        }
        Ok(())
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
