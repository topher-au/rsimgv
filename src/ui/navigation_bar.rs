//! Back, forward and up buttons with a visited-location history.
//!
//! The widget keeps the history itself. `set_location` records a visit (dropping any forward
//! entries, as a browser does). Back, forward and up move through the history and emit
//! `open-location`, which the application handles by showing that folder. Long-pressing back or
//! forward opens a menu of the locations in that direction, and picking one jumps straight there.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib::prelude::StaticType;
use gtk::glib::subclass::Signal;
use gtk::glib;
use gtk::glib::prelude::ToVariant;
use gtk::gio;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Back,
    Forward,
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rimgv/ui/navigation_bar.ui")]
    pub struct NavigationBar {
        #[template_child]
        pub back_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub forward_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub up_button: TemplateChild<gtk::Button>,
        pub(super) history: RefCell<Vec<PathBuf>>,
        /// The item selected in each location of the history, by its name, if one was.
        pub(super) selections: RefCell<Vec<Option<String>>>,
        pub(super) index: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NavigationBar {
        const NAME: &'static str = "RsimgvNavigationBar";
        type Type = super::NavigationBar;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for NavigationBar {
        fn signals() -> &'static [Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![Signal::builder("open-location")
                    .param_types([String::static_type()])
                    .build()]
            });
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_css_class("linked");

            for (button, direction) in [
                (self.back_button.clone(), Direction::Back),
                (self.forward_button.clone(), Direction::Forward),
            ] {
                // A long press also releases a click; the flag makes that click do nothing.
                let suppress_click = Rc::new(Cell::new(false));

                let press = gtk::GestureClick::new();
                let flag = suppress_click.clone();
                press.connect_pressed(move |_, _, _, _| flag.set(false));
                button.add_controller(press);

                let long_press = gtk::GestureLongPress::new();
                let flag = suppress_click.clone();
                long_press.connect_pressed(move |gesture, _, _| {
                    flag.set(true);
                    let Some(button) = gesture.widget().and_downcast::<gtk::Button>() else {
                        return;
                    };
                    if let Some(bar) = button.ancestor(super::NavigationBar::static_type()).and_downcast::<super::NavigationBar>() {
                        bar.show_history_menu(&button, direction);
                    }
                });
                button.add_controller(long_press);

                button.connect_clicked(move |button| {
                    if suppress_click.replace(false) {
                        return;
                    }
                    let Some(bar) = button.ancestor(super::NavigationBar::static_type()).and_downcast::<super::NavigationBar>() else {
                        return;
                    };
                    match direction {
                        Direction::Back => bar.back(),
                        Direction::Forward => bar.forward(),
                    }
                });
            }
            self.up_button.connect_clicked(|button| {
                if let Some(bar) = button.ancestor(super::NavigationBar::static_type()).and_downcast::<super::NavigationBar>() {
                    bar.up();
                }
            });

            // The menu items name this action by its target index.
            let actions = gio::SimpleActionGroup::new();
            let go_to = gio::SimpleAction::new("go-to", Some(glib::VariantTy::UINT32));
            let weak = obj.downgrade();
            go_to.connect_activate(move |_, parameter| {
                let (Some(bar), Some(index)) = (weak.upgrade(), parameter.and_then(|p| p.get::<u32>())) else {
                    return;
                };
                bar.go_to(index as usize);
            });
            actions.add_action(&go_to);
            obj.insert_action_group("nav", Some(&actions));

            obj.update_buttons();
        }
    }

    impl WidgetImpl for NavigationBar {}
    impl BoxImpl for NavigationBar {}
}

