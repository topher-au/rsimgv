//! The preferences dialog. Its rows are bound to the application's GSettings keys, so a change
//! made here, from the menu, or by another instance applies immediately.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::AdwDialogExt;
use adw::subclass::prelude::*;

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
        pub collapse_on_navigate_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub item_size_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub filename_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub show_directories_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub info_size_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub info_modified_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub info_content_type_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub media_only_row: TemplateChild<adw::SwitchRow>,
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
        pub one_to_one_focus_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub expand_up_to_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub scaling_filter_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub smooth_upscaling_row: TemplateChild<adw::SwitchRow>,
        /// The handler that keeps the size row in step with the setting; removed when the dialog closes.
        pub(super) size_handler: RefCell<Option<glib::SignalHandlerId>>,
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
    pub fn new(settings: &gio::Settings) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();

        settings.bind("show-hidden", &*imp.show_hidden_row, "active").build();
        settings.bind("collapse-on-navigate", &*imp.collapse_on_navigate_row, "active").build();
        settings.bind("media-only", &*imp.media_only_row, "active").build();

        bind_switches(settings, dialog.imp());
        bind_choices(settings, dialog.imp());
        bind_info_fields(settings, dialog.imp());
        settings
            .bind("expand-up-to", &*imp.expand_up_to_scale, "value")
            .mapping(|variant, _| variant.get::<i32>().map(|size| f64::from(size).to_value()))
            .set_mapping(|value, _| value.get::<f64>().ok().map(|size| (size.round() as i32).to_variant()))
            .build();

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
    ];
    for (key, row) in switches {
        settings.bind(key, &**row, "active").build();
    }
}

/// Binds each combo row to a string setting whose value is one of `choices`, by position in the row.
fn bind_choices(settings: &gio::Settings, imp: &imp::PreferencesDialog) {
    let combos: [(&str, &adw::ComboRow, &[&str]); 6] = [
        ("filename-display", &imp.filename_row, &["off", "wrapped", "ellipsized"]),
        ("show-directories", &imp.show_directories_row, &["none", "first-last", "inline"]),
        ("zoom-indicator", &imp.zoom_indicator_row, &["on", "off", "auto"]),
        ("image-fit", &imp.image_fit_row, &["fit-window", "stretch-width", "one-to-one", "fit-stretch"]),
        ("one-to-one-focus", &imp.one_to_one_focus_row, &["top", "center", "cursor"]),
        ("scaling-filter", &imp.scaling_filter_row, &["nearest", "bilinear", "bicubic", "lanczos"]),
    ];
    for (key, row, choices) in combos {
        let choices: &'static [&'static str] = choices;
        settings
            .bind(key, row, "selected")
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

/// Binds the info switches to the `info-fields` list: a switch on adds its field, off removes it.
fn bind_info_fields(settings: &gio::Settings, imp: &imp::PreferencesDialog) {
    let rows = [
        (InfoField::Size, &imp.info_size_row),
        (InfoField::Modified, &imp.info_modified_row),
        (InfoField::ContentType, &imp.info_content_type_row),
    ];
    let shown = |settings: &gio::Settings, field: InfoField| settings.strv("info-fields").iter().any(|key| key == field.key());
    for (field, row) in &rows {
        row.set_active(shown(settings, *field));
        row.connect_active_notify({
            let settings = settings.clone();
            let field = *field;
            move |row| {
                let mut keys: Vec<String> = settings.strv("info-fields").iter().map(|key| key.to_string()).collect();
                let listed = keys.iter().any(|key| key == field.key());
                if row.is_active() == listed {
                    return;
                }
                if row.is_active() {
                    keys.push(field.key().to_owned());
                } else {
                    keys.retain(|key| key != field.key());
                }
                // Keep the list in the order the fields are offered in.
                keys.sort_by_key(|key| InfoField::ALL.iter().position(|f| f.key() == key));
                let _ = settings.set_strv("info-fields", keys.as_slice());
            }
        });
    }
    // A change made elsewhere, such as by editing the file, moves the switches.
    let weak: Vec<(InfoField, glib::WeakRef<adw::SwitchRow>)> =
        rows.iter().map(|(field, row)| (*field, row.downgrade())).collect();
    settings.connect_changed(Some("info-fields"), move |settings, _| {
        for (field, row) in &weak {
            if let Some(row) = row.upgrade() {
                row.set_active(shown(settings, *field));
            }
        }
    });
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
