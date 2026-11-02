//! The catalogue of bindable actions. Each action has a name the window activates, a label for the editor,
//! the contexts it can be bound in, and its default chords.

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
}

impl Scope {
    /// The scope's id in the IPC and the keybindings file: `global`, `browse`, `view`, `image` or `video`.
    pub fn id(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Browse => "browse",
            Scope::View => "view",
            Scope::Image => "image",
            Scope::Video => "video",
        }
    }

    /// Every scope, in the order the editor offers them.
    pub const ALL: [Scope; 5] = [Scope::Global, Scope::Browse, Scope::View, Scope::Image, Scope::Video];

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
        }
    }

    /// How specific the scope is: deeper scopes win when both claim a chord.
    pub fn depth(self) -> u8 {
        match self {
            Scope::Global => 0,
            Scope::Browse | Scope::View => 1,
            Scope::Image | Scope::Video => 2,
        }
    }

    /// Whether a chord bound in one scope could also be pressed in the other, so binding it in both is a
    /// conflict. Global overlaps everything; the viewer overlaps the picture and video scopes inside it.
    pub fn overlaps(self, other: Scope) -> bool {
        self == other
            || self == Scope::Global
            || other == Scope::Global
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
    /// The contexts the action can be bound in: Global, then the action's own context. A binding picks one of
    /// them, and a new binding gets the last, which is the action's own.
    pub contexts: &'static [Scope],
    pub defaults: &'static [&'static str],
}

/// The action that runs a script. Each binding of it names the script it runs.
pub const SCRIPT_ACTION: &str = "script";

impl ActionInfo {
    /// Whether each binding of the action names a script to run.
    pub fn takes_script(&self) -> bool {
        self.name == SCRIPT_ACTION
    }

    /// The context a binding of the action gets unless it picks another: the action's own.
    pub fn default_context(&self) -> Scope {
        self.contexts.last().copied().unwrap_or(Scope::Global)
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
        }
    }
}

