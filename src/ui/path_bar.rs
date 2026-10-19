//! A breadcrumb path bar, a port of `NautilusPathBar` (nautilus `src/nautilus-pathbar.c`).
//!
//! The current folder is the last segment: bold, taking the spare width, and clicking it asks to
//! edit the location. Each ancestor is a button. Clicking one asks to open it; middle-click and
//! Ctrl-click ask for a new tab or window, and right-click opens a menu with those and Properties.
//! Dragging files over a segment opens it after a short hover, and dropping them emits
//! `files-dropped`. The row scrolls sideways with the wheel and stays scrolled to the newest
//! segment. Segments start at the nearest of `/`, the home directory, or a mount point, as in
//! Nautilus, and root, home and mount segments have an icon and no separator.
//!
//! Left out, because this application browses local paths only: the trash, recent, starred,
//! network, computer, burn and admin-root locations, which Nautilus gets from URI schemes. The
//! current-folder menu's contents are supplied by the application through `set_current_view_menu`.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::glib::prelude::{StaticType, ToVariant};
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// Hover time before a drag over a segment opens it, in milliseconds (`HOVER_TIMEOUT` in nautilus-dnd.h).
const HOVER_TIMEOUT_MS: u64 = 500;
/// Labels longer than this many characters get ellipsized (`NAUTILUS_PATH_BAR_BUTTON_ELLIPSIZE_MINIMUM_CHARS`).
const ELLIPSIZE_MINIMUM_CHARS: i32 = 7;
const ICON_FILESYSTEM: &str = "drive-harddisk-symbolic";
const ICON_HOME: &str = "user-home-symbolic";

/// Where a segment asks to open. `Here` is the default, and matches a plain click.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "RsimgvPathBarOpenTarget")]
pub enum OpenTarget {
    #[default]
    #[enum_value(name = "Here", nick = "here")]
    Here,
    #[enum_value(name = "New tab", nick = "new-tab")]
    NewTab,
    #[enum_value(name = "New window", nick = "new-window")]
    NewWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Root,
    Home,
    Mount,
    Normal,
}

struct Segment {
    path: PathBuf,
    kind: Kind,
    label: String,
    icon: Option<gio::Icon>,
}

fn icon_named(name: &str) -> gio::Icon {
    gio::ThemedIcon::new(name).upcast()
}

/// The mount whose root is exactly `path`, as `nautilus_get_mounted_mount_for_root` finds it.
fn mount_for(path: &Path) -> Option<gio::Mount> {
    gio::VolumeMonitor::get()
        .mounts()
        .into_iter()
        .find(|mount| mount.root().path().as_deref() == Some(path))
}

/// The kind of segment `path` starts, if it is one: `/`, the home directory, or a mount point.
fn start_kind(path: &Path, home: &Path) -> Option<Kind> {
    if path == Path::new("/") {
        Some(Kind::Root)
    } else if path == home {
        Some(Kind::Home)
    } else if mount_for(path).is_some() {
        Some(Kind::Mount)
    } else {
        None
    }
}

