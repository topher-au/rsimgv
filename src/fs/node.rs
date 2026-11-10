//! A directory entry in `FsTree`. Children are read on demand, through the shared `FileStore`. A node that is
//! watched is also kept up to date as its folder changes, as Nautilus does for the folders expanded in its list view.

use std::cell::{Cell, OnceCell, RefCell};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures_channel::oneshot;
use gio::prelude::*;
use glib::subclass::prelude::*;

use super::{natural_cmp, FileEntry, FileStore, ListingChange, Watching};

/// How long a node stays followed after it stops being watched, so that one shown again soon is not read again.
const UNWATCH_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Default)]
enum LoadState {
    #[default]
    NotLoaded,
    Loading(Vec<oneshot::Sender<()>>),
    Loaded,
}

/// What a directory's entries contain. Each flag is only set once a subdirectory of that kind
/// has been seen; the check stops early, so a `false` flag may just mean "not seen yet".
#[derive(Clone, Copy, Default)]
struct Subdirs {
    plain: bool,
    hidden: bool,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FsNode {
        pub(super) path: OnceCell<PathBuf>,
        pub(super) is_dir: Cell<bool>,
        pub(super) children: OnceCell<gio::ListStore>,
        pub(super) state: RefCell<LoadState>,
        pub(super) subdirs: Cell<Option<Subdirs>>,
        pub(super) checking: Cell<bool>,
        /// Whether the node is meant to follow changes to its folder.
        pub(super) watched: Cell<bool>,
        /// The hold on the folder's listing, while it is followed.
        pub(super) watching: RefCell<Option<Watching>>,
        /// Lets go of the listing once the node has stopped being watched for `UNWATCH_DELAY`.
        pub(super) unwatch_timer: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FsNode {
        const NAME: &'static str = "RsimgvFsNode";
        type Type = super::FsNode;
    }

    impl ObjectImpl for FsNode {}
}

glib::wrapper! {
    pub struct FsNode(ObjectSubclass<imp::FsNode>);
}

impl FsNode {
    pub fn new(path: PathBuf, is_dir: bool) -> Self {
        let node: Self = glib::Object::new();
        let imp = node.imp();
        imp.path.set(path).expect("path is set once");
        imp.is_dir.set(is_dir);
        node
    }

    pub fn path(&self) -> &Path {
        self.imp().path.get().expect("path is set in new")
    }

    pub fn name(&self) -> String {
        let path = self.path();
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned()
    }

    /// Dot-prefixed entries are hidden; the tree filters them unless the toggle is on.
    pub fn is_hidden(&self) -> bool {
        self.name().starts_with('.')
    }

    /// Whether this directory has something to expand, or `None` until the check has run.
    /// Hidden subdirectories count only when `show_hidden` is set.
    pub fn expandable(&self, show_hidden: bool) -> Option<bool> {
        if !self.is_dir() {
            return Some(false);
        }
        self.imp()
            .subdirs
            .get()
            .map(|found| found.plain || (show_hidden && found.hidden))
    }

    /// Checks for subdirectories in the background, once. `on_done` runs when the result is known.
    pub fn start_check(&self, on_done: impl FnOnce() + 'static) {
        let imp = self.imp();
        if imp.checking.get() || imp.subdirs.get().is_some() {
            return;
        }
        imp.checking.set(true);
        let node = self.clone();
        glib::spawn_future_local(async move {
            let found = check_subdirs(node.path().to_path_buf()).await;
            node.imp().subdirs.set(Some(found));
            on_done();
        });
    }

    pub fn is_dir(&self) -> bool {
        self.imp().is_dir.get()
    }

    /// Model of this directory's entries. Empty until `loaded()` resolves.
    pub fn children(&self) -> gio::ListStore {
        self.imp()
            .children
            .get_or_init(|| gio::ListStore::new::<FsNode>())
            .clone()
    }

    pub fn child_with_path(&self, path: &Path) -> Option<FsNode> {
        let store = self.children();
        (0..store.n_items())
            .filter_map(|i| store.item(i)?.downcast::<FsNode>().ok())
            .find(|node| node.path() == path)
    }

