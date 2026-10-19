use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use gtk::glib::prelude::StaticType;
use gtk::glib::subclass::Signal;
use gtk::graphene;
use std::sync::LazyLock;

use crate::fs::FsNode;

const HIDDEN_CLASS: &str = "rsimgv-hidden-dir";
/// Indent per level below the root's children, in pixels.
const LEVEL_INDENT: i32 = 16;

/// Indent for a row at `depth`. The root is depth 0 and its children are depth 1; neither is
/// indented, so the root folder's items line up with the root. Each deeper level adds one step.
pub fn row_indent(depth: u32) -> i32 {
    (depth.saturating_sub(1) as i32) * LEVEL_INDENT
}

/// How the root header is labelled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "RsimgvRootDisplay")]
pub enum RootDisplay {
    /// The directory's own name, e.g. "/" or "Downloads".
    #[enum_value(name = "Directory name", nick = "directory-name")]
    DirectoryName,
    /// The home directory and `/` get a friendly label and icon; other roots keep their name.
    #[default]
    #[enum_value(name = "Friendly", nick = "friendly")]
    Friendly,
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties, gtk::CompositeTemplate)]
    #[properties(wrapper_type = super::FsTree)]
    #[template(resource = "/au/topher/rimgv/ui/fs_tree.ui")]
    pub struct FsTree {
        #[template_child]
        pub scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub list_view: TemplateChild<gtk::ListView>,
        /// The root directory; it is the first row of the tree.
        pub(super) root: RefCell<Option<FsNode>>,
        pub(super) tree_model: RefCell<Option<gtk::TreeListModel>>,
        pub(super) selection: RefCell<Option<gtk::SingleSelection>>,
        /// Bumped by each reveal, so a reveal that is overtaken by a newer one selects nothing.
        pub(super) reveal_generation: Cell<u64>,
        /// Where a row sat when an arrow press began, restored once the toggle has laid out.
        pub(super) pending_anchor: RefCell<Option<Anchor>>,
        pub(super) filter: RefCell<Option<gtk::CustomFilter>>,
        /// Shared with the filter, so toggling needs only `filter.changed()`.
        pub(super) show_hidden: Rc<Cell<bool>>,
        /// Whether the header has a toggle that shows or hides the tree below it.
        #[property(get, set)]
        root_collapsible: Cell<bool>,
        #[property(get, set, builder(RootDisplay::default()))]
        root_display: Cell<RootDisplay>,
        /// Whether navigating collapses every folder outside the new folder's path.
        #[property(get, set)]
        collapse_on_navigate: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FsTree {
        const NAME: &'static str = "RsimgvFsTree";
        type Type = super::FsTree;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for FsTree {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("item-context-requested")
                        .param_types([FsNode::static_type(), f64::static_type(), f64::static_type()])
                        .build(),
                    Signal::builder("location-selected")
                        .param_types([String::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.add_css_class("rsimgv-fs-tree");
            obj.connect_root_collapsible_notify(|tree| tree.refresh_root_rows());
            obj.connect_root_display_notify(|tree| tree.refresh_root_rows());

            // Double-click (or Enter) on a row activates it, which toggles its expansion.
            let tree = obj.downgrade();
            self.list_view.connect_activate(move |_, position| {
                if let Some(tree) = tree.upgrade() {
                    tree.toggle_row_at(position);
                }
            });

            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(|_, obj| {
                let item = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
                // Row layout lives in XML; the factory only wires it up.
                let builder = gtk::Builder::from_resource("/au/topher/rimgv/ui/fs_tree_row.ui");
                let expander: gtk::TreeExpander = builder
                    .object("expander")
                    .expect("fs_tree_row.ui defines `expander`");
                item.set_child(Some(&expander));

                // Right-click reports the row's node and the pointer position in the tree's coordinates.
                // Presses on the arrow (left of the label) keep the row where it was on screen.
                let arrow = gtk::GestureClick::new();
                arrow.set_button(gdk::BUTTON_PRIMARY);
                arrow.set_propagation_phase(gtk::PropagationPhase::Capture);
                arrow.connect_pressed(|gesture, _, x, _| {
                    let Some(expander) = gesture.widget().and_downcast::<gtk::TreeExpander>() else {
                        return;
                    };
                    let label_x = expander
                        .child()
                        .and_then(|child| child.compute_bounds(&expander))
                        .map_or(0.0, |bounds| f64::from(bounds.x()));
                    if x >= label_x {
                        return;
                    }
                    if let Some((tree, path)) = super::tree_and_path(&expander) {
                        tree.capture_toggle_anchor(&path);
                    }
                });
                arrow.connect_released(|gesture, _, _, _| {
                    let Some(expander) = gesture.widget().and_downcast::<gtk::TreeExpander>() else {
                        return;
                    };
                    if let Some((tree, _)) = super::tree_and_path(&expander) {
                        tree.release_toggle_anchor();
                    }
                });
                expander.add_controller(arrow);

                let click = gtk::GestureClick::new();
                click.set_button(3);
                click.connect_pressed(|gesture, _, x, y| {
                    let Some(expander) = gesture.widget().and_downcast::<gtk::TreeExpander>() else {
                        return;
                    };
                    let Some(node) = expander.list_row().and_then(|row| node_of(&row)) else {
                        return;
                    };
                    let Some(tree) = expander.ancestor(super::FsTree::static_type()).and_downcast::<super::FsTree>()
                    else {
                        return;
                    };
                    let point = graphene::Point::new(x as f32, y as f32);
                    let (x, y) = expander
                        .compute_point(&tree, &point)
                        .map_or((x, y), |p| (f64::from(p.x()), f64::from(p.y())));
                    tree.emit_by_name::<()>("item-context-requested", &[&node, &x, &y]);
                });
                expander.add_controller(click);
            });
            let tree = obj.downgrade();
            factory.connect_bind(move |_, obj| {
                let item = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
                let Some(row) = item
                    .item()
                    .and_then(|o| o.downcast::<gtk::TreeListRow>().ok())
                else {
                    return;
                };
                let Some(node) = super::node_of(&row) else { return };
                let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
                    return;
                };
                expander.set_list_row(Some(&row));
                if let Some(tree) = tree.upgrade() {
                    super::style_row(&tree, &expander, row.depth(), &node);
                }
            });
            self.list_view.set_factory(Some(&factory));
        }
    }

    impl WidgetImpl for FsTree {}
    impl BoxImpl for FsTree {}
}

glib::wrapper! {
    pub struct FsTree(ObjectSubclass<imp::FsTree>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FsTree {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Shows `path` as the header, with its subdirectories as the top-level rows.
    /// Entries load in the background.
    pub fn set_root_path(&self, path: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(path.is_dir(), "{} is not a directory", path.display());
        let imp = self.imp();

        let show_hidden = imp.show_hidden.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            show_hidden.get() || obj.downcast_ref::<FsNode>().is_none_or(|node| !node.is_hidden())
        });

        let root = FsNode::new(path.to_path_buf(), true);
        let store = gio::ListStore::new::<FsNode>();
        store.append(&root);

        // Called for each visible row; starting the load here is idempotent.
        let model = gtk::TreeListModel::new(store, false, false, {
            let filter = filter.clone();
            move |item| {
                let node = item.downcast_ref::<FsNode>()?;
                if !node.is_dir() {
                    return None;
                }
                node.start_loading();
                let visible = gtk::FilterListModel::new(Some(node.children()), Some(filter.clone()));
                Some(visible.upcast())
            }
        });

        if let Some(root_row) = model.row(0) {
            root_row.set_expanded(true);
        }
        // Selection only follows the user: nothing is selected until a row is clicked.
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let weak = self.downgrade();
        selection.connect_selected_item_notify(move |selection| {
            let Some(tree) = weak.upgrade() else { return };
            let path = selection
                .selected_item()
                .and_then(|item| item.downcast::<gtk::TreeListRow>().ok())
                .and_then(|row| node_of(&row))
                .map(|node| node.path().to_string_lossy().into_owned());
            if let Some(path) = path {
                tree.emit_by_name::<()>("location-selected", &[&path]);
            }
        });
        imp.list_view.set_model(Some(&selection));
        imp.selection.replace(Some(selection));
        imp.root.replace(Some(root));
        imp.tree_model.replace(Some(model));
        imp.filter.replace(Some(filter));
        self.refresh_root_rows();
        Ok(())
    }

    pub fn show_hidden(&self) -> bool {
        self.imp().show_hidden.get()
    }

    /// Calls `callback(tree, path)` when the user selects a row, with that row's absolute path.
    pub fn connect_location_selected<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, &Path) + 'static,
    {
        self.connect_local("location-selected", false, move |values| {
            let tree = values[0].get::<FsTree>().expect("tree argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&tree, Path::new(&path));
            None
        })
    }

    /// Selects the row for `path` if it is visible, as a user selection does: the view does not scroll.
    /// Returns whether it was found.
    pub fn select_path(&self, path: &Path) -> bool {
        self.select_row(path, false)
    }

    /// Selects the row for `path`. With `frame`, the view also scrolls to show the row, its parent,
    /// and as many of its children as fit (used when the selection follows a change of path).
    fn select_row(&self, path: &Path, frame: bool) -> bool {
        let imp = self.imp();
        let (Some(model), Some(selection)) = (imp.tree_model.borrow().clone(), imp.selection.borrow().clone())
        else {
            return false;
        };
        let found = (0..model.n_items()).find(|&position| {
            model
                .row(position)
                .and_then(|row| node_of(&row))
                .is_some_and(|node| node.path() == path)
        });
        match found {
            Some(position) => {
                selection.set_selected(position);
                if frame {
                    imp.list_view.scroll_to(position, gtk::ListScrollFlags::NONE, None);
                    self.frame_selection_after_layout(model, position, path.to_path_buf());
                }
                true
            }
            None => false,
        }
    }

    /// Connects to right-clicks on rows: `callback(tree, node, x, y)`, where `x` and `y` are in the
    /// tree's coordinates. The application decides what the context menu contains.
    pub fn connect_item_context_requested<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, &FsNode, f64, f64) + 'static,
    {
        self.connect_local("item-context-requested", false, move |values| {
            let tree = values[0].get::<FsTree>().expect("tree argument");
            let node = values[1].get::<FsNode>().expect("node argument");
            let x = values[2].get::<f64>().expect("x argument");
            let y = values[3].get::<f64>().expect("y argument");
            callback(&tree, &node, x, y);
            None
        })
    }

    /// Shows or hides dot-prefixed directories. The top visible row keeps its place on screen.
    pub fn set_show_hidden(&self, show: bool) {
        let imp = self.imp();
        if imp.show_hidden.get() == show {
            return;
        }

        let anchor = capture_anchor(&imp.list_view);
        imp.show_hidden.set(show);
        if let Some(filter) = imp.filter.borrow().as_ref() {
            filter.changed(gtk::FilterChange::Different);
        }
        self.restyle_rows();
        if let Some(anchor) = anchor {
            self.restore_after_layout(anchor);
        }
    }

    /// Expands every directory from the root down to `path`, loading each level asynchronously.
    /// With `collapse_others`, expanded rows outside that chain are collapsed first.
    pub fn expand_to_path(&self, path: &Path, collapse_others: bool) {
        self.expand_chain_to(path, collapse_others, true);
    }

    /// Expands to `path`'s parents only, without expanding `path` itself, and without selecting or
    /// scrolling: the user has already selected the folder in the tree.
    pub fn reveal_path_selecting_only(&self, path: &Path) {
        self.expand_chain_to(path, self.collapse_on_navigate(), false);
    }

    fn expand_chain_to(&self, path: &Path, collapse_others: bool, expand_target: bool) {
        let imp = self.imp();
        let (Some(model), Some(root)) = (imp.tree_model.borrow().clone(), imp.root.borrow().clone())
        else {
            return;
        };
        if !path.is_absolute() || !path.starts_with(root.path()) {
            return;
        }

        // Ancestors from the root down to `path`, inclusive.
        let mut chain: Vec<PathBuf> = path
            .ancestors()
            .take_while(|p| p.starts_with(root.path()))
            .map(Path::to_path_buf)
            .collect();
        chain.reverse();

        if collapse_others {
            collapse_except(&model, &chain);
        }
        let generation = imp.reveal_generation.get() + 1;
        imp.reveal_generation.set(generation);
        let tree = self.clone();
        let target = path.to_path_buf();
        glib::spawn_future_local(async move {
            expand_chain(model, root, chain, expand_target).await;
            // A later navigation has taken over; its reveal will select its own folder. A selection
            // the user made in the tree is already selected, so nothing is selected or scrolled then.
            if expand_target && tree.imp().reveal_generation.get() == generation {
                tree.select_row(&target, true);
            }
        });
    }

    /// Shows `path` in the tree: expands to it, selects it and scrolls it into view. Other folders
    /// are collapsed only if `collapse-on-navigate` is set.
    pub fn reveal_path(&self, path: &Path) {
        self.expand_to_path(path, self.collapse_on_navigate());
    }

    /// The path of the selected row, if any.
    pub fn selected_path(&self) -> Option<PathBuf> {
        let selection = self.imp().selection.borrow().clone()?;
        selection
            .selected_item()
            .and_then(|item| item.downcast::<gtk::TreeListRow>().ok())
            .and_then(|row| node_of(&row))
            .map(|node| node.path().to_path_buf())
    }

    /// Whether the row for `path` is expanded. False if it is not visible.
    pub fn is_expanded(&self, path: &Path) -> bool {
        let model = self.imp().tree_model.borrow().clone();
        model
            .and_then(|model| find_row(&model, path))
            .is_some_and(|row| row.is_expanded())
    }

    /// Toggles the row at `position` if it shows an arrow.
    fn toggle_row_at(&self, position: u32) {
        let Some(row) = self
            .imp()
            .list_view
            .model()
            .and_then(|model| model.item(position))
            .and_then(|item| item.downcast::<gtk::TreeListRow>().ok())
        else {
            return;
        };
        let Some(node) = node_of(&row) else { return };
        if arrow_visible(self, &node) {
            let anchor = capture_row_anchor(&self.imp().list_view, node.path());
            row.set_expanded(!row.is_expanded());
            if let Some(anchor) = anchor {
                self.restore_after_layout(anchor);
            }
        }
    }

    /// Remembers where the row at `path` is, when an arrow press begins.
    fn capture_toggle_anchor(&self, path: &Path) {
        let anchor = capture_row_anchor(&self.imp().list_view, path);
        self.imp().pending_anchor.replace(anchor);
    }

    /// Once the arrow's release has toggled the row, puts it back where it was on screen.
    fn release_toggle_anchor(&self) {
        if let Some(anchor) = self.imp().pending_anchor.take() {
            self.restore_after_layout(anchor);
        }
    }

    /// Whether `node` has children, starting the background check the first time it is asked.
    /// `None` means the answer is not known yet; the row is restyled once it is.
    fn expandable_of(&self, node: &FsNode) -> Option<bool> {
        let known = node.expandable(self.show_hidden());
        if known.is_none() {
            let tree = self.clone();
            node.start_check(move || tree.restyle_rows());
        }
        known
    }

    /// Re-applies styling to every rendered row.
    fn restyle_rows(&self) {
        let mut child = self.imp().list_view.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Some(expander) = widget.first_child().and_downcast::<gtk::TreeExpander>() else {
                continue;
            };
            let Some(row) = expander.list_row() else { continue };
            let Some(node) = node_of(&row) else { continue };
            style_row(self, &expander, row.depth(), &node);
        }
    }

    /// Re-expands the root when it cannot collapse, then restyles the rows.
    fn refresh_root_rows(&self) {
        if !self.root_collapsible() {
            if let Some(root) = self.imp().tree_model.borrow().as_ref().and_then(|model| model.row(0)) {
                root.set_expanded(true);
            }
        }
        self.restyle_rows();
    }

    /// Two frames after a selection, once the list has laid it out, frames the selection.
    fn frame_selection_after_layout(&self, model: gtk::TreeListModel, position: u32, path: PathBuf) {
        let imp = self.imp();
        let list_view = (*imp.list_view).clone();
        let scroller = (*imp.scroller).clone();
        let frames = Cell::new(0u8);
        self.add_tick_callback(move |_, _| {
            frames.set(frames.get() + 1);
            if frames.get() < 2 {
                return glib::ControlFlow::Continue;
            }
            frame_selection(&list_view, &scroller, &model, position, &path);
            glib::ControlFlow::Break
        });
    }

    /// Restores the anchor once the list has laid out the change. Layout can take a few frames
    /// (and the scroll may be clamped along the way), so the anchor is re-applied for a short while;
    /// once the row is where it was, re-applying changes nothing.
    fn restore_after_layout(&self, anchor: Anchor) {
        const FRAMES: u8 = 8;
        let imp = self.imp();
        let list_view = (*imp.list_view).clone();
        let scroller = (*imp.scroller).clone();
        let frames = Cell::new(0u8);
        self.add_tick_callback(move |_, _| {
            frames.set(frames.get() + 1);
            if frames.get() < 2 {
                return glib::ControlFlow::Continue;
            }
            restore_anchor(&list_view, &scroller, &anchor);
            if frames.get() < FRAMES {
                return glib::ControlFlow::Continue;
            }
            glib::ControlFlow::Break
        });
    }
}

