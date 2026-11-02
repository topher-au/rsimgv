//! The playlist: pictures and videos in the order the user chooses, wherever they are, apart from the folder being
//! browsed. There is one, which the windows share, kept in `playlist.m3u8` beside the settings file. Pure: no GTK.

pub mod format;

use std::cell::RefCell;
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use format::Format;

use crate::settings::config_path;

/// The lines in the playlist's own file that say which item it was left on, and the shuffled order, as indices into the
/// original order. Other players ignore them, as comments.
const CURRENT_TAG: &str = "#X-RSIMGV-CURRENT:";
const SHUFFLE_TAG: &str = "#X-RSIMGV-SHUFFLE:";

/// Each path appears once, so an item is known by its path, and adding one twice changes nothing.
///
/// The items are kept in the order the user made them, which is what is saved. Shuffling makes a second order, in
/// `shuffled`, that is what the list shows and plays in while it is on. Changing the order in one leaves the other as
/// it was, and removing an item removes it from both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Playlist {
    items: Vec<PathBuf>,
    shuffled: Option<Vec<PathBuf>>,
    members: HashSet<PathBuf>,
    /// The index, in `order`, of the item last shown, which playing starts from.
    current: Option<usize>,
    /// The titles given to items, where they differ from the item's file name. An item without one is shown by that.
    titles: HashMap<PathBuf, String>,
}

impl Playlist {
    /// The title the item at `path` is shown by: the one it was given, else its file name.
    pub fn title(&self, path: &Path) -> String {
        self.titles.get(path).cloned().unwrap_or_else(|| format::default_title(path))
    }

    /// Gives the item at `path` the title `title`, or, if `title` is empty or is its file name, takes the title back.
    pub fn set_title(&mut self, path: &Path, title: &str) {
        let title = title.trim();
        if title.is_empty() || title == format::default_title(path) {
            self.titles.remove(path);
        } else {
            self.titles.insert(path.to_path_buf(), title.to_owned());
        }
    }

    /// Adds the entries not already there, each with the title it was given if it was given one, as `add` does.
    pub fn add_with_titles(&mut self, entries: impl IntoIterator<Item = (PathBuf, Option<String>)>) -> usize {
        let mut titled = Vec::new();
        let added = self.add(entries.into_iter().map(|(path, title)| {
            titled.push((path.clone(), title));
            path
        }));
        for (path, title) in titled {
            if let Some(title) = title {
                self.set_title(&path, &title);
            }
        }
        added
    }

    /// Puts the items the shown order holds in the order `key` gives, lowest first, or highest first if `descending`.
    /// The sort is stable, so items with equal keys keep the order they were in. It changes the order shown, as moving
    /// an item does: the made order is kept while shuffling, and changed when it is not.
    pub fn sort_shown_by<K: Ord>(&mut self, key: impl Fn(&Path) -> K, descending: bool) {
        let current = self.current_path().map(Path::to_path_buf);
        let order = self.order_mut();
        if descending {
            order.sort_by_cached_key(|path| std::cmp::Reverse(key(path.as_path())));
        } else {
            order.sort_by_cached_key(|path| key(path.as_path()));
        }
        self.current = current.and_then(|path| self.order().iter().position(|item| *item == path));
    }

    /// The items in the order they were made, whether or not they are shuffled.
    #[cfg(test)]
    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    /// The items in the order they are shown and played in: the shuffled order while shuffling, else `items`.
    pub fn order(&self) -> &[PathBuf] {
        self.shuffled.as_deref().unwrap_or(self.items.as_slice())
    }

    fn order_mut(&mut self) -> &mut Vec<PathBuf> {
        self.shuffled.as_mut().unwrap_or(&mut self.items)
    }

    pub fn is_shuffled(&self) -> bool {
        self.shuffled.is_some()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The item last shown, if there is one.
    pub fn current_path(&self) -> Option<&Path> {
        self.current.and_then(|current| self.order().get(current)).map(PathBuf::as_path)
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.members.contains(path)
    }

    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Adds the paths not already there to the end of each order. Returns how many were added.
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        let before = self.items.len();
        for path in paths {
            if self.members.insert(path.clone()) {
                if let Some(shuffled) = self.shuffled.as_mut() {
                    shuffled.push(path.clone());
                }
                self.items.push(path);
            }
        }
        self.items.len() - before
    }

