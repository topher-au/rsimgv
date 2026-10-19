//! The preferences dialog. Its rows are bound to the application's GSettings keys, so a change
//! made here, from the menu, or by another instance applies immediately.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::{AdwDialogExt, PreferencesGroupExt, PreferencesPageExt};
use adw::subclass::prelude::*;

use super::keybinding_list::KeybindingList;
use super::browse_view::InfoField;
use gtk::prelude::*;
use gtk::{gio, glib};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rimgv/ui/preferences.ui")]
    pub struct PreferencesDialog {
        #[template_child]
        pub show_hidden_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub ipc_socket_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_bar_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub thumbnail_bar_shown_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub blurred_background_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub ipc_tcp_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub ipc_http_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub collapse_on_navigate_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub item_size_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub filename_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub show_directories_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub info_layout_dropdown: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub info_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub add_info_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub media_only_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub start_location_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub start_directory_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub start_directory_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub open_fullscreen_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub auto_hide_cursor_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub smooth_scrolling_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub image_info_in_title_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub zoom_indicator_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub image_fit_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub keep_fit_mode_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub zoom_step_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub expand_up_to_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub video_backend_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub mpv_args_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub scaling_filter_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub smooth_upscaling_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub keybindings_page: TemplateChild<adw::PreferencesPage>,
        /// The handler that keeps the size row in step with the setting; removed when the dialog closes.
        pub(super) size_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// The handler that rebuilds the info list when the setting changes; removed when the dialog closes.
        pub(super) info_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "RsimgvPreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PreferencesDialog {}
    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
    impl PreferencesDialogImpl for PreferencesDialog {}
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PreferencesDialog {
    /// Shows the extra mpv options, saved when applied, and only while mpv plays videos.
    fn bind_mpv_args(&self, settings: &gio::Settings) {
        use adw::prelude::EntryRowExt;
        let row = &*self.imp().mpv_args_row;
        row.set_text(&settings.string("mpv-args"));
        row.connect_apply({
            let settings = settings.clone();
            move |row| {
                let _ = settings.set_string("mpv-args", &row.text());
            }
        });
        let update = {
            let row = row.clone();
            move |settings: &gio::Settings| row.set_sensitive(settings.string("video-backend") == "mpv")
        };
        update(settings);
        settings.connect_changed(Some("video-backend"), move |settings, _| update(settings));
    }

    pub fn new(settings: &gio::Settings) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();

        dialog.build_keybindings_page();

        settings.bind("show-hidden", &*imp.show_hidden_row, "active").build();
        settings.bind("collapse-on-navigate", &*imp.collapse_on_navigate_row, "active").build();
        settings.bind("media-only", &*imp.media_only_row, "active").build();
        settings.bind("ipc-socket", &*imp.ipc_socket_row, "active").build();
        settings.bind("viewer-blurred-background", &*imp.blurred_background_row, "active").build();
        bind_port(settings, &imp.ipc_tcp_row, "ipc-tcp-port");
        bind_port(settings, &imp.zoom_step_row, "zoom-step");
        dialog.bind_mpv_args(settings);
        bind_port(settings, &imp.ipc_http_row, "ipc-http-port");

        bind_switches(settings, dialog.imp());
        bind_choices(settings, dialog.imp());
        dialog.bind_start_directory(settings);
        bind_info_fields(settings, &dialog);
        // The scale steps through no limit, shown as "-", then 1x to 4x, with a mark at each.
        let scale = &*imp.expand_up_to_scale;
        for factor in 0..=4 {
            scale.add_mark(f64::from(factor), gtk::PositionType::Bottom, None);
        }
        scale.set_format_value_func(|_, value| match value.round() as i32 {
            0 => "-".to_owned(),
            factor => format!("{factor}x"),
        });
        // The scale holds a double and the key an integer, so the factor is written by hand.
        scale.set_value(f64::from(settings.int("expand-up-to-factor")));
        scale.connect_value_changed({
            let settings = settings.clone();
            move |scale| {
                let factor = scale.value().round() as i32;
                if settings.int("expand-up-to-factor") != factor {
                    let _ = settings.set_int("expand-up-to-factor", factor);
                }
            }
        });

        // The spin row holds a double and the key an integer, so the size is converted by hand.
        imp.item_size_row.set_value(f64::from(settings.int("item-size")));
        let row = imp.item_size_row.downgrade();
        imp.item_size_row.connect_value_notify({
            let settings = settings.clone();
            move |row| {
                let size = row.value() as i32;
                if settings.int("item-size") != size {
                    let _ = settings.set_int("item-size", size);
                }
            }
        });
        let handler = settings.connect_changed(Some("item-size"), move |settings, _| {
            if let Some(row) = row.upgrade() {
                row.set_value(f64::from(settings.int("item-size")));
            }
        });
        imp.size_handler.replace(Some(handler));

        // Closing the dialog ends the size handler; the bindings above end with the widgets.
        let settings = settings.clone();
        let handler = Rc::new(RefCell::new(None::<glib::SignalHandlerId>));
        let _ = &handler;
        dialog.connect_closed(move |dialog: &PreferencesDialog| {
            if let Some(handler) = dialog.imp().size_handler.take() {
                settings.disconnect(handler);
            }
            if let Some(handler) = dialog.imp().info_handler.take() {
                settings.disconnect(handler);
            }
        });
        dialog
    }
}

