use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::path::{Path, PathBuf};

use adw::subclass::prelude::*;
use gio::prelude::StaticType;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::keybindings::actions::{Scope, DEFAULT_NOTIFICATION_SECONDS, MAX_NOTIFICATION_SECONDS};
use super::browse_view::{InfoField, ListedFile};
use crate::bookmarks::{self, Bookmark};
use crate::playlist::{self, format::{self, Format}};
use super::image_canvas::{Fit, OriginalFocus, Scaling, PAN_STEP};
use super::media_controls::SEEK_STEP;
use super::media_viewer::{side_from_name, FolderEnd, ZoomIndicator, VOLUME_STEP};
use super::key_handler::ActionKeyHandler;
use super::fs_tree::TreeSnapshot;
use super::path_bar::OpenTarget;
use super::{BrowseView, FsTree, HeaderBar, MediaViewer, MessageOverlay, NavigationBar, OverlaySplitView, PathBar, PreferencesDialog, ShownItem};
use super::{Level, Message};
use super::message_overlay::{MessageId, Position};
use super::keybinds_overlay::KeybindsOverlay;
use super::playlist_panel::{stamp, PlaylistPanel, SortColumn};
use crate::fs::metadata::MediaInfo;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rimgv/ui/window.ui")]
    pub struct Window {
        #[template_child]
        pub tree: TemplateChild<FsTree>,
        #[template_child]
        pub split: TemplateChild<OverlaySplitView>,
        /// The window's content: the browse split view, or the viewer, which fills the window.
        #[template_child]
        pub pages: TemplateChild<gtk::Stack>,
        #[template_child]
        pub message_layer: TemplateChild<gtk::Overlay>,
        /// The messages over the window, made once the template is built.
        pub messages: std::cell::OnceCell<MessageOverlay>,
        /// The list of the keybindings in force, made when it is first wanted.
        pub(super) keybinds: std::cell::OnceCell<KeybindsOverlay>,
        /// The message naming the folder the viewer moved to, while it is up, so the next one replaces it.
        pub(super) folder_notice: Cell<Option<MessageId>>,
        /// The message naming the pane the focus moved to, while it is up, so the next one replaces it.
        pub(super) focus_notice: Cell<Option<MessageId>>,
        /// The last message about the playlist, while it is up, so the next one replaces it.
        pub(super) playlist_notice: Cell<Option<MessageId>>,
        #[template_child]
        pub viewer: TemplateChild<MediaViewer>,
        #[template_child]
        pub bookmarks_expander: TemplateChild<gtk::Expander>,
        #[template_child]
        pub bookmarks_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub playlist_split: TemplateChild<OverlaySplitView>,
        /// The window the playlist is in while it is taken out of the window, if it is.
        pub(super) playlist_window: RefCell<Option<adw::Window>>,
        /// The header of that window, which its header style setting changes.
        pub(super) playlist_header: RefCell<Option<adw::HeaderBar>>,
        /// The playlist in the sidebar, made once the template is built.
        pub(super) playlist_panel: std::cell::OnceCell<PlaylistPanel>,
        #[template_child]
        pub filesystem_expander: TemplateChild<gtk::Expander>,
        #[template_child]
        pub filesystem_root_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub filesystem_home_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub sidebar_header_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub content_sidebar_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub content_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub navigation: TemplateChild<NavigationBar>,
        #[template_child]
        pub title_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub path_bar: TemplateChild<PathBar>,
        #[template_child]
        pub location_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub tab_view: TemplateChild<adw::TabView>,
        #[template_child]
        pub tab_bar: TemplateChild<adw::TabBar>,
        /// What each tab shows when it is not the active one, keyed by tab id.
        pub(super) tabs: RefCell<BTreeMap<u64, TabState>>,
        /// The tab whose state is in the widgets now.
        pub(super) active_tab: Cell<Option<u64>>,
        pub(super) next_tab: Cell<u64>,
        /// The folder the window shows. Every navigation goes through `Window::navigate`.
        pub(super) current_path: RefCell<Option<PathBuf>>,
        /// Set while the window pushes a path into its widgets, so the signals that come back are ignored.
        pub(super) updating: Cell<bool>,
        /// Whether the viewer is showing, in place of the browse view.
        pub(super) viewer_open: Cell<bool>,
        /// Whether the viewer is showing the playlist, rather than the folder's items. The viewer then has nothing to
        /// do with the folder the window shows, which stays as it was.
        pub(super) playlist_playing: Cell<bool>,
        /// Whether the playlist's sidebar is open. It is kept as the sidebar opens and closes, and the sidebar is then
        /// put beside whichever page is shown.
        pub(super) playlist_shown: Cell<bool>,
        /// Whether the viewer made the window fullscreen when it opened, so closing it leaves fullscreen again.
        pub(super) viewer_fullscreened: Cell<bool>,
        /// The keybinding handler, detached when the window closes.
        pub(super) key_handler: std::cell::OnceCell<ActionKeyHandler>,
        pub(super) settings: std::cell::OnceCell<gio::Settings>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "RsimgvWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            // The template references these custom types, so they must be registered first.
            FsTree::static_type();
            OverlaySplitView::static_type();
            NavigationBar::static_type();
            PathBar::static_type();
            HeaderBar::static_type();
            BrowseView::static_type();
            MediaViewer::static_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Window {}
    impl WidgetImpl for Window {}
    impl WindowImpl for Window {}
    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

/// Where `Window::open_path` shows a path: in the tab the window is showing, or in a new tab, which is
/// activated or not.
#[derive(Clone, Copy, Debug)]
pub enum Destination {
    CurrentTab,
    NewTab { activate: bool },
}

impl Window {
    pub fn new(app: &adw::Application, settings: &gio::Settings) -> Self {
        let window: Self = glib::Object::builder().property("application", app).build();
        let imp = window.imp();
        let _ = imp.settings.set(settings.clone());
        window.bind_pointer();

        // The window opens at the size and state it had when it was last closed.
        window.set_default_size(settings.int("window-width"), settings.int("window-height"));
        if settings.boolean("window-maximized") {
            window.maximize();
        }
        // Save as the window changes, not only on close. A size change arrives on every step of a
        // drag, so each save waits until the changes stop.
        let pending: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
        let schedule: Rc<dyn Fn(&Window)> = {
            let settings = settings.clone();
            let pending = pending.clone();
            Rc::new(move |window: &Window| schedule_window_save(window, &settings, &pending))
        };
        // KDE's blur is kept behind the viewer's floating parts, each frame, when the setting is on.
        let blur_last = Rc::new(RefCell::new(Vec::new()));
        let weak = window.downgrade();
        window.add_tick_callback(move |_, _| {
            let Some(window) = weak.upgrade() else { return glib::ControlFlow::Break };
            let enabled = window.imp().settings.get().is_some_and(|settings| settings.boolean("kde-effects"));
            let widgets = if enabled { window.imp().viewer.floating_widgets() } else { Vec::new() };
            crate::ui::kde_blur::update(window.upcast_ref(), &widgets, &blur_last);
            glib::ControlFlow::Continue
        });
        window.connect_maximized_notify({
            let schedule = schedule.clone();
            move |window| schedule(window)
        });
        window.connect_realize(move |window| {
            let Some(surface) = window.surface() else { return };
            let weak = window.downgrade();
            let schedule_width = schedule.clone();
            let weak_width = weak.clone();
            surface.connect_width_notify(move |_| {
                if let Some(window) = weak_width.upgrade() {
                    schedule_width(&window);
                }
            });
            let schedule_height = schedule.clone();
            surface.connect_height_notify(move |_| {
                if let Some(window) = weak.upgrade() {
                    schedule_height(&window);
                }
            });
        });
        let settings_on_close = settings.clone();
        window.connect_close_request(move |window| {
            save_window_state(window, &settings_on_close);
            // The item shown keeps where it was left, as leaving it would.
            window.imp().viewer.close();
            if let Some(keys) = window.imp().key_handler.get() {
                keys.uninstall();
            }
            // A playlist in a window of its own closes with the window it belongs to.
            if let Some(detached) = window.imp().playlist_window.take() {
                detached.destroy();
            }
            glib::Propagation::Proceed
        });
        let tree = (*imp.tree).clone();
        let split = (*imp.split).clone();
        // Two toggles show and hide the sidebar: one in the sidebar's header, and one in the main
        // header, which is hidden while the sidebar is open. Both follow the split view's state.
        let toggles = [(*imp.sidebar_header_toggle).clone(), (*imp.content_sidebar_toggle).clone()];
        for toggle in &toggles {
            let split = split.clone();
            toggle.connect_toggled(move |button| split.set_show_sidebar(button.is_active()));
        }
        let content_revealer = (*imp.content_revealer).clone();
        let show_state = move |shown: bool| {
            for toggle in &toggles {
                toggle.set_active(shown);
            }
            content_revealer.set_reveal_child(!shown);
        };
        show_state(split.show_sidebar());
        split.connect_show_sidebar_changed(move |_, shown| show_state(shown));

        if let Err(err) = tree.set_root_path(Path::new("/")) {
            eprintln!("rsimgv: {err:#}");
        }

        // Settings drive the view: the browse view's properties and the tree follow their keys, and
        // the menu's "Show Hidden Directories" item is an action bound to its key.
        settings.bind("collapse-on-navigate", &tree, "collapse-on-navigate").build();
        settings.bind("sidebar-bookmarks-expanded", &*imp.bookmarks_expander, "expanded").build();
        settings.bind("sidebar-filesystem-expanded", &*imp.filesystem_expander, "expanded").build();
        tree.set_show_hidden(settings.boolean("show-hidden"));
        let tree_for_settings = tree.clone();
        settings.connect_changed(Some("playlist-window-headerbar"), {
            let weak = window.downgrade();
            move |settings, _| {
                if let Some(window) = weak.upgrade() {
                    if let Some(header) = window.imp().playlist_header.borrow().as_ref() {
                        apply_playlist_header_style(header, &settings.string("playlist-window-headerbar"));
                    }
                }
            }
        });
        settings.connect_changed(Some("show-hidden"), move |settings, _| {
            tree_for_settings.set_show_hidden(settings.boolean("show-hidden"));
        });
        window.add_action(&settings.create_action("show-hidden"));
        add_sort_action(&window, settings, "sort-by");
        add_sort_action(&window, settings, "sort-order");
        settings.connect_changed(Some("remember-folder-sort"), {
            let weak = window.downgrade();
            move |_, _| {
                if let Some(window) = weak.upgrade() {
                    window.apply_sort_settings();
                }
            }
        });
        add_show_directories_action(&window, settings);

        window.add_context_actions();
        window.add_tab_actions();
        window.add_binding_actions();
        window.add_playlist_actions(settings);
        let keys = ActionKeyHandler::install(&window);
        let _ = window.imp().key_handler.set(keys);
        window.connect_sources();
        window.refresh_bookmarks();
        let panel = PlaylistPanel::new(settings);
        let _ = window.imp().playlist_panel.set(panel);
        window.setup_playlist_splits(settings);
        GtkWindowExt::connect_focus_widget_notify(&window, |window| window.update_handle_highlights());
        if settings.boolean("playlist-detached") {
            window.detach_playlist(settings);
        }
        window.imp().viewer.set_use_mpv(settings.string("video-backend") == "mpv");
        window.imp().viewer.set_mpv_options(&settings.string("mpv-args"));
        crate::mpv::watch_config({
            let weak = window.imp().viewer.downgrade();
            move || {
                if let Some(viewer) = weak.upgrade() {
                    viewer.reload_video();
                }
            }
        });
        settings.connect_changed(Some("mpv-args"), {
            let weak = window.downgrade();
            move |settings, _| {
                if let Some(window) = weak.upgrade() {
                    window.imp().viewer.set_mpv_options(&settings.string("mpv-args"));
                }
            }
        });
        settings.connect_changed(Some("video-backend"), {
            let weak = window.downgrade();
            move |settings, _| {
                if let Some(window) = weak.upgrade() {
                    window.imp().viewer.set_use_mpv(settings.string("video-backend") == "mpv");
                }
            }
        });
        window.apply_viewer_settings(settings);
        for key in VIEWER_SETTINGS {
            settings.connect_changed(Some(key), {
                let weak = window.downgrade();
                move |settings, _| {
                    if let Some(window) = weak.upgrade() {
                        window.apply_viewer_settings(settings);
                    }
                }
            });
        }
        imp.viewer.connect_item_changed({
            let weak = window.downgrade();
            move |_, _| {
                if let Some(window) = weak.upgrade() {
                    window.update_title();
                    window.refresh_keybinds_overlay();
                }
            }
        });
        let width = settings.int("sidebar-width");
        imp.split.set_user_sidebar_width((width > 0).then_some(width));
        // In fullscreen the viewer's header floats over the picture and hides; otherwise it sits above it.
        imp.viewer.set_fullscreen(window.is_fullscreen());
        window.connect_fullscreened_notify(|window| window.imp().viewer.set_fullscreen(window.is_fullscreen()));
        let apply_thumbnail_bar = {
            let weak = window.downgrade();
            move |settings: &gio::Settings, _: &str| {
                let Some(window) = weak.upgrade() else { return };
                window.imp().viewer.set_thumbnail_bar(
                    side_from_name(&settings.string("viewer-thumbnail-bar")),
                    settings.boolean("viewer-thumbnail-bar-shown"),
                    settings.boolean("viewer-thumbnail-bar-overlay"),
                    settings.boolean("viewer-thumbnail-bar-pinned"),
                );
                window.imp().viewer.set_thumbnail_size(settings.int("viewer-thumbnail-bar-size"));
            }
        };
        apply_thumbnail_bar(settings, "viewer-thumbnail-bar");
        for key in [
            "viewer-thumbnail-bar",
            "viewer-thumbnail-bar-shown",
            "viewer-thumbnail-bar-size",
            "viewer-thumbnail-bar-overlay",
            "viewer-thumbnail-bar-pinned",
        ] {
            settings.connect_changed(Some(key), apply_thumbnail_bar.clone());
        }
        imp.viewer.set_preloading(settings.boolean("image-preloading"));
        settings.connect_changed(Some("image-preloading"), {
            let weak = window.downgrade();
            move |settings, key| {
                if let Some(window) = weak.upgrade() {
                    window.imp().viewer.set_preloading(settings.boolean(key));
                }
            }
        });
        imp.viewer.connect_thumbnail_side_requested({
            let settings = settings.clone();
            move |_, side| {
                let _ = settings.set_string("viewer-thumbnail-bar", side);
            }
        });
        imp.viewer.thumbnail_bar().connect_pin_toggled({
            let settings = settings.clone();
            move |_, on| {
                let _ = settings.set_boolean("viewer-thumbnail-bar-pinned", on);
            }
        });
        imp.viewer.thumbnail_bar().connect_overlay_toggled({
            let settings = settings.clone();
            move |_, on| {
                let _ = settings.set_boolean("viewer-thumbnail-bar-overlay", on);
            }
        });
        imp.viewer.connect_thumbnail_size_requested({
            let settings = settings.clone();
            move |_, size| {
                let _ = settings.set_int("viewer-thumbnail-bar-size", size);
            }
        });
        // Ctrl and the wheel over the browse view resize its items, a step per notch; a smooth scroll
        // adds up until it makes a notch.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pending = std::rc::Rc::new(std::cell::Cell::new(0.0_f64));
        scroll.connect_scroll({
            let weak = window.downgrade();
            move |controller, _, dy| {
                let Some(window) = weak.upgrade() else { return glib::Propagation::Proceed };
                if !controller.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                    return glib::Propagation::Proceed;
                }
                let total = pending.get() + dy;
                let notches = total.trunc();
                pending.set(total - notches);
                if notches != 0.0 {
                    window.resize_items(-(notches as i32) * THUMBNAIL_STEP);
                }
                glib::Propagation::Stop
            }
        });
        imp.tab_view.add_controller(scroll);
        window.navigate(&start_location(settings));
        window.reselect_last_file(settings);
        window
    }

    /// Reselects the `last-file` setting once the folder lists, if the window started in its folder.
    fn reselect_last_file(&self, settings: &gio::Settings) {
        if settings.string("start-location") != "last" {
            return;
        }
        let file = PathBuf::from(settings.string("last-file").as_str());
        let Some(file_folder) = file.parent().map(Path::to_path_buf) else { return };
        if self.current_path().as_deref() != Some(file_folder.as_path()) {
            return;
        }
        if let Some(view) = self.active_browse() {
            let file = file.to_string_lossy().into_owned();
            view.set_pending(&file_folder, move |view: &BrowseView| {
                view.select_named(&file);
            });
        }
    }

    /// The folder the window shows, or `None` before the first navigation.
    pub fn current_path(&self) -> Option<PathBuf> {
        self.imp().current_path.borrow().clone()
    }

    /// Makes `path` the window's folder. This is the only way the folder changes: it records the
    /// visit in the history and sets the path on the path bar, the tree, the browse list and its
    /// label. Returns `false`, changing nothing, if `path` is not a directory.
    pub fn navigate(&self, path: &Path) -> bool {
        self.navigate_with(path, Reveal::Expand)
    }

    fn navigate_with(&self, path: &Path, reveal: Reveal) -> bool {
        let imp = self.imp();
        if imp.updating.get() {
            return false;
        }
        if imp.current_path.borrow().as_deref() == Some(path) {
            return true;
        }
        if !path.is_dir() {
            eprintln!("rsimgv: {} is not a folder", path.display());
            return false;
        }

        // With no tab open, navigating opens one at the folder and activates it.
        if imp.tab_view.n_pages() == 0 {
            self.open_tab(path);
            match reveal {
                Reveal::Expand => imp.tree.reveal_path(path),
                Reveal::SelectOnly => imp.tree.reveal_path_selecting_only(path),
            }
            return true;
        }

        imp.updating.set(true);
        imp.navigation.set_location(path);
        self.show_path(path);
        match reveal {
            Reveal::Expand => imp.tree.reveal_path(path),
            Reveal::SelectOnly => imp.tree.reveal_path_selecting_only(path),
        }
        imp.updating.set(false);
        true
    }

    /// The actions behind the context menus. Each takes the path it works on as its parameter.
    fn add_context_actions(&self) {
        let weak = self.downgrade();
        for name in ["open-path", "open-in-new-tab", "open-in-new-window", "copy-path", "show-properties", "rename-bookmark", "remove-bookmark"] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = weak.clone();
            action.connect_activate(move |_, parameter| {
                let (Some(window), Some(path)) = (weak.upgrade(), parameter.and_then(|p| p.get::<String>())) else {
                    return;
                };
                let path = PathBuf::from(path);
                match name {
                    "open-path" => {
                        window.navigate(&path);
                    }
                    "open-in-new-tab" if path.is_dir() => window.open_background_tab(&path),
                    "open-in-new-window" => window.open_in_new_window(&path),
                    "copy-path" => window.clipboard().set_text(&path.to_string_lossy()),
                    "show-properties" => window.show_properties(&path),
                    "rename-bookmark" => window.ask_bookmark_name("Rename Bookmark", path),
                    "remove-bookmark" => window.remove_bookmark(&path),
                    _ => {}
                }
            });
            self.add_action(&action);
        }
    }

    /// The actions behind the tab context menu. Each takes the id of the tab it works on.
    fn add_tab_actions(&self) {
        let weak = self.downgrade();
        for name in ["tab-duplicate", "tab-new-window", "tab-copy-path", "tab-close-others", "tab-close"] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT64));
            let weak = weak.clone();
            action.connect_activate(move |_, parameter| {
                let (Some(window), Some(id)) = (weak.upgrade(), parameter.and_then(|p| p.get::<u64>())) else {
                    return;
                };
                let Some(page) = window.page_for_tab(id) else { return };
                let path = window.tab_path(id);
                match name {
                    "tab-duplicate" => {
                        if let Some(path) = path {
                            window.open_tab(&path);
                        }
                    }
                    "tab-new-window" => {
                        if let Some(path) = path {
                            window.open_in_new_window(&path);
                        }
                    }
                    "tab-copy-path" => {
                        if let Some(path) = path {
                            window.clipboard().set_text(&path.to_string_lossy());
                        }
                    }
                    "tab-close-others" => {
                        let tab_view = window.imp().tab_view.clone();
                        for other in tab_view.pages().iter::<adw::TabPage>().flatten() {
                            if other != page {
                                tab_view.close_page(&other);
                            }
                        }
                    }
                    "tab-close" => window.imp().tab_view.close_page(&page),
                    _ => {}
                }
            });
            self.add_action(&action);
        }
    }

    /// The page of the tab with `id`.
    fn page_for_tab(&self, id: u64) -> Option<adw::TabPage> {
        let tab_view = &self.imp().tab_view;
        (0..tab_view.n_pages())
            .map(|position| tab_view.nth_page(position))
            .find(|page| tab_id(page) == Some(id))
    }

    /// The folder a tab shows: the current one for the active tab, the saved one otherwise.
    fn tab_path(&self, id: u64) -> Option<PathBuf> {
        let imp = self.imp();
        if imp.active_tab.get() == Some(id) {
            self.current_path()
        } else {
            imp.tabs.borrow().get(&id).and_then(|state| state.path.clone())
        }
    }

    /// Shows a context menu for a folder or file at (`x`, `y`) within `parent`. Folders can be opened in a tab or a window.
    fn show_context_menu(&self, parent: &gtk::Widget, path: &Path, is_dir: bool, x: f64, y: f64) {
        let item = |label: &str, action: &str| path_menu_item(label, action, path);
        let menu = gio::Menu::new();
        if is_dir {
            menu.append_item(&item("Open", "win.open-path"));
            menu.append_item(&item("Open in New Tab", "win.open-in-new-tab"));
            menu.append_item(&item("Open in New Window", "win.open-in-new-window"));
        }
        let details = gio::Menu::new();
        details.append_item(&item("Copy Path", "win.copy-path"));
        details.append_item(&item("Properties", "win.show-properties"));
        let playlist_section = gio::Menu::new();
        let targets = self.context_targets(path);
        for (label, action) in playlist_menu_items(path, is_dir, targets.len()) {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&targets.to_variant()));
            playlist_section.append_item(&item);
        }
        if playlist_section.n_items() > 0 {
            menu.append_section(None, &playlist_section);
        }
        menu.append_section(None, &details);
        popup_menu(parent, &menu, x, y);
    }

    /// Shows a context menu for the bookmark of `path`, for a press at (`x`, `y`) within its row. The menu belongs
    /// to the list rather than the row, because removing the bookmark rebuilds the rows.
    fn show_bookmark_menu(&self, row: &gtk::Widget, path: &Path, x: f64, y: f64) {
        let list = &self.imp().bookmarks_list;
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        let (x, y) = row.compute_point(&**list, &point).map_or((x, y), |p| (f64::from(p.x()), f64::from(p.y())));

        let item = |label: &str, action: &str| path_menu_item(label, action, path);
        let menu = gio::Menu::new();
        let open = gio::Menu::new();
        open.append_item(&item("Open in New Tab", "win.open-in-new-tab"));
        open.append_item(&item("Open in New Window", "win.open-in-new-window"));
        menu.append_section(None, &open);
        let bookmark = gio::Menu::new();
        bookmark.append_item(&item("Rename…", "win.rename-bookmark"));
        bookmark.append_item(&item("Remove", "win.remove-bookmark"));
        menu.append_section(None, &bookmark);
        let details = gio::Menu::new();
        details.append_item(&item("Copy Path", "win.copy-path"));
        details.append_item(&item("Properties", "win.show-properties"));
        menu.append_section(None, &details);
        popup_menu(list.upcast_ref(), &menu, x, y);
    }

    /// The items a context menu on `path` acts on, as text: every marked item if it is one of them, else `path` alone.
    fn context_targets(&self, path: &Path) -> Vec<String> {
        let marked = self.active_browse().map(|browse| browse.marked_paths()).unwrap_or_default();
        let paths = if marked.iter().any(|item| item == path) { marked } else { vec![path.to_path_buf()] };
        paths.iter().map(|path| path.to_string_lossy().into_owned()).collect()
    }

    /// Opens a second window at `path`.
    fn open_in_new_window(&self, path: &Path) {
        let Some(app) = self.application().and_downcast::<adw::Application>() else { return };
        let Some(settings) = self.imp().settings.get() else { return };
        let window = Window::new(&app, settings);
        window.navigate(path);
        gtk::prelude::GtkWindowExt::present(&window);
    }

    /// Shows a folder's or file's name, type, size, and when it was modified.
    fn show_properties(&self, path: &Path) {
        // The file is queried on a worker, so a slow or remote file does not hold up the window.
        let path = path.to_path_buf();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let query = path.clone();
            let Ok((heading, body)) = gio::spawn_blocking(move || properties_text(&query)).await else { return };
            let Some(window) = weak.upgrade() else { return };
            window.present_properties(&heading, &body);
        });
    }

    /// Presents a properties dialog with `heading` and `body`.
    fn present_properties(&self, heading: &str, body: &str) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        adw::prelude::AlertDialogExt::add_response(&dialog, "close", "Close");
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Roots the tree at `root`, and shows the folder in it if it lies under `root`. The folder itself does not change.
    fn set_tree_root(&self, root: &Path) {
        let imp = self.imp();
        if imp.tree.root_path().as_deref() == Some(root) {
            return;
        }
        if let Err(err) = imp.tree.set_root_path(root) {
            eprintln!("rsimgv: {err:#}");
            self.sync_tree_root_buttons();
            return;
        }
        if let Some(path) = self.current_path().filter(|path| path.starts_with(root)) {
            imp.tree.reveal_path(&path);
        }
    }

    /// Marks the header button of the root the tree has.
    fn sync_tree_root_buttons(&self) {
        let imp = self.imp();
        let root = imp.tree.root_path();
        imp.filesystem_root_button.set_active(root.as_deref() == Some(Path::new("/")));
        imp.filesystem_home_button.set_active(root.as_deref() == Some(glib::home_dir().as_path()));
    }

    /// Shows `path` in the path bar, the browse view, and the tab's title. It does not change the
    /// history or the tree; navigation and tab switching do that.
    fn show_path(&self, path: &Path) {
        let imp = self.imp();
        let previous = imp.current_path.replace(Some(path.to_path_buf()));
        if previous.as_deref() != Some(path) && self.viewer_is_open() && !imp.playlist_playing.get() {
            self.announce_folder(path);
        }
        if let Err(err) = imp.path_bar.set_path(path) {
            eprintln!("rsimgv: {err:#}");
        }
        if let Some(browse) = self.active_browse() {
            if browse.path().as_deref() != Some(path) {
                // Once the folder lists, the item selected the last time this location was shown is selected again,
                // or else, going up to a parent, the item that leads to the folder shown before.
                let selected = imp.navigation.selected().or_else(|| previous.as_deref().and_then(|previous| child_toward(path, previous)));
                // A step to another folder with media has arranged which item to select, which the item remembered
                // for the folder does not replace.
                if let (Some(name), false) = (selected, browse.has_pending_for(path)) {
                    browse.set_pending(path, move |view| {
                        view.select_named(&name);
                    });
                }
                browse.set_path(path);
                self.apply_folder_sort(&browse);
            }
        }
        if let Some(page) = imp.tab_view.selected_page() {
            label_page(&page, path);
        }
        self.sync_bookmark_selection();
    }

    /// Shows `path` in the viewer, which steps through the pictures and videos `view` lists. A file the
    /// view does not list (one that is not a picture or video) is not shown.
    fn open_viewer(&self, view: &BrowseView, path: &Path) {
        let paths = view.media_paths();
        let Some(index) = paths.iter().position(|listed| listed == path) else { return };
        self.show_in_viewer(paths, index, false);
    }

    /// Shows the item at `index` of `paths` in the viewer, which steps through them. They are the playlist if `playlist`
    /// is set, and otherwise the items of the folder shown.
    fn show_in_viewer(&self, paths: Vec<PathBuf>, index: usize, playlist: bool) {
        let imp = self.imp();
        imp.playlist_playing.set(playlist);
        if let (true, Some(path)) = (playlist, paths.get(index)) {
            self.read_length_when_played(path);
        }
        self.apply_folder_end();
        imp.viewer.show(paths, index);
        imp.viewer_open.set(true);
        self.place_playlist();
        self.refresh_keybinds_overlay();
        imp.pages.set_visible_child(&*imp.viewer);
        imp.viewer.grab_focus();
        if imp.settings.get().is_some_and(|settings| settings.boolean("open-fullscreen")) && !self.is_fullscreen() {
            self.fullscreen();
            imp.viewer_fullscreened.set(true);
        }
        self.update_title();
    }

    /// Reads the length of the playlist item `path` when it is played, if the playlist does not read lengths in the
    /// background. The panel then shows it once it is read.
    fn read_length_when_played(&self, path: &Path) {
        let Some(settings) = self.imp().settings.get() else { return };
        if settings.boolean("playlist-fetch-in-background") {
            return;
        }
        let Some((mtime, size)) = stamp(path) else { return };
        let weak = self.downgrade();
        MediaInfo::shared().request(path, mtime, size, || true, move || {
            if let Some(window) = weak.upgrade() {
                if let Some(panel) = window.imp().playlist_panel.get() {
                    panel.refresh();
                }
            }
        });
    }

    /// Returns from the viewer to the browse view, releasing what the viewer held.
    fn close_viewer(&self) {
        let imp = self.imp();
        imp.viewer.close();
        imp.viewer_open.set(false);
        self.place_playlist();
        imp.playlist_playing.set(false);
        self.apply_folder_end();
        self.refresh_keybinds_overlay();
        imp.pages.set_visible_child(&*imp.split);
        if imp.viewer_fullscreened.take() {
            self.unfullscreen();
        }
        self.update_title();
    }

    /// Applies the settings the viewer shows its pictures and videos with, and the window title, which
    /// depends on one of them.
    fn apply_viewer_settings(&self, settings: &gio::Settings) {
        let viewer = &self.imp().viewer;
        viewer.set_zoom_step(settings.int("zoom-step"));
        viewer.set_expand_limit(settings.int("expand-up-to-factor"));
        viewer.set_keep_fit(settings.boolean("keep-fit-mode"));
        viewer.set_original_focus(OriginalFocus::from_name(&settings.string("original-focus")));
        viewer.set_minimum_zoom_unlocked(settings.boolean("unlock-minimum-zoom"));
        viewer.set_default_fit(Fit::from_name(&settings.string("image-fit")).unwrap_or_default());
        viewer.set_smooth_scrolling(settings.boolean("smooth-scrolling"));
        viewer.set_scaling(Scaling::from_name(&settings.string("scaling-filter")), settings.boolean("smooth-upscaling"));
        viewer.set_zoom_indicator(ZoomIndicator::from_name(&settings.string("zoom-indicator")).unwrap_or_default());
        viewer.set_auto_hide_cursor(settings.boolean("auto-hide-cursor"));
        viewer.set_blurred_background(settings.boolean("viewer-blurred-background"));
        viewer.set_status_bar(settings.boolean("viewer-status-bar"));
        viewer.set_status_overlay_position(&settings.string("viewer-status-overlay-position"));
        viewer.set_svp(crate::svp::Config::from_settings(settings));
        self.apply_folder_end();
        viewer.set_seek_options(super::seek_overlay::SeekOptions::from_settings(settings));
        self.update_title();
    }

    /// Tells the viewer what stepping past the ends does: what the setting says for a folder. In the playlist it stops,
    /// so a video that ends does not start the playlist over, and there is no next folder to go to.
    fn apply_folder_end(&self) {
        let Some(settings) = self.imp().settings.get() else { return };
        let end = if self.imp().playlist_playing.get() {
            FolderEnd::Stop
        } else {
            FolderEnd::from_name(&settings.string("folder-end"))
        };
        self.imp().viewer.set_folder_end(end);
    }

    /// Titles the window with the file name and size of the picture or video the viewer shows, when the
    /// setting that asks for it is on. Otherwise, and when the viewer is not open, it is titled by the app.
    fn update_title(&self) {
        let imp = self.imp();
        let info = imp.settings.get().is_some_and(|settings| settings.boolean("image-info-in-title"));
        let item = (info && imp.viewer_open.get()).then(|| imp.viewer.shown_item()).flatten();
        let title = match item {
            Some(item) => {
                let name = item.path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                if item.width > 0 && item.height > 0 {
                    format!("{name} · {} × {}", item.width, item.height)
                } else {
                    name
                }
            }
            None => "rsimgv".to_owned(),
        };
        self.set_title(Some(&title));
    }

    /// The actions keybindings activate, by group: `win` on the window, and `browse`, `view` and `video` for
    /// the browse list, the viewer and the video. The catalogue in `keybindings::actions` gives each one's contexts.
    fn add_binding_actions(&self) {
        let weak = self.downgrade();
        // The notification action takes what to show as its parameter: the message, the level and the seconds.
        let notify = gio::SimpleAction::new("notify", Some(&<(String, String, f64)>::static_variant_type()));
        notify.connect_activate({
            let weak = weak.clone();
            move |_, parameter| {
                let Some(window) = weak.upgrade() else { return };
                if let Some((message, level, seconds)) = parameter.and_then(|parameter| parameter.get::<(String, String, f64)>()) {
                    window.show_notification(&message, &level, seconds);
                }
            }
        });
        self.add_action(&notify);
        bound_action(self, &weak, "toggle-keybinds", Window::toggle_keybinds_overlay);
        bound_action(self, &weak, "settings", Window::show_preferences);
        bound_action(self, &weak, "settings-keybindings", |window| window.present_preferences(Some("keybindings")));
        bound_action(self, &weak, "open-folder", Window::open_folder);
        bound_action(self, &weak, "bookmark-folder", Window::bookmark_current_folder);
        bound_action(self, &weak, "toggle-bookmarks", |window| {
            let expander = &window.imp().bookmarks_expander;
            expander.set_expanded(!expander.is_expanded());
        });
        bound_action(self, &weak, "toggle-filesystem", |window| {
            let expander = &window.imp().filesystem_expander;
            expander.set_expanded(!expander.is_expanded());
        });
        bound_action(self, &weak, "edit-location", Window::edit_location);
        bound_action(self, &weak, "navigate-back", |window| window.imp().navigation.back());
        bound_action(self, &weak, "navigate-forward", |window| window.imp().navigation.forward());
        bound_action(self, &weak, "navigate-up", Window::go_up);
        // In the viewer, the folder steps go to the next folder with media, and show its first item.
        bound_action(self, &weak, "next-folder", |window| {
            if window.viewer_is_open() {
                window.step_media_folder_later(true);
            } else {
                window.step_folder_later(true);
            }
        });
        bound_action(self, &weak, "previous-folder", |window| {
            if window.viewer_is_open() {
                window.step_media_folder_later(false);
            } else {
                window.step_folder_later(false);
            }
        });
        bound_action(self, &weak, "next-media-folder", |window| {
            window.step_media_folder_later(true);
        });
        bound_action(self, &weak, "previous-media-folder", |window| {
            window.step_media_folder_later(false);
        });
        bound_action(self, &weak, "playlist-toggle", Window::toggle_in_playlist);
        bound_action(self, &weak, "playlist-play", Window::play_playlist);
        bound_action(self, &weak, "playlist-next", |window| window.step_playlist(true));
        bound_action(self, &weak, "playlist-previous", |window| window.step_playlist(false));
        bound_action(self, &weak, "playlist-first", |window| window.jump_playlist(false));
        bound_action(self, &weak, "playlist-last", |window| window.jump_playlist(true));
        bound_action(self, &weak, "playlist-load", Window::choose_playlist_to_load);
        bound_action(self, &weak, "playlist-save", Window::choose_playlist_to_save);
        bound_action(self, &weak, "playlist-clear", Window::confirm_clear_playlist);
        bound_action(self, &weak, "playlist-add-current", Window::add_current_to_playlist);
        bound_action(self, &weak, "playlist-add-files", Window::choose_files_to_add);
        bound_action(self, &weak, "playlist-add-directory", Window::choose_directory_to_add);
        bound_action(self, &weak, "playlist-remove-current", Window::remove_current_from_playlist);
        bound_action(self, &weak, "playlist-save-current", Window::save_playlist_current);
        bound_action(self, &weak, "playlist-load-url", Window::ask_playlist_url);
        bound_action(self, &weak, "playlist-pause", |window| window.imp().viewer.pause());
        bound_action(self, &weak, "toggle-playlist", Window::toggle_playlist_panel);
        bound_action(self, &weak, "focus-tree", |window| window.focus_pane(FocusPane::Tree));
        bound_action(self, &weak, "focus-playlist", |window| window.focus_pane(FocusPane::Playlist));
        bound_action(self, &weak, "focus-viewer", |window| window.focus_pane(FocusPane::Content));
        bound_action(self, &weak, "focus-cycle", |window| window.cycle_focus(false));
        bound_action(self, &weak, "focus-cycle-reverse", |window| window.cycle_focus(true));
        bound_action(self, &weak, "toggle-sidebar", |window| {
            let split = &window.imp().split;
            split.set_show_sidebar(!split.show_sidebar());
        });
        bound_action(self, &weak, "toggle-fullscreen", |window| {
            if window.is_fullscreen() {
                window.unfullscreen();
            } else {
                window.fullscreen();
            }
        });

        let browse = gio::SimpleActionGroup::new();
        bound_action(&browse, &weak, "up-directory", Window::go_up);
        bound_action(&browse, &weak, "cursor-up", |window| window.with_browse(|view| view.move_cursor_y(-1)));
        bound_action(&browse, &weak, "cursor-down", |window| window.with_browse(|view| view.move_cursor_y(1)));
        bound_action(&browse, &weak, "cursor-previous", |window| window.with_browse(|view| view.move_cursor_x(-1)));
        bound_action(&browse, &weak, "cursor-next", |window| window.with_browse(|view| view.move_cursor_x(1)));
        bound_action(&browse, &weak, "cursor-page-up", |window| window.with_browse(|view| view.move_cursor_page(-1)));
        bound_action(&browse, &weak, "cursor-page-down", |window| window.with_browse(|view| view.move_cursor_page(1)));
        bound_action(&browse, &weak, "first", |window| window.with_browse(|view| view.select_first()));
        bound_action(&browse, &weak, "last", |window| window.with_browse(|view| view.select_last()));
        bound_action(&browse, &weak, "select-all", |window| window.with_browse(|view| view.select_all()));
        bound_action(&browse, &weak, "open", |window| window.with_browse(|view| view.activate_selected()));
        bound_action(&browse, &weak, "show-tooltip", |window| window.with_browse(|view| view.show_tooltip_now()));
        bound_action(&browse, &weak, "toggle-hidden", |window| window.toggle_setting("show-hidden"));
        bound_action(&browse, &weak, "toggle-media-only", |window| window.toggle_setting("media-only"));
        bound_action(&browse, &weak, "toggle-filename", Window::toggle_filename);
        bound_action(&browse, &weak, "toggle-item-info", Window::toggle_item_info);
        bound_action(&browse, &weak, "thumbnail-larger", |window| window.resize_items(THUMBNAIL_STEP));
        bound_action(&browse, &weak, "thumbnail-smaller", |window| window.resize_items(-THUMBNAIL_STEP));
        self.insert_action_group("browse", Some(&browse));

        let view = gio::SimpleActionGroup::new();
        bound_action(&view, &weak, "previous", |window| window.imp().viewer.step(false));
        bound_action(&view, &weak, "next", |window| window.imp().viewer.step(true));
        bound_action(&view, &weak, "first", |window| window.imp().viewer.jump(false));
        bound_action(&view, &weak, "last", |window| window.imp().viewer.jump(true));
        bound_action(&view, &weak, "close", Window::close_viewer);
        bound_action(&view, &weak, "pin", |window| window.imp().viewer.toggle_pinned());
        bound_action(&view, &weak, "toggle-thumbnail-bar", |window| window.toggle_setting("viewer-thumbnail-bar-shown"));
        bound_action(&view, &weak, "toggle-status-bar", |window| window.toggle_setting("viewer-status-bar"));
        bound_action(&view, &weak, "toggle-thumbnail-bar-pin", |window| window.toggle_setting("viewer-thumbnail-bar-pinned"));
        bound_action(&view, &weak, "toggle-thumbnail-bar-overlay", |window| window.toggle_setting("viewer-thumbnail-bar-overlay"));
        self.insert_action_group("view", Some(&view));

        let video = gio::SimpleActionGroup::new();
        bound_action(&video, &weak, "play-pause", |window| window.imp().viewer.toggle_playback());
        bound_action(&video, &weak, "seek-forward", |window| window.imp().viewer.seek(SEEK_STEP));
        bound_action(&video, &weak, "seek-backward", |window| window.imp().viewer.seek(-SEEK_STEP));
        valued_action(&video, &weak, "video.seek", |window, seconds| window.imp().viewer.seek((seconds * 1_000_000.0) as i64));
        valued_action(&video, &weak, "video.volume", |window, percent| window.imp().viewer.adjust_volume(percent / 100.0));
        valued_action(&video, &weak, "video.set-speed", |window, speed| window.imp().viewer.set_speed(speed));
        valued_action(&video, &weak, "video.adjust-speed", |window, percent| window.imp().viewer.adjust_speed(percent / 100.0));
        bound_action(&video, &weak, "volume-up", |window| window.imp().viewer.adjust_volume(VOLUME_STEP));
        bound_action(&video, &weak, "volume-down", |window| window.imp().viewer.adjust_volume(-VOLUME_STEP));
        bound_action(&video, &weak, "mute", |window| window.imp().viewer.toggle_mute());
        bound_action(&video, &weak, "toggle-svp", |window| window.toggle_setting("svp-enabled"));
        bound_action(&video, &weak, "repeat", |window| window.imp().viewer.toggle_repeat());
        bound_action(&video, &weak, "auto-advance", |window| window.imp().viewer.toggle_auto_advance());
        self.insert_action_group("video", Some(&video));

        let image = gio::SimpleActionGroup::new();
        bound_action(&image, &weak, "zoom-in", |window| window.imp().viewer.zoom_in());
        bound_action(&image, &weak, "zoom-out", |window| window.imp().viewer.zoom_out());
        bound_action(&image, &weak, "zoom-in-at-pointer", |window| window.imp().viewer.zoom_in_at_pointer());
        bound_action(&image, &weak, "zoom-out-at-pointer", |window| window.imp().viewer.zoom_out_at_pointer());
        bound_action(&image, &weak, "fit-cycle", |window| window.imp().viewer.cycle_fit());
        bound_action(&image, &weak, "fit-window", |window| window.imp().viewer.set_fit(Fit::Window));
        bound_action(&image, &weak, "fit-width", |window| window.imp().viewer.set_fit(Fit::Width));
        bound_action(&image, &weak, "fit-height", |window| window.imp().viewer.set_fit(Fit::Height));
        bound_action(&image, &weak, "fit-fill", |window| window.imp().viewer.set_fit(Fit::Fill));
        bound_action(&image, &weak, "fit-original", |window| window.imp().viewer.set_fit(Fit::Original));
        bound_action(&image, &weak, "pan-up", |window| window.imp().viewer.pan(0.0, -PAN_STEP));
        bound_action(&image, &weak, "pan-down", |window| window.imp().viewer.pan(0.0, PAN_STEP));
        bound_action(&image, &weak, "pan-left", |window| window.imp().viewer.pan(-PAN_STEP, 0.0));
        bound_action(&image, &weak, "pan-right", |window| window.imp().viewer.pan(PAN_STEP, 0.0));
        self.insert_action_group("image", Some(&image));

        let playlist = gio::SimpleActionGroup::new();
        bound_action(&playlist, &weak, "cursor-up", |window| window.with_playlist(|panel| panel.select_step(-1)));
        bound_action(&playlist, &weak, "cursor-down", |window| window.with_playlist(|panel| panel.select_step(1)));
        bound_action(&playlist, &weak, "move-up", |window| window.with_playlist(|panel| panel.run_selection_action("move-up")));
        bound_action(&playlist, &weak, "move-down", |window| window.with_playlist(|panel| panel.run_selection_action("move-down")));
        bound_action(&playlist, &weak, "move-top", |window| window.with_playlist(|panel| panel.run_selection_action("move-top")));
        bound_action(&playlist, &weak, "move-bottom", |window| window.with_playlist(|panel| panel.run_selection_action("move-bottom")));
        bound_action(&playlist, &weak, "toggle-search", |window| window.with_playlist(PlaylistPanel::toggle_search));
        bound_action(&playlist, &weak, "focus-search", |window| window.with_playlist(PlaylistPanel::focus_search));
        self.insert_action_group("playlist", Some(&playlist));

        let tree = gio::SimpleActionGroup::new();
        bound_action(&tree, &weak, "cursor-up", |window| window.imp().tree.move_selection(-1));
        bound_action(&tree, &weak, "cursor-down", |window| window.imp().tree.move_selection(1));
        bound_action(&tree, &weak, "first", |window| window.imp().tree.select_edge(false));
        bound_action(&tree, &weak, "last", |window| window.imp().tree.select_edge(true));
        bound_action(&tree, &weak, "activate", |window| window.imp().tree.toggle_selected());
        bound_action(&tree, &weak, "expand", |window| window.imp().tree.set_selected_expanded(true));
        bound_action(&tree, &weak, "collapse", |window| window.imp().tree.set_selected_expanded(false));
        self.insert_action_group("tree", Some(&tree));
    }

    /// Lights the drag handle of the sidebar that holds the focus: the folder tree's or the playlist's.
    fn update_handle_highlights(&self) {
        let imp = self.imp();
        let scopes = self.active_scopes();
        imp.split.set_handle_active(scopes.contains(&Scope::Tree));
        let playlist = scopes.contains(&Scope::Playlist);
        imp.playlist_split.set_handle_active(playlist);
        imp.viewer.playlist_split().set_handle_active(playlist);
    }

    /// Where the keyboard focus is: in the folder tree, in the playlist, or else in the content, the browse view or the
    /// viewer.
    fn focused_pane(&self) -> FocusPane {
        let imp = self.imp();
        if self.focus_is_within(&*imp.tree) {
            FocusPane::Tree
        } else if imp.playlist_panel.get().is_some_and(PlaylistPanel::has_focus) {
            FocusPane::Playlist
        } else {
            FocusPane::Content
        }
    }

    /// Whether the keyboard focus is in `widget` or one of its descendants.
    fn focus_is_within(&self, widget: &impl IsA<gtk::Widget>) -> bool {
        gtk::prelude::GtkWindowExt::focus(self).is_some_and(|focus| focus == *widget.upcast_ref::<gtk::Widget>() || focus.is_ancestor(widget))
    }

    /// Whether `pane` is on screen, so cycling the focus can land on it.
    fn pane_shown(&self, pane: FocusPane) -> bool {
        let imp = self.imp();
        match pane {
            FocusPane::Tree => imp.split.show_sidebar(),
            FocusPane::Content => true,
            FocusPane::Playlist => imp.playlist_window.borrow().is_some() || self.active_playlist_split().show_sidebar(),
        }
    }

    /// Moves the focus to the next pane that is shown: the folder tree, the content, then the playlist. Backwards, it
    /// goes the other way round.
    fn cycle_focus(&self, reverse: bool) {
        const ORDER: [FocusPane; 3] = [FocusPane::Tree, FocusPane::Content, FocusPane::Playlist];
        let current = self.focused_pane();
        let start = ORDER.iter().position(|pane| *pane == current).unwrap_or(1);
        let len = ORDER.len();
        let next = (1..=len).map(|step| ORDER[(start + if reverse { len - step } else { step }) % len]).find(|pane| self.pane_shown(*pane));
        self.focus_pane(next.unwrap_or(FocusPane::Content));
    }

    /// Moves the focus to `pane`, showing it first if it is hidden, then outlines it for a moment and names it over the
    /// window.
    fn focus_pane(&self, pane: FocusPane) {
        let imp = self.imp();
        let (widget, name): (gtk::Widget, &str) = match pane {
            FocusPane::Tree => {
                self.set_show_sidebar(true);
                (imp.tree.clone().upcast(), "Folder Tree")
            }
            FocusPane::Playlist => {
                let Some(panel) = imp.playlist_panel.get() else { return };
                let detached = imp.playlist_window.borrow().clone();
                match detached {
                    Some(detached) => gtk::prelude::GtkWindowExt::present(&detached),
                    None => self.active_playlist_split().set_show_sidebar(true),
                }
                (panel.content().clone().upcast(), "Playlist")
            }
            FocusPane::Content if self.viewer_is_open() => (imp.viewer.clone().upcast(), "Viewer"),
            FocusPane::Content => match self.active_browse() {
                Some(browse) => (browse.upcast(), "Browser"),
                None => return,
            },
        };
        // A pane that was hidden may not be on screen yet, so the focus is tried again once it is.
        if !self.give_focus(pane) {
            let weak = self.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(window) = weak.upgrade() {
                    window.give_focus(pane);
                }
            });
        }
        pulse(&widget);
        let messages = self.messages();
        if let Some(previous) = imp.focus_notice.take() {
            messages.dismiss(previous);
        }
        let notice = messages.show(Message::notification(format!("Focus: {name}")).level(Level::Plain).lasting(FOCUS_NOTICE));
        imp.focus_notice.set(Some(notice));
    }

    /// Asks `pane` to take the keyboard focus. Returns whether it did.
    fn give_focus(&self, pane: FocusPane) -> bool {
        let imp = self.imp();
        match pane {
            FocusPane::Tree => imp.tree.focus_list(),
            FocusPane::Playlist => imp.playlist_panel.get().is_some_and(PlaylistPanel::focus_list),
            FocusPane::Content if self.viewer_is_open() => imp.viewer.grab_focus(),
            FocusPane::Content => self.active_browse().is_some_and(|browse| browse.focus_grid()),
        }
    }

    /// Runs `f` on the playlist panel.
    fn with_playlist(&self, f: impl FnOnce(&PlaylistPanel)) {
        if let Some(panel) = self.imp().playlist_panel.get() {
            f(panel);
        }
    }

    /// The actions behind the playlist's menu items and its panel, which say what they work on in their parameter.
    fn add_playlist_actions(&self, settings: &gio::Settings) {
        let weak = self.downgrade();
        // The menu's column toggles are window actions named by their settings, so they work from any part of the window.
        for (key, _, _, _) in crate::ui::playlist_panel::COLUMNS {
            self.add_action(&settings.create_action(key));
        }
        self.add_action(&settings.create_action("playlist-repeat"));
        for name in ["playlist-add-paths", "playlist-remove-paths"] {
            let action = gio::SimpleAction::new(name, Some(&<Vec<String>>::static_variant_type()));
            let weak = weak.clone();
            action.connect_activate(move |_, parameter| {
                let (Some(window), Some(paths)) = (weak.upgrade(), parameter.and_then(|p| p.get::<Vec<String>>())) else { return };
                let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
                match name {
                    "playlist-add-paths" => window.add_to_playlist_later(paths, false),
                    _ => window.remove_from_playlist(&paths),
                }
            });
            self.add_action(&action);
        }
        for name in ["playlist-play-index", "playlist-remove-index"] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT32));
            let weak = weak.clone();
            action.connect_activate(move |_, parameter| {
                let (Some(window), Some(index)) = (weak.upgrade(), parameter.and_then(|p| p.get::<u32>())) else { return };
                match name {
                    "playlist-play-index" => window.play_playlist_at(index as usize),
                    _ => {
                        if playlist::with(|list| list.remove_at(index as usize)) {
                            window.playlist_changed();
                        }
                    }
                }
            });
            self.add_action(&action);
        }
        // The state is the column the playlist was last sorted by, and whether it was sorted descending. An empty name is no sort.
        let sort_type = <(String, bool)>::static_variant_type();
        let saved_sort: (String, bool) = settings.value("playlist-sort").get().unwrap_or_default();
        let sort = gio::SimpleAction::new_stateful("playlist-sort", Some(&sort_type), &saved_sort.to_variant());
        let weak_sort = weak.clone();
        let settings_sort = settings.clone();
        sort.connect_activate(move |action, parameter| {
            let (Some(window), Some((column, descending))) = (weak_sort.upgrade(), parameter.and_then(|p| p.get::<(String, bool)>())) else { return };
            if let Some(sort) = SortColumn::from_name(&column) {
                window.sort_playlist(sort, descending);
                let state = (column, descending).to_variant();
                action.set_state(&state);
                let _ = settings_sort.set_value("playlist-sort", &state);
            }
        });
        self.add_action(&sort);
        // A check mark shows while the playlist is in its own window, which the main menu's item toggles.
        let detach = gio::SimpleAction::new_stateful("playlist-detach", None, &false.to_variant());
        let weak_detach = weak.clone();
        detach.connect_activate(move |_, _| {
            if let Some(window) = weak_detach.upgrade() {
                window.toggle_detached_playlist();
            }
        });
        self.add_action(&detach);
        let shuffle = gio::SimpleAction::new_stateful("playlist-shuffle", None, &playlist::with(|list| list.is_shuffled()).to_variant());
        let weak_shuffle = weak.clone();
        shuffle.connect_activate(move |_, _| {
            if let Some(window) = weak_shuffle.upgrade() {
                window.toggle_playlist_shuffle();
            }
        });
        self.add_action(&shuffle);
        let edit = gio::SimpleAction::new("playlist-edit-title", Some(glib::VariantTy::UINT32));
        let weak_edit = weak.clone();
        edit.connect_activate(move |_, parameter| {
            let (Some(window), Some(index)) = (weak_edit.upgrade(), parameter.and_then(|p| p.get::<u32>())) else { return };
            window.edit_playlist_title(index as usize);
        });
        self.add_action(&edit);
        let delete = gio::SimpleAction::new("playlist-delete", Some(glib::VariantTy::STRING));
        let weak_delete = weak.clone();
        delete.connect_activate(move |_, parameter| {
            let (Some(window), Some(path)) = (weak_delete.upgrade(), parameter.and_then(|p| p.get::<String>())) else { return };
            window.confirm_delete_from_disk(PathBuf::from(path));
        });
        self.add_action(&delete);
        let reveal = gio::SimpleAction::new("reveal-path", Some(glib::VariantTy::STRING));
        let weak_reveal = weak.clone();
        reveal.connect_activate(move |_, parameter| {
            let (Some(window), Some(path)) = (weak_reveal.upgrade(), parameter.and_then(|p| p.get::<String>())) else { return };
            window.reveal_path(Path::new(&path));
        });
        self.add_action(&reveal);
        let open_with = gio::SimpleAction::new("playlist-open-with", Some(&<(String, String)>::static_variant_type()));
        let weak_open = weak.clone();
        open_with.connect_activate(move |_, parameter| {
            let (Some(window), Some((script, path))) = (weak_open.upgrade(), parameter.and_then(|p| p.get::<(String, String)>())) else { return };
            window.run_script_on(&script, Some(PathBuf::from(path)));
        });
        self.add_action(&open_with);
        let moved = gio::SimpleAction::new("playlist-move", Some(&<(u32, u32)>::static_variant_type()));
        moved.connect_activate(move |_, parameter| {
            let (Some(window), Some((from, to))) = (weak.upgrade(), parameter.and_then(|p| p.get::<(u32, u32)>())) else { return };
            eprintln!("playlist-debug: move {from} -> {to}");
            if playlist::with(|list| list.move_item(from as usize, to as usize)) {
                window.playlist_changed();
            }
        });
        self.add_action(&moved);
    }

    /// The items a key adds to or removes from the playlist: the one the viewer shows or, while browsing, the marked
    /// ones or else the one under the cursor.
    fn playlist_targets(&self) -> Vec<PathBuf> {
        if !self.viewer_is_open() {
            let marked = self.active_browse().map(|browse| browse.marked_paths()).unwrap_or_default();
            if !marked.is_empty() {
                return marked;
            }
        }
        self.script_target().into_iter().collect()
    }

    /// Removes the items in front of the user from the playlist if they are all in it, and otherwise adds them.
    fn toggle_in_playlist(&self) {
        let targets = self.playlist_targets();
        if targets.is_empty() {
            self.announce_playlist("Nothing is selected to add to the playlist");
        } else if playlist::with(|list| targets.iter().all(|target| list.contains(target))) {
            self.remove_from_playlist(&targets);
        } else {
            self.add_to_playlist_later(targets, true);
        }
    }

    /// Adds the pictures and videos among `targets` to the playlist, and those in and under the ones that are folders.
    /// With `toggle`, if they are all in it already, they are removed instead. The folders are read on a worker thread,
    /// so a large one does not freeze the window.
    fn add_to_playlist_later(&self, targets: Vec<PathBuf>, toggle: bool) {
        let show_hidden = self.show_hidden();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let found = gio::spawn_blocking(move || media_among(&targets, show_hidden)).await;
            let (Some(window), Ok(files)) = (weak.upgrade(), found) else { return };
            window.add_to_playlist(&files, toggle);
        });
    }

    fn add_to_playlist(&self, files: &[PathBuf], toggle: bool) {
        if files.is_empty() {
            self.announce_playlist("There are no pictures or videos to add");
            return;
        }
        if toggle && playlist::with(|list| files.iter().all(|file| list.contains(file))) {
            self.remove_from_playlist(files);
            return;
        }
        let added = playlist::with(|list| list.add(files.iter().cloned()));
        eprintln!("playlist-debug: window add_to_playlist files={} added={added}", files.len());
        if added == 0 {
            self.announce_playlist("Already in the playlist");
            return;
        }
        self.announce_playlist(&format!("Added {} to the playlist", count_text(files, added)));
        self.playlist_changed();
    }

    /// Removes `paths` from the playlist, and whatever is under those that are folders.
    fn remove_from_playlist(&self, paths: &[PathBuf]) {
        let removed = playlist::with(|list| list.remove_under(paths));
        eprintln!("playlist-debug: window remove_from_playlist paths={} removed={removed}", paths.len());
        if removed == 0 {
            self.announce_playlist("Not in the playlist");
            return;
        }
        self.announce_playlist(&format!("Removed {} from the playlist", count_text(paths, removed)));
        self.playlist_changed();
    }

    /// Saves the playlist and has every window show it as it is now.
    fn playlist_changed(&self) {
        eprintln!("playlist-debug: window playlist_changed len={}", playlist::with(|list| list.len()));
        self.save_playlist();
        for window in self.app_windows() {
            window.show_playlist_change();
        }
    }

    /// Saves the playlist with the item it is on, and has every window mark that item.
    fn playlist_position_changed(&self) {
        eprintln!("playlist-debug: window playlist_position_changed current={:?}", playlist::with(|list| list.current()));
        self.save_playlist();
        for window in self.app_windows() {
            if let Some(panel) = window.imp().playlist_panel.get() {
                panel.refresh_current();
            }
        }
    }

    fn save_playlist(&self) {
        if let Err(err) = playlist::save() {
            eprintln!("rsimgv: could not save the playlist: {err}");
            self.notify_error(&format!("Could not save the playlist: {err}"));
        }
    }

    /// The application's windows of this kind, which share the playlist.
    fn app_windows(&self) -> Vec<Window> {
        let Some(app) = self.application() else { return vec![self.clone()] };
        app.windows().into_iter().filter_map(|window| window.downcast::<Window>().ok()).collect()
    }

    /// Shows the playlist as it is now: in the panel, on the browse items, and in the viewer if it is showing the
    /// playlist. The viewer goes back to browsing if nothing is left to show.
    fn show_playlist_change(&self) {
        let imp = self.imp();
        eprintln!("playlist-debug: window show_playlist_change playing={}", imp.playlist_playing.get());
        self.set_action_state("playlist-shuffle", playlist::with(|list| list.is_shuffled()).to_variant());
        if let Some(panel) = imp.playlist_panel.get() {
            panel.refresh();
        }
        for page in imp.tab_view.pages().iter::<adw::TabPage>().flatten() {
            if let Ok(browse) = page.child().downcast::<BrowseView>() {
                browse.refresh_playlist_marks();
            }
        }
        if imp.playlist_playing.get() {
            let items = playlist::with(|list| list.order().to_vec());
            if items.is_empty() {
                self.close_viewer();
            } else {
                imp.viewer.update_paths(items);
            }
        }
    }

    /// Shows the playlist in the viewer, from the item it was left on. If it is showing already, it stays on its item.
    fn play_playlist(&self) {
        if self.imp().playlist_playing.get() {
            return;
        }
        match playlist::with(|list| list.resume_index()) {
            Some(index) => self.play_playlist_at(index),
            None => self.announce_playlist("The playlist is empty"),
        }
    }

    /// Shows the next or previous item in the playlist, going round the ends. If the viewer is not showing the playlist
    /// it starts to, from the item it was left on, leaving the folder shown as it is.
    fn step_playlist(&self, forward: bool) {
        if !self.imp().playlist_playing.get() {
            self.play_playlist();
            return;
        }
        let (target, current, len) = playlist::with(|list| (list.step_index(forward), list.current(), list.len()));
        eprintln!("playlist-debug: window step_playlist forward={forward} current={current:?} target={target:?} len={len}");
        // With repeat off, going past either end stops there instead of going round.
        let at_end = current.is_some_and(|current| if forward { current + 1 == len } else { current == 0 });
        if at_end && !gio::Settings::new("au.topher.rimgv").boolean("playlist-repeat") {
            self.announce_playlist("The end of the playlist");
            return;
        }
        if let Some(target) = target.filter(|target| Some(*target) != current) {
            self.play_playlist_at(target);
        }
    }

    /// Shows the first or last item in the playlist.
    fn jump_playlist(&self, last: bool) {
        let (len, current) = playlist::with(|list| (list.len(), list.current()));
        let Some(end) = len.checked_sub(1) else {
            self.announce_playlist("The playlist is empty");
            return;
        };
        let target = if last { end } else { 0 };
        if !self.imp().playlist_playing.get() || current != Some(target) {
            self.play_playlist_at(target);
        }
    }

    /// Shows the playlist's item at `index` in the viewer, if there is one.
    fn play_playlist_at(&self, index: usize) {
        let imp = self.imp();
        eprintln!("playlist-debug: window play_playlist_at index={index} playing={}", imp.playlist_playing.get());
        let items = if imp.playlist_playing.get() { None } else { Some(playlist::with(|list| list.order().to_vec())) };
        if index >= playlist::with(|list| list.len()) {
            return;
        }
        playlist::with(|list| list.set_current(index));
        match items {
            Some(items) => {
                let len = items.len();
                self.show_in_viewer(items, index, true);
                self.announce_playlist(&format!("Playlist: item {} of {len}", index + 1));
            }
            None => imp.viewer.go_to(index),
        }
        self.playlist_position_changed();
    }

    /// Notes that the viewer shows the playlist's item `path`, by any way it got there, and says so if it is not there.
    /// The file is looked for on a worker thread, as it may be on a slow disk.
    fn playlist_item_shown(&self, path: &Path) {
        if playlist::with(|list| list.set_current_path(path)) {
            self.playlist_position_changed();
        }
        let path = path.to_path_buf();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
            let exists = gio::spawn_blocking(move || path.exists()).await.unwrap_or(true);
            if let (false, Some(window)) = (exists, weak.upgrade()) {
                window.notify_error(&format!("{name} is not there any more"));
            }
        });
    }

    /// Shows the playlist beside the browse view, on the side opposite the sidebar, or hides it. While it is in a window
    /// of its own, that window is brought forward instead.
    fn toggle_playlist_panel(&self) {
        let imp = self.imp();
        if let Some(detached) = imp.playlist_window.borrow().as_ref() {
            gtk::prelude::GtkWindowExt::present(detached);
            return;
        }
        let split = self.active_playlist_split();
        split.set_show_sidebar(!split.show_sidebar());
    }

    /// The split view the playlist is beside now: the viewer's while it is shown, else the browse view's.
    fn active_playlist_split(&self) -> OverlaySplitView {
        let imp = self.imp();
        if self.viewer_is_open() {
            imp.viewer.playlist_split()
        } else {
            (*imp.playlist_split).clone()
        }
    }

    /// Sets up the playlist's two split views, the browse view's and the viewer's: their sidebars are on the end side,
    /// they collapse together, and their openness is kept as the playlist's. Then the playlist is put beside the page.
    fn setup_playlist_splits(&self, settings: &gio::Settings) {
        let imp = self.imp();
        let browse = (*imp.playlist_split).clone();
        let viewer = imp.viewer.playlist_split();
        imp.playlist_shown.set(settings.boolean("playlist-shown"));
        let width = settings.int("playlist-width");
        for split in [&browse, &viewer] {
            split.set_user_sidebar_width((width > 0).then_some(width));
            split.set_sidebar_position(gtk::PackType::End);
            split.connect_show_sidebar_changed({
                let weak = self.downgrade();
                move |view, shown| {
                    // A split view with no playlist beside it opening or closing says nothing about the playlist.
                    if view.sidebar().is_none() {
                        return;
                    }
                    if let Some(window) = weak.upgrade() {
                        window.imp().playlist_shown.set(shown);
                    }
                }
            });
        }
        browse.bind_property("collapsed", &viewer, "collapsed").sync_create().build();
        self.place_playlist();
    }

    /// Puts the playlist's sidebar in the split view of the page on show, open or closed as it was, and takes it out of
    /// the other. While the playlist is in a window of its own, neither split view holds it.
    fn place_playlist(&self) {
        let imp = self.imp();
        let Some(panel) = imp.playlist_panel.get() else { return };
        let active = self.active_playlist_split();
        let browse = (*imp.playlist_split).clone();
        let viewer = imp.viewer.playlist_split();
        let idle = if active == browse { viewer } else { browse };
        idle.set_sidebar(None::<&gtk::Widget>);
        idle.set_show_sidebar(false);
        if imp.playlist_window.borrow().is_some() {
            active.set_sidebar(None::<&gtk::Widget>);
            return;
        }
        let content = panel.content();
        if let Some(parent) = content.parent().and_downcast::<gtk::Box>() {
            parent.remove(content);
        }
        active.set_sidebar(Some(content));
        active.set_show_sidebar(imp.playlist_shown.get());
    }

    /// Takes the playlist out of both split views, so it can be put in a window of its own.
    fn clear_playlist_sidebars(&self) {
        let imp = self.imp();
        for split in [(*imp.playlist_split).clone(), imp.viewer.playlist_split()] {
            split.set_sidebar(None::<&gtk::Widget>);
            split.set_show_sidebar(false);
        }
    }

    /// Shuffles the order the playlist plays in, or puts back the order its items were made in, and shows the change.
    fn toggle_playlist_shuffle(&self) {
        let on = !playlist::with(|list| list.is_shuffled());
        playlist::with(|list| list.set_shuffled(on));
        self.playlist_changed();
    }

    /// Takes the playlist out into a window of its own, or puts it back beside the browse view.
    fn toggle_detached_playlist(&self) {
        let settings = gio::Settings::new("au.topher.rimgv");
        if self.imp().playlist_window.borrow().is_some() {
            self.attach_playlist(&settings);
        } else {
            self.detach_playlist(&settings);
        }
    }

    /// Moves the playlist from beside the browse view into a window of its own. That window takes this window's actions,
    /// so its buttons and the playlist's actions work from it.
    fn detach_playlist(&self, settings: &gio::Settings) {
        let imp = self.imp();
        let Some(panel) = imp.playlist_panel.get() else { return };
        let content = panel.content();
        self.clear_playlist_sidebars();
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.append(content);

        let attach = gtk::Button::from_icon_name("view-restore-symbolic");
        attach.set_tooltip_text(Some("Put the playlist back beside the browse view"));
        attach.set_action_name(Some("win.playlist-detach"));
        let header = adw::HeaderBar::new();
        header.pack_end(&attach);
        apply_playlist_header_style(&header, &settings.string("playlist-window-headerbar"));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&holder));

        let window = adw::Window::new();
        window.set_title(Some("Playlist"));
        window.set_default_size(settings.int("playlist-window-width"), settings.int("playlist-window-height"));
        window.set_transient_for(Some(self.upcast_ref::<gtk::Window>()));
        adw::prelude::AdwWindowExt::set_content(&window, Some(&toolbar));
        window.insert_action_group("win", Some(self));
        let weak = self.downgrade();
        let closing = settings.clone();
        window.connect_close_request(move |detached| {
            save_playlist_window_size(detached, &closing);
            if let Some(window) = weak.upgrade() {
                window.imp().playlist_window.take();
                window.reattach_playlist();
                save_playlist_layout(&window, &closing);
            }
            glib::Propagation::Proceed
        });
        gtk::prelude::GtkWindowExt::present(&window);
        imp.playlist_window.replace(Some(window));
        imp.playlist_header.replace(Some(header));
        self.set_action_state("playlist-detach", true.to_variant());
        save_playlist_layout(self, settings);
    }

    /// Puts the playlist back beside the browse view, and closes the window it was in.
    fn attach_playlist(&self, settings: &gio::Settings) {
        let detached = self.imp().playlist_window.take();
        self.reattach_playlist();
        if let Some(window) = detached {
            window.close();
        }
        save_playlist_layout(self, settings);
    }

    /// Moves the playlist's widget back beside the browse view, and shows it there.
    fn reattach_playlist(&self) {
        let imp = self.imp();
        let Some(panel) = imp.playlist_panel.get() else { return };
        let content = panel.content();
        if let Some(parent) = content.parent().and_downcast::<gtk::Box>() {
            parent.remove(content);
        }
        imp.playlist_header.take();
        self.set_action_state("playlist-detach", false.to_variant());
        self.place_playlist();
    }

    /// Adds what the viewer shows, or the marked items while browsing, to the end of the playlist.
    fn add_current_to_playlist(&self) {
        let targets = self.playlist_targets();
        if targets.is_empty() {
            self.announce_playlist("Nothing is selected to add to the playlist");
        } else {
            self.add_to_playlist_later(targets, false);
        }
    }

    /// Asks for files whose pictures and videos are added to the playlist.
    fn choose_files_to_add(&self) {
        let dialog = gtk::FileDialog::builder().title("Add to Playlist").build();
        let weak = self.downgrade();
        dialog.open_multiple(Some(self), None::<&gio::Cancellable>, move |result| {
            let Some(files) = result.ok() else { return };
            let paths: Vec<PathBuf> = files.iter::<gio::File>().flatten().filter_map(|file| file.path()).collect();
            if let Some(window) = weak.upgrade() {
                window.add_to_playlist_later(paths, false);
            }
        });
    }

    /// Asks for a folder whose pictures and videos are added to the playlist.
    fn choose_directory_to_add(&self) {
        let dialog = gtk::FileDialog::builder().title("Add Folder to Playlist").build();
        let weak = self.downgrade();
        dialog.select_folder(Some(self), None::<&gio::Cancellable>, move |result| {
            let Some(path) = result.ok().and_then(|folder| folder.path()) else { return };
            if let Some(window) = weak.upgrade() {
                window.add_to_playlist_later(vec![path], false);
            }
        });
    }

    /// Takes the item the playlist is on out of it.
    fn remove_current_from_playlist(&self) {
        match playlist::with(|list| list.current()) {
            Some(index) if playlist::with(|list| list.remove_at(index)) => self.playlist_changed(),
            _ => self.announce_playlist("Nothing is playing to remove from the playlist"),
        }
    }

    /// Saves the playlist to the file it was loaded from or saved to, or asks for a file if it has none.
    fn save_playlist_current(&self) {
        match playlist::with(|list| list.source().map(Path::to_path_buf)) {
            Some(path) => self.write_playlist_file(path),
            None => self.choose_playlist_to_save(),
        }
    }

    /// Asks for the address of a playlist to load.
    fn ask_playlist_url(&self) {
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("https://example.com/playlist.m3u8"));
        entry.set_activates_default(true);
        let dialog = adw::AlertDialog::new(Some("Load Playlist from URL"), Some("Only pictures and videos on this computer are kept."));
        adw::prelude::AlertDialogExt::add_response(&dialog, "cancel", "Cancel");
        adw::prelude::AlertDialogExt::add_response(&dialog, "load", "Load");
        adw::prelude::AlertDialogExt::set_default_response(&dialog, Some("load"));
        adw::prelude::AlertDialogExt::set_close_response(&dialog, "cancel");
        adw::prelude::AlertDialogExt::set_extra_child(&dialog, Some(&entry));
        let weak = self.downgrade();
        adw::prelude::AlertDialogExt::connect_response(&dialog, None, move |_, response| {
            let url = entry.text().trim().to_owned();
            if response != "load" || url.is_empty() {
                return;
            }
            if let Some(window) = weak.upgrade() {
                window.load_playlist_url(url);
            }
        });
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Reads the playlist at `url`, in whatever scheme the address names, and offers it as a file would be. Its items are
    /// read as absolute paths, since a remote playlist has no folder of its own to find them in.
    fn load_playlist_url(&self, url: String) {
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let fetch = url.clone();
            let read = gio::spawn_blocking(move || {
                gio::File::for_uri(&fetch)
                    .load_contents(None::<&gio::Cancellable>)
                    .map(|(bytes, _)| playlist_from_bytes(&bytes, Path::new("/")))
            })
            .await;
            let Some(window) = weak.upgrade() else { return };
            match read {
                Ok(Ok(loaded)) => window.offer_loaded_playlist(&url, loaded, None),
                Ok(Err(err)) => window.report_playlist_problem(&format!("Could not read {url}: {err}")),
                Err(_) => window.report_playlist_problem(&format!("Reading {url} panicked")),
            }
        });
    }

    /// Asks for a playlist file, and loads it.
    fn choose_playlist_to_load(&self) {
        let dialog = gtk::FileDialog::builder().title("Load Playlist").filters(&playlist_file_filters()).build();
        let weak = self.downgrade();
        dialog.open(Some(self), None::<&gio::Cancellable>, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else { return };
            if let Some(window) = weak.upgrade() {
                window.load_playlist_file(path);
            }
        });
    }

    /// Reads the playlist file `path` on a worker thread, then takes what it holds into the playlist, asking first
    /// whether to replace what is there.
    fn load_playlist_file(&self, path: PathBuf) {
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
            let source = path.clone();
            let read = gio::spawn_blocking(move || read_playlist_file(&path)).await;
            let Some(window) = weak.upgrade() else { return };
            let loaded = match read {
                Ok(Ok(loaded)) => loaded,
                Ok(Err(err)) => {
                    window.report_playlist_problem(&format!("Could not read {name}: {err}"));
                    return;
                }
                Err(_) => {
                    window.report_playlist_problem(&format!("Reading {name} panicked"));
                    return;
                }
            };
            window.offer_loaded_playlist(&name, loaded, Some(source));
        });
    }

    /// Offers a playlist that was read. It becomes the playlist if the playlist is empty; otherwise the user is asked whether
    /// it replaces the playlist or is added to its end. `source` is the file it was read from, if any.
    fn offer_loaded_playlist(&self, name: &str, loaded: format::Parsed, source: Option<PathBuf>) {
        if loaded.paths.is_empty() {
            self.report_playlist_problem(&format!("{name} has no pictures or videos on this computer"));
        } else if playlist::with(|list| list.is_empty()) {
            self.take_loaded_playlist(loaded, true, source);
        } else {
            self.confirm_replace_playlist(name, loaded, source);
        }
    }

    /// Asks whether the file called `name` replaces the playlist or is added to its end.
    fn confirm_replace_playlist(&self, name: &str, loaded: format::Parsed, source: Option<PathBuf>) {
        let dialog = adw::AlertDialog::new(Some("Load Playlist"), Some(&format!("The playlist already has items. Replace them with {name}, or add it to the end?")));
        adw::prelude::AlertDialogExt::add_response(&dialog, "cancel", "Cancel");
        adw::prelude::AlertDialogExt::add_response(&dialog, "add", "Add to End");
        adw::prelude::AlertDialogExt::add_response(&dialog, "replace", "Replace");
        adw::prelude::AlertDialogExt::set_response_appearance(&dialog, "replace", adw::ResponseAppearance::Destructive);
        adw::prelude::AlertDialogExt::set_default_response(&dialog, Some("add"));
        adw::prelude::AlertDialogExt::set_close_response(&dialog, "cancel");
        let weak = self.downgrade();
        let loaded = RefCell::new(Some((loaded, source)));
        adw::prelude::AlertDialogExt::connect_response(&dialog, None, move |_, response| {
            let (Some(window), Some((loaded, source))) = (weak.upgrade(), loaded.take()) else { return };
            match response {
                "replace" => window.take_loaded_playlist(loaded, true, source),
                "add" => window.take_loaded_playlist(loaded, false, None),
                _ => {}
            }
        });
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Makes the playlist what was loaded, or adds it to the end, and says how that went.
    fn take_loaded_playlist(&self, loaded: format::Parsed, replace: bool, source: Option<PathBuf>) {
        let format::Parsed { paths, titles, skipped } = loaded;
        let entries = paths.into_iter().zip(titles);
        let count = playlist::with(|list| {
            if replace {
                list.clear();
                list.set_source(source);
                list.add_with_titles(entries);
                list.len()
            } else {
                list.add_with_titles(entries)
            }
        });
        let mut text = format!("Loaded {count} {}", if count == 1 { "item" } else { "items" });
        if skipped > 0 {
            text.push_str(&format!(", leaving out {skipped} that {} not a picture or video on this computer", if skipped == 1 { "is" } else { "are" }));
        }
        self.announce_playlist(&text);
        self.playlist_changed();
    }

    /// Asks where to save the playlist, and saves it there.
    fn choose_playlist_to_save(&self) {
        if playlist::with(|list| list.is_empty()) {
            self.announce_playlist("The playlist is empty");
            return;
        }
        let dialog = gtk::FileDialog::builder().title("Save Playlist").initial_name("playlist.m3u8").filters(&playlist_file_filters()).build();
        let weak = self.downgrade();
        dialog.save(Some(self), None::<&gio::Cancellable>, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else { return };
            if let Some(window) = weak.upgrade() {
                window.write_playlist_file(path);
            }
        });
    }

    /// Writes the playlist to `path`, in the format its extension names. A name without one becomes an M3U8 file.
    fn write_playlist_file(&self, path: PathBuf) {
        let (path, file_format) = match Format::from_path(&path) {
            Some(file_format) => (path, file_format),
            None => {
                let mut name = path.into_os_string();
                name.push(".m3u8");
                (PathBuf::from(name), Format::M3u)
            }
        };
        let text = playlist::with(|list| {
            let entries: Vec<(PathBuf, String)> = list.order().iter().map(|path| (path.clone(), list.title(path))).collect();
            format::write(file_format, &entries)
        });
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
            let saved = path.clone();
            let written = gio::spawn_blocking(move || crate::atomic_file::write(&path, text.as_bytes())).await;
            let Some(window) = weak.upgrade() else { return };
            match written {
                Ok(Ok(())) => {
                    playlist::with(|list| list.set_source(Some(saved)));
                    window.announce_playlist(&format!("Saved the playlist as {name}"));
                }
                Ok(Err(err)) => window.report_playlist_problem(&format!("Could not save {name}: {err}")),
                Err(_) => window.report_playlist_problem(&format!("Saving {name} panicked")),
            }
        });
    }

    /// Sorts the playlist by `column`, highest first if `descending`, and shows the change. Length is read first where it
    /// is not known, so that sort waits for those readings.
    fn sort_playlist(&self, column: SortColumn, descending: bool) {
        match column {
            SortColumn::Name => {
                playlist::with(|list| {
                    let keys: HashMap<PathBuf, String> = list.order().iter().map(|path| (path.clone(), list.title(path).to_lowercase())).collect();
                    list.sort_shown_by(|path| keys[path].clone(), descending);
                });
                self.playlist_changed();
            }
            SortColumn::Filename => {
                playlist::with(|list| list.sort_shown_by(|path| path.to_string_lossy().to_lowercase(), descending));
                self.playlist_changed();
            }
            SortColumn::Length => self.sort_playlist_by_length(descending),
        }
    }

    /// Sorts the playlist by length. The lengths are read from the files where the metadata is not known, and the sort
    /// happens once they all are.
    fn sort_playlist_by_length(&self, descending: bool) {
        let info = MediaInfo::shared();
        let paths = playlist::with(|list| list.order().to_vec());
        let finish = {
            let weak = self.downgrade();
            let info = info.clone();
            Rc::new(move || {
                let Some(window) = weak.upgrade() else { return };
                playlist::with(|list| {
                    let keys: HashMap<PathBuf, i64> = list.order().iter().map(|path| (path.clone(), duration_millis(&info, path))).collect();
                    list.sort_shown_by(|path| keys[path], descending);
                });
                window.playlist_changed();
            })
        };
        let waiting = Rc::new(Cell::new(0usize));
        for path in &paths {
            let Some((mtime, size)) = stamp(path) else { continue };
            waiting.set(waiting.get() + 1);
            let done = {
                let waiting = waiting.clone();
                let finish = finish.clone();
                move || {
                    waiting.set(waiting.get() - 1);
                    if waiting.get() == 0 {
                        finish();
                    }
                }
            };
            if info.request(path, mtime, size, || true, done).is_some() {
                waiting.set(waiting.get() - 1);
            }
        }
        if waiting.get() == 0 {
            finish();
        }
    }

    /// Asks for the title of the item at `index` in the playlist, and gives it the one entered. An empty title takes back
    /// the file name.
    fn edit_playlist_title(&self, index: usize) {
        let Some(path) = playlist::with(|list| list.order().get(index).cloned()) else { return };
        let title = playlist::with(|list| list.title(&path));
        let entry = gtk::Entry::new();
        entry.set_text(&title);
        entry.set_activates_default(true);
        let dialog = adw::AlertDialog::new(Some("Edit Title"), Some("The title is shown in the playlist. Leave it empty to show the file name."));
        adw::prelude::AlertDialogExt::add_response(&dialog, "cancel", "Cancel");
        adw::prelude::AlertDialogExt::add_response(&dialog, "save", "Save");
        adw::prelude::AlertDialogExt::set_default_response(&dialog, Some("save"));
        adw::prelude::AlertDialogExt::set_close_response(&dialog, "cancel");
        adw::prelude::AlertDialogExt::set_extra_child(&dialog, Some(&entry));
        let weak = self.downgrade();
        adw::prelude::AlertDialogExt::connect_response(&dialog, None, move |_, response| {
            if response != "save" {
                return;
            }
            playlist::with(|list| list.set_title(&path, &entry.text()));
            if let Some(window) = weak.upgrade() {
                window.playlist_changed();
            }
        });
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Asks whether the file at `path` is deleted from the disk. It is deleted for good, not moved to the trash, and then
    /// it is taken out of the playlist.
    fn confirm_delete_from_disk(&self, path: PathBuf) {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
        let dialog = adw::AlertDialog::new(Some("Delete from Disk?"), Some(&format!("{name} will be deleted for good. This cannot be undone.")));
        adw::prelude::AlertDialogExt::add_response(&dialog, "cancel", "Cancel");
        adw::prelude::AlertDialogExt::add_response(&dialog, "delete", "Delete");
        adw::prelude::AlertDialogExt::set_response_appearance(&dialog, "delete", adw::ResponseAppearance::Destructive);
        adw::prelude::AlertDialogExt::set_default_response(&dialog, Some("cancel"));
        adw::prelude::AlertDialogExt::set_close_response(&dialog, "cancel");
        let weak = self.downgrade();
        adw::prelude::AlertDialogExt::connect_response(&dialog, None, move |_, response| {
            if response != "delete" {
                return;
            }
            if let Some(window) = weak.upgrade() {
                window.delete_from_disk(&path, &name);
            }
        });
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Deletes the file at `path` from the disk, and takes it out of the playlist, if it could be deleted.
    fn delete_from_disk(&self, path: &Path, name: &str) {
        if let Err(err) = std::fs::remove_file(path) {
            self.notify_error(&format!("Could not delete {name}: {err}"));
            return;
        }
        self.remove_from_playlist(&[path.to_path_buf()]);
    }

    /// Shows the file or folder at `path` in the browse view, with it selected in its folder, leaving the viewer if it is shown.
    fn reveal_path(&self, path: &Path) {
        if self.viewer_is_open() {
            self.close_viewer();
        }
        self.open_path(path, Destination::CurrentTab, true);
    }

    /// Sets the state of this window's action `name`, which shows the playlist's state.
    fn set_action_state(&self, name: &str, state: glib::Variant) {
        if let Some(action) = self.lookup_action(name).and_downcast::<gio::SimpleAction>() {
            action.set_state(&state);
        }
    }

    /// Asks whether to clear the playlist, and clears it.
    fn confirm_clear_playlist(&self) {
        let len = playlist::with(|list| list.len());
        if len == 0 {
            return;
        }
        let dialog = adw::AlertDialog::new(Some("Clear the Playlist?"), Some(&format!("{len} {} will be taken off the playlist. The files stay where they are.", if len == 1 { "item" } else { "items" })));
        adw::prelude::AlertDialogExt::add_response(&dialog, "cancel", "Cancel");
        adw::prelude::AlertDialogExt::add_response(&dialog, "clear", "Clear");
        adw::prelude::AlertDialogExt::set_response_appearance(&dialog, "clear", adw::ResponseAppearance::Destructive);
        adw::prelude::AlertDialogExt::set_default_response(&dialog, Some("cancel"));
        adw::prelude::AlertDialogExt::set_close_response(&dialog, "cancel");
        let weak = self.downgrade();
        adw::prelude::AlertDialogExt::connect_response(&dialog, None, move |_, response| {
            if response != "clear" {
                return;
            }
            playlist::with(|list| list.clear());
            if let Some(window) = weak.upgrade() {
                window.playlist_changed();
            }
        });
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Says `text` over the window, at the top, clear of the video controls. A message still up from the last time is
    /// taken down, so going through the playlist quickly does not stack them.
    fn announce_playlist(&self, text: &str) {
        let messages = self.messages();
        if let Some(previous) = self.imp().playlist_notice.take() {
            messages.dismiss(previous);
        }
        let notice = messages.show(Message::notification(text).at(Position::TopCenter));
        self.imp().playlist_notice.set(Some(notice));
    }

    /// Tells the user, and the terminal, what went wrong with a playlist file.
    fn report_playlist_problem(&self, problem: &str) {
        eprintln!("rsimgv: {problem}");
        self.notify_error(problem);
    }

    /// Makes the side buttons run what their Mouse settings say, and the scroll wheel over the viewer do what its
    /// settings say.
    fn bind_pointer(&self) {
        let buttons = gtk::GestureClick::new();
        buttons.set_button(0);
        buttons.connect_pressed({
            let weak = self.downgrade();
            move |gesture, _, _, _| {
                let Some(window) = weak.upgrade() else { return };
                match gesture.current_button() {
                    8 => window.run_mouse_setting("mouse-back"),
                    9 => window.run_mouse_setting("mouse-forward"),
                    _ => {}
                }
            }
        });
        self.add_controller(buttons);

        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.connect_scroll({
            let weak = self.downgrade();
            move |controller, _, dy| {
                let Some(window) = weak.upgrade() else { return glib::Propagation::Proceed };
                let ctrl = controller.current_event_state().contains(gtk::gdk::ModifierType::CONTROL_MASK);
                window.scroll_viewer(dy, ctrl)
            }
        });
        self.imp().viewer.add_controller(wheel);
    }

    /// Runs the window or viewer action that the mouse setting `setting` names, or nothing for `none`.
    fn run_mouse_setting(&self, setting: &str) {
        let Some(settings) = self.imp().settings.get() else { return };
        let action = match settings.string(setting).as_str() {
            "back" => "win.navigate-back",
            "up" => "win.navigate-up",
            "forward" => "win.navigate-forward",
            "previous" if self.viewer_is_open() => "view.previous",
            "previous" => "browse.cursor-previous",
            "next" if self.viewer_is_open() => "view.next",
            "next" => "browse.cursor-next",
            _ => return,
        };
        self.run_action(action, None);
    }

    /// Runs what the scroll wheel does over the viewer's picture or video, as the Mouse settings say, scaled by the
    /// scroll speed. Returns whether it used the scroll, so nothing else scrolls too.
    fn scroll_viewer(&self, dy: f64, ctrl: bool) -> glib::Propagation {
        let Some(settings) = self.imp().settings.get() else { return glib::Propagation::Proceed };
        let viewer = &self.imp().viewer;
        let speed = f64::from(settings.int("scroll-speed")) / 100.0;
        // A trackpad sends small amounts, so a notch counts as at most one.
        let notch = dy.clamp(-1.0, 1.0);
        let used = if viewer.shows_video() {
            match settings.string("scroll-video").as_str() {
                "volume" => self.run_action("video.volume", Some(-notch * WHEEL_VOLUME_STEP * speed)),
                "seek" => self.run_action("video.seek", Some(-notch * WHEEL_SEEK_STEP * speed)),
                "step" => self.step_by_scroll(notch),
                _ => false,
            }
        } else {
            match settings.string("scroll-image").as_str() {
                "ctrl-zoom" if ctrl => self.zoom_by_scroll(notch * speed),
                "zoom" => self.zoom_by_scroll(notch * speed),
                "step" => self.step_by_scroll(notch),
                _ => false,
            }
        };
        if used { glib::Propagation::Stop } else { glib::Propagation::Proceed }
    }

    /// Zooms the picture by `notches` of the scroll wheel. Always returns true, since the zoom uses the scroll.
    fn zoom_by_scroll(&self, notches: f64) -> bool {
        self.imp().viewer.zoom_wheel(notches);
        true
    }

    /// Moves to the item before or after the viewer's, as a scroll up or down asks. Returns whether it moved.
    fn step_by_scroll(&self, notch: f64) -> bool {
        if notch == 0.0 {
            return false;
        }
        self.run_action(if notch < 0.0 { "view.previous" } else { "view.next" }, None)
    }

    /// Runs the action `name` on this window, as a key binding would, passing `value` if the action takes one.
    /// Returns whether it ran.
    pub fn run_action(&self, name: &str, value: Option<f64>) -> bool {
        WidgetExt::activate_action(self, name, action_parameter(name, value).as_ref()).is_ok()
    }

    /// Asks for a name for the folder shown, then bookmarks it.
    fn bookmark_current_folder(&self) {
        let Some(path) = self.current_path() else { return };
        self.ask_bookmark_name("Bookmark Folder", path);
    }

    /// Asks for a name for the bookmark of `path`, then saves it. The name offered is the bookmark's own, or else the
    /// folder's.
    fn ask_bookmark_name(&self, heading: &str, path: PathBuf) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(&path.display().to_string()));
        adw::prelude::AlertDialogExt::add_response(&dialog, "cancel", "Cancel");
        adw::prelude::AlertDialogExt::add_response(&dialog, "save", "Save");
        adw::prelude::AlertDialogExt::set_response_appearance(&dialog, "save", adw::ResponseAppearance::Suggested);
        adw::prelude::AlertDialogExt::set_default_response(&dialog, Some("save"));
        adw::prelude::AlertDialogExt::set_close_response(&dialog, "cancel");

        let entry = gtk::Entry::new();
        let current = bookmarks::with(|list| list.iter().find(|bookmark| bookmark.path == path).map(|bookmark| bookmark.name.clone()));
        entry.set_text(&current.unwrap_or_else(|| bookmarks::default_name(&path)));
        entry.set_activates_default(true);
        adw::prelude::AlertDialogExt::set_extra_child(&dialog, Some(&entry));

        let weak = self.downgrade();
        adw::prelude::AlertDialogExt::connect_response(&dialog, None, move |_, response| {
            if response != "save" {
                return;
            }
            let typed = entry.text().trim().to_owned();
            let name = if typed.is_empty() { bookmarks::default_name(&path) } else { typed };
            bookmarks::with(|list| bookmarks::add(list, path.clone(), name));
            if let Some(window) = weak.upgrade() {
                window.bookmarks_changed();
            }
        });
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Saves the bookmarks and shows them in the sidebar.
    fn bookmarks_changed(&self) {
        if let Err(err) = bookmarks::save() {
            eprintln!("rsimgv: could not save bookmarks: {err}");
        }
        self.refresh_bookmarks();
    }

    /// Shows the bookmarks in the sidebar, in their saved order.
    fn refresh_bookmarks(&self) {
        let list = &self.imp().bookmarks_list;
        list.remove_all();
        let saved: Vec<Bookmark> = bookmarks::with(|bookmarks| bookmarks.clone());
        for bookmark in saved {
            list.append(&self.bookmark_row(&bookmark));
        }
        self.sync_bookmark_selection();
    }

    /// Highlights the bookmark of the folder shown, if it has one.
    fn sync_bookmark_selection(&self) {
        let list = &self.imp().bookmarks_list;
        let current = self.current_path();
        let position = bookmarks::with(|bookmarks| bookmarks.iter().position(|bookmark| Some(&bookmark.path) == current.as_ref()));
        let row = position.and_then(|position| list.row_at_index(position as i32));
        list.select_row(row.as_ref());
    }

    /// Moves the bookmark at `from` to position `to`, and saves the bookmarks.
    fn move_bookmark(&self, from: usize, to: usize) {
        bookmarks::with(|list| bookmarks::move_to(list, from, to));
        self.bookmarks_changed();
    }

    /// A row of the bookmarks list. A click opens the folder, a middle-click opens it in a new tab, and a right-click
    /// shows a menu. Dragging the row onto another reorders the list.
    fn bookmark_row(&self, bookmark: &Bookmark) -> gtk::ListBoxRow {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&gtk::Image::from_icon_name(bookmark_icon(&bookmark.path)));
        let label = gtk::Label::new(Some(&bookmark.name));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&label);

        let remove = gtk::Button::from_icon_name("window-close-symbolic");
        remove.set_tooltip_text(Some("Remove bookmark"));
        remove.add_css_class("flat");
        remove.add_css_class("rsimgv-bookmark-remove");
        remove.set_focus_on_click(false);
        remove.set_valign(gtk::Align::Center);
        let weak = self.downgrade();
        let path = bookmark.path.clone();
        remove.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.remove_bookmark(&path);
            }
        });
        content.append(&remove);

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&content));
        row.set_tooltip_text(Some(&bookmark.path.display().to_string()));

        let menu = gtk::GestureClick::new();
        menu.set_button(gdk::BUTTON_SECONDARY);
        let weak = self.downgrade();
        let path = bookmark.path.clone();
        menu.connect_pressed(move |gesture, _, x, y| {
            if let (Some(window), Some(row)) = (weak.upgrade(), gesture.widget()) {
                window.show_bookmark_menu(&row, &path, x, y);
            }
        });
        row.add_controller(menu);

        let middle = gtk::GestureClick::new();
        middle.set_button(gdk::BUTTON_MIDDLE);
        let weak = self.downgrade();
        let path = bookmark.path.clone();
        middle.connect_pressed(move |_, _, _, _| {
            if let Some(window) = weak.upgrade().filter(|_| path.is_dir()) {
                window.open_background_tab(&path);
            }
        });
        row.add_controller(middle);

        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE);
        drag.connect_prepare(|drag, _, _| {
            let row = drag.widget().and_downcast::<gtk::ListBoxRow>()?;
            Some(gdk::ContentProvider::for_value(&row.index().to_value()))
        });
        drag.connect_drag_begin(|drag, _| {
            if let Some(row) = drag.widget() {
                drag.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
            }
        });
        row.add_controller(drag);

        let drop = gtk::DropTarget::new(i32::static_type(), gdk::DragAction::MOVE);
        let weak = self.downgrade();
        drop.connect_drop(move |drop, value, _, _| {
            let (Some(row), Ok(from)) = (drop.widget().and_downcast::<gtk::ListBoxRow>(), value.get::<i32>()) else {
                return false;
            };
            let to = row.index();
            let weak = weak.clone();
            // Moving a bookmark rebuilds the rows, which must outlast the drop being handled.
            glib::idle_add_local_once(move || {
                if let Some(window) = weak.upgrade() {
                    window.move_bookmark(from as usize, to as usize);
                }
            });
            true
        });
        row.add_controller(drop);
        row
    }

    /// Opens `path` from the command line, as `destination` says. A folder is shown in its tab, or navigated
    /// to. A file is shown with its folder, and selected there, and a picture or video also opens in the viewer
    /// when its tab is the one shown. With `select_item`, `path` is never opened: its containing folder is shown
    /// with `path` selected, whether `path` is a file or a folder. Returns `false`, opening nothing, if `path`
    /// is neither.
    pub fn open_path(&self, path: &Path, destination: Destination, select_item: bool) -> bool {
        if !path.is_dir() && !path.is_file() {
            return false;
        }
        let (folder, file, open) = if select_item {
            (path.parent(), Some(path), false)
        } else if path.is_dir() {
            (Some(path), None, false)
        } else {
            (path.parent(), Some(path), true)
        };
        let Some(folder) = folder else { return false };
        let (current, activate) = match destination {
            Destination::CurrentTab if self.imp().tab_view.n_pages() > 0 => (true, true),
            Destination::CurrentTab => (false, true),
            Destination::NewTab { activate } => (false, activate),
        };
        let open = open && activate;
        if !current {
            let id = self.open_tab_at(folder, activate);
            if let Some(file) = file {
                let Some(browse) = self.page_for_tab(id).and_then(|page| page.child().downcast::<BrowseView>().ok())
                else {
                    return false;
                };
                self.show_file_once_listed(&browse, file, open);
            }
            return true;
        }
        let Some(file) = file else { return self.navigate(folder) };
        let Some(browse) = self.active_browse() else { return false };
        if self.current_path().as_deref() == Some(folder) {
            self.show_file_in(&browse, file, open);
            return true;
        }
        self.show_file_once_listed(&browse, file, open);
        if self.navigate(folder) {
            return true;
        }
        browse.clear_pending();
        false
    }

    /// Opens the default folder, the one a new window starts in, in a new tab that is activated.
    pub fn open_new_tab_at_default(&self) {
        if let Some(settings) = self.imp().settings.get() {
            self.open_tab(&start_location(settings));
        }
    }

    /// Shows the file `path` in `browse` once its folder has listed: in the viewer if `open` is set and it is a
    /// picture or video, and otherwise selected.
    fn show_file_once_listed(&self, browse: &BrowseView, path: &Path, open: bool) {
        let file = path.to_path_buf();
        let weak = self.downgrade();
        let folder = path.parent().unwrap_or(path);
        browse.set_pending(folder, move |view| {
            if let Some(window) = weak.upgrade() {
                window.show_file_in(view, &file, open);
            }
        });
    }

    /// Shows a file dropped on the viewer: in the viewer, in the active tab's folder. A file in another folder
    /// first browses to its folder, and is shown once that has listed.
    fn open_dropped_file(&self, path: &Path) {
        let Some(browse) = self.active_browse() else { return };
        let Some(folder) = path.parent() else { return };
        if browse.path().as_deref() == Some(folder) {
            self.open_viewer(&browse, path);
        } else {
            self.navigate(folder);
            self.show_file_once_listed(&browse, path, true);
        }
    }

    /// Shows the file `path` in `browse`: in the viewer if `open` is set and it is a picture or video, and
    /// otherwise selected.
    fn show_file_in(&self, browse: &BrowseView, path: &Path, open: bool) {
        if open && browse.media_paths().iter().any(|listed| listed == path) {
            self.open_viewer(browse, path);
        } else {
            browse.select_named(&path.to_string_lossy());
        }
    }

    /// Opens a folder the user picks.
    fn open_folder(&self) {
        let dialog = gtk::FileDialog::builder().title("Open Folder").build();
        let weak = self.downgrade();
        dialog.select_folder(Some(self), None::<&gio::Cancellable>, move |result| {
            let Some(path) = result.ok().and_then(|folder| folder.path()) else { return };
            if let Some(window) = weak.upgrade() {
                window.navigate(&path);
            }
        });
    }

    /// Flips a boolean setting.
    fn toggle_setting(&self, key: &str) {
        if let Some(settings) = self.imp().settings.get() {
            // The keys are in the schema, so the write cannot fail.
            let _ = settings.set_boolean(key, !settings.boolean(key));
        }
    }

    /// Shows the browse items' filenames, or hides them.
    fn toggle_filename(&self) {
        if let Some(settings) = self.imp().settings.get() {
            let shown = settings.string("filename-display") != "off";
            let _ = settings.set_string("filename-display", if shown { "off" } else { "ellipsized" });
        }
    }

    /// Shows the browse items' information, or hides it.
    fn toggle_item_info(&self) {
        if let Some(settings) = self.imp().settings.get() {
            let shown = settings.string("info-layout") != "disabled";
            let _ = settings.set_string("info-layout", if shown { "disabled" } else { "inline" });
        }
    }

    /// Makes the browse items larger or smaller by `delta` pixels, within the setting's range.
    fn resize_items(&self, delta: i32) {
        if let Some(settings) = self.imp().settings.get() {
            let size = (settings.int("item-size") + delta).clamp(ITEM_SIZE_MIN, ITEM_SIZE_MAX);
            let _ = settings.set_int("item-size", size);
        }
    }

    /// Runs `f` on the browse view of the selected tab, if it has focus. Browse keys only apply to the
    /// list, so the folder tree keeps its own arrow keys.
    fn with_browse(&self, f: impl FnOnce(&BrowseView)) {
        if let Some(view) = self.active_browse() {
            f(&view);
        }
    }

    /// The scopes a key press can be bound in right now: the browse context while the media viewer is closed,
    /// and while it is open, the video or image context for the item it shows, plus the viewer's own actions.
    /// The playlist or folder tree context is added while that has the focus. Global applies everywhere and is not listed.
    pub(super) fn active_scopes(&self) -> Vec<Scope> {
        let imp = self.imp();
        let mut scopes = if !imp.viewer_open.get() {
            vec![Scope::Browse]
        } else {
            let media = if imp.viewer.shows_video() { Scope::Video } else { Scope::Image };
            vec![Scope::View, media]
        };
        if imp.playlist_panel.get().is_some_and(PlaylistPanel::has_focus) {
            scopes.push(Scope::Playlist);
        }
        if self.focus_is_within(&*imp.tree) {
            scopes.push(Scope::Tree);
        }
        scopes
    }

    /// Whether the media viewer is open.
    /// The media the viewer shows, at its own size, while it is open.
    pub fn viewer_screenshot(&self) -> Option<gdk::Texture> {
        if !self.viewer_is_open() {
            return None;
        }
        self.imp().viewer.screenshot()
    }

    /// The item the viewer shows, while the viewer is open. See `MediaViewer::shown_item`.
    pub fn viewer_item(&self) -> Option<ShownItem> {
        if !self.viewer_is_open() {
            return None;
        }
        self.imp().viewer.shown_item()
    }

    /// The overlay that shows messages over the window.
    pub fn messages(&self) -> &MessageOverlay {
        self.imp().messages.get_or_init(|| MessageOverlay::new(&self.imp().message_layer))
    }

    /// Shows `text` over the window for a few seconds.
    pub fn notify(&self, text: &str) {
        self.messages().show(Message::notification(text));
    }

    /// Shows the list of the keybindings in force where the user is, or hides it.
    fn toggle_keybinds_overlay(&self) {
        let overlay = self.imp().keybinds.get_or_init(|| KeybindsOverlay::new(&self.keybinds_host()));
        if overlay.is_shown() {
            overlay.hide();
        } else {
            overlay.move_to(&self.keybinds_host());
            overlay.show(&self.active_scopes());
        }
    }

    /// The overlay the keybindings list sits over: the picture of the viewer while it is open, so the list stays within
    /// it, or the whole window otherwise.
    fn keybinds_host(&self) -> gtk::Overlay {
        let imp = self.imp();
        if self.viewer_is_open() { imp.viewer.overlay() } else { (*imp.message_layer).clone() }
    }

    /// Makes the list of keybindings, if it is up, the ones in force now, as the viewer opened or closed or showed
    /// another kind of item.
    fn refresh_keybinds_overlay(&self) {
        if let Some(overlay) = self.imp().keybinds.get().filter(|overlay| overlay.is_shown()) {
            overlay.move_to(&self.keybinds_host());
            overlay.rebuild(&self.active_scopes());
        }
    }

    /// Shows `message` over the window as a notification of `level`, one of `NOTIFICATION_LEVELS` (an unknown one is
    /// `info`), for `seconds`.
    pub fn show_notification(&self, message: &str, level: &str, seconds: f64) -> MessageId {
        let seconds = if seconds.is_finite() { seconds.clamp(0.1, MAX_NOTIFICATION_SECONDS) } else { DEFAULT_NOTIFICATION_SECONDS };
        let level = Level::from_name(level).unwrap_or_default();
        self.messages().show(Message::notification(message).level(level).lasting(std::time::Duration::from_secs_f64(seconds)))
    }

    /// Names `folder` over the window, as the viewer moves to it. A name still up from the last move is taken down.
    fn announce_folder(&self, folder: &Path) {
        let name = folder.file_name().map_or_else(|| folder.display().to_string(), |name| name.to_string_lossy().into_owned());
        let messages = self.messages();
        if let Some(previous) = self.imp().folder_notice.take() {
            messages.dismiss(previous);
        }
        // At the top, clear of the video controls that come up at the bottom.
        let notice = messages.show(Message::notification(name).at(Position::TopCenter));
        self.imp().folder_notice.set(Some(notice));
    }

    /// Shows `text` over the window for a few seconds, as something that went wrong.
    pub fn notify_error(&self, text: &str) {
        self.messages().show(Message::notification(text).level(Level::Error));
    }

    /// The item a script is run on: the one the viewer shows, or while browsing, the one under the cursor.
    fn script_target(&self) -> Option<PathBuf> {
        if let Some(item) = self.viewer_item() {
            return Some(item.path);
        }
        let browse = self.active_browse()?;
        Some(browse.path()?.join(browse.selected_name()?))
    }

    /// Runs the script called `name` on the item in front of the user. A script that waits is followed to its end,
    /// and the folder is listed again then, as the script may have changed what is in it. Problems are printed.
    pub fn run_script(&self, name: &str) {
        self.run_script_on(name, self.script_target());
    }

    /// Runs the script called `name` on `target`, the item it runs on if it uses one. Otherwise as `run_script`.
    pub fn run_script_on(&self, name: &str, target: Option<PathBuf>) {
        let Some(settings) = self.imp().settings.get() else { return };
        let Some(script) = crate::scripts::find_script(settings, name) else {
            self.report_script_problem(&format!("There is no script called {name}"));
            return;
        };
        if script.uses_file() && target.is_none() {
            self.report_script_problem(&format!("{name} needs an item to run on, and none is selected"));
            return;
        }
        let mut child = match script.spawn(target.as_deref().filter(|_| script.uses_file())) {
            Ok(child) => child,
            Err(err) => {
                self.report_script_problem(&format!("Could not run {name}: {err}"));
                return;
            }
        };
        let weak = self.downgrade();
        let wait = script.wait;
        glib::spawn_future_local(async move {
            // Waiting reaps the script either way, so a script that is not followed leaves no process behind.
            let status = gio::spawn_blocking(move || child.wait()).await;
            let problem = match status {
                Ok(Ok(status)) if !status.success() => Some(format!("{} ended with {status}", script.name)),
                Ok(Err(err)) => Some(format!("Could not wait for {}: {err}", script.name)),
                Err(_) => Some(format!("Waiting for {} panicked", script.name)),
                Ok(Ok(_)) => None,
            };
            let Some(window) = weak.upgrade() else { return };
            if let Some(problem) = problem {
                window.report_script_problem(&problem);
            }
            if wait {
                window.reload_folder();
            }
        });
    }

    /// Tells the user, and the terminal, what went wrong with a script.
    fn report_script_problem(&self, problem: &str) {
        eprintln!("rsimgv: {problem}");
        self.notify_error(problem);
    }

    /// Lists the folder shown again, keeping the item selected.
    fn reload_folder(&self) {
        let (Some(browse), Some(path)) = (self.active_browse(), self.current_path()) else { return };
        if let Some(name) = browse.selected_name() {
            browse.set_pending(&path, move |view| {
                view.select_named(&name);
            });
        }
        browse.set_path(&path);
    }

    pub fn viewer_is_open(&self) -> bool {
        self.imp().viewer_open.get()
    }

    /// Moves to the next or previous folder that holds a picture or video, in depth-first order of the folder tree.
    /// Browsing, the folder's first picture or video is selected going forward, and its last going back. In the
    /// viewer, the first picture or video is shown either way. Returns the folder moved to, or `None` when there is no
    /// such folder, in which case the window stays where it is.
    pub fn step_media_folder(&self, forward: bool) -> Option<PathBuf> {
        let current = self.current_path()?;
        let target = walk_folder(&current, self.show_hidden(), forward, true)?;
        self.show_media_folder(target, forward, false)
    }

    /// Shows the next folder, or the previous one, in depth-first order of the folder tree. Returns the folder shown,
    /// or `None` when there is no such folder, in which case the window stays where it is.
    pub fn step_folder(&self, forward: bool) -> Option<PathBuf> {
        let current = self.current_path()?;
        let target = walk_folder(&current, self.show_hidden(), forward, false)?;
        self.navigate(&target).then_some(target)
    }

    /// Steps to the next or previous folder with media, as `step_media_folder` does, for the keys. The tree is walked
    /// on a worker thread, so a large or slow tree does not freeze the window while it is searched.
    pub fn step_media_folder_later(&self, forward: bool) {
        self.walk_media_folder_later(forward, false);
    }

    /// Finds the next or previous folder with media on a worker thread and shows it, as `show_media_folder` does.
    fn walk_media_folder_later(&self, forward: bool, viewer_last: bool) {
        let Some(current) = self.current_path() else { return };
        let show_hidden = self.show_hidden();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let walked = gio::spawn_blocking(move || walk_folder(&current, show_hidden, forward, true)).await;
            if let (Some(window), Some(target)) = (weak.upgrade(), walked.ok().flatten()) {
                window.show_media_folder(target, forward, viewer_last);
            }
        });
    }

    /// Shows the next or previous folder, as `step_folder` does, for the keys. The tree is walked on a worker thread.
    pub fn step_folder_later(&self, forward: bool) {
        let Some(current) = self.current_path() else { return };
        let show_hidden = self.show_hidden();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let walked = gio::spawn_blocking(move || walk_folder(&current, show_hidden, forward, false)).await;
            if let (Some(window), Some(target)) = (weak.upgrade(), walked.ok().flatten()) {
                window.navigate(&target);
            }
        });
    }

    /// Whether hidden folders are listed, from the settings.
    fn show_hidden(&self) -> bool {
        self.imp().settings.get().is_some_and(|settings| settings.boolean("show-hidden"))
    }

    /// Opens the folder `target` that a media-folder step found: in the viewer its first picture or video is shown, or
    /// its last with `viewer_last`, and while browsing its first or last is selected, once the folder has listed.
    /// Returns the folder moved to, or `None` when the window could not move there.
    fn show_media_folder(&self, target: PathBuf, forward: bool, viewer_last: bool) -> Option<PathBuf> {
        // The arrangement waits for the folder to list, so it is set before the folder is shown.
        let viewer = self.viewer_is_open();
        let last = !forward && !viewer;
        let weak = self.downgrade();
        let view = self.active_browse();
        if let Some(view) = &view {
            view.set_pending(&target, move |view: &BrowseView| {
                if viewer {
                    let mut paths = view.media_paths();
                    let item = if viewer_last { paths.pop() } else { paths.into_iter().next() };
                    if let (Some(window), Some(path)) = (weak.upgrade(), item) {
                        window.open_viewer(view, &path);
                    }
                } else {
                    view.select_media(last);
                }
            });
        }
        if self.navigate(&target) {
            Some(target)
        } else {
            if let Some(view) = &view {
                view.clear_pending();
            }
            None
        }
    }

    /// Moves to the folder above the one shown.
    fn go_up(&self) {
        if let Some(parent) = self.current_path().and_then(|path| path.parent().map(Path::to_path_buf)) {
            self.navigate(&parent);
        }
    }

    /// Every open tab, in tab order.
    pub fn tabs(&self) -> Vec<TabInfo> {
        let imp = self.imp();
        let selected = imp.tab_view.selected_page().and_then(|page| tab_id(&page));
        (0..imp.tab_view.n_pages())
            .filter_map(|index| {
                let id = tab_id(&imp.tab_view.nth_page(index))?;
                Some(TabInfo { id, index: index as usize, path: self.tab_path(id), active: Some(id) == selected })
            })
            .collect()
    }

    /// The position of the selected tab.
    pub fn current_index(&self) -> Option<usize> {
        let page = self.imp().tab_view.selected_page()?;
        (0..self.imp().tab_view.n_pages()).position(|index| self.imp().tab_view.nth_page(index) == page)
    }

    /// The id of the tab at `index`, if there is one.
    pub fn tab_id_at(&self, index: usize) -> Option<u64> {
        let imp = self.imp();
        if index >= imp.tab_view.n_pages() as usize {
            return None;
        }
        tab_id(&imp.tab_view.nth_page(index as i32))
    }

    /// Closes the tab with `id`. The last tab stays open, as it does when closed by hand.
    pub fn close_tab(&self, id: u64) -> bool {
        match self.page_for_tab(id) {
            Some(page) => {
                self.imp().tab_view.close_page(&page);
                true
            }
            None => false,
        }
    }

    /// The name of the item selected in the selected tab, if one is.
    pub fn selected_file(&self) -> Option<String> {
        let imp = self.imp();
        let page = imp.tab_view.selected_page()?;
        let browse = page.child().downcast::<BrowseView>().ok()?;
        browse.selected_name()
    }

    pub fn show_sidebar(&self) -> bool {
        self.imp().split.show_sidebar()
    }

    pub fn set_show_sidebar(&self, shown: bool) {
        self.imp().split.set_show_sidebar(shown);
    }

    /// Goes one step back, or forward, in the folders shown. Returns whether there was a step to take.
    pub fn step_history(&self, forward: bool) -> bool {
        let navigation = &self.imp().navigation;
        let (history, _, index) = navigation.history_state();
        let possible = if forward { index + 1 < history.len() } else { index > 0 };
        if !possible {
            return false;
        }
        if forward {
            navigation.forward();
        } else {
            navigation.back();
        }
        true
    }

    /// Goes up `levels` folders, stopping at the root. Returns how many levels were climbed.
    pub fn go_up_levels(&self, levels: u32) -> usize {
        let mut climbed = 0;
        for _ in 0..levels {
            let Some(parent) = self.current_path().and_then(|path| path.parent().map(Path::to_path_buf)) else {
                break;
            };
            if !self.navigate(&parent) {
                break;
            }
            climbed += 1;
        }
        climbed
    }

    /// The position of the item selected in the selected tab, if one is selected.
    pub fn selected_position(&self) -> Option<usize> {
        let page = self.imp().tab_view.selected_page()?;
        let browse = page.child().downcast::<BrowseView>().ok()?;
        browse.selected_position().map(|position| position as usize)
    }

    /// Selects the item at `index` in the selected tab, counting from the end for a negative index. Returns the
    /// name of the item selected.
    pub fn select_at_index(&self, index: i64) -> Option<String> {
        let page = self.imp().tab_view.selected_page()?;
        let browse = page.child().downcast::<BrowseView>().ok()?;
        browse.select_at(index)
    }

    /// Bookmarks `path`, under `name` or its own name, and saves the bookmarks.
    pub fn add_bookmark(&self, path: &Path, name: Option<String>) {
        let name = name.unwrap_or_else(|| bookmarks::default_name(path));
        bookmarks::with(|list| bookmarks::add(list, path.to_path_buf(), name));
        self.bookmarks_changed();
    }

    /// Removes the bookmark for `path`, if there is one, and saves the bookmarks.
    pub fn remove_bookmark(&self, path: &Path) {
        bookmarks::with(|list| list.retain(|bookmark| bookmark.path != path));
        self.bookmarks_changed();
    }

    /// The id of the selected tab, if one is open.
    pub fn current_tab_id(&self) -> Option<u64> {
        self.imp().tab_view.selected_page().and_then(|page| tab_id(&page))
    }

    pub fn has_tab(&self, id: u64) -> bool {
        self.page_for_tab(id).is_some()
    }

    /// Opens `path` in a new tab, switching to it if `activate` is set, and returns the new tab's id.
    pub fn open_tab_at(&self, path: &Path, activate: bool) -> u64 {
        self.add_tab(path, activate)
    }

    /// Selects the tab with `id`. Returns whether there is such a tab.
    pub fn activate_tab(&self, id: u64) -> bool {
        match self.page_for_tab(id) {
            Some(page) => {
                self.imp().tab_view.set_selected_page(&page);
                true
            }
            None => false,
        }
    }

    /// The items in the tab with `id`, or `None` if there is no such tab.
    pub fn tab_files(&self, id: u64) -> Option<Vec<ListedFile>> {
        let page = self.page_for_tab(id)?;
        let browse = page.child().downcast::<BrowseView>().ok()?;
        Some(browse.entries())
    }

    /// Selects the item named or pathed `file` in the tab with `id`. Returns the name selected, or `None` if
    /// the tab or the item does not exist.
    pub fn select_in_tab(&self, id: u64, file: &str) -> Option<String> {
        let page = self.page_for_tab(id)?;
        let browse = page.child().downcast::<BrowseView>().ok()?;
        browse.select_named(file)
    }

    /// Opens a new tab at `path` and switches to it.
    pub fn open_tab(&self, path: &Path) {
        self.add_tab(path, true);
    }

    /// Opens a new tab at `path` behind the current one, leaving the current tab selected.
    pub fn open_background_tab(&self, path: &Path) {
        self.add_tab(path, false);
    }

    fn add_tab(&self, path: &Path, select: bool) -> u64 {
        let imp = self.imp();
        let id = imp.next_tab.get();
        imp.next_tab.set(id + 1);
        imp.tabs.borrow_mut().insert(
            id,
            TabState {
                path: Some(path.to_path_buf()),
                history: vec![path.to_path_buf()],
                selections: vec![None],
                index: 0,
                tree: TreeSnapshot::revealing(path),
            },
        );
        let browse = BrowseView::new();
        browse.set_widget_name(&id.to_string());
        self.attach_browse(&browse);
        let page = imp.tab_view.append(&browse);
        label_page(&page, path);
        // A tab opened in the background loads its folder now; its history and tree wait in its state.
        if !select && imp.tab_view.n_pages() > 1 {
            browse.set_path(path);
            self.apply_folder_sort(&browse);
            return id;
        }
        imp.tab_view.set_selected_page(&page);
        self.sync_active_browse();
        if imp.active_tab.get() != Some(id) {
            self.switch_to_tab(id);
        }
        id
    }

    /// Records what the active tab shows, so switching away from it keeps it.
    fn save_active_tab(&self) {
        let imp = self.imp();
        let Some(id) = imp.active_tab.get() else { return };
        let (history, selections, index) = imp.navigation.history_state();
        let state = TabState {
            path: self.current_path(),
            history,
            selections,
            index,
            tree: imp.tree.snapshot(),
        };
        imp.tabs.borrow_mut().insert(id, state);
    }

    /// Shows tab `id`: its history, its folder, and its tree.
    fn switch_to_tab(&self, id: u64) {
        let imp = self.imp();
        let Some(state) = imp.tabs.borrow().get(&id).cloned() else { return };
        imp.active_tab.set(Some(id));
        imp.navigation.set_history_state(state.history, state.selections, state.index);
        if let Some(path) = &state.path {
            self.show_path(path);
        }
        imp.tree.restore(state.tree);
    }

    /// Routes every widget's request to navigate into the window's signal handling.
    fn connect_sources(&self) {
        let imp = self.imp();
        let weak = self.downgrade();

        // A folder picked in the tree is highlighted but not opened.
        imp.tree.connect_open_in_new_tab_requested({
            let weak = weak.clone();
            move |_, path| {
                if let Some(window) = weak.upgrade() {
                    window.open_background_tab(path);
                }
            }
        });
        imp.tree.connect_location_selected({
            let weak = weak.clone();
            move |_, path| {
                if let Some(window) = weak.upgrade() {
                    window.navigate_with(path, Reveal::SelectOnly);
                }
            }
        });
        imp.bookmarks_list.connect_row_activated({
            let weak = weak.clone();
            move |_, row| {
                let Some(window) = weak.upgrade() else { return };
                let path = bookmarks::with(|list| list.get(row.index() as usize).map(|bookmark| bookmark.path.clone()));
                if let Some(path) = path {
                    window.navigate(&path);
                }
                // A folder that could not be opened, or the one already shown, leaves the highlight as it was.
                window.sync_bookmark_selection();
            }
        });
        // The header's buttons pick the tree's root, and show which one it has.
        for (button, root) in [
            (&*imp.filesystem_root_button, PathBuf::from("/")),
            (&*imp.filesystem_home_button, glib::home_dir()),
        ] {
            let weak = weak.clone();
            button.connect_toggled(move |button| {
                if let (true, Some(window)) = (button.is_active(), weak.upgrade()) {
                    window.set_tree_root(&root);
                }
            });
        }
        imp.tree.connect_root_changed({
            let weak = weak.clone();
            move |_| {
                if let Some(window) = weak.upgrade() {
                    window.sync_tree_root_buttons();
                }
            }
        });
        self.sync_tree_root_buttons();
        imp.viewer.connect_item_changed({
            let weak = weak.clone();
            move |_, path| {
                let Some(window) = weak.upgrade() else { return };
                // The playlist's items are nothing to do with the folder shown, which keeps its selection.
                if window.imp().playlist_playing.get() {
                    window.playlist_item_shown(path);
                } else if let Some(view) = window.active_browse() {
                    view.select_named(&path.to_string_lossy());
                }
            }
        });
        imp.viewer.connect_file_dropped({
            let weak = weak.clone();
            move |_, path| {
                if let Some(window) = weak.upgrade() {
                    window.open_dropped_file(path);
                }
            }
        });
        imp.viewer.connect_back_requested({
            let weak = weak.clone();
            move |_| {
                if let Some(window) = weak.upgrade() {
                    window.close_viewer();
                }
            }
        });
        imp.viewer.connect_folder_end_reached({
            let weak = weak.clone();
            move |_, forward| {
                if let Some(window) = weak.upgrade() {
                    // Stepping back past the first item goes on from the last item of the folder before.
                    window.walk_media_folder_later(forward, !forward);
                }
            }
        });
        imp.navigation.connect_open_location({
            let weak = weak.clone();
            move |_, path| {
                if let Some(window) = weak.upgrade() {
                    window.navigate(path);
                }
            }
        });
        imp.path_bar.connect_open_location({
            let weak = weak.clone();
            move |_, path, target| {
                let Some(window) = weak.upgrade() else { return };
                match target {
                    OpenTarget::Here => {
                        window.navigate(path);
                    }
                    OpenTarget::NewTab => window.open_background_tab(path),
                    OpenTarget::NewWindow => window.open_in_new_window(path),
                }
            }
        });
        imp.path_bar.connect_properties_requested({
            let weak = weak.clone();
            move |_, path| {
                if let Some(window) = weak.upgrade() {
                    window.show_properties(path);
                }
            }
        });
        imp.path_bar.connect_edit_location({
            let weak = weak.clone();
            move |_| {
                if let Some(window) = weak.upgrade() {
                    window.edit_location();
                }
            }
        });

        imp.location_entry.connect_activate({
            let weak = weak.clone();
            move |entry| {
                if let Some(window) = weak.upgrade() {
                    window.commit_entry(&entry.text());
                }
            }
        });
        let focus = gtk::EventControllerFocus::new();
        let entry_weak = weak.clone();
        focus.connect_leave(move |_| {
            if let Some(window) = entry_weak.upgrade() {
                window.show_path_bar();
            }
        });
        imp.location_entry.add_controller(focus);
        let keys = gtk::EventControllerKey::new();
        let escape_weak = weak.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let Some(window) = escape_weak.upgrade() {
                window.show_path_bar();
            }
            glib::Propagation::Stop
        });
        imp.location_entry.add_controller(keys);


        // Each tab keeps its own folder, history, and tree. Switching saves the one left and shows the one chosen.
        imp.tab_view.connect_selected_page_notify({
            let weak = weak.clone();
            move |view| {
                let Some(window) = weak.upgrade() else { return };
                window.save_active_tab();
                if let Some(id) = view.selected_page().and_then(|page| tab_id(&page)) {
                    window.switch_to_tab(id);
                }
                window.sync_active_browse();
            }
        });
        // The tab context menu is one menu object, set on the view up front so the bar always has it.
        // Each time it is about to show, its items are rebuilt for the tab that was right-clicked.
        let tab_menu = gio::Menu::new();
        imp.tab_view.set_menu_model(Some(&tab_menu));
        imp.tab_view.connect_setup_menu(move |_, page| {
            tab_menu.remove_all();
            let Some(id) = page.and_then(tab_id) else { return };
            let target = id.to_variant();
            let item = |label: &str, action: &str| {
                let item = gio::MenuItem::new(Some(label), None);
                item.set_action_and_target_value(Some(action), Some(&target));
                item
            };
            tab_menu.append_item(&item("_Duplicate Tab", "win.tab-duplicate"));
            tab_menu.append_item(&item("Open in New _Window", "win.tab-new-window"));
            tab_menu.append_item(&item("Copy _Path", "win.tab-copy-path"));
            let close = gio::Menu::new();
            close.append_item(&item("Close _Other Tabs", "win.tab-close-others"));
            close.append_item(&item("_Close Tab", "win.tab-close"));
            tab_menu.append_section(None, &close);
        });
        imp.tab_view.connect_close_page({
            let weak = weak.clone();
            move |view, page| {
                let Some(window) = weak.upgrade() else { return glib::Propagation::Stop };
                // The last tab stays open.
                if view.n_pages() <= 1 {
                    view.close_page_finish(page, false);
                    return glib::Propagation::Stop;
                }
                if let Some(id) = tab_id(page) {
                    window.imp().tabs.borrow_mut().remove(&id);
                    if window.imp().active_tab.get() == Some(id) {
                        window.imp().active_tab.set(None);
                    }
                }
                view.close_page_finish(page, true);
                glib::Propagation::Stop
            }
        });
        imp.tree.connect_item_context_requested({
            let weak = weak.clone();
            move |tree, node, x, y| {
                if let Some(window) = weak.upgrade() {
                    window.show_context_menu(tree.upcast_ref(), node.path(), node.is_dir(), x, y);
                }
            }
        });
    }

    /// Makes the browse view of the selected tab the active one, which watches its folder, and the others
    /// inactive. The viewer does not change this: its folder stays active while it is open.
    fn sync_active_browse(&self) {
        let tabs = &self.imp().tab_view;
        let selected = tabs.selected_page();
        for index in 0..tabs.n_pages() {
            let page = tabs.nth_page(index);
            if let Ok(browse) = page.child().downcast::<BrowseView>() {
                browse.set_active(selected.as_ref() == Some(&page));
            }
        }
        self.sync_sort_actions();
    }

    /// Sorts `browse` as its folder is remembered, or else by the default. The folder is listed before this, while
    /// the view is not sorting, so a change of sort does not first sort the folder it is leaving.
    fn apply_folder_sort(&self, browse: &BrowseView) {
        let Some(settings) = self.imp().settings.get() else { return };
        let remembered = browse.path().filter(|_| settings.boolean("remember-folder-sort")).and_then(|path| crate::folder_sort::remembered(&path));
        let (by, order) = match remembered {
            Some(sort) => (sort.by, sort.order),
            None => (settings.string("sort-by").to_string(), settings.string("sort-order").to_string()),
        };
        if browse.sort_by() != by {
            browse.set_sort_by(by);
        }
        if browse.sort_order() != order {
            browse.set_sort_order(order);
        }
        if self.active_browse().as_ref() == Some(browse) {
            self.sync_sort_actions();
        }
    }

    /// Applies a change of the default sort, or of whether folders remember theirs, to every tab.
    fn apply_sort_settings(&self) {
        let tabs = &self.imp().tab_view;
        for index in 0..tabs.n_pages() {
            if let Ok(browse) = tabs.nth_page(index).child().downcast::<BrowseView>() {
                self.apply_folder_sort(&browse);
            }
        }
        self.sync_sort_actions();
    }

    /// Has the sort menu show how the active tab is sorted, or with no tab the default.
    fn sync_sort_actions(&self) {
        let Some(settings) = self.imp().settings.get() else { return };
        let (by, order) = match self.active_browse() {
            Some(browse) => (browse.sort_by(), browse.sort_order()),
            None => (settings.string("sort-by").to_string(), settings.string("sort-order").to_string()),
        };
        for (key, value) in [("sort-by", by), ("sort-order", order)] {
            if let Some(action) = self.lookup_action(key).and_downcast::<gio::SimpleAction>() {
                action.set_state(&value.to_variant());
            }
        }
    }

    /// Applies a choice in the sort menu: to the folder shown alone while folders remember their sorts, otherwise to
    /// the default, which every tab follows.
    fn choose_sort(&self, key: &str, value: &str) {
        let Some(settings) = self.imp().settings.get() else { return };
        let remembering = self.active_browse().filter(|_| settings.boolean("remember-folder-sort"));
        let Some((browse, path)) = remembering.and_then(|browse| browse.path().map(|path| (browse, path))) else {
            let _ = settings.set_string(key, value);
            return;
        };
        match key {
            "sort-by" => browse.set_sort_by(value),
            _ => browse.set_sort_order(value),
        }
        let sort = crate::folder_sort::FolderSort { by: browse.sort_by(), order: browse.sort_order() };
        if let Err(err) = crate::folder_sort::remember(&path, &sort) {
            eprintln!("rsimgv: could not remember the sort of {}: {err}", path.display());
        }
        self.sync_sort_actions();
    }

    /// The browse view of the selected tab.
    fn active_browse(&self) -> Option<BrowseView> {
        self.imp()
            .tab_view
            .selected_page()
            .and_then(|page| page.child().downcast::<BrowseView>().ok())
    }

    /// Connects a tab's browse view to the settings, its context menu, and opening folders.
    fn attach_browse(&self, browse: &BrowseView) {
        let imp = self.imp();
        // A folder that changes while the viewer shows it keeps the viewer's items in step with it.
        let weak_window = self.downgrade();
        browse.connect_listing_changed(move |view| {
            let Some(window) = weak_window.upgrade() else { return };
            let shows_folder = window.imp().viewer_open.get() && !window.imp().playlist_playing.get();
            if shows_folder && window.active_browse().as_ref() == Some(view) {
                window.imp().viewer.update_paths(view.media_paths());
            }
        });
        if let Some(settings) = imp.settings.get() {
            settings.bind("item-size", browse, "item-size").build();
            settings.bind("filename-display", browse, "filename-display").build();
            settings.bind("show-directories", browse, "show-directories").build();
            settings.bind("info-layout", browse, "info-layout").build();
            set_info_fields_from(settings, browse);
            let weak = browse.downgrade();
            settings.connect_changed(Some("info-fields"), move |settings, _| {
                if let Some(browse) = weak.upgrade() {
                    set_info_fields_from(settings, &browse);
                }
            });
            settings.bind("media-only", browse, "media-only").build();
            settings.bind("thumbnail-crop", browse, "thumbnail-crop").build();
            settings.bind("video-frame-previews", browse, "video-frame-previews").build();
            settings.bind("directory-previews", browse, "directory-previews").build();
            settings.bind("folder-preview-cycle", browse, "folder-preview-cycle").build();
            settings.bind("folder-preview-count", browse, "folder-preview-count").build();
            settings.bind("folder-preview-fit", browse, "folder-preview-fit").build();
            settings.bind("show-hidden", browse, "show-hidden").build();
        }
        self.apply_folder_sort(browse);
        // The active tab's selection is what the history remembers for its current location.
        browse.connect_selection_changed({
            let weak = self.downgrade();
            move |view, name| {
                if let Some(window) = weak.upgrade() {
                    if window.active_browse().as_ref() == Some(view) {
                        window.imp().navigation.set_selected((!name.is_empty()).then(|| name.to_owned()));
                    }
                }
            }
        });
        let weak = self.downgrade();
        browse.connect_file_activated({
            let weak = weak.clone();
            move |view, path| {
                if let Some(window) = weak.upgrade() {
                    window.open_viewer(view, path);
                }
            }
        });
        browse.connect_open_in_new_tab_requested({
            let weak = weak.clone();
            move |_, path| {
                if let Some(window) = weak.upgrade() {
                    window.open_background_tab(path);
                }
            }
        });
        browse.connect_item_context_requested({
            let weak = weak.clone();
            move |browse, path, is_dir, x, y| {
                if let Some(window) = weak.upgrade() {
                    window.show_context_menu(browse.upcast_ref(), path, is_dir, x, y);
                }
            }
        });
        browse.connect_directory_activated(move |_, path| {
            if let Some(window) = weak.upgrade() {
                window.navigate(path);
            }
        });
    }

    fn edit_location(&self) {
        let imp = self.imp();
        let entry = &*imp.location_entry;
        let text = self.current_path().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
        entry.set_text(&text);
        entry.remove_css_class("error");
        imp.title_stack.set_visible_child(entry);
        entry.grab_focus();
        entry.select_region(0, -1);
    }

    /// Navigates to what was typed. The entry stays open, marked as an error, if it is not a folder.
    fn commit_entry(&self, text: &str) {
        let expanded = expand_home(text.trim());
        if self.navigate(Path::new(&expanded)) {
            self.show_path_bar();
        } else {
            self.imp().location_entry.add_css_class("error");
        }
    }

    fn show_path_bar(&self) {
        let imp = self.imp();
        imp.title_stack.set_visible_child(&*imp.path_bar);
    }

    /// Opens the preferences dialog, whose rows are bound to the settings.
    pub fn show_preferences(&self) {
        self.present_preferences(None);
    }

    /// Opens the preferences dialog on the page named `page`, or on the first page.
    fn present_preferences(&self, page: Option<&str>) {
        if let Some(settings) = self.imp().settings.get() {
            let dialog = PreferencesDialog::new(settings);
            if let Some(page) = page {
                dialog.show_section(page);
            }
            adw::prelude::AdwDialogExt::present(&dialog, Some(self));
            dialog.close_on_outside_click(self.upcast_ref());
        }
    }
}

