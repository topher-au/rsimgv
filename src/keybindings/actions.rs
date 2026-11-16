//! The catalogue of bindable actions. Each action has a name the window activates, a label for the editor,
//! the contexts it can be bound in, and its default chords.

use std::cell::RefCell;
use std::collections::HashMap;

/// Where an action applies. A deeper scope takes a chord from a shallower one when both are active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everywhere in the window.
    Global,
    /// Browsing a folder.
    Browse,
    /// Viewing a picture or a video.
    View,
    /// Viewing a picture.
    Image,
    /// Viewing a video.
    Video,
    /// The playlist has the focus. It applies on top of browsing or the viewer.
    Playlist,
    /// The folder tree has the focus. It applies on top of browsing or the viewer.
    Tree,
}

impl Scope {
    /// The scope's id in the IPC and the keybindings file: `global`, `browse`, `view`, `image`, `video`, `playlist` or `tree`.
    pub fn id(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Browse => "browse",
            Scope::View => "view",
            Scope::Image => "image",
            Scope::Video => "video",
            Scope::Playlist => "playlist",
            Scope::Tree => "tree",
        }
    }

    /// Every scope, in the order the editor offers them.
    pub const ALL: [Scope; 7] =
        [Scope::Global, Scope::Browse, Scope::View, Scope::Image, Scope::Video, Scope::Playlist, Scope::Tree];

    /// The scope with the id `id`, if there is one.
    pub fn from_id(id: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|scope| scope.id() == id)
    }

    pub fn title(self) -> &'static str {
        match self {
            Scope::Global => "Global",
            Scope::Browse => "Browsing",
            Scope::View => "Viewer",
            Scope::Image => "Picture",
            Scope::Video => "Video",
            Scope::Playlist => "Playlist",
            Scope::Tree => "Folder Tree",
        }
    }

    /// A small icon for the scope.
    pub fn icon(self) -> &'static str {
        match self {
            Scope::Global => "emblem-system-symbolic",
            Scope::Browse => "folder-symbolic",
            Scope::View => "view-reveal-symbolic",
            Scope::Image => "image-x-generic-symbolic",
            Scope::Video => "video-x-generic-symbolic",
            Scope::Playlist => "view-list-symbolic",
            Scope::Tree => "folder-open-symbolic",
        }
    }

    /// How specific the scope is: deeper scopes win when both claim a chord.
    pub fn depth(self) -> u8 {
        match self {
            Scope::Global => 0,
            Scope::Browse | Scope::View => 1,
            Scope::Image | Scope::Video => 2,
            Scope::Playlist | Scope::Tree => 3,
        }
    }

    /// Whether a chord bound in one scope could also be pressed in the other, so binding it in both is a
    /// conflict. Global overlaps everything, and so do the playlist and the folder tree, which have the focus while browsing or viewing;
    /// the viewer overlaps the picture and video scopes inside it.
    pub fn overlaps(self, other: Scope) -> bool {
        self == other
            || matches!(self, Scope::Global | Scope::Playlist | Scope::Tree)
            || matches!(other, Scope::Global | Scope::Playlist | Scope::Tree)
            || matches!((self, other), (Scope::View, Scope::Image | Scope::Video) | (Scope::Image | Scope::Video, Scope::View))
    }
}

pub struct ActionInfo {
    /// The name activated on the window, with its group as prefix, such as `video.toggle-playback`.
    pub name: &'static str,
    /// What the action does, in a sentence.
    pub description: &'static str,
    pub label: &'static str,
    /// The symbolic icon the editor shows beside the action.
    pub icon: &'static str,
    pub defaults: &'static [&'static str],
}

/// The action that runs a script. Each binding of it names the script it runs.
pub const SCRIPT_ACTION: &str = "script";

/// The action that shows a notification over the window. Each binding of it carries the message, its level and how
/// long it stays.
pub const NOTIFY_ACTION: &str = "win.notify";

/// The levels a notification can have, as the IPC and the bindings file name them.
pub const NOTIFICATION_LEVELS: [&str; 5] = ["info", "success", "warning", "error", "plain"];

/// How long a notification stays up unless its binding says otherwise, in seconds, and the longest it can be set to.
pub const DEFAULT_NOTIFICATION_SECONDS: f64 = 4.0;
pub const MAX_NOTIFICATION_SECONDS: f64 = 600.0;

impl ActionInfo {
    /// Whether each binding of the action names a script to run.
    pub fn takes_script(&self) -> bool {
        self.name == SCRIPT_ACTION
    }

    /// Whether each binding of the action carries a notification to show.
    pub fn takes_notification(&self) -> bool {
        self.name == NOTIFY_ACTION
    }

    /// The scope the action belongs to, which its name gives: `browse.`, `view.`, `image.`, `video.`, `playlist.` and `tree.` actions to
    /// their own, and the rest, which act on the window, to Global. A plugin action's scope comes from its manifest.
    fn home(&self) -> Scope {
        if let Some(scope) = PLUGIN_SCOPES.with(|scopes| scopes.borrow().get(self.name).copied()) {
            return scope;
        }
        match self.name.split_once('.').map(|(group, _)| group) {
            Some("browse") => Scope::Browse,
            Some("view") => Scope::View,
            Some("image") => Scope::Image,
            Some("video") => Scope::Video,
            Some("playlist") => Scope::Playlist,
            Some("tree") => Scope::Tree,
            _ => Scope::Global,
        }
    }

    /// The context a binding of the action gets unless it picks another: the action's own.
    pub fn default_context(&self) -> Scope {
        self.home()
    }

    /// The contexts the action can be bound in, in the order the editor lists them. An action that acts on the window
    /// can be bound in any of them. A viewer action can be bound in the viewer as a whole, or only while it shows a
    /// picture or a video. The actions that need what is shown, the browsing cursor, a picture or a video, are bound
    /// where it is, or in Global. The two settings that change what the viewer lists as well are bound anywhere.
    pub fn contexts(&self) -> &'static [Scope] {
        let contexts: &'static [Scope] = match (self.home(), self.name) {
            (_, "browse.toggle-hidden" | "browse.toggle-media-only") | (Scope::Global, _) => &Scope::ALL,
            (Scope::Browse, _) => &[Scope::Global, Scope::Browse],
            (Scope::View, _) => &[Scope::Global, Scope::View, Scope::Image, Scope::Video],
            (Scope::Image, _) => &[Scope::Global, Scope::Image],
            (Scope::Video, _) => &[Scope::Global, Scope::Video],
            (Scope::Playlist, _) => &[Scope::Global, Scope::Playlist],
            (Scope::Tree, _) => &[Scope::Global, Scope::Tree],
        };
        contexts
    }
}

