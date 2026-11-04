//! Where the metadata of pictures and videos is kept between runs. Each record is keyed by the file's path and is only
//! valid while the file keeps the modification time and size it was read at.

use std::io;
use std::path::Path;

use crate::fs::metadata::MediaMetadata;

/// What is known about a file: its metadata, and the movie hash and file type that only the search index keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stored {
    pub metadata: MediaMetadata,
    pub hash: Option<String>,
    pub file_type: Option<String>,
}

pub trait MetadataStore {
    /// The record for the file at `path`, if it was saved at this modification time. A record saved at another time is
    /// out of date, so it is removed, and nothing is returned.
    fn load(&self, path: &Path, mtime: i64) -> Option<Stored>;

    /// Saves the record for the file at `path`, read at this modification time and size.
    fn save(&self, path: &Path, mtime: i64, size: u64, stored: &Stored) -> io::Result<()>;

    /// Whether the store keeps the file's movie hash, which then has to be read along with its metadata.
    fn keeps_hash(&self) -> bool;
}