    /// Starts loading children in the background, if not already started. A folder already loaded is listed again,
    /// since it may have changed: the tree is shown again each time the folder is revealed.
    pub fn start_loading(&self) {
        if matches!(*self.imp().state.borrow(), LoadState::Loaded) {
            *self.imp().state.borrow_mut() = LoadState::Loading(Vec::new());
            FileStore::shared().discard_idle(self.path());
            load(self.clone());
            return;
        }
        let (tx, _rx) = oneshot::channel();
        self.request_load(tx);
    }

    /// Resolves once `children()` is populated. Files resolve immediately.
    pub fn loaded(&self) -> impl Future<Output = ()> {
        let (tx, rx) = oneshot::channel();
        self.request_load(tx);
        async move {
            let _ = rx.await;
        }
    }

    fn request_load(&self, tx: oneshot::Sender<()>) {
        if !self.is_dir() {
            let _ = tx.send(());
            return;
        }

        let mut state = self.imp().state.borrow_mut();
        if let LoadState::Loaded = *state {
            let _ = tx.send(());
            return;
        }
        if let LoadState::Loading(waiters) = &mut *state {
            waiters.push(tx);
            return;
        }

        *state = LoadState::Loading(vec![tx]);
        drop(state);
        load(self.clone());
    }

    fn finish_loading(&self) {
        let previous = std::mem::replace(&mut *self.imp().state.borrow_mut(), LoadState::Loaded);
        if let LoadState::Loading(waiters) = previous {
            for tx in waiters {
                let _ = tx.send(());
            }
        }
        if self.imp().watched.get() {
            self.follow();
        }
    }

    /// Sets whether the node follows changes to its folder, so that its children stay as the folder is. Following
    /// ends `UNWATCH_DELAY` after the node stops being watched, unless it is watched again by then.
    pub fn set_watched(&self, watched: bool) {
        let imp = self.imp();
        if !self.is_dir() {
            return;
        }
        imp.watched.set(watched);
        if watched {
            if let Some(timer) = imp.unwatch_timer.take() {
                timer.remove();
            }
            if matches!(*imp.state.borrow(), LoadState::Loaded) {
                self.follow();
            }
        } else if imp.watching.borrow().is_some() && imp.unwatch_timer.borrow().is_none() {
            let node = self.downgrade();
            let timer = glib::timeout_add_local_once(UNWATCH_DELAY, move || {
                let Some(node) = node.upgrade() else { return };
                node.imp().unwatch_timer.take();
                if !node.imp().watched.get() {
                    node.imp().watching.take();
                }
            });
            imp.unwatch_timer.replace(Some(timer));
        }
    }

    /// Whether the node follows changes to its folder now.
    pub fn is_following(&self) -> bool {
        self.imp().watching.borrow().is_some()
    }

    /// Starts following the folder's listing, if it is not followed already.
    fn follow(&self) {
        let imp = self.imp();
        if imp.watching.borrow().is_some() {
            return;
        }
        let node = self.downgrade();
        let watching = FileStore::shared().watch(self.path(), move |change| {
            if let Some(node) = node.upgrade() {
                node.apply_change(change);
            }
        });
        imp.watching.replace(watching);
    }

    /// Brings the children up to date with a change to the folder's listing. Only the subfolders count.
    fn apply_change(&self, change: &ListingChange) {
        // A node still loading takes the listing as it is when its read ends.
        if !matches!(*self.imp().state.borrow(), LoadState::Loaded) {
            return;
        }
        let children = self.children();
        match change {
            ListingChange::Added(entries) | ListingChange::Changed(entries) => {
                for entry in entries {
                    self.put_child(&children, entry);
                }
            }
            ListingChange::Removed(paths) => {
                for path in paths {
                    remove_child(&children, path);
                }
            }
            ListingChange::Moved(moves) => {
                for (old, new) in moves {
                    remove_child(&children, old);
                    self.put_child(&children, new);
                }
            }
            ListingChange::Reload => {
                // Let go of the listing, which is current no longer, so that it is read again.
                self.imp().watching.take();
                self.start_loading();
                return;
            }
        }
        self.imp().subdirs.set(Some(subdirs_of(&children)));
    }

