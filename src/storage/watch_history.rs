//! The viewing history: how many times each picture or video has been viewed or played, and how long it has been
//! watched. It is kept in the metadata database, so one SQLite file holds both, and every window shares it.
//!
//! A file is known by its content hash, as its settings are (see `file_settings`), so its history follows it when it is
//! moved or renamed. The last path it was viewed at is kept for the history list; it is updated on each view, and is
//! indexed so the browse view can look a file up without reading it.
//!
//! The time kept is the time the file was actually watched, measured by the application: a video played at twice the
//! normal speed adds half the time a normal-speed play of the same length would, and one played at half speed adds twice
//! as much. It is not the video's playback position.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub use super::media_index::WatchEntry;

use super::media_index::MediaIndex;

/// The viewing history of every file, in the metadata database.
pub struct WatchHistory {
    index: Mutex<MediaIndex>,
}

impl WatchHistory {
    /// Opens the history in `file`, creating it if it does not exist. Several windows, or processes, may share it.
    pub fn open(file: &Path) -> anyhow::Result<Self> {
        Ok(Self { index: Mutex::new(MediaIndex::open(file)?) })
    }

    /// The history of this app, opened the first time it is asked for. `None` if the database cannot be opened, which
    /// is not worth failing the application over.
    pub fn shared() -> Option<&'static Self> {
        static SHARED: OnceLock<Option<WatchHistory>> = OnceLock::new();
        SHARED
            .get_or_init(|| match Self::open(&MediaIndex::file()) {
                Ok(history) => Some(history),
                Err(err) => {
                    eprintln!("rsimgv: viewing history is off: {err:#}");
                    None
                }
            })
            .as_ref()
    }

    fn lock(&self) -> MutexGuard<'_, MediaIndex> {
        self.index.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records that the file with content hash `hash` was watched at `path`: one more view if `viewed`, and `seconds`
    /// more time. A record is kept even for a file that was not watched long enough to count as a view, as long as some
    /// time was spent on it.
    pub fn record(&self, hash: &str, path: &Path, viewed: bool, seconds: f64) -> rusqlite::Result<()> {
        self.lock().record_watch(hash, path, viewed, seconds)
    }

    /// The history of the file with content hash `hash`, if it has been watched.
    pub fn entry(&self, hash: &str) -> rusqlite::Result<Option<WatchEntry>> {
        self.lock().watch_entry(hash)
    }

    /// The views and watched time of the file last seen at `path`, added up over every record that names it. This is
    /// for looking a file up in the browse view, where reading the file to find its content hash would be too slow.
    pub fn totals_at(&self, path: &Path) -> rusqlite::Result<Option<(u64, f64)>> {
        self.lock().watch_totals_at(path)
    }

    /// The most recently watched files, newest first, at most `limit`.
    pub fn recent(&self, limit: usize) -> rusqlite::Result<Vec<WatchEntry>> {
        self.lock().recent_watch(limit)
    }

    /// Removes the whole history.
    pub fn clear(&self) -> rusqlite::Result<()> {
        self.lock().clear_watch()
    }
}

/// A span of watched time, in a compact form: `45s`, `3m 20s`, `2h 05m`, `3d 4h`.
pub fn format_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        let rest = seconds % 60;
        return if rest == 0 { format!("{minutes}m") } else { format!("{minutes}m {rest}s") };
    }
    let hours = minutes / 60;
    if hours < 24 {
        let rest = minutes % 60;
        return if rest == 0 { format!("{hours}h") } else { format!("{hours}h {rest:02}m") };
    }
    let days = hours / 24;
    let rest = hours % 24;
    if rest == 0 { format!("{days}d") } else { format!("{days}d {rest}h") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn history() -> (WatchHistory, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rsimgv-history-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("media-index.sqlite3");
        (WatchHistory::open(&file).unwrap(), dir.join("clip.mp4"), file)
    }

    #[test]
    fn views_and_time_add_up() {
        let (history, path, _) = history();
        history.record("abc", &path, true, 30.0).unwrap();
        history.record("abc", &path, true, 12.5).unwrap();
        history.record("abc", &path, false, 7.5).unwrap();
        let entry = history.entry("abc").unwrap().unwrap();
        assert_eq!(entry.views, 2, "a play that did not count is not a view");
        assert!((entry.watched_seconds - 50.0).abs() < 1e-9, "all time spent watching counts");
        assert_eq!(entry.path, path);
        assert!(entry.last_viewed >= entry.first_viewed);
    }

    #[test]
    fn a_flash_of_a_file_is_not_recorded() {
        let (history, path, _) = history();
        history.record("abc", &path, false, 0.1).unwrap();
        assert_eq!(history.entry("abc").unwrap(), None);
    }

    #[test]
    fn totals_are_found_by_path_for_the_browse_view() {
        let (history, path, _) = history();
        history.record("one", &path, true, 10.0).unwrap();
        history.record("two", &path, false, 5.0).unwrap();
        assert_eq!(history.totals_at(&path).unwrap(), Some((1, 15.0)));
        assert_eq!(history.totals_at(Path::new("/elsewhere/clip.mp4")).unwrap(), None);
    }

    #[test]
    fn recent_files_are_newest_first() {
        let (history, path, file) = history();
        history.record("old", &path, true, 10.0).unwrap();
        history.record("new", &path, true, 20.0).unwrap();
        // Make the order distinct even when both writes fall in the same second.
        let connection = rusqlite::Connection::open(file).unwrap();
        connection.execute("UPDATE watch SET last_viewed = 100 WHERE hash = 'old'", []).unwrap();
        connection.execute("UPDATE watch SET last_viewed = 200 WHERE hash = 'new'", []).unwrap();
        let recent = history.recent(10).unwrap();
        assert_eq!(recent.iter().map(|entry| entry.hash.as_str()).collect::<Vec<_>>(), ["new", "old"]);
        assert_eq!(recent[0].watched_seconds, 20.0);
    }

    #[test]
    fn clearing_empties_the_history() {
        let (history, path, _) = history();
        history.record("abc", &path, true, 10.0).unwrap();
        history.clear().unwrap();
        assert!(history.recent(10).unwrap().is_empty());
        assert_eq!(history.entry("abc").unwrap(), None);
    }

    #[test]
    fn durations_are_compact_and_readable() {
        assert_eq!(format_duration(0.0), "0s");
        assert_eq!(format_duration(45.4), "45s");
        assert_eq!(format_duration(60.0), "1m");
        assert_eq!(format_duration(200.0), "3m 20s");
        assert_eq!(format_duration(3600.0), "1h");
        assert_eq!(format_duration(7500.0), "2h 05m");
        assert_eq!(format_duration(86_400.0 + 4.0 * 3600.0), "1d 4h");
    }
}
