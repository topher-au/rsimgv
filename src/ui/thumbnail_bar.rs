//! The viewer's thumbnail bar: a scrolling row or column of the thumbnails of the items the viewer
//! steps through, with the one shown selected. Clicking a thumbnail shows its item. Only the
//! thumbnails in view are made, as the list view only binds the rows it shows. Dragging the bar asks for
//! it to move to the side of the viewer it is dropped nearest; dragging its inner edge, the one facing the
//! picture, makes the thumbnails bigger or smaller. A small header at the start of the bar has the position in
//! its centre, the toggles to pin the bar shown and to float it over the picture nearest the window's edge, and a
//! button to close the bar at the other end.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::glib;

use super::film_strip::FilmStrip;
use super::media_viewer::is_video;
use super::rotated_label::RotatedLabel;
use crate::fs::thumbnail::{self, Media as ThumbnailMedia, Size as ThumbnailSize, Thumbnails};

/// The size of a thumbnail in the bar to start with, and the range it is kept within, in pixels.
const THUMBNAIL_PIXELS: i32 = 80;
const THUMBNAIL_MIN: i32 = 48;
const THUMBNAIL_MAX: i32 = 256;

/// How deep, in pixels, the band along the bar's inner edge is where a drag resizes the bar.
const RESIZE_EDGE: f64 = 8.0;

/// How far, in pixels, the pointer must move before a press on the bar becomes a drag rather than a click.
const DRAG_THRESHOLD: f64 = 24.0;

/// The URI of `path`, which unlike its UTF-8 text keeps every byte of a file name. Paths shown here are absolute,
/// so the conversion does not fail.
fn uri_of(path: &Path) -> String {
    glib::filename_to_uri(path, None).map(|uri| uri.to_string()).unwrap_or_default()
}

/// The path of the item `object` holds, from the URI it was made with.
fn path_of(object: &gtk::StringObject) -> Option<PathBuf> {
    glib::filename_from_uri(object.string().as_str()).ok().map(|(path, _)| path)
}

mod imp {
    use super::*;
    use std::sync::LazyLock;

    pub struct ThumbnailBar {
        pub(super) paths: gtk::StringList,
        pub(super) selection: gtk::SingleSelection,
        pub(super) list: gtk::ListView,
        pub(super) scroller: gtk::ScrolledWindow,
        /// Lays the header and the thumbnails out along the bar.
        pub(super) content: gtk::Box,
        pub(super) header: gtk::CenterBox,
        /// The pin and overlay toggles, and the button that closes the bar. The toggles are in the header's slot nearest
        /// the window's edge and the close button in the other.
        pub(super) toggles: gtk::Box,
        pub(super) close_button: gtk::Button,
        pub(super) pin_button: gtk::ToggleButton,
        pub(super) overlay_button: gtk::ToggleButton,
        /// Which item is shown out of how many, as `3 / 120`.
        pub(super) counter: RotatedLabel,
        /// Whether the bar is being dragged to another side.
        pub(super) dragging: Cell<bool>,
        /// The thumbnail size the drag of the inner edge started from, while one is under way.
        pub(super) resizing: Cell<Option<i32>>,
        /// The side of the viewer the bar is on, so which of its edges faces the picture.
        pub(super) side: Cell<gtk::PositionType>,
        /// The thumbnail size, shared with the factory, and the images it made, to resize them.
        pub(super) size: Rc<Cell<i32>>,
        pub(super) images: Rc<RefCell<Vec<glib::WeakRef<gtk::Image>>>>,
        /// The stamp of each path shown, taken when the paths were set, so binding a row does not read the file.
        pub(super) stamps: Rc<RefCell<HashMap<PathBuf, i64>>>,
    }

