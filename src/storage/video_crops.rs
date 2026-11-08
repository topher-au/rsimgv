//! The crop locked for a video, kept by path in the metadata database so the thumbnail maker can use it without
//! reading the video's content hash.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::mpv::framing::CropRect;

use super::media_index::MediaIndex;

/// The locked crops of every video.
pub struct VideoCrops {
    index: Mutex<MediaIndex>,
}

impl VideoCrops {
    /// Opens the crops in `file`, creating the database if it does not exist.
    pub fn open(file: &Path) -> anyhow::Result<Self> {
        Ok(Self { index: Mutex::new(MediaIndex::open(file)?) })
    }

    /// The crops of this app, opened the first time one is asked for. `None` if the database cannot be opened.
    pub fn shared() -> Option<&'static Self> {
        static SHARED: OnceLock<Option<VideoCrops>> = OnceLock::new();
        SHARED
            .get_or_init(|| match Self::open(&MediaIndex::file()) {
                Ok(crops) => Some(crops),
                Err(err) => {
                    eprintln!("rsimgv: the locked crops are unavailable: {err:#}");
                    None
                }
            })
            .as_ref()
    }

    fn lock(&self) -> MutexGuard<'_, MediaIndex> {
        self.index.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The crop locked for the video at `path`, if one is.
    pub fn get(&self, path: &Path) -> rusqlite::Result<Option<CropRect>> {
        self.lock().video_crop(path)
    }

    /// Keeps `crop` for the video at `path`, or clears it for `None`.
    pub fn set(&self, path: &Path, crop: Option<CropRect>) -> rusqlite::Result<()> {
        self.lock().set_video_crop(path, crop)
    }
}
