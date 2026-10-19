//! The viewer's thumbnail bar: a scrolling row or column of the thumbnails of the items the viewer
//! steps through, with the one shown selected. Clicking a thumbnail shows its item. Only the
//! thumbnails in view are made, as the list view only binds the rows it shows. Dragging the bar asks for
//! it to move to the side of the viewer it is dropped nearest.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::glib;

use super::film_strip::FilmStrip;
use super::media_viewer::is_video;
use crate::fs::thumbnail::{Media as ThumbnailMedia, Size as ThumbnailSize, Thumbnails};

/// The size of a thumbnail in the bar, in pixels.
const THUMBNAIL_PIXELS: i32 = 80;

/// How far, in pixels, the pointer must move before a press on the bar becomes a drag rather than a click.
const DRAG_THRESHOLD: f64 = 24.0;

/// The modification time of `path` in seconds, or 0 if it cannot be read.
fn modified_secs(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs() as i64)
}

mod imp {
    use super::*;
    use std::sync::LazyLock;

    pub struct ThumbnailBar {
        pub(super) paths: gtk::StringList,
        pub(super) selection: gtk::SingleSelection,
        pub(super) list: gtk::ListView,
        pub(super) scroller: gtk::ScrolledWindow,
        /// Whether the bar is being dragged to another side.
        pub(super) dragging: Cell<bool>,
    }

    impl Default for ThumbnailBar {
        fn default() -> Self {
            let paths = gtk::StringList::new(&[]);
            let selection = gtk::SingleSelection::new(Some(paths.clone()));
            selection.set_autoselect(false);
            selection.set_can_unselect(true);
            let list = gtk::ListView::new(Some(selection.clone()), Some(factory()));
            list.set_single_click_activate(true);
            list.add_css_class("thumbnail-bar-list");
            let scroller = gtk::ScrolledWindow::builder().child(&list).build();
            Self { paths, selection, list, scroller, dragging: Cell::new(false) }
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
            self.scroller.set_parent(&*obj);
            let weak = obj.downgrade();
            self.list.connect_activate(move |_, position| {
                if let Some(bar) = weak.upgrade() {
                    bar.emit_by_name::<()>("item-activated", &[&position]);
                }
            });
            obj.set_orientation(gtk::Orientation::Horizontal);

            // A press that moves far enough becomes a drag of the whole bar; dropping it asks for the
            // side nearest the pointer. Claiming the drag cancels the list's click, so no item opens.
            let drag = gtk::GestureDrag::new();
            drag.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = obj.downgrade();
            drag.connect_drag_update(move |gesture, dx, dy| {
                let Some(bar) = weak.upgrade() else { return };
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
        }

        fn dispose(&self) {
            self.scroller.unparent();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> =
                LazyLock::new(|| vec![
                    Signal::builder("item-activated").param_types([u32::static_type()]).build(),
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

    /// Lays the thumbnails out in a row, or a column, scrolling along it.
    pub fn set_orientation(&self, orientation: gtk::Orientation) {
        let imp = self.imp();
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

    /// Shows the thumbnails of `paths`, replacing those shown.
    pub fn set_paths(&self, paths: &[PathBuf]) {
        let imp = self.imp();
        let names: Vec<String> = paths.iter().map(|path| path.to_string_lossy().into_owned()).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        imp.paths.splice(0, imp.paths.n_items(), &names);
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

    /// Whether the bar is being dragged to another side.
    pub fn is_dragging(&self) -> bool {
        self.imp().dragging.get()
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

/// Makes each row a fixed-size image, which asks for its item's thumbnail when bound.
fn factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let image = gtk::Image::builder()
            .pixel_size(THUMBNAIL_PIXELS)
            .icon_name("image-x-generic-symbolic")
            .width_request(THUMBNAIL_PIXELS)
            .height_request(THUMBNAIL_PIXELS)
            .build();
        image.add_css_class("thumbnail");
        item.set_child(Some(&image));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let (Some(image), Some(path)) = (
            item.child().and_downcast::<gtk::Image>(),
            item.item().and_downcast::<gtk::StringObject>().map(|object| PathBuf::from(object.string().as_str())),
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
            wanted_item.upgrade().and_then(|item| item.item().and_downcast::<gtk::StringObject>()).is_some_and(
                |object| Path::new(object.string().as_str()) == wanted_path,
            )
        };
        let shown = image.downgrade();
        let ready_item = item.downgrade();
        let ready_path = path.clone();
        Thumbnails::shared().request_still(
            &path,
            modified_secs(&path),
            media,
            ThumbnailSize::for_pixels(THUMBNAIL_PIXELS),
            wanted,
            move |texture| {
                let (Some(image), Some(item)) = (shown.upgrade(), ready_item.upgrade()) else { return };
                let same = item
                    .item()
                    .and_downcast::<gtk::StringObject>()
                    .is_some_and(|object| Path::new(object.string().as_str()) == ready_path);
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
    });
    factory
}
