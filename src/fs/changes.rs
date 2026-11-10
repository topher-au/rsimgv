//! Changes to files, as a folder monitor reports them, in the way Nautilus handles them: each event becomes a typed
//! change on one queue, and the queue is later emptied in runs of the same kind, in the order the changes came. A run
//! of additions is then handled together, and so on, without a deletion ever being reordered past an addition.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use gtk::gio::FileMonitorEvent;

/// What happened to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A file appeared.
    Added(PathBuf),
    /// A file's contents or attributes changed.
    Changed(PathBuf),
    /// A file is gone.
    Removed(PathBuf),
    /// A file was moved or renamed.
    Moved { from: PathBuf, to: PathBuf },
    /// The place a path is on was unmounted, so it and everything under it is gone.
    Unmounted(PathBuf),
}

/// The kinds of change that are handled together: an unmount is a removal that is told why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Added,
    Changed,
    Moved,
    Removed,
}

impl Change {
    fn kind(&self) -> Kind {
        match self {
            Change::Added(_) => Kind::Added,
            Change::Changed(_) => Kind::Changed,
            Change::Moved { .. } => Kind::Moved,
            Change::Removed(_) | Change::Unmounted(_) => Kind::Removed,
        }
    }

    /// The change that a monitor's `event` on `child` is, if it is one. `other` is the second file of a move.
    pub fn from_event(event: FileMonitorEvent, child: &Path, other: Option<&Path>) -> Option<Change> {
        let child = child.to_path_buf();
        match event {
            FileMonitorEvent::AttributeChanged | FileMonitorEvent::ChangesDoneHint => Some(Change::Changed(child)),
            FileMonitorEvent::Created => Some(Change::Added(child)),
            FileMonitorEvent::Deleted => Some(Change::Removed(child)),
            FileMonitorEvent::Unmounted => Some(Change::Unmounted(child)),
            // A move from outside the watched folder has no source to name, so it is an addition.
            FileMonitorEvent::MovedIn => Some(match other {
                Some(from) => Change::Moved { from: from.to_path_buf(), to: child },
                None => Change::Added(child),
            }),
            FileMonitorEvent::MovedOut => Some(match other {
                Some(to) => Change::Moved { from: child, to: to.to_path_buf() },
                None => Change::Removed(child),
            }),
            FileMonitorEvent::Renamed => Some(match other {
                Some(to) => Change::Moved { from: child, to: to.to_path_buf() },
                None => Change::Removed(child),
            }),
            // `Changed` is a write in progress: `ChangesDoneHint` tells when it is over.
            _ => None,
        }
    }
}

thread_local! {
    static QUEUE: RefCell<VecDeque<Change>> = const { RefCell::new(VecDeque::new()) };
}

/// Puts `change` on the queue.
pub fn push(change: Change) {
    QUEUE.with(|queue| queue.borrow_mut().push_back(change));
}

/// Empties the queue, oldest change first.
pub fn take_all() -> Vec<Change> {
    QUEUE.with(|queue| queue.borrow_mut().drain(..).collect())
}

/// `changes` cut into runs of one kind, in the order they came: a run ends where the next change is of another kind.
pub fn batches(changes: Vec<Change>) -> Vec<Vec<Change>> {
    let mut runs: Vec<Vec<Change>> = Vec::new();
    for change in changes {
        match runs.last_mut() {
            Some(run) if run[0].kind() == change.kind() => run.push(change),
            _ => runs.push(vec![change]),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn added(name: &str) -> Change {
        Change::Added(PathBuf::from(name))
    }

    fn removed(name: &str) -> Change {
        Change::Removed(PathBuf::from(name))
    }

    #[test]
    fn runs_of_one_kind_stay_together_and_in_order() {
        let runs = batches(vec![
            added("/a"),
            added("/b"),
            removed("/a"),
            Change::Unmounted(PathBuf::from("/m")),
            added("/c"),
            Change::Changed(PathBuf::from("/c")),
        ]);
        assert_eq!(
            runs,
            vec![
                vec![added("/a"), added("/b")],
                vec![removed("/a"), Change::Unmounted(PathBuf::from("/m"))],
                vec![added("/c")],
                vec![Change::Changed(PathBuf::from("/c"))],
            ]
        );
    }

    #[test]
    fn nothing_makes_no_runs() {
        assert!(batches(Vec::new()).is_empty());
    }

    #[test]
    fn the_queue_is_emptied_in_the_order_it_was_filled() {
        take_all();
        push(added("/one"));
        push(removed("/two"));
        assert_eq!(take_all(), vec![added("/one"), removed("/two")]);
        assert!(take_all().is_empty());
    }

    #[test]
    fn events_become_changes() {
        let (a, b) = (Path::new("/d/a"), Path::new("/d/b"));
        assert_eq!(Change::from_event(FileMonitorEvent::Created, a, None), Some(added("/d/a")));
        assert_eq!(Change::from_event(FileMonitorEvent::Deleted, a, None), Some(removed("/d/a")));
        assert_eq!(Change::from_event(FileMonitorEvent::ChangesDoneHint, a, None), Some(Change::Changed(a.into())));
        assert_eq!(Change::from_event(FileMonitorEvent::Changed, a, None), None);
        assert_eq!(
            Change::from_event(FileMonitorEvent::Renamed, a, Some(b)),
            Some(Change::Moved { from: a.into(), to: b.into() })
        );
        assert_eq!(
            Change::from_event(FileMonitorEvent::MovedIn, b, Some(a)),
            Some(Change::Moved { from: a.into(), to: b.into() })
        );
        assert_eq!(Change::from_event(FileMonitorEvent::MovedIn, b, None), Some(added("/d/b")));
        assert_eq!(Change::from_event(FileMonitorEvent::MovedOut, a, None), Some(removed("/d/a")));
        assert_eq!(
            Change::from_event(FileMonitorEvent::MovedOut, a, Some(b)),
            Some(Change::Moved { from: a.into(), to: b.into() })
        );
        assert_eq!(Change::from_event(FileMonitorEvent::Unmounted, a, None), Some(Change::Unmounted(a.into())));
    }
}