    impl Default for ThumbnailBar {
        fn default() -> Self {
            let paths = gtk::StringList::new(&[]);
            let selection = gtk::SingleSelection::new(Some(paths.clone()));
            selection.set_autoselect(false);
            selection.set_can_unselect(true);
            let size = Rc::new(Cell::new(THUMBNAIL_PIXELS));
            let images = Rc::new(RefCell::new(Vec::new()));
            let stamps = Rc::new(RefCell::new(HashMap::new()));
            let list = gtk::ListView::new(Some(selection.clone()), Some(factory(size.clone(), images.clone(), stamps.clone())));
            list.set_single_click_activate(true);
            list.add_css_class("thumbnail-bar-list");
            let scroller = gtk::ScrolledWindow::builder().child(&list).hexpand(true).vexpand(true).build();
            // Always shown, as a thin line the stylesheet keeps faint, rather than appearing only while scrolling.
            scroller.set_overlay_scrolling(false);
            scroller.add_css_class("thumbnail-bar-scroller");
            let pin_button = gtk::ToggleButton::builder()
                .icon_name("view-pin-symbolic")
                .tooltip_text("Keep the thumbnail bar shown")
                .build();
            let overlay_button = gtk::ToggleButton::builder()
                .icon_name("window-restore-symbolic")
                .tooltip_text("Float the thumbnail bar over the picture")
                .build();
            let header = gtk::CenterBox::new();
            header.add_css_class("thumbnail-bar-header");
            header.set_valign(gtk::Align::Fill);
            let toggles = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let close_button = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .tooltip_text("Close the thumbnail bar")
                .action_name("view.toggle-thumbnail-bar")
                .build();
            close_button.add_css_class("flat");
            close_button.add_css_class("circular");
            let counter = RotatedLabel::new("", 0.0);
            counter.add_css_class("title-3");
            counter.add_css_class("numeric");
            counter.set_tooltip_text(Some("Position in the folder"));
            header.set_center_widget(Some(&counter));
            for button in [&pin_button, &overlay_button] {
                button.add_css_class("flat");
                button.add_css_class("circular");
                toggles.append(button);
            }
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            content.append(&header);
            content.append(&scroller);
            Self {
                paths,
                selection,
                list,
                scroller,
                content,
                header,
                toggles,
                close_button,
                pin_button,
                overlay_button,
                counter,
                dragging: Cell::new(false),
                resizing: Cell::new(None),
                side: Cell::new(gtk::PositionType::Bottom),
                size,
                images,
                stamps,
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ThumbnailBar {
        const NAME: &'static str = "RsimgvThumbnailBar";
        type Type = super::ThumbnailBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("thumbnail-bar");
        }
    }

    impl ObjectImpl for ThumbnailBar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.content.set_parent(&*obj);
            self.pin_button.connect_toggled({
                let weak = obj.downgrade();
                move |button| {
                    if let Some(bar) = weak.upgrade() {
                        bar.emit_by_name::<()>("pin-toggled", &[&button.is_active()]);
                    }
                }
            });
            self.overlay_button.connect_toggled({
                let weak = obj.downgrade();
                move |button| {
                    if let Some(bar) = weak.upgrade() {
                        bar.emit_by_name::<()>("overlay-toggled", &[&button.is_active()]);
                    }
                }
            });
            let weak = obj.downgrade();
            self.list.connect_activate(move |_, position| {
                if let Some(bar) = weak.upgrade() {
                    bar.emit_by_name::<()>("item-activated", &[&position]);
                }
            });
            obj.set_orientation(gtk::Orientation::Horizontal);
            // The counter follows the selection and the number of items.
            self.selection.connect_selection_changed({
                let weak = obj.downgrade();
                move |_, _, _| {
                    if let Some(bar) = weak.upgrade() {
                        bar.update_counter();
                    }
                }
            });
            self.paths.connect_items_changed({
                let weak = obj.downgrade();
                move |_, _, _, _| {
                    if let Some(bar) = weak.upgrade() {
                        bar.update_counter();
                    }
                }
            });
            obj.update_counter();

            // A press that moves far enough becomes a drag of the whole bar; dropping it asks for the
            // side nearest the pointer. Claiming the drag cancels the list's click, so no item opens.
            // A press on the inner edge resizes instead, at once.
            let drag = gtk::GestureDrag::new();
            drag.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = obj.downgrade();
            drag.connect_drag_begin(move |gesture, x, y| {
                let Some(bar) = weak.upgrade() else { return };
                if bar.on_inner_edge(x, y) {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    bar.imp().resizing.set(Some(bar.imp().size.get()));
                }
            });
            let weak = obj.downgrade();
            drag.connect_drag_update(move |gesture, dx, dy| {
                let Some(bar) = weak.upgrade() else { return };
                if let Some(start) = bar.imp().resizing.get() {
                    // The inner edge follows the pointer, so moving it toward the picture grows the bar.
                    let grow = match bar.imp().side.get() {
                        gtk::PositionType::Top => dy,
                        gtk::PositionType::Bottom => -dy,
                        gtk::PositionType::Left => dx,
                        _ => -dx,
                    };
                    bar.set_thumbnail_size(start + grow.round() as i32);
                    return;
                }
                if !bar.imp().dragging.get() && dx.hypot(dy) >= DRAG_THRESHOLD {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    bar.imp().dragging.set(true);
                    bar.add_css_class("dragging");
                    bar.set_cursor_from_name(Some("grabbing"));
                }
            });
            let weak = obj.downgrade();
            drag.connect_drag_end(move |gesture, dx, dy| {
                let Some(bar) = weak.upgrade() else { return };
                if bar.imp().resizing.take().is_some() {
                    bar.refresh_thumbnails();
                    bar.emit_by_name::<()>("size-chosen", &[&bar.imp().size.get()]);
                    return;
                }
                if !bar.imp().dragging.replace(false) {
                    return;
                }
                bar.remove_css_class("dragging");
                bar.set_cursor(None);
                if let Some((x, y)) = gesture.start_point() {
                    bar.emit_by_name::<()>("side-dropped", &[&(x + dx), &(y + dy)]);
                }
            });
            obj.add_controller(drag);

            // The resize cursor shows over the inner edge.
            let motion = gtk::EventControllerMotion::new();
            let weak = obj.downgrade();
            motion.connect_motion(move |_, x, y| {
                let Some(bar) = weak.upgrade() else { return };
                if bar.imp().dragging.get() {
                    return;
                }
                let cursor = (bar.imp().resizing.get().is_some() || bar.on_inner_edge(x, y)).then(|| {
                    match bar.imp().side.get() {
                        gtk::PositionType::Top | gtk::PositionType::Bottom => "ns-resize",
                        _ => "ew-resize",
                    }
                });
                bar.set_cursor_from_name(cursor);
            });
            obj.add_controller(motion);
        }

