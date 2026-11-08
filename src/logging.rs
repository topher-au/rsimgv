//! Debug output, which is printed only when the app is run with `--verbose`.

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Turns the printing of debug output on. Once on, it stays on for the life of the process.
pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

/// Whether debug output is printed.
pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Prints to standard error, as `eprintln!` does, when the app was run with `--verbose`.
#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if $crate::logging::verbose() {
            eprintln!($($arg)*);
        }
    };
}