glib::wrapper! {
    pub struct NavigationBar(ObjectSubclass<imp::NavigationBar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl NavigationBar {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// The location being shown, or `None` before the first visit.
    pub fn location(&self) -> Option<PathBuf> {
        let imp = self.imp();
        imp.history.borrow().get(imp.index.get()).cloned()
    }

    /// Records a visit to `path`. Visiting the current location does nothing; visiting anything
    /// else drops the forward history, as a browser does.
    pub fn set_location(&self, path: &Path) {
        let imp = self.imp();
        {
            let mut history = imp.history.borrow_mut();
            if history.get(imp.index.get()).is_some_and(|current| current == path) {
                return;
            }
            if !history.is_empty() {
                history.truncate(imp.index.get() + 1);
                imp.selections.borrow_mut().truncate(imp.index.get() + 1);
            }
            history.push(path.to_path_buf());
            imp.selections.borrow_mut().push(None);
            imp.index.set(history.len() - 1);
        }
        self.update_buttons();
    }

    pub fn back(&self) {
        let index = self.imp().index.get();
        if index > 0 {
            self.go_to(index - 1);
        }
    }

    pub fn forward(&self) {
        let index = self.imp().index.get();
        if index + 1 < self.imp().history.borrow().len() {
            self.go_to(index + 1);
        }
    }

    /// Visits the parent of the current location, if it has one.
    pub fn up(&self) {
        let Some(parent) = self.location().and_then(|current| current.parent().map(Path::to_path_buf)) else {
            return;
        };
        self.set_location(&parent);
        self.emit_location(&parent);
    }

    /// Jumps to an entry in the history by its index, emitting `open-location` for it.
    pub fn go_to(&self, index: usize) {
        let target = {
            let history = self.imp().history.borrow();
            match history.get(index) {
                Some(path) => path.clone(),
                None => return,
            }
        };
        self.imp().index.set(index);
        self.update_buttons();
        self.emit_location(&target);
    }

    /// Records the item selected in the current location, by its name, or none.
    pub fn set_selected(&self, name: Option<String>) {
        let imp = self.imp();
        if let Some(selection) = imp.selections.borrow_mut().get_mut(imp.index.get()) {
            *selection = name;
        }
    }

    /// The item selected the last time the current location was shown, if one was.
    pub fn selected(&self) -> Option<String> {
        let imp = self.imp();
        imp.selections.borrow().get(imp.index.get()).cloned().flatten()
    }

    /// The visited locations, the item selected in each, and the index of the current one, for keeping a tab's
    /// history.
    pub fn history_state(&self) -> (Vec<PathBuf>, Vec<Option<String>>, usize) {
        let imp = self.imp();
        (imp.history.borrow().clone(), imp.selections.borrow().clone(), imp.index.get())
    }

    /// Replaces the history, as when switching to another tab. Emits nothing.
    pub fn set_history_state(&self, history: Vec<PathBuf>, mut selections: Vec<Option<String>>, index: usize) {
        let imp = self.imp();
        let index = index.min(history.len().saturating_sub(1));
        selections.resize(history.len(), None);
        imp.history.replace(history);
        imp.selections.replace(selections);
        imp.index.set(index);
        self.update_buttons();
    }

    /// The entries a menu in `direction` offers: each one's history index and path, nearest first.
    pub fn history_entries(&self, direction: Direction) -> Vec<(usize, PathBuf)> {
        let imp = self.imp();
        let history = imp.history.borrow();
        let index = imp.index.get();
        let indices: Vec<usize> = match direction {
            Direction::Back => (0..index).rev().collect(),
            Direction::Forward => (index + 1..history.len()).collect(),
        };
        indices.into_iter().map(|i| (i, history[i].clone())).collect()
    }

    /// Calls `callback(bar, path)` when back, forward, up or a history entry asks for a location.
    pub fn connect_open_location<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, &Path) + 'static,
    {
        self.connect_local("open-location", false, move |values| {
            let bar = values[0].get::<NavigationBar>().expect("bar argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&bar, Path::new(&path));
            None
        })
    }

    fn emit_location(&self, path: &Path) {
        let text = path.to_string_lossy().into_owned();
        self.emit_by_name::<()>("open-location", &[&text]);
    }

    fn update_buttons(&self) {
        let imp = self.imp();
        let len = imp.history.borrow().len();
        let index = imp.index.get();
        imp.back_button.set_sensitive(index > 0);
        imp.forward_button.set_sensitive(index + 1 < len);
        let has_parent = self.location().is_some_and(|current| current.parent().is_some());
        imp.up_button.set_sensitive(has_parent);
    }

    /// Pops up the history in `direction` from `button`. Nothing shows if there is no history there.
    fn show_history_menu(&self, button: &gtk::Button, direction: Direction) {
        let entries = self.history_entries(direction);
        if entries.is_empty() {
            return;
        }
        let menu = gio::Menu::new();
        for (index, path) in entries {
            let item = gio::MenuItem::new(Some(&path.to_string_lossy()), None);
            item.set_action_and_target_value(Some("nav.go-to"), Some(&(index as u32).to_variant()));
            menu.append_item(&item);
        }
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(button);
        // The menu activates its item's action after closing, so the popover must stay in the
        // widget tree until then or actions like `win.open-in-new-tab` aren't found.
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn run_scenarios() {
        history_moves_back_and_forward();
        visiting_drops_forward_history();
        up_visits_the_parent();
    }

    fn recorder(bar: &NavigationBar) -> Rc<RefCell<Vec<PathBuf>>> {
        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        bar.connect_open_location(move |_, path| sink.borrow_mut().push(path.to_path_buf()));
        opened
    }

    fn history_moves_back_and_forward() {
        let bar = NavigationBar::new();
        let opened = recorder(&bar);
        bar.set_location(Path::new("/a"));
        bar.set_location(Path::new("/a/b"));
        bar.set_location(Path::new("/a/b/c"));
        assert_eq!(bar.location(), Some(PathBuf::from("/a/b/c")));

        bar.back();
        assert_eq!(bar.location(), Some(PathBuf::from("/a/b")));
        assert_eq!(*opened.borrow(), vec![PathBuf::from("/a/b")], "back asks the app to open it");
        assert_eq!(bar.history_entries(Direction::Back), vec![(0, PathBuf::from("/a"))]);
        assert_eq!(bar.history_entries(Direction::Forward), vec![(2, PathBuf::from("/a/b/c"))]);

        bar.back();
        bar.back();
        assert_eq!(bar.location(), Some(PathBuf::from("/a")), "back stops at the first visit");
        assert!(!bar.imp().back_button.is_sensitive());

        bar.forward();
        bar.forward();
        assert_eq!(bar.location(), Some(PathBuf::from("/a/b/c")));
        assert!(!bar.imp().forward_button.is_sensitive());
    }

    fn visiting_drops_forward_history() {
        let bar = NavigationBar::new();
        bar.set_location(Path::new("/a"));
        bar.set_location(Path::new("/a/b"));
        bar.back();
        bar.set_location(Path::new("/x"));
        assert!(bar.history_entries(Direction::Forward).is_empty(), "forward history was dropped");
        assert_eq!(bar.history_entries(Direction::Back), vec![(0, PathBuf::from("/a"))]);
    }

    fn up_visits_the_parent() {
        let bar = NavigationBar::new();
        let opened = recorder(&bar);
        bar.set_location(Path::new("/a/b"));
        bar.up();
        assert_eq!(bar.location(), Some(PathBuf::from("/a")));
        assert_eq!(*opened.borrow(), vec![PathBuf::from("/a")]);

        bar.set_location(Path::new("/"));
        assert!(!bar.imp().up_button.is_sensitive(), "the root has no parent");
    }
}
