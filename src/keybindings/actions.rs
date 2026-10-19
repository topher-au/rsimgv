//! The catalogue of bindable actions. Each action has a name the window activates, a label for the editor,
//! the scope it applies in, and its default chords.

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
    pub scope: Scope,
    pub defaults: &'static [&'static str],
}

/// Every bindable action, in the order the editor lists them. The names, labels, icons and default keys
/// follow pyimgv-gtk's catalogue. Actions that need a feature rsimgv does not have yet are left out.
pub const ACTIONS: &[ActionInfo] = &[
    ActionInfo {
        name: "app.quit", description: "Close the application.", icon: "application-exit-symbolic", label: "Quit", scope: Scope::Global, defaults: &["ctrl+q"],
    },
    ActionInfo {
        name: "win.settings", description: "Open the settings window.", icon: "preferences-system-symbolic", label: "Settings", scope: Scope::Global, defaults: &["ctrl+comma"],
    },
    ActionInfo {
        name: "win.settings-keybindings", description: "Open the settings on the Keybindings page.", icon: "preferences-desktop-keyboard-symbolic", label: "Edit Keybindings", scope: Scope::Global, defaults: &["ctrl+shift+comma"],
    },
    ActionInfo {
        name: "win.open-folder", description: "Choose a folder to browse in this window.", icon: "folder-open-symbolic", label: "Open Folder", scope: Scope::Global, defaults: &["ctrl+o"],
    },
    ActionInfo {
        name: "win.edit-location", description: "Type a path to open, in the path bar.", icon: "document-edit-symbolic", label: "Edit Location", scope: Scope::Global, defaults: &["ctrl+l"],
    },
    ActionInfo {
        name: "win.navigate-back", description: "Go back to the folder shown before this one.", icon: "go-previous-symbolic", label: "Back", scope: Scope::Global, defaults: &["alt+left"],
    },
    ActionInfo {
        name: "win.navigate-forward", description: "Go forward to the folder after this one in the history.", icon: "go-next-symbolic", label: "Forward", scope: Scope::Global, defaults: &["alt+right"],
    },
    ActionInfo {
        name: "win.navigate-up", description: "Go to the parent folder.", icon: "go-up-symbolic", label: "Go Up", scope: Scope::Global, defaults: &["alt+up"],
    },
    ActionInfo {
        name: "win.next-folder", description: "Go to the next folder in depth-first order of the folder tree. In the viewer, go to the next folder with a picture or video and show its first one.", icon: "go-down-symbolic", label: "Next Folder", scope: Scope::Global, defaults: &[],
    },
    ActionInfo {
        name: "win.previous-folder", description: "Go to the previous folder in depth-first order of the folder tree. In the viewer, go to the previous folder with a picture or video and show its first one.", icon: "go-up-symbolic", label: "Previous Folder", scope: Scope::Global, defaults: &[],
    },
    ActionInfo {
        name: "win.next-media-folder", description: "Go to the next folder with a picture or video, and select its first one.", icon: "go-down-symbolic", label: "Next Media Folder", scope: Scope::Global, defaults: &[],
    },
    ActionInfo {
        name: "win.previous-media-folder", description: "Go to the previous folder with a picture or video, and select its last one.", icon: "go-up-symbolic", label: "Previous Media Folder", scope: Scope::Global, defaults: &[],
    },
    ActionInfo {
        name: "win.bookmark-folder", description: "Bookmark the folder shown, under a name you choose.", icon: "bookmark-new-symbolic", label: "Bookmark Folder", scope: Scope::Global, defaults: &["ctrl+d"],
    },
    ActionInfo {
        name: "win.toggle-bookmarks", description: "Expand or collapse the bookmarks list in the sidebar.", icon: "user-bookmarks-symbolic", label: "Toggle Bookmarks", scope: Scope::Global, defaults: &[],
    },
    ActionInfo {
        name: "win.toggle-sidebar", description: "Show or hide the sidebar.", icon: "sidebar-show-symbolic", label: "Toggle Sidebar", scope: Scope::Global, defaults: &["f9"],
    },
    ActionInfo {
        name: "win.toggle-fullscreen", description: "Switch the window between fullscreen and windowed.", icon: "view-fullscreen-symbolic", label: "Fullscreen", scope: Scope::Global, defaults: &["f11"],
    },
    ActionInfo {
        name: "browse.cursor-up", description: "Move the cursor up one row.", icon: "go-up-symbolic", label: "Move Up", scope: Scope::Browse, defaults: &["up", "w"],
    },
    ActionInfo {
        name: "browse.cursor-down", description: "Move the cursor down one row.", icon: "go-down-symbolic", label: "Move Down", scope: Scope::Browse, defaults: &["down", "s"],
    },
    ActionInfo {
        name: "browse.cursor-previous", description: "Move the cursor to the item before it.", icon: "go-previous-symbolic", label: "Move Left", scope: Scope::Browse, defaults: &["left", "a"],
    },
    ActionInfo {
        name: "browse.cursor-next", description: "Move the cursor to the item after it.", icon: "go-next-symbolic", label: "Move Right", scope: Scope::Browse, defaults: &["right", "d"],
    },
    ActionInfo {
        name: "browse.cursor-page-up", description: "Move the cursor up one page.", icon: "pan-up-symbolic", label: "Page Up", scope: Scope::Browse, defaults: &["page_up"],
    },
    ActionInfo {
        name: "browse.cursor-page-down", description: "Move the cursor down one page.", icon: "pan-down-symbolic", label: "Page Down", scope: Scope::Browse, defaults: &["page_down"],
    },
    ActionInfo {
        name: "browse.first", description: "Move the cursor to the first item.", icon: "go-first-symbolic", label: "First Item", scope: Scope::Browse, defaults: &["home"],
    },
    ActionInfo {
        name: "browse.last", description: "Move the cursor to the last item.", icon: "go-last-symbolic", label: "Last Item", scope: Scope::Browse, defaults: &["end"],
    },
    ActionInfo {
        name: "browse.open", description: "Open the item under the cursor: a folder is browsed, a picture or video is shown in the viewer.", icon: "document-open-symbolic", label: "Open", scope: Scope::Browse, defaults: &["e", "return"],
    },
    ActionInfo {
        name: "browse.up-directory", description: "Go to the parent folder.", icon: "go-up-symbolic", label: "Up Directory", scope: Scope::Browse, defaults: &["q", "backspace"],
    },
    ActionInfo {
        name: "browse.thumbnail-larger", description: "Make the items in the browse view larger.", icon: "zoom-in-symbolic", label: "Larger Thumbnails", scope: Scope::Browse, defaults: &["ctrl+shift+equal", "ctrl+equal"],
    },
    ActionInfo {
        name: "browse.thumbnail-smaller", description: "Make the items in the browse view smaller.", icon: "zoom-out-symbolic", label: "Smaller Thumbnails", scope: Scope::Browse, defaults: &["ctrl+minus"],
    },
    ActionInfo {
        name: "browse.toggle-hidden", description: "Show or hide hidden files and folders.", icon: "view-reveal-symbolic", label: "Toggle Hidden Files", scope: Scope::Browse, defaults: &["ctrl+h"],
    },
    ActionInfo {
        name: "browse.toggle-media-only", description: "Show only pictures and videos, or every file.", icon: "video-x-generic-symbolic", label: "Toggle Media Only", scope: Scope::Browse, defaults: &["ctrl+m"],
    },
    ActionInfo {
        name: "view.previous", description: "Show the picture or video before this one in the folder.", icon: "go-previous-symbolic", label: "Previous Item", scope: Scope::View, defaults: &["comma"],
    },
    ActionInfo {
        name: "view.next", description: "Show the picture or video after this one in the folder.", icon: "go-next-symbolic", label: "Next Item", scope: Scope::View, defaults: &["period"],
    },
    ActionInfo {
        name: "view.first", description: "Show the first picture or video in the folder.", icon: "go-first-symbolic", label: "First Item", scope: Scope::View, defaults: &["shift+comma"],
    },
    ActionInfo {
        name: "view.last", description: "Show the last picture or video in the folder.", icon: "go-last-symbolic", label: "Last Item", scope: Scope::View, defaults: &["shift+period"],
    },
    ActionInfo {
        name: "view.close", description: "Close the viewer and return to the browse view.", icon: "window-close-symbolic", label: "Close Viewer", scope: Scope::View, defaults: &["q", "escape"],
    },
    ActionInfo {
        name: "view.pin", description: "Keep the viewer's header above the picture in fullscreen, and a video's controls shown.", icon: "view-pin-symbolic", label: "Pin Viewer Header", scope: Scope::View, defaults: &["p"],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar", description: "Show or hide the viewer's thumbnail bar.", icon: "view-grid-symbolic", label: "Toggle Thumbnail Bar", scope: Scope::View, defaults: &["t"],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar-pin", description: "Keep the floating thumbnail bar shown, or let it hide until the pointer comes near.", icon: "view-pin-symbolic", label: "Pin Thumbnail Bar", scope: Scope::View, defaults: &[],
    },
    ActionInfo {
        name: "view.toggle-thumbnail-bar-overlay", description: "Float the thumbnail bar over the picture, or put it beside the picture.", icon: "window-restore-symbolic", label: "Toggle Thumbnail Bar Overlay", scope: Scope::View, defaults: &[],
    },
    ActionInfo {
        name: "image.zoom-in", description: "Enlarge the picture.", icon: "zoom-in-symbolic", label: "Zoom In", scope: Scope::Image, defaults: &["shift+equal", "equal"],
    },
    ActionInfo {
        name: "image.zoom-out", description: "Shrink the picture.", icon: "zoom-out-symbolic", label: "Zoom Out", scope: Scope::Image, defaults: &["minus"],
    },
    ActionInfo {
        name: "image.zoom-in-at-pointer", description: "Enlarge the picture, keeping the point under the pointer in place.", icon: "zoom-in-symbolic", label: "Zoom In at Pointer", scope: Scope::Image, defaults: &[],
    },
    ActionInfo {
        name: "image.zoom-out-at-pointer", description: "Shrink the picture, keeping the point under the pointer in place.", icon: "zoom-out-symbolic", label: "Zoom Out at Pointer", scope: Scope::Image, defaults: &[],
    },
    ActionInfo {
        name: "image.fit-cycle", description: "Switch to the next fit mode: window, width, height, then original size.", icon: "zoom-fit-best-symbolic", label: "Cycle Fit Mode", scope: Scope::Image, defaults: &["f"],
    },
    ActionInfo {
        name: "image.fit-window", description: "Scale the picture to fit the window.", icon: "zoom-fit-best-symbolic", label: "Fit Window", scope: Scope::Image, defaults: &["1"],
    },
    ActionInfo {
        name: "image.fit-width", description: "Scale the picture to the width of the window.", icon: "zoom-fit-best-symbolic", label: "Fit Width", scope: Scope::Image, defaults: &["2"],
    },
    ActionInfo {
        name: "image.fit-height", description: "Scale the picture to the height of the window.", icon: "zoom-fit-best-symbolic", label: "Fit Height", scope: Scope::Image, defaults: &["3"],
    },
    ActionInfo {
        name: "image.fit-original", description: "Show the picture at its own size.", icon: "zoom-original-symbolic", label: "Original Size", scope: Scope::Image, defaults: &["4"],
    },
    ActionInfo {
        name: "image.pan-up", description: "Move the picture up within the window.", icon: "go-up-symbolic", label: "Pan Up", scope: Scope::Image, defaults: &["up"],
    },
    ActionInfo {
        name: "image.pan-down", description: "Move the picture down within the window.", icon: "go-down-symbolic", label: "Pan Down", scope: Scope::Image, defaults: &["down"],
    },
    ActionInfo {
        name: "image.pan-left", description: "Move the picture left within the window.", icon: "go-previous-symbolic", label: "Pan Left", scope: Scope::Image, defaults: &["left"],
    },
    ActionInfo {
        name: "image.pan-right", description: "Move the picture right within the window.", icon: "go-next-symbolic", label: "Pan Right", scope: Scope::Image, defaults: &["right"],
    },
    ActionInfo {
        name: "video.play-pause", description: "Play the video, or pause it while it plays.", icon: "media-playback-start-symbolic", label: "Play / Pause", scope: Scope::Video, defaults: &["space"],
    },
    ActionInfo {
        name: "video.seek-forward", description: "Skip forward 5 seconds.", icon: "media-seek-forward-symbolic", label: "Seek Forward", scope: Scope::Video, defaults: &["right"],
    },
    ActionInfo {
        name: "video.seek-backward", description: "Skip back 5 seconds.", icon: "media-seek-backward-symbolic", label: "Seek Backward", scope: Scope::Video, defaults: &["left"],
    },
    ActionInfo {
        name: "video.volume-up", description: "Raise the volume.", icon: "audio-volume-high-symbolic", label: "Volume Up", scope: Scope::Video, defaults: &["up"],
    },
    ActionInfo {
        name: "video.volume-down", description: "Lower the volume.", icon: "audio-volume-low-symbolic", label: "Volume Down", scope: Scope::Video, defaults: &["down"],
    },
    ActionInfo {
        name: "video.mute", description: "Mute the sound, or unmute it.", icon: "audio-volume-muted-symbolic", label: "Mute", scope: Scope::Video, defaults: &["m"],
    },
    ActionInfo {
        name: "video.repeat", description: "Play the video again from the start when it ends, or stop repeating it.", icon: "media-playlist-repeat-symbolic", label: "Toggle Repeat", scope: Scope::Video, defaults: &["r"],
    },
    ActionInfo {
        name: "video.auto-advance", description: "When a video ends, show the next item in the folder, or stop doing so.", icon: "media-skip-forward-symbolic", label: "Toggle Auto-Advance", scope: Scope::Video, defaults: &["n"],
    },
];

/// The catalogue entry for `name`, if there is one.
pub fn find(name: &str) -> Option<&'static ActionInfo> {
    ACTIONS.iter().find(|action| action.name == name)
}