/// How far one notch of the scroll wheel moves a video's volume, in percent, and its position, in seconds.
const WHEEL_VOLUME_STEP: f64 = 5.0;
const WHEEL_SEEK_STEP: f64 = 5.0;

/// The settings the viewer and the window title are set from, applied again whenever one changes.
const VIEWER_SETTINGS: [&str; 33] = [
    "seek-overlay",
    "seek-overlay-bar",
    "seek-overlay-time",
    "seek-overlay-time-format",
    "seek-overlay-position",
    "seek-overlay-seconds",
    "seek-overlay-width",
    "seek-overlay-always",
    "folder-end",
    "original-focus",
    "unlock-minimum-zoom",
    "zoom-step",
    "expand-up-to-factor",
    "keep-fit-mode",
    "image-fit",
    "smooth-scrolling",
    "scaling-filter",
    "smooth-upscaling",
    "zoom-indicator",
    "auto-hide-cursor",
    "viewer-blurred-background",
    "viewer-status-bar",
    "viewer-status-overlay-position",
    "svp-enabled",
    "svp-plugin-directory",
    "svp-fps-mode",
    "svp-fps-target",
    "svp-fps-minimum",
    "svp-fps-maximum",
    "svp-nvof",
    "svp-seek-bypass",
    "svp-seek-bypass-delay",
    "image-info-in-title",
];
/// How far a thumbnail size key moves each time, in pixels, and the range it is kept within.
const THUMBNAIL_STEP: i32 = 16;
const ITEM_SIZE_MIN: i32 = 32;
const ITEM_SIZE_MAX: i32 = 2048;

