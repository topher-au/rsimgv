//! The playlist: pictures and videos in the order the user chooses, wherever they are, apart from the folder being
//! browsed. There is one, which the windows share, kept in `playlist.m3u8` beside the settings file. Pure: no GTK.

pub mod format;

use std::cell::RefCell;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use format::Format;

use crate::settings::config_path;

/// The line in the playlist's own file saying which item it was left on. Other players ignore it, as a comment.
const CURRENT_TAG: &str = "#X-RSIMGV-CURRENT:";

/// Each path appears once, so an item is known by its path, and adding one twice changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Playlist {
    items: Vec<PathBuf>,
    members: HashSet<PathBuf>,
    /// The item last shown, which playing starts from.
    current: Option<usize>,
}

impl Playlist {
    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.members.contains(path)
    }

    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Adds the paths not already there to the end. Returns how many were added.
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        let before = self.items.len();
        for path in paths {
            if self.members.insert(path.clone()) {
                self.items.push(path);
            }
        }
        self.items.len() - before
    }

    /// Makes the playlist `paths`, starting again from the first.
    pub fn replace(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.clear();
        self.add(paths);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Removes `paths`, and everything under those that are folders. Returns how many items went.
    pub fn remove_under(&mut self, paths: &[PathBuf]) -> usize {
        let paths: HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        self.retain(|item| !item.ancestors().any(|ancestor| paths.contains(ancestor)))
    }

    /// Removes the item at `index`, if there is one. Returns whether there was.
    pub fn remove_at(&mut self, index: usize) -> bool {
        let Some(path) = self.items.get(index).cloned() else { return false };
        self.retain(|item| item != path) == 1
    }

    /// Keeps the items `keep` says to, and returns how many went. If the current item goes, the one after it takes its
    /// place, or at the end the one before.
    fn retain(&mut self, keep: impl Fn(&Path) -> bool) -> usize {
        let current = self.current;
        let items = std::mem::take(&mut self.items);
        let total = items.len();
        let mut gone_before_current = 0;
        for (index, item) in items.into_iter().enumerate() {
            if keep(&item) {
                self.items.push(item);
            } else {
                self.members.remove(&item);
                if current.is_some_and(|current| index < current) {
                    gone_before_current += 1;
                }
            }
        }
        let last = self.items.len().checked_sub(1);
        self.current = current.zip(last).map(|(current, last)| (current - gone_before_current).min(last));
        total - self.items.len()
    }

    /// Makes the item at `index` the current one, if there is one.
    pub fn set_current(&mut self, index: usize) {
        if index < self.items.len() {
            self.current = Some(index);
        }
    }

    /// Makes the item `path` the current one, if it is there. Returns whether that changed anything.
    pub fn set_current_path(&mut self, path: &Path) -> bool {
        let index = self.members.contains(path).then(|| self.items.iter().position(|item| item == path)).flatten();
        let changed = index.is_some() && index != self.current;
        if changed {
            self.current = index;
        }
        changed
    }

    /// Moves the item at `from` to the place of the one at `to`. Returns whether it moved.
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.items.len() || to >= self.items.len() {
            return false;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.current = self.current.map(|current| match current {
            current if current == from => to,
            current if from < current && current <= to => current - 1,
            current if to <= current && current < from => current + 1,
            current => current,
        });
        true
    }

    /// The item to go to from the current one, going round from one end to the other. With no current item, the first
    /// going forward and the last going back.
    pub fn step_index(&self, forward: bool) -> Option<usize> {
        let last = self.items.len().checked_sub(1)?;
        Some(match (self.current, forward) {
            (Some(current), true) if current < last => current + 1,
            (Some(current), false) if current > 0 => current - 1,
            (Some(_), true) | (None, true) => 0,
            (Some(_), false) | (None, false) => last,
        })
    }

    /// The item to start playing from: the one it was left on, or the first.
    pub fn resume_index(&self) -> Option<usize> {
        (!self.is_empty()).then(|| self.current.unwrap_or(0))
    }

    /// The text of the playlist's own file: an M3U file, with the current item noted in it.
    fn to_saved_text(&self) -> String {
        let text = format::write(Format::M3u, &self.items);
        match self.current {
            Some(current) => text.replacen('\n', &format!("\n{CURRENT_TAG}{current}\n"), 1),
            None => text,
        }
    }

    fn from_saved_bytes(bytes: &[u8], base: &Path) -> Self {
        let mut playlist = Self::default();
        playlist.add(format::read(bytes, base).paths);
        let current = format::decode(bytes).lines().find_map(|line| line.strip_prefix(CURRENT_TAG)?.trim().parse::<usize>().ok());
        if let Some(current) = current {
            playlist.set_current(current);
        }
        playlist
    }
}

thread_local! {
    static PLAYLIST: RefCell<Playlist> = RefCell::new(Playlist::default());
}

/// Runs `f` on the live playlist. Do not call `with` again inside `f`.
pub fn with<R>(f: impl FnOnce(&mut Playlist) -> R) -> R {
    PLAYLIST.with(|playlist| f(&mut playlist.borrow_mut()))
}

/// Whether `path` is in the live playlist.
pub fn contains(path: &Path) -> bool {
    with(|playlist| playlist.contains(path))
}

/// The playlist's own file: `playlist.m3u8` beside the settings file.
pub fn file() -> PathBuf {
    config_path().with_file_name("playlist.m3u8")
}

/// Loads the playlist's own file, if there is one.
pub fn load() {
    let file = file();
    let Ok(bytes) = std::fs::read(&file) else { return };
    let loaded = Playlist::from_saved_bytes(&bytes, file.parent().unwrap_or(Path::new("/")));
    with(|playlist| *playlist = loaded);
}

