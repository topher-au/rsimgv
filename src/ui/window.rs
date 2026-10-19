use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::path::{Path, PathBuf};

use adw::subclass::prelude::*;
use gio::prelude::StaticType;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use super::fs_tree::TreeSnapshot;
use super::path_bar::OpenTarget;
use super::{BrowseView, FsTree, HeaderBar, NavigationBar, OverlaySplitView, PathBar, PreferencesDialog};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rimgv/ui/window.ui")]
    pub struct Window {
        #[template_child]
        pub tree: TemplateChild<FsTree>,
        #[template_child]
        pub split: TemplateChild<OverlaySplitView>,
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

impl Window {
    pub fn new(app: &adw::Application, settings: &gio::Settings) -> Self {
        let window: Self = glib::Object::builder().property("application", app).build();
        let imp = window.imp();
        let _ = imp.settings.set(settings.clone());

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

        window.add_context_actions();
        window.connect_sources();
        window.navigate(&glib::home_dir());
        window
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
        imp.current_path.replace(Some(path.to_path_buf()));
        if let Err(err) = imp.path_bar.set_path(path) {
            eprintln!("rsimgv: {err:#}");
        }
        if let Some(browse) = self.active_browse() {
            if browse.path().as_deref() != Some(path) {
                browse.set_path(path);
            }
        }
        if let Some(page) = imp.tab_view.selected_page() {
            page.set_title(&folder_name(path));
        }
    }

    /// Opens a new tab at `path` and switches to it.
    pub fn open_tab(&self, path: &Path) {
        self.add_tab(path, true);
    }

    /// Opens a new tab at `path` behind the current one, leaving the current tab selected.
    pub fn open_background_tab(&self, path: &Path) {
        self.add_tab(path, false);
    }

    fn add_tab(&self, path: &Path, select: bool) {
        let imp = self.imp();
        let id = imp.next_tab.get();
        imp.next_tab.set(id + 1);
        imp.tabs.borrow_mut().insert(
            id,
            TabState {
                path: Some(path.to_path_buf()),
                history: vec![path.to_path_buf()],
                index: 0,
                tree: TreeSnapshot::revealing(path),
            },
        );
        let browse = BrowseView::new();
        browse.set_widget_name(&id.to_string());
        self.attach_browse(&browse);
        let page = imp.tab_view.append(&browse);
        page.set_title(&folder_name(path));
        // A tab opened in the background loads its folder now; its history and tree wait in its state.
        if !select && imp.tab_view.n_pages() > 1 {
            browse.set_path(path);
            return;
        }
        imp.tab_view.set_selected_page(&page);
        if imp.active_tab.get() != Some(id) {
            self.switch_to_tab(id);
        }
    }

    /// Records what the active tab shows, so switching away from it keeps it.
    fn save_active_tab(&self) {
        let imp = self.imp();
        let Some(id) = imp.active_tab.get() else { return };
        let (history, index) = imp.navigation.history_state();
        let state = TabState {
            path: self.current_path(),
            history,
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
        imp.navigation.set_history_state(state.history, state.index);
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
        imp.tree.connect_location_selected({
            let weak = weak.clone();
            move |_, path| {
                if let Some(window) = weak.upgrade() {
                    window.navigate_with(path, Reveal::SelectOnly);
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
            }
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
        if let Some(settings) = imp.settings.get() {
            settings.bind("item-size", browse, "item-size").build();
            settings.bind("wrap-names", browse, "wrap-names").build();
            settings.bind("media-only", browse, "media-only").build();
            settings.bind("show-hidden", browse, "show-hidden").build();
        }
        let weak = self.downgrade();
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
        if let Some(settings) = self.imp().settings.get() {
            let dialog = PreferencesDialog::new(settings);
            adw::prelude::AdwDialogExt::present(&dialog, Some(self));
        }
    }
}

/// Saves whether the window is maximized and, if it is not, its size. A maximized window is as big as
/// the screen, so the size kept is the one from before it was maximized.
fn save_window_state(window: &Window, settings: &gio::Settings) {
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
    index: usize,
    tree: TreeSnapshot,
}

/// The id of the tab on `page`, stored as the name of its child.
fn tab_id(page: &adw::TabPage) -> Option<u64> {
    page.child().widget_name().parse().ok()
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
