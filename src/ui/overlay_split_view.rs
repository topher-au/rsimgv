//! A port of `AdwOverlaySplitView` (libadwaita `src/adw-overlay-split-view.c`) with a sidebar
//! that can be resized by dragging its inner edge, like a `GtkPaned`.
//!
//! The sizing and allocation follow the C layout functions: an uncollapsed split pins the sidebar
//! beside the content, a collapsed one overlays it, and both clamp to the min/max width. The
//! show/hide animation is a spring, and clicking the shield or pressing Escape hides a collapsed
//! sidebar. Not ported: the swipe gestures, the shadow, and focus hand-off between panes.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::glib;
use gtk::glib::prelude::StaticType;
use gtk::glib::subclass::Signal;
use gtk::graphene;
use gtk::gsk;
use gtk::subclass::prelude::*;
use std::sync::LazyLock;

/// Width of the grip between the sidebar and the content, in pixels.
pub const HANDLE_WIDTH: i32 = 6;

mod imp {
    use super::*;

    pub struct OverlaySplitView {
        pub(super) content: adw::Bin,
        pub(super) shield: gtk::Box,
        pub(super) sidebar_box: gtk::Box,
        pub(super) sidebar_slot: adw::Bin,
        pub(super) handle: gtk::Box,
        pub(super) animation: RefCell<Option<adw::SpringAnimation>>,
        pub(super) sidebar_position: Cell<gtk::PackType>,
        pub(super) show_sidebar: Cell<bool>,
        pub(super) collapsed: Cell<bool>,
        pub(super) pin_sidebar: Cell<bool>,
        pub(super) enable_show_gesture: Cell<bool>,
        pub(super) enable_hide_gesture: Cell<bool>,
        pub(super) min_sidebar_width: Cell<f64>,
        pub(super) max_sidebar_width: Cell<f64>,
        pub(super) sidebar_width_fraction: Cell<f64>,
        pub(super) sidebar_width_unit: Cell<adw::LengthUnit>,
        /// 0 hidden, 1 fully shown; the spring animation drives it.
        pub(super) show_progress: Cell<f64>,
        /// Last sidebar width laid out, including the handle.
        pub(super) sidebar_width: Cell<i32>,
        /// Width set by dragging, including the handle; -1 when the layout chooses it.
        pub(super) user_sidebar_width: Cell<i32>,
        pub(super) drag_start: Cell<i32>,
    }

    impl Default for OverlaySplitView {
        fn default() -> Self {
            let handle = gtk::Box::new(gtk::Orientation::Vertical, 0);
            handle.set_width_request(HANDLE_WIDTH);
            handle.set_vexpand(true);
            handle.add_css_class("rsimgv-split-handle");
            handle.set_cursor_from_name(Some("col-resize"));

            let sidebar_slot = adw::Bin::new();
            sidebar_slot.set_hexpand(true);
            sidebar_slot.set_vexpand(true);

            let sidebar_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            sidebar_box.append(&sidebar_slot);
            sidebar_box.append(&handle);

            Self {
                content: adw::Bin::new(),
                shield: gtk::Box::new(gtk::Orientation::Vertical, 0),
                sidebar_box,
                sidebar_slot,
                handle,
                animation: RefCell::new(None),
                sidebar_position: Cell::new(gtk::PackType::Start),
                show_sidebar: Cell::new(true),
                collapsed: Cell::new(false),
                pin_sidebar: Cell::new(false),
                enable_show_gesture: Cell::new(true),
                enable_hide_gesture: Cell::new(true),
                min_sidebar_width: Cell::new(180.0),
                max_sidebar_width: Cell::new(280.0),
                sidebar_width_fraction: Cell::new(0.25),
                sidebar_width_unit: Cell::new(adw::LengthUnit::Sp),
                show_progress: Cell::new(1.0),
                sidebar_width: Cell::new(0),
                user_sidebar_width: Cell::new(-1),
                drag_start: Cell::new(0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OverlaySplitView {
        const NAME: &'static str = "RsimgvOverlaySplitView";
        type Type = super::OverlaySplitView;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Buildable,);

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<layout::SplitLayout>();
            klass.set_css_name("overlay-split-view");
        }
    }

    impl ObjectImpl for OverlaySplitView {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![Signal::builder("show-sidebar-changed")
                    .param_types([bool::static_type()])
                    .build()]
            });
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Children are parented in the order the C widget uses: content, shield, sidebar.
            // The sidebar sits above the shield, so clicks on the shield only reach it when open.
            self.content.set_parent(&*obj);
            self.shield.set_parent(&*obj);
            self.sidebar_box.set_parent(&*obj);
            self.shield.set_visible(true);
            self.shield.set_child_visible(false);
            self.sidebar_box.set_hexpand(false);
            obj.update_pane_classes();

