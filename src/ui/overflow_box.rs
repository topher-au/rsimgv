//! A horizontal box of buttons that lends the ones that do not fit to an overflow menu. The box lays its buttons out
//! again whenever its width changes, so it can be resized freely.

use std::cell::{Cell, RefCell, OnceCell};

use gtk::gio;
use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// A button in the box, with its width when it was last measured and its entry in the overflow menu.
struct Item {
    widget: gtk::Widget,
    entry: gio::MenuItem,
    width: Cell<i32>,
}

/// The end of the box whose buttons collapse into the overflow menu when the box is too narrow.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "RsimgvOverflowCollapse")]
pub enum CollapseFrom {
    /// The last buttons collapse, and the overflow button sits at the end.
    #[default]
    #[enum_value(name = "End", nick = "end")]
    End,
    /// The first buttons collapse, and the overflow button sits at the start.
    #[enum_value(name = "Start", nick = "start")]
    Start,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct OverflowBox {
        pub(super) items: RefCell<Vec<Item>>,
        pub(super) overflow: OnceCell<gtk::MenuButton>,
        pub(super) menu: OnceCell<gio::Menu>,
        pub(super) actions: OnceCell<gio::SimpleActionGroup>,
        pub(super) next_action: Cell<u32>,
        /// The stretch of empty room that takes what the buttons leave.
        pub(super) spacer: RefCell<Option<gtk::Widget>>,
        /// The widget kept after the overflow button, which the buttons make room for.
        pub(super) end: RefCell<Option<gtk::Widget>>,
        /// The overflow button's minimum width, kept while it is hidden so the box can still shrink to it.
        pub(super) overflow_min: Cell<i32>,
        /// The end whose buttons collapse into the overflow menu.
        pub(super) collapse_from: Cell<CollapseFrom>,
        /// The room between two neighbours in the row, in pixels.
        pub(super) spacing: Cell<i32>,
        /// The width the buttons were last laid out for, so a frame without a resize does no work.
        pub(super) laid_out: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OverflowBox {
        const NAME: &'static str = "RsimgvOverflowBox";
        type Type = super::OverflowBox;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<layout::OverflowLayout>();
            klass.set_css_name("overflow-box");
        }
    }

    impl ObjectImpl for OverflowBox {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let menu = gio::Menu::new();
            let overflow = gtk::MenuButton::new();
            overflow.set_icon_name("view-more-symbolic");
            overflow.set_tooltip_text(Some("More actions"));
            overflow.add_css_class("flat");
            overflow.set_menu_model(Some(&menu));
            overflow.set_parent(&*obj);
            overflow.set_visible(true);
            self.overflow_min.set(overflow.measure(gtk::Orientation::Horizontal, -1).0);
            overflow.set_visible(false);

            let actions = gio::SimpleActionGroup::new();
            obj.insert_action_group("overflow", Some(&actions));

