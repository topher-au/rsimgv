//! Keybindings: the catalogue of bindable actions, the chords bound to them, and their file. The window
//! resolves each key press here and activates the action it names. The table is kept per thread, since
//! GTK runs on one.

pub mod actions;
pub mod chord;
mod table;

use std::cell::RefCell;
use std::path::PathBuf;

pub use table::{Binding, BindingTable, Notification};

use crate::settings::config_path;

thread_local! {
    static TABLE: RefCell<BindingTable> = RefCell::new(BindingTable::defaults());
}

/// Runs `f` on the live table. Do not call `with_table` again inside `f`.
pub fn with_table<R>(f: impl FnOnce(&mut BindingTable) -> R) -> R {
    TABLE.with(|table| f(&mut table.borrow_mut()))
}

/// The bindings file: `keybindings.yaml` beside the settings file.
pub fn path() -> PathBuf {
    config_path().with_file_name("keybindings.yaml")
}

/// Loads the bindings file over the defaults. A missing file leaves the defaults, and problems are printed.
pub fn load() {
    let Ok(text) = std::fs::read_to_string(path()) else { return };
    for problem in with_table(|table| table.apply_yaml(&text)) {
        eprintln!("rsimgv: keybindings: {problem}");
    }
}

/// Writes the chords that differ from the defaults to the bindings file.
pub fn save() -> std::io::Result<()> {
    let text = with_table(|table| table.to_yaml());
    let path = path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::storage::atomic_file::write(&path, text.as_bytes())
}