            let weak = obj.downgrade();
            let target = adw::CallbackAnimationTarget::new(move |progress| {
                if let Some(view) = weak.upgrade() {
                    view.set_show_progress(progress);
                }
            });
            let animation = adw::SpringAnimation::new(
                &*obj,
                0.0,
                0.0,
                adw::SpringParams::new(1.0, 0.5, 500.0),
                target,
            );
            let weak = obj.downgrade();
            animation.connect_done(move |_| {
                if let Some(view) = weak.upgrade() {
                    if view.imp().show_progress.get() < 0.5 {
                        view.imp().sidebar_box.set_child_visible(false);
                    }
                }
            });
            self.animation.replace(Some(animation));

            // Clicking the shield hides a collapsed sidebar.
            let click = gtk::GestureClick::new();
            click.set_exclusive(true);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = obj.downgrade();
            click.connect_released(move |_, _, _, _| {
                if let Some(view) = weak.upgrade() {
                    view.set_show_sidebar(false);
                }
            });
            self.shield.add_controller(click);

            // Escape hides a collapsed sidebar that is open.
            let keys = gtk::EventControllerKey::new();
            let weak = obj.downgrade();
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(view) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let open = view.imp().show_progress.get() > 0.0;
                if key == gtk::gdk::Key::Escape && view.collapsed() && open {
                    view.set_show_sidebar(false);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            obj.add_controller(keys);

            // Dragging the handle sets the sidebar width.
            let drag = gtk::GestureDrag::new();
            let weak = obj.downgrade();
            drag.connect_drag_begin(move |_, _, _| {
                if let Some(view) = weak.upgrade() {
                    view.imp().drag_start.set(view.imp().sidebar_width.get());
                }
            });
            let weak = obj.downgrade();
            drag.connect_drag_update(move |_, offset_x, _| {
                if let Some(view) = weak.upgrade() {
                    view.drag_sidebar(view.imp().drag_start.get(), offset_x as i32);
                }
            });
            self.handle.add_controller(drag);

            obj.update_handle_side();
        }

        fn dispose(&self) {
            self.content.unparent();
            self.shield.unparent();
            self.sidebar_box.unparent();
        }
    }

    impl WidgetImpl for OverlaySplitView {}

    /// `<child type="sidebar">` and `<child type="content">` in XML, as with AdwOverlaySplitView.
    impl BuildableImpl for OverlaySplitView {
        fn add_child(&self, builder: &gtk::Builder, child: &glib::Object, type_: Option<&str>) {
            let obj = self.obj();
            match (type_, child.downcast_ref::<gtk::Widget>()) {
                (Some("sidebar"), Some(widget)) => obj.set_sidebar(Some(widget)),
                (Some("content"), Some(widget)) => obj.set_content(Some(widget)),
                _ => self.parent_add_child(builder, child, type_),
            }
        }
    }
}

mod layout {
    use super::*;
    use gtk::subclass::layout_manager::LayoutManagerImpl;

    /// Measures and allocates the split view. The C version swaps between two layouts when
    /// `collapsed` changes; this one reads the flag each time.
    #[derive(Default)]
    pub struct SplitLayoutImpl;

