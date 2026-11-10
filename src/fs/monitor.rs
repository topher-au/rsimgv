//! Watches one folder for changes, the way Nautilus's `nautilus-monitor.c` does: the folder's events are turned into
//! changes on the shared queue, and one idle call on the main loop empties the queue and hands the changes to the
//! `FileStore`, which updates the listings they touch. A monitor does not know who is interested: the store looks up
//! the listing a change belongs to.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use gio::prelude::*;
use gtk::{gio, glib};

use super::changes::{self, Change};
use super::FileStore;

thread_local! {
    /// Whether an idle call to empty the queue is waiting, so that a burst of events is one call.
    static CONSUME_SCHEDULED: Cell<bool> = const { Cell::new(false) };
}

/// Empties the queue on the main loop's next idle moment, unless that is already arranged.
fn schedule_consume() {
    if !CONSUME_SCHEDULED.with(|scheduled| scheduled.replace(true)) {
        // At idle priority, so the interface is served first. On the thread's own main context, whichever that is.
        glib::MainContext::ref_thread_default().spawn_local_with_priority(glib::Priority::DEFAULT_IDLE, async {
            CONSUME_SCHEDULED.with(|scheduled| scheduled.set(false));
            consume();
        });
    }
}

/// Hands every queued change to the shared store, in runs of one kind.
fn consume() {
    let store = FileStore::shared();
    for run in changes::batches(changes::take_all()) {
        store.notify(run);
    }
}

/// A watch on one folder. Dropping it stops the watch.
pub struct DirMonitor {
    /// `None` if the folder cannot be watched. The monitor is still made, so that nobody tries again.
    monitor: Option<(gio::FileMonitor, glib::SignalHandlerId)>,
    /// Tells when the folder's mount goes away, which some backends never say through the folder's own events.
    mounts: Option<(gio::VolumeMonitor, glib::SignalHandlerId)>,
}

impl DirMonitor {
    pub fn new(dir: &Path) -> Self {
        let location = gio::File::for_path(dir);
        let flags = gio::FileMonitorFlags::WATCH_MOUNTS | gio::FileMonitorFlags::WATCH_MOVES;
        let monitor = location.monitor_directory(flags, gio::Cancellable::NONE).ok().map(|monitor| {
            let id = monitor.connect_changed(|_, child, other, event| {
                let (Some(child), other) = (child.path(), other.and_then(|other| other.path())) else { return };
                if let Some(change) = Change::from_event(event, &child, other.as_deref()) {
                    changes::push(change);
                    schedule_consume();
                }
            });
            (monitor, id)
        });

        let volumes = gio::VolumeMonitor::get();
        let dir: PathBuf = dir.to_path_buf();
        let id = volumes.connect_mount_removed(move |_, mount| {
            let root = mount.root();
            if location.equal(&root) || location.has_prefix(&root) {
                changes::push(Change::Unmounted(dir.clone()));
                schedule_consume();
            }
        });
        Self { monitor, mounts: Some((volumes, id)) }
    }

    /// Whether the folder is being watched.
    pub fn is_watching(&self) -> bool {
        self.monitor.is_some()
    }
}

impl Drop for DirMonitor {
    fn drop(&mut self) {
        if let Some((monitor, id)) = self.monitor.take() {
            monitor.disconnect(id);
            monitor.cancel();
        }
        if let Some((volumes, id)) = self.mounts.take() {
            volumes.disconnect(id);
        }
    }
}