/// Binds each switch row to its boolean setting.
fn bind_switches(settings: &gio::Settings, imp: &imp::PreferencesDialog) {
    let switches = [
        ("open-fullscreen", &imp.open_fullscreen_row),
        ("auto-hide-cursor", &imp.auto_hide_cursor_row),
        ("smooth-scrolling", &imp.smooth_scrolling_row),
        ("image-info-in-title", &imp.image_info_in_title_row),
        ("keep-fit-mode", &imp.keep_fit_mode_row),
        ("smooth-upscaling", &imp.smooth_upscaling_row),
        ("viewer-thumbnail-bar-shown", &imp.thumbnail_bar_shown_row),
    ];
    for (key, row) in switches {
        settings.bind(key, &**row, "active").build();
    }
}

/// Binds each combo row to a string setting whose value is one of `choices`, by position in the row.
fn bind_choices(settings: &gio::Settings, imp: &imp::PreferencesDialog) {
    let combos: [(&str, &glib::Object, &[&str]); 9] = [
        ("video-backend", imp.video_backend_row.upcast_ref(), &["mpv", "gtk"]),
        ("viewer-thumbnail-bar", imp.thumbnail_bar_row.upcast_ref(), &["top", "left", "bottom", "right"]),
        ("start-location", imp.start_location_row.upcast_ref(), &["last", "root", "home", "specific"]),
        ("filename-display", imp.filename_row.upcast_ref(), &["off", "wrapped", "ellipsized"]),
        ("show-directories", imp.show_directories_row.upcast_ref(), &["none", "first-last", "inline"]),
        ("zoom-indicator", imp.zoom_indicator_row.upcast_ref(), &["on", "off", "auto"]),
        ("image-fit", imp.image_fit_row.upcast_ref(), &["fit-window", "stretch-width", "one-to-one", "fit-stretch"]),
        ("scaling-filter", imp.scaling_filter_row.upcast_ref(), &["nearest", "bilinear", "bicubic", "lanczos"]),
        ("info-layout", imp.info_layout_dropdown.upcast_ref(), &["disabled", "stacked", "inline"]),
    ];
    for (key, object, choices) in combos {
        let choices: &'static [&'static str] = choices;
        settings
            .bind(key, object, "selected")
            .mapping(move |variant, _| {
                let name = variant.str()?;
                let index = choices.iter().position(|choice| *choice == name).unwrap_or(0);
                Some((index as u32).to_value())
            })
            .set_mapping(move |value, _| {
                let index = value.get::<u32>().ok()? as usize;
                choices.get(index).map(|choice| choice.to_variant())
            })
            .build();
    }
}

/// Fills the info list from the `info-fields` setting, one row per field in its order, and offers the
/// fields not yet shown in the add menu. Called again whenever the setting changes.
fn rebuild_info_list(settings: &gio::Settings, imp: &imp::PreferencesDialog) {
    let list = &imp.info_list;
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let shown: Vec<String> = settings.strv("info-fields").iter().map(|key| key.to_string()).collect();
    for (index, key) in shown.iter().enumerate() {
        if let Some(field) = InfoField::from_key(key) {
            list.append(&info_row(settings, index, field));
        }
    }

    let menu = gio::Menu::new();
    for field in InfoField::ALL.into_iter().filter(|field| !shown.iter().any(|key| key == field.key())) {
        let item = gio::MenuItem::new(Some(field.title()), None);
        item.set_action_and_target_value(Some("prefs.add-info"), Some(&field.key().to_variant()));
        menu.append_item(&item);
    }
    imp.add_info_button.set_menu_model(Some(&menu));
}

/// One row of the info list: a drag handle, the field's name, and a remove button. Dropping another row
/// onto it moves that field to this row's place.
fn info_row(settings: &gio::Settings, index: usize, field: InfoField) -> gtk::Box {
    let builder = gtk::Builder::from_resource("/au/topher/rimgv/ui/info_field_row.ui");
    let row: gtk::Box = builder.object("row").expect("info_field_row.ui defines `row`");
    let handle: gtk::Image = builder.object("handle").expect("info_field_row.ui defines `handle`");
    let title: gtk::Label = builder.object("title").expect("info_field_row.ui defines `title`");
    let remove: gtk::Button = builder.object("remove").expect("info_field_row.ui defines `remove`");
    title.set_label(field.title());

    let drag = gtk::DragSource::new();
    drag.set_actions(gtk::gdk::DragAction::MOVE);
    drag.connect_prepare(move |_, _, _| Some(gtk::gdk::ContentProvider::for_value(&field.key().to_value())));
    handle.add_controller(drag);

    let drop = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    drop.connect_drop({
        let settings = settings.clone();
        move |_, value, _, _| {
            let Ok(dragged) = value.get::<String>() else { return false };
            let mut keys: Vec<String> = settings.strv("info-fields").iter().map(|key| key.to_string()).collect();
            let Some(from) = keys.iter().position(|key| *key == dragged) else { return false };
            let moved = keys.remove(from);
            keys.insert(index.min(keys.len()), moved);
            let _ = settings.set_strv("info-fields", keys.as_slice());
            true
        }
    });
    row.add_controller(drop);

    remove.connect_clicked({
        let settings = settings.clone();
        move |_| {
            let keys: Vec<String> =
                settings.strv("info-fields").iter().map(|key| key.to_string()).filter(|key| key != field.key()).collect();
            let _ = settings.set_strv("info-fields", keys.as_slice());
        }
    });

    row
}

