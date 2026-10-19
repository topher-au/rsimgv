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

use crate::fs::thumbnail::{Media as ThumbnailMedia, Size as ThumbnailSize, Thumbnails};
use super::film_strip::FilmStrip;
use crate::fs::{natural_cmp, FileEntry, FileStore, Reading};

/// A file or folder in a browse view, as the IPC reports it.
pub struct ListedFile {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

pub const DEFAULT_ITEM_SIZE: i32 = 128;
/// How long the pointer rests on an item before its description shows.
const TOOLTIP_DELAY: std::time::Duration = std::time::Duration::from_millis(600);
/// How long each frame of a hovered video shows.
const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(350);

/// Pieces of information shown under an item's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoField {
    Size,
    Modified,
    ContentType,
    Resolution,
    Duration,
    Framerate,
}

impl InfoField {
    /// Every field, in the order the preferences list them.
    pub const ALL: [InfoField; 6] = [
        InfoField::Size,
        InfoField::Modified,
        InfoField::ContentType,
        InfoField::Resolution,
        InfoField::Duration,
        InfoField::Framerate,
    ];

    /// The name shown for a field in the preferences.
    pub fn title(self) -> &'static str {
        match self {
            InfoField::Size => "Size",
            InfoField::Modified => "Modified",
            InfoField::ContentType => "Type",
            InfoField::Resolution => "Resolution",
            InfoField::Duration => "Duration",
            InfoField::Framerate => "Frame rate",
        }
    }

    /// The name a field is saved under in the settings.
    pub fn key(self) -> &'static str {
        match self {
            InfoField::Size => "size",
            InfoField::Modified => "modified",
            InfoField::ContentType => "content-type",
            InfoField::Resolution => "resolution",
            InfoField::Duration => "duration",
            InfoField::Framerate => "framerate",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.key() == key)
    }

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
            // The size of an image, read from its header without decoding it.
            InfoField::Resolution if entry.is_dir() => None,
            InfoField::Resolution => gtk::gdk_pixbuf::Pixbuf::file_info(entry.path())
                .map(|(_, width, height)| format!("{width} × {height}")),
            // Video length and frame rate need a media reader, which the viewer will bring.
            InfoField::Duration | InfoField::Framerate => None,
        }
    }
}

/// The info for `entry`: its chosen fields that have a value, joined by `separator`.
fn info_text(entry: &FileEntry, fields: &[InfoField], separator: &str) -> String {
    fields
        .iter()
        .filter_map(|field| field.text(entry))
        .collect::<Vec<_>>()
        .join(separator)
}

/// The tooltip for an item, as markup: its full name in bold, then its type and, for a file, its size.
fn full_description(entry: &FileEntry) -> String {
    let mut lines = vec![format!("<b>{}</b>", glib::markup_escape_text(&entry.name()))];
    if let Some(kind) = InfoField::ContentType.text(entry) {
        lines.push(glib::markup_escape_text(&kind).to_string());
    }
    if let Some(size) = InfoField::Size.text(entry) {
        lines.push(glib::markup_escape_text(&size).to_string());
    }
    lines.join("\n")
}

/// The thumbnail `entry` gets: a picture or a video's still. SVG pictures keep their icon.
fn thumbnail_media(entry: &FileEntry) -> Option<ThumbnailMedia> {
    if entry.is_dir() {
        return None;
    }
    let kind = entry.info().content_type()?;
    if kind.starts_with("image/") && !kind.starts_with("image/svg") {
        Some(ThumbnailMedia::Picture)
    } else if kind.starts_with("video/") {
        Some(ThumbnailMedia::Video)
    } else {
        None
    }
}

/// When `entry` was last modified, in seconds since the epoch.
fn modified_secs(entry: &FileEntry) -> i64 {
    entry.info().modification_date_time().map_or(0, |time| time.to_unix())
}

/// The item a cell shows, if it shows one.
fn entry_in(view: &BrowseView, cell: &gtk::Box) -> Option<FileEntry> {
    let position = cell.widget_name().parse::<u32>().ok()?;
    view.imp().grid.model()?.item(position).and_downcast::<FileEntry>()
}

/// Whether `cell` currently shows the item at `path`.
fn shows(view: &BrowseView, cell: &gtk::Box, path: &Path) -> bool {
    entry_in(view, cell).is_some_and(|entry| entry.path() == path)
}

/// Whether a request for the item at `path` is still wanted: its cell still shows it.
fn wanted_for(view: &BrowseView, cell: &gtk::Box, path: &Path) -> impl Fn() -> bool + 'static {
    let (view, cell, path) = (view.downgrade(), cell.downgrade(), path.to_path_buf());
    move || match (view.upgrade(), cell.upgrade()) {
        (Some(view), Some(cell)) => shows(&view, &cell, &path),
        _ => false,
    }
}