            self.overflow.set(overflow).expect("the overflow button is made once");
            self.menu.set(menu).expect("the overflow menu is made once");
            self.actions.set(actions).expect("the overflow actions are made once");
        }

        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: std::sync::LazyLock<Vec<glib::ParamSpec>> = std::sync::LazyLock::new(|| {
                vec![glib::ParamSpecEnum::builder::<CollapseFrom>("collapse-from").build()]
            });
            PROPERTIES.as_ref()
        }

        fn set_property(&self, id: usize, value: &glib::Value, _pspec: &glib::ParamSpec) {
            match id {
                1 => self.collapse_from.set(value.get().unwrap_or_default()),
                _ => unimplemented!(),
            }
            let obj = self.obj();
            obj.position_overflow();
            obj.fit();
            obj.queue_resize();
        }

        fn property(&self, id: usize, _pspec: &glib::ParamSpec) -> glib::Value {
            match id {
                1 => self.collapse_from.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn dispose(&self) {
            // The items hold strong references to the box's children, which would keep them alive with their parent.
            self.items.borrow_mut().clear();
            self.spacer.borrow_mut().take();
            self.end.borrow_mut().take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for OverflowBox {}
}

mod layout {
    use super::*;
    use gtk::subclass::layout_manager::LayoutManagerImpl;

    #[derive(Default)]
    pub struct OverflowLayoutImpl;

    #[glib::object_subclass]
    impl ObjectSubclass for OverflowLayoutImpl {
        const NAME: &'static str = "RsimgvOverflowBoxLayout";
        type Type = OverflowLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for OverflowLayoutImpl {}

    impl LayoutManagerImpl for OverflowLayoutImpl {
        fn measure(&self, widget: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let view = widget.downcast_ref::<super::OverflowBox>().expect("overflow box");
            view.measure_overflow(orientation, for_size)
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, baseline: i32) {
            let view = widget.downcast_ref::<super::OverflowBox>().expect("overflow box");
            view.allocate_overflow(width, height, baseline);
        }
    }

    glib::wrapper! {
        pub struct OverflowLayout(ObjectSubclass<OverflowLayoutImpl>)
            @extends gtk::LayoutManager;
    }
}

glib::wrapper! {
    pub struct OverflowBox(ObjectSubclass<imp::OverflowBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OverflowBox {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Sets the end whose buttons collapse into the overflow menu. The overflow button moves to that end.
    pub fn set_collapse_from(&self, end: CollapseFrom) {
        self.set_property("collapse-from", end);
    }

    /// The end whose buttons collapse into the overflow menu.
    pub fn collapse_from(&self) -> CollapseFrom {
        self.property("collapse-from")
    }

    /// Puts the overflow button at the end the buttons collapse from.
    fn position_overflow(&self) {
        let Some(overflow) = self.imp().overflow.get() else { return };
        match self.imp().collapse_from.get() {
            CollapseFrom::Start => overflow.insert_after(self, None::<&gtk::Widget>),
            CollapseFrom::End => match self.imp().end.borrow().as_ref() {
                Some(end) => overflow.insert_before(self, Some(end)),
                None => overflow.insert_before(self, None::<&gtk::Widget>),
            },
        }
    }

    /// Appends `button`. When it does not fit in the box, it is hidden and listed in the overflow menu under its
    /// tooltip, or its label, running the action it names.
    pub fn add_button(&self, button: &gtk::Button) {
        let label = button
            .tooltip_text()
            .map(|text| text.to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| {
                let label = button.label().map(|label| label.to_string()).unwrap_or_default();
                if label.is_empty() { "Action".to_owned() } else { label }
            });
        let entry = match button.action_name() {
            Some(action) => gio::MenuItem::new(Some(&label), Some(&action)),
            None => self.click_entry(label, button.clone().upcast()),
        };
        self.add_item(button, entry);
    }

    /// Appends `button`. When it does not fit, it is listed in the overflow menu under its tooltip, with its menu as a
    /// submenu.
    pub fn add_menu_button(&self, button: &gtk::MenuButton) {
        let label = button
            .tooltip_text()
            .map(|text| text.to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "More actions".to_owned());
        let entry = match button.menu_model() {
            Some(model) => gio::MenuItem::new_submenu(Some(&label), &model),
            None => self.click_entry(label, button.clone().upcast()),
        };
        self.add_item(button, entry);
    }

    /// Appends `widget` with the overflow entry `entry`, before the overflow button.
    pub fn add_item(&self, widget: &impl IsA<gtk::Widget>, entry: gio::MenuItem) {
        let widget = widget.as_ref().clone();
        widget.set_parent(self);
        self.imp().items.borrow_mut().push(Item { widget, entry, width: Cell::new(0) });
        self.position_overflow();
        self.fit();
    }

    /// Adds a stretch of empty room after the buttons, so the overflow button and the end widget sit at the end of the
    /// row while every button fits.
    pub fn add_spacer(&self) {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        spacer.set_parent(self);
        *self.imp().spacer.borrow_mut() = Some(spacer.upcast());
        self.position_overflow();
        self.fit();
    }

    /// Puts `widget` after the overflow button, where it stays whatever collapses.
    pub fn set_end(&self, widget: &impl IsA<gtk::Widget>) {
        let widget = widget.as_ref().clone();
        widget.set_parent(self);
        *self.imp().end.borrow_mut() = Some(widget);
        self.position_overflow();
        self.fit();
    }

    /// Sets the text that explains what the overflow button shows.
    pub fn set_overflow_tooltip(&self, text: &str) {
        if let Some(overflow) = self.imp().overflow.get() {
            overflow.set_tooltip_text(Some(text));
        }
    }

    /// Sets the room between two neighbours in the row, in pixels.
    pub fn set_spacing(&self, spacing: i32) {
        self.imp().spacing.set(spacing);
        self.queue_resize();
    }

    /// The room between two neighbours in the row, in pixels.
    pub fn spacing(&self) -> i32 {
        self.imp().spacing.get()
    }

    /// An entry for a button with no action: activating it clicks the button.
    fn click_entry(&self, label: String, widget: gtk::Widget) -> gio::MenuItem {
        let imp = self.imp();
        let number = imp.next_action.get();
        imp.next_action.set(number + 1);
        let name = format!("item-{number}");
        let action = gio::SimpleAction::new(&name, None);
        action.connect_activate(move |_, _| {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                button.emit_clicked();
            } else if let Some(menu) = widget.downcast_ref::<gtk::MenuButton>() {
                menu.popup();
            } else {
                widget.activate();
            }
        });
        imp.actions.get().expect("the actions exist once the box is constructed").add_action(&action);
        gio::MenuItem::new(Some(&label), Some(&format!("overflow.{name}")))
    }

    /// Shows the buttons that fit in the box's width, and lists the rest in the overflow menu.
    fn fit(&self) {
        self.fit_width(self.width());
    }

    /// Shows the buttons that fit in `width`, and lists the rest in the overflow menu.
    fn fit_width(&self, width: i32) {
        let imp = self.imp();
        if width <= 0 {
            return;
        }
        imp.laid_out.set(width);

        // Every button is shown while it is measured, so that each has its own width.
        let items = imp.items.borrow();
        for item in items.iter() {
            item.widget.set_visible(true);
            item.width.set(item.widget.measure(gtk::Orientation::Horizontal, -1).1);
        }
        let overflow = imp.overflow.get().expect("the overflow button exists once the box is constructed");
        overflow.set_visible(true);
        let (overflow_min, overflow_width) = {
            let (minimum, natural, ..) = overflow.measure(gtk::Orientation::Horizontal, -1);
            (minimum, natural)
        };
        imp.overflow_min.set(overflow_min);
        let spacing = imp.spacing.get();
        // The end widget sits after the overflow button, with one gap between them.
        let end = imp.end.borrow().as_ref().map_or(0, |widget| widget.measure(gtk::Orientation::Horizontal, -1).1 + spacing);
        let widths: Vec<i32> = items.iter().map(|item| item.width.get()).collect();
        let available = (width - end).max(0);
        let shown = match imp.collapse_from.get() {
            CollapseFrom::End => visible_count(&widths, overflow_width, spacing, available),
            CollapseFrom::Start => {
                // The buttons that stay are the last ones, so count from the other end.
                let reversed: Vec<i32> = widths.iter().rev().copied().collect();
                visible_count(&reversed, overflow_width, spacing, available)
            }
        };
        let first = items.len() - shown;
        for (index, item) in items.iter().enumerate() {
            item.widget.set_visible(match imp.collapse_from.get() {
                CollapseFrom::End => index < shown,
                CollapseFrom::Start => index >= first,
            });
        }
        overflow.set_visible(shown < items.len());

        let menu = imp.menu.get().expect("the overflow menu exists once the box is constructed");
        menu.remove_all();
        match imp.collapse_from.get() {
            CollapseFrom::End => {
                for item in items.iter().skip(shown) {
                    menu.append_item(&item.entry);
                }
            }
            CollapseFrom::Start => {
                for item in items.iter().take(first) {
                    menu.append_item(&item.entry);
                }
            }
        }
    }

    /// Measures the row. The buttons never set the minimum width: they can all fold into the overflow menu. Their
    /// widths stay in the natural width, so the box can grow back out again.
    fn measure_overflow(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
        let imp = self.imp();
        let spacing = imp.spacing.get();
        if orientation == gtk::Orientation::Vertical {
            let mut minimum = 0;
            let mut natural = 0;
            for child in self.children() {
                if child.is_visible() {
                    let (child_min, child_nat, ..) = child.measure(orientation, for_size);
                    minimum = minimum.max(child_min);
                    natural = natural.max(child_nat);
                }
            }
            return (minimum, natural, -1, -1);
        }

        let items = imp.items.borrow();
        let mut buttons = 0;
        for item in items.iter() {
            let measured = if item.widget.is_visible() { item.widget.measure(orientation, for_size).1 } else { 0 };
            buttons += item.width.get().max(measured);
        }
        let gaps = if items.len() > 1 { spacing * (items.len() as i32 - 1) } else { 0 };
        let end = imp.end.borrow().as_ref().map_or(0, |widget| widget.measure(orientation, for_size).1);
        let end_gap = if !items.is_empty() && end > 0 { spacing } else { 0 };
        let natural = buttons + gaps + end + end_gap;
        let minimum = if items.is_empty() {
            end
        } else {
            imp.overflow_min.get() + end + if end > 0 { spacing } else { 0 }
        };
        (minimum, natural, -1, -1)
    }

    /// Allocates the visible children from left to right. The spacer takes the room the others leave.
    fn allocate_overflow(&self, width: i32, height: i32, baseline: i32) {
        self.fit_width(width);

        let imp = self.imp();
        let spacing = imp.spacing.get();
        let spacer = imp.spacer.borrow().clone();
        let mut visible: Vec<(gtk::Widget, i32, bool)> = Vec::new();
        let mut child = self.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            if widget.is_visible() {
                let expand = spacer.as_ref().is_some_and(|spacer| spacer == &widget);
                let width = widget.measure(gtk::Orientation::Horizontal, -1).1;
                visible.push((widget, width, expand));
            }
            child = next;
        }

        let gaps = if visible.len() > 1 { spacing * (visible.len() as i32 - 1) } else { 0 };
        let fixed: i32 = visible.iter().filter(|(_, _, expand)| !expand).map(|(_, width, _)| *width).sum();
        let expandable = visible.iter().filter(|(_, _, expand)| *expand).count();
        let spare = (width - fixed - gaps).max(0);
        let each = if expandable > 0 { spare / expandable as i32 } else { 0 };
        let mut extra = if expandable > 0 { spare % expandable as i32 } else { 0 };

        let mut x = 0;
        for (index, (widget, width, expand)) in visible.iter().enumerate() {
            if index > 0 {
                x += spacing;
            }
            let mut width = *width;
            if *expand {
                width = each;
                if extra > 0 {
                    width += 1;
                    extra -= 1;
                }
            }
            widget.allocate(width.max(0), height, baseline, Some(translate(x)));
            x += width.max(0);
        }
    }

    fn children(&self) -> Vec<gtk::Widget> {
        let mut children = Vec::new();
        let mut child = self.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            children.push(widget);
        }
        children
    }
}

impl Default for OverflowBox {
    fn default() -> Self {
        Self::new()
    }
}

fn translate(x: i32) -> gsk::Transform {
    gsk::Transform::new().translate(&graphene::Point::new(x as f32, 0.0))
}

/// How many leading buttons fit in `width` with an overflow button of `overflow` and `spacing` between neighbours. All
/// of them fit when none needs the overflow button.
fn visible_count(widths: &[i32], overflow: i32, spacing: i32, width: i32) -> usize {
    let count = widths.len();
    let gaps = if count > 1 { spacing * (count as i32 - 1) } else { 0 };
    let mut used: i32 = widths.iter().sum();
    if used + gaps <= width {
        return count;
    }
    let mut shown = count;
    while shown > 0 && used + overflow + spacing * shown as i32 > width {
        shown -= 1;
        used -= widths[shown];
    }
    shown
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn run_scenarios() {
        buttons_collapse_from_the_end();
        spacing_counts_between_shown_buttons_and_the_overflow();
        a_widget_collapses_at_its_width();
        the_overflow_button_moves_to_the_collapse_end();
    }

    fn buttons_collapse_from_the_end() {
        assert_eq!(visible_count(&[50, 50, 50], 30, 0, 200), 3, "all the buttons fit");
        assert_eq!(visible_count(&[50, 50, 50], 30, 0, 120), 1, "the last two make way for the overflow button");
        assert_eq!(visible_count(&[50, 50, 50], 30, 0, 10), 0, "no button fits");
        assert_eq!(visible_count(&[], 30, 0, 100), 0, "an empty box needs no overflow");
    }

    fn spacing_counts_between_shown_buttons_and_the_overflow() {
        assert_eq!(visible_count(&[50, 50, 50], 30, 10, 150), 2, "the gaps are part of the row");
        assert_eq!(visible_count(&[50, 50, 50], 30, 10, 200), 3, "the gaps still fit");
    }

    fn a_widget_collapses_at_its_width() {
        let bar = OverflowBox::new();
        let buttons: Vec<gtk::Button> = (0..4)
            .map(|index| {
                let button = gtk::Button::with_label(&format!("Button {index}"));
                bar.add_button(&button);
                button
            })
            .collect();
        let overflow = bar.imp().overflow.get().expect("the overflow button");
        let menu = bar.imp().menu.get().expect("the overflow menu");

        bar.allocate(10, 30, -1, None);
        assert!(!buttons[0].is_visible(), "not even the first button fits at 10px");
        assert!(overflow.is_visible(), "the overflow button is shown");
        assert_eq!(menu.n_items(), buttons.len() as i32, "every button is listed");

        bar.allocate(2000, 30, -1, None);
        assert!(buttons.iter().all(|button| button.is_visible()), "every button fits in 2000px");
        assert!(!overflow.is_visible(), "there is nothing to overflow");
        assert_eq!(menu.n_items(), 0, "the menu is emptied");

        let (minimum, natural, ..) = bar.measure(gtk::Orientation::Horizontal, -1);
        assert!(minimum < natural, "the buttons do not set the minimum width: minimum {minimum}, natural {natural}");
    }

    fn the_overflow_button_moves_to_the_collapse_end() {
        let bar = OverflowBox::new();
        let buttons: Vec<gtk::Button> = (0..4)
            .map(|index| {
                let button = gtk::Button::with_label(&format!("Button {index}"));
                bar.add_button(&button);
                button
            })
            .collect();
        let overflow = bar.imp().overflow.get().expect("the overflow button");
        assert_eq!(bar.last_child().as_ref(), Some(overflow.upcast_ref::<gtk::Widget>()), "by default the overflow button is at the end");

        bar.set_collapse_from(CollapseFrom::Start);
        assert_eq!(bar.first_child().as_ref(), Some(overflow.upcast_ref::<gtk::Widget>()), "collapsing from the start moves the overflow button there");

        bar.allocate(10, 30, -1, None);
        let overflow_width = overflow.measure(gtk::Orientation::Horizontal, -1).1;
        let widths: Vec<i32> = bar.imp().items.borrow().iter().map(|item| item.width.get()).collect();
        bar.allocate(overflow_width + widths[2] + widths[3], 30, -1, None);
        assert!(!buttons[0].is_visible() && !buttons[1].is_visible(), "the first buttons collapse");
        assert!(buttons[2].is_visible() && buttons[3].is_visible(), "the last buttons stay");
    }
}
