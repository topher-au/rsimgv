//! The floating bar over a view: a spinner and a stop button while something loads, and one or two
//! lines of status otherwise. It is a port of Nautilus's `NautilusFloatingBar`, including its hiding
//! while the pointer is over it, so it does not cover what the pointer is pointing at.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene};

/// How long the pointer must rest over the bar before it hides, in milliseconds.
const HOVER_HIDE_DELAY: u64 = 100;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rimgv/ui/floating_bar.ui")]
    pub struct FloatingBar {
        #[template_child]
        pub spinner: TemplateChild<adw::Spinner>,
        #[template_child]
        pub primary: TemplateChild<gtk::Label>,
        #[template_child]
        pub details: TemplateChild<gtk::Label>,
        #[template_child]
        pub stop_button: TemplateChild<gtk::Button>,
        /// Whether the bar offers to stop something, which keeps it from hiding under the pointer.
        pub(super) interactive: Cell<bool>,
        pub(super) hover_timeout: RefCell<Option<glib::SourceId>>,
        pub(super) pointer: Cell<(f64, f64)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FloatingBar {
        const NAME: &'static str = "RsimgvFloatingBar";
        type Type = super::FloatingBar;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for FloatingBar {
        fn constructed(&self) {
            self.parent_constructed();
            self.pointer.set((-1.0, -1.0));
            let obj = self.obj();
            self.stop_button.connect_clicked({
                let obj = obj.downgrade();
                move |_| {
                    if let Some(bar) = obj.upgrade() {
                        bar.emit_by_name::<()>("stop", &[]);
                    }
                }
            });
        }

        fn signals() -> &'static [glib::subclass::Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<glib::subclass::Signal>> =
                LazyLock::new(|| vec![glib::subclass::Signal::builder("stop").build()]);
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for FloatingBar {}
    impl BoxImpl for FloatingBar {}
}

glib::wrapper! {
    pub struct FloatingBar(ObjectSubclass<imp::FloatingBar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl FloatingBar {
    /// Sets the primary and details labels. An empty or missing label is hidden.
    pub fn set_labels(&self, primary: Option<&str>, details: Option<&str>) {
        let imp = self.imp();
        imp.primary.set_label(primary.unwrap_or_default());
        imp.primary.set_visible(primary.is_some_and(|text| !text.is_empty()));
        imp.details.set_label(details.unwrap_or_default());
        imp.details.set_visible(details.is_some_and(|text| !text.is_empty()));
    }

    /// Shows or hides the spinner and the stop button together, for something that can be stopped.
    pub fn set_loading(&self, loading: bool) {
        self.imp().spinner.set_visible(loading);
        self.set_show_stop(loading);
    }

    pub fn set_show_stop(&self, show: bool) {
        self.imp().stop_button.set_visible(show);
        self.imp().interactive.set(show);
    }

    /// Calls `callback` when the stop button is clicked.
    pub fn connect_stop<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("stop", false, move |values| {
            let bar = values[0].get::<FloatingBar>().expect("bar argument");
            callback(&bar);
            None
        })
    }

    /// Hides the bar while the pointer rests over it, and shows it again when the pointer leaves. The
    /// pointer is tracked over `parent`, the widget the bar floats on.
    pub fn attach_hover_hide(&self, parent: &impl IsA<gtk::Widget>) {
        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        let parent_widget = parent.as_ref().clone();
        motion.connect_motion(move |_, x, y| {
            let Some(bar) = weak.upgrade() else { return };
            bar.imp().pointer.set((x, y));
            if bar.imp().interactive.get() || !bar.is_visible() || bar.imp().hover_timeout.borrow().is_some() {
                return;
            }
            let Some(bounds) = bar.compute_bounds(&parent_widget) else { return };
            if !bounds.contains_point(&graphene::Point::new(x as f32, y as f32)) {
                return;
            }
            let weak = bar.downgrade();
            let source = glib::timeout_add_local(Duration::from_millis(HOVER_HIDE_DELAY), move || {
                let Some(bar) = weak.upgrade() else { return glib::ControlFlow::Break };
                let (x, y) = bar.imp().pointer.get();
                if !bounds.contains_point(&graphene::Point::new(x as f32, y as f32)) {
                    bar.set_visible(true);
                    bar.imp().hover_timeout.take();
                    return glib::ControlFlow::Break;
                }
                bar.set_visible(false);
                glib::ControlFlow::Continue
            });
            bar.imp().hover_timeout.replace(Some(source));
        });
        let weak = self.downgrade();
        motion.connect_leave(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.imp().pointer.set((-1.0, -1.0));
            }
        });
        parent.as_ref().add_controller(motion);
    }

    /// Stops any pending hover check and shows the bar again.
    pub fn cancel_hover_hide(&self) {
        if let Some(source) = self.imp().hover_timeout.take() {
            source.remove();
        }
    }
}