/// Whether `node` is the root directory, which is the tree's first row.
fn is_root(tree: &FsTree, node: &FsNode) -> bool {
    tree.imp().root.borrow().as_ref().is_some_and(|root| root.path() == node.path())
}

/// Whether a row shows its expander arrow: only for directories that have children to show, and
/// the root only when it can collapse.
fn arrow_visible(tree: &FsTree, node: &FsNode) -> bool {
    if is_root(tree, node) && !tree.root_collapsible() {
        return false;
    }
    node.is_dir() && tree.expandable_of(node) != Some(false)
}

/// Applies per-row presentation: arrow, label (friendly for the root), and dimming for hidden
/// directories.
fn style_row(tree: &FsTree, expander: &gtk::TreeExpander, depth: u32, node: &FsNode) {
    expander.set_hide_expander(!arrow_visible(tree, node));
    // The custom indent replaces GTK's per-depth indent, which would also indent the root's children.
    expander.set_indent_for_depth(false);
    expander.set_margin_start(row_indent(depth));
    expander.set_tooltip_text(Some(&node.path().to_string_lossy()));
    // A pinned root drops the arrow column entirely; other rows keep it so labels stay aligned.
    let root = is_root(tree, node);
    expander.set_indent_for_icon(!(root && !tree.root_collapsible()));

    if let Some(content) = expander.child().and_downcast::<gtk::Box>() {
        if let (Some(icon), Some(label)) = (
            content.first_child().and_downcast::<gtk::Image>(),
            content.last_child().and_downcast::<gtk::Label>(),
        ) {
            let friendly = match tree.root_display() {
                RootDisplay::Friendly if root => friendly_root(node.path()),
                _ => None,
            };
            match friendly {
                Some((icon_name, text)) => {
                    icon.set_icon_name(Some(icon_name));
                    label.set_label(&text);
                }
                None => {
                    icon.set_icon_name(Some("folder-symbolic"));
                    label.set_label(&node.name());
                }
            }
        }
    }

    if node.is_hidden() {
        expander.add_css_class(HIDDEN_CLASS);
    } else {
        expander.remove_css_class(HIDDEN_CLASS);
    }
}

