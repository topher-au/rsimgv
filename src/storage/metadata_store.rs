//! Where the metadata of pictures and videos is kept between runs. Each record is keyed by the file's path and is out
//! of date once the file's modification time changes. Every store can look up, record and forget a file's record. Only
//! a store that can search across files, the database, also implements `search`.

use std::io;
use std::path::{Path, PathBuf};

use crate::fs::metadata::MediaMetadata;
use crate::search::query::Token;

/// What is known about a file, as a store keeps it. Each store keeps only some of these fields: the JSON files keep the
/// raw probe, and the database keeps the metadata, the movie hash and the file type.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stored {
    pub metadata: MediaMetadata,
    /// The file's content hash, which per-file settings are found by.
    pub hash: Option<String>,
    /// The file's content type.
    pub file_type: Option<String>,
    /// The raw `ffprobe` output the metadata was read from.
    pub probe: Option<serde_json::Value>,
}

pub trait MetadataStore {
    /// The record of the file at `path`, if it was recorded at this modification time and size. A record from
    /// another time, or of another size, is out of date: it is forgotten, and nothing is returned.
    fn lookup(&self, path: &Path, mtime: i64, size: u64) -> Option<Stored>;

    /// Records what was read from the file at `path`, at this modification time and size.
    fn record(&self, path: &Path, mtime: i64, size: u64, stored: &Stored) -> io::Result<()>;

    /// Forgets the record of the file at `path`, if there is one.
    fn forget(&self, path: &Path) -> io::Result<()>;

    /// Whether the store keeps each file's content hash, which then has to be read along with its metadata.
    fn keeps_hash(&self) -> bool;

    /// The paths of the files whose records match `tokens`. `None` for a store that cannot search across files.
    // Nothing searches yet; the search bar will.
    #[allow(dead_code)]
    fn search(&self, tokens: &[Token]) -> Option<Vec<PathBuf>> {
        let _ = tokens;
        None
    }
}