/// The slowest and fastest playback speeds, as multiples of normal.
pub const MIN_SPEED: f64 = 0.05;
pub const MAX_SPEED: f64 = 16.0;

/// What a binding's value counts, which decides how it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// A change in time, shown as `+ns` or `-ns`.
    Seconds,
    /// A change, shown as `+n%` or `-n%`.
    Percent,
    /// A multiple, shown as `n×`.
    Times,
    /// An angle, shown as `n°`.
    Degrees,
    /// A number of frames, shown as `+n` or `-n`.
    Frames,
}

/// The value a binding passes to an action that takes one: what it is called and counts, the range it is kept
/// within, the step the editor moves it by, how many decimals it shows, and what it is when the binding gives none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueInfo {
    pub label: &'static str,
    pub unit: Unit,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub digits: u32,
    pub default: f64,
}

impl ValueInfo {
    /// `value` as it is shown, in its unit, with at most the value's decimals and no trailing zeros.
    pub fn format(&self, value: f64) -> String {
        let mut number = format!("{value:.*}", self.digits as usize);
        if number.contains('.') {
            number = number.trim_end_matches('0').trim_end_matches('.').to_owned();
        }
        // Changes always show their sign.
        let signed = if value >= 0.0 { format!("+{number}") } else { number.clone() };
        match self.unit {
            Unit::Seconds => format!("{signed}s"),
            Unit::Percent => format!("{signed}%"),
            Unit::Times => format!("{number}\u{d7}"),
            Unit::Degrees => format!("{number}\u{b0}"),
            Unit::Frames => signed,
        }
    }
}

/// A choice a binding makes for an action, such as which volume a volume action changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetInfo {
    /// What the choice is called in the editor.
    pub label: &'static str,
    /// The target a binding gets when it names none.
    pub default: &'static str,
    /// The targets the action takes, each with the name the bindings file uses and the label the editor shows.
    pub choices: &'static [(&'static str, &'static str)],
}

impl TargetInfo {
    /// The name of `value`, or of the default when it names no target the action takes.
    pub fn name(&self, value: Option<&str>) -> &'static str {
        value
            .and_then(|value| self.choices.iter().find(|(name, _)| *name == value).map(|(name, _)| *name))
            .unwrap_or(self.default)
    }

    /// What `value` is called, or the default when it names no target the action takes.
    pub fn title(&self, value: Option<&str>) -> &'static str {
        let name = self.name(value);
        self.choices.iter().find(|(choice, _)| *choice == name).map_or(self.default, |(_, title)| *title)
    }

    /// Whether `value` names a target the action takes.
    pub fn has(&self, value: &str) -> bool {
        self.choices.iter().any(|(name, _)| *name == value)
    }
}

/// The volume a volume action adjusts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VolumeTarget {
    /// The video shown, or the next one.
    #[default]
    Video,
    /// The application's own volume, which videos without one of their own start at.
    Application,
}

impl VolumeTarget {
    /// The name of the target in a bindings file.
    pub const fn name(self) -> &'static str {
        match self {
            VolumeTarget::Video => "video",
            VolumeTarget::Application => "application",
        }
    }

    /// The target `name` calls for, if it is one.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "video" => Some(VolumeTarget::Video),
            "application" => Some(VolumeTarget::Application),
            _ => None,
        }
    }
}

/// The targets a binding of a volume action can choose between.
pub const VOLUME_TARGETS: &[(&str, &str)] = &[(VolumeTarget::Video.name(), "Current Video"), (VolumeTarget::Application.name(), "Application")];

/// The aspect ratios a binding of the set-aspect action can choose between, and that the cycle action steps through.
pub const ASPECT_TARGETS: &[(&str, &str)] =
    &[("auto", "Original"), ("4:3", "4:3"), ("16:9", "16:9"), ("9:16", "9:16"), ("1:1", "1:1"), ("3:2", "3:2"), ("2:3", "2:3"), ("21:9", "21:9")];

/// The target the action `name` takes from its bindings, if it takes one.
pub fn target_info(name: &str) -> Option<TargetInfo> {
    match name {
        "video.volume" | "video.volume-up" | "video.volume-down" => {
            Some(TargetInfo { label: "Volume", default: VolumeTarget::Video.name(), choices: VOLUME_TARGETS })
        }
        "video.set-aspect" => Some(TargetInfo { label: "Aspect ratio", default: "auto", choices: ASPECT_TARGETS }),
        _ => None,
    }
}

/// The value the action `name` takes from its bindings, if it takes one.
pub fn value_info(name: &str) -> Option<ValueInfo> {
    match name {
        "video.seek" => Some(ValueInfo { label: "Seconds", unit: Unit::Seconds, min: -3600.0, max: 3600.0, step: 1.0, digits: 1, default: 10.0 }),
        "video.frame-seek" => Some(ValueInfo { label: "Frames", unit: Unit::Frames, min: -1000.0, max: 1000.0, step: 1.0, digits: 0, default: 1.0 }),
        "video.volume" => Some(ValueInfo { label: "Change", unit: Unit::Percent, min: -100.0, max: 100.0, step: 1.0, digits: 0, default: 5.0 }),
        "video.set-speed" => Some(ValueInfo { label: "Speed", unit: Unit::Times, min: MIN_SPEED, max: MAX_SPEED, step: 0.25, digits: 2, default: 1.0 }),
        "view.set-rotation" => Some(ValueInfo { label: "Degrees", unit: Unit::Degrees, min: 0.0, max: 270.0, step: 90.0, digits: 0, default: 0.0 }),
        "video.adjust-speed" => Some(ValueInfo {
            label: "Change",
            unit: Unit::Percent,
            min: -MAX_SPEED * 100.0,
            max: MAX_SPEED * 100.0,
            step: 5.0,
            digits: 0,
            default: 25.0,
        }),
        _ => None,
    }
}

