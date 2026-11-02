use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::path::{Path, PathBuf};

use adw::subclass::prelude::*;
use gio::prelude::StaticType;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::keybindings::actions::Scope;
use super::browse_view::{InfoField, ListedFile};
use crate::bookmarks::{self, Bookmark};
use super::image_canvas::{Fit, PAN_STEP};
use super::media_controls::SEEK_STEP;
use super::media_viewer::{side_from_name, ZoomIndicator, VOLUME_STEP};
use super::key_handler::ActionKeyHandler;
use super::fs_tree::TreeSnapshot;
use super::path_bar::OpenTarget;
use super::{BrowseView, FsTree, HeaderBar, MediaViewer, NavigationBar, OverlaySplitView, PathBar, PreferencesDialog, ShownItem};

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
        pub viewer: TemplateChild<MediaViewer>,
        #[template_child]
        pub bookmarks_expander: TemplateChild<gtk::Expander>,
        #[template_child]
        pub bookmarks_list: TemplateChild<gtk::ListBox>,
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
        tree.set_show_hidden(settings.boolean("show-hidden"));
        let tree_for_settings = tree.clone();
        settings.connect_changed(Some("show-hidden"), move |settings, _| {
            tree_for_settings.set_show_hidden(settings.boolean("show-hidden"));
        });
        window.add_action(&settings.create_action("show-hidden"));
        add_setting_radio_action(&window, settings, "sort-by");
        add_setting_radio_action(&window, settings, "sort-order");
        add_show_directories_action(&window, settings);

        window.add_context_actions();
        window.add_tab_actions();
        window.add_binding_actions();
        let keys = ActionKeyHandler::install(&window);
        let _ = window.imp().key_handler.set(keys);
        window.connect_sources();
        window.refresh_bookmarks();
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
        if file.parent().is_none() || file.parent() != self.current_path().as_deref() {
            return;
        }
        if let Some(view) = self.active_browse() {
            let file = file.to_string_lossy().into_owned();
            view.set_pending(move |view: &BrowseView| {
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
        for name in ["open-folder", "open-in-new-tab", "open-in-new-window", "copy-path", "show-properties"] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = weak.clone();
            action.connect_activate(move |_, parameter| {
                let (Some(window), Some(path)) = (weak.upgrade(), parameter.and_then(|p| p.get::<String>())) else {
                    return;
                };
                let path = PathBuf::from(path);
                match name {
                    "open-folder" => {
                        window.navigate(&path);
                    }
                    "open-in-new-tab" if path.is_dir() => window.open_background_tab(&path),
                    "open-in-new-window" => window.open_in_new_window(&path),
                    "copy-path" => window.clipboard().set_text(&path.to_string_lossy()),
                    "show-properties" => window.show_properties(&path),
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
        let target = path.to_string_lossy().into_owned();
        let item = |label: &str, action: &str| {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
            item
        };
        let menu = gio::Menu::new();
        if is_dir {
            menu.append_item(&item("Open", "win.open-folder"));
            menu.append_item(&item("Open in New Tab", "win.open-in-new-tab"));
            menu.append_item(&item("Open in New Window", "win.open-in-new-window"));
        }
        let details = gio::Menu::new();
        details.append_item(&item("Copy Path", "win.copy-path"));
        details.append_item(&item("Properties", "win.show-properties"));
        menu.append_section(None, &details);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
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
        let file = gio::File::for_path(path);
        let attributes = "standard::display-name,standard::size,standard::content-type,time::modified";
        let (heading, body) = match file.query_info(attributes, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
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
        };
        let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
        adw::prelude::AlertDialogExt::add_response(&dialog, "close", "Close");
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Shows `path` in the path bar, the browse view, and the tab's title. It does not change the
    /// history or the tree; navigation and tab switching do that.
    fn show_path(&self, path: &Path) {
        let imp = self.imp();
        let previous = imp.current_path.replace(Some(path.to_path_buf()));
        if let Err(err) = imp.path_bar.set_path(path) {
            eprintln!("rsimgv: {err:#}");
        }
        if let Some(browse) = self.active_browse() {
            if browse.path().as_deref() != Some(path) {
                // Once the folder lists, the item selected the last time this location was shown is selected again,
                // or else, going up to a parent, the item that leads to the folder shown before.
                let selected = imp.navigation.selected().or_else(|| previous.as_deref().and_then(|previous| child_toward(path, previous)));
                if let Some(name) = selected {
                    browse.set_pending(move |view| {
                        view.select_named(&name);
                    });
                }
                browse.set_path(path);
            }
        }
        if let Some(page) = imp.tab_view.selected_page() {
            label_page(&page, path);
        }
    }

    /// Shows `path` in the viewer, which steps through the pictures and videos `view` lists. A file the
    /// view does not list (one that is not a picture or video) is not shown.
    fn open_viewer(&self, view: &BrowseView, path: &Path) {
        let paths = view.media_paths();
        let Some(index) = paths.iter().position(|listed| listed == path) else { return };
        let imp = self.imp();
        imp.viewer.show(paths, index);
        imp.viewer_open.set(true);
        imp.pages.set_visible_child(&*imp.viewer);
        imp.viewer.grab_focus();
        if imp.settings.get().is_some_and(|settings| settings.boolean("open-fullscreen")) && !self.is_fullscreen() {
            self.fullscreen();
            imp.viewer_fullscreened.set(true);
        }
        self.update_title();
    }

    /// Returns from the viewer to the browse view, releasing what the viewer held.
    fn close_viewer(&self) {
        let imp = self.imp();
        imp.viewer.close();
        imp.viewer_open.set(false);
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
        viewer.set_default_fit(Fit::from_name(&settings.string("image-fit")).unwrap_or_default());
        viewer.set_smooth_scrolling(settings.boolean("smooth-scrolling"));
        viewer.set_scaling(settings.string("scaling-filter") == "nearest", settings.boolean("smooth-upscaling"));
        viewer.set_zoom_indicator(ZoomIndicator::from_name(&settings.string("zoom-indicator")).unwrap_or_default());
        viewer.set_auto_hide_cursor(settings.boolean("auto-hide-cursor"));
        viewer.set_blurred_background(settings.boolean("viewer-blurred-background"));
        viewer.set_status_bar(settings.boolean("viewer-status-bar"));
        viewer.set_status_overlay_position(&settings.string("viewer-status-overlay-position"));
        viewer.set_svp(crate::svp::Config::from_settings(settings));
        self.update_title();
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
        bound_action(self, &weak, "settings", Window::show_preferences);
        bound_action(self, &weak, "settings-keybindings", |window| window.present_preferences(Some("keybindings")));
        bound_action(self, &weak, "open-folder", Window::open_folder);
        bound_action(self, &weak, "bookmark-folder", Window::bookmark_current_folder);
        bound_action(self, &weak, "toggle-bookmarks", |window| {
            let expander = &window.imp().bookmarks_expander;
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
        bound_action(&image, &weak, "fit-original", |window| window.imp().viewer.set_fit(Fit::Original));
        bound_action(&image, &weak, "pan-up", |window| window.imp().viewer.pan(0.0, -PAN_STEP));
        bound_action(&image, &weak, "pan-down", |window| window.imp().viewer.pan(0.0, PAN_STEP));
        bound_action(&image, &weak, "pan-left", |window| window.imp().viewer.pan(-PAN_STEP, 0.0));
        bound_action(&image, &weak, "pan-right", |window| window.imp().viewer.pan(PAN_STEP, 0.0));
        self.insert_action_group("image", Some(&image));
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

    /// Asks for a name for the folder shown, then bookmarks it. The name defaults to the folder's own.
    fn bookmark_current_folder(&self) {
        let Some(path) = self.current_path() else { return };
        let dialog = adw::AlertDialog::new(Some("Bookmark Folder"), Some(&path.display().to_string()));
        adw::prelude::AlertDialogExt::add_response(&dialog, "cancel", "Cancel");
        adw::prelude::AlertDialogExt::add_response(&dialog, "save", "Save");
        adw::prelude::AlertDialogExt::set_response_appearance(&dialog, "save", adw::ResponseAppearance::Suggested);
        adw::prelude::AlertDialogExt::set_default_response(&dialog, Some("save"));
        adw::prelude::AlertDialogExt::set_close_response(&dialog, "cancel");

        let entry = gtk::Entry::new();
        entry.set_text(&bookmarks::default_name(&path));
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

    /// Shows the bookmarks in the sidebar: each one's name, a button to remove it, and a click opens it.
    fn refresh_bookmarks(&self) {
        let list = &self.imp().bookmarks_list;
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        let saved: Vec<Bookmark> = bookmarks::with(|bookmarks| bookmarks.clone());
        for bookmark in saved {
            list.append(&self.bookmark_row(&bookmark));
        }
    }

    fn bookmark_row(&self, bookmark: &Bookmark) -> gtk::ListBoxRow {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = gtk::Label::new(Some(&bookmark.name));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_tooltip_text(Some(&bookmark.path.display().to_string()));
        content.append(&label);

        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.set_tooltip_text(Some("Remove bookmark"));
        remove.add_css_class("flat");
        let weak = self.downgrade();
        let path = bookmark.path.clone();
        remove.connect_clicked(move |_| {
            bookmarks::with(|list| list.retain(|saved| saved.path != path));
            if let Some(window) = weak.upgrade() {
                window.bookmarks_changed();
            }
        });
        content.append(&remove);

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&content));
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
        browse.set_pending(move |view| {
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
    /// Global applies everywhere and is not listed.
    pub(super) fn active_scopes(&self) -> Vec<Scope> {
        let imp = self.imp();
        if !imp.viewer_open.get() {
            return vec![Scope::Browse];
        }
        let media = if imp.viewer.shows_video() { Scope::Video } else { Scope::Image };
        vec![Scope::View, media]
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
        self.show_media_folder(target, forward)
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
        let Some(current) = self.current_path() else { return };
        let show_hidden = self.show_hidden();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let walked = gio::spawn_blocking(move || walk_folder(&current, show_hidden, forward, true)).await;
            if let (Some(window), Some(target)) = (weak.upgrade(), walked.ok().flatten()) {
                window.show_media_folder(target, forward);
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

    /// Opens the folder `target` that a media-folder step found: in the viewer its first picture or video is shown,
    /// and while browsing its first or last is selected, once the folder has listed. Returns the folder moved to, or
    /// `None` when the window could not move there.
    fn show_media_folder(&self, target: PathBuf, forward: bool) -> Option<PathBuf> {
        // The arrangement waits for the folder to list, so it is set before the folder is shown.
        let viewer = self.viewer_is_open();
        let last = !forward && !viewer;
        let weak = self.downgrade();
        let view = self.active_browse();
        if let Some(view) = &view {
            view.set_pending(move |view: &BrowseView| {
                if viewer {
                    let first = view.media_paths().into_iter().next();
                    if let (Some(window), Some(path)) = (weak.upgrade(), first) {
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
            }
        });
        imp.viewer.connect_item_changed({
            let weak = weak.clone();
            move |_, path| {
                let Some(window) = weak.upgrade() else { return };
                if let Some(view) = window.active_browse() {
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
            if window.imp().viewer_open.get() && window.active_browse().as_ref() == Some(view) {
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
            settings.bind("sort-by", browse, "sort-by").build();
            settings.bind("sort-order", browse, "sort-order").build();
        }
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
const VIEWER_SETTINGS: [&str; 20] = [
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

/// Adds a stateful action for the string setting `key`, whose radio items in the sort menu set it. Menu items
/// activate their action, and some pass the new value as a change of state, so both set the setting.
fn add_setting_radio_action(window: &Window, settings: &gio::Settings, key: &'static str) {
    let action = gio::SimpleAction::new_stateful(key, Some(glib::VariantTy::STRING), &settings.string(key).to_variant());
    let set_from = {
        let settings = settings.clone();
        move |parameter: Option<&glib::Variant>| {
            if let Some(value) = parameter.and_then(|value| value.get::<String>()) {
                let _ = settings.set_string(key, &value);
            }
        }
    };
    action.connect_activate({
        let set_from = set_from.clone();
        move |_, parameter| set_from(parameter)
    });
    action.connect_change_state(move |_, parameter| set_from(parameter));
    settings.connect_changed(Some(key), {
        let action = action.clone();
        move |settings, _| action.set_state(&settings.string(key).to_variant())
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
        assert_eq!(window.current_path(), Some(root.join("docs")));

        gtk::prelude::WidgetExt::activate_action(&window, "win.open-folder", Some(&root.to_string_lossy().into_owned().to_variant())).expect("open folder");
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
        window.set_size_request(812, 520);
        pump(150);
        assert_eq!((window.width(), window.height()), (812, 520), "the window grew to the new size");
        assert_eq!(settings.int("window-width"), 812, "the width is saved without closing");
        assert_eq!(settings.int("window-height"), 520, "the height is saved without closing");
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
        assert_eq!((window.width(), window.height()), (700, 450), "the window is the size it was set to");
        window.close();
        assert_eq!(settings.int("window-width"), 700, "the width is saved");
        assert_eq!(settings.int("window-height"), 450, "the height is saved");
        assert!(!settings.boolean("window-maximized"), "an unmaximized window is saved as such");

        // A window opened afterwards starts at the saved size.
        let reopened = Window::new(&app, &settings);
        reopened.present();
        assert_eq!(reopened.default_width(), 700);
        assert_eq!(reopened.default_height(), 450);
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