/// Writes the playlist to its own file.
pub fn save() -> io::Result<()> {
    let text = with(|playlist| playlist.to_saved_text());
    let file = file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::atomic_file::write(&file, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|name| PathBuf::from(format!("/{name}"))).collect()
    }

    fn playlist(names: &[&str]) -> Playlist {
        let mut playlist = Playlist::default();
        playlist.add(paths(names));
        playlist
    }

    #[test]
    fn adding_keeps_the_order_and_each_item_once() {
        let mut list = playlist(&["b", "a"]);
        assert_eq!(list.add(paths(&["c", "a", "c", "d"])), 2);
        assert_eq!(list.items(), paths(&["b", "a", "c", "d"]));
        assert!(list.contains(Path::new("/c")) && !list.contains(Path::new("/z")));
    }

    #[test]
    fn removing_a_folder_removes_what_is_under_it_and_no_more() {
        let mut list = Playlist::default();
        list.add(["/a/one.png", "/a/sub/two.png", "/ab/three.png", "/four.png"].map(PathBuf::from));
        assert_eq!(list.remove_under(&[PathBuf::from("/a")]), 2);
        assert_eq!(list.items(), ["/ab/three.png", "/four.png"].map(PathBuf::from));
        assert!(!list.contains(Path::new("/a/one.png")), "removed items are forgotten");
        assert_eq!(list.add([PathBuf::from("/a/one.png")]), 1, "so they can come back");
    }

    #[test]
    fn the_current_item_survives_removals_around_it() {
        let mut list = playlist(&["a", "b", "c", "d"]);
        list.set_current(2);
        list.remove_under(&paths(&["a"]));
        assert_eq!(list.current(), Some(1), "still c");
        list.remove_under(&paths(&["d"]));
        assert_eq!(list.current(), Some(1), "still c");
    }

    #[test]
    fn when_the_current_item_goes_the_next_takes_its_place() {
        let mut list = playlist(&["a", "b", "c"]);
        list.set_current(1);
        assert!(list.remove_at(1));
        assert_eq!(list.current(), Some(1), "c");
        assert!(list.remove_at(1));
        assert_eq!(list.current(), Some(0), "at the end, the one before");
        assert!(list.remove_at(0));
        assert_eq!(list.current(), None, "and nothing once it is empty");
        assert!(!list.remove_at(0));
    }

    #[test]
    fn the_current_item_follows_a_move() {
        let moved = |from, to, current| {
            let mut list = playlist(&["a", "b", "c", "d", "e"]);
            list.set_current(current);
            let shown = list.items()[current].clone();
            assert!(list.move_item(from, to));
            assert_eq!(list.items()[list.current().unwrap()], shown, "from {from} to {to} with {current} current");
        };
        for (from, to) in [(0, 4), (4, 0), (1, 3), (3, 1), (2, 3)] {
            for current in 0..5 {
                moved(from, to, current);
            }
        }
        let mut list = playlist(&["a", "b", "c"]);
        assert!(list.move_item(0, 2));
        assert_eq!(list.items(), paths(&["b", "c", "a"]));
        assert!(!list.move_item(1, 1) && !list.move_item(0, 3) && !list.move_item(3, 0));
    }

    #[test]
    fn stepping_goes_round_the_ends() {
        let mut list = playlist(&["a", "b", "c"]);
        assert_eq!((list.step_index(true), list.step_index(false)), (Some(0), Some(2)), "from nothing, the ends");
        list.set_current(0);
        assert_eq!((list.step_index(true), list.step_index(false)), (Some(1), Some(2)));
        list.set_current(2);
        assert_eq!((list.step_index(true), list.step_index(false)), (Some(0), Some(1)));
        assert_eq!(Playlist::default().step_index(true), None);
    }

    #[test]
    fn playing_resumes_where_it_was_left() {
        let mut list = playlist(&["a", "b"]);
        assert_eq!(list.resume_index(), Some(0));
        assert!(list.set_current_path(Path::new("/b")));
        assert!(!list.set_current_path(Path::new("/b")), "nothing changed");
        assert!(!list.set_current_path(Path::new("/z")), "not in the playlist");
        assert_eq!(list.resume_index(), Some(1));
        assert_eq!(Playlist::default().resume_index(), None);
    }

    #[test]
    fn replacing_starts_again() {
        let mut list = playlist(&["a", "b"]);
        list.set_current(1);
        list.replace(paths(&["c", "d", "c"]));
        assert_eq!(list.items(), paths(&["c", "d"]));
        assert_eq!(list.current(), None);
        assert!(!list.contains(Path::new("/a")));
    }

    #[test]
    fn the_saved_file_keeps_the_items_and_the_current_one() {
        let mut list = playlist(&["a b.png", "c.mp4", "d.png"]);
        list.set_current(1);
        let text = list.to_saved_text();
        assert!(text.starts_with("#EXTM3U\n#X-RSIMGV-CURRENT:1\n"), "{text}");
        assert_eq!(Playlist::from_saved_bytes(text.as_bytes(), Path::new("/")), list);

        let unstarted = playlist(&["a"]);
        assert_eq!(Playlist::from_saved_bytes(unstarted.to_saved_text().as_bytes(), Path::new("/")), unstarted);
        assert_eq!(Playlist::from_saved_bytes(Playlist::default().to_saved_text().as_bytes(), Path::new("/")), Playlist::default());
        assert_eq!(Playlist::from_saved_bytes(b"#X-RSIMGV-CURRENT:9\n/a\n", Path::new("/")).current(), None, "out of range");
    }
}
