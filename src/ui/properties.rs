//! The properties dialog for a selection of files and folders, laid out as Nautilus's: the icon or thumbnail, the
//! name, what kind of thing it is, its size, or for a folder (or several things) what it holds, counted in the
//! background; where it is, when it changed, and a page of permissions to read and change. The dialog follows the
//! files while it is open: a change on disk updates it, and it closes once every file is gone.
//!
//! What it shows is worked out in `fs::properties`; this draws it.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::fs::metadata::{format_fps, MediaInfo};
use crate::fs::properties::{
    access_choices, access_text, bytes_text, class_of, contents_text, count_contents, free_space, modes_for_access,
    modes_for_executable, name_summary, set_mode, shared_content_type, size_text, summarize_permissions, time_text, type_text,
    Counts, Executable, FileFacts, Kind, Who,
};
use crate::fs::thumbnail::{Media as ThumbnailMedia, Size as ThumbnailSize, Thumbnails};
use crate::storage::watch_history::format_duration;

/// The width of the dialog, as Nautilus's.
const WIDTH: i32 = 480;
/// The size of the icon or thumbnail at the top, in pixels.
const ICON_SIZE: i32 = 96;
/// How long after a change on disk the dialog is brought up to date, so that a burst of changes is one update.
const REFRESH_DELAY: Duration = Duration::from_millis(200);
/// How often the count of a folder's contents is shown while it runs.
const COUNT_INTERVAL: Duration = Duration::from_millis(100);

/// One choice of what a class can do with the files or the folders.
struct AccessRow {
    row: adw::ComboRow,
    who: Who,
    kind: Kind,
}

/// The rows of the permissions page, which depend on whether the selection has files, folders, or both.
struct PermissionRows {
    layout: (bool, bool),
    access: Vec<AccessRow>,
    owner: adw::ActionRow,
    group: adw::ActionRow,
    banner: adw::Banner,
}

/// The dialog for one selection.
pub struct Properties {
    dialog: glib::WeakRef<adw::Dialog>,
    nav: adw::NavigationView,
    toasts: adw::ToastOverlay,
    paths: Vec<PathBuf>,
    /// The folder the browse view shows, whose own listing says where its files are, so no parent row is needed.
    current_folder: Option<PathBuf>,
    open: Rc<dyn Fn(&Path)>,
    facts: RefCell<Vec<FileFacts>>,
    /// The bytes free where the first folder is, for a selection of folders.
    free: Cell<Option<u64>>,

    icon: gtk::Image,
    name: gtk::Label,
    kind: gtk::Label,
    size: gtk::Label,
    contents_box: gtk::Box,
    contents: gtk::Label,
    spinner: adw::Spinner,
    free_space: gtk::Label,
    locations: adw::PreferencesGroup,
    link_row: adw::ActionRow,
    parent_row: adw::ActionRow,
    times: adw::PreferencesGroup,
    accessed_row: adw::ActionRow,
    modified_row: adw::ActionRow,
    created_row: adw::ActionRow,
    media: adw::PreferencesGroup,
    media_rows: [adw::ActionRow; 5],
    permissions_group: adw::PreferencesGroup,
    permissions_value: gtk::Label,
    execute_row: adw::SwitchRow,
    permissions_content: gtk::Box,
    permission_rows: RefCell<Option<PermissionRows>>,
    /// Set while the dialog changes its own rows, so that a row changed by it is not taken for the user's choice.
    updating: Cell<bool>,

    /// Stops the count of what the folders hold, when it is started again or the dialog closes.
    count_cancel: RefCell<Arc<AtomicBool>>,
    /// What each file looked like when the contents were counted, to count again when one has changed.
    counted: RefCell<Vec<(Option<i64>, u64)>>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    refresh_generation: Cell<u64>,
    /// The thumbnail asked for, so a changed file asks again and an answer for an older one is dropped.
    thumbnail_for: RefCell<Option<(PathBuf, Option<i64>)>>,
}

/// Shows the properties of `paths` over `parent`. `current_folder` is the folder being browsed, and `open` takes the
/// dialog's user to a folder it links to. Returns the dialog's state, which a test looks into.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    paths: Vec<PathBuf>,
    current_folder: Option<PathBuf>,
    open: impl Fn(&Path) + 'static,
) -> Rc<Properties> {
    let this = Properties::new(paths, current_folder, Rc::new(open));
    let dialog = adw::Dialog::builder().title("Properties").content_width(WIDTH).child(&this.toasts).build();
    this.dialog.set(Some(&dialog));
    // The dialog keeps the state alive, and the state only knows the dialog weakly, so neither holds the other.
    dialog.connect_closed({
        let this = this.clone();
        move |_| this.finish()
    });
    dialog.present(Some(parent));
    this.load();
    this
}

