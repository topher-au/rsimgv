//! A file or folder shown in the browse view.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use gtk::{gio, glib};
use gtk::glib::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileEntry {
        pub(super) path: OnceCell<PathBuf>,
        pub(super) info: OnceCell<gio::FileInfo>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FileEntry {
        const NAME: &'static str = "RsimgvFileEntry";
        type Type = super::FileEntry;
    }

    impl ObjectImpl for FileEntry {}
}

glib::wrapper! {
    pub struct FileEntry(ObjectSubclass<imp::FileEntry>);
}

impl FileEntry {
    pub fn new(path: PathBuf, info: gio::FileInfo) -> Self {
        let entry: Self = glib::Object::new();
        let _ = entry.imp().path.set(path);
        let _ = entry.imp().info.set(info);
        entry
    }

    pub fn path(&self) -> &Path {
        self.imp().path.get().expect("path is set in new")
    }

    pub fn info(&self) -> &gio::FileInfo {
        self.imp().info.get().expect("info is set in new")
    }

    pub fn name(&self) -> String {
        self.info().display_name().to_string()
    }

    pub fn is_dir(&self) -> bool {
        self.info().file_type() == gio::FileType::Directory
    }

    pub fn size(&self) -> i64 {
        self.info().size()
    }

    /// When the entry was last modified, in seconds since the epoch.
    pub fn modified(&self) -> u64 {
        self.info().attribute_uint64("time::modified")
    }

    /// The content type, or empty if the listing did not give one.
    pub fn content_type(&self) -> String {
        self.info().attribute_string("standard::content-type").map(|kind| kind.to_string()).unwrap_or_default()
    }
}