/// Friendly label and icon for the home directory and `/`; `None` for anything else.
fn friendly_root(path: &Path) -> Option<(&'static str, String)> {
    if path == glib::home_dir().as_path() {
        let user = glib::user_name().to_string_lossy().into_owned();
        return Some(("user-home-symbolic", user));
    }
    if path == Path::new("/") {
        let os = glib::os_info("PRETTY_NAME")?;
        return Some(("drive-harddisk-symbolic", os.to_string()));
    }
    None
}

/// The tree that contains `widget`, and the path of the row `widget` shows.
fn tree_and_path(widget: &impl IsA<gtk::Widget>) -> Option<(FsTree, PathBuf)> {
    let tree = widget.ancestor(FsTree::static_type()).and_downcast::<FsTree>()?;
    let expander = widget.as_ref().downcast_ref::<gtk::TreeExpander>()?;
    let path = expander.list_row().and_then(|row| node_of(&row))?.path().to_path_buf();
    Some((tree, path))
}

fn node_of(row: &gtk::TreeListRow) -> Option<FsNode> {
    row.item()?.downcast().ok()
}

fn find_row(model: &gtk::TreeListModel, path: &Path) -> Option<gtk::TreeListRow> {
    (0..model.n_items())
        .filter_map(|i| model.row(i))
        .find(|row| node_of(row).is_some_and(|node| node.path() == path))
}