/// The segments for `path`, from the nearest starting segment down to `path` itself.
fn segments(path: &Path) -> anyhow::Result<Vec<Segment>> {
    anyhow::ensure!(path.is_absolute(), "{} is not an absolute path", path.display());
    let home = glib::home_dir();

    // Climb from `path` to the nearest starting segment; `/` always qualifies.
    let chain: Vec<&Path> = path.ancestors().collect();
    let start = chain
        .iter()
        .position(|candidate| start_kind(candidate, &home).is_some())
        .unwrap_or(chain.len() - 1);

    chain[..=start]
        .iter()
        .rev()
        .enumerate()
        .map(|(position, candidate)| {
            let is_first = position == 0;
            let kind = match start_kind(candidate, &home) {
                Some(kind) if is_first => kind,
                _ => Kind::Normal,
            };
            let segment_path = candidate.to_path_buf();
            Ok(match kind {
                Kind::Root => Segment {
                    path: segment_path,
                    kind,
                    label: glib::os_info("NAME").map_or_else(|| "Operating System".to_owned(), |name| name.to_string()),
                    icon: Some(icon_named(ICON_FILESYSTEM)),
                },
                Kind::Home => Segment {
                    path: segment_path,
                    kind,
                    label: "Home".to_owned(),
                    icon: Some(icon_named(ICON_HOME)),
                },
                Kind::Mount => {
                    let mount = mount_for(candidate);
                    Segment {
                        path: segment_path,
                        kind,
                        label: mount.as_ref().map_or_else(String::new, |m| m.name().to_string()),
                        icon: mount.map(|m| m.symbolic_icon()),
                    }
                }
                Kind::Normal => Segment {
                    label: candidate.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                    path: segment_path,
                    kind,
                    icon: None,
                },
            })
        })
        .collect()
}

/// Ellipsizes a label the way Nautilus does: long names shrink in the middle, to a minimum width
/// (four times as wide for the current folder); short names are not ellipsized at all.
fn set_label_width(label: &gtk::Label, text: &str, is_current: bool) {
    let minimum = if is_current { 4 * ELLIPSIZE_MINIMUM_CHARS } else { ELLIPSIZE_MINIMUM_CHARS };
    if text.chars().count() as f64 > f64::from(minimum) * 1.5 {
        label.set_width_chars(minimum);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    } else {
        label.set_width_chars(-1);
        label.set_ellipsize(gtk::pango::EllipsizeMode::None);
    }
}

/// Builds one segment: its separator (normal segments only), and its button with icon and label.
fn build_segment(segment: &Segment, is_current: bool) -> gtk::Box {
    let builder = gtk::Builder::from_resource("/au/topher/rimgv/ui/path_bar_segment.ui");
    let container: gtk::Box = builder.object("container").expect("container");
    let separator: gtk::Image = builder.object("separator").expect("separator");
    let button: gtk::Button = builder.object("button").expect("button");
    let icon: gtk::Image = builder.object("icon").expect("icon");
    let label: gtk::Label = builder.object("label").expect("label");

    separator.set_visible(segment.kind == Kind::Normal);
    match &segment.icon {
        Some(gicon) => {
            icon.set_from_gicon(gicon);
            icon.set_visible(true);
        }
        None => icon.set_visible(false),
    }
    label.set_label(&segment.label);
    set_label_width(&label, &segment.label, is_current);
    button.set_tooltip_text(Some(&segment.label));
    button.add_css_class("rsimgv-path-button");

    if is_current {
        button.add_css_class("current-dir");
        button.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        label.set_single_line_mode(true);
        label.add_css_class("heading");
    } else {
        label.add_css_class("dim-label");
        icon.add_css_class("dim-label");
    }

    attach_click(&button, &segment.path, is_current);
    attach_drop(&button, &segment.path);
    container
}