/// A tab as the IPC reports it: its id, its position, its folder, and whether it is the selected one.
pub struct TabInfo {
    pub id: u64,
    pub index: usize,
    pub path: Option<PathBuf>,
    pub active: bool,
}

/// Whether the browse view lists folders at all, which is `show-directories` other than `none`.
fn shows_directories(settings: &gio::Settings) -> bool {
    settings.string("show-directories") != "none"
}

/// Adds a stateful action for the sort setting `key`, whose radio items in the sort menu choose it. Menu items
/// activate their action, and some pass the new value as a change of state, so both make the choice.
fn add_sort_action(window: &Window, settings: &gio::Settings, key: &'static str) {
    let action = gio::SimpleAction::new_stateful(key, Some(glib::VariantTy::STRING), &settings.string(key).to_variant());
    let choose = {
        let weak = window.downgrade();
        move |parameter: Option<&glib::Variant>| {
            if let (Some(window), Some(value)) = (weak.upgrade(), parameter.and_then(|value| value.get::<String>())) {
                window.choose_sort(key, &value);
            }
        }
    };
    action.connect_activate({
        let choose = choose.clone();
        move |_, parameter| choose(parameter)
    });
    action.connect_change_state(move |_, parameter| choose(parameter));
    settings.connect_changed(Some(key), {
        let weak = window.downgrade();
        move |_, _| {
            if let Some(window) = weak.upgrade() {
                window.apply_sort_settings();
            }
        }
    });
    window.add_action(&action);
}