fn expand_row(model: &gtk::TreeListModel, node: &FsNode) {
    if let Some(row) = find_row(model, node.path()) {
        row.set_expanded(true);
    }
}

/// Walks down the chain, waiting for each directory to load before expanding it. The last entry
/// is the target, which is expanded only if `expand_target` is set.
/// The root is the first row, so it is expanded like any other directory on the way down.
async fn expand_chain(model: gtk::TreeListModel, root: FsNode, chain: Vec<PathBuf>, expand_target: bool) {
    let mut node = root;
    for next in chain.into_iter().skip(1) {
        node.loaded().await;
        expand_row(&model, &node);
        let Some(child) = node.child_with_path(&next) else {
            return;
        };
        node = child;
    }
    if expand_target {
        node.loaded().await;
        expand_row(&model, &node);
    }
}

/// Collapses expanded rows not on `keep`. Walks bottom-up so that row
/// indices above the current one stay valid after each collapse.
fn collapse_except(model: &gtk::TreeListModel, keep: &[PathBuf]) {
    for i in (0..model.n_items()).rev() {
        let Some(row) = model.row(i) else { continue };
        if !row.is_expanded() {
            continue;
        }
        let kept = node_of(&row).is_some_and(|node| keep.iter().any(|p| p == node.path()));
        if !kept {
            row.set_expanded(false);
        }
    }
}