/// Left, middle, right and Ctrl-click, as the pathbar's click gesture handles them.
fn attach_click(button: &gtk::Button, path: &Path, is_current: bool) {
    let path = path.to_path_buf();
    let is_current_flag = is_current;

    // Left click without modifiers is the button's `clicked`, handled below.
    button.connect_clicked({
        let path = path.clone();
        move |button| {
            let Some(bar) = bar_of(button) else { return };
            if is_current_flag {
                bar.emit_by_name::<()>("edit-location", &[]);
            } else {
                bar.emit_open(&path, OpenTarget::Here);
            }
        }
    });

    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.connect_pressed(move |gesture, n_press, x, y| {
        if n_press != 1 {
            return;
        }
        let Some(button) = gesture.widget().and_downcast::<gtk::Button>() else { return };
        let Some(bar) = bar_of(&button) else { return };
        let state = gesture.current_event_state();
        let modifiers = state & gtk::accelerator_get_default_mod_mask();
        match gesture.current_button() {
            gdk::BUTTON_MIDDLE if modifiers.is_empty() => {
                bar.emit_open(&path, OpenTarget::NewTab);
            }
            gdk::BUTTON_SECONDARY => {
                if is_current_flag {
                    return;
                }
                bar.show_context_menu(&button, &path, x, y);
            }
            gdk::BUTTON_PRIMARY if state.contains(gdk::ModifierType::CONTROL_MASK) => {
                bar.emit_open(&path, OpenTarget::NewWindow);
            }
            _ => return,
        }
        // Claiming the sequence stops the window from also handling these clicks, as Nautilus does.
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    button.add_controller(click);
}

/// Drops of files onto a segment: hovering opens it after a short delay, and a drop reports the files.
fn attach_drop(button: &gtk::Button, path: &Path) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let hover_timer: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
    let path = path.to_path_buf();

    target.connect_enter({
        let hover_timer = hover_timer.clone();
        let path = path.clone();
        let button = button.downgrade();
        move |_, _, _| {
            if hover_timer.take().is_none() {
                let timer = hover_timer.clone();
                let button = button.clone();
                let path = path.clone();
                let id = glib::timeout_add_local(Duration::from_millis(HOVER_TIMEOUT_MS), move || {
                    timer.set(None);
                    if let Some(bar) = button.upgrade().and_then(|button| bar_of(&button)) {
                        bar.emit_open(&path, OpenTarget::Here);
                    }
                    glib::ControlFlow::Break
                });
                hover_timer.set(Some(id));
            } else {
                // The timer was already running; keep it.
            }
            gdk::DragAction::COPY
        }
    });
    target.connect_motion(|_, _, _| gdk::DragAction::COPY);
    target.connect_leave({
        let hover_timer = hover_timer.clone();
        move |_| {
            if let Some(id) = hover_timer.take() {
                id.remove();
            }
        }
    });
    target.connect_drop({
        let path = path.clone();
        move |target, value, _, _| {
            if let Some(id) = hover_timer.take() {
                id.remove();
            }
            let Ok(file_list) = value.get::<gdk::FileList>() else { return false };
            let Some(bar) = target.widget().and_then(|widget| bar_of(&widget)) else { return false };
            let files: Vec<String> = file_list
                .files()
                .iter()
                .filter_map(|file| file.path())
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            bar.emit_files_dropped(&path, &files);
            true
        }
    });
    button.add_controller(target);
}

/// The path bar that owns a segment widget.
fn bar_of(widget: &impl IsA<gtk::Widget>) -> Option<PathBar> {
    widget.ancestor(PathBar::static_type()).and_downcast::<PathBar>()
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rimgv/ui/path_bar.ui")]
    pub struct PathBar {
        #[template_child]
        pub scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub segments: TemplateChild<gtk::Box>,
        #[template_child]
        pub current_view_menu_button: TemplateChild<gtk::MenuButton>,
        pub(super) path: RefCell<Option<PathBuf>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PathBar {
        const NAME: &'static str = "RsimgvPathBar";
        type Type = super::PathBar;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PathBar {
        fn signals() -> &'static [Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("open-location")
                        .param_types([String::static_type(), OpenTarget::static_type()])
                        .build(),
                    Signal::builder("edit-location").build(),
                    Signal::builder("properties-requested")
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder("files-dropped")
                        .param_types([String::static_type(), glib::Variant::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_css_class("linked");
            obj.add_css_class("rsimgv-pathbar");

            // Keep the newest segment in view whenever the content grows.
            let hadjustment = self.scroller.hadjustment();
            hadjustment.connect_changed(|adjustment| {
                adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
            });

            // Vertical wheel motion scrolls the row sideways, one step per notch.
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            let horizontal = self.scroller.hadjustment();
            scroll.connect_scroll(move |_, _, dy| {
                if dy == 0.0 {
                    return glib::Propagation::Proceed;
                }
                horizontal.set_value(horizontal.value() + dy * horizontal.step_increment());
                glib::Propagation::Stop
            });
            self.scroller.add_controller(scroll);

            // Actions behind the right-click menu. Their targets are the segment's path.
            let actions = gio::SimpleActionGroup::new();
            for (name, kind) in [
                ("open-item-new-window", OpenTarget::NewWindow),
                ("open-item-new-tab", OpenTarget::NewTab),
            ] {
                let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
                let weak = obj.downgrade();
                action.connect_activate(move |_, parameter| {
                    let (Some(bar), Some(path)) = (weak.upgrade(), parameter.and_then(|p| p.get::<String>())) else {
                        return;
                    };
                    bar.emit_open(Path::new(&path), kind);
                });
                actions.add_action(&action);
            }
            let properties = gio::SimpleAction::new("properties", Some(glib::VariantTy::STRING));
            let weak = obj.downgrade();
            properties.connect_activate(move |_, parameter| {
                let (Some(bar), Some(path)) = (weak.upgrade(), parameter.and_then(|p| p.get::<String>())) else {
                    return;
                };
                bar.emit_by_name::<()>("properties-requested", &[&path]);
            });
            actions.add_action(&properties);
            obj.insert_action_group("pathbar", Some(&actions));
        }
    }

    impl WidgetImpl for PathBar {}
    impl BoxImpl for PathBar {}
}

glib::wrapper! {
    pub struct PathBar(ObjectSubclass<imp::PathBar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl PathBar {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Shows `path`. Fails if the path is not absolute.
    pub fn set_path(&self, path: &Path) -> anyhow::Result<()> {
        let segments = segments(path)?;
        let segment_box = &self.imp().segments;
        while let Some(child) = segment_box.first_child() {
            segment_box.remove(&child);
        }
        let last = segments.len() - 1;
        for (index, segment) in segments.iter().enumerate() {
            segment_box.append(&build_segment(segment, index == last));
        }
        self.imp().path.replace(Some(path.to_path_buf()));
        Ok(())
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.imp().path.borrow().clone()
    }

    /// Sets the menu behind the button at the end of the bar, the current folder's menu.
    pub fn set_current_view_menu(&self, menu: &impl IsA<gio::MenuModel>) {
        self.imp().current_view_menu_button.set_menu_model(Some(menu));
    }

    /// Opens the current folder's menu, as the Nautilus pathbar's `show_current_location_menu` does.
    pub fn show_current_view_menu(&self) {
        self.imp().current_view_menu_button.popup();
    }

    /// Calls `callback(bar, path, target)` when a segment asks to open a folder.
    pub fn connect_open_location<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, &Path, OpenTarget) + 'static,
    {
        self.connect_local("open-location", false, move |values| {
            let bar = values[0].get::<PathBar>().expect("bar argument");
            let path = values[1].get::<String>().expect("path argument");
            let target = values[2].get::<OpenTarget>().expect("target argument");
            callback(&bar, Path::new(&path), target);
            None
        })
    }

    /// Calls `callback(bar)` when the current folder's segment is clicked.
    pub fn connect_edit_location<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("edit-location", false, move |values| {
            let bar = values[0].get::<PathBar>().expect("bar argument");
            callback(&bar);
            None
        })
    }

    /// Calls `callback(bar, path)` when Properties is chosen from a segment's menu.
    pub fn connect_properties_requested<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, &Path) + 'static,
    {
        self.connect_local("properties-requested", false, move |values| {
            let bar = values[0].get::<PathBar>().expect("bar argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&bar, Path::new(&path));
            None
        })
    }

    /// Calls `callback(bar, target, files)` when files are dropped on a segment.
    pub fn connect_files_dropped<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, &Path, Vec<PathBuf>) + 'static,
    {
        self.connect_local("files-dropped", false, move |values| {
            let bar = values[0].get::<PathBar>().expect("bar argument");
            let target = values[1].get::<String>().expect("target argument");
            let files = values[2].get::<glib::Variant>().expect("files argument");
            let files: Vec<String> = files.get().unwrap_or_default();
            callback(&bar, Path::new(&target), files.into_iter().map(PathBuf::from).collect());
            None
        })
    }

    fn emit_open(&self, path: &Path, target: OpenTarget) {
        let text = path.to_string_lossy().into_owned();
        self.emit_by_name::<()>("open-location", &[&text, &target]);
    }

    fn emit_files_dropped(&self, target: &Path, files: &[String]) {
        let text = target.to_string_lossy().into_owned();
        let files = files.to_vec().to_variant();
        self.emit_by_name::<()>("files-dropped", &[&text, &files]);
    }

    /// The right-click menu for a segment: open in a new window or tab, or its properties.
    fn show_context_menu(&self, button: &gtk::Button, path: &Path, x: f64, y: f64) {
        let text = path.to_string_lossy().into_owned();
        let menu = gio::Menu::new();
        for (label, action) in [
            ("Open in New _Window", "pathbar.open-item-new-window"),
            ("Open in New _Tab", "pathbar.open-item-new-tab"),
            ("_Properties", "pathbar.properties"),
        ] {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&text.to_variant()));
            menu.append_item(&item);
        }
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(self);
        popover.set_has_arrow(false);
        if let Some(point) = button.compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32)) {
            popover.set_pointing_to(Some(&gdk::Rectangle::new(point.x() as i32, point.y() as i32, 0, 0)));
        }
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    pub(crate) fn run_scenarios() {
        segments_start_at_root_or_home();
        set_path_builds_segments_and_emits_on_click();
    }

    fn segments_start_at_root_or_home() {
        let outside = segments(Path::new("/tmp/a/b")).unwrap();
        let kinds: Vec<Kind> = outside.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Kind::Root, Kind::Normal, Kind::Normal, Kind::Normal]);
        assert_eq!(outside.last().unwrap().path, Path::new("/tmp/a/b"));
        assert!(outside[1..].iter().all(|s| s.icon.is_none()), "normal segments have no icon");

        let home = glib::home_dir();
        let inside = segments(&home.join("projects")).unwrap();
        assert_eq!(inside[0].kind, Kind::Home);
        assert_eq!(inside[0].label, "Home");
        assert_eq!(inside.len(), 2, "the path starts at home, not at /");

        assert!(segments(Path::new("relative/path")).is_err());
    }

    fn set_path_builds_segments_and_emits_on_click() {
        let bar = PathBar::new();
        bar.set_path(Path::new("/tmp/a/b")).unwrap();
        assert_eq!(bar.path(), Some(PathBuf::from("/tmp/a/b")));
        assert_eq!(bar.imp().segments.observe_children().n_items(), 4);

        let clicked = Rc::new(RefCell::new(None));
        let sink = clicked.clone();
        bar.connect_open_location(move |_, path, target| *sink.borrow_mut() = Some((path.to_path_buf(), target)));
        let edits = Rc::new(Cell::new(0));
        let edit_count = edits.clone();
        bar.connect_edit_location(move |_| edit_count.set(edit_count.get() + 1));

        // The second segment is /tmp; clicking it asks to open that folder here.
        let second = bar.imp().segments.observe_children().item(1).and_downcast::<gtk::Box>().unwrap();
        let button = second.last_child().and_downcast::<gtk::Button>().unwrap();
        button.emit_clicked();
        assert_eq!(*clicked.borrow(), Some((PathBuf::from("/tmp"), OpenTarget::Here)));

        // The current folder asks to be edited instead of opened.
        *clicked.borrow_mut() = None;
        let last = bar.imp().segments.last_child().and_downcast::<gtk::Box>().unwrap();
        last.last_child().and_downcast::<gtk::Button>().unwrap().emit_clicked();
        assert_eq!(*clicked.borrow(), None);
        assert_eq!(edits.get(), 1);
    }
}