/// The image in a cell's visual part.
fn icon_of(cell: &gtk::Box) -> Option<gtk::Image> {
    cell.first_child().and_then(|visual| visual.first_child()).and_downcast::<gtk::Image>()
}

/// Shows the still thumbnail of `entry` in `icon` once it is made, as long as the cell still shows `entry`.
fn request_thumbnail(
    view: &BrowseView,
    cell: &gtk::Box,
    entry: &FileEntry,
    icon: &gtk::Image,
    size: i32,
    media: ThumbnailMedia,
) {
    let path = entry.path().to_path_buf();
    let icon = icon.downgrade();
    // A video's still is drawn between film strips.
    let film = media == ThumbnailMedia::Video;
    Thumbnails::shared().request_still(
        &path,
        modified_secs(entry),
        media,
        ThumbnailSize::for_pixels(size),
        wanted_for(view, cell, &path),
        move |texture| {
            if let Some(icon) = icon.upgrade() {
                icon.set_pixel_size(size);
                if film {
                    icon.set_paintable(Some(&FilmStrip::new(texture)));
                } else {
                    icon.set_paintable(Some(texture));
                }
                if let Some(visual) = icon.parent() {
                    visual.add_css_class("has-thumbnail");
                }
            }
        },
    );
}

/// Whether the hover playback `generation` is the current one.
fn is_current_hover(hovered: &Option<Hover>, generation: u64) -> bool {
    hovered.as_ref().is_some_and(|hover| hover.generation == generation)
}