impl Properties {
    fn new(paths: Vec<PathBuf>, current_folder: Option<PathBuf>, open: Rc<dyn Fn(&Path)>) -> Rc<Self> {
        let label = |class: &str| {
            let label = gtk::Label::builder()
                .selectable(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .visible(false)
                .build();
            if !class.is_empty() {
                label.add_css_class(class);
            }
            label
        };
        let name = gtk::Label::builder()
            .selectable(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(3)
            .max_width_chars(24)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .justify(gtk::Justification::Center)
            .build();
        name.add_css_class("title-3");
        let kind = label("");
        let size = label("caption");
        let contents = label("");
        contents.set_visible(true);
        contents.set_justify(gtk::Justification::Center);
        let spinner = adw::Spinner::new();
        spinner.set_visible(false);
        let contents_box = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).halign(gtk::Align::Center).spacing(12).visible(false).build();
        contents_box.append(&contents);
        contents_box.append(&spinner);
        let free_space = label("caption");
        let icon = gtk::Image::builder().pixel_size(ICON_SIZE).halign(gtk::Align::Center).build();
        icon.add_css_class("icon-dropshadow");

        let property = |title: &str| {
            let row = adw::ActionRow::builder().title(title).selectable(false).subtitle_selectable(true).subtitle_lines(1).visible(false).build();
            row.add_css_class("property");
            row.add_css_class("numeric");
            row
        };
        let link_row = property("Link Target");
        let parent_row = property("Parent Folder");
        for row in [&link_row, &parent_row] {
            row.remove_css_class("numeric");
            row.set_subtitle_lines(0);
            row.set_activatable(true);
            let open_button = gtk::Button::builder().icon_name("folder-open-symbolic").valign(gtk::Align::Center).build();
            open_button.add_css_class("flat");
            open_button.set_tooltip_text(Some(if row == &link_row { "Open the Target's Location" } else { "Open Parent Folder" }));
            row.add_suffix(&open_button);
            open_button.connect_clicked({
                let row = row.clone();
                move |_| {
                    ActionRowExt::activate(&row);
                }
            });
        }
        let accessed_row = property("Accessed");
        let modified_row = property("Modified");
        let created_row = property("Created");
        let media_rows = ["Dimensions", "Duration", "Frame Rate", "Codec", "Bit Rate"].map(|title| {
            let row = property(title);
            row.remove_css_class("numeric");
            row
        });

        let permissions_value = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).build();
        permissions_value.add_css_class("dim-label");
        let execute_row = adw::SwitchRow::builder().title("Executable as Program").visible(false).build();

        let permissions_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        Rc::new(Self {
            dialog: glib::WeakRef::new(),
            nav: adw::NavigationView::new(),
            toasts: adw::ToastOverlay::new(),
            paths,
            current_folder,
            open,
            facts: RefCell::default(),
            free: Cell::new(None),
            icon,
            name,
            kind,
            size,
            contents_box,
            contents,
            spinner,
            free_space,
            locations: adw::PreferencesGroup::builder().visible(false).build(),
            link_row,
            parent_row,
            times: adw::PreferencesGroup::builder().visible(false).build(),
            accessed_row,
            modified_row,
            created_row,
            media: adw::PreferencesGroup::builder().visible(false).build(),
            media_rows,
            permissions_group: adw::PreferencesGroup::new(),
            permissions_value,
            execute_row,
            permissions_content,
            permission_rows: RefCell::default(),
            updating: Cell::new(false),
            count_cancel: RefCell::default(),
            counted: RefCell::default(),
            monitors: RefCell::default(),
            refresh_generation: Cell::new(0),
            thumbnail_for: RefCell::default(),
        })
    }

