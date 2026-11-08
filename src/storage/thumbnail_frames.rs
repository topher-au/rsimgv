//! The still frame chosen for a video's thumbnail, when it is not the one made by default. It is kept in the metadata
//! database, by path, because the thumbnail maker has the path and no content hash.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::media_index::MediaIndex;

/// The chosen thumbnail frames of every video.
pub struct ThumbnailFrames {
    index: Mutex<MediaIndex>,
}

impl ThumbnailFrames {
    /// Opens the frames in `file`, creating the database if it does not exist.
    pub fn open(file: &Path) -> anyhow::Result<Self> {
        Ok(Self { index: Mutex::new(MediaIndex::open(file)?) })
    }

    /// The frames of this app, opened the first time one is asked for. `None` if the database cannot be opened.
    pub fn shared() -> Option<&'static Self> {
        static SHARED: OnceLock<Option<ThumbnailFrames>> = OnceLock::new();
        SHARED
            .get_or_init(|| match Self::open(&MediaIndex::file()) {
                Ok(frames) => Some(frames),
                Err(err) => {
                    eprintln!("rsimgv: the chosen thumbnail frames are unavailable: {err:#}");
                    None
                }
            })
            .as_ref()
    }

    fn lock(&self) -> MutexGuard<'_, MediaIndex> {
        self.index.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The frame chosen for the video at `path`, if one is.
    pub fn get(&self, path: &Path) -> rusqlite::Result<Option<i64>> {
        self.lock().thumbnail_frame(path)
    }

    /// Chooses `frame` as the video at `path`'s thumbnail, or takes the choice back for `None`.
    pub fn set(&self, path: &Path, frame: Option<i64>) -> rusqlite::Result<()> {
        self.lock().set_thumbnail_frame(path, frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_frame_round_trips_and_can_be_taken_back() {
        let dir = std::env::temp_dir().join(format!("rsimgv-thumb-frame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let frames = ThumbnailFrames::open(&dir.join("media-index.sqlite3")).unwrap();
        let path = dir.join("clip.mp4");
        assert_eq!(frames.get(&path).unwrap(), None);
        frames.set(&path, Some(1234)).unwrap();
        assert_eq!(frames.get(&path).unwrap(), Some(1234));
        frames.set(&path, Some(99)).unwrap();
        assert_eq!(frames.get(&path).unwrap(), Some(99));
        frames.set(&path, None).unwrap();
        assert_eq!(frames.get(&path).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
