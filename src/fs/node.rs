//! A directory entry in `FsTree`. Children are read on demand, through the shared `FileStore`.

use std::cell::{Cell, OnceCell, RefCell};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures_channel::oneshot;
use gio::prelude::*;
use glib::subclass::prelude::*;

use super::{natural_cmp, FileEntry, FileStore};

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
    }
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