/// Adds the sort menu's "Show Directories" checkbox, which turns the listing of folders on or off.
fn add_show_directories_action(window: &Window, settings: &gio::Settings) {
    let action = gio::SimpleAction::new_stateful("show-directories", None, &shows_directories(settings).to_variant());
    action.connect_activate({
        let settings = settings.clone();
        move |_, _| {
            // Turning folders back on lists them first-last, the default.
            let value = if shows_directories(&settings) { "none" } else { "first-last" };
            let _ = settings.set_string("show-directories", value);
        }
    });
    settings.connect_changed(Some("show-directories"), {
        let action = action.clone();
        move |settings, _| action.set_state(&shows_directories(settings).to_variant())
    });
    window.add_action(&action);
}

/// The folder a new window opens at, from the `start-location` setting. Falls back to the home folder
/// when the chosen folder is unset or no longer exists.
fn start_location(settings: &gio::Settings) -> PathBuf {
    let path = match settings.string("start-location").as_str() {
        "last" => PathBuf::from(settings.string("last-directory").as_str()),
        "root" => PathBuf::from("/"),
        "specific" => PathBuf::from(settings.string("start-directory").as_str()),
        _ => glib::home_dir(),
    };
    if path.is_dir() { path } else { glib::home_dir() }
}

/// Saves whether the window is maximized and, if it is not, its size. A maximized window is as big as
/// the screen, so the size kept is the one from before it was maximized.
fn save_window_state(window: &Window, settings: &gio::Settings) {
    if settings.string("start-location") == "last" {
        if let Some(path) = window.current_path() {
            let _ = settings.set_string("last-directory", &path.to_string_lossy());
            let file = window.selected_file().map(|name| path.join(name).to_string_lossy().into_owned());
            let _ = settings.set_string("last-file", file.as_deref().unwrap_or(""));
        }
    }
    let _ = settings.set_int("sidebar-width", window.imp().split.user_sidebar_width().unwrap_or(-1));
    let maximized = window.is_maximized();
    let _ = settings.set_boolean("window-maximized", maximized);
    if !maximized && window.width() > 0 && window.height() > 0 {
        let _ = settings.set_int("window-width", window.width());
        let _ = settings.set_int("window-height", window.height());
    }
    save_playlist_layout(window, settings);
}