/// Every built-in bindable action, in the order the editor lists them. The names, labels, icons and default keys
/// follow pyimgv-gtk's catalogue. Actions that need a feature rsimgv does not have yet are left out.
pub const ACTIONS: &[ActionInfo] = &[
    ActionInfo {
        name: "app.quit", description: "Close the application.", icon: "application-exit-symbolic", label: "Quit", defaults: &["ctrl+q"],
    },
    ActionInfo {
        name: "win.settings", description: "Open the settings window.", icon: "preferences-system-symbolic", label: "Settings", defaults: &["ctrl+comma"],
    },
    ActionInfo {
        name: "win.settings-keybindings", description: "Open the settings on the Keybindings page.", icon: "preferences-desktop-keyboard-symbolic", label: "Edit Keybindings", defaults: &["ctrl+shift+comma"],
    },
    ActionInfo {
        name: "win.open-folder", description: "Choose a folder to browse in this window.", icon: "folder-open-symbolic", label: "Open Folder", defaults: &["ctrl+o"],
    },
    ActionInfo {
        name: "win.edit-location", description: "Type a path to open, in the path bar.", icon: "document-edit-symbolic", label: "Edit Location", defaults: &["ctrl+l"],
    },
    ActionInfo {
        name: "win.navigate-back", description: "Go back to the folder shown before this one.", icon: "go-previous-symbolic", label: "Back", defaults: &["alt+left"],
    },
    ActionInfo {
        name: "win.navigate-forward", description: "Go forward to the folder after this one in the history.", icon: "go-next-symbolic", label: "Forward", defaults: &["alt+right"],
    },
    ActionInfo {
        name: "win.navigate-up", description: "Go to the parent folder.", icon: "go-up-symbolic", label: "Go Up", defaults: &["alt+up"],
    },
    ActionInfo {
        name: "win.next-folder", description: "Go to the next folder in depth-first order of the folder tree. In the viewer, go to the next folder with a picture or video and show its first one.", icon: "go-down-symbolic", label: "Next Folder", defaults: &[],
    },
    ActionInfo {
        name: "win.previous-folder", description: "Go to the previous folder in depth-first order of the folder tree. In the viewer, go to the previous folder with a picture or video and show its first one.", icon: "go-up-symbolic", label: "Previous Folder", defaults: &[],
    },
    ActionInfo {
        name: "win.next-media-folder", description: "Go to the next folder with a picture or video, and select its first one.", icon: "go-down-symbolic", label: "Next Media Folder", defaults: &[],
    },
    ActionInfo {
        name: "win.previous-media-folder", description: "Go to the previous folder with a picture or video, and select its last one.", icon: "go-up-symbolic", label: "Previous Media Folder", defaults: &[],
    },
    ActionInfo {
        name: "win.bookmark-folder", description: "Bookmark the folder shown, under a name you choose.", icon: "bookmark-new-symbolic", label: "Bookmark Folder", defaults: &["ctrl+d"],
    },
    ActionInfo {
        name: "win.toggle-bookmarks", description: "Expand or collapse the bookmarks list in the sidebar.", icon: "user-bookmarks-symbolic", label: "Toggle Bookmarks", defaults: &[],
    },
    ActionInfo {
        name: "win.playlist-toggle", description: "Add the item the viewer shows to the playlist, or the item under the cursor or the marked items while browsing, or remove them if they are already in it. A folder stands for the pictures and videos in and under it.", icon: "list-add-symbolic", label: "Add to or Remove from Playlist", defaults: &["i"],
    },
    ActionInfo {
        name: "win.playlist-play", description: "Show the playlist in the viewer, from the item it was left on. The folder being browsed stays as it is.", icon: "media-playback-start-symbolic", label: "Play Playlist", defaults: &["ctrl+p"],
    },
    ActionInfo {
        name: "win.playlist-next", description: "Show the next item in the playlist, going round to the first after the last. If the viewer is not showing the playlist, show the playlist from the item it was left on.", icon: "media-skip-forward-symbolic", label: "Next Playlist Item", defaults: &["bracketright"],
    },
    ActionInfo {
        name: "win.playlist-previous", description: "Show the item before this one in the playlist, going round to the last before the first. If the viewer is not showing the playlist, show the playlist from the item it was left on.", icon: "media-skip-backward-symbolic", label: "Previous Playlist Item", defaults: &["bracketleft"],
    },
    ActionInfo {
        name: "win.playlist-first", description: "Show the first item in the playlist.", icon: "go-first-symbolic", label: "First Playlist Item", defaults: &[],
    },
    ActionInfo {
        name: "win.playlist-last", description: "Show the last item in the playlist.", icon: "go-last-symbolic", label: "Last Playlist Item", defaults: &[],
    },
    ActionInfo {
        name: "win.playlist-load", description: "Choose an M3U, PLS or XSPF file to load into the playlist.", icon: "document-open-symbolic", label: "Load Playlist", defaults: &[],
    },
    ActionInfo {
        name: "win.playlist-save", description: "Save the playlist as an M3U, PLS or XSPF file.", icon: "document-save-as-symbolic", label: "Save Playlist", defaults: &[],
    },
    ActionInfo {
        name: "win.playlist-clear", description: "Remove everything from the playlist, after asking.", icon: "edit-clear-all-symbolic", label: "Clear Playlist", defaults: &[],
    },
    ActionInfo {
        name: "win.playlist-shuffle", description: "Shuffle the order the playlist plays in, or go back to the order its items were made in. The other order is kept either way.", icon: "media-playlist-shuffle-symbolic", label: "Shuffle Playlist", defaults: &[],
    },
    ActionInfo {
        name: "win.playlist-detach", description: "Take the playlist out into a window of its own, or put it back beside the browse view.", icon: "window-new-symbolic", label: "Detach Playlist", defaults: &[],
    },
    ActionInfo {
        name: "win.toggle-playlist", description: "Show the playlist beside the browse view, on the side opposite the sidebar, or hide it. While it is in its own window, that window comes forward.", icon: "view-list-symbolic", label: "Toggle Playlist Panel", defaults: &["ctrl+shift+p"],
    },
    ActionInfo {
        name: "win.toggle-filesystem", description: "Expand or collapse the folder tree in the sidebar.", icon: "drive-harddisk-symbolic", label: "Toggle Filesystem", defaults: &[],
    },
    ActionInfo {
        name: "win.toggle-sidebar", description: "Show or hide the sidebar.", icon: "sidebar-show-symbolic", label: "Toggle Sidebar", defaults: &["f9"],
    },
    ActionInfo {
        name: "win.focus-tree", description: "Move the keyboard focus to the folder tree, showing the sidebar if it is hidden.", icon: "folder-open-symbolic", label: "Focus Folder Tree", defaults: &[],
    },
    ActionInfo {
        name: "win.focus-playlist", description: "Move the keyboard focus to the playlist, showing it if it is hidden.", icon: "view-list-symbolic", label: "Focus Playlist", defaults: &[],
    },
    ActionInfo {
        name: "win.focus-viewer", description: "Move the keyboard focus to the viewer while it is open, or else to the browse view.", icon: "view-reveal-symbolic", label: "Focus Browser / Viewer", defaults: &[],
    },
    ActionInfo {
        name: "win.toggle-viewer", description: "Show the selected picture or video in the viewer, or return to the browse view, without changing the item shown or selected.", icon: "view-reveal-symbolic", label: "Toggle Viewer", defaults: &["b"],
    },
    ActionInfo {
        name: "win.focus-cycle", description: "Move the keyboard focus to the next of the folder tree, the browse view or viewer, and the playlist, skipping any that is hidden.", icon: "media-playlist-repeat-symbolic", label: "Cycle Focus", defaults: &["f6"],
    },
    ActionInfo {
        name: "win.focus-cycle-reverse", description: "Move the keyboard focus to the previous of the playlist, the browse view or viewer, and the folder tree, skipping any that is hidden.", icon: "media-playlist-repeat-symbolic", label: "Cycle Focus Backwards", defaults: &["shift+f6"],
    },
    ActionInfo {
        name: "win.toggle-keybinds", description: "Show or hide a list of the keybindings in force where you are: the ones for browsing, or for the viewer, a picture or a video, and the global ones.", icon: "input-keyboard-symbolic", label: "Toggle Keybinds Overlay", defaults: &["f1"],
    },
    ActionInfo {
        name: "win.toggle-fullscreen", description: "Switch the window between fullscreen and windowed.", icon: "view-fullscreen-symbolic", label: "Fullscreen", defaults: &["f11"],
    },
    ActionInfo {
        name: NOTIFY_ACTION, description: "Show a message over the window. Each binding sets the message, its level and how many seconds it stays.", icon: "dialog-information-symbolic", label: "Show Notification", defaults: &[],
    },
    ActionInfo {
        name: SCRIPT_ACTION, description: "Run one of the scripts from the Scripts page on the item under the cursor while browsing, or on the item the viewer shows. Each binding picks its script.", icon: "utilities-terminal-symbolic", label: "Run Script", defaults: &[],
    },
    ActionInfo {
        name: "browse.cursor-up", description: "Move the cursor up one row.", icon: "go-up-symbolic", label: "Move Up", defaults: &["up", "w"],
    },
    ActionInfo {
        name: "browse.cursor-down", description: "Move the cursor down one row.", icon: "go-down-symbolic", label: "Move Down", defaults: &["down", "s"],
    },
    ActionInfo {
        name: "browse.cursor-previous", description: "Move the cursor to the item before it.", icon: "go-previous-symbolic", label: "Move Left", defaults: &["left", "a"],
    },
    ActionInfo {
        name: "browse.cursor-next", description: "Move the cursor to the item after it.", icon: "go-next-symbolic", label: "Move Right", defaults: &["right", "d"],
    },
    ActionInfo {
        name: "browse.cursor-page-up", description: "Move the cursor up one page.", icon: "pan-up-symbolic", label: "Page Up", defaults: &["page_up"],
    },
    ActionInfo {
        name: "browse.cursor-page-down", description: "Move the cursor down one page.", icon: "pan-down-symbolic", label: "Page Down", defaults: &["page_down"],
    },
    ActionInfo {
        name: "browse.keep-up", description: "Move the cursor up one row, keeping the marked items.", icon: "go-up-symbolic", label: "Move Up Keeping Selection", defaults: &["ctrl+up"],
    },
    ActionInfo {
        name: "browse.keep-down", description: "Move the cursor down one row, keeping the marked items.", icon: "go-down-symbolic", label: "Move Down Keeping Selection", defaults: &["ctrl+down"],
    },
    ActionInfo {
        name: "browse.keep-previous", description: "Move the cursor to the item before it, keeping the marked items.", icon: "go-previous-symbolic", label: "Move Left Keeping Selection", defaults: &["ctrl+left"],
    },
    ActionInfo {
        name: "browse.keep-next", description: "Move the cursor to the item after it, keeping the marked items.", icon: "go-next-symbolic", label: "Move Right Keeping Selection", defaults: &["ctrl+right"],
    },
    ActionInfo {
        name: "browse.keep-page-up", description: "Move the cursor up one page, keeping the marked items.", icon: "pan-up-symbolic", label: "Page Up Keeping Selection", defaults: &["ctrl+page_up"],
    },
    ActionInfo {
        name: "browse.keep-page-down", description: "Move the cursor down one page, keeping the marked items.", icon: "pan-down-symbolic", label: "Page Down Keeping Selection", defaults: &["ctrl+page_down"],
    },
    ActionInfo {
        name: "browse.extend-up", description: "Move the cursor up one row, marking every item from the anchor to it.", icon: "go-up-symbolic", label: "Extend Selection Up", defaults: &["shift+up"],
    },
    ActionInfo {
        name: "browse.extend-down", description: "Move the cursor down one row, marking every item from the anchor to it.", icon: "go-down-symbolic", label: "Extend Selection Down", defaults: &["shift+down"],
    },
    ActionInfo {
        name: "browse.extend-previous", description: "Move the cursor to the item before it, marking every item from the anchor to it.", icon: "go-previous-symbolic", label: "Extend Selection Left", defaults: &["shift+left"],
    },
    ActionInfo {
        name: "browse.extend-next", description: "Move the cursor to the item after it, marking every item from the anchor to it.", icon: "go-next-symbolic", label: "Extend Selection Right", defaults: &["shift+right"],
    },
    ActionInfo {
        name: "browse.extend-page-up", description: "Move the cursor up one page, marking every item from the anchor to it.", icon: "pan-up-symbolic", label: "Extend Selection Page Up", defaults: &["shift+page_up"],
    },
    ActionInfo {
        name: "browse.extend-page-down", description: "Move the cursor down one page, marking every item from the anchor to it.", icon: "pan-down-symbolic", label: "Extend Selection Page Down", defaults: &["shift+page_down"],
    },
    ActionInfo {
        name: "browse.toggle-mark", description: "Mark the item under the cursor, or unmark it if it is marked, leaving the other marks as they are.", icon: "emblem-default-symbolic", label: "Toggle Selection", defaults: &["ctrl+space"],
    },
    ActionInfo {
        name: "browse.first", description: "Move the cursor to the first item.", icon: "go-first-symbolic", label: "First Item", defaults: &["home"],
    },
    ActionInfo {
        name: "browse.last", description: "Move the cursor to the last item.", icon: "go-last-symbolic", label: "Last Item", defaults: &["end"],
    },
    ActionInfo {
        name: "browse.select-all", description: "Mark every item in the folder, to act on them together.", icon: "edit-select-all-symbolic", label: "Select All", defaults: &["ctrl+a"],
    },
    ActionInfo {
        name: "browse.open", description: "Open the item under the cursor: a folder is browsed, a picture or video is shown in the viewer.", icon: "document-open-symbolic", label: "Open", defaults: &["e", "return"],
    },
    ActionInfo {
        name: "browse.properties", description: "Show the properties of the marked items, or of the item under the cursor, or of the folder if there is none: what they are, how big, when they changed, and their permissions.", icon: "document-properties-symbolic", label: "Properties", defaults: &["alt+return"],
    },
    ActionInfo {
        name: "browse.show-tooltip", description: "Show the description of the item under the cursor, which otherwise opens only when the pointer rests over an item.", icon: "dialog-information-symbolic", label: "Show Tooltip", defaults: &[],
    },
    ActionInfo {
        name: "browse.up-directory", description: "Go to the parent folder.", icon: "go-up-symbolic", label: "Up Directory", defaults: &["q", "backspace"],
    },
    ActionInfo {
        name: "browse.thumbnail-larger", description: "Make the items in the browse view larger.", icon: "zoom-in-symbolic", label: "Larger Thumbnails", defaults: &["ctrl+shift+equal", "ctrl+equal"],
    },
    ActionInfo {
        name: "browse.thumbnail-smaller", description: "Make the items in the browse view smaller.", icon: "zoom-out-symbolic", label: "Smaller Thumbnails", defaults: &["ctrl+minus"],
    },
    ActionInfo {
        name: "browse.toggle-hidden", description: "Show or hide hidden files and folders.", icon: "view-reveal-symbolic", label: "Toggle Hidden Files", defaults: &["ctrl+h"],
    },
    ActionInfo {
        name: "browse.toggle-media-only", description: "Show only pictures and videos, or every file.", icon: "video-x-generic-symbolic", label: "Toggle Media Only", defaults: &["ctrl+m"],
    },
    ActionInfo {
        name: "browse.toggle-search", description: "Show the search bar under the header, to filter the items by name or an attribute and include subfolders, or hide it and show every item again.", icon: "system-search-symbolic", label: "Toggle Search", defaults: &["ctrl+shift+f"],
    },
    ActionInfo {
        name: "browse.toggle-filename", description: "Show or hide each item's filename.", icon: "insert-text-symbolic", label: "Toggle Filename", defaults: &[],
    },
    ActionInfo {
        name: "browse.toggle-item-info", description: "Show or hide each item's information under its name.", icon: "dialog-information-symbolic", label: "Toggle Item Info", defaults: &[],
    },
    ActionInfo {
        name: "playlist.cursor-up", description: "Select only the row above the selected ones in the playlist, or the last row if none is selected.", icon: "go-up-symbolic", label: "Select Previous Row", defaults: &["up"],
    },
    ActionInfo {
        name: "playlist.cursor-down", description: "Select only the row below the selected ones in the playlist, or the first row if none is selected.", icon: "go-down-symbolic", label: "Select Next Row", defaults: &["down"],
    },
    ActionInfo {
        name: "playlist.extend-up", description: "Add the row above the selected ones to the selection.", icon: "go-up-symbolic", label: "Extend Selection Up", defaults: &["shift+up"],
    },
    ActionInfo {
        name: "playlist.extend-down", description: "Add the row below the selected ones to the selection.", icon: "go-down-symbolic", label: "Extend Selection Down", defaults: &["shift+down"],
    },
    ActionInfo {
        name: "playlist.select-all", description: "Select every row of the playlist that the search shows.", icon: "edit-select-all-symbolic", label: "Select All", defaults: &["ctrl+a"],
    },
    ActionInfo {
        name: "playlist.move-up", description: "Move the selected playlist items up one place, together.", icon: "go-up-symbolic", label: "Move Items Up", defaults: &["ctrl+up"],
    },
    ActionInfo {
        name: "playlist.move-down", description: "Move the selected playlist items down one place, together.", icon: "go-down-symbolic", label: "Move Items Down", defaults: &["ctrl+down"],
    },
    ActionInfo {
        name: "playlist.move-top", description: "Move the selected playlist items to the start of the playlist, keeping their order.", icon: "go-top-symbolic", label: "Move Items to Start", defaults: &["ctrl+home"],
    },
    ActionInfo {
        name: "playlist.move-bottom", description: "Move the selected playlist items to the end of the playlist, keeping their order.", icon: "go-bottom-symbolic", label: "Move Items to End", defaults: &["ctrl+end"],
    },
    ActionInfo {
        name: "playlist.toggle-search", description: "Show the playlist's search field, or hide it and show every item again.", icon: "system-search-symbolic", label: "Toggle Search", defaults: &["ctrl+f"],
    },
    ActionInfo {
        name: "playlist.focus-search", description: "Show the playlist's search field if it is hidden, and move the focus to it.", icon: "edit-find-symbolic", label: "Focus Search", defaults: &[],
    },
    ActionInfo {
        name: "tree.cursor-up", description: "Select the folder above the selected one in the folder tree, or the last one if none is selected.", icon: "go-up-symbolic", label: "Select Previous Folder", defaults: &["up"],
    },
    ActionInfo {
        name: "tree.cursor-down", description: "Select the folder below the selected one in the folder tree, or the first one if none is selected.", icon: "go-down-symbolic", label: "Select Next Folder", defaults: &["down"],
    },
    ActionInfo {
        name: "tree.first", description: "Select the first row of the folder tree.", icon: "go-first-symbolic", label: "First Folder", defaults: &["home"],
    },
    ActionInfo {
        name: "tree.last", description: "Select the last visible row of the folder tree.", icon: "go-last-symbolic", label: "Last Folder", defaults: &["end"],
    },
    ActionInfo {
        name: "tree.activate", description: "Expand the selected folder in the folder tree, or collapse it if it is expanded.", icon: "go-next-symbolic", label: "Toggle Folder", defaults: &["return"],
    },
    ActionInfo {
        name: "tree.expand", description: "Expand the selected folder in the folder tree.", icon: "pan-end-symbolic", label: "Expand Folder", defaults: &["right"],
    },
    ActionInfo {
        name: "tree.collapse", description: "Collapse the selected folder in the folder tree.", icon: "pan-down-symbolic", label: "Collapse Folder", defaults: &["left"],
    },
    ActionInfo {
        name: "view.previous", description: "Show the picture or video before this one in the folder, or in the playlist while the viewer shows that.", icon: "go-previous-symbolic", label: "Previous Item", defaults: &["comma"],
    },
    ActionInfo {
        name: "view.next", description: "Show the picture or video after this one in the folder, or in the playlist while the viewer shows that.", icon: "go-next-symbolic", label: "Next Item", defaults: &["period"],
    },
    ActionInfo {
        name: "view.first", description: "Show the first picture or video in the folder, or in the playlist while the viewer shows that.", icon: "go-first-symbolic", label: "First Item", defaults: &["shift+comma"],
    },
    ActionInfo {
        name: "view.last", description: "Show the last picture or video in the folder, or in the playlist while the viewer shows that.", icon: "go-last-symbolic", label: "Last Item", defaults: &["shift+period"],
    },
    ActionInfo {
        name: "view.close", description: "Close the viewer and return to the browse view.", icon: "window-close-symbolic", label: "Close Viewer", defaults: &["q", "escape"],
    },
    ActionInfo {
        name: "view.pin", description: "Keep the viewer's header above the picture in fullscreen, and a video's controls shown.", icon: "view-pin-symbolic", label: "Pin Viewer Header", defaults: &["p"],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar", description: "Show or hide the viewer's thumbnail bar.", icon: "view-grid-symbolic", label: "Toggle Thumbnail Bar", defaults: &["t"],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar-pin", description: "Keep the floating thumbnail bar shown, or let it hide until the pointer comes near.", icon: "view-pin-symbolic", label: "Pin Thumbnail Bar", defaults: &[],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar-overlay", description: "Float the thumbnail bar over the picture, or put it beside the picture.", icon: "window-restore-symbolic", label: "Toggle Thumbnail Bar Overlay", defaults: &[],
    },
    ActionInfo {
        name: "view.toggle-status-bar", description: "Show or hide the status bar under the viewer's item.", icon: "view-continuous-symbolic", label: "Toggle Status Bar", defaults: &[],
    },
    ActionInfo {
        name: "view.rotate-clockwise", description: "Turn the picture or video a quarter turn clockwise.", icon: "object-rotate-right-symbolic", label: "Rotate Clockwise", defaults: &[],
    },
    ActionInfo {
        name: "view.rotate-counter-clockwise", description: "Turn the picture or video a quarter turn counter-clockwise.", icon: "object-rotate-left-symbolic", label: "Rotate Counter-Clockwise", defaults: &[],
    },
    ActionInfo {
        name: "view.set-rotation", description: "Turn the picture or video to the angle bound, clockwise from how the file has it: 0, 90, 180 or 270 degrees.", icon: "object-rotate-right-symbolic", label: "Set Rotation", defaults: &[],
    },
    ActionInfo {
        name: "view.flip-vertical", description: "Mirror the picture or video from top to bottom.", icon: "object-flip-vertical-symbolic", label: "Flip Top-Bottom", defaults: &[],
    },
    ActionInfo {
        name: "view.flip-horizontal", description: "Mirror the picture or video from left to right.", icon: "object-flip-horizontal-symbolic", label: "Flip Left-Right", defaults: &[],
    },
    ActionInfo {
        name: "image.zoom-in", description: "Enlarge the picture.", icon: "zoom-in-symbolic", label: "Zoom In", defaults: &["shift+equal", "equal"],
    },
    ActionInfo {
        name: "image.zoom-out", description: "Shrink the picture.", icon: "zoom-out-symbolic", label: "Zoom Out", defaults: &["minus"],
    },
    ActionInfo {
        name: "image.zoom-in-at-pointer", description: "Enlarge the picture, keeping the point under the pointer in place.", icon: "zoom-in-symbolic", label: "Zoom In at Pointer", defaults: &[],
    },
    ActionInfo {
        name: "image.zoom-out-at-pointer", description: "Shrink the picture, keeping the point under the pointer in place.", icon: "zoom-out-symbolic", label: "Zoom Out at Pointer", defaults: &[],
    },
    ActionInfo {
        name: "image.fit-cycle", description: "Switch to the next fit mode: window, width, height, fill, then original size.", icon: "zoom-fit-best-symbolic", label: "Cycle Fit Mode", defaults: &["f"],
    },
    ActionInfo {
        name: "image.fit-window", description: "Scale the picture to fit the window.", icon: "zoom-fit-best-symbolic", label: "Fit Window", defaults: &["1"],
    },
    ActionInfo {
        name: "image.fit-width", description: "Scale the picture to the width of the window.", icon: "zoom-fit-best-symbolic", label: "Fit Width", defaults: &["2"],
    },
    ActionInfo {
        name: "image.fit-height", description: "Scale the picture to the height of the window.", icon: "zoom-fit-best-symbolic", label: "Fit Height", defaults: &["3"],
    },
    ActionInfo {
        name: "image.fit-fill", description: "Scale the picture to cover the window, cropping what overhangs.", icon: "zoom-fit-best-symbolic", label: "Fill Window", defaults: &["5"],
    },
    ActionInfo {
        name: "image.fit-original", description: "Show the picture at its own size.", icon: "zoom-original-symbolic", label: "Original Size", defaults: &["4"],
    },
    ActionInfo {
        name: "image.toggle-upscaled", description: "Switch between the picture and its upscaled copy.", icon: "image-x-generic-symbolic", label: "Toggle Upscaled", defaults: &["u"],
    },
    ActionInfo {
        name: "image.compare-upscaled", description: "Show the upscaled picture beside its original, with a divider to drag between them.", icon: "view-dual-symbolic", label: "Compare Upscaled", defaults: &["shift+u"],
    },
    ActionInfo {
        name: "image.pan-up", description: "Move the picture up within the window.", icon: "go-up-symbolic", label: "Pan Up", defaults: &["up"],
    },
    ActionInfo {
        name: "image.pan-down", description: "Move the picture down within the window.", icon: "go-down-symbolic", label: "Pan Down", defaults: &["down"],
    },
    ActionInfo {
        name: "image.pan-left", description: "Move the picture left within the window.", icon: "go-previous-symbolic", label: "Pan Left", defaults: &["left"],
    },
    ActionInfo {
        name: "image.pan-right", description: "Move the picture right within the window.", icon: "go-next-symbolic", label: "Pan Right", defaults: &["right"],
    },
    ActionInfo {
        name: "video.play-pause", description: "Play the video, or pause it while it plays.", icon: "media-playback-start-symbolic", label: "Play / Pause", defaults: &["space"],
    },
    ActionInfo {
        name: "video.seek-forward", description: "Skip forward 5 seconds.", icon: "media-seek-forward-symbolic", label: "Seek Forward", defaults: &["right"],
    },
    ActionInfo {
        name: "video.seek-backward", description: "Skip back 5 seconds.", icon: "media-seek-backward-symbolic", label: "Seek Backward", defaults: &["left"],
    },
    ActionInfo {
        name: "video.seek", description: "Move the playback position by the number of seconds bound; a negative number goes back.", icon: "media-seek-forward-symbolic", label: "Seek", defaults: &[],
    },
    ActionInfo {
        name: "video.frame-seek", description: "Step by the number of frames bound, forwards or back with a negative number.", icon: "media-skip-forward-symbolic", label: "Seek by Frames", defaults: &[],
    },
    ActionInfo {
        name: "video.set-speed", description: "Play at the speed bound, such as 2 for double speed or 0.5 for half; 1 is normal.", icon: "power-profile-performance-symbolic", label: "Set Speed", defaults: &[],
    },
    ActionInfo {
        name: "video.adjust-speed", description: "Change the playback speed by the amount bound, such as 0.25 faster; a negative amount slows it.", icon: "power-profile-balanced-symbolic", label: "Adjust Speed", defaults: &[],
    },
    ActionInfo {
        name: "video.volume-up", description: "Raise the volume of the video shown, or of the application.", icon: "audio-volume-high-symbolic", label: "Volume Up", defaults: &["up"],
    },
    ActionInfo {
        name: "video.volume-down", description: "Lower the volume of the video shown, or of the application.", icon: "audio-volume-low-symbolic", label: "Volume Down", defaults: &["down"],
    },
    ActionInfo {
        name: "video.volume", description: "Change the volume of the video shown, or of the application, by the percentage bound; a negative number turns it down.", icon: "audio-volume-high-symbolic", label: "Adjust Volume", defaults: &[],
    },
    ActionInfo {
        name: "video.mute", description: "Mute the sound, or unmute it.", icon: "audio-volume-muted-symbolic", label: "Mute", defaults: &["m"],
    },
    ActionInfo {
        name: "video.toggle-svp", description: "Turn SVP frame interpolation on or off.", icon: "video-display-symbolic", label: "Toggle SVP", defaults: &["v"],
    },
    ActionInfo {
        name: "video.toggle-backdrop", description: "Fill the space round the video with a blurred, zoomed copy of it, or stop doing so.", icon: "view-reveal-symbolic", label: "Toggle Backdrop", defaults: &[],
    },
    ActionInfo {
        name: "video.toggle-crop", description: "Crop the video to its picture, or stop detecting where to crop it.", icon: "edit-crop-symbolic", label: "Toggle Crop Detection", defaults: &[],
    },
    ActionInfo {
        name: "video.cycle-crop-ratio", description: "Cycle the shape the video is cropped to through auto and the standard aspect ratios.", icon: "object-select-symbolic", label: "Cycle Crop Shape", defaults: &[],
    },
    ActionInfo {
        name: "video.set-aspect", description: "Set the output aspect ratio of the video to the ratio the binding chooses, or back to the video's own with Original.", icon: "video-display-symbolic", label: "Set Aspect Ratio", defaults: &[],
    },
    ActionInfo {
        name: "video.cycle-aspect", description: "Cycle the output aspect ratio of the video through Original and the common aspect ratios.", icon: "object-select-symbolic", label: "Cycle Aspect Ratio", defaults: &[],
    },
    ActionInfo {
        name: "video.edit-crop", description: "Drag the crop over the video, then apply it or cancel.", icon: "edit-crop-symbolic", label: "Edit Crop", defaults: &[],
    },
    ActionInfo {
        name: "video.lock-crop", description: "Hold the crop the video has now so it stops being detected, or let it be found again.", icon: "changes-prevent-symbolic", label: "Lock Crop", defaults: &[],
    },
    ActionInfo {
        name: "video.zoom-in", description: "Zoom in on the video.", icon: "zoom-in-symbolic", label: "Zoom In Video", defaults: &["shift+equal", "equal"],
    },
    ActionInfo {
        name: "video.zoom-out", description: "Zoom out from the video.", icon: "zoom-out-symbolic", label: "Zoom Out Video", defaults: &["minus"],
    },
    ActionInfo {
        name: "video.zoom-reset", description: "Show all of the video again.", icon: "zoom-fit-best-symbolic", label: "Reset Video Zoom", defaults: &["0"],
    },
    ActionInfo {
        name: "video.pan-left", description: "Move the zoomed part of the video to the left.", icon: "go-previous-symbolic", label: "Pan Video Left", defaults: &["ctrl+shift+left"],
    },
    ActionInfo {
        name: "video.pan-right", description: "Move the zoomed part of the video to the right.", icon: "go-next-symbolic", label: "Pan Video Right", defaults: &["ctrl+shift+right"],
    },
    ActionInfo {
        name: "video.pan-up", description: "Move the zoomed part of the video up.", icon: "go-up-symbolic", label: "Pan Video Up", defaults: &["ctrl+shift+up"],
    },
    ActionInfo {
        name: "video.pan-down", description: "Move the zoomed part of the video down.", icon: "go-down-symbolic", label: "Pan Video Down", defaults: &["ctrl+shift+down"],
    },
    ActionInfo {
        name: "video.timeline-set", description: "From the position on, apply the video's turn, mirror, zoom, crop and speed as they are now, in place of any set for about here.", icon: "document-edit-symbolic", label: "Apply Settings From Here", defaults: &["k"],
    },
    ActionInfo {
        name: "video.timeline-remove", description: "Take away the settings that apply at the position, so the ones before them hold again.", icon: "edit-delete-symbolic", label: "Remove Settings Here", defaults: &["shift+k"],
    },
    ActionInfo {
        name: "video.timeline-clear", description: "Take away every setting set to apply from a time on in the video.", icon: "edit-clear-all-symbolic", label: "Clear Timed Settings", defaults: &["ctrl+shift+k"],
    },
    ActionInfo {
        name: "video.timeline-previous", description: "Skip to the previous time settings apply from.", icon: "media-skip-backward-symbolic", label: "Previous Timed Settings", defaults: &["j"],
    },
    ActionInfo {
        name: "video.timeline-next", description: "Skip to the next time settings apply from.", icon: "media-skip-forward-symbolic", label: "Next Timed Settings", defaults: &["l"],
    },
    ActionInfo {
        name: "video.set-thumbnail-frame", description: "Make the frame the video shows now the one its thumbnail is made from.", icon: "view-grid-symbolic", label: "Set Thumbnail Frame", defaults: &[],
    },
    ActionInfo {
        name: "video.repeat", description: "Play the video again from the start when it ends, or stop repeating it.", icon: "media-playlist-repeat-symbolic", label: "Toggle Repeat", defaults: &["r"],
    },
    ActionInfo {
        name: "video.auto-advance", description: "Run a slideshow: show the next item in the folder or the playlist when a video ends or a picture has been up for the slideshow delay, or stop doing so.", icon: "media-skip-forward-symbolic", label: "Toggle Auto-Advance", defaults: &["n"],
    },
];