/// Scrolls so the selected row's parent is at the top, with the selection and as many of its
/// children as fit below. If the selection and its children are taller than the view, the
/// selection itself goes to the top. Rows are assumed to share one height, as they do here.
fn frame_selection(
    list_view: &gtk::ListView,
    scroller: &gtk::ScrolledWindow,
    model: &gtk::TreeListModel,
    position: u32,
    path: &Path,
) {
    let Some(selected) = model.row(position) else {
        return;
    };
    let depth = selected.depth();

    // The parent is the nearest row above with a shallower depth; the children are the deeper rows below.
    let parent = if depth == 0 {
        None
    } else {
        (0..position).rev().find(|&index| model.row(index).is_some_and(|row| row.depth() < depth))
    };
    let mut last = position;
    while model.row(last + 1).is_some_and(|row| row.depth() > depth) {
        last += 1;
    }
    let top = parent.unwrap_or(position);

    let Some((selected_y, row_height)) = rendered_geometry(list_view, scroller, path) else {
        return;
    };
    let adjustment = scroller.vadjustment();
    let span = f64::from(last - top + 1) * row_height;
    // Viewport offset of the row to bring to the top.
    let target_y = if span <= adjustment.page_size() {
        selected_y + (f64::from(top) - f64::from(position)) * row_height
    } else {
        selected_y
    };
    adjustment.set_value(adjustment.value() + target_y);
}

/// The vertical position (within the scroller's visible area) and height of the rendered row for `path`.
fn rendered_geometry(
    list_view: &gtk::ListView,
    scroller: &gtk::ScrolledWindow,
    path: &Path,
) -> Option<(f64, f64)> {
    let mut child = list_view.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Some(expander) = widget.first_child().and_downcast::<gtk::TreeExpander>() else {
            continue;
        };
        let Some(node) = expander.list_row().and_then(|row| node_of(&row)) else {
            continue;
        };
        if node.path() == path {
            let bounds = widget.compute_bounds(scroller)?;
            return Some((f64::from(bounds.y()), f64::from(bounds.height())));
        }
    }
    None
}