/// Saves the playlist's layout: whether it is shown and how wide it is beside the page, and whether it is in a window of
/// its own, with that window's size.
fn save_playlist_layout(window: &Window, settings: &gio::Settings) {
    let imp = window.imp();
    let _ = settings.set_boolean("playlist-shown", imp.playlist_shown.get());
    if let Some(width) = window.active_playlist_split().user_sidebar_width() {
        let _ = settings.set_int("playlist-width", width);
    }
    let detached = imp.playlist_window.borrow().clone();
    let _ = settings.set_boolean("playlist-detached", detached.is_some());
    if let Some(detached) = &detached {
        save_playlist_window_size(detached, settings);
    }
}

/// Saves the size of the window the playlist is in, while it is shown.
fn save_playlist_window_size(window: &adw::Window, settings: &gio::Settings) {
    if window.width() > 0 && window.height() > 0 {
        let _ = settings.set_int("playlist-window-width", window.width());
        let _ = settings.set_int("playlist-window-height", window.height());
    }
}

/// How long the window must stay unchanged before its state is saved.
const WINDOW_SAVE_DELAY_MS: u64 = 400;

/// Saves the window state once it has stopped changing for `WINDOW_SAVE_DELAY_MS`. A new change
/// restarts the wait.
/// The name of the item in `parent` that leads to `folder`, if `folder` is inside `parent`.
fn child_toward(parent: &Path, folder: &Path) -> Option<String> {
    let rest = folder.strip_prefix(parent).ok()?;
    rest.components().next()?.as_os_str().to_str().map(str::to_owned)
}