    #[glib::object_subclass]
    impl ObjectSubclass for SplitLayoutImpl {
        const NAME: &'static str = "RsimgvOverlaySplitViewLayout";
        type Type = SplitLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for SplitLayoutImpl {}

    impl LayoutManagerImpl for SplitLayoutImpl {
        fn measure(
            &self,
            widget: &gtk::Widget,
            orientation: gtk::Orientation,
            for_size: i32,
        ) -> (i32, i32, i32, i32) {
            let view = widget.downcast_ref::<super::OverlaySplitView>().expect("split view");
            view.measure_split(orientation, for_size)
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, baseline: i32) {
            let view = widget.downcast_ref::<super::OverlaySplitView>().expect("split view");
            view.allocate_split(width, height, baseline);
        }
    }

    glib::wrapper! {
        pub struct SplitLayout(ObjectSubclass<SplitLayoutImpl>)
            @extends gtk::LayoutManager;
    }
}

glib::wrapper! {
    pub struct OverlaySplitView(ObjectSubclass<imp::OverlaySplitView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for OverlaySplitView {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlaySplitView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_content(&self, content: Option<&impl IsA<gtk::Widget>>) {
        self.imp().content.set_child(content);
    }

    pub fn content(&self) -> Option<gtk::Widget> {
        self.imp().content.child()
    }

    pub fn set_sidebar(&self, sidebar: Option<&impl IsA<gtk::Widget>>) {
        self.imp().sidebar_slot.set_child(sidebar);
    }

    pub fn sidebar(&self) -> Option<gtk::Widget> {
        self.imp().sidebar_slot.child()
    }

    pub fn sidebar_position(&self) -> gtk::PackType {
        self.imp().sidebar_position.get()
    }

    pub fn set_sidebar_position(&self, position: gtk::PackType) {
        self.imp().sidebar_position.set(position);
        self.update_handle_side();
        self.queue_resize();
    }

    pub fn collapsed(&self) -> bool {
        self.imp().collapsed.get()
    }

    /// Collapsing hides the sidebar unless it is pinned; uncollapsing shows it.
    pub fn set_collapsed(&self, collapsed: bool) {
        let imp = self.imp();
        if imp.collapsed.get() == collapsed {
            return;
        }
        imp.collapsed.set(collapsed);
        self.update_pane_classes();
        self.set_show_progress(imp.show_progress.get());
        if !imp.pin_sidebar.get() {
            self.set_show_sidebar_inner(!collapsed, false);
        }
        self.queue_resize();
    }

    pub fn show_sidebar(&self) -> bool {
        self.imp().show_sidebar.get()
    }

    /// Shows or hides the sidebar with the same spring animation the C widget uses.
    /// Calls `callback(view, shown)` whenever the sidebar starts opening or closing.
    pub fn connect_show_sidebar_changed<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, bool) + 'static,
    {
        self.connect_local("show-sidebar-changed", false, move |values| {
            let view = values[0].get::<OverlaySplitView>().expect("view argument");
            let shown = values[1].get::<bool>().expect("shown argument");
            callback(&view, shown);
            None
        })
    }

    pub fn set_show_sidebar(&self, show: bool) {
        self.set_show_sidebar_inner(show, true);
    }

    pub fn pin_sidebar(&self) -> bool {
        self.imp().pin_sidebar.get()
    }

    pub fn set_pin_sidebar(&self, pin: bool) {
        self.imp().pin_sidebar.set(pin);
    }

    /// Stored for parity with the C widget. Edge swipes are not implemented in this port.
    pub fn set_enable_show_gesture(&self, enable: bool) {
        self.imp().enable_show_gesture.set(enable);
    }

    /// Stored for parity with the C widget. Edge swipes are not implemented in this port.
    pub fn set_enable_hide_gesture(&self, enable: bool) {
        self.imp().enable_hide_gesture.set(enable);
    }

    pub fn min_sidebar_width(&self) -> f64 {
        self.imp().min_sidebar_width.get()
    }

    pub fn set_min_sidebar_width(&self, width: f64) {
        self.imp().min_sidebar_width.set(width.max(0.0));
        self.queue_resize();
    }

    pub fn max_sidebar_width(&self) -> f64 {
        self.imp().max_sidebar_width.get()
    }

    pub fn set_max_sidebar_width(&self, width: f64) {
        self.imp().max_sidebar_width.set(width.max(0.0));
        self.queue_resize();
    }

    pub fn set_sidebar_width_fraction(&self, fraction: f64) {
        self.imp().sidebar_width_fraction.set(fraction.clamp(0.0, 1.0));
        self.queue_resize();
    }

    pub fn set_sidebar_width_unit(&self, unit: adw::LengthUnit) {
        self.imp().sidebar_width_unit.set(unit);
        self.queue_resize();
    }

    /// The sidebar width chosen by dragging, including the handle, or `None` for the layout's own.
    pub fn user_sidebar_width(&self) -> Option<i32> {
        let width = self.imp().user_sidebar_width.get();
        (width >= 0).then_some(width)
    }

    pub fn set_user_sidebar_width(&self, width: Option<i32>) {
        self.imp().user_sidebar_width.set(width.unwrap_or(-1).max(-1));
        self.queue_resize();
    }

    /// The sidebar width last laid out, including the handle.
    pub fn sidebar_width(&self) -> i32 {
        self.imp().sidebar_width.get()
    }

    /// Sets the sidebar to `start_width` plus a drag of `offset` pixels, clamped to the limits.
    /// Dragging toward the content widens a start sidebar; for an end sidebar it is the reverse.
    pub fn drag_sidebar(&self, start_width: i32, offset: i32) {
        let delta = if self.sidebar_position() == gtk::PackType::End {
            -offset
        } else {
            offset
        };
        let (min, max) = self.sidebar_limits();
        self.set_user_sidebar_width(Some((start_width + delta).clamp(min, max)));
    }

    /// The width the sidebar gets for a split of `width`, as the C layout computes it.
    pub fn sidebar_width_for(&self, width: i32, collapsed: bool) -> i32 {
        let (min, max) = self.sidebar_limits();
        let wanted = match self.user_sidebar_width() {
            Some(width) => width,
            None if collapsed => width,
            None => (f64::from(width) * self.imp().sidebar_width_fraction.get()) as i32,
        };
        wanted.clamp(min, max)
    }

    fn set_show_sidebar_inner(&self, show: bool, animate: bool) {
        let imp = self.imp();
        if imp.show_sidebar.get() == show {
            return;
        }
        imp.show_sidebar.set(show);
        self.emit_by_name::<()>("show-sidebar-changed", &[&show]);

        let target = if show { 1.0 } else { 0.0 };
        if show {
            imp.sidebar_box.set_child_visible(true);
        }
        if animate {
            if let Some(animation) = imp.animation.borrow().as_ref() {
                animation.set_value_from(imp.show_progress.get());
                animation.set_value_to(target);
                animation.play();
            }
        } else {
            self.set_show_progress(target);
            if !show {
                imp.sidebar_box.set_child_visible(false);
            }
        }
    }

    fn set_show_progress(&self, progress: f64) {
        let imp = self.imp();
        imp.show_progress.set(progress);
        imp.shield.set_child_visible(imp.collapsed.get() && progress > 0.0);
        if imp.collapsed.get() {
            self.queue_allocate();
        } else {
            self.queue_resize();
        }
    }

    /// Pane style classes, so libadwaita's stylesheet paints the sidebar and content backgrounds.
    fn update_pane_classes(&self) {
        let imp = self.imp();
        if self.collapsed() {
            imp.content.remove_css_class("content-pane");
            imp.sidebar_box.remove_css_class("sidebar-pane");
            imp.sidebar_box.add_css_class("overlay-pane");
        } else {
            imp.content.add_css_class("content-pane");
            imp.sidebar_box.add_css_class("sidebar-pane");
            imp.sidebar_box.remove_css_class("overlay-pane");
        }
    }

    /// Keeps the handle on the sidebar's inner edge: right of it, or left for an end sidebar.
    fn update_handle_side(&self) {
        let imp = self.imp();
        let handle: &gtk::Widget = imp.handle.upcast_ref();
        let slot: &gtk::Widget = imp.sidebar_slot.upcast_ref();
        if self.sidebar_position() == gtk::PackType::End {
            imp.sidebar_box.reorder_child_after(handle, None::<&gtk::Widget>);
        } else {
            imp.sidebar_box.reorder_child_after(handle, Some(slot));
        }
    }

    /// Converts a length in `unit` to pixels. Font-relative units follow the `gtk-xft-dpi` setting.
    fn length_to_px(&self, unit: adw::LengthUnit, value: f64) -> f64 {
        let dpi = gtk::Settings::default()
            .map(|settings| settings.property::<i32>("gtk-xft-dpi"))
            .filter(|dpi| *dpi > 0)
            .map_or(96.0, |dpi| f64::from(dpi) / 1024.0);
        match unit {
            adw::LengthUnit::Pt => value * dpi / 72.0,
            adw::LengthUnit::Sp => value * dpi / 96.0,
            _ => value,
        }
    }

    /// The sidebar's pixel limits. The maximum is never below the minimum.
    fn sidebar_limits(&self) -> (i32, i32) {
        let imp = self.imp();
        let unit = imp.sidebar_width_unit.get();
        let (measured_min, _, _, _) = imp.sidebar_box.measure(gtk::Orientation::Horizontal, -1);
        let min = measured_min.max(self.length_to_px(unit, imp.min_sidebar_width.get()).ceil() as i32);
        let max = min.max(self.length_to_px(unit, imp.max_sidebar_width.get()).ceil() as i32);
        (min, max)
    }

    /// Port of `measure_uncollapsed` and `measure_collapsed`.
    fn measure_split(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
        let imp = self.imp();
        let (content_min, content_nat, _, _) = imp.content.measure(orientation, -1);
        let (sidebar_min, sidebar_nat, _, _) = imp.sidebar_box.measure(orientation, -1);

        if orientation == gtk::Orientation::Vertical {
            return (sidebar_min.max(content_min), sidebar_nat.max(content_nat), -1, -1);
        }

        let (min_limit, max_limit) = self.sidebar_limits();
        if self.collapsed() {
            let nat = sidebar_nat.clamp(min_limit, max_limit);
            return (content_min.max(min_limit), content_nat.max(nat), -1, -1);
        }

        // The sidebar's own natural width is ignored; it is estimated from the content and fraction.
        let fraction = imp.sidebar_width_fraction.get();
        let estimate = (f64::from(content_nat) * fraction / (1.0 - fraction).max(f64::EPSILON)).ceil();
        let mut nat = (estimate as i32).clamp(min_limit, max_limit);
        if let Some(width) = self.user_sidebar_width() {
            nat = width.clamp(min_limit, max_limit);
        }
        let progress = imp.show_progress.get().clamp(0.0, 1.0);
        let min = (f64::from(min_limit) * progress) as i32 + content_min;
        let nat = (f64::from(nat) * progress) as i32 + content_nat;
        (min, nat, -1, -1)
    }

    /// Port of `allocate_uncollapsed` and `allocate_collapsed`.
    fn allocate_split(&self, width: i32, height: i32, baseline: i32) {
        let imp = self.imp();
        let progress = imp.show_progress.get();

        if !self.collapsed() {
            let (content_min, _, _, _) = imp.content.measure(gtk::Orientation::Horizontal, -1);
            let sidebar_width = self.sidebar_width_for(width, false).min(width - content_min).max(0);
            imp.sidebar_width.set(sidebar_width);

            let mut offset = (f64::from(sidebar_width) * progress) as i32;
            let mut sidebar_alloc = sidebar_width;
            if offset > sidebar_width {
                sidebar_alloc = offset;
                offset = sidebar_width;
            }

            if self.sidebar_position() == gtk::PackType::End {
                let x = if sidebar_alloc > sidebar_width {
                    width - sidebar_alloc
                } else {
                    width - offset
                };
                imp.sidebar_box.allocate(sidebar_alloc, height, baseline, Some(translate(x)));
                imp.content.allocate(width - offset, height, baseline, None);
            } else {
                let x = offset - sidebar_width;
                imp.sidebar_box.allocate(sidebar_alloc, height, baseline, Some(translate(x)));
                imp.content.allocate(width - offset, height, baseline, Some(translate(offset)));
            }
            return;
        }

        let sidebar_width = self.sidebar_width_for(width, true);
        imp.sidebar_width.set(sidebar_width);
        let offset = (f64::from(sidebar_width) * progress) as i32;

        let (mut pos, mut alloc) = if self.sidebar_position() != gtk::PackType::End {
            (offset - sidebar_width, sidebar_width)
        } else {
            (width - offset, sidebar_width)
        };
        if offset > sidebar_width {
            alloc = offset;
            if self.sidebar_position() != gtk::PackType::End {
                pos = 0;
            }
        }

        imp.content.allocate(width, height, baseline, None);
        imp.sidebar_box.allocate(alloc, height, baseline, Some(translate(pos)));
        imp.shield.allocate(width, height, baseline, None);
    }
}

fn translate(x: i32) -> gsk::Transform {
    gsk::Transform::new().translate(&graphene::Point::new(x as f32, 0.0))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn run_scenarios() {
        widths_follow_fraction_and_limits();
        drag_resizes_within_limits();
        collapse_hides_unless_pinned();
    }

    fn widths_follow_fraction_and_limits() {
        let view = OverlaySplitView::new();
        view.set_sidebar(Some(&gtk::Label::new(Some("side"))));
        view.set_content(Some(&gtk::Label::new(Some("content"))));

        // Defaults: 25% of the width, between 180 and 280 sp (96 dpi gives px = sp).
        assert_eq!(view.sidebar_width_for(400, false), 180, "25% is below the minimum");
        assert_eq!(view.sidebar_width_for(800, false), 200);
        assert_eq!(view.sidebar_width_for(2000, false), 280, "clamped to the maximum");

        // Collapsed, the sidebar takes the available width within the limits.
        assert_eq!(view.sidebar_width_for(1000, true), 280);
        assert_eq!(view.sidebar_width_for(200, true), 200);
    }

    fn drag_resizes_within_limits() {
        let view = OverlaySplitView::new();
        view.set_sidebar(Some(&gtk::Label::new(Some("side"))));
        view.set_min_sidebar_width(150.0);
        view.set_max_sidebar_width(400.0);
        view.set_sidebar_width_unit(adw::LengthUnit::Px);

        assert_eq!(view.user_sidebar_width(), None, "the layout chooses by default");
        view.drag_sidebar(300, 50);
        assert_eq!(view.user_sidebar_width(), Some(350), "drag toward the content widens");
        assert_eq!(view.sidebar_width_for(1000, false), 350, "the dragged width is used");

        view.drag_sidebar(300, 500);
        assert_eq!(view.user_sidebar_width(), Some(400), "clamped to the maximum");
        view.drag_sidebar(300, -500);
        assert_eq!(view.user_sidebar_width(), Some(150), "clamped to the minimum");

        view.set_sidebar_position(gtk::PackType::End);
        view.drag_sidebar(300, 40);
        assert_eq!(view.user_sidebar_width(), Some(260), "drag direction flips for an end sidebar");
    }

    fn collapse_hides_unless_pinned() {
        let view = OverlaySplitView::new();
        view.set_sidebar(Some(&gtk::Label::new(Some("side"))));
        assert!(view.show_sidebar());

        view.set_collapsed(true);
        assert!(!view.show_sidebar(), "collapsing hides the sidebar");
        view.set_collapsed(false);
        assert!(view.show_sidebar(), "uncollapsing shows it again");

        view.set_pin_sidebar(true);
        view.set_collapsed(true);
        assert!(view.show_sidebar(), "a pinned sidebar stays shown");
    }
}
