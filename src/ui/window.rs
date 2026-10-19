use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use adw::subclass::prelude::*;
use gio::prelude::StaticType;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

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
        pub sidebar_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub navigation: TemplateChild<NavigationBar>,
        #[template_child]
        pub title_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub path_bar: TemplateChild<PathBar>,
        #[template_child]
        pub location_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub browse: TemplateChild<BrowseView>,
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
        let tree = (*imp.tree).clone();
        let split = (*imp.split).clone();
        let sidebar_toggle = (*imp.sidebar_toggle).clone();

        // The toggle shows and hides the sidebar, and follows the split view's state.
        sidebar_toggle.set_active(split.show_sidebar());
        sidebar_toggle.connect_toggled({
            let split = split.clone();
            move |button| split.set_show_sidebar(button.is_active())
        });
        split.connect_show_sidebar_changed(move |_, shown| {
            sidebar_toggle.set_active(shown);
            sidebar_toggle.set_tooltip_text(Some(if shown { "Hide sidebar" } else { "Show sidebar" }));
        });

        if let Err(err) = tree.set_root_path(Path::new("/")) {
            eprintln!("rsimgv: {err:#}");
        }

        // Settings drive the view: the browse view's properties and the tree follow their keys, and
        // the menu's "Show Hidden Directories" item is an action bound to its key.
        settings.bind("item-size", &*imp.browse, "item-size").build();
        settings.bind("wrap-names", &*imp.browse, "wrap-names").build();
        settings.bind("media-only", &*imp.browse, "media-only").build();
        settings.bind("show-hidden", &*imp.browse, "show-hidden").build();
        settings.bind("collapse-on-navigate", &tree, "collapse-on-navigate").build();
        tree.set_show_hidden(settings.boolean("show-hidden"));
        let tree_for_settings = tree.clone();
        settings.connect_changed(Some("show-hidden"), move |settings, _| {
            tree_for_settings.set_show_hidden(settings.boolean("show-hidden"));
        });
        window.add_action(&settings.create_action("show-hidden"));

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

        imp.updating.set(true);
        imp.current_path.replace(Some(path.to_path_buf()));
        imp.navigation.set_location(path);
        if let Err(err) = imp.path_bar.set_path(path) {
            eprintln!("rsimgv: {err:#}");
        }
        match reveal {
            Reveal::Expand => imp.tree.reveal_path(path),
            Reveal::SelectOnly => imp.tree.reveal_path_selecting_only(path),
        }
        imp.browse.set_path(path);
        imp.updating.set(false);
        true
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
        // New tabs and windows are not implemented yet, so every target navigates in this window.
        imp.path_bar.connect_open_location({
            let weak = weak.clone();
            move |_, path, _| {
                if let Some(window) = weak.upgrade() {
                    window.navigate(path);
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

        imp.browse.connect_directory_activated(move |_, path| {
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
        let window = Window::new(&app, &settings);
        let imp = window.imp();

        // Every widget follows one navigation.
        assert!(window.navigate(&root.join("alpha")));
        assert_eq!(window.current_path(), Some(root.join("alpha")));
        assert_eq!(imp.path_bar.path(), Some(root.join("alpha")));
        assert_eq!(imp.navigation.location(), Some(root.join("alpha")));
        wait_for("alpha listed in the browse view", || imp.browse.item_names() == ["beta"]);

        // Staying put is fine; a file is refused and changes nothing.
        assert!(window.navigate(&root.join("alpha")));
        assert!(!window.navigate(&root.join("file.txt")));
        assert_eq!(window.current_path(), Some(root.join("alpha")));

        // A subfolder entered from the browse list.
        imp.browse.activate(0);
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
