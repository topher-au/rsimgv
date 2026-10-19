//! The main browse view: a grid of the folder's files and folders.
//!
//! Each item is a visual part (the icon, later a thumbnail) in a box of exactly `item-size` pixels,
//! with the name below it (ellipsized, or wrapped to two lines with `wrap-names`) and an info line
//! below that, built from the chosen `InfoField`s. Activating a folder asks to open it; activating a
//! file asks to open it in the viewer.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use gtk::glib::prelude::StaticType;
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::fs::{natural_cmp, FileEntry};

const QUERY_ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,standard::size,\
standard::icon,standard::content-type,standard::is-hidden,time::modified";
pub const DEFAULT_ITEM_SIZE: i32 = 128;

/// Pieces of information shown under an item's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoField {
    Size,
    Modified,
    ContentType,
}

impl InfoField {
    fn text(self, entry: &FileEntry) -> Option<String> {
        let info = entry.info();
        match self {
            InfoField::Size if entry.is_dir() => None,
            InfoField::Size => Some(glib::format_size(info.size() as u64).to_string()),
            InfoField::Modified => info
                .modification_date_time()
                .and_then(|time| time.format("%Y-%m-%d %H:%M").ok())
                .map(|text| text.to_string()),
            InfoField::ContentType => info
                .content_type()
                .map(|kind| gio::content_type_get_description(&kind).to_string()),
        }
    }
}