    /// Puts the folder `entry` among the children, in its place by name. An entry that is not a folder is not a child,
    /// and takes the place of one that was.
    fn put_child(&self, children: &gio::ListStore, entry: &FileEntry) {
        if !entry.is_dir() {
            remove_child(children, entry.path());
            return;
        }
        if self.child_with_path(entry.path()).is_some() {
            return;
        }
        let node = FsNode::new(entry.path().to_path_buf(), true);
        let name = node.name();
        let position = (0..children.n_items())
            .find(|&position| {
                children.item(position).and_downcast::<FsNode>().is_some_and(|other| natural_cmp(&other.name(), &name).is_gt())
            })
            .unwrap_or(children.n_items());
        children.insert(position, &node);
    }
}

/// Takes the child at `path` out of `children`, if there is one.
fn remove_child(children: &gio::ListStore, path: &Path) {
    let position = (0..children.n_items())
        .find(|&position| children.item(position).and_downcast::<FsNode>().is_some_and(|node| node.path() == path));
    if let Some(position) = position {
        children.remove(position);
    }
}

/// What the subfolders in `children` are, as far as expanding is concerned.
fn subdirs_of(children: &gio::ListStore) -> Subdirs {
    let mut found = Subdirs::default();
    for node in (0..children.n_items()).filter_map(|position| children.item(position).and_downcast::<FsNode>()) {
        if node.is_hidden() {
            found.hidden = true;
        } else {
            found.plain = true;
        }
    }
    found
}

/// Reads this directory through the shared store and fills in the subdirectories once the read ends.
fn load(node: FsNode) {
    let found = Rc::new(RefCell::new(Vec::new()));
    let on_batch = {
        let found = found.clone();
        move |batch: &[FileEntry]| found.borrow_mut().extend(batch.iter().filter(|entry| entry.is_dir()).cloned())
    };
    let on_done = {
        let node = node.clone();
        move |result: Result<(), String>| {
            if let Err(err) = result {
                eprintln!("rsimgv: failed to read {}: {err}", node.path().display());
            }
            // The tree shows directories only; files are dropped here.
            let mut entries = std::mem::take(&mut *found.borrow_mut());
            entries.sort_by(|a, b| natural_cmp(&a.name(), &b.name()));
            let listed: Vec<FsNode> = entries
                .iter()
                .map(|entry| node.child_with_path(entry.path()).unwrap_or_else(|| FsNode::new(entry.path().to_path_buf(), true)))
                .collect();
            sync_children(&node.children(), &listed);
            node.finish_loading();
        }
    };
    // The reading is not cancelled: the tree keeps the children it asked for.
    FileStore::shared().read(node.path(), on_batch, on_done);
}

/// Reads entries through the shared store until the first visible subdirectory turns up. Hidden
/// subdirectories are remembered but do not stop the read, so a directory with only hidden
/// subdirectories is read to the end.
async fn check_subdirs(dir: PathBuf) -> Subdirs {
    let found = Rc::new(Cell::new(Subdirs::default()));
    let (tx, rx) = oneshot::channel::<()>();
    let tx = Rc::new(RefCell::new(Some(tx)));
    let stop: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(());
        }
    });
    let reading = FileStore::shared().read(
        &dir,
        {
            let found = found.clone();
            let stop = stop.clone();
            move |batch: &[FileEntry]| {
                for entry in batch.iter().filter(|entry| entry.is_dir()) {
                    let mut subdirs = found.get();
                    if entry.name().starts_with('.') {
                        subdirs.hidden = true;
                    } else {
                        subdirs.plain = true;
                    }
                    found.set(subdirs);
                }
                if found.get().plain {
                    stop();
                }
            }
        },
        move |_| stop(),
    );
    let _ = rx.await;
    // Stop reading as soon as the answer is known, unless another reader still wants the listing.
    reading.cancel();
    found.get()
}