/// Whether `entry` is a picture or a video, judged by its content type.
fn is_media(entry: &FileEntry) -> bool {
    entry
        .info()
        .content_type()
        .is_some_and(|kind| kind.starts_with("image/") || kind.starts_with("video/"))
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties, gtk::CompositeTemplate)]
    #[properties(wrapper_type = super::BrowseView)]
    #[template(resource = "/au/topher/rimgv/ui/browse_view.ui")]
    pub struct BrowseView {
        #[template_child]
        pub grid: TemplateChild<super::super::ItemGrid>,
        #[template_child]
        pub floating_bar: TemplateChild<super::super::FloatingBar>,
        /// Whether a folder is loading. Its listing is stopped by the floating bar's stop button.
        pub(super) loading: Cell<bool>,
        /// What to do once the folder has listed, such as selecting its first picture or video.
        pub(super) pending: RefCell<Option<Box<dyn FnOnce(&super::BrowseView)>>>,
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) filter: OnceCell<gtk::CustomFilter>,
        pub(super) sorter: OnceCell<gtk::CustomSorter>,
        /// The selection the grid shows. It is taken off the grid while a folder loads.
        pub(super) selection: OnceCell<gtk::SingleSelection>,
        /// Show dot-prefixed files and folders.
        #[property(get, set)]
        show_hidden: Cell<bool>,
        /// Show only pictures and videos (and folders), not every file.
        #[property(get, set)]
        media_only: Cell<bool>,
        /// The read of the folder shown. Opening another folder, or stopping, cancels it.
        pub(super) reading: RefCell<Option<Reading>>,
        /// The video under the pointer, whose frames play while it is hovered.
        pub(super) hovered: RefCell<Option<Hover>>,
        pub(super) hover_generation: Cell<u64>,
        /// The popover that shows an item's description, below the item, and the timer that opens it.
        pub(super) tooltip: OnceCell<gtk::Popover>,
        pub(super) tooltip_timer: RefCell<Option<glib::SourceId>>,
        /// How far each folder visited in this view was scrolled, so returning to one restores it.
        pub(super) scroll_memory: RefCell<std::collections::HashMap<PathBuf, f64>>,
        pub(super) path: RefCell<Option<PathBuf>>,
        pub(super) info_fields: RefCell<Vec<InfoField>>,
        /// How the info is laid out under a name: `disabled` (hidden), `stacked` (one field a line), or
        /// `inline` (fields separated by a bullet).
        #[property(get, set)]
        info_layout: RefCell<String>,
        /// How item names are shown: `off`, `wrapped` (two lines) or `ellipsized` (one line).
        #[property(get, set)]
        filename_display: RefCell<String>,
        /// Where folders are listed: `none`, `first-last` (before the files), or `inline` among them.
        /// Width and height of the visual part of each item, in pixels.
        #[property(get, set)]
        item_size: Cell<i32>,
        #[property(get, set)]
        show_directories: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BrowseView {
        const NAME: &'static str = "RsimgvBrowseView";
        type Type = super::BrowseView;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            // The floating bar and the grid are built from the template, so their types must be registered first.
            super::super::FloatingBar::static_type();
            super::super::ItemGrid::static_type();
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
                    Signal::builder("item-context-requested")
                        .param_types([String::static_type(), bool::static_type(), f64::static_type(), f64::static_type()])
                        .build(),
                    Signal::builder("open-in-new-tab-requested")
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
            self.filename_display.replace("ellipsized".to_owned());
            self.info_layout.replace("inline".to_owned());
            self.show_directories.replace("first-last".to_owned());
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
                let visible_when_listed = !entry.is_dir() || view.show_directories() != "none";
                visible_when_hidden && visible_when_media && visible_when_listed
            });

            // Folders go before the files, or among them when show-directories is inline.
            let sorter = gtk::CustomSorter::new({
                let view = obj.downgrade();
                move |a, b| {
                    let (Some(a), Some(b), Some(view)) =
                        (a.downcast_ref::<FileEntry>(), b.downcast_ref::<FileEntry>(), view.upgrade())
                    else {
                        return gtk::Ordering::Equal;
                    };
                    let mode = view.show_directories();
                    let rank = |entry: &FileEntry| match (mode.as_str(), entry.is_dir()) {
                        ("inline", _) => 0,
                        (_, is_dir) => u8::from(!is_dir),
                    };
                    match rank(a).cmp(&rank(b)).then_with(|| natural_cmp(&a.name(), &b.name())) {
                        std::cmp::Ordering::Less => gtk::Ordering::Smaller,
                        std::cmp::Ordering::Equal => gtk::Ordering::Equal,
                        std::cmp::Ordering::Greater => gtk::Ordering::Larger,
                    }
                }
            });
            let sorted = gtk::SortListModel::new(Some(store.clone()), Some(sorter.clone()));
            let visible = gtk::FilterListModel::new(Some(sorted), Some(filter.clone()));
            let selection = gtk::SingleSelection::new(Some(visible));
            selection.set_autoselect(false);
            self.grid.set_model(Some(&selection));
            selection.connect_selection_changed({
                let view = obj.downgrade();
                move |_, _, _| {
                    if let Some(view) = view.upgrade() {
                        view.update_status();
                    }
                }
            });
            self.floating_bar.connect_stop({
                let view = obj.downgrade();
                move |_| {
                    if let Some(view) = view.upgrade() {
                        view.stop_loading();
                    }
                }
            });
            if let Some(overlay) = self.floating_bar.parent() {
                self.floating_bar.attach_hover_hide(&overlay);
            }
            let _ = self.selection.set(selection);
            let _ = self.store.set(store);
            let _ = self.filter.set(filter);
            let _ = self.sorter.set(sorter);

            self.grid.set_builders(
                || {
                    let builder = gtk::Builder::from_resource("/au/topher/rimgv/ui/browse_item.ui");
                    let cell: gtk::Box = builder.object("cell").expect("browse_item.ui defines `cell`");

                // Right-click on an item asks for its context menu.
                let click = gtk::GestureClick::new();
                click.set_button(gtk::gdk::BUTTON_SECONDARY);
                click.connect_pressed(|gesture, n_press, x, y| {
                    if n_press != 1 {
                        return;
                    }
                    let Some(cell) = gesture.widget().and_downcast::<gtk::Box>() else { return };
                    let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() else {
                        return;
                    };
                    let Ok(position) = cell.widget_name().parse::<u32>() else { return };
                    let Some(entry) = view.imp().grid.model().and_then(|model| model.item(position)).and_downcast::<FileEntry>() else {
                        return;
                    };
                    let point = cell
                        .compute_point(&view, &gtk::graphene::Point::new(x as f32, y as f32))
                        .map_or((x, y), |p| (f64::from(p.x()), f64::from(p.y())));
                    let path = entry.path().to_string_lossy().into_owned();
                    view.emit_by_name::<()>(
                        "item-context-requested",
                        &[&path, &entry.is_dir(), &point.0, &point.1],
                    );
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                });
                cell.add_controller(click);

                // Middle-click on a folder asks for it to be opened in a new tab.
                let middle = gtk::GestureClick::new();
                middle.set_button(gtk::gdk::BUTTON_MIDDLE);
                middle.connect_pressed(|gesture, _, _, _| {
                    let Some(cell) = gesture.widget().and_downcast::<gtk::Box>() else { return };
                    let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() else {
                        return;
                    };
                    let Ok(position) = cell.widget_name().parse::<u32>() else { return };
                    let Some(entry) = view.imp().grid.model().and_then(|model| model.item(position)).and_downcast::<FileEntry>() else {
                        return;
                    };
                    if entry.is_dir() {
                        let path = entry.path().to_string_lossy().into_owned();
                        view.emit_by_name::<()>("open-in-new-tab-requested", &[&path]);
                    }
                });
                cell.add_controller(middle);

                // Hovering a video plays its frames; leaving it shows the still thumbnail again.
                let motion = gtk::EventControllerMotion::new();
                motion.connect_enter(|controller, _, _| {
                    let Some(cell) = controller.widget().and_downcast::<gtk::Box>() else { return };
                    cell.add_css_class("hovered");
                    let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() else {
                        return;
                    };
                    view.hover_enter(&cell);
                    view.tooltip_enter(&cell);
                });
                motion.connect_leave(|controller| {
                    let Some(cell) = controller.widget().and_downcast::<gtk::Box>() else { return };
                    cell.remove_css_class("hovered");
                    let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() else {
                        return;
                    };
                    view.hover_leave(&cell);
                    view.tooltip_leave();
                });
                cell.add_controller(motion);
                    cell.upcast()
                },
                {
                    let view = obj.downgrade();
                    move |widget, entry, _| {
                        let (Some(cell), Some(view)) = (widget.downcast_ref::<gtk::Box>(), view.upgrade()) else {
                            return;
                        };
                        style_item(&view, cell, entry);
                    }
                },
            );

            // The description of the item under the pointer, in a popover below the item rather than a
            // tooltip, which GTK places at the pointer and so over the thumbnail.
            let label = gtk::Label::new(None);
            label.set_use_markup(true);
            label.set_can_target(false);
            let popover = gtk::Popover::new();
            popover.set_autohide(false);
            popover.set_has_arrow(false);
            popover.set_position(gtk::PositionType::Bottom);
            popover.set_can_target(false);
            popover.set_child(Some(&label));
            let _ = self.tooltip.set(popover);

            // Cursor movement is exposed as actions on the `browse` group, so keys and menus can share them.
            let actions = gtk::gio::SimpleActionGroup::new();
            for (name, move_cursor) in [
                ("cursor-x", crate::ui::ItemGrid::move_cursor_x as fn(&crate::ui::ItemGrid, i32)),
                ("cursor-y", crate::ui::ItemGrid::move_cursor_y),
                ("cursor-page", crate::ui::ItemGrid::move_cursor_page),
            ] {
                let action = gtk::gio::SimpleAction::new(name, Some(gtk::glib::VariantTy::INT32));
                let grid = self.grid.downgrade();
                action.connect_activate(move |_, parameter| {
                    let (Some(grid), Some(n)) = (grid.upgrade(), parameter.and_then(|value| value.get::<i32>())) else {
                        return;
                    };
                    move_cursor(&grid, n);
                });
                actions.add_action(&action);
            }
            obj.insert_action_group("browse", Some(&actions));

            let view = obj.downgrade();
            self.grid.connect_activate(move |_, position| {
                if let Some(view) = view.upgrade() {
                    view.activate(position);
                }
            });

            // Changing the size or the name mode rebinds every item, which re-lays them out.
            obj.connect_item_size_notify(|view| view.refresh_items());
            obj.connect_filename_display_notify(|view| view.refresh_items());
            obj.connect_info_layout_notify(|view| view.refresh_items());
            obj.connect_show_hidden_notify(|view| view.refresh_filter());
            obj.connect_media_only_notify(|view| view.refresh_filter());
            obj.connect_show_directories_notify(|view| {
                view.refresh_filter();
                if let Some(sorter) = view.imp().sorter.get() {
                    sorter.changed(gtk::SorterChange::Different);
                }
            });
        }
    }

    impl WidgetImpl for BrowseView {}
    impl BoxImpl for BrowseView {}
}