fn schedule_window_save(window: &Window, settings: &gio::Settings, pending: &Rc<Cell<Option<glib::SourceId>>>) {
    if let Some(id) = pending.take() {
        id.remove();
    }
    let weak = window.downgrade();
    let settings = settings.clone();
    let slot = pending.clone();
    let id = glib::timeout_add_local_once(std::time::Duration::from_millis(WINDOW_SAVE_DELAY_MS), move || {
        slot.set(None);
        if let Some(window) = weak.upgrade() {
            save_window_state(&window, &settings);
        }
    });
    pending.set(Some(id));
}

/// The state a tab keeps while it is not the active one.
#[derive(Clone, Default)]
struct TabState {
    path: Option<PathBuf>,
    history: Vec<PathBuf>,
    selections: Vec<Option<String>>,
    index: usize,
    tree: TreeSnapshot,
}

/// Gives `browse` the info fields the settings list, skipping names it does not know.
fn set_info_fields_from(settings: &gio::Settings, browse: &BrowseView) {
    let fields: Vec<InfoField> = settings.strv("info-fields").iter().filter_map(|key| InfoField::from_key(key)).collect();
    browse.set_info_fields(&fields);
}

/// The id of the tab on `page`, stored as the name of its child.
fn tab_id(page: &adw::TabPage) -> Option<u64> {
    page.child().widget_name().parse().ok()
}