    /// Shuffles the order the list plays in, or goes back to the order the items were made in. Shuffling puts the item
    /// shown first, so it keeps playing.
    pub fn set_shuffled(&mut self, on: bool) {
        self.set_shuffled_with(on, random_source());
    }

    /// `set_shuffled`, with the random numbers from `next`, which gives a number below the bound it is passed.
    fn set_shuffled_with(&mut self, on: bool, next: impl FnMut(usize) -> usize) {
        let current = self.current_path().map(Path::to_path_buf);
        self.shuffled = on.then(|| {
            let shown = current.as_ref().and_then(|path| self.items.iter().position(|item| item == path));
            shuffle_order(&self.items, shown, next)
        });
        self.current = current.and_then(|path| self.order().iter().position(|item| *item == path));
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

    /// Removes the item at `index` in `order`, if there is one. Returns whether there was.
    pub fn remove_at(&mut self, index: usize) -> bool {
        let Some(path) = self.order().get(index).cloned() else { return false };
        self.retain(|item| item != path) == 1
    }

    /// Keeps the items `keep` says to, in both orders, and returns how many went. If the current item goes, the one
    /// after it takes its place, or at the end the one before.
    fn retain(&mut self, keep: impl Fn(&Path) -> bool) -> usize {
        let total = self.len();
        let gone_before_current = self.current.map_or(0, |current| self.order().iter().take(current).filter(|item| !keep(item)).count());
        self.items.retain(|item| keep(item));
        if let Some(shuffled) = self.shuffled.as_mut() {
            shuffled.retain(|item| keep(item));
        }
        self.members = self.items.iter().cloned().collect();
        let last = self.len().checked_sub(1);
        self.current = self.current.zip(last).map(|(current, last)| (current - gone_before_current).min(last));
        total - self.len()
    }

    /// Makes the item at `index` the current one, if there is one.
    pub fn set_current(&mut self, index: usize) {
        if index < self.len() {
            self.current = Some(index);
        }
    }

    /// Makes the item `path` the current one, if it is there. Returns whether that changed anything.
    pub fn set_current_path(&mut self, path: &Path) -> bool {
        let index = self.members.contains(path).then(|| self.order().iter().position(|item| item == path)).flatten();
        let changed = index.is_some() && index != self.current;
        if changed {
            self.current = index;
        }
        changed
    }

    /// Moves the item at `from` to the place of the one at `to`, in the order shown. The other order is left as it
    /// was. Returns whether it moved.
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.len() || to >= self.len() {
            return false;
        }
        let order = self.order_mut();
        let item = order.remove(from);
        order.insert(to, item);
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
        let last = self.len().checked_sub(1)?;
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

    /// The text of the playlist's own file: an M3U file of the items in the order they were made, with the item shown,
    /// and the shuffled order if there is one, noted in it.
    fn to_saved_text(&self) -> String {
        let titled: Vec<(PathBuf, String)> = self.items.iter().map(|item| (item.clone(), self.title(item))).collect();
        let text = format::write(Format::M3u, &titled);
        let positions: HashMap<&Path, usize> = self.items.iter().enumerate().map(|(index, item)| (item.as_path(), index)).collect();
        let mut notes = String::new();
        if let Some(current) = self.current_path().and_then(|path| positions.get(path)) {
            notes.push_str(&format!("{CURRENT_TAG}{current}\n"));
        }
        if let Some(shuffled) = &self.shuffled {
            let indices: Vec<String> = shuffled.iter().filter_map(|item| positions.get(item.as_path())).map(usize::to_string).collect();
            notes.push_str(&format!("{SHUFFLE_TAG}{}\n", indices.join(",")));
        }
        text.replacen('\n', &format!("\n{notes}"), 1)
    }

    /// The playlist a saved file holds. A shuffled order that does not hold each item exactly once is not used.
    fn from_saved_bytes(bytes: &[u8], base: &Path) -> Self {
        let text = format::decode(bytes);
        let read = format::read(bytes, base);
        let saved = read.paths.clone();
        let mut playlist = Self::default();
        playlist.add_with_titles(read.paths.into_iter().zip(read.titles));
        let note = |tag: &str| text.lines().find_map(|line| line.strip_prefix(tag)).map(str::trim);
        if let Some(indices) = note(SHUFFLE_TAG) {
            let mut seen = HashSet::new();
            let shuffled: Vec<PathBuf> = indices
                .split(',')
                .filter_map(|index| index.trim().parse::<usize>().ok())
                .filter_map(|index| saved.get(index).cloned())
                .filter(|item| seen.insert(item.clone()))
                .collect();
            if shuffled.len() == playlist.len() {
                playlist.shuffled = Some(shuffled);
            }
        }
        let current = note(CURRENT_TAG).and_then(|index| index.parse::<usize>().ok()).and_then(|index| saved.get(index));
        if let Some(current) = current.cloned() {
            playlist.set_current_path(&current);
        }
        playlist
    }
}

/// The order `items` is shuffled into, with `shown` (an index into `items`) first if there is one. `next(bound)` gives a
    /// number from 0 to `bound`, exclusive, and the rest are shuffled with it (Fisher–Yates).
    fn shuffle_order(items: &[PathBuf], shown: Option<usize>, mut next: impl FnMut(usize) -> usize) -> Vec<PathBuf> {
        let mut order = items.to_vec();
        let mut rest = 0;
        if let Some(shown) = shown.filter(|shown| *shown < order.len()) {
            order.swap(0, shown);
            rest = 1;
        }
        let tail = &mut order[rest..];
        for index in (1..tail.len()).rev() {
            let other = next(index + 1);
            tail.swap(index, other);
        }
        order
    }