/// The built-in actions plus the actions plugins contributed this session. The registry starts with the
/// built-ins; plugins register before the binding table is first used, so their defaults are part of it.
pub fn all() -> Vec<&'static ActionInfo> {
    REGISTRY.with(|registry| registry.borrow().clone())
}

/// The catalogue entry for `name`, if there is one.
pub fn find(name: &str) -> Option<&'static ActionInfo> {
    REGISTRY.with(|registry| registry.borrow().iter().copied().find(|action| action.name == name))
}

/// Adds an action contributed by a plugin. The strings are leaked for the process lifetime: actions are few,
/// and every part of the keybinding and menu code treats catalogue entries as `'static`.
pub fn register_plugin_action(
    name: &str,
    label: &str,
    description: &str,
    scope: Scope,
    icon: &str,
    defaults: &[String],
) -> &'static ActionInfo {
    fn leak(text: &str) -> &'static str {
        Box::leak(text.to_owned().into_boxed_str())
    }

    let default_keys: &'static [&'static str] = Box::leak(
        defaults
            .iter()
            .map(|chord| &*leak(chord))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    let action: &'static ActionInfo = Box::leak(Box::new(ActionInfo {
        name: leak(name),
        description: leak(description),
        label: leak(label),
        icon: leak(icon),
        defaults: default_keys,
    }));
    PLUGIN_SCOPES.with(|scopes| {
        scopes.borrow_mut().insert(name.to_owned(), scope);
    });
    REGISTRY.with(|registry| registry.borrow_mut().push(action));
    action
}