/// The video under the pointer. `generation` tells the frame playback whether it is still current.
struct Hover {
    cell: glib::WeakRef<gtk::Box>,
    path: PathBuf,
    generation: u64,
}

/// The item at the top of the view and how far it is above the view's top edge.
#[derive(Clone)]
struct TopItem {
    position: u32,
    offset: f64,
}

/// Finds the rendered item nearest the top edge of the grid's scroller, and where it sits.
fn top_item_anchor(grid: &super::ItemGrid) -> Option<TopItem> {
    let scroller = grid.ancestor(gtk::ScrolledWindow::static_type())?;
    let mut rendered = Vec::new();
    let mut child = grid.first_child();
    while let Some(cell) = child {
        child = cell.next_sibling();
        let Ok(position) = cell.widget_name().parse::<u32>() else { continue };
        let Some(bounds) = cell.compute_bounds(&scroller) else { continue };
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
fn anchored_item_y(grid: &super::ItemGrid, position: u32) -> Option<f64> {
    let scroller = grid.ancestor(gtk::ScrolledWindow::static_type())?;
    let mut child = grid.first_child();
    while let Some(cell) = child {
        child = cell.next_sibling();
        if cell.widget_name() == position.to_string() {
            return cell.compute_bounds(&scroller).map(|bounds| f64::from(bounds.y()));
        }
    }
    None
}

/// Scrolls the grid so the anchored item is back at its old offset. Layout can take a few frames
/// and the scroll may be clamped along the way, so the correction is re-applied for a while; once
/// the item is where it was, re-applying changes nothing.
fn restore_top_item_after_layout(grid: &super::ItemGrid, anchor: TopItem) {
    const FRAMES: u32 = 8;
    let grid = grid.clone();
    let frames = Cell::new(0u32);
    grid.clone().add_tick_callback(move |_, _| {
        frames.set(frames.get() + 1);
        if frames.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        if let (Some(scroller), Some(y)) = (grid.ancestor(gtk::ScrolledWindow::static_type()), anchored_item_y(&grid, anchor.position)) {
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
    visual.remove_css_class("has-thumbnail");
    if let Some(media) = thumbnail_media(entry) {
        request_thumbnail(view, cell, entry, &icon, size, media);
    }

    let display = view.filename_display();
    let wrap = display == "wrapped";
    name.set_label(&entry.name());
    name.set_visible(display != "off");
    name.set_wrap(wrap);
    name.set_lines(if wrap { 2 } else { 1 });
    name.set_ellipsize(if wrap {
        gtk::pango::EllipsizeMode::End
    } else {
        gtk::pango::EllipsizeMode::Middle
    });
    // Names and info are capped to about the item's width, so long text does not widen the item.
    let chars = (size / 8).max(4);
    name.set_max_width_chars(chars);
    info.set_max_width_chars(chars);

    let fields = view.imp().info_fields.borrow().clone();
    let layout = view.info_layout();
    let separator = if layout == "stacked" { "\n" } else { " · " };
    let text = if layout == "disabled" { String::new() } else { info_text(entry, &fields, separator) };
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

    /// Lists `path` in the background, showing its entries as they arrive. Opening another folder stops
    /// this listing.
    pub fn set_path(&self, path: &Path) {
        let imp = self.imp();
        let scroller = imp.grid.ancestor(gtk::ScrolledWindow::static_type()).and_downcast::<gtk::ScrolledWindow>();
        if let (Some(old), Some(scroller)) = (imp.path.borrow().clone(), &scroller) {
            imp.scroll_memory.borrow_mut().insert(old, scroller.vadjustment().value());
        }
        let saved = imp.scroll_memory.borrow().get(path).copied().unwrap_or(0.0);
        imp.path.replace(Some(path.to_path_buf()));
        imp.loading.set(true);
        imp.floating_bar.cancel_hover_hide();
        self.update_status();
        if let Some(store) = imp.store.get() {
            store.remove_all();
        }
        // The grid is detached while the folder loads, so it does not lay out each item as it arrives.
        imp.grid.set_model(None::<&gtk::SingleSelection>);
        if let Some(previous) = imp.reading.take() {
            previous.cancel();
        }

        // The folder is listed afresh, since it may have changed since the last visit.
        let files = FileStore::shared();
        files.discard_idle(path);
        let view = self.downgrade();
        let on_batch = {
            let view = view.clone();
            move |batch: &[FileEntry]| {
                let Some(view) = view.upgrade() else { return };
                if let Some(store) = view.imp().store.get() {
                    store.splice(store.n_items(), 0, batch);
                }
            }
        };
        let on_done = move |_: Result<(), String>| {
            let Some(view) = view.upgrade() else { return };
            view.imp().reading.take();
            view.imp().loading.set(false);
            view.attach_selection();
            view.update_status();
            let pending = view.imp().pending.take();
            if let Some(run) = pending {
                run(&view);
            }
            if let Some(scroller) = &scroller {
                restore_scroll_after_layout(scroller, saved);
            }
        };
        let reading = files.read(path, on_batch, on_done);
        imp.reading.replace(Some(reading));
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

    /// Moves the cursor to the first item.
    pub fn select_first(&self) {
        self.imp().grid.select_first();
    }

    /// Moves the cursor to the last item.
    pub fn select_last(&self) {
        self.imp().grid.select_last();
    }

    /// Opens the item under the cursor.
    pub fn activate_selected(&self) {
        self.imp().grid.activate_selected();
    }

    /// Moves the cursor `items` along the list: negative moves back.
    pub fn move_cursor_x(&self, items: i32) {
        self.imp().grid.move_cursor_x(items);
    }

    /// Moves the cursor `rows` rows down the grid: negative moves up.
    pub fn move_cursor_y(&self, rows: i32) {
        self.imp().grid.move_cursor_y(rows);
    }

    /// Moves the cursor `pages` pages down the grid: negative moves up.
    pub fn move_cursor_page(&self, pages: i32) {
        self.imp().grid.move_cursor_page(pages);
    }

    /// The items shown, in display order.
    pub fn entries(&self) -> Vec<ListedFile> {
        let Some(model) = self.imp().grid.model() else { return Vec::new() };
        (0..model.n_items())
            .filter_map(|position| model.item(position).and_downcast::<FileEntry>())
            .map(|entry| ListedFile { name: entry.name(), path: entry.path().to_path_buf(), is_dir: entry.is_dir() })
            .collect()
    }

    /// Selects the item at `index`, counting from the start, or from the end for a negative index. Returns the
    /// name of the item selected, or `None` if there is no item at that index.
    pub fn select_at(&self, index: i64) -> Option<String> {
        let model = self.imp().grid.model()?;
        let count = i64::from(model.n_items());
        let position = if index < 0 { count + index } else { index };
        if !(0..count).contains(&position) {
            return None;
        }
        let position = position as u32;
        self.imp().grid.select(position);
        model.item(position).and_downcast::<FileEntry>().map(|entry| entry.name())
    }

    /// Arranges for `run` to happen once the folder being shown has listed. A later listing replaces it.
    pub fn set_pending(&self, run: impl FnOnce(&BrowseView) + 'static) {
        self.imp().pending.replace(Some(Box::new(run)));
    }

    /// Drops whatever was arranged with `set_pending`.
    pub fn clear_pending(&self) {
        self.imp().pending.take();
    }

    /// Selects the first picture or video in the folder, or the last one if `last` is set. Returns its name.
    pub fn select_media(&self, last: bool) -> Option<String> {
        let model = self.imp().grid.model()?;
        let is_media_entry = |position: u32| {
            model.item(position).and_downcast::<FileEntry>().is_some_and(|entry| !entry.is_dir() && is_media(&entry))
        };
        let positions: Vec<u32> = (0..model.n_items()).filter(|&position| is_media_entry(position)).collect();
        let position = if last { positions.last() } else { positions.first() }.copied()?;
        self.imp().grid.select(position);
        model.item(position).and_downcast::<FileEntry>().map(|entry| entry.name())
    }

    /// The position of the selected item, if one is selected.
    pub fn selected_position(&self) -> Option<u32> {
        let model = self.imp().grid.model()?;
        let position = model.selected();
        (position < model.n_items()).then_some(position)
    }

    /// The name of the selected item, if one is selected.
    pub fn selected_name(&self) -> Option<String> {
        let model = self.imp().grid.model()?;
        model.item(model.selected()).and_downcast::<FileEntry>().map(|entry| entry.name())
    }

    /// Selects the item whose name or path is `file`, and returns the name of the item selected.
    pub fn select_named(&self, file: &str) -> Option<String> {
        let model = self.imp().grid.model()?;
        let position = (0..model.n_items()).find(|&position| {
            model
                .item(position)
                .and_downcast::<FileEntry>()
                .is_some_and(|entry| entry.name() == file || entry.path().to_string_lossy() == file)
        })?;
        self.imp().grid.select(position);
        model.item(position).and_downcast::<FileEntry>().map(|entry| entry.name())
    }

    /// The pictures and videos shown, in display order: the items the viewer steps through.
    pub fn media_paths(&self) -> Vec<PathBuf> {
        let Some(model) = self.imp().grid.model() else {
            return Vec::new();
        };
        (0..model.n_items())
            .filter_map(|position| model.item(position).and_downcast::<FileEntry>())
            .filter(|entry| is_media(entry))
            .map(|entry| entry.path().to_path_buf())
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

    /// Calls `callback(view, path, is_dir, x, y)` when an item is right-clicked. The pointer is in the view's
    /// coordinates.
    /// Calls `callback(view, path)` when a folder is middle-clicked, with its absolute path.
    pub fn connect_open_in_new_tab_requested<F: Fn(&Self, &Path) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("open-in-new-tab-requested", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&view, Path::new(&path));
            None
        })
    }

    pub fn connect_item_context_requested<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, &Path, bool, f64, f64) + 'static,
    {
        self.connect_local("item-context-requested", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let path = values[1].get::<String>().expect("path argument");
            let is_dir = values[2].get::<bool>().expect("is_dir argument");
            let x = values[3].get::<f64>().expect("x argument");
            let y = values[4].get::<f64>().expect("y argument");
            callback(&view, Path::new(&path), is_dir, x, y);
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
        self.update_status();
    }

    /// Stops loading the folder: the entries read so far are shown.
    fn stop_loading(&self) {
        let imp = self.imp();
        if let Some(reading) = imp.reading.take() {
            reading.cancel();
        }
        imp.loading.set(false);
        self.attach_selection();
        self.update_status();
    }

    /// Shows the selection's items in the grid again.
    fn attach_selection(&self) {
        let imp = self.imp();
        if let Some(selection) = imp.selection.get() {
            imp.grid.set_model(Some(selection));
        }
    }

    /// Updates the floating bar like Nautilus's: while a folder loads, a spinner and a stop button;
    /// with an item selected, what it is and its size; otherwise the bar is hidden.
    fn update_status(&self) {
        let imp = self.imp();
        if imp.loading.get() {
            imp.floating_bar.set_labels(Some("Loading…"), None);
            imp.floating_bar.set_loading(true);
            imp.floating_bar.set_visible(true);
            return;
        }
        imp.floating_bar.set_loading(false);
        let selected = imp.selection.get().and_then(|selection| selection.selected_item());
        let Some(entry) = selected.and_downcast::<FileEntry>() else {
            imp.floating_bar.set_visible(false);
            return;
        };
        if entry.is_dir() {
            let primary = format!("“{}” selected", entry.name());
            imp.floating_bar.set_labels(Some(&primary), None);
        } else {
            let size = glib::format_size(entry.info().size() as u64);
            imp.floating_bar.set_labels(Some("1 item selected"), Some(&format!("({size})")));
        }
        imp.floating_bar.set_visible(true);
    }

    /// The height of one item: the visual, then the name and info lines as the settings lay them out.
    fn cell_height(&self) -> i32 {
        let builder = gtk::Builder::from_resource("/au/topher/rimgv/ui/browse_item.ui");
        let (Some(cell), Some(visual), Some(name), Some(info)) = (
            builder.object::<gtk::Box>("cell"),
            builder.object::<gtk::Box>("visual"),
            builder.object::<gtk::Label>("name"),
            builder.object::<gtk::Label>("info"),
        ) else {
            return self.item_size();
        };
        // The visual is the item's square, as style_item sizes it.
        visual.set_size_request(self.item_size(), self.item_size());
        cell.set_size_request(self.item_size(), -1);
        // Two lines of text are measured when names wrap and when info is stacked, so the rows fit them.
        let name_lines = if self.filename_display() == "wrapped" { "Ag\nAg" } else { "Ag" };
        name.set_label(name_lines);
        name.set_visible(self.filename_display() != "off");
        let info_lines = if self.info_layout() == "stacked" { "Ag\nAg" } else { "Ag" };
        info.set_label(info_lines);
        info.set_visible(self.info_layout() != "disabled");
        cell.measure(gtk::Orientation::Vertical, self.item_size()).1
    }

    /// Opens the description of the item in `cell` below it, once the pointer has rested there.
    fn tooltip_enter(&self, cell: &gtk::Box) {
        self.tooltip_leave();
        let view = self.downgrade();
        let cell = cell.downgrade();
        let timer = glib::timeout_add_local_once(TOOLTIP_DELAY, move || {
            if let (Some(view), Some(cell)) = (view.upgrade(), cell.upgrade()) {
                view.tooltip_show(&cell);
            }
        });
        *self.imp().tooltip_timer.borrow_mut() = Some(timer);
    }

    /// Closes the description, and cancels it if it has not opened yet.
    fn tooltip_leave(&self) {
        if let Some(timer) = self.imp().tooltip_timer.take() {
            timer.remove();
        }
        if let Some(popover) = self.imp().tooltip.get() {
            popover.popdown();
        }
    }

    fn tooltip_show(&self, cell: &gtk::Box) {
        // The timer has fired, so there is nothing left to cancel.
        self.imp().tooltip_timer.take();
        let (Some(popover), Some(entry)) = (self.imp().tooltip.get(), entry_in(self, cell)) else { return };
        if let Some(label) = popover.child().and_downcast::<gtk::Label>() {
            label.set_markup(&full_description(&entry));
        }
        if popover.parent().is_some() {
            popover.unparent();
        }
        popover.set_parent(cell);
        // Anchored to the thumbnail's container, so the popover opens directly below it.
        if let Some(bounds) = cell.first_child().and_then(|visual| visual.compute_bounds(cell)) {
            let thumbnail = gtk::gdk::Rectangle::new(
                bounds.x() as i32,
                bounds.y() as i32,
                bounds.width() as i32,
                bounds.height() as i32,
            );
            popover.set_pointing_to(Some(&thumbnail));
        }
        popover.popup();
    }

    /// Starts playing the frames of the video in `cell`, once they are made.
    fn hover_enter(&self, cell: &gtk::Box) {
        self.stop_hover();
        let Some(entry) = entry_in(self, cell) else { return };
        if thumbnail_media(&entry) != Some(ThumbnailMedia::Video) {
            return;
        }
        let imp = self.imp();
        let generation = imp.hover_generation.get() + 1;
        imp.hover_generation.set(generation);
        let path = entry.path().to_path_buf();
        imp.hovered.replace(Some(Hover {
            cell: cell.downgrade(),
            path: path.clone(),
            generation,
        }));
        let view = self.downgrade();
        let weak_cell = cell.downgrade();
        Thumbnails::shared().request_frames(
            &path,
            modified_secs(&entry),
            ThumbnailSize::for_pixels(self.item_size()),
            wanted_for(self, cell, &path),
            move |frames| {
                let (Some(view), Some(cell)) = (view.upgrade(), weak_cell.upgrade()) else { return };
                view.start_playing(&cell, generation, frames.to_vec());
            },
        );
    }

    /// Plays `frames` in `cell`, one every `FRAME_INTERVAL`, until the hover ends or the cell shows
    /// another item.
    fn start_playing(&self, cell: &gtk::Box, generation: u64, frames: Vec<gtk::gdk::Texture>) {
        if !is_current_hover(&self.imp().hovered.borrow(), generation) {
            return;
        }
        // Each frame is drawn between film strips, as a video's still is.
        let frames: Vec<FilmStrip> = frames.iter().map(FilmStrip::new).collect();
        let view = self.downgrade();
        let weak_cell = cell.downgrade();
        let mut index = 0;
        glib::timeout_add_local(FRAME_INTERVAL, move || {
            let (Some(view), Some(cell)) = (view.upgrade(), weak_cell.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            let path = {
                let hovered = view.imp().hovered.borrow();
                match hovered.as_ref().filter(|hover| hover.generation == generation) {
                    Some(hover) => hover.path.clone(),
                    None => return glib::ControlFlow::Break,
                }
            };
            if !shows(&view, &cell, &path) {
                return glib::ControlFlow::Break;
            }
            index = (index + 1) % frames.len();
            if let Some(icon) = icon_of(&cell) {
                icon.set_paintable(Some(&frames[index]));
            }
            glib::ControlFlow::Continue
        });
    }

    /// Stops the hover playback in `cell` when the pointer leaves it.
    fn hover_leave(&self, cell: &gtk::Box) {
        let hovered_here = self
            .imp()
            .hovered
            .borrow()
            .as_ref()
            .is_some_and(|hover| hover.cell.upgrade().as_ref() == Some(cell));
        if hovered_here {
            self.stop_hover();
        }
    }

    /// Ends the hover playback, if any, and shows the still thumbnail of its video again.
    fn stop_hover(&self) {
        let Some(hover) = self.imp().hovered.take() else { return };
        let Some(cell) = hover.cell.upgrade() else { return };
        let (Some(entry), Some(icon)) = (entry_in(self, &cell), icon_of(&cell)) else { return };
        if let Some(media) = thumbnail_media(&entry) {
            request_thumbnail(self, &cell, &entry, &icon, self.item_size(), media);
        }
    }

    /// Rebinds every item so a size or layout change shows, keeping the item at the top of the view
    /// where it was.
    fn refresh_items(&self) {
        let imp = self.imp();
        let anchor = top_item_anchor(&imp.grid);
        imp.grid.set_cell_size(self.item_size(), self.cell_height());
        imp.grid.refresh();
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

    /// Runs the main context for about `frames` frames, so layout and item binding happen.
    fn pump_frames(frames: u32) {
        let ctx = glib::MainContext::default();
        for _ in 0..frames {
            while ctx.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }

    /// The visual box (the icon's frame) of every rendered item.
    fn rendered_visuals(view: &BrowseView) -> Vec<gtk::Box> {
        let mut visuals = Vec::new();
        let mut child = view.imp().grid.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Some(visual) = widget
                .first_child()
                .and_downcast::<gtk::Box>()
            {
                visuals.push(visual);
            }
        }
        visuals
    }

    /// Changing the size updates items that are already on screen, not just the next ones bound.
    fn item_size_change_reaches_rendered_items() {
        let root = std::env::temp_dir().join(format!("rsimgv-browse-size-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("one")).unwrap();
        fs::create_dir_all(root.join("two")).unwrap();
        fs::write(root.join("three.mp4"), "x").unwrap();

        let view = BrowseView::new();
        let window = gtk::Window::new();
        window.set_default_size(600, 400);
        window.set_child(Some(&view));
        window.present();
        view.set_path(&root);
        wait_for("listing", || view.item_count() == 3);
        pump_frames(60);

        view.set_item_size(128);
        pump_frames(60);
        let visuals = rendered_visuals(&view);
        assert!(!visuals.is_empty(), "items are rendered");
        assert!(visuals.iter().all(|v| v.size_request() == (128, 128)), "128px before the change");

        view.set_item_size(64);
        pump_frames(60);
        let visuals = rendered_visuals(&view);
        assert!(visuals.iter().all(|v| v.size_request() == (64, 64)), "64px after the change");

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    pub(crate) fn run_scenarios() {
        item_size_change_reaches_rendered_items();
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
        view.set_filename_display("wrapped".to_owned());
        view.set_info_fields(&[InfoField::Size]);
        assert_eq!(view.item_count(), 3);

        let _ = fs::remove_dir_all(&root);
    }
}

/// Scrolls to `value` once the newly listed items have been laid out, over a few frames because the
/// grid's height grows as it measures them.
fn restore_scroll_after_layout(scroller: &gtk::ScrolledWindow, value: f64) {
    const FRAMES: u32 = 8;
    let frames = Cell::new(0u32);
    scroller.add_tick_callback(move |scroller, _| {
        frames.set(frames.get() + 1);
        scroller.vadjustment().set_value(value);
        if frames.get() < FRAMES {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
}