    /// Reads the files and fills the dialog in. Until then it shows that it is loading.
    fn load(self: &Rc<Self>) {
        self.build_pages();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let (facts, free) = this.query().await;
            if facts.is_empty() {
                this.show_unreadable();
                return;
            }
            this.facts.replace(facts);
            this.free.set(free);
            this.update();
            this.nav.replace_with_tags(&["main"]);
            this.watch_files();
            this.start_count(true);
        });
    }

    /// Builds the three pages: loading, the properties, and the permissions.
    fn build_pages(self: &Rc<Self>) {
        let loading = adw::NavigationPage::builder()
            .title("Loading…")
            .tag("loading")
            .child(&{
                let toolbar = adw::ToolbarView::new();
                toolbar.add_top_bar(&adw::HeaderBar::new());
                toolbar.set_content(Some(&adw::StatusPage::builder().child(&adw::Spinner::builder().width_request(64).height_request(64).build()).build()));
                toolbar
            })
            .build();
        self.nav.push(&loading);

        // The top of the main page: icon, name, and what the name stands for.
        let top = gtk::Box::new(gtk::Orientation::Vertical, 12);
        top.append(&self.icon);
        let words = gtk::Box::new(gtk::Orientation::Vertical, 6);
        for widget in [self.name.upcast_ref::<gtk::Widget>(), self.kind.upcast_ref(), self.size.upcast_ref(), self.contents_box.upcast_ref(), self.free_space.upcast_ref()] {
            words.append(widget);
        }
        top.append(&words);
        let header = adw::PreferencesGroup::new();
        header.add(&top);

        self.locations.add(&self.link_row);
        self.locations.add(&self.parent_row);
        for row in [&self.accessed_row, &self.modified_row, &self.created_row] {
            self.times.add(row);
        }
        for row in &self.media_rows {
            self.media.add(row);
        }

        let permissions_row = adw::ActionRow::builder().title("Permissions").activatable(true).selectable(false).build();
        permissions_row.add_suffix(&self.permissions_value);
        permissions_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        permissions_row.connect_activated({
            let nav = self.nav.clone();
            move |_| nav.push_by_tag("permissions")
        });
        self.permissions_group.add(&permissions_row);
        self.permissions_group.add(&self.execute_row);
        self.execute_row.connect_active_notify({
            let weak = Rc::downgrade(self);
            move |row| {
                let Some(this) = weak.upgrade() else { return };
                if !this.updating.get() {
                    let modes = modes_for_executable(&this.facts.borrow(), row.is_active());
                    this.write_modes(modes);
                }
            }
        });

        let page = adw::PreferencesPage::new();
        for group in [&header, &self.locations, &self.times, &self.media, &self.permissions_group] {
            page.add(group);
        }
        let toolbar = adw::ToolbarView::new();
        let bar = adw::HeaderBar::new();
        bar.set_show_title(false);
        toolbar.add_top_bar(&bar);
        toolbar.set_content(Some(&page));
        self.nav.add(&adw::NavigationPage::builder().title("Properties").tag("main").child(&toolbar).build());

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&self.permissions_content));
        self.nav.add(&adw::NavigationPage::builder().title("Set Custom Permissions").tag("permissions").child(&toolbar).build());
        self.toasts.set_child(Some(&self.nav));

        self.link_row.connect_activated({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(this) = weak.upgrade() {
                    this.open_link_target();
                }
            }
        });
        self.parent_row.connect_activated({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(this) = weak.upgrade() {
                    this.open_parent_folder();
                }
            }
        });
    }

    /// Reads the facts of the files, and the free space of the first folder, on a worker thread. Files that cannot be
    /// read are left out.
    async fn query(&self) -> (Vec<FileFacts>, Option<u64>) {
        let paths = self.paths.clone();
        gio::spawn_blocking(move || {
            let facts: Vec<FileFacts> = paths.iter().filter_map(|path| FileFacts::query(path).ok()).collect();
            let free = facts.iter().all(|file| file.is_dir).then(|| facts.first().and_then(|file| free_space(&file.path))).flatten();
            (facts, free)
        })
        .await
        .unwrap_or_default()
    }

    /// Says that the files could not be read.
    fn show_unreadable(&self) {
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let status = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("Properties Unavailable")
            .description("The properties of the selected files could not be read")
            .build();
        toolbar.set_content(Some(&status));
        let page = adw::NavigationPage::builder().title("Properties").tag("unreadable").child(&toolbar).build();
        self.nav.push(&page);
    }

    /// Whether the selection is several files, which is told by how many were chosen and not how many remain.
    fn is_multi(&self) -> bool {
        self.paths.len() > 1
    }

    /// Brings every part of the dialog up to date with the facts.
    fn update(self: &Rc<Self>) {
        let facts = self.facts.borrow().clone();
        let Some(first) = facts.first() else { return };
        self.updating.set(true);
        let multi = self.is_multi();

        // Name and kind.
        let names: Vec<String> = facts.iter().map(|file| file.name.clone()).collect();
        let name = if !multi && first.path == Path::new("/") {
            glib::os_info("NAME").map_or_else(|| "Operating System".to_owned(), |name| name.to_string())
        } else {
            name_summary(&names)
        };
        self.name.set_text(&name);
        self.name.select_region(-1, -1);
        match shared_content_type(&facts).filter(|_| multi || !first.is_dir) {
            Some(content_type) => {
                self.kind.set_text(&type_text(content_type));
                self.kind.set_visible(true);
            }
            None => self.kind.set_visible(false),
        }
        if multi || first.is_dir {
            self.size.set_visible(false);
            self.contents_box.set_visible(true);
        } else {
            self.size.set_text(&size_text(first.size));
            self.size.set_visible(true);
            self.contents_box.set_visible(false);
        }
        self.update_icon(first);
        self.update_free_space();

        // Where it is.
        let link = (!multi && first.is_symlink).then(|| first.link_target.clone()).flatten();
        self.link_row.set_visible(link.is_some());
        self.link_row.set_subtitle(link.as_deref().unwrap_or_default());
        let parent = first.path.parent().filter(|parent| facts.iter().all(|file| file.path.parent() == Some(*parent)));
        let parent = parent.filter(|parent| self.current_folder.as_deref() != Some(*parent));
        self.parent_row.set_visible(parent.is_some());
        self.parent_row.set_subtitle(&parent.map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default());
        self.locations.set_visible(self.link_row.get_visible() || self.parent_row.get_visible());

        // When it changed.
        let show_time = |row: &adw::ActionRow, seconds: Option<i64>, wanted: bool| {
            let seconds = seconds.filter(|_| wanted);
            row.set_visible(seconds.is_some());
            row.set_subtitle(&seconds.map(time_text).unwrap_or_default());
        };
        show_time(&self.modified_row, first.modified, !multi);
        show_time(&self.created_row, first.created, !multi);
        // The time a folder was read says little, so Nautilus leaves it out.
        show_time(&self.accessed_row, first.accessed, !multi && !first.is_dir);
        self.times.set_visible([&self.modified_row, &self.created_row, &self.accessed_row].iter().any(|row| row.get_visible()));

        self.update_media(first);
        self.update_permissions(&facts);
        self.updating.set(false);
    }

    /// The icon of the first file, or its thumbnail if it is a picture or a video.
    fn update_icon(self: &Rc<Self>, first: &FileFacts) {
        let content_type = first.content_type.as_deref().unwrap_or("application/octet-stream");
        let icon = if first.is_dir { gio::content_type_get_icon("inode/directory") } else { gio::content_type_get_icon(content_type) };
        if self.icon.paintable().is_none() || self.thumbnail_for.borrow().is_none() {
            self.icon.set_from_gicon(&icon);
        }
        let media = if content_type.starts_with("image/") {
            Some(ThumbnailMedia::Picture)
        } else if content_type.starts_with("video/") {
            Some(ThumbnailMedia::Video)
        } else {
            None
        };
        let Some(media) = media.filter(|_| !self.is_multi() && !first.is_dir) else { return };
        let key = (first.path.clone(), first.modified);
        if self.thumbnail_for.borrow().as_ref() == Some(&key) {
            return;
        }
        self.thumbnail_for.replace(Some(key.clone()));
        let weak = Rc::downgrade(self);
        let wanted = {
            let weak = weak.clone();
            let key = key.clone();
            move || weak.upgrade().is_some_and(|this| this.thumbnail_for.borrow().as_ref() == Some(&key))
        };
        Thumbnails::shared().request_still(
            &first.path,
            first.modified.unwrap_or_default(),
            media,
            ThumbnailSize::for_pixels(ICON_SIZE * 2),
            wanted,
            move |texture| {
                if let Some(this) = weak.upgrade() {
                    this.icon.set_paintable(Some(texture));
                }
            },
        );
    }

    /// Says how much room is left where the folder is, for a selection of folders.
    fn update_free_space(&self) {
        let free = self.free.get();
        self.free_space.set_visible(free.is_some());
        self.free_space.set_text(&free.map(|free| format!("{} free", glib::format_size(free))).unwrap_or_default());
    }

    /// Shows what the probe knows of a picture or a video: its size, and for a video its length, rate and codecs.
    fn update_media(self: &Rc<Self>, first: &FileFacts) {
        let content_type = first.content_type.as_deref().unwrap_or_default();
        let is_media = !self.is_multi() && !first.is_dir && (content_type.starts_with("image/") || content_type.starts_with("video/"));
        let known = is_media
            .then(|| {
                let weak = Rc::downgrade(self);
                let path = first.path.clone();
                let wanted = {
                    let weak = weak.clone();
                    move || weak.upgrade().is_some()
                };
                MediaInfo::shared().request(&first.path, first.modified.unwrap_or_default(), first.size, wanted, move || {
                    if let Some(this) = weak.upgrade() {
                        let first = this.facts.borrow().iter().find(|file| file.path == path).cloned();
                        if let Some(first) = first {
                            this.update_media(&first);
                        }
                    }
                })
            })
            .flatten();
        let values: [Option<String>; 5] = match known {
            Some(metadata) => [
                metadata.width.zip(metadata.height).map(|(width, height)| format!("{width} × {height}")),
                metadata.duration.map(format_duration),
                metadata.frame_rate.map(format_fps),
                match (metadata.codec, metadata.audio_codec) {
                    (Some(video), Some(audio)) => Some(format!("{video}, {audio}")),
                    (codec, audio) => codec.or(audio),
                },
                metadata.bit_rate.map(|bits| format!("{} kb/s", bits / 1000)),
            ],
            None => Default::default(),
        };
        for (row, value) in self.media_rows.iter().zip(&values) {
            row.set_visible(value.is_some());
            row.set_subtitle(value.as_deref().unwrap_or_default());
        }
        self.media.set_visible(values.iter().any(Option::is_some));
    }

    /// Shows what the user may do with the files, in the row that leads to the permissions page and on that page.
    fn update_permissions(self: &Rc<Self>, facts: &[FileFacts]) {
        let summary = summarize_permissions(facts);
        let known = summary.files.is_some() || summary.folders.is_some();
        self.permissions_group.set_visible(known);
        if !known {
            return;
        }
        let first = &facts[0];
        if self.is_multi() {
            self.permissions_value.set_text("");
        } else {
            let slot = if first.is_dir { summary.folders } else { summary.files };
            let class = Who::ALL.iter().position(|who| *who == class_of(first)).unwrap_or(2);
            self.permissions_value.set_text(slot.map_or("", |slot| access_text(slot[class], first.is_dir)));
        }

        let executable = !self.is_multi()
            && !first.is_dir
            && first.content_type.as_deref().is_some_and(gio::content_type_can_be_executable);
        self.execute_row.set_visible(executable);
        self.execute_row.set_active(summary.executable == Some(Executable::Yes));
        self.execute_row.set_sensitive(summary.can_set_any_file);

        let layout = (summary.files.is_some(), summary.folders.is_some());
        if self.permission_rows.borrow().as_ref().is_none_or(|rows| rows.layout != layout) {
            self.rebuild_permission_rows(layout);
        }
        let rows = self.permission_rows.borrow();
        let Some(rows) = rows.as_ref() else { return };
        let same = |value: Option<String>| value.unwrap_or_else(|| "—".to_owned());
        let shared = |pick: fn(&FileFacts) -> Option<&String>| {
            let first = pick(first);
            facts.iter().all(|file| pick(file) == first).then(|| first.cloned()).flatten()
        };
        rows.owner.set_subtitle(&same(shared(|file| file.owner.as_ref())));
        rows.group.set_subtitle(&same(shared(|file| file.group.as_ref())));
        rows.banner.set_revealed(!summary.can_set_all_files && !summary.can_set_all_folders);
        for access in &rows.access {
            let folder = access.kind == Kind::Folders;
            let slot = if folder { summary.folders } else { summary.files };
            let Some(slot) = slot else { continue };
            let value = slot[Who::ALL.iter().position(|who| *who == access.who).unwrap_or(0)];
            let choices = access_choices(access.who, folder);
            let mut items: Vec<&str> = choices.iter().map(|choice| access_text(Some(*choice), folder)).collect();
            // A value that is not among the choices, or a mix, is shown as it is until another is chosen.
            let selected = match value.and_then(|value| choices.iter().position(|choice| *choice == value)) {
                Some(position) => position,
                None => {
                    items.push(access_text(value, folder));
                    items.len() - 1
                }
            };
            access.row.set_model(Some(&gtk::StringList::new(&items)));
            access.row.set_selected(selected as u32);
            access.row.set_sensitive(if folder { summary.can_set_all_folders } else { summary.can_set_all_files });
        }
    }

    /// Builds the permissions page for a selection of files, folders or both.
    fn rebuild_permission_rows(self: &Rc<Self>, (files, folders): (bool, bool)) {
        while let Some(child) = self.permissions_content.first_child() {
            self.permissions_content.remove(&child);
        }
        let banner = adw::Banner::new("Only the owner can edit these permissions");
        self.permissions_content.append(&banner);
        let page = adw::PreferencesPage::new();
        self.permissions_content.append(&page);

        let kinds: Vec<(Kind, &str)> = match (folders, files) {
            (true, true) => vec![(Kind::Folders, "Folder Access"), (Kind::Files, "File Access")],
            (true, false) => vec![(Kind::Folders, "Access")],
            _ => vec![(Kind::Files, "Access")],
        };
        let owner = adw::ActionRow::builder().title("Owner").selectable(false).subtitle_selectable(true).build();
        let group = adw::ActionRow::builder().title("Group").selectable(false).subtitle_selectable(true).build();
        let mut access = Vec::new();
        for who in Who::ALL {
            let section = adw::PreferencesGroup::new();
            match who {
                Who::Owner => section.add(&owner),
                Who::Group => section.add(&group),
                Who::Others => section.set_title("Other Users"),
            }
            for (kind, title) in &kinds {
                let row = adw::ComboRow::builder().title(*title).activatable(false).build();
                let (kind, who) = (*kind, who);
                row.connect_selected_notify({
                    let weak = Rc::downgrade(self);
                    move |row| {
                        let Some(this) = weak.upgrade() else { return };
                        if this.updating.get() {
                            return;
                        }
                        let choices = access_choices(who, kind == Kind::Folders);
                        if let Some(choice) = choices.get(row.selected() as usize) {
                            let modes = modes_for_access(&this.facts.borrow(), kind, who, *choice);
                            this.write_modes(modes);
                        }
                    }
                });
                section.add(&row);
                access.push(AccessRow { row, who, kind });
            }
            page.add(&section);
        }
        self.permission_rows.replace(Some(PermissionRows { layout: (files, folders), access, owner, group, banner }));
    }

    /// Writes the new modes on a worker, then reads the files again. A mode that could not be written is reported.
    fn write_modes(self: &Rc<Self>, modes: Vec<(PathBuf, u32)>) {
        if modes.is_empty() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || modes.iter().try_for_each(|(path, mode)| set_mode(path, *mode))).await;
            if let Ok(Err(err)) = result {
                this.toasts.add_toast(adw::Toast::new(&format!("Could not change permissions: {err}")));
            }
            this.refresh();
        });
    }

    /// Counts what the folders and files hold in the background, showing the count as it grows if `visible`. A count
    /// started again while one runs ends the first.
    fn start_count(self: &Rc<Self>, visible: bool) {
        let facts = self.facts.borrow().clone();
        self.counted.replace(facts.iter().map(|file| (file.modified, file.size)).collect());
        if !self.is_multi() && !facts.first().is_some_and(|file| file.is_dir) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.count_cancel.replace(cancel.clone()).store(true, Ordering::Relaxed);
        let shared: Arc<Mutex<(Counts, bool)>> = Arc::default();
        let paths: Vec<PathBuf> = facts.iter().map(|file| file.path.clone()).collect();
        if visible {
            self.spinner.set_visible(true);
            self.contents.set_text(&contents_text(&Counts::default(), true));
        }
        drop(gio::spawn_blocking({
            let (shared, cancel) = (shared.clone(), cancel.clone());
            move || {
                let total = count_contents(&paths, &cancel, &mut |counts| {
                    if let Ok(mut state) = shared.lock() {
                        state.0 = *counts;
                    }
                });
                if let Ok(mut state) = shared.lock() {
                    *state = (total, true);
                }
            }
        }));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            loop {
                glib::timeout_future(COUNT_INTERVAL).await;
                let Some(this) = weak.upgrade() else { return };
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let (counts, done) = shared.lock().map(|state| *state).unwrap_or_default();
                if visible || done {
                    this.contents.set_text(&contents_text(&counts, !done));
                    this.contents.set_tooltip_text(Some(&bytes_text(counts.size)));
                }
                if done {
                    this.spinner.set_visible(false);
                    return;
                }
            }
        });
    }

    /// Watches the files, to bring the dialog up to date when one changes.
    fn watch_files(self: &Rc<Self>) {
        let monitors = self
            .paths
            .iter()
            .filter_map(|path| {
                let file = gio::File::for_path(path);
                // A folder is watched for what is added to and removed from it, which changes what it holds.
                let flags = gio::FileMonitorFlags::WATCH_MOVES;
                let monitor = if path.is_dir() {
                    file.monitor_directory(flags, gio::Cancellable::NONE)
                } else {
                    file.monitor_file(flags, gio::Cancellable::NONE)
                }
                .ok()?;
                let weak = Rc::downgrade(self);
                monitor.connect_changed(move |_, _, _, event| {
                    use gio::FileMonitorEvent::*;
                    if matches!(event, ChangesDoneHint | AttributeChanged | Deleted | Renamed | MovedOut | Created | MovedIn) {
                        if let Some(this) = weak.upgrade() {
                            this.schedule_refresh();
                        }
                    }
                });
                Some(monitor)
            })
            .collect();
        self.monitors.replace(monitors);
    }

    /// Reads the files again once they have stopped changing for `REFRESH_DELAY`.
    fn schedule_refresh(self: &Rc<Self>) {
        let generation = self.refresh_generation.get() + 1;
        self.refresh_generation.set(generation);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            glib::timeout_future(REFRESH_DELAY).await;
            if let Some(this) = weak.upgrade().filter(|this| this.refresh_generation.get() == generation) {
                this.refresh();
            }
        });
    }

    /// Reads the files again and shows what they are now. The dialog closes if none is left.
    fn refresh(self: &Rc<Self>) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let (facts, free) = this.query().await;
            if facts.is_empty() {
                this.close();
                return;
            }
            let changed = this.counted.borrow().len() != facts.len()
                || this.counted.borrow().iter().zip(&facts).any(|(counted, file)| *counted != (file.modified, file.size));
            this.facts.replace(facts);
            this.free.set(free);
            this.update();
            if changed {
                this.start_count(false);
            }
        });
    }

    /// Opens the folder the link points at, and closes the dialog.
    fn open_link_target(&self) {
        let facts = self.facts.borrow();
        let Some(file) = facts.first() else { return };
        let Some(target) = file.link_target.as_deref() else { return };
        let target = file.path.parent().map_or_else(|| PathBuf::from(target), |parent| parent.join(target));
        drop(facts);
        (self.open)(&target);
        self.close();
    }

    /// Opens the folder the files are in, and closes the dialog.
    fn open_parent_folder(&self) {
        let parent = self.facts.borrow().first().and_then(|file| file.path.parent().map(Path::to_path_buf));
        if let Some(parent) = parent {
            (self.open)(&parent);
            self.close();
        }
    }

    fn close(&self) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.close();
        }
    }

    /// Ends the work in the background, when the dialog closes.
    fn finish(&self) {
        self.count_cancel.borrow().store(true, Ordering::Relaxed);
        self.refresh_generation.set(self.refresh_generation.get() + 1);
        self.thumbnail_for.replace(None);
        self.monitors.take();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use super::*;
    use crate::fs::properties::Access;

    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        let ctx = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for: {what}");
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rsimgv-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn open(paths: Vec<PathBuf>, current: Option<PathBuf>) -> (gtk::Window, Rc<Properties>, Rc<RefCell<Vec<PathBuf>>>) {
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.present();
        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        let properties = present(&window, paths, current, move |path| sink.borrow_mut().push(path.to_path_buf()));
        wait_for("the properties", || properties.nav.visible_page_tag().as_deref() == Some("main"));
        (window, properties, opened)
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn selected_text(row: &adw::ComboRow) -> String {
        row.selected_item().and_downcast::<gtk::StringObject>().map(|item| item.string().to_string()).unwrap_or_default()
    }

    /// A lone file shows what it is and where, and a change on disk reaches the dialog.
    fn a_file_shows_its_facts_and_follows_changes() {
        let dir = root("props-file");
        let file = dir.join("note.txt");
        fs::write(&file, "hello").unwrap();
        let (window, properties, opened) = open(vec![file.clone()], None);

        assert_eq!(properties.name.text(), "note.txt");
        assert!(properties.kind.is_visible() && properties.kind.text().contains("text/plain"));
        assert!(properties.size.is_visible() && properties.size.text().starts_with("5 bytes"));
        assert!(!properties.contents_box.is_visible(), "a file has no contents");
        assert!(properties.parent_row.is_visible());
        assert_eq!(properties.parent_row.subtitle().as_deref(), Some(dir.to_string_lossy().as_ref()));
        assert!(properties.modified_row.is_visible() && properties.accessed_row.is_visible());
        assert!(properties.execute_row.is_visible());

        fs::write(&file, "hello, longer").unwrap();
        wait_for("the new size", || properties.size.text().starts_with("13 bytes"));

        ActionRowExt::activate(&properties.parent_row);
        assert_eq!(*opened.borrow(), [dir.clone()], "the parent row opens the folder");
        wait_for("the dialog closing", || properties.dialog.upgrade().is_none_or(|dialog| !dialog.is_mapped()));

        window.destroy();
        let _ = fs::remove_dir_all(&dir);
    }

    /// The parent row is left out when the file is in the folder being browsed.
    fn the_parent_row_is_left_out_for_the_folder_being_browsed() {
        let dir = root("props-parent");
        let file = dir.join("a.txt");
        fs::write(&file, "").unwrap();
        let (window, properties, _) = open(vec![file], Some(dir.clone()));
        assert!(!properties.parent_row.is_visible());
        assert!(!properties.locations.is_visible());
        window.destroy();
        let _ = fs::remove_dir_all(&dir);
    }

    /// A folder counts what it holds, and a count started again after a change shows the new total.
    fn a_folder_counts_its_contents() {
        let dir = root("props-folder");
        let folder = dir.join("album");
        fs::create_dir_all(folder.join("inner")).unwrap();
        fs::write(folder.join("a.txt"), "12345").unwrap();
        fs::write(folder.join("inner/b.txt"), "123").unwrap();
        let (window, properties, _) = open(vec![folder.clone()], None);

        assert!(properties.contents_box.is_visible() && !properties.size.is_visible());
        assert!(!properties.kind.is_visible(), "a folder's type is not worth saying");
        assert!(!properties.accessed_row.is_visible(), "nor when it was read");
        wait_for("the count", || properties.contents.text().starts_with("3 items, totalling"));
        assert_eq!(properties.contents.tooltip_text().as_deref(), Some("8 bytes"));
        assert!(!properties.spinner.is_visible());

        fs::write(folder.join("c.txt"), "x").unwrap();
        wait_for("the new total", || properties.contents.text().starts_with("4 items, totalling"));

        window.destroy();
        let _ = fs::remove_dir_all(&dir);
    }

    /// Several files are named together, counted together, and show a type only if they share one.
    fn several_files_are_named_and_counted_together() {
        let dir = root("props-several");
        let paths: Vec<PathBuf> = (1..=3).map(|i| dir.join(format!("f{i}.txt"))).collect();
        for path in &paths {
            fs::write(path, "1234").unwrap();
        }
        fs::write(dir.join("p.png"), "").unwrap();
        let (window, properties, _) = open(paths.clone(), None);
        assert_eq!(properties.name.text(), "f1.txt, f2.txt, f3.txt");
        assert!(properties.kind.is_visible());
        assert!(!properties.modified_row.is_visible(), "times are for one file");
        wait_for("the total", || properties.contents.text().starts_with("3 items, totalling"));
        window.destroy();

        let mut mixed = paths.clone();
        mixed.push(dir.join("p.png"));
        let (window, properties, _) = open(mixed, None);
        assert!(!properties.kind.is_visible(), "different types show none");
        window.destroy();

        let many: Vec<PathBuf> = (0..6).map(|i| dir.join(format!("m{i}"))).collect();
        for path in &many {
            fs::write(path, "").unwrap();
        }
        let (window, properties, _) = open(many, None);
        assert_eq!(properties.name.text(), "6 Selected Items");
        window.destroy();
        let _ = fs::remove_dir_all(&dir);
    }

    /// The permissions page shows what each class may do, and choosing another changes the file.
    fn permissions_are_shown_and_changed() {
        let dir = root("props-perms");
        let file = dir.join("script.sh");
        fs::write(&file, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        let (window, properties, _) = open(vec![file.clone()], None);

        assert_eq!(properties.permissions_value.text(), "Read and Write");
        assert!(properties.execute_row.is_visible() && !properties.execute_row.is_active());
        let guard = properties.permission_rows.borrow();
        let rows = guard.as_ref().expect("permission rows");
        assert_eq!(rows.access.len(), 3, "one access row each for the owner, the group and others");
        let row_of = |who: Who| rows.access.iter().find(|row| row.who == who).unwrap();
        assert_eq!(selected_text(&row_of(Who::Owner).row), "Read and Write");
        assert_eq!(selected_text(&row_of(Who::Group).row), "Read-Only");
        assert_eq!(selected_text(&row_of(Who::Others).row), "None");
        assert!(rows.owner.subtitle().is_some_and(|owner| !owner.is_empty()));

        // Others may read: choose "Read-Only" in the others' row.
        let others = row_of(Who::Others);
        others.row.set_selected(access_choices(Who::Others, false).iter().position(|c| *c == Access::READ).unwrap() as u32);
        wait_for("the mode written", || mode(&file) == 0o644);
        drop(guard);
        wait_for("the dialog reading the file again", || properties.facts.borrow()[0].mode == Some(0o644));
        assert_eq!(properties.permissions_value.text(), "Read and Write", "the page's own class is unchanged");

        // The program switch sets every execute bit, and clearing it takes them away.
        properties.execute_row.set_active(true);
        wait_for("the execute bits", || mode(&file) == 0o755);
        wait_for("the switch following the file", || properties.facts.borrow()[0].mode == Some(0o755));
        assert!(properties.execute_row.is_active());
        properties.execute_row.set_active(false);
        wait_for("the execute bits cleared", || mode(&file) == 0o644);

        window.destroy();
        let _ = fs::remove_dir_all(&dir);
    }

    /// A selection of files and folders has a row for each, and a mix is shown as one.
    fn files_and_folders_have_separate_access_rows_and_a_mix_shows_a_dash() {
        let dir = root("props-mixed");
        let (a, b, folder) = (dir.join("a"), dir.join("b"), dir.join("d"));
        fs::write(&a, "").unwrap();
        fs::write(&b, "").unwrap();
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&a, fs::Permissions::from_mode(0o640)).unwrap();
        fs::set_permissions(&b, fs::Permissions::from_mode(0o660)).unwrap();
        let (window, properties, _) = open(vec![a.clone(), b.clone(), folder.clone()], None);

        let guard = properties.permission_rows.borrow();
        let rows = guard.as_ref().unwrap();
        assert_eq!(rows.access.len(), 6, "folder and file access for each class");
        let group_files = rows.access.iter().find(|row| row.who == Who::Group && row.kind == Kind::Files).unwrap();
        assert_eq!(selected_text(&group_files.row), "---", "files that differ show a mix");
        let owner_files = rows.access.iter().find(|row| row.who == Who::Owner && row.kind == Kind::Files).unwrap();
        assert_eq!(selected_text(&owner_files.row), "Read and Write");
        assert!(!properties.execute_row.is_visible(), "the program switch is for one file");

        // Choosing for the group sets both files and leaves the folder alone.
        let folder_mode = mode(&folder);
        group_files.row.set_selected(access_choices(Who::Group, false).iter().position(|c| *c == Access::READ_WRITE).unwrap() as u32);
        wait_for("both files", || mode(&a) == 0o660 && mode(&b) == 0o660);
        assert_eq!(mode(&folder), folder_mode);
        drop(guard);

        window.destroy();
        let _ = fs::remove_dir_all(&dir);
    }

    /// The dialog closes when the file it shows is removed.
    fn the_dialog_closes_when_every_file_is_gone() {
        let dir = root("props-gone");
        let file = dir.join("brief.txt");
        fs::write(&file, "x").unwrap();
        let (window, properties, _) = open(vec![file.clone()], None);
        let dialog = properties.dialog.upgrade().expect("the dialog");
        let closed = Rc::new(Cell::new(false));
        dialog.connect_closed({
            let closed = closed.clone();
            move |_| closed.set(true)
        });
        fs::remove_file(&file).unwrap();
        wait_for("the dialog to close", || closed.get());
        window.destroy();
        let _ = fs::remove_dir_all(&dir);
    }

    pub(crate) fn run_scenarios() {
        a_file_shows_its_facts_and_follows_changes();
        the_parent_row_is_left_out_for_the_folder_being_browsed();
        a_folder_counts_its_contents();
        several_files_are_named_and_counted_together();
        permissions_are_shown_and_changed();
        files_and_folders_have_separate_access_rows_and_a_mix_shows_a_dash();
        the_dialog_closes_when_every_file_is_gone();
    }
}