/// Where the rendered row for `path` sits, with every rendered row's position for the restore.
fn capture_row_anchor(list_view: &gtk::ListView, path: &Path) -> Option<Anchor> {
    let rows = visible_rows(list_view);
    let y = rows.iter().find(|(row_path, _)| row_path == path)?.1;
    Some(Anchor {
        path: path.to_path_buf(),
        y,
        rows: rows.into_iter().collect(),
    })
}

/// The row nearest the top edge of the list, plus where every rendered row sits.
struct Anchor {
    path: PathBuf,
    y: f64,
    rows: HashMap<PathBuf, f64>,
}

/// Rendered rows and their vertical position within the visible area. Positions are measured
/// against the scroller, not the list, so a scroll that the layout clamps shows up as a move.
fn visible_rows(list_view: &gtk::ListView) -> Vec<(PathBuf, f64)> {
    let Some(scroller) = list_view.parent() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut child = list_view.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Some(expander) = widget.first_child().and_downcast::<gtk::TreeExpander>() else {
            continue;
        };
        let Some(node) = expander.list_row().and_then(|row| node_of(&row)) else {
            continue;
        };
        let Some(bounds) = widget.compute_bounds(&scroller) else {
            continue;
        };
        rows.push((node.path().to_path_buf(), f64::from(bounds.y())));
    }
    rows
}