    /// A source of numbers for shuffling. It is seeded from a hasher's random keys, which is enough to vary the order;
    /// it is not for anything that must be unpredictable.
    fn random_source() -> impl FnMut(usize) -> usize {
        let mut state = RandomState::new().build_hasher().finish();
        move |bound| {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            (z % bound as u64) as usize
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
            let shown = list.order()[current].clone();
            assert!(list.move_item(from, to));
            assert_eq!(list.order()[list.current().unwrap()], shown, "from {from} to {to} with {current} current");
        };
        for (from, to) in [(0, 4), (4, 0), (1, 3), (3, 1), (2, 3)] {
            for current in 0..5 {
                moved(from, to, current);
            }
        }
        let mut list = playlist(&["a", "b", "c"]);
        assert!(list.move_item(0, 2));
        assert_eq!(list.order(), paths(&["b", "c", "a"]));
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

    /// A deterministic source of numbers below each bound it is asked for.
    fn counting() -> impl FnMut(usize) -> usize {
        let mut calls = 0;
        move |bound| {
            calls += 1;
            (calls * 7) % bound
        }
    }

    #[test]
    fn shuffling_keeps_the_items_in_their_own_order() {
        let mut list = playlist(&["a", "b", "c", "d", "e"]);
        list.set_current(3);
        list.set_shuffled_with(true, counting());
        assert!(list.is_shuffled());
        assert_eq!(list.items(), paths(&["a", "b", "c", "d", "e"]), "the made order is untouched");
        let mut shown = list.order().to_vec();
        assert_eq!(shown[0], PathBuf::from("/d"), "the item shown comes first");
        shown.sort();
        assert_eq!(shown, paths(&["a", "b", "c", "d", "e"]), "every item is there once");
        assert_eq!(list.current(), Some(0), "and it is still the one shown");
    }

    #[test]
    fn turning_shuffle_off_brings_back_the_made_order() {
        let mut list = playlist(&["a", "b", "c"]);
        list.set_shuffled_with(true, counting());
        list.set_shuffled(false);
        assert!(!list.is_shuffled());
        assert_eq!(list.order(), paths(&["a", "b", "c"]));
    }

    #[test]
    fn a_change_to_one_order_leaves_the_other_alone() {
        let mut list = playlist(&["a", "b", "c"]);
        list.set_shuffled_with(true, counting());
        let shuffled = list.order().to_vec();
        assert!(list.move_item(0, 2));
        assert_ne!(list.order(), shuffled.as_slice());
        assert_eq!(list.items(), paths(&["a", "b", "c"]), "moving in the shuffled order does not move the made order");
        list.set_shuffled(false);
        assert_eq!(list.order(), paths(&["a", "b", "c"]));
    }

    #[test]
    fn adding_and_removing_change_both_orders() {
        let mut list = playlist(&["a", "b", "c"]);
        list.set_shuffled_with(true, counting());
        assert_eq!(list.add(paths(&["d"])), 1);
        assert_eq!(list.order().last(), Some(&PathBuf::from("/d")));
        let removed = list.order()[1].clone();
        assert!(list.remove_at(1));
        assert_eq!(list.len(), 3);
        list.set_shuffled(false);
        assert!(!list.contains(&removed), "gone from the made order too");
        assert_eq!(list.order(), paths(&["a", "b", "c", "d"]).into_iter().filter(|item| *item != removed).collect::<Vec<_>>());
    }

    #[test]
    fn stepping_follows_the_shuffled_order() {
        let mut list = playlist(&["a", "b", "c"]);
        list.set_shuffled_with(true, counting());
        let order = list.order().to_vec();
        list.set_current(0);
        assert_eq!(list.step_index(true), Some(1));
        assert_eq!(list.order()[1], order[1]);
        assert_eq!(list.current_path(), Some(order[0].as_path()));
    }

    #[test]
    fn the_saved_file_keeps_both_orders_and_the_item_shown() {
        let mut list = playlist(&["a", "b", "c", "d"]);
        list.set_shuffled_with(true, counting());
        list.set_current_path(Path::new("/c"));
        let text = list.to_saved_text();
        assert!(text.contains("#X-RSIMGV-SHUFFLE:"), "{text}");
        let loaded = Playlist::from_saved_bytes(text.as_bytes(), Path::new("/"));
        assert_eq!(loaded.items(), list.items());
        assert_eq!(loaded.order(), list.order());
        assert_eq!(loaded.current_path(), Some(Path::new("/c")));
    }

    #[test]
    fn a_shuffled_order_that_does_not_fit_the_items_is_dropped() {
        let text = "#EXTM3U\n#X-RSIMGV-SHUFFLE:0,0,1\n/a\n/b\n/c\n";
        let loaded = Playlist::from_saved_bytes(text.as_bytes(), Path::new("/"));
        assert!(!loaded.is_shuffled());
        assert_eq!(loaded.order(), paths(&["a", "b", "c"]));
    }

    #[test]
    fn shuffle_order_is_a_permutation_that_starts_with_the_shown_item() {
        let items = paths(&["a", "b", "c", "d", "e", "f"]);
        let mut random = random_source();
        for shown in [None, Some(0), Some(5)] {
            let order = shuffle_order(&items, shown, &mut random);
            let mut sorted = order.clone();
            sorted.sort();
            assert_eq!(sorted, items);
            if let Some(shown) = shown {
                assert_eq!(order[0], items[shown]);
            }
        }
    }

    #[test]
    fn a_title_is_kept_until_it_is_cleared() {
        let mut list = Playlist::default();
        list.add(paths(&["a"]));
        assert_eq!(list.title(Path::new("/a")), "a", "the file name until one is given");
        list.set_title(Path::new("/a"), "  Sunset  ");
        assert_eq!(list.title(Path::new("/a")), "Sunset");
        list.set_title(Path::new("/a"), "   ");
        assert_eq!(list.title(Path::new("/a")), "a", "an empty title takes the file name back");
    }

    #[test]
    fn sorting_changes_the_order_shown_both_ways() {
        let mut list = Playlist::default();
        list.add(paths(&["c", "a", "b"]));
        list.sort_shown_by(|path| path.to_path_buf(), false);
        assert_eq!(list.order(), paths(&["a", "b", "c"]));
        list.sort_shown_by(|path| path.to_path_buf(), true);
        assert_eq!(list.order(), paths(&["c", "b", "a"]));
    }

    #[test]
    fn sorting_while_shuffled_leaves_the_made_order() {
        let mut list = Playlist::default();
        list.add(paths(&["c", "a", "b"]));
        list.set_shuffled(true);
        let made = list.items().to_vec();
        list.sort_shown_by(|path| path.to_path_buf(), false);
        assert_eq!(list.order(), paths(&["a", "b", "c"]));
        assert_eq!(list.items(), made.as_slice());
    }

    #[test]
    fn titles_are_saved_with_the_playlist() {
        let mut list = Playlist::default();
        list.add(paths(&["a", "b"]));
        list.set_title(Path::new("/b"), "Second");
        let loaded = Playlist::from_saved_bytes(list.to_saved_text().as_bytes(), Path::new("/"));
        assert_eq!(loaded.title(Path::new("/b")), "Second");
        assert_eq!(loaded.title(Path::new("/a")), "a");
    }
}
