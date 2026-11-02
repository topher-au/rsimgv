//! Drag and drop of files with other applications. Files travel as a list of paths, which other applications
//! receive as the `text/uri-list` of their locations; a list dropped here is read back the same way.

use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// The content of a drag carrying `paths`, as files.
pub fn file_list(paths: &[PathBuf]) -> gdk::ContentProvider {
    let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
    gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value())
}

/// The paths of the files in a dropped value, or none if it holds no files.
pub fn dropped_paths(value: &glib::Value) -> Vec<PathBuf> {
    value
        .get::<gdk::FileList>()
        .map(|list| list.files().iter().filter_map(|file| file.path()).collect())
        .unwrap_or_default()
}