/// The info line for `entry`: its chosen fields that have a value, joined.
fn info_text(entry: &FileEntry, fields: &[InfoField]) -> String {
    fields
        .iter()
        .filter_map(|field| field.text(entry))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Whether `entry` is a picture or a video, judged by its content type.
fn is_media(entry: &FileEntry) -> bool {
    entry
        .info()
        .content_type()
        .is_some_and(|kind| kind.starts_with("image/") || kind.starts_with("video/"))
}

/// Lists a folder, folders first and then by natural name order.
async fn list_directory(dir: PathBuf) -> Result<Vec<FileEntry>, glib::Error> {
    let file = gio::File::for_path(&dir);
    let enumerator = file
        .enumerate_children_future(QUERY_ATTRIBUTES, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
        .await?;
    let mut infos = Vec::new();
    loop {
        let batch = enumerator.next_files_future(256, glib::Priority::DEFAULT).await?;
        if batch.is_empty() {
            break;
        }
        infos.extend(batch);
    }
    infos.sort_by(|a, b| {
        let a_dir = a.file_type() == gio::FileType::Directory;
        let b_dir = b.file_type() == gio::FileType::Directory;
        b_dir.cmp(&a_dir).then_with(|| natural_cmp(&a.display_name(), &b.display_name()))
    });
    Ok(infos
        .into_iter()
        .map(|info| FileEntry::new(dir.join(info.name()), info))
        .collect())
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties, gtk::CompositeTemplate)]
    #[properties(wrapper_type = super::BrowseView)]
    #[template(resource = "/au/topher/rimgv/ui/browse_view.ui")]
    pub struct BrowseView {
        #[template_child]
        pub grid: TemplateChild<gtk::GridView>,
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) filter: OnceCell<gtk::CustomFilter>,
        /// Show dot-prefixed files and folders.
        #[property(get, set)]
        show_hidden: Cell<bool>,
        /// Show only pictures and videos (and folders), not every file.
        #[property(get, set)]
        media_only: Cell<bool>,
        /// Bumped on every new folder, so a listing that finishes late is dropped.
        pub(super) generation: Cell<u64>,
        pub(super) path: RefCell<Option<PathBuf>>,
        pub(super) info_fields: RefCell<Vec<InfoField>>,
        /// Width and height of the visual part of each item, in pixels.
        #[property(get, set)]
        item_size: Cell<i32>,
        /// Show names wrapped to two lines instead of ellipsized on one.
        #[property(get, set)]
        wrap_names: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BrowseView {
        const NAME: &'static str = "RsimgvBrowseView";
        type Type = super::BrowseView;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for BrowseView {
        fn signals() -> &'static [Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("directory-activated")
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder("file-activated")
                        .param_types([String::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            self.item_size.set(DEFAULT_ITEM_SIZE);
            self.media_only.set(true);
            self.info_fields
                .replace(vec![InfoField::Size, InfoField::Modified]);

            let store = gio::ListStore::new::<FileEntry>();
            // The filter reads the view's properties, so it always reflects the current settings.
            let view = obj.downgrade();
            let filter = gtk::CustomFilter::new(move |item| {
                let Some(entry) = item.downcast_ref::<FileEntry>() else {
                    return true;
                };
                let Some(view) = view.upgrade() else {
                    return true;
                };
                let visible_when_hidden = view.show_hidden() || !entry.name().starts_with('.');
                let visible_when_media = !view.media_only() || entry.is_dir() || is_media(entry);
                visible_when_hidden && visible_when_media
            });
            let visible = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
            let selection = gtk::SingleSelection::new(Some(visible));
            selection.set_autoselect(false);
            self.grid.set_model(Some(&selection));
            let _ = self.store.set(store);
            let _ = self.filter.set(filter);

            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(|_, obj| {
                let item = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
                let builder = gtk::Builder::from_resource("/au/topher/rimgv/ui/browse_item.ui");
                let cell: gtk::Box = builder.object("cell").expect("browse_item.ui defines `cell`");
                item.set_child(Some(&cell));
            });
            let view = obj.downgrade();
            factory.connect_bind(move |_, obj| {
                let item = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
                let (Some(entry), Some(cell), Some(view)) = (
                    item.item().and_downcast::<FileEntry>(),
                    item.child().and_downcast::<gtk::Box>(),
                    view.upgrade(),
                ) else {
                    return;
                };
                // The cell remembers its item's position, so a scroll anchor can find it again.
                cell.set_widget_name(&item.position().to_string());
                style_item(&view, &cell, &entry);
            });
            self.grid.set_factory(Some(&factory));

            let view = obj.downgrade();
            self.grid.connect_activate(move |_, position| {
                if let Some(view) = view.upgrade() {
                    view.activate(position);
                }
            });

            // Changing the size or the name mode rebinds every item, which re-lays them out.
            obj.connect_item_size_notify(|view| view.refresh_items());
            obj.connect_wrap_names_notify(|view| view.refresh_items());
            obj.connect_show_hidden_notify(|view| view.refresh_filter());
            obj.connect_media_only_notify(|view| view.refresh_filter());
        }
    }

    impl WidgetImpl for BrowseView {}
    impl BoxImpl for BrowseView {}
}

/// The item at the top of the view and how far it is above the view's top edge.
#[derive(Clone)]
struct TopItem {
    position: u32,
    offset: f64,
}

/// Finds the rendered item nearest the top edge of the grid's scroller, and where it sits.
fn top_item_anchor(grid: &gtk::GridView) -> Option<TopItem> {
    let scroller = grid.parent()?;
    let mut rendered = Vec::new();
    let mut child = grid.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Some(cell) = widget.first_child() else { continue };
        let Ok(position) = cell.widget_name().parse::<u32>() else { continue };
        let Some(bounds) = widget.compute_bounds(&scroller) else { continue };
        rendered.push((position, f64::from(bounds.y())));
    }
    // Prefer the last item at or above the top edge; otherwise the first one below it.
    let (position, y) = rendered
        .iter()
        .filter(|(_, y)| *y <= 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .or_else(|| rendered.iter().min_by(|a, b| a.1.total_cmp(&b.1)))?;
    Some(TopItem {
        position: *position,
        offset: *y,
    })
}

/// Current position of the anchored item within the grid's scroller, if it is rendered.
fn anchored_item_y(grid: &gtk::GridView, position: u32) -> Option<f64> {
    let scroller = grid.parent()?;
    let mut child = grid.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Some(cell) = widget.first_child() else { continue };
        if cell.widget_name() == position.to_string() {
            return widget.compute_bounds(&scroller).map(|bounds| f64::from(bounds.y()));
        }
    }
    None
}

/// Scrolls the grid so the anchored item is back at its old offset. Layout can take a few frames
/// and the scroll may be clamped along the way, so the correction is re-applied for a while; once
/// the item is where it was, re-applying changes nothing.
fn restore_top_item_after_layout(grid: &gtk::GridView, anchor: TopItem) {
    const FRAMES: u32 = 8;
    let grid = grid.clone();
    let frames = Cell::new(0u32);
    grid.clone().add_tick_callback(move |_, _| {
        frames.set(frames.get() + 1);
        if frames.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        if let (Some(scroller), Some(y)) = (grid.parent(), anchored_item_y(&grid, anchor.position)) {
            if let Some(scroller) = scroller.downcast_ref::<gtk::ScrolledWindow>() {
                let adjustment = scroller.vadjustment();
                adjustment.set_value(adjustment.value() + (y - anchor.offset));
            }
        }
        if frames.get() < FRAMES {
            return glib::ControlFlow::Continue;
        }
        glib::ControlFlow::Break
    });
}

/// Lays out one item: a visual box of exactly the item size, the name, and the info line.
fn style_item(view: &BrowseView, cell: &gtk::Box, entry: &FileEntry) {
    let size = view.item_size();
    let Some(visual) = cell.first_child().and_downcast::<gtk::Box>() else { return };
    let Some(name) = visual.next_sibling().and_downcast::<gtk::Label>() else { return };
    let Some(info) = name.next_sibling().and_downcast::<gtk::Label>() else { return };
    let Some(icon) = visual.first_child().and_downcast::<gtk::Image>() else { return };

    visual.set_size_request(size, size);
    cell.set_size_request(size, -1);
    icon.set_pixel_size((size * 3 / 5).max(16));
    match entry.info().icon() {
        Some(gicon) => icon.set_from_gicon(&gicon),
        None => icon.set_icon_name(Some("text-x-generic-symbolic")),
    }

    let wrap = view.wrap_names();
    name.set_label(&entry.name());
    name.set_wrap(wrap);
    name.set_lines(if wrap { 2 } else { 1 });
    name.set_ellipsize(if wrap {
        gtk::pango::EllipsizeMode::End
    } else {
        gtk::pango::EllipsizeMode::Middle
    });
    name.set_tooltip_text(Some(&entry.name()));

    let fields = view.imp().info_fields.borrow().clone();
    let text = info_text(entry, &fields);
    info.set_label(&text);
    info.set_visible(!text.is_empty());
}

glib::wrapper! {
    pub struct BrowseView(ObjectSubclass<imp::BrowseView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl BrowseView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// The folder being listed, if any.
    pub fn path(&self) -> Option<PathBuf> {
        self.imp().path.borrow().clone()
    }

    /// Lists `path` in the background. A listing that is overtaken by another is dropped.
    pub fn set_path(&self, path: &Path) {
        let imp = self.imp();
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        imp.path.replace(Some(path.to_path_buf()));
        if let Some(store) = imp.store.get() {
            store.remove_all();
        }

        let view = self.downgrade();
        let dir = path.to_path_buf();
        glib::spawn_future_local(async move {
            let Ok(entries) = list_directory(dir).await else {
                return;
            };
            let Some(view) = view.upgrade() else { return };
            if view.imp().generation.get() != generation {
                return;
            }
            if let Some(store) = view.imp().store.get() {
                store.splice(0, 0, &entries);
            }
        });
    }

    pub fn set_info_fields(&self, fields: &[InfoField]) {
        self.imp().info_fields.replace(fields.to_vec());
        self.refresh_items();
    }

    /// The number of items shown, after the hidden filter.
    pub fn item_count(&self) -> u32 {
        self.imp().grid.model().map_or(0, |model| model.n_items())
    }

    /// The names of the items shown, in display order.
    pub fn item_names(&self) -> Vec<String> {
        let Some(model) = self.imp().grid.model() else {
            return Vec::new();
        };
        (0..model.n_items())
            .filter_map(|position| model.item(position).and_downcast::<FileEntry>())
            .map(|entry| entry.name())
            .collect()
    }

    /// Opens the item at `position`: a folder asks to open it, a file asks to open it in the viewer.
    pub fn activate(&self, position: u32) {
        let Some(entry) = self
            .imp()
            .grid
            .model()
            .and_then(|model| model.item(position))
            .and_downcast::<FileEntry>()
        else {
            return;
        };
        let path = entry.path().to_string_lossy().into_owned();
        let signal = if entry.is_dir() { "directory-activated" } else { "file-activated" };
        self.emit_by_name::<()>(signal, &[&path]);
    }

    /// Calls `callback(view, path)` when a folder is activated.
    pub fn connect_directory_activated<F: Fn(&Self, &Path) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("directory-activated", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&view, Path::new(&path));
            None
        })
    }

    /// Calls `callback(view, path)` when a file is activated.
    pub fn connect_file_activated<F: Fn(&Self, &Path) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("file-activated", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&view, Path::new(&path));
            None
        })
    }

    /// Re-applies the hidden and media filter after either changes.
    fn refresh_filter(&self) {
        if let Some(filter) = self.imp().filter.get() {
            filter.changed(gtk::FilterChange::Different);
        }
    }

    /// Rebinds every item so a size or layout change shows, keeping the item at the top of the view
    /// where it was.
    fn refresh_items(&self) {
        let imp = self.imp();
        let anchor = top_item_anchor(&imp.grid);
        if let Some(store) = imp.store.get() {
            let count = store.n_items();
            store.items_changed(0, count, count);
        }
        if let Some(anchor) = anchor {
            restore_top_item_after_layout(&imp.grid, anchor);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::rc::Rc;

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

    pub(crate) fn run_scenarios() {
        let root = std::env::temp_dir().join(format!("rsimgv-browse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("a.txt"), "hello").unwrap();
        fs::write(root.join("b.png"), "not really a picture").unwrap();
        fs::write(root.join("a.mp4"), "not really a video").unwrap();

        let view = BrowseView::new();
        assert_eq!(view.item_size(), DEFAULT_ITEM_SIZE);
        view.set_path(&root);
        wait_for("listing", || view.item_count() == 3);
        // Media only is the default, so the text file is left out.
        assert!(view.media_only());
        assert_eq!(view.item_names(), ["sub", "a.mp4", "b.png"]);

        view.set_media_only(false);
        assert_eq!(view.item_names(), ["sub", "a.mp4", "a.txt", "b.png"], "folders first, then natural order");
        view.set_show_hidden(true);
        assert_eq!(view.item_count(), 5);
        view.set_show_hidden(false);
        assert_eq!(view.item_count(), 4);
        view.set_media_only(true);
        assert_eq!(view.item_names(), ["sub", "a.mp4", "b.png"]);

        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        view.connect_directory_activated(move |_, path| sink.borrow_mut().push(path.to_path_buf()));
        view.activate(0);
        assert_eq!(*opened.borrow(), vec![root.join("sub")]);

        // Resizing and re-laying out the items does not lose them.
        view.set_item_size(64);
        view.set_wrap_names(true);
        view.set_info_fields(&[InfoField::Size]);
        assert_eq!(view.item_count(), 3);

        let _ = fs::remove_dir_all(&root);
    }
}