/// Makes `store` hold `listed`, in its order, by removing the nodes no longer listed and inserting the new ones. The
/// nodes that remain are the same objects, so their own state (what is expanded, what is loaded) is kept.
fn sync_children(store: &gio::ListStore, listed: &[FsNode]) {
    let mut position = store.n_items();
    while position > 0 {
        position -= 1;
        let Some(stored) = store.item(position).and_downcast::<FsNode>() else { continue };
        if !listed.iter().any(|node| *node == stored) {
            store.remove(position);
        }
    }
    for (position, node) in listed.iter().enumerate() {
        if store.item(position as u32).as_ref() != Some(node.upcast_ref::<glib::Object>()) {
            store.insert(position as u32, node);
        }
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    #[test]
    fn a_listing_keeps_the_nodes_that_remain_and_takes_the_new_ones_in_order() {
        let node = |name: &str| FsNode::new(PathBuf::from(format!("/root/{name}")), true);
        let (a, b, c) = (node("a"), node("b"), node("c"));
        let store = gio::ListStore::new::<FsNode>();
        store.splice(0, 0, &[a.clone(), b.clone(), c.clone()]);

        let d = node("d");
        sync_children(&store, &[a.clone(), d.clone(), c.clone()]);

        let names: Vec<String> = (0..store.n_items())
            .map(|position| store.item(position).and_downcast::<FsNode>().unwrap().path().display().to_string())
            .collect();
        assert_eq!(names, ["/root/a", "/root/d", "/root/c"]);
        assert!(store.item(0).as_ref() == Some(a.upcast_ref::<glib::Object>()), "a is the same node");
        assert!(store.item(2).as_ref() == Some(c.upcast_ref::<glib::Object>()), "c is the same node");
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;

    fn folder(name: &str, subfolders: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rsimgv-node-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for sub in subfolders {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        dir
    }

    fn run_until(ctx: &glib::MainContext, what: &str, done: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "{what} did not happen");
            if !ctx.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    fn child_names(node: &FsNode) -> Vec<String> {
        let children = node.children();
        (0..children.n_items()).filter_map(|i| children.item(i).and_downcast::<FsNode>()).map(|child| child.name()).collect()
    }

    /// A node loaded and watched.
    fn watched_node(ctx: &glib::MainContext, dir: &Path) -> FsNode {
        let node = FsNode::new(dir.to_path_buf(), true);
        let loaded = Rc::new(Cell::new(false));
        node.start_loading();
        glib::spawn_future_local({
            let (node, loaded) = (node.clone(), loaded.clone());
            async move {
                node.loaded().await;
                loaded.set(true);
            }
        });
        run_until(ctx, "the load", || loaded.get());
        node.set_watched(true);
        assert!(node.is_following());
        node
    }

    #[test]
    fn a_watched_node_follows_its_subfolders_in_order_and_ignores_files() {
        let dir = folder("follow", &["b", "d"]);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let node = watched_node(&ctx, &dir);
            assert_eq!(child_names(&node), ["b", "d"]);

            std::fs::create_dir(dir.join("c")).unwrap();
            std::fs::write(dir.join("file.txt"), b"x").unwrap();
            run_until(&ctx, "the new folder", || child_names(&node) == ["b", "c", "d"]);

            std::fs::remove_dir(dir.join("b")).unwrap();
            run_until(&ctx, "the removal", || child_names(&node) == ["c", "d"]);

            std::fs::rename(dir.join("d"), dir.join("a")).unwrap();
            run_until(&ctx, "the rename", || child_names(&node) == ["a", "c"]);
            assert_eq!(node.expandable(false), Some(true));
        })
        .unwrap();
    }

    #[test]
    fn the_subfolder_flag_follows_the_folder() {
        let dir = folder("flag", &[]);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let node = watched_node(&ctx, &dir);
            std::fs::create_dir(dir.join(".hidden")).unwrap();
            run_until(&ctx, "the hidden folder", || child_names(&node) == [".hidden"]);
            assert_eq!(node.expandable(false), Some(false), "a hidden folder does not count unless hidden ones are shown");
            assert_eq!(node.expandable(true), Some(true));
            std::fs::create_dir(dir.join("shown")).unwrap();
            run_until(&ctx, "the folder", || node.expandable(false) == Some(true));
        })
        .unwrap();
    }
}