        fn dispose(&self) {
            self.content.unparent();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> =
                LazyLock::new(|| vec![
                    Signal::builder("item-activated").param_types([u32::static_type()]).build(),
                    Signal::builder("pin-toggled").param_types([bool::static_type()]).build(),
                    Signal::builder("overlay-toggled").param_types([bool::static_type()]).build(),
                    Signal::builder("size-chosen").param_types([i32::static_type()]).build(),
                    Signal::builder("side-dropped").param_types([f64::static_type(), f64::static_type()]).build(),
                ]);
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for ThumbnailBar {}
}

glib::wrapper! {
    pub struct ThumbnailBar(ObjectSubclass<imp::ThumbnailBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ThumbnailBar {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbnailBar {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Puts the bar on `side` of the viewer: a row along the top or bottom, a column beside.
    pub fn set_side(&self, side: gtk::PositionType) {
        self.imp().side.set(side);
        // The scrollbar runs along the bar's outer edge, the one at the window's edge.
        self.imp().scroller.set_placement(match side {
            gtk::PositionType::Top => gtk::CornerType::BottomLeft,
            gtk::PositionType::Left => gtk::CornerType::TopRight,
            _ => gtk::CornerType::TopLeft,
        });
        let imp = self.imp();
        // The toggles go in the slot nearest the window's edge: the start of a bar at the top or left, the end of one at
        // the bottom or right. The close button takes the other.
        let (header, toggles, close) = (&imp.header, &imp.toggles, &imp.close_button);
        header.set_start_widget(None::<&gtk::Widget>);
        header.set_end_widget(None::<&gtk::Widget>);
        if matches!(side, gtk::PositionType::Top | gtk::PositionType::Left) {
            header.set_start_widget(Some(toggles));
            header.set_end_widget(Some(close));
        } else {
            header.set_start_widget(Some(close));
            header.set_end_widget(Some(toggles));
        }
        self.set_orientation(match side {
            gtk::PositionType::Top | gtk::PositionType::Bottom => gtk::Orientation::Horizontal,
            _ => gtk::Orientation::Vertical,
        });
    }

    /// Makes the thumbnails `size` pixels, kept within the bar's range.
    pub fn set_thumbnail_size(&self, size: i32) {
        let imp = self.imp();
        let size = size.clamp(THUMBNAIL_MIN, THUMBNAIL_MAX);
        if imp.size.replace(size) == size {
            return;
        }
        imp.images.borrow_mut().retain(|image| {
            let Some(image) = image.upgrade() else { return false };
            image.set_pixel_size(size);
            image.set_size_request(size, size);
            true
        });
        if imp.resizing.get().is_none() {
            self.refresh_thumbnails();
        }
    }

    /// Asks again for the thumbnails shown, at the size they are now.
    fn refresh_thumbnails(&self) {
        let imp = self.imp();
        imp.images.borrow_mut().clear();
        imp.list.set_factory(Some(&factory(imp.size.clone(), imp.images.clone(), imp.stamps.clone())));
        if let Some(position) = (imp.selection.selected() != gtk::INVALID_LIST_POSITION).then(|| imp.selection.selected()) {
            imp.list.scroll_to(position, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// Whether `x`, `y` is in the band along the edge of the bar that faces the picture.
    fn on_inner_edge(&self, x: f64, y: f64) -> bool {
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        match self.imp().side.get() {
            gtk::PositionType::Top => y > height - RESIZE_EDGE,
            gtk::PositionType::Bottom => y < RESIZE_EDGE,
            gtk::PositionType::Left => x > width - RESIZE_EDGE,
            _ => x < RESIZE_EDGE,
        }
    }

    /// Lays the thumbnails out in a row, or a column, scrolling along it.
    fn set_orientation(&self, orientation: gtk::Orientation) {
        let imp = self.imp();
        // The header runs across the bar at its start: a column before a row of thumbnails, a row above a column.
        imp.content.set_orientation(orientation);
        let horizontal = orientation == gtk::Orientation::Horizontal;
        let header_orientation = if horizontal { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal };
        imp.header.set_orientation(header_orientation);
        // In a column the position reads upward along it, rather than widening the column.
        imp.counter.set_angle(if horizontal { -90.0 } else { 0.0 });
        imp.toggles.set_orientation(header_orientation);
        // The header spans the bar's width in a row, or its height in a column, and is only as thick as it needs.
        imp.header.set_hexpand(!horizontal);
        imp.header.set_vexpand(horizontal);
        imp.header.set_halign(gtk::Align::Fill);
        imp.header.set_valign(gtk::Align::Fill);
        imp.list.set_orientation(orientation);
        let (h, v) = match orientation {
            gtk::Orientation::Horizontal => (gtk::PolicyType::Automatic, gtk::PolicyType::Never),
            _ => (gtk::PolicyType::Never, gtk::PolicyType::Automatic),
        };
        imp.scroller.set_policy(h, v);
        imp.scroller.set_propagate_natural_height(orientation == gtk::Orientation::Horizontal);
        imp.scroller.set_propagate_natural_width(orientation != gtk::Orientation::Horizontal);
        for class in ["horizontal", "vertical"] {
            self.remove_css_class(class);
        }
        self.add_css_class(if orientation == gtk::Orientation::Horizontal { "horizontal" } else { "vertical" });
    }

    /// Shows the selected item's place among the items: its number and how many there are. With no item selected it
    /// shows a dash for the number, and with no items nothing.
    fn update_counter(&self) {
        let imp = self.imp();
        let (selected, total) = (imp.selection.selected(), imp.paths.n_items());
        let text = match (total, selected) {
            (0, _) => String::new(),
            (_, gtk::INVALID_LIST_POSITION) => format!("\u{2013} / {total}"),
            _ => format!("{} / {total}", selected + 1),
        };
        imp.counter.set_text(&text);
    }

    /// Shows the thumbnails of `paths`, replacing those shown.
    pub fn set_paths(&self, paths: &[PathBuf]) {
        let imp = self.imp();
        *imp.stamps.borrow_mut() = paths.iter().map(|path| (path.clone(), thumbnail::stamp(path))).collect();
        let uris: Vec<String> = paths.iter().map(|path| uri_of(path)).collect();
        let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
        imp.paths.splice(0, imp.paths.n_items(), &uris);
    }

    /// Selects the thumbnail at `index` and scrolls it into view.
    pub fn select(&self, index: usize) {
        let imp = self.imp();
        let position = index as u32;
        if position >= imp.paths.n_items() {
            return;
        }
        imp.selection.set_selected(position);
        imp.list.scroll_to(position, gtk::ListScrollFlags::NONE, None);
    }

    /// Shows the pin and overlay toggles as `pinned` and `overlay`. The pin only matters while floating.
    pub fn set_toggles(&self, pinned: bool, overlay: bool) {
        let imp = self.imp();
        imp.pin_button.set_active(pinned);
        imp.overlay_button.set_active(overlay);
        imp.pin_button.set_sensitive(overlay);
        // A floating bar that is not pinned hides itself; any other stays until it is closed.
        imp.close_button.set_visible(!overlay || pinned);
    }

    /// Calls `callback` when the pin toggle is clicked, with whether it is now on.
    pub fn connect_pin_toggled<F: Fn(&Self, bool) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("pin-toggled", false, move |values| {
            let bar = values[0].get::<ThumbnailBar>().expect("bar argument");
            callback(&bar, values[1].get::<bool>().expect("active argument"));
            None
        })
    }

    /// Calls `callback` when the overlay toggle is clicked, with whether it is now on.
    pub fn connect_overlay_toggled<F: Fn(&Self, bool) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("overlay-toggled", false, move |values| {
            let bar = values[0].get::<ThumbnailBar>().expect("bar argument");
            callback(&bar, values[1].get::<bool>().expect("active argument"));
            None
        })
    }

    /// Whether the bar is being dragged to another side or resized.
    pub fn is_dragging(&self) -> bool {
        self.imp().dragging.get() || self.imp().resizing.get().is_some()
    }

    /// Calls `callback` with the thumbnail size, in pixels, a resize of the bar ended at.
    pub fn connect_size_chosen<F: Fn(&Self, i32) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("size-chosen", false, move |values| {
            let bar = values[0].get::<ThumbnailBar>().expect("bar argument");
            let size = values[1].get::<i32>().expect("size argument");
            callback(&bar, size);
            None
        })
    }

    /// Calls `callback` with where, in the bar's coordinates, a drag of the bar was dropped.
    pub fn connect_side_dropped<F: Fn(&Self, f64, f64) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("side-dropped", false, move |values| {
            let bar = values[0].get::<ThumbnailBar>().expect("bar argument");
            let x = values[1].get::<f64>().expect("x argument");
            let y = values[2].get::<f64>().expect("y argument");
            callback(&bar, x, y);
            None
        })
    }

    /// Calls `callback` with the index of a thumbnail when it is clicked.
    pub fn connect_item_activated<F: Fn(&Self, usize) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("item-activated", false, move |values| {
            let bar = values[0].get::<ThumbnailBar>().expect("bar argument");
            let position = values[1].get::<u32>().expect("position argument");
            callback(&bar, position as usize);
            None
        })
    }
}

/// Makes each row an image of `size`, which asks for its item's thumbnail when bound, and keeps the
/// images in `images`.
fn factory(
    size: Rc<Cell<i32>>,
    images: Rc<RefCell<Vec<glib::WeakRef<gtk::Image>>>>,
    stamps: Rc<RefCell<HashMap<PathBuf, i64>>>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let setup_size = size.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let pixels = setup_size.get();
        let image = gtk::Image::builder()
            .pixel_size(pixels)
            .icon_name("image-x-generic-symbolic")
            .width_request(pixels)
            .height_request(pixels)
            .build();
        image.add_css_class("thumbnail");
        images.borrow_mut().push(image.downgrade());
        item.set_child(Some(&image));
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let (Some(image), Some(path)) = (
            item.child().and_downcast::<gtk::Image>(),
            item.item().and_downcast::<gtk::StringObject>().and_then(|object| path_of(&object)),
        ) else {
            return;
        };
        let video = is_video(&path);
        image.set_icon_name(Some(if video { "video-x-generic-symbolic" } else { "image-x-generic-symbolic" }));
        image.set_tooltip_text(path.file_name().and_then(|name| name.to_str()));
        let media = if video { ThumbnailMedia::Video } else { ThumbnailMedia::Picture };

        // The thumbnail is still wanted while the row shows the same file.
        let wanted_item = item.downgrade();
        let wanted_path = path.clone();
        let wanted = move || {
            wanted_item.upgrade().and_then(|item| item.item().and_downcast::<gtk::StringObject>()).and_then(|object| path_of(&object)).is_some_and(
                |shown| shown == wanted_path,
            )
        };
        let shown = image.downgrade();
        let ready_item = item.downgrade();
        let ready_path = path.clone();
        Thumbnails::shared().request_still(
            &path,
            stamps.borrow().get(&path).copied().unwrap_or_default(),
            media,
            ThumbnailSize::for_pixels(size.get() * image.scale_factor()),
            wanted,
            move |texture| {
                let (Some(image), Some(item)) = (shown.upgrade(), ready_item.upgrade()) else { return };
                let same = item
                    .item()
                    .and_downcast::<gtk::StringObject>()
                    .and_then(|object| path_of(&object))
                    .is_some_and(|shown| shown == ready_path);
                if !same {
                    return;
                }
                if video {
                    image.set_paintable(Some(&FilmStrip::new(texture)));
                } else {
                    image.set_paintable(Some(texture));
                }
            },
        );
    });
    factory.connect_unbind(|_, item| {
        if let Some(image) = item.downcast_ref::<gtk::ListItem>().and_then(|item| item.child()).and_downcast::<gtk::Image>() {
            image.set_icon_name(Some("image-x-generic-symbolic"));
        }
        Thumbnails::shared().unload_soon();
    });
    factory
}
