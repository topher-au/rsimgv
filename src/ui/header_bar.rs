//! The main content header bar, built without `AdwHeaderBar`: a centre box whose title area takes
//! the spare width, with start and end groups on either side and the window controls at the far end.
//! Children are added with `<child type="start|title|end">`, as with `AdwHeaderBar`.

use adw::subclass::prelude::*;
use gtk::glib;
use gtk::prelude::*;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rsimgv/ui/header_bar.ui")]
    pub struct HeaderBar {
        #[template_child]
        pub start: TemplateChild<gtk::Box>,
        #[template_child]
        pub title: TemplateChild<gtk::Box>,
        #[template_child]
        pub end: TemplateChild<gtk::Box>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HeaderBar {
        const NAME: &'static str = "RsimgvHeaderBar";
        type Type = super::HeaderBar;
        type ParentType = gtk::Box;
        type Interfaces = (gtk::Buildable,);

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for HeaderBar {}
    impl WidgetImpl for HeaderBar {}
    impl BoxImpl for HeaderBar {}

    impl BuildableImpl for HeaderBar {
        fn add_child(&self, builder: &gtk::Builder, child: &glib::Object, type_: Option<&str>) {
            let slot = match type_ {
                Some("start") => Some(&self.start),
                Some("title") => Some(&self.title),
                Some("end") => Some(&self.end),
                _ => None,
            };
            match (slot, child.downcast_ref::<gtk::Widget>()) {
                (Some(slot), Some(widget)) => slot.append(widget),
                _ => self.parent_add_child(builder, child, type_),
            }
        }
    }
}

glib::wrapper! {
    pub struct HeaderBar(ObjectSubclass<imp::HeaderBar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl HeaderBar {
    pub fn new() -> Self {
        glib::Object::new()
    }
}