thread_local! {
    static REGISTRY: RefCell<Vec<&'static ActionInfo>> = RefCell::new(ACTIONS.iter().collect());
    static PLUGIN_SCOPES: RefCell<HashMap<String, Scope>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contexts(name: &str) -> Vec<Scope> {
        find(name).unwrap_or_else(|| panic!("no action {name}")).contexts().to_vec()
    }

    #[test]
    fn viewer_actions_can_be_bound_for_pictures_and_videos_alone() {
        for name in ["view.next", "view.previous", "view.close", "view.first", "view.last", "view.pin"] {
            assert_eq!(contexts(name), [Scope::Global, Scope::View, Scope::Image, Scope::Video], "{name}");
            assert_eq!(find(name).unwrap().default_context(), Scope::View, "{name} starts in the viewer");
        }
    }

    #[test]
    fn window_actions_can_be_bound_anywhere() {
        for name in ["win.toggle-fullscreen", "win.next-folder", "win.playlist-next", "win.playlist-toggle", "app.quit", "script", "browse.toggle-hidden"] {
            assert_eq!(contexts(name), Scope::ALL, "{name}");
        }
        assert_eq!(find("script").unwrap().default_context(), Scope::Global);
        assert_eq!(find("browse.toggle-hidden").unwrap().default_context(), Scope::Browse, "it still starts in browsing");
    }

    #[test]
    fn actions_that_need_what_they_act_on_stay_where_it_is() {
        assert_eq!(contexts("browse.cursor-up"), [Scope::Global, Scope::Browse]);
        assert_eq!(contexts("image.zoom-in"), [Scope::Global, Scope::Image]);
        assert_eq!(contexts("video.mute"), [Scope::Global, Scope::Video]);
        assert_eq!(contexts("video.toggle-svp"), [Scope::Global, Scope::Video]);
    }

    #[test]
    fn playlist_actions_are_bound_in_the_playlist_or_globally() {
        for name in ["playlist.cursor-up", "playlist.move-top", "playlist.toggle-search", "playlist.focus-search"] {
            assert_eq!(contexts(name), [Scope::Global, Scope::Playlist], "{name}");
            assert_eq!(find(name).unwrap().default_context(), Scope::Playlist, "{name}");
        }
    }

    #[test]
    fn tree_actions_are_bound_in_the_tree_or_globally() {
        for name in ["tree.cursor-up", "tree.activate", "tree.collapse"] {
            assert_eq!(contexts(name), [Scope::Global, Scope::Tree], "{name}");
            assert_eq!(find(name).unwrap().default_context(), Scope::Tree, "{name}");
        }
        assert_eq!(contexts("win.focus-cycle"), Scope::ALL);
    }

    #[test]
    fn the_volume_actions_choose_the_video_or_the_application() {
        for name in ["video.volume", "video.volume-up", "video.volume-down"] {
            let info = target_info(name).unwrap_or_else(|| panic!("{name} takes a target"));
            assert_eq!(info.default, "video");
            assert!(info.has("application") && info.has("video") && !info.has("elsewhere"));
            assert_eq!(info.title(Some("application")), "Application");
            assert_eq!(info.title(None), "Current Video");
        }
        assert!(target_info("video.mute").is_none());

        let aspect = target_info("video.set-aspect").expect("the aspect action takes a ratio");
        assert_eq!(aspect.default, "auto");
        assert!(aspect.has("16:9") && aspect.has("21:9") && !aspect.has("elsewhere"));
        assert_eq!(aspect.title(Some("16:9")), "16:9");
    }

    #[test]
    fn every_action_starts_in_a_context_it_can_be_bound_in() {
        for action in ACTIONS {
            assert!(action.contexts().contains(&action.default_context()), "{}", action.name);
            assert_eq!(action.contexts().first(), Some(&Scope::Global), "{} can always be global", action.name);
        }
    }
}