/// Keeps the info list in step with the `info-fields` setting, and gives the add menu its action.
fn bind_info_fields(settings: &gio::Settings, dialog: &PreferencesDialog) {
    let imp = dialog.imp();
    rebuild_info_list(settings, imp);
    let weak = dialog.downgrade();
    let handler = settings.connect_changed(Some("info-fields"), move |settings, _| {
        if let Some(dialog) = weak.upgrade() {
            rebuild_info_list(settings, dialog.imp());
        }
    });
    imp.info_handler.replace(Some(handler));

    let actions = gio::SimpleActionGroup::new();
    let add = gio::SimpleAction::new("add-info", Some(glib::VariantTy::STRING));
    add.connect_activate({
        let settings = settings.clone();
        move |_, key| {
            let Some(key) = key.and_then(|key| key.get::<String>()) else { return };
            let mut keys: Vec<String> = settings.strv("info-fields").iter().map(|key| key.to_string()).collect();
            if !keys.contains(&key) {
                keys.push(key);
                let _ = settings.set_strv("info-fields", keys.as_slice());
            }
        }
    });
    actions.add_action(&add);
    dialog.insert_action_group("prefs", Some(&actions));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn run_scenarios() {
        let settings = gio::Settings::new("au.topher.rimgv");
        settings.set_int("item-size", 96).unwrap();
        let dialog = PreferencesDialog::new(&settings);
        assert_eq!(dialog.imp().item_size_row.value(), 96.0, "the row shows the setting");

        // Changing the row writes the setting, and changing the setting moves the row.
        dialog.imp().item_size_row.set_value(160.0);
        assert_eq!(settings.int("item-size"), 160);
        settings.set_int("item-size", 112).unwrap();
        assert_eq!(dialog.imp().item_size_row.value(), 112.0);

        settings.set_boolean("media-only", false).unwrap();
        assert!(!dialog.imp().media_only_row.is_active());
        settings.set_boolean("media-only", true).unwrap();
        settings.set_int("item-size", 128).unwrap();
    }
}

impl PreferencesDialog {
    /// Puts the keybindings editor on the keybindings page.
    fn build_keybindings_page(&self) {
        let group = adw::PreferencesGroup::new();
        group.add(&KeybindingList::new());
        self.imp().keybindings_page.add(&group);
    }
}

/// Keeps a port row in step with an integer setting, in both directions.
fn bind_port(settings: &gio::Settings, row: &adw::SpinRow, key: &'static str) {
    row.set_value(f64::from(settings.int(key)));
    let settings = settings.clone();
    row.connect_value_notify(move |row| {
        let _ = settings.set_int(key, row.value() as i32);
    });
}

impl PreferencesDialog {
    /// Shows the start directory row only for a specific start location, with the chosen folder as
    /// its subtitle, and lets its button pick the folder.
    fn bind_start_directory(&self, settings: &gio::Settings) {
        let imp = self.imp();
        settings
            .bind("start-location", &*imp.start_directory_row, "visible")
            .mapping(|variant, _| Some((variant.str()? == "specific").to_value()))
            .get_only()
            .build();
        settings
            .bind("start-directory", &*imp.start_directory_row, "subtitle")
            .mapping(|variant, _| {
                let path = variant.str()?;
                Some(if path.is_empty() { "None chosen".to_value() } else { path.to_value() })
            })
            .get_only()
            .build();

        let settings = settings.clone();
        let weak = self.downgrade();
        imp.start_directory_button.connect_clicked(move |_| {
            let Some(dialog) = weak.upgrade() else { return };
            let root = dialog.root().and_downcast::<gtk::Window>();
            let chooser = gtk::FileDialog::builder().title("Start Directory").build();
            let current = settings.string("start-directory");
            if !current.is_empty() {
                chooser.set_initial_folder(Some(&gio::File::for_path(current.as_str())));
            }
            let settings = settings.clone();
            chooser.select_folder(root.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let Some(path) = result.ok().and_then(|folder| folder.path()) {
                    let _ = settings.set_string("start-directory", &path.to_string_lossy());
                }
            });
        });
    }
}