fn capture_anchor(list_view: &gtk::ListView) -> Option<Anchor> {
    let rows = visible_rows(list_view);
    // Prefer the last row at or above the top edge; otherwise the first row below it.
    let (path, y) = rows
        .iter()
        .filter(|(_, y)| *y <= 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .or_else(|| rows.iter().min_by(|a, b| a.1.total_cmp(&b.1)))?
        .clone();
    Some(Anchor {
        path,
        y,
        rows: rows.into_iter().collect(),
    })
}

/// Scrolls so the anchor (or its nearest surviving ancestor) sits where it did before.
fn restore_anchor(list_view: &gtk::ListView, scroller: &gtk::ScrolledWindow, anchor: &Anchor) {
    let now: HashMap<PathBuf, f64> = visible_rows(list_view).into_iter().collect();
    for candidate in anchor.path.ancestors() {
        let Some(&new_y) = now.get(candidate) else {
            continue;
        };
        // An ancestor that was not rendered before takes the anchor's old position.
        let old_y = anchor.rows.get(candidate).copied().unwrap_or(anchor.y);
        let adjustment = scroller.vadjustment();
        adjustment.set_value(adjustment.value() + (new_y - old_y));
        return;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use gtk::gio;

    use super::*;

    pub(crate) fn init_gtk() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let resource = gio::Resource::from_data(&glib::Bytes::from_static(crate::RESOURCES))
                .expect("embedded resources");
            gio::resources_register(&resource);
            gtk::init().expect("GTK needs a display (try xvfb-run)");
        });
    }

    /// Pumps the default main context until `cond` holds or five seconds pass.
    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        let ctx = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for: {what}");
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn model_of(tree: &FsTree) -> Option<gtk::TreeListModel> {
        tree.imp().tree_model.borrow().clone()
    }

    fn row_exists(tree: &FsTree, path: &Path) -> bool {
        model_of(tree).and_then(|model| find_row(&model, path)).is_some()
    }

    fn is_expanded(tree: &FsTree, path: &Path) -> bool {
        model_of(tree)
            .and_then(|model| find_row(&model, path))
            .is_some_and(|row| row.is_expanded())
    }

    fn node_at(tree: &FsTree, path: &Path) -> Option<FsNode> {
        model_of(tree)
            .and_then(|model| find_row(&model, path))
            .and_then(|row| node_of(&row))
    }

    /// Asks the widget for a node's expandability, as rendering does; that starts the check.
    fn expandability(tree: &FsTree, path: &Path) -> Option<bool> {
        let node = node_at(tree, path)?;
        tree.expandable_of(&node)
    }

    fn position_of(tree: &FsTree, path: &Path) -> Option<u32> {
        let model = model_of(tree)?;
        (0..model.n_items()).find(|&i| {
            model
                .row(i)
                .and_then(|row| node_of(&row))
                .is_some_and(|node| node.path() == path)
        })
    }

    fn context_request_reports_node() {
        let root = temp_root("fs-tree-context");
        fs::create_dir_all(root.join("docs")).unwrap();

        let tree = FsTree::new();
        tree.set_root_path(&root).unwrap();
        wait_for("docs listed", || row_exists(&tree, &root.join("docs")));

        let seen = Rc::new(RefCell::new(None));
        let sink = seen.clone();
        tree.connect_item_context_requested(move |_, node, x, y| {
            *sink.borrow_mut() = Some((node.path().to_path_buf(), x, y));
        });
        tree.emit_by_name::<()>("item-context-requested", &[&node_at(&tree, &root.join("docs")).unwrap(), &4.0f64, &9.0f64]);
        assert_eq!(*seen.borrow(), Some((root.join("docs"), 4.0, 9.0)));

        let _ = fs::remove_dir_all(&root);
    }

    fn indent_skips_root_children() {
        assert_eq!(row_indent(0), 0, "the root");
        assert_eq!(row_indent(1), 0, "items directly in the root folder are not indented");
        assert_eq!(row_indent(2), LEVEL_INDENT, "one level below the root's items");
        assert_eq!(row_indent(3), 2 * LEVEL_INDENT);
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rsimgv-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn expands_to_path_and_collapses_others() {
        let root = temp_root("fs-tree-expand");
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::create_dir_all(root.join("other/x")).unwrap();
        fs::write(root.join("a/b/c/file.txt"), "").unwrap();

        let tree = FsTree::new();
        tree.set_root_path(&root).unwrap();
        wait_for("top level listed", || row_exists(&tree, &root.join("a")));
        assert!(is_expanded(&tree, &root), "root starts expanded");

        // Deep expansion: every ancestor of the target opens, loading as it goes.
        tree.expand_to_path(&root.join("a/b/c/file.txt"), false);
        wait_for("a/b/c expanded", || is_expanded(&tree, &root.join("a/b/c")));
        assert!(is_expanded(&tree, &root.join("a")));
        assert!(is_expanded(&tree, &root.join("a/b")));

        // Open `other`, then reveal `a/b/c` with collapse_others: `other` must close.
        tree.expand_to_path(&root.join("other/x"), false);
        wait_for("other/x expanded", || is_expanded(&tree, &root.join("other/x")));
        tree.expand_to_path(&root.join("a/b/c"), true);
        wait_for("a/b/c expanded again", || is_expanded(&tree, &root.join("a/b/c")));
        assert!(!is_expanded(&tree, &root.join("other")), "other should be collapsed");
        assert!(!is_expanded(&tree, &root.join("other/x")), "other/x should be collapsed");

        let _ = fs::remove_dir_all(&root);
    }

    fn toggles_hidden_directories() {
        let root = temp_root("fs-tree-hidden");
        fs::create_dir_all(root.join("visible")).unwrap();
        fs::create_dir_all(root.join(".cache")).unwrap();

        let tree = FsTree::new();
        tree.set_root_path(&root).unwrap();
        wait_for("visible listed", || row_exists(&tree, &root.join("visible")));
        assert!(!tree.show_hidden());
        assert!(!row_exists(&tree, &root.join(".cache")), "hidden by default");

        tree.set_show_hidden(true);
        wait_for(".cache shown", || row_exists(&tree, &root.join(".cache")));

        tree.set_show_hidden(false);
        assert!(!row_exists(&tree, &root.join(".cache")), "hidden again after toggle off");
        assert!(row_exists(&tree, &root.join("visible")));

        let _ = fs::remove_dir_all(&root);
    }

    fn root_properties() {
        let root = temp_root("fs-tree-root-props");
        fs::create_dir_all(root.join("child")).unwrap();

        let tree = FsTree::new();
        assert!(!tree.root_collapsible(), "root is not collapsible by default");
        assert_eq!(tree.root_display(), RootDisplay::Friendly, "friendly is the default");

        tree.set_root_path(&root).unwrap();
        wait_for("child listed", || row_exists(&tree, &root.join("child")));
        assert!(is_expanded(&tree, &root), "the root row starts expanded");

        // A collapsible root can be collapsed, which hides its children.
        tree.set_root_collapsible(true);
        model_of(&tree).unwrap().row(0).unwrap().set_expanded(false);
        assert!(!is_expanded(&tree, &root), "root collapses when allowed");
        assert!(!row_exists(&tree, &root.join("child")), "children hide with the root");

        // Making the root non-collapsible expands it again.
        tree.set_root_collapsible(false);
        assert!(is_expanded(&tree, &root), "root re-expands when not collapsible");

        assert!(friendly_root(&glib::home_dir()).is_some_and(|(icon, _)| icon == "user-home-symbolic"));
        assert!(friendly_root(Path::new("/")).is_some());
        assert!(friendly_root(&root).is_none(), "other roots keep their directory name");

        let _ = fs::remove_dir_all(&root);
    }

    /// Arrows follow whether a directory has children, and double-click toggles expansion.
    fn arrows_and_double_click() {
        let root = temp_root("fs-tree-arrows");
        fs::create_dir_all(root.join("parent/sub")).unwrap();
        fs::create_dir_all(root.join("leaf")).unwrap();
        fs::write(root.join("leaf/file.txt"), "").unwrap();
        fs::create_dir_all(root.join("hiddenonly/.secret")).unwrap();

        let tree = FsTree::new();
        tree.set_root_path(&root).unwrap();

        wait_for("parent listed", || expandability(&tree, &root.join("parent")).is_some());
        wait_for("leaf listed", || expandability(&tree, &root.join("leaf")).is_some());
        wait_for("hiddenonly listed", || expandability(&tree, &root.join("hiddenonly")).is_some());

        let node = |path: &Path| node_at(&tree, path).unwrap();

        assert!(arrow_visible(&tree, &node(&root.join("parent"))));
        assert!(!arrow_visible(&tree, &node(&root.join("leaf"))));

        // Only hidden subdirectories: no arrow until hidden directories are shown.
        let hidden_only = root.join("hiddenonly");
        assert!(!arrow_visible(&tree, &node(&hidden_only)));
        tree.set_show_hidden(true);
        assert!(arrow_visible(&tree, &node(&hidden_only)));
        tree.set_show_hidden(false);

        // Double-click (activate) toggles a row with an arrow, and ignores one without.
        let parent_pos = position_of(&tree, &root.join("parent")).unwrap();
        tree.toggle_row_at(parent_pos);
        assert!(is_expanded(&tree, &root.join("parent")), "double-click expands");
        tree.toggle_row_at(parent_pos);
        assert!(!is_expanded(&tree, &root.join("parent")), "double-click collapses");

        let leaf_pos = position_of(&tree, &root.join("leaf")).unwrap();
        tree.toggle_row_at(leaf_pos);
        assert!(!is_expanded(&tree, &root.join("leaf")), "leaf has no arrow to toggle");

        let _ = fs::remove_dir_all(&root);
    }

    /// Runs the main context for about `frames` frames, so layout and frame callbacks happen.
    fn pump_frames(frames: u32) {
        let ctx = glib::MainContext::default();
        for _ in 0..frames {
            while ctx.iteration(false) {}
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// A parent with many children before the target, and the target with a few children, in a
    /// view too short to show everything. The reveal should put the parent at the top and show
    /// the target's children.
    fn frames_selection_with_its_children() {
        let root = temp_root("fs-tree-frame");
        for i in 0..40 {
            fs::create_dir_all(root.join(format!("parent/d{i:02}"))).unwrap();
        }
        for i in 0..5 {
            fs::create_dir_all(root.join(format!("parent/zsel/c{i}"))).unwrap();
        }

        let tree = FsTree::new();
        let window = gtk::Window::new();
        window.set_default_size(300, 300);
        window.set_child(Some(&tree));
        window.present();
        tree.set_root_path(&root).unwrap();
        tree.reveal_path(&root.join("parent/zsel"));
        wait_for("zsel selected", || tree.selected_path() == Some(root.join("parent/zsel")));
        pump_frames(120);

        let imp = tree.imp();
        let geometry = |path: &Path| {
            rendered_geometry(&imp.list_view, &imp.scroller, path).expect("row is rendered")
        };
        let (_, row_height) = geometry(&root.join("parent/zsel"));
        let (parent_y, _) = geometry(&root.join("parent"));
        let (last_child_y, _) = geometry(&root.join("parent/zsel/c4"));
        let page = imp.scroller.vadjustment().page_size();
        assert!(parent_y.abs() < row_height, "the parent is at the top of the view (y = {parent_y})");
        assert!(last_child_y + row_height <= page + 0.5, "the last child is visible (y = {last_child_y})");

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// Collapsing a folder at the end of a scrolled list must not move it on screen: the content
    /// shrinks, which would otherwise clamp the scroll and shift every row.
    fn toggle_keeps_row_in_place() {
        let root = temp_root("fs-tree-toggle");
        for i in 0..20 {
            fs::create_dir_all(root.join(format!("parent/d{i:02}"))).unwrap();
        }
        for i in 0..6 {
            fs::create_dir_all(root.join(format!("parent/zlast/x{i}"))).unwrap();
        }

        let tree = FsTree::new();
        let window = gtk::Window::new();
        window.set_default_size(300, 300);
        window.set_child(Some(&tree));
        window.present();
        tree.set_root_path(&root).unwrap();
        tree.reveal_path(&root.join("parent/zlast"));
        wait_for("zlast expanded", || tree.is_expanded(&root.join("parent/zlast")));
        pump_frames(30);

        let imp = tree.imp();
        let scroller = imp.scroller.clone();
        let adjustment = scroller.vadjustment();
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        pump_frames(30);

        let geometry = |path: &Path| rendered_geometry(&imp.list_view, &imp.scroller, path).expect("row is rendered").0;
        let before = geometry(&root.join("parent/zlast"));
        tree.toggle_row_at(position_of(&tree, &root.join("parent/zlast")).unwrap());
        pump_frames(30);
        let after = geometry(&root.join("parent/zlast"));
        assert!((after - before).abs() < 1.0, "the folder stayed in place (moved from {before} to {after})");

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    pub(crate) fn run_scenarios() {
        toggle_keeps_row_in_place();
        frames_selection_with_its_children();
        context_request_reports_node();
        indent_skips_root_children();
        expands_to_path_and_collapses_others();
        toggles_hidden_directories();
        root_properties();
        arrows_and_double_click();
    }
}