/// Titles a tab page with its folder's name and icon, and its tooltip with the folder's full path.
fn label_page(page: &adw::TabPage, path: &Path) {
    page.set_title(&folder_name(path));
    page.set_icon(Some(&gio::ThemedIcon::new("folder-symbolic")));
    page.set_tooltip(&path.to_string_lossy());
}

/// The name shown for a folder in a tab title.
fn folder_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "/".to_owned(), |name| name.to_string_lossy().into_owned())
}

/// How much of the tree a navigation reveals.
#[derive(Clone, Copy)]
enum Reveal {
    /// Expand to the folder and into it.
    Expand,
    /// Expand to the folder's parents and select it, leaving the folder itself as it is.
    SelectOnly,
}

/// A leading `~` in typed input means the home directory.
fn expand_home(text: &str) -> String {
    match text.strip_prefix('~') {
        Some(rest) => format!("{}{rest}", glib::home_dir().display()),
        None => text.to_owned(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    /// Pumps the main context until `cond` holds, failing after five seconds.
    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        let ctx = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for: {what}");
            if !ctx.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// The path bar's segment button whose tooltip is `path`.
    fn segment_button(bar: &PathBar, path: &Path) -> gtk::Button {
        let segments = &bar.imp().segments;
        let mut child = segments.first_child();
        while let Some(widget) = child {
            let button = widget.last_child().and_downcast::<gtk::Button>().unwrap();
            if button.tooltip_text().as_deref() == Some(path.file_name().unwrap().to_str().unwrap()) {
                return button;
            }
            child = widget.next_sibling();
        }
        panic!("no segment for {}", path.display());
    }


    /// The context menu actions act on the path they are given.
    fn context_actions_work(app: &adw::Application, settings: &gio::Settings) {
        let root = std::env::temp_dir().join(format!("rsimgv-context-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("docs")).unwrap();

        let window = Window::new(app, settings);
        let tabs = window.imp().tab_view.clone();
        let before = tabs.n_pages();
        let folder = root.join("docs").to_string_lossy().into_owned().to_variant();
        gtk::prelude::WidgetExt::activate_action(&window, "win.open-in-new-tab", Some(&folder)).expect("open in a new tab");
        assert_eq!(tabs.n_pages(), before + 1, "a folder opens in a new tab");
        assert_ne!(window.current_path(), Some(root.join("docs")), "a background tab leaves the current folder alone");

        gtk::prelude::WidgetExt::activate_action(&window, "win.open-path", Some(&root.to_string_lossy().into_owned().to_variant())).expect("open folder");
        assert_eq!(window.current_path(), Some(root.clone()), "open navigates to the folder");

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// Each tab keeps its own folder and history; switching tabs shows the one chosen.
    fn tabs_keep_their_own_state(app: &adw::Application, settings: &gio::Settings) {
        let root = std::env::temp_dir().join(format!("rsimgv-tabs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("alpha")).unwrap();
        fs::create_dir_all(root.join("gamma")).unwrap();

        let window = Window::new(app, settings);
        let tabs = window.imp().tab_view.clone();
        let first = tabs.nth_page(0);
        window.navigate(&root.join("alpha"));
        window.open_tab(&root.join("gamma"));
        assert_eq!(tabs.n_pages(), 2, "a second tab is open");
        assert_eq!(window.current_path(), Some(root.join("gamma")), "the new tab shows its folder");

        tabs.set_selected_page(&first);
        assert_eq!(window.current_path(), Some(root.join("alpha")), "the first tab shows its own folder");
        assert_eq!(window.imp().path_bar.path(), Some(root.join("alpha")));
        assert_eq!(window.imp().navigation.location(), Some(root.join("alpha")), "and its history");

        let second = tabs.nth_page(1);
        tabs.set_selected_page(&second);
        assert_eq!(window.current_path(), Some(root.join("gamma")), "the second tab shows its folder again");
        assert_eq!(window.imp().navigation.location(), Some(root.join("gamma")));

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// Stepping past a folder's last item goes on to the next folder's first, and back past its first to the previous
    /// folder's last, while the setting asks for it.
    fn viewer_steps_between_folders(app: &adw::Application, settings: &gio::Settings) {
        let root = std::env::temp_dir().join(format!("rsimgv-folder-end-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (alpha, gamma) = (root.join("alpha"), root.join("gamma"));
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 8, 8).unwrap();
        for folder in [&alpha, &gamma] {
            fs::create_dir_all(folder).unwrap();
            for name in ["one.png", "two.png"] {
                pixbuf.savev(folder.join(name), "png", &[]).unwrap();
            }
        }

        settings.set_string("folder-end", "next-folder").unwrap();
        let window = Window::new(app, settings);
        window.navigate(&alpha);
        let browse = window.active_browse().unwrap();
        wait_for("the first folder to list", || browse.media_paths().len() == 2);
        window.open_viewer(&browse, &alpha.join("two.png"));
        let viewer = window.imp().viewer.clone();

        viewer.step(true);
        wait_for("the next folder's first item", || {
            window.current_path().as_ref() == Some(&gamma) && viewer.shown_item().map(|item| item.path) == Some(gamma.join("one.png"))
        });
        assert!(window.viewer_is_open(), "the viewer stays open");

        viewer.step(false);
        wait_for("the previous folder's last item", || {
            window.current_path().as_ref() == Some(&alpha) && viewer.shown_item().map(|item| item.path) == Some(alpha.join("two.png"))
        });

        settings.reset("folder-end");
        viewer.step(true);
        assert_eq!(window.current_path().as_ref(), Some(&alpha), "stopping stays in the folder");
        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// The menu's column toggles show and hide the columns, and "Reveal in browser" shows the item's folder.
    fn playlist_menu_actions(app: &adw::Application, settings: &gio::Settings) {
        let root = std::env::temp_dir().join(format!("rsimgv-reveal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (alpha, gamma) = (root.join("alpha"), root.join("gamma"));
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 8, 8).unwrap();
        for folder in [&alpha, &gamma] {
            fs::create_dir_all(folder).unwrap();
            pixbuf.savev(folder.join("one.png"), "png", &[]).unwrap();
        }

        let window = Window::new(app, settings);
        let panel = window.imp().playlist_panel.get().unwrap();
        for (index, key) in ["playlist-show-position", "playlist-show-name", "playlist-show-length", "playlist-show-filename"].into_iter().enumerate() {
            let before = settings.boolean(key);
            panel.content().activate_action(&format!("win.{key}"), None).unwrap();
            assert_eq!(settings.boolean(key), !before, "the toggle changes the setting for {key}");
            assert_eq!(panel.column_shown(index as u32), !before, "the toggle changes the column for {key}");
            panel.content().activate_action(&format!("win.{key}"), None).unwrap();
            assert_eq!(panel.column_shown(index as u32), before, "the toggle changes the column back for {key}");
        }

        window.navigate(&gamma);
        let browse = window.active_browse().unwrap();
        wait_for("the folder to list", || browse.media_paths().len() == 1);
        window.open_viewer(&browse, &gamma.join("one.png"));
        assert!(window.viewer_is_open(), "the viewer is open before revealing");
        gio::prelude::ActionGroupExt::activate_action(&window, "reveal-path", Some(&alpha.join("one.png").to_string_lossy().to_variant()));
        assert!(!window.viewer_is_open(), "revealing leaves the viewer");
        assert_eq!(window.current_path().as_ref(), Some(&alpha), "revealing shows the item's folder");
        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// The viewer goes through the playlist without the folder shown, or the item selected in it, having any part in it.
    fn playlist_plays_apart_from_browsing(app: &adw::Application, settings: &gio::Settings) {
        let root = std::env::temp_dir().join(format!("rsimgv-playlist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (alpha, gamma) = (root.join("alpha"), root.join("gamma"));
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 8, 8).unwrap();
        for folder in [&alpha, &gamma] {
            fs::create_dir_all(folder).unwrap();
            for name in ["one.png", "two.png"] {
                pixbuf.savev(folder.join(name), "png", &[]).unwrap();
            }
        }
        fs::write(alpha.join("notes.txt"), "").unwrap();
        playlist::with(|list| list.clear());

        // Looping and moving on to other folders are for folders, and do not apply here.
        settings.set_string("folder-end", "next-folder").unwrap();
        let window = Window::new(app, settings);
        let panel = window.imp().playlist_panel.get().unwrap();
        let viewer = window.imp().viewer.clone();
        window.navigate(&alpha);
        let browse = window.active_browse().unwrap();
        wait_for("the folder to list", || browse.media_paths().len() == 2);
        let shows = |path: &Path| viewer.shown_item().is_some_and(|item| item.path == path);
        let len = || playlist::with(|list| list.len());
        let current = || playlist::with(|list| list.current());

        window.play_playlist();
        assert!(!window.viewer_is_open(), "an empty playlist has nothing to show");

        // The key adds the item under the cursor, or a whole folder, and takes them off again.
        browse.select_named("one.png");
        window.toggle_in_playlist();
        wait_for("the item to be added", || len() == 1);
        assert!(playlist::contains(&alpha.join("one.png")));
        assert_eq!(panel.row_count(), 1);
        window.toggle_in_playlist();
        assert_eq!(len(), 0, "the key takes it off again");
        assert_eq!(panel.row_count(), 0);
        window.add_to_playlist_later(vec![alpha.join("notes.txt")], false);
        window.add_to_playlist_later(vec![alpha.join("one.png"), gamma.clone()], false);
        wait_for("the item and the folder to be added", || len() == 3);
        assert_eq!(playlist::with(|list| list.order().to_vec()), [alpha.join("one.png"), gamma.join("one.png"), gamma.join("two.png")]);
        window.add_to_playlist_later(vec![gamma.clone()], true);
        wait_for("the folder to be taken off", || len() == 1);
        window.add_to_playlist_later(vec![gamma.clone()], false);
        wait_for("the folder to be added again", || len() == 3);
        assert_eq!(panel.row_count(), 3);

        // Playing shows the playlist, and the browse view stays as it was.
        browse.select_named("two.png");
        window.play_playlist();
        assert!(window.viewer_is_open() && window.imp().playlist_playing.get());
        wait_for("the first item", || shows(&alpha.join("one.png")));
        assert_eq!(browse.selected_name().as_deref(), Some("two.png"), "the selection stays");

        window.step_playlist(true);
        wait_for("the next item", || shows(&gamma.join("one.png")));
        assert_eq!(window.current_path().as_ref(), Some(&alpha), "the folder stays");
        assert_eq!(browse.selected_name().as_deref(), Some("two.png"), "the selection stays");
        assert_eq!(current(), Some(1));

        // The viewer's own steps follow the playlist, stop at its end and leave the current item right.
        viewer.step(true);
        wait_for("the viewer's step", || shows(&gamma.join("two.png")) && current() == Some(2));
        viewer.step(true);
        assert_eq!(window.current_path().as_ref(), Some(&alpha), "the end of the playlist goes to no other folder");
        // The keys go round the ends.
        window.step_playlist(true);
        wait_for("going round the end", || shows(&alpha.join("one.png")));
        window.step_playlist(false);
        wait_for("going round the start", || shows(&gamma.join("two.png")));
        window.jump_playlist(false);
        wait_for("the first item", || shows(&alpha.join("one.png")));

        // A change to the folder's items does not reach the viewer.
        browse.emit_by_name::<()>("listing-changed", &[]);
        wait_for("the viewer to keep its items", || shows(&alpha.join("one.png")));
        window.step_playlist(true);
        wait_for("the playlist's next item", || shows(&gamma.join("one.png")));

        // Taking the item shown off the playlist shows the next one.
        window.remove_from_playlist(&[gamma.join("one.png")]);
        wait_for("the item after it", || shows(&gamma.join("two.png")));
        assert_eq!(len(), 2);

        // Closing goes back to the folder as it was, and the folder's own viewing still works.
        window.close_viewer();
        assert!(!window.imp().playlist_playing.get());
        assert_eq!(window.imp().viewer_open.get(), false);
        assert_eq!(browse.selected_name().as_deref(), Some("two.png"));
        window.open_viewer(&browse, &alpha.join("two.png"));
        assert!(!window.imp().playlist_playing.get());
        wait_for("the folder's item", || shows(&alpha.join("two.png")));
        // A key jumps from there to the playlist, where it was left, and the folder's item goes.
        window.step_playlist(true);
        assert!(window.imp().playlist_playing.get());
        wait_for("the item it was left on", || shows(&gamma.join("two.png")));
        // A move in the panel keeps the item shown, at its new place.
        assert_eq!(current(), Some(1));
        gtk::prelude::WidgetExt::activate_action(&window, "win.playlist-move", Some(&(1u32, 0u32).to_variant())).unwrap();
        assert_eq!(playlist::with(|list| list.order().to_vec()), [gamma.join("two.png"), alpha.join("one.png")]);
        assert_eq!(current(), Some(0));
        assert!(shows(&gamma.join("two.png")));

        // The files in each format hold the playlist, and loading one brings it back.
        for name in ["list.m3u8", "list.m3u", "list.pls", "list.xspf"] {
            let file = root.join(name);
            window.write_playlist_file(file.clone());
            wait_for("the file to be written", || file.exists());
            let read = read_playlist_file(&file).unwrap();
            assert_eq!(read.paths, [gamma.join("two.png"), alpha.join("one.png")], "{name}");
        }
        window.write_playlist_file(root.join("bare"));
        wait_for("a name with no extension to become M3U8", || root.join("bare.m3u8").exists());

        // Saved as it changes, and read back when the app starts.
        let saved = playlist::with(|list| list.clone());
        playlist::load();
        assert_eq!(playlist::with(|list| list.clone()), saved);

        // Emptying it while it shows goes back to browsing.
        playlist::with(|list| list.clear());
        window.playlist_changed();
        assert!(!window.viewer_is_open());
        assert!(!window.imp().playlist_playing.get());

        window.load_playlist_file(root.join("list.xspf"));
        wait_for("the file to load", || len() == 2);
        assert_eq!(panel.row_count(), 2);
        fs::write(root.join("web.m3u"), "https://example.com/a.mp4\n/somewhere/song.mp3\n/somewhere/clip.mp4\n").unwrap();
        let read = read_playlist_file(&root.join("web.m3u")).unwrap();
        assert_eq!((read.paths, read.skipped), (vec![PathBuf::from("/somewhere/clip.mp4")], 2));

        playlist::with(|list| list.clear());
        window.playlist_changed();
        settings.reset("folder-end");
        window.destroy();
        let _ = fs::remove_file(playlist::file());
        let _ = fs::remove_dir_all(&root);
    }

    /// While folders remember their sorts the sort menu changes only the folder shown, and the others use the default.
    fn folders_remember_their_sorts(app: &adw::Application, settings: &gio::Settings) {
        let root = std::env::temp_dir().join(format!("rsimgv-sorts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("alpha")).unwrap();
        fs::create_dir_all(root.join("gamma")).unwrap();
        let (alpha, gamma) = (root.join("alpha"), root.join("gamma"));

        let window = Window::new(app, settings);
        window.navigate(&alpha);
        let browse = window.active_browse().unwrap();
        let menu_state = |key: &str| window.lookup_action(key).unwrap().state().unwrap().get::<String>().unwrap();
        assert_eq!(browse.sort_by(), "name", "a folder starts with the default");

        settings.set_boolean("remember-folder-sort", true).unwrap();
        window.choose_sort("sort-by", "size");
        assert_eq!(browse.sort_by(), "size", "the folder shown takes the choice");
        assert_eq!(menu_state("sort-by"), "size", "and the menu shows it");
        assert_eq!(settings.string("sort-by"), "name", "the default stays as it was");

        window.navigate(&gamma);
        assert_eq!(browse.sort_by(), "name", "another folder uses the default");
        assert_eq!(menu_state("sort-by"), "name");
        window.navigate(&alpha);
        assert_eq!(browse.sort_by(), "size", "coming back brings the folder's own sort");

        settings.set_string("sort-by", "type").unwrap();
        assert_eq!(browse.sort_by(), "size", "a remembered sort outranks the default");
        window.navigate(&gamma);
        assert_eq!(browse.sort_by(), "type", "a folder without one follows the default");

        settings.set_boolean("remember-folder-sort", false).unwrap();
        window.navigate(&alpha);
        assert_eq!(browse.sort_by(), "type", "with remembering off, every folder uses the default");
        window.choose_sort("sort-order", "descending");
        assert_eq!(settings.string("sort-order"), "descending", "and the menu changes the default");
        assert_eq!(browse.sort_order(), "descending");

        settings.reset("sort-by");
        settings.reset("sort-order");
        settings.reset("remember-folder-sort");
        window.destroy();
        let _ = fs::remove_file(crate::settings::config_path().with_file_name("folder_sort.yaml"));
        let _ = fs::remove_dir_all(&root);
    }

    /// A size change is saved shortly after it happens, without waiting for the window to close.
    fn size_change_is_saved_without_closing(app: &adw::Application, settings: &gio::Settings) {
        let window = Window::new(app, settings);
        window.present();
        let ctx = glib::MainContext::default();
        let pump = |frames: u32| {
            for _ in 0..frames {
                while ctx.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        };
        pump(60);
        let before = (window.width(), window.height());
        window.set_size_request(812, 520);
        pump(150);
        // The allocated size can be a little less than the size requested, so growth and the saved size are checked
        // against the window's own size.
        let grown = (window.width(), window.height());
        assert!(grown.0 > before.0 && grown.1 > before.1, "the window grew to the new size: {before:?} to {grown:?}");
        assert_eq!(settings.int("window-width"), grown.0, "the width is saved without closing");
        assert_eq!(settings.int("window-height"), grown.1, "the height is saved without closing");
        window.set_size_request(-1, -1);
        window.destroy();
    }

    /// Closing a window saves its size and that it is not maximized; a new window opens at that size.
    fn window_size_is_saved(app: &adw::Application, settings: &gio::Settings) {
        let window = Window::new(&app, &settings);
        window.set_default_size(700, 450);
        window.present();
        let ctx = glib::MainContext::default();
        for _ in 0..60 {
            while ctx.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        // The size saved is the size the window was allocated, which the display may make a little different
        // from the size asked for, so the saved size is checked against the window's own size.
        let (width, height) = (window.width(), window.height());
        window.close();
        assert_eq!(settings.int("window-width"), width, "the width is saved");
        assert_eq!(settings.int("window-height"), height, "the height is saved");
        assert!(!settings.boolean("window-maximized"), "an unmaximized window is saved as such");

        // A window opened afterwards starts at the saved size.
        let reopened = Window::new(&app, &settings);
        reopened.present();
        assert_eq!(reopened.default_width(), width);
        assert_eq!(reopened.default_height(), height);
        reopened.destroy();
    }

    pub(crate) fn run_scenarios() {
        let root = std::env::temp_dir().join(format!("rsimgv-window-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("alpha/beta")).unwrap();
        fs::create_dir_all(root.join("gamma")).unwrap();
        fs::write(root.join("file.txt"), "").unwrap();
        fs::create_dir_all(root.join("delta/epsilon")).unwrap();

        let app = adw::Application::builder().application_id("au.topher.rimgv.Tests").build();
        // Windows may only be created once the application has started.
        gio::prelude::ApplicationExt::register(&app, gio::Cancellable::NONE).expect("register test application");
        let settings = crate::settings::open("au.topher.rimgv");
        tabs_keep_their_own_state(&app, &settings);
        folders_remember_their_sorts(&app, &settings);
        viewer_steps_between_folders(&app, &settings);
        playlist_plays_apart_from_browsing(&app, &settings);
        playlist_menu_actions(&app, &settings);
        context_actions_work(&app, &settings);
        window_size_is_saved(&app, &settings);
        size_change_is_saved_without_closing(&app, &settings);
        let window = Window::new(&app, &settings);
        let imp = window.imp();

        // Every widget follows one navigation.
        assert!(window.navigate(&root.join("alpha")));
        assert_eq!(window.current_path(), Some(root.join("alpha")));
        assert_eq!(imp.path_bar.path(), Some(root.join("alpha")));
        assert_eq!(imp.navigation.location(), Some(root.join("alpha")));
        wait_for("alpha listed in the browse view", || window.active_browse().is_some_and(|b| b.item_names() == ["beta"]));

        // Staying put is fine; a file is refused and changes nothing.
        assert!(window.navigate(&root.join("alpha")));
        assert!(!window.navigate(&root.join("file.txt")));
        assert_eq!(window.current_path(), Some(root.join("alpha")));

        // A subfolder entered from the browse list.
        window.active_browse().expect("a browse view").activate(0);
        assert_eq!(window.current_path(), Some(root.join("alpha/beta")));

        // A breadcrumb click in the path bar.
        segment_button(&imp.path_bar, &root.join("alpha")).emit_clicked();
        assert_eq!(window.current_path(), Some(root.join("alpha")));

        // The history is now: home, alpha, alpha/beta, alpha. Back twice lands on alpha; forward returns to alpha/beta.
        imp.navigation.back();
        assert_eq!(window.current_path(), Some(root.join("alpha/beta")), "back follows the history");
        imp.navigation.back();
        assert_eq!(window.current_path(), Some(root.join("alpha")));
        imp.navigation.forward();
        assert_eq!(window.current_path(), Some(root.join("alpha/beta")));

        // A typed path, then a typed path that is not a folder (the entry stays marked as an error).
        window.commit_entry(&root.join("gamma").to_string_lossy());
        assert_eq!(window.current_path(), Some(root.join("gamma")));
        window.commit_entry(&root.join("missing").to_string_lossy());
        assert!(imp.location_entry.has_css_class("error"));
        assert_eq!(window.current_path(), Some(root.join("gamma")));

        // A row selected in the tree, once the tree has listed it.
        window.navigate(&root.join("gamma"));
        wait_for("alpha listed in the tree", || imp.tree.select_path(&root.join("alpha")));
        assert_eq!(window.current_path(), Some(root.join("alpha")), "tree selection navigates");

        // Navigating reveals and selects the folder. Other folders stay expanded unless the setting is on.
        window.navigate(&root.join("alpha/beta"));
        wait_for("beta selected", || imp.tree.selected_path() == Some(root.join("alpha/beta")));
        assert!(imp.tree.is_expanded(&root.join("alpha")), "the path to the folder is expanded");
        window.navigate(&root.join("gamma"));
        wait_for("gamma selected", || imp.tree.selected_path() == Some(root.join("gamma")));
        assert!(imp.tree.is_expanded(&root.join("alpha")), "other folders stay expanded by default");

        // Picking a folder in the tree selects it without expanding it.
        wait_for("delta listed", || imp.tree.select_path(&root.join("delta")));
        assert_eq!(window.current_path(), Some(root.join("delta")));
        assert!(!imp.tree.is_expanded(&root.join("delta")), "a selected folder is not expanded");

        imp.tree.set_collapse_on_navigate(true);
        window.navigate(&root.join("alpha/beta"));
        wait_for("beta selected again", || imp.tree.selected_path() == Some(root.join("alpha/beta")));
        window.navigate(&root.join("gamma"));
        wait_for("gamma selected again", || imp.tree.selected_path() == Some(root.join("gamma")));
        assert!(!imp.tree.is_expanded(&root.join("alpha")), "collapse-on-navigate closes other folders");
        imp.tree.set_collapse_on_navigate(false);

        // The header's buttons show and change the tree's root, without changing the folder shown.
        assert!(imp.filesystem_root_button.is_active(), "the tree starts at the filesystem root");
        imp.filesystem_home_button.set_active(true);
        assert_eq!(imp.tree.root_path(), Some(glib::home_dir()), "the home button roots the tree at the home folder");
        assert!(!imp.filesystem_root_button.is_active());
        assert_eq!(window.current_path(), Some(root.join("gamma")), "changing the root is not a navigation");

        // A folder outside the root returns the tree to the filesystem root.
        imp.tree.set_root_path(&root.join("alpha")).unwrap();
        window.navigate(&root.join("delta"));
        assert_eq!(imp.tree.root_path(), Some(PathBuf::from("/")));
        assert!(imp.filesystem_root_button.is_active(), "the button follows the root");
        wait_for("delta selected in the new root", || imp.tree.selected_path() == Some(root.join("delta")));

        let _ = fs::remove_dir_all(&root);
    }
}

/// The parameter to activate the action `name` with: `value` for an action that takes one, as a maybe-double,
/// and nothing otherwise.
pub fn action_parameter(name: &str, value: Option<f64>) -> Option<glib::Variant> {
    crate::keybindings::actions::value_info(name).map(|_| value.to_variant())
}

/// Adds the action `full_name` (with its group, such as `video.seek`) to `map`, which is that group, running `run`
/// on the window with the value it is activated with, or the action's default when it is given none.
fn valued_action(map: &impl IsA<gio::ActionMap>, window: &glib::WeakRef<Window>, full_name: &'static str, run: fn(&Window, f64)) {
    let name = full_name.split_once('.').map_or(full_name, |(_, name)| name);
    let default = crate::keybindings::actions::value_info(full_name).map_or(0.0, |info| info.default);
    let action = gio::SimpleAction::new(name, Some(glib::VariantTy::new("md").expect("a valid type")));
    let window = window.clone();
    action.connect_activate(move |_, parameter| {
        let Some(window) = window.upgrade() else { return };
        let value = parameter.and_then(|parameter| parameter.get::<Option<f64>>()).flatten();
        run(&window, value.unwrap_or(default));
    });
    map.add_action(&action);
}

/// Adds an action named `name` to `map` that runs `run` on the window, if the window still exists.
/// A menu item that runs `action` on `path`.
fn path_menu_item(label: &str, action: &str, path: &Path) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&path.to_string_lossy().into_owned().to_variant()));
    item
}

/// Pops up `menu` at (`x`, `y`) within `parent`.
fn popup_menu(parent: &gtk::Widget, menu: &gio::Menu, x: f64, y: f64) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.set_parent(parent);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 0, 0)));
    // The menu activates its item's action after closing, so the popover must stay in the
    // widget tree until then or actions like `win.open-in-new-tab` aren't found.
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}

/// The icon of a bookmark: the tree's own for the home folder and `/`, and a folder for anything else.
fn bookmark_icon(path: &Path) -> &'static str {
    if path == glib::home_dir().as_path() {
        "user-home-symbolic"
    } else if path == Path::new("/") {
        "drive-harddisk-symbolic"
    } else {
        "folder-symbolic"
    }
}

/// The parts of the window the keyboard focus moves between.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Tree,
    /// The viewer while it is open, else the browse view.
    Content,
    Playlist,
}

/// How long the outline around a pane the focus moved to stays up, and how long its name does.
const FOCUS_PULSE: std::time::Duration = std::time::Duration::from_millis(700);
const FOCUS_NOTICE: std::time::Duration = std::time::Duration::from_millis(1200);

/// Outlines `widget` for a moment, which the style sheet fades out.
fn pulse(widget: &gtk::Widget) {
    widget.remove_css_class("focus-pulse");
    widget.add_css_class("focus-pulse");
    let widget = widget.downgrade();
    glib::timeout_add_local_once(FOCUS_PULSE, move || {
        if let Some(widget) = widget.upgrade() {
            widget.remove_css_class("focus-pulse");
        }
    });
}

fn bound_action(map: &impl IsA<gio::ActionMap>, window: &glib::WeakRef<Window>, name: &str, run: fn(&Window)) {
    let action = gio::SimpleAction::new(name, None);
    let window = window.clone();
    action.connect_activate(move |_, _| {
        if let Some(window) = window.upgrade() {
            run(&window);
        }
    });
    map.add_action(&action);
}

/// The items of a context menu for playlist changes to the item at `path`, with `targets` items being acted on: a lone
/// file gets the one that applies to it, and a folder or several items, which may be partly in the playlist, get both.
/// The duration of the file at `path` in milliseconds, from its metadata if that is known, or -1 if it is not. Items of
/// unknown length sort first.
fn duration_millis(info: &MediaInfo, path: &Path) -> i64 {
    let metadata = stamp(path).and_then(|(mtime, size)| info.get(path, mtime, size));
    metadata.and_then(|metadata| metadata.duration).map_or(-1, |seconds| (seconds * 1000.0).round() as i64)
}

fn playlist_menu_items(path: &Path, is_dir: bool, targets: usize) -> Vec<(&'static str, &'static str)> {
    let add = ("Add to Playlist", "win.playlist-add-paths");
    let remove = ("Remove from Playlist", "win.playlist-remove-paths");
    if is_dir || targets > 1 {
        vec![add, remove]
    } else if crate::playlist::contains(path) {
        vec![remove]
    } else if path.file_name().is_some_and(|name| crate::fs::walk::is_media_name(&name.to_string_lossy())) {
        vec![add]
    } else {
        Vec::new()
    }
}

/// The pictures and videos among `targets`, and those in and under the ones that are folders. This reads the disk, so
/// it runs on a worker thread.
fn media_among(targets: &[PathBuf], show_hidden: bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for target in targets {
        if target.is_dir() {
            found.extend(crate::fs::walk::media_files(target, show_hidden));
        } else if target.file_name().is_some_and(|name| crate::fs::walk::is_media_name(&name.to_string_lossy())) {
            found.push(target.clone());
        }
    }
    found
}

/// How a message names what a playlist change was to: the file if there was one, or how many.
fn count_text(paths: &[PathBuf], count: usize) -> String {
    match paths {
        [path] if count == 1 => format!("“{}”", path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())),
        _ if count == 1 => "1 item".to_owned(),
        _ => format!("{count} items"),
    }
}

/// Reads the playlist file at `path`. Entries that are not pictures or videos, by their names, count as skipped: audio
/// is common in these files and the viewer has nothing to show for it.
fn read_playlist_file(path: &Path) -> std::io::Result<format::Parsed> {
    let bytes = std::fs::read(path)?;
    Ok(playlist_from_bytes(&bytes, path.parent().unwrap_or(Path::new("/"))))
}

/// Shows the playlist window's header as `style` says: `full`, `compact` (shorter, with a smaller title), or `none`, which hides it.
fn apply_playlist_header_style(header: &adw::HeaderBar, style: &str) {
    header.set_visible(style != "none");
    header.set_show_title(style != "none");
    if style == "compact" {
        header.add_css_class("playlist-header-compact");
    } else {
        header.remove_css_class("playlist-header-compact");
    }
}

/// The pictures and videos a playlist's text lists, with how many entries it had that are not those left out. Relative
/// entries are found from `base`.
fn playlist_from_bytes(bytes: &[u8], base: &Path) -> format::Parsed {
    let mut parsed = format::read(bytes, base);
    let listed = parsed.paths.len();
    parsed.paths.retain(|item| playlist::is_playable(item));
    parsed.skipped += listed - parsed.paths.len();
    parsed
}

/// The file types the playlist dialogs offer: all the formats, then each one.
fn playlist_file_filters() -> gio::ListStore {
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    for (name, suffixes) in [
        ("All Playlists", &["m3u8", "m3u", "pls", "xspf"][..]),
        ("M3U Playlist", &["m3u8", "m3u"][..]),
        ("PLS Playlist", &["pls"][..]),
        ("XSPF Playlist", &["xspf"][..]),
    ] {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(name));
        for suffix in suffixes {
            filter.add_suffix(suffix);
        }
        filters.append(&filter);
    }
    filters
}

/// The heading and body of the properties dialog for the file at `path`, read from the file.
fn properties_text(path: &Path) -> (String, String) {
    let attributes = "standard::display-name,standard::size,standard::content-type,time::modified";
    let file = gio::File::for_path(path);
    match file.query_info(attributes, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
        Ok(info) => {
            let mut lines = vec![format!("Location: {}", path.parent().map_or_else(String::new, |p| p.display().to_string()))];
            if let Some(kind) = info.content_type() {
                lines.push(format!("Type: {}", gio::content_type_get_description(&kind)));
            }
            if info.file_type() != gio::FileType::Directory {
                lines.push(format!("Size: {}", glib::format_size(info.size() as u64)));
            }
            if let Some(time) = info.modification_date_time().and_then(|t| t.format("%Y-%m-%d %H:%M").ok()) {
                lines.push(format!("Modified: {time}"));
            }
            (info.display_name().to_string(), lines.join("\n"))
        }
        Err(err) => (path.display().to_string(), err.to_string()),
    }
}

/// The folder a step moves to from `current`, in depth-first order of the folder tree: the next or previous folder,
/// or with `media` the next or previous one that holds a picture or video. This reads the disk, so it runs on a
/// worker thread.
fn walk_folder(current: &Path, show_hidden: bool, forward: bool, media: bool) -> Option<PathBuf> {
    let children = |folder: &Path| crate::fs::walk::child_directories(folder, show_hidden);
    match (forward, media) {
        (true, false) => crate::fs::walk::next_folder(current, &children),
        (false, false) => crate::fs::walk::previous_folder(current, &children),
        (true, true) => crate::fs::walk::next_media_folder(current, &children, &|folder| crate::fs::walk::has_media(folder)),
        (false, true) => crate::fs::walk::previous_media_folder(current, &children, &|folder| crate::fs::walk::has_media(folder)),
    }
}