/// The value the action `name` takes from its bindings, if it takes one.
pub fn value_info(name: &str) -> Option<ValueInfo> {
    match name {
        "video.seek" => Some(ValueInfo { label: "Seconds", unit: Unit::Seconds, min: -3600.0, max: 3600.0, step: 1.0, digits: 1, default: 10.0 }),
        "video.volume" => Some(ValueInfo { label: "Change", unit: Unit::Percent, min: -100.0, max: 100.0, step: 1.0, digits: 0, default: 5.0 }),
        "video.set-speed" => Some(ValueInfo { label: "Speed", unit: Unit::Times, min: MIN_SPEED, max: MAX_SPEED, step: 0.25, digits: 2, default: 1.0 }),
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

/// Every bindable action, in the order the editor lists them. The names, labels, icons and default keys
/// follow pyimgv-gtk's catalogue. Actions that need a feature rsimgv does not have yet are left out.
pub const ACTIONS: &[ActionInfo] = &[
    ActionInfo {
        name: "app.quit", description: "Close the application.", icon: "application-exit-symbolic", label: "Quit", contexts: &[Scope::Global], defaults: &["ctrl+q"],
    },
    ActionInfo {
        name: "win.settings", description: "Open the settings window.", icon: "preferences-system-symbolic", label: "Settings", contexts: &[Scope::Global], defaults: &["ctrl+comma"],
    },
    ActionInfo {
        name: "win.settings-keybindings", description: "Open the settings on the Keybindings page.", icon: "preferences-desktop-keyboard-symbolic", label: "Edit Keybindings", contexts: &[Scope::Global], defaults: &["ctrl+shift+comma"],
    },
    ActionInfo {
        name: "win.open-folder", description: "Choose a folder to browse in this window.", icon: "folder-open-symbolic", label: "Open Folder", contexts: &[Scope::Global], defaults: &["ctrl+o"],
    },
    ActionInfo {
        name: "win.edit-location", description: "Type a path to open, in the path bar.", icon: "document-edit-symbolic", label: "Edit Location", contexts: &[Scope::Global], defaults: &["ctrl+l"],
    },
    ActionInfo {
        name: "win.navigate-back", description: "Go back to the folder shown before this one.", icon: "go-previous-symbolic", label: "Back", contexts: &[Scope::Global], defaults: &["alt+left"],
    },
    ActionInfo {
        name: "win.navigate-forward", description: "Go forward to the folder after this one in the history.", icon: "go-next-symbolic", label: "Forward", contexts: &[Scope::Global], defaults: &["alt+right"],
    },
    ActionInfo {
        name: "win.navigate-up", description: "Go to the parent folder.", icon: "go-up-symbolic", label: "Go Up", contexts: &[Scope::Global], defaults: &["alt+up"],
    },
    ActionInfo {
        name: "win.next-folder", description: "Go to the next folder in depth-first order of the folder tree. In the viewer, go to the next folder with a picture or video and show its first one.", icon: "go-down-symbolic", label: "Next Folder", contexts: &[Scope::Global], defaults: &[],
    },
    ActionInfo {
        name: "win.previous-folder", description: "Go to the previous folder in depth-first order of the folder tree. In the viewer, go to the previous folder with a picture or video and show its first one.", icon: "go-up-symbolic", label: "Previous Folder", contexts: &[Scope::Global], defaults: &[],
    },
    ActionInfo {
        name: "win.next-media-folder", description: "Go to the next folder with a picture or video, and select its first one.", icon: "go-down-symbolic", label: "Next Media Folder", contexts: &[Scope::Global], defaults: &[],
    },
    ActionInfo {
        name: "win.previous-media-folder", description: "Go to the previous folder with a picture or video, and select its last one.", icon: "go-up-symbolic", label: "Previous Media Folder", contexts: &[Scope::Global], defaults: &[],
    },
    ActionInfo {
        name: "win.bookmark-folder", description: "Bookmark the folder shown, under a name you choose.", icon: "bookmark-new-symbolic", label: "Bookmark Folder", contexts: &[Scope::Global], defaults: &["ctrl+d"],
    },
    ActionInfo {
        name: "win.toggle-bookmarks", description: "Expand or collapse the bookmarks list in the sidebar.", icon: "user-bookmarks-symbolic", label: "Toggle Bookmarks", contexts: &[Scope::Global], defaults: &[],
    },
    ActionInfo {
        name: "win.toggle-sidebar", description: "Show or hide the sidebar.", icon: "sidebar-show-symbolic", label: "Toggle Sidebar", contexts: &[Scope::Global], defaults: &["f9"],
    },
    ActionInfo {
        name: "win.toggle-fullscreen", description: "Switch the window between fullscreen and windowed.", icon: "view-fullscreen-symbolic", label: "Fullscreen", contexts: &[Scope::Global], defaults: &["f11"],
    },
    ActionInfo {
        name: SCRIPT_ACTION, description: "Run one of the scripts from the Scripts page on the item under the cursor while browsing, or on the item the viewer shows. Each binding picks its script.", icon: "utilities-terminal-symbolic", label: "Run Script", contexts: &[Scope::Browse, Scope::View, Scope::Global], defaults: &[],
    },
    ActionInfo {
        name: "browse.cursor-up", description: "Move the cursor up one row.", icon: "go-up-symbolic", label: "Move Up", contexts: &[Scope::Global, Scope::Browse], defaults: &["up", "w"],
    },
    ActionInfo {
        name: "browse.cursor-down", description: "Move the cursor down one row.", icon: "go-down-symbolic", label: "Move Down", contexts: &[Scope::Global, Scope::Browse], defaults: &["down", "s"],
    },
    ActionInfo {
        name: "browse.cursor-previous", description: "Move the cursor to the item before it.", icon: "go-previous-symbolic", label: "Move Left", contexts: &[Scope::Global, Scope::Browse], defaults: &["left", "a"],
    },
    ActionInfo {
        name: "browse.cursor-next", description: "Move the cursor to the item after it.", icon: "go-next-symbolic", label: "Move Right", contexts: &[Scope::Global, Scope::Browse], defaults: &["right", "d"],
    },
    ActionInfo {
        name: "browse.cursor-page-up", description: "Move the cursor up one page.", icon: "pan-up-symbolic", label: "Page Up", contexts: &[Scope::Global, Scope::Browse], defaults: &["page_up"],
    },
    ActionInfo {
        name: "browse.cursor-page-down", description: "Move the cursor down one page.", icon: "pan-down-symbolic", label: "Page Down", contexts: &[Scope::Global, Scope::Browse], defaults: &["page_down"],
    },
    ActionInfo {
        name: "browse.first", description: "Move the cursor to the first item.", icon: "go-first-symbolic", label: "First Item", contexts: &[Scope::Global, Scope::Browse], defaults: &["home"],
    },
    ActionInfo {
        name: "browse.last", description: "Move the cursor to the last item.", icon: "go-last-symbolic", label: "Last Item", contexts: &[Scope::Global, Scope::Browse], defaults: &["end"],
    },
    ActionInfo {
        name: "browse.select-all", description: "Mark every item in the folder, to act on them together.", icon: "edit-select-all-symbolic", label: "Select All", contexts: &[Scope::Global, Scope::Browse], defaults: &["ctrl+a"],
    },
    ActionInfo {
        name: "browse.open", description: "Open the item under the cursor: a folder is browsed, a picture or video is shown in the viewer.", icon: "document-open-symbolic", label: "Open", contexts: &[Scope::Global, Scope::Browse], defaults: &["e", "return"],
    },
    ActionInfo {
        name: "browse.show-tooltip", description: "Show the description of the item under the cursor, which otherwise opens only when the pointer rests over an item.", icon: "dialog-information-symbolic", label: "Show Tooltip", contexts: &[Scope::Global, Scope::Browse], defaults: &[],
    },
    ActionInfo {
        name: "browse.up-directory", description: "Go to the parent folder.", icon: "go-up-symbolic", label: "Up Directory", contexts: &[Scope::Global, Scope::Browse], defaults: &["q", "backspace"],
    },
    ActionInfo {
        name: "browse.thumbnail-larger", description: "Make the items in the browse view larger.", icon: "zoom-in-symbolic", label: "Larger Thumbnails", contexts: &[Scope::Global, Scope::Browse], defaults: &["ctrl+shift+equal", "ctrl+equal"],
    },
    ActionInfo {
        name: "browse.thumbnail-smaller", description: "Make the items in the browse view smaller.", icon: "zoom-out-symbolic", label: "Smaller Thumbnails", contexts: &[Scope::Global, Scope::Browse], defaults: &["ctrl+minus"],
    },
    ActionInfo {
        name: "browse.toggle-hidden", description: "Show or hide hidden files and folders.", icon: "view-reveal-symbolic", label: "Toggle Hidden Files", contexts: &[Scope::Global, Scope::Browse], defaults: &["ctrl+h"],
    },
    ActionInfo {
        name: "browse.toggle-media-only", description: "Show only pictures and videos, or every file.", icon: "video-x-generic-symbolic", label: "Toggle Media Only", contexts: &[Scope::Global, Scope::Browse], defaults: &["ctrl+m"],
    },
    ActionInfo {
        name: "browse.toggle-filename", description: "Show or hide each item's filename.", icon: "insert-text-symbolic", label: "Toggle Filename", contexts: &[Scope::Global, Scope::Browse], defaults: &[],
    },
    ActionInfo {
        name: "browse.toggle-item-info", description: "Show or hide each item's information under its name.", icon: "dialog-information-symbolic", label: "Toggle Item Info", contexts: &[Scope::Global, Scope::Browse], defaults: &[],
    },
    ActionInfo {
        name: "view.previous", description: "Show the picture or video before this one in the folder.", icon: "go-previous-symbolic", label: "Previous Item", contexts: &[Scope::Global, Scope::View], defaults: &["comma"],
    },
    ActionInfo {
        name: "view.next", description: "Show the picture or video after this one in the folder.", icon: "go-next-symbolic", label: "Next Item", contexts: &[Scope::Global, Scope::View], defaults: &["period"],
    },
    ActionInfo {
        name: "view.first", description: "Show the first picture or video in the folder.", icon: "go-first-symbolic", label: "First Item", contexts: &[Scope::Global, Scope::View], defaults: &["shift+comma"],
    },
    ActionInfo {
        name: "view.last", description: "Show the last picture or video in the folder.", icon: "go-last-symbolic", label: "Last Item", contexts: &[Scope::Global, Scope::View], defaults: &["shift+period"],
    },
    ActionInfo {
        name: "view.close", description: "Close the viewer and return to the browse view.", icon: "window-close-symbolic", label: "Close Viewer", contexts: &[Scope::Global, Scope::View], defaults: &["q", "escape"],
    },
    ActionInfo {
        name: "view.pin", description: "Keep the viewer's header above the picture in fullscreen, and a video's controls shown.", icon: "view-pin-symbolic", label: "Pin Viewer Header", contexts: &[Scope::Global, Scope::View], defaults: &["p"],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar", description: "Show or hide the viewer's thumbnail bar.", icon: "view-grid-symbolic", label: "Toggle Thumbnail Bar", contexts: &[Scope::Global, Scope::View], defaults: &["t"],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar-pin", description: "Keep the floating thumbnail bar shown, or let it hide until the pointer comes near.", icon: "view-pin-symbolic", label: "Pin Thumbnail Bar", contexts: &[Scope::Global, Scope::View], defaults: &[],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar-overlay", description: "Float the thumbnail bar over the picture, or put it beside the picture.", icon: "window-restore-symbolic", label: "Toggle Thumbnail Bar Overlay", contexts: &[Scope::Global, Scope::View], defaults: &[],
    },
    ActionInfo {
        name: "view.toggle-status-bar", description: "Show or hide the status bar under the viewer's item.", icon: "view-continuous-symbolic", label: "Toggle Status Bar", contexts: &[Scope::Global, Scope::View], defaults: &[],
    },
    ActionInfo {
        name: "image.zoom-in", description: "Enlarge the picture.", icon: "zoom-in-symbolic", label: "Zoom In", contexts: &[Scope::Global, Scope::Image], defaults: &["shift+equal", "equal"],
    },
    ActionInfo {
        name: "image.zoom-out", description: "Shrink the picture.", icon: "zoom-out-symbolic", label: "Zoom Out", contexts: &[Scope::Global, Scope::Image], defaults: &["minus"],
    },
    ActionInfo {
        name: "image.zoom-in-at-pointer", description: "Enlarge the picture, keeping the point under the pointer in place.", icon: "zoom-in-symbolic", label: "Zoom In at Pointer", contexts: &[Scope::Global, Scope::Image], defaults: &[],
    },
    ActionInfo {
        name: "image.zoom-out-at-pointer", description: "Shrink the picture, keeping the point under the pointer in place.", icon: "zoom-out-symbolic", label: "Zoom Out at Pointer", contexts: &[Scope::Global, Scope::Image], defaults: &[],
    },
    ActionInfo {
        name: "image.fit-cycle", description: "Switch to the next fit mode: window, width, height, fill, then original size.", icon: "zoom-fit-best-symbolic", label: "Cycle Fit Mode", contexts: &[Scope::Global, Scope::Image], defaults: &["f"],
    },
    ActionInfo {
        name: "image.fit-window", description: "Scale the picture to fit the window.", icon: "zoom-fit-best-symbolic", label: "Fit Window", contexts: &[Scope::Global, Scope::Image], defaults: &["1"],
    },
    ActionInfo {
        name: "image.fit-width", description: "Scale the picture to the width of the window.", icon: "zoom-fit-best-symbolic", label: "Fit Width", contexts: &[Scope::Global, Scope::Image], defaults: &["2"],
    },
    ActionInfo {
        name: "image.fit-height", description: "Scale the picture to the height of the window.", icon: "zoom-fit-best-symbolic", label: "Fit Height", contexts: &[Scope::Global, Scope::Image], defaults: &["3"],
    },
    ActionInfo {
        name: "image.fit-fill", description: "Scale the picture to cover the window, cropping what overhangs.", icon: "zoom-fit-best-symbolic", label: "Fill Window", contexts: &[Scope::Global, Scope::Image], defaults: &["5"],
    },
    ActionInfo {
        name: "image.fit-original", description: "Show the picture at its own size.", icon: "zoom-original-symbolic", label: "Original Size", contexts: &[Scope::Global, Scope::Image], defaults: &["4"],
    },
    ActionInfo {
        name: "image.pan-up", description: "Move the picture up within the window.", icon: "go-up-symbolic", label: "Pan Up", contexts: &[Scope::Global, Scope::Image], defaults: &["up"],
    },
    ActionInfo {
        name: "image.pan-down", description: "Move the picture down within the window.", icon: "go-down-symbolic", label: "Pan Down", contexts: &[Scope::Global, Scope::Image], defaults: &["down"],
    },
    ActionInfo {
        name: "image.pan-left", description: "Move the picture left within the window.", icon: "go-previous-symbolic", label: "Pan Left", contexts: &[Scope::Global, Scope::Image], defaults: &["left"],
    },
    ActionInfo {
        name: "image.pan-right", description: "Move the picture right within the window.", icon: "go-next-symbolic", label: "Pan Right", contexts: &[Scope::Global, Scope::Image], defaults: &["right"],
    },
    ActionInfo {
        name: "video.play-pause", description: "Play the video, or pause it while it plays.", icon: "media-playback-start-symbolic", label: "Play / Pause", contexts: &[Scope::Global, Scope::Video], defaults: &["space"],
    },
    ActionInfo {
        name: "video.seek-forward", description: "Skip forward 5 seconds.", icon: "media-seek-forward-symbolic", label: "Seek Forward", contexts: &[Scope::Global, Scope::Video], defaults: &["right"],
    },
    ActionInfo {
        name: "video.seek-backward", description: "Skip back 5 seconds.", icon: "media-seek-backward-symbolic", label: "Seek Backward", contexts: &[Scope::Global, Scope::Video], defaults: &["left"],
    },
    ActionInfo {
        name: "video.seek", description: "Move the playback position by the number of seconds bound; a negative number goes back.", icon: "media-seek-forward-symbolic", label: "Seek", contexts: &[Scope::Global, Scope::Video], defaults: &[],
    },
    ActionInfo {
        name: "video.set-speed", description: "Play at the speed bound, such as 2 for double speed or 0.5 for half; 1 is normal.", icon: "power-profile-performance-symbolic", label: "Set Speed", contexts: &[Scope::Global, Scope::Video], defaults: &[],
    },
    ActionInfo {
        name: "video.adjust-speed", description: "Change the playback speed by the amount bound, such as 0.25 faster; a negative amount slows it.", icon: "power-profile-balanced-symbolic", label: "Adjust Speed", contexts: &[Scope::Global, Scope::Video], defaults: &[],
    },
    ActionInfo {
        name: "video.volume-up", description: "Raise the volume.", icon: "audio-volume-high-symbolic", label: "Volume Up", contexts: &[Scope::Global, Scope::Video], defaults: &["up"],
    },
    ActionInfo {
        name: "video.volume-down", description: "Lower the volume.", icon: "audio-volume-low-symbolic", label: "Volume Down", contexts: &[Scope::Global, Scope::Video], defaults: &["down"],
    },
    ActionInfo {
        name: "video.volume", description: "Change the volume by the percentage bound; a negative number turns it down.", icon: "audio-volume-high-symbolic", label: "Adjust Volume", contexts: &[Scope::Global, Scope::Video], defaults: &[],
    },
    ActionInfo {
        name: "video.mute", description: "Mute the sound, or unmute it.", icon: "audio-volume-muted-symbolic", label: "Mute", contexts: &[Scope::Global, Scope::Video], defaults: &["m"],
    },
    ActionInfo {
        name: "video.repeat", description: "Play the video again from the start when it ends, or stop repeating it.", icon: "media-playlist-repeat-symbolic", label: "Toggle Repeat", contexts: &[Scope::Global, Scope::Video], defaults: &["r"],
    },
    ActionInfo {
        name: "video.auto-advance", description: "When a video ends, show the next item in the folder, or stop doing so.", icon: "media-skip-forward-symbolic", label: "Toggle Auto-Advance", contexts: &[Scope::Global, Scope::Video], defaults: &["n"],
    },
];

/// The catalogue entry for `name`, if there is one.
pub fn find(name: &str) -> Option<&'static ActionInfo> {
    ACTIONS.iter().find(|action| action.name == name)
}
