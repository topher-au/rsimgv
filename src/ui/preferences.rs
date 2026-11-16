//! The preferences dialog. Its rows are bound to the application's GSettings keys, so a change
//! made here, from the menu, or by another instance applies immediately.

use crate::settings::{ChangedHandler, Settings};

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::{
    ActionRowExt, AdwDialogExt, ComboRowExt, EntryRowExt, ExpanderRowExt, PreferencesGroupExt, PreferencesPageExt,
    PreferencesRowExt,
};
use adw::subclass::prelude::*;

use super::keybinding_list::KeybindingList;
use crate::services::scripts::{read_scripts, write_scripts, Script};
use super::tight_layout::TightLayout;
use super::browse_view::InfoField;
use gtk::prelude::*;
use gtk::{gio, glib};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rsimgv/ui/preferences.ui")]
    pub struct PreferencesDialog {
        #[template_child]
        pub show_hidden_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub ipc_socket_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub ipc_socket_path_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub ipc_tcp_switch_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub ipc_http_switch_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_bar_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub thumbnail_bar_shown_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_bar_overlay_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_bar_pinned_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_bar_size_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub blurred_background_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub svp_enabled_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub svp_fps_mode_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub svp_fps_target_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub svp_fps_minimum_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub svp_fps_maximum_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub svp_nvof_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub svp_plugin_directory_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub svp_plugin_directory_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub svp_plugin_clear_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub svp_seek_bypass_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub svp_seek_bypass_delay_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub viewer_status_bar_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub ipc_tcp_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub ipc_http_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub ipc_external_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub collapse_on_navigate_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub tree_monitoring_row: TemplateChild<adw::ComboRow>,
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
        pub image_preloading_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_threads_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub thumbnail_cache_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_unload_offscreen_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_aggression_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub thumbnail_max_image_size_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub thumbnail_max_video_size_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub thumbnail_crop_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub thumbnail_crop_video_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub video_frame_previews_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub directory_previews_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub folder_preview_cycle_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub folder_preview_count_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub folder_preview_fit_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub start_location_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub start_directory_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub start_directory_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub folder_end_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub slideshow_delay_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub slideshow_countdown_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub remember_folder_sort_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub playlist_fetch_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub playlist_repeat_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub playlist_column_position_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub playlist_column_name_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub playlist_column_length_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub playlist_column_filename_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub show_images_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub show_videos_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub playlist_window_headerbar_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub default_sort_by_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub default_sort_order_row: TemplateChild<adw::ComboRow>,
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
        pub original_focus_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub unlock_minimum_zoom_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub keep_fit_mode_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub zoom_step_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub expand_up_to_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub video_backend_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub video_start_delay_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub video_scaled_times_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub mpv_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub gstreamer_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub svp_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub svp_rate_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub svp_seek_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub overlay_page: TemplateChild<gtk::Box>,
        #[template_child]
        pub video_crop_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub video_crop_method_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub video_crop_ratio_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub video_crop_trim_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub video_backdrop_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub video_backdrop_zoom_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub video_backdrop_blur_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub video_backdrop_dim_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub remember_file_settings_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remember_position_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remember_speed_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remember_volume_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remember_aspect_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remember_rotation_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remember_zoom_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remember_pan_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub sidebar_animations_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub sidebar_animation_ms_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub history_enabled_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub history_images_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub history_image_seconds_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub history_videos_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub history_video_seconds_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub history_video_percent_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub history_count_paused_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub history_show_stats_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub controls_locked_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub controls_reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub svp_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub mpv_args_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpv_opengl_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub scaling_filter_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub smooth_upscaling_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub keybindings_page: TemplateChild<gtk::Box>,
        #[template_child]
        pub back_button_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub forward_button_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub scroll_image_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub scroll_video_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub scroll_speed_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub section_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub section_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub tab_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub search_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub content_toolbar: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub browse_stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub general_stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub about_version_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub app_version_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub gtk_version_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub app_version: TemplateChild<gtk::Label>,
        #[template_child]
        pub gtk_version: TemplateChild<gtk::Label>,
        #[template_child]
        pub scripts_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub plugins_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub plugin_auto_reload_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub add_script_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub edit_script_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove_script_button: TemplateChild<gtk::Button>,
        /// The handler that keeps the size row in step with the setting; removed when the dialog closes.
        pub(super) size_handler: RefCell<Option<ChangedHandler>>,
        /// The handler that rebuilds the info list when the setting changes; removed when the dialog closes.
        pub(super) info_handler: RefCell<Option<ChangedHandler>>,
        /// The handler that rebuilds the scripts list when the setting changes; removed when the dialog closes.
        pub(super) scripts_handler: RefCell<Option<ChangedHandler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "RsimgvPreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::Dialog;

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
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PreferencesDialog {
    /// Shows the options of the chosen video backend, and lets SVP be changed only while mpv plays videos, with a
    /// banner that offers mpv while it does not. The extra mpv options are saved when applied.
    fn bind_video_backend(&self, settings: &Settings) {
        use adw::prelude::EntryRowExt;
        let imp = self.imp();
        let row = &*imp.mpv_args_row;
        row.set_text(&settings.string("video.mpv.args"));
        row.connect_apply({
            let settings = settings.clone();
            move |row| {
                let _ = settings.set_string("video.mpv.args", &row.text());
            }
        });
        settings.bind("video.mpv.opengl", &*imp.mpv_opengl_row, "active").build();
        settings
            .bind("video.backend", &*imp.svp_banner, "revealed")
            .mapping(|variant, _| Some((variant.str()? != "mpv").to_value()))
            .get_only()
            .build();
        imp.svp_banner.connect_button_clicked({
            let settings = settings.clone();
            move |_| {
                let _ = settings.set_string("video.backend", "mpv");
            }
        });
        let groups = [
            ("visible", &imp.mpv_group, "mpv"),
            ("visible", &imp.gstreamer_group, "gtk"),
            ("sensitive", &imp.svp_group, "mpv"),
            ("sensitive", &imp.svp_rate_group, "mpv"),
            ("sensitive", &imp.svp_seek_group, "mpv"),
        ];
        for (property, group, backend) in groups {
            settings
                .bind("video.backend", &**group, property)
                .mapping(move |variant, _| Some((variant.str()? == backend).to_value()))
                .get_only()
                .build();
        }
    }

    /// Binds the remote control rows. Each transport's path or port row is insensitive while its toggle is off.
    fn bind_remote(&self, settings: &Settings) {
        use adw::prelude::EntryRowExt;
        let imp = self.imp();
        settings.bind("remote.socket.enabled", &*imp.ipc_socket_row, "active").build();
        settings.bind("remote.socket.enabled", &*imp.ipc_socket_path_row, "sensitive").build();
        settings.bind("remote.tcp.enabled", &*imp.ipc_tcp_switch_row, "active").build();
        settings.bind("remote.tcp.enabled", &*imp.ipc_tcp_row, "sensitive").build();
        settings.bind("remote.http.enabled", &*imp.ipc_http_switch_row, "active").build();
        settings.bind("remote.http.enabled", &*imp.ipc_http_row, "sensitive").build();
        bind_port(settings, &imp.ipc_tcp_row, "remote.tcp.port");
        bind_port(settings, &imp.ipc_http_row, "remote.http.port");

        // The path is saved when applied, and a blank path means the default socket.
        let row = &*imp.ipc_socket_path_row;
        row.set_text(&settings.string("remote.socket.path"));
        row.connect_apply({
            let settings = settings.clone();
            move |row| {
                let _ = settings.set_string("remote.socket.path", &row.text());
            }
        });
    }

    pub fn new(settings: &Settings) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();

        dialog.build_keybindings_page();
        dialog.bind_sections();
        tighten(imp.section_stack.upcast_ref());
        dialog.bind_scripts(settings);
        dialog.bind_plugins(settings);
        imp.about_version_row.set_subtitle(env!("CARGO_PKG_VERSION"));
        imp.app_version.set_label(env!("CARGO_PKG_VERSION"));
        let gtk_version = format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version());
        imp.gtk_version.set_label(&gtk_version);
        imp.app_version_box.set_tooltip_text(Some(&format!("rsimgv {}", env!("CARGO_PKG_VERSION"))));
        imp.gtk_version_box.set_tooltip_text(Some(&format!("GTK {gtk_version}")));

        settings.bind("browse.show-hidden", &*imp.show_hidden_row, "active").build();
        settings.bind("browse.collapse-on-navigate", &*imp.collapse_on_navigate_row, "active").build();
        settings.bind("browse.media-only", &*imp.media_only_row, "active").build();
        settings.bind("image.preloading", &*imp.image_preloading_row, "active").build();
        imp.thumbnail_threads_row.adjustment().set_upper(crate::fs::thumbnail::thread_limit() as f64);
        bind_port(settings, &imp.thumbnail_threads_row, "thumbnails.threads");
        settings.bind("thumbnails.cache", &*imp.thumbnail_cache_row, "active").build();
        settings.bind("thumbnails.unload-offscreen", &*imp.thumbnail_unload_offscreen_row, "active").build();
        bind_thumbnail_size(settings, &imp.thumbnail_max_image_size_row, "thumbnails.max-image-size");
        bind_thumbnail_size(settings, &imp.thumbnail_max_video_size_row, "thumbnails.max-video-size");
        settings.bind("thumbnails.crop", &*imp.thumbnail_crop_row, "active").build();
        settings.bind("browse.folder-previews.enabled", &*imp.directory_previews_row, "active").build();
        settings.bind("browse.folder-previews.cycle", &*imp.folder_preview_cycle_row, "active").build();
        bind_port(settings, &imp.folder_preview_count_row, "browse.folder-previews.count");
        settings.bind("browse.folder-previews.fit", &*imp.folder_preview_fit_row, "active").build();
        // The number of previews is not used while they are fitted, so its row is off then.
        settings
            .bind("browse.folder-previews.fit", &*imp.folder_preview_count_row, "sensitive")
            .flags(gio::SettingsBindFlags::INVERT_BOOLEAN)
            .build();
        settings.bind("viewer.blurred-background", &*imp.blurred_background_row, "active").build();
        settings.bind("video.svp.enabled", &*imp.svp_enabled_row, "active").build();
        settings.bind("video.svp.fps.target", &*imp.svp_fps_target_row, "value").build();
        settings.bind("video.svp.nvof", &*imp.svp_nvof_row, "active").build();
        settings.bind("video.svp.seek-bypass.enabled", &*imp.svp_seek_bypass_row, "active").build();
        settings.bind("video.svp.seek-bypass.delay", &*imp.svp_seek_bypass_delay_row, "value").build();
        settings.bind("video.svp.seek-bypass.enabled", &*imp.svp_seek_bypass_delay_row, "sensitive").get_only().build();
        bind_port(settings, &imp.svp_fps_minimum_row, "video.svp.fps.minimum");
        bind_port(settings, &imp.svp_fps_maximum_row, "video.svp.fps.maximum");
        settings.bind("viewer.status-bar.shown", &*imp.viewer_status_bar_row, "active").build();
        bind_port(settings, &imp.zoom_step_row, "image.zoom-step");
        bind_port(settings, &imp.sidebar_animation_ms_row, "window.sidebar.animation-ms");
        bind_port(settings, &imp.history_image_seconds_row, "history.image-seconds");
        bind_port(settings, &imp.history_video_seconds_row, "history.video-seconds");
        bind_port(settings, &imp.history_video_percent_row, "history.video-percent");
        bind_port(settings, &imp.slideshow_delay_row, "viewer.slideshow-delay");
        // The history's options mean nothing while it is off, and each count's threshold means nothing while it is off.
        {
            let settings = settings.clone();
            let watch_settings = settings.clone();
            let (images, image_seconds) = (imp.history_images_row.clone(), imp.history_image_seconds_row.clone());
            let (videos, video_seconds) = (imp.history_videos_row.clone(), imp.history_video_seconds_row.clone());
            let video_percent = imp.history_video_percent_row.clone();
            let count_paused = imp.history_count_paused_row.clone();
            let show_stats = imp.history_show_stats_row.clone();
            let update = move || {
                let enabled = watch_settings.boolean("history.enabled");
                let videos_enabled = enabled && watch_settings.boolean("history.videos");
                images.set_sensitive(enabled);
                videos.set_sensitive(enabled);
                show_stats.set_sensitive(enabled);
                image_seconds.set_sensitive(enabled && watch_settings.boolean("history.images"));
                video_seconds.set_sensitive(videos_enabled);
                video_percent.set_sensitive(videos_enabled);
                count_paused.set_sensitive(videos_enabled);
            };
            update();
            for key in ["history.enabled", "history.images", "history.videos"] {
                settings.connect_changed(key, {
                    let update = update.clone();
                    move |_, _| update()
                });
            }
        }
        bind_port(settings, &imp.video_crop_trim_row, "video.crop.trim");
        bind_port(settings, &imp.video_backdrop_blur_row, "video.backdrop.blur");
        for (row, key) in [
            (&imp.video_backdrop_zoom_row, "video.backdrop.zoom"),
            (&imp.video_backdrop_dim_row, "video.backdrop.dim"),
        ] {
            settings.bind(key, &**row, "value").build();
        }
        // The options mean nothing while their filter is off.
        for row in [imp.video_crop_method_row.upcast_ref::<gtk::Widget>(), imp.video_crop_trim_row.upcast_ref()] {
            settings.bind("video.crop.enabled", row, "sensitive").get_only().build();
        }
        for row in [
            imp.video_backdrop_zoom_row.upcast_ref::<gtk::Widget>(),
            imp.video_backdrop_blur_row.upcast_ref(),
            imp.video_backdrop_dim_row.upcast_ref(),
        ] {
            settings.bind("video.backdrop.enabled", row, "sensitive").get_only().build();
        }
        settings.bind("window.sidebar.animations", &*imp.sidebar_animation_ms_row, "sensitive").get_only().build();
        bind_port(settings, &imp.video_start_delay_row, "video.start-delay");
        bind_port(settings, &imp.scroll_speed_row, "mouse.scroll.speed");
        bind_port(settings, &imp.thumbnail_bar_size_row, "viewer.thumbnail-bar.size");
        dialog.bind_video_backend(settings);
        dialog.bind_remote(settings);
        settings.bind("remote.external-access", &*imp.ipc_external_row, "active").build();

        bind_switches(settings, dialog.imp());
        bind_choices(settings, dialog.imp());
        imp.controls_reset_button.connect_clicked({
            let settings = settings.clone();
            move |_| {
                for key in ["video.controls.x", "video.controls.y", "video.controls.width"] {
                    settings.reset(key);
                }
            }
        });
        dialog.bind_file_settings(settings);
        imp.overlay_page.append(&super::overlay_editor::build(settings));
        dialog.bind_start_directory(settings);
        dialog.bind_svp_plugins(settings);
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
        scale.set_value(f64::from(settings.int("image.expand-up-to-factor")));
        scale.connect_value_changed({
            let settings = settings.clone();
            move |scale| {
                let factor = scale.value().round() as i32;
                if settings.int("image.expand-up-to-factor") != factor {
                    let _ = settings.set_int("image.expand-up-to-factor", factor);
                }
            }
        });

        // The spin row holds a double and the key an integer, so the size is converted by hand.
        imp.item_size_row.set_value(f64::from(settings.int("browse.item-size")));
        let row = imp.item_size_row.downgrade();
        imp.item_size_row.connect_value_notify({
            let settings = settings.clone();
            move |row| {
                let size = row.value() as i32;
                if settings.int("browse.item-size") != size {
                    let _ = settings.set_int("browse.item-size", size);
                }
            }
        });
        let handler = settings.connect_changed("browse.item-size", move |settings, _| {
            if let Some(row) = row.upgrade() {
                row.set_value(f64::from(settings.int("browse.item-size")));
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
            if let Some(handler) = dialog.imp().scripts_handler.take() {
                settings.disconnect(handler);
            }
        });
        dialog
    }
}

impl PreferencesDialog {
    /// Lists the plugins. Each row has a switch that turns the plugin on or off at once, a reload button and the
    /// settings the plugin exposes. Also binds the auto-reload switch.
    fn bind_plugins(&self, settings: &Settings) {
        settings.bind("plugins.auto-reload", &*self.imp().plugin_auto_reload_row, "active").build();

        let list = self.imp().plugins_list.clone();
        list.remove_all();
        let value = crate::plugins::list();
        for plugin in value["plugins"].as_array().into_iter().flatten() {
            list.append(&Self::plugin_row(plugin, settings));
        }
    }

    /// The row for one plugin: its name and status, a switch that enables it, a reload button, and the settings it
    /// exposes. Turning it on or off applies at once and refreshes the row's status.
    fn plugin_row(plugin: &serde_json::Value, settings: &Settings) -> adw::ExpanderRow {
        let id = plugin["id"].as_str().unwrap_or_default().to_owned();
        let name = plugin["name"].as_str().unwrap_or("Plugin");
        let enabled = plugin["enabled"].as_bool().unwrap_or(true);

        let row = adw::ExpanderRow::builder().title(name).subtitle(Self::plugin_subtitle(plugin)).build();
        let declared = crate::plugins::plugin_settings(&id);
        row.set_enable_expansion(!declared.is_empty());
        let values = crate::plugins::plugin_config(&id);
        for setting in declared {
            let current = values.get(&setting.key).cloned().unwrap_or(setting.default.clone());
            let key = setting.key.clone();
            let plugin_id = id.clone();
            let widget: gtk::Widget = match setting.kind.as_str() {
                "bool" => {
                    let widget = adw::SwitchRow::builder()
                        .title(&setting.label)
                        .subtitle(&setting.description)
                        .active(current.as_bool().unwrap_or(false))
                        .build();
                    widget.connect_active_notify(move |widget| {
                        let _ = crate::plugins::set_plugin_setting(
                            &plugin_id,
                            &key,
                            serde_json::Value::Bool(widget.is_active()),
                        );
                    });
                    widget.upcast()
                }
                "int" => {
                    let minimum = setting.min.unwrap_or(0) as f64;
                    let maximum = setting.max.unwrap_or(1_000_000) as f64;
                    let value = current.as_i64().unwrap_or(minimum as i64) as f64;
                    let widget = adw::SpinRow::with_range(minimum, maximum, 1.0);
                    widget.set_title(&setting.label);
                    widget.set_subtitle(&setting.description);
                    widget.set_value(value.clamp(minimum, maximum));
                    widget.connect_value_notify(move |widget| {
                        let _ = crate::plugins::set_plugin_setting(
                            &plugin_id,
                            &key,
                            serde_json::Value::from(widget.value() as i64),
                        );
                    });
                    widget.upcast()
                }
                "choice" => {
                    let choices: Vec<&str> = setting.choices.iter().map(String::as_str).collect();
                    let widget = adw::ComboRow::builder()
                        .title(&setting.label)
                        .subtitle(&setting.description)
                        .model(&gtk::StringList::new(&choices))
                        .build();
                    let selected = current
                        .as_str()
                        .and_then(|value| setting.choices.iter().position(|choice| choice == value))
                        .unwrap_or(0);
                    widget.set_selected(selected as u32);
                    let choices = setting.choices.clone();
                    widget.connect_selected_notify(move |widget| {
                        if let Some(choice) = choices.get(widget.selected() as usize) {
                            let _ = crate::plugins::set_plugin_setting(
                                &plugin_id,
                                &key,
                                serde_json::Value::String(choice.clone()),
                            );
                        }
                    });
                    widget.upcast()
                }
                _ => {
                    let widget = adw::EntryRow::builder()
                        .title(&setting.label)
                        .text(current.as_str().unwrap_or_default())
                        .show_apply_button(true)
                        .build();
                    if !setting.description.is_empty() {
                        widget.set_tooltip_text(Some(&setting.description));
                    }
                    widget.connect_apply(move |widget| {
                        let _ = crate::plugins::set_plugin_setting(
                            &plugin_id,
                            &key,
                            serde_json::Value::String(widget.text().to_string()),
                        );
                    });
                    widget.upcast()
                }
            };
            row.add_row(&widget);
        }

        let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
        reload.set_tooltip_text(Some("Reload plugin"));
        reload.set_valign(gtk::Align::Center);
        reload.add_css_class("flat");
        reload.set_sensitive(enabled);
        let reload_id = id.clone();
        reload.connect_clicked(move |_| {
            if let Err(err) = crate::plugins::reload(Some(&reload_id)) {
                eprintln!("rsimgv: {err}");
            }
        });

        let switch = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .active(enabled)
            .tooltip_text("Enable plugin")
            .build();
        let switch_id = id.clone();
        let settings = settings.clone();
        switch.connect_state_set({
            let row = row.clone();
            let reload = reload.clone();
            move |_, state| {
                // A failed change keeps the switch where it was, so it still shows what is running.
                if let Err(err) = crate::plugins::set_enabled(&switch_id, state, &settings) {
                    eprintln!("rsimgv: {err}");
                    return glib::Propagation::Stop;
                }
                let current = crate::plugins::list()["plugins"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|plugin| plugin["id"].as_str() == Some(switch_id.as_str()))
                    .cloned()
                    .unwrap_or_default();
                row.set_subtitle(&Self::plugin_subtitle(&current));
                reload.set_sensitive(state);
                glib::Propagation::Proceed
            }
        });

        row.add_suffix(&switch);
        row.add_suffix(&reload);
        row
    }

    /// The status line under a plugin's name: what it does, its runtime, and whether it is running.
    fn plugin_subtitle(plugin: &serde_json::Value) -> String {
        let id = plugin["id"].as_str().unwrap_or_default();
        let description = plugin["description"].as_str().filter(|text| !text.is_empty()).unwrap_or(id);
        let runtime = plugin["runtime"].as_str().unwrap_or_default();
        let status = if !plugin["enabled"].as_bool().unwrap_or(true) {
            "disabled"
        } else if let Some(error) = plugin["error"].as_str() {
            error
        } else if plugin["running"].as_bool().unwrap_or(false) {
            "running"
        } else {
            "not running"
        };
        format!("{description} · {runtime} · {status}")
    }

    /// Lists the scripts, and adds, edits or removes them through a dialog. Each change is saved to the
    /// `scripts` setting, and the list follows that setting.
    fn bind_scripts(&self, settings: &Settings) {
        let imp = self.imp();
        let editing: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

        let name_row = adw::EntryRow::builder().title("Name").build();
        let command_row = adw::EntryRow::builder().title("Command").build();
        let wait_row = adw::SwitchRow::builder().title("Wait to finish").build();
        let fields = adw::PreferencesGroup::builder().description("Keywords: %file%").build();
        fields.add(&name_row);
        fields.add(&command_row);
        fields.add(&wait_row);
        let page = adw::PreferencesPage::new();
        page.add(&fields);
        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::builder().label("Create").css_classes(["suggested-action"]).sensitive(false).build();
        let header = adw::HeaderBar::new();
        header.pack_start(&cancel);
        header.pack_end(&save);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&page));
        let script_dialog = adw::Dialog::builder().content_width(420).child(&view).build();

        // Saving needs a name and a command.
        let update_save = {
            let (name_row, command_row, save) = (name_row.clone(), command_row.clone(), save.clone());
            move || save.set_sensitive(!name_row.text().trim().is_empty() && !command_row.text().trim().is_empty())
        };
        name_row.connect_changed({
            let update_save = update_save.clone();
            move |_| update_save()
        });
        command_row.connect_changed(move |_| update_save());

        // Opens the dialog empty for a new script, or filled in for the script at `index`.
        let parent = self.downgrade();
        let show = {
            let (script_dialog, name_row, command_row, wait_row, save, editing) =
                (script_dialog.clone(), name_row.clone(), command_row.clone(), wait_row.clone(), save.clone(), editing.clone());
            let settings = settings.clone();
            move |index: Option<usize>| {
                let script = index.and_then(|index| read_scripts(&settings).get(index).cloned()).unwrap_or_default();
                name_row.set_text(&script.name);
                command_row.set_text(&script.command);
                wait_row.set_active(script.wait);
                script_dialog.set_title(if index.is_some() { "Edit script" } else { "New application/script" });
                save.set_label(if index.is_some() { "Save" } else { "Create" });
                editing.set(index);
                if let Some(parent) = parent.upgrade() {
                    script_dialog.present(Some(&parent));
                }
            }
        };

        cancel.connect_clicked({
            let script_dialog = script_dialog.clone();
            move |_| {
                script_dialog.close();
            }
        });
        save.connect_clicked({
            let (script_dialog, name_row, command_row, wait_row, editing) =
                (script_dialog.clone(), name_row.clone(), command_row.clone(), wait_row.clone(), editing.clone());
            let settings = settings.clone();
            move |_| {
                let script = Script {
                    name: name_row.text().trim().to_owned(),
                    command: command_row.text().trim().to_owned(),
                    wait: wait_row.is_active(),
                };
                let mut scripts = read_scripts(&settings);
                match editing.get() {
                    Some(index) if index < scripts.len() => scripts[index] = script,
                    _ => scripts.push(script),
                }
                write_scripts(&settings, &scripts);
                script_dialog.close();
            }
        });

        let list = imp.scripts_list.clone();
        let selected_index = move || list.selected_row().map(|row| row.index() as usize);
        imp.add_script_button.connect_clicked({
            let show = show.clone();
            move |_| show(None)
        });
        imp.edit_script_button.connect_clicked({
            let (show, selected_index) = (show.clone(), selected_index.clone());
            move |_| {
                if let Some(index) = selected_index() {
                    show(Some(index));
                }
            }
        });
        imp.remove_script_button.connect_clicked({
            let (settings, selected_index) = (settings.clone(), selected_index.clone());
            move |_| {
                if let Some(index) = selected_index() {
                    let mut scripts = read_scripts(&settings);
                    if index < scripts.len() {
                        scripts.remove(index);
                        write_scripts(&settings, &scripts);
                    }
                }
            }
        });

        // Only a selected script can be edited or removed.
        imp.scripts_list.connect_row_selected({
            let (edit, remove) = (imp.edit_script_button.clone(), imp.remove_script_button.clone());
            move |_, row| {
                edit.set_sensitive(row.is_some());
                remove.set_sensitive(row.is_some());
            }
        });

        // The list is rebuilt from the setting now, and again whenever the setting changes.
        let list = imp.scripts_list.clone();
        let rebuild = move |settings: &Settings| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            for script in read_scripts(settings) {
                let row = adw::ActionRow::builder().title(script.name.as_str()).subtitle(script.command.as_str()).build();
                list.append(&row);
            }
        };
        rebuild(settings);
        let handler = settings.connect_changed("scripts.list", move |settings, _| rebuild(settings));
        imp.scripts_handler.replace(Some(handler));
    }
}

/// Binds each switch row to its boolean setting.
fn bind_switches(settings: &Settings, imp: &imp::PreferencesDialog) {
    let switches = [
        ("browse.sort.remember-per-folder", &imp.remember_folder_sort_row),
        ("browse.show-images", &imp.show_images_row),
        ("browse.show-videos", &imp.show_videos_row),
        ("playlist.fetch-in-background", &imp.playlist_fetch_row),
        ("playlist.repeat", &imp.playlist_repeat_row),
        ("playlist.columns.position", &imp.playlist_column_position_row),
        ("playlist.columns.name", &imp.playlist_column_name_row),
        ("playlist.columns.length", &imp.playlist_column_length_row),
        ("playlist.columns.filename", &imp.playlist_column_filename_row),
        ("viewer.open-fullscreen", &imp.open_fullscreen_row),
        ("viewer.auto-hide-cursor", &imp.auto_hide_cursor_row),
        ("image.smooth-scrolling", &imp.smooth_scrolling_row),
        ("viewer.image-info-in-title", &imp.image_info_in_title_row),
        ("remember.enabled", &imp.remember_file_settings_row),
        ("remember.position", &imp.remember_position_row),
        ("remember.speed", &imp.remember_speed_row),
        ("remember.volume", &imp.remember_volume_row),
        ("remember.aspect", &imp.remember_aspect_row),
        ("remember.rotation", &imp.remember_rotation_row),
        ("remember.zoom", &imp.remember_zoom_row),
        ("remember.pan", &imp.remember_pan_row),
        ("video.crop.enabled", &imp.video_crop_row),
        ("video.backdrop.enabled", &imp.video_backdrop_row),
        ("video.controls.locked", &imp.controls_locked_row),
        ("window.sidebar.animations", &imp.sidebar_animations_row),
        ("image.keep-fit", &imp.keep_fit_mode_row),
        ("image.unlock-minimum-zoom", &imp.unlock_minimum_zoom_row),
        ("image.smooth-upscaling", &imp.smooth_upscaling_row),
        ("viewer.thumbnail-bar.shown", &imp.thumbnail_bar_shown_row),
        ("viewer.thumbnail-bar.overlay", &imp.thumbnail_bar_overlay_row),
        ("viewer.thumbnail-bar.pinned", &imp.thumbnail_bar_pinned_row),
        ("thumbnails.crop-video", &imp.thumbnail_crop_video_row),
        ("video.scaled-times", &imp.video_scaled_times_row),
        ("history.enabled", &imp.history_enabled_row),
        ("history.images", &imp.history_images_row),
        ("history.videos", &imp.history_videos_row),
        ("history.count-paused", &imp.history_count_paused_row),
        ("history.show-stats", &imp.history_show_stats_row),
    ];
    for (key, row) in switches {
        settings.bind(key, &**row, "active").build();
    }
}

/// Binds each combo row to a string setting whose value is one of `choices`, by position in the row.
fn bind_choices(settings: &Settings, imp: &imp::PreferencesDialog) {
    let combos: [(&str, &glib::Object, &[&str]); 25] = [
        ("browse.tree-monitoring", imp.tree_monitoring_row.upcast_ref(), &["off", "expanded", "visible"]),
        ("browse.video-frame-previews", imp.video_frame_previews_row.upcast_ref(), &["none", "frames", "embedded"]),
        ("thumbnails.aggression", imp.thumbnail_aggression_row.upcast_ref(), &["low", "standard", "increased", "high", "extreme"]),
        ("video.crop.method", imp.video_crop_method_row.upcast_ref(), &["cropdetect", "edges", "static", "across"]),
        ("video.crop.ratio", imp.video_crop_ratio_row.upcast_ref(), &["auto", "4:3", "16:9", "9:16", "1:1", "3:2", "2:3", "21:9"]),
        ("viewer.folder-end", imp.folder_end_row.upcast_ref(), &["stop", "loop", "next-folder"]),
        ("viewer.slideshow-countdown", imp.slideshow_countdown_row.upcast_ref(), &["none", "bar", "circle", "text"]),
        ("browse.sort.by", imp.default_sort_by_row.upcast_ref(), &["name", "modified", "size", "type"]),
        ("browse.sort.order", imp.default_sort_order_row.upcast_ref(), &["ascending", "descending"]),
        ("mouse.back", imp.back_button_row.upcast_ref(), &["back", "up", "previous", "next", "forward", "none"]),
        ("mouse.forward", imp.forward_button_row.upcast_ref(), &["back", "up", "previous", "next", "forward", "none"]),
        ("mouse.scroll.image", imp.scroll_image_row.upcast_ref(), &["ctrl-zoom", "zoom", "step", "none"]),
        ("mouse.scroll.video", imp.scroll_video_row.upcast_ref(), &["volume", "seek", "step", "none"]),
        ("video.backend", imp.video_backend_row.upcast_ref(), &["mpv", "gtk"]),
        ("viewer.thumbnail-bar.side", imp.thumbnail_bar_row.upcast_ref(), &["top", "left", "bottom", "right"]),
        ("start.location", imp.start_location_row.upcast_ref(), &["last", "root", "home", "specific"]),
        ("browse.filename-display", imp.filename_row.upcast_ref(), &["off", "wrapped", "ellipsized"]),
        ("browse.show-directories", imp.show_directories_row.upcast_ref(), &["none", "first-last", "inline"]),
        ("image.zoom-indicator", imp.zoom_indicator_row.upcast_ref(), &["on", "off", "auto"]),
        ("image.fit", imp.image_fit_row.upcast_ref(), &["window", "width", "height", "fill", "original"]),
        ("image.original-focus", imp.original_focus_row.upcast_ref(), &["top", "center", "cursor"]),
        ("image.scaling-filter", imp.scaling_filter_row.upcast_ref(), &["nearest", "linear", "linear-sharp", "cubic", "cubic-sharp"]),
        ("browse.info.layout", imp.info_layout_dropdown.upcast_ref(), &["disabled", "stacked", "inline"]),
        ("video.svp.fps.mode", imp.svp_fps_mode_row.upcast_ref(), &["default", "fixed", "variable", "display"]),
        ("playlist.window.headerbar", imp.playlist_window_headerbar_row.upcast_ref(), &["full", "compact", "none"]),
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
fn rebuild_info_list(settings: &Settings, imp: &imp::PreferencesDialog) {
    let list = &imp.info_list;
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let shown: Vec<String> = settings.strv("browse.info.fields").iter().map(|key| key.to_string()).collect();
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
fn info_row(settings: &Settings, index: usize, field: InfoField) -> gtk::Box {
    let builder = gtk::Builder::from_resource("/au/topher/rsimgv/ui/info_field_row.ui");
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
            let mut keys: Vec<String> = settings.strv("browse.info.fields").iter().map(|key| key.to_string()).collect();
            let Some(from) = keys.iter().position(|key| *key == dragged) else { return false };
            let moved = keys.remove(from);
            keys.insert(index.min(keys.len()), moved);
            let _ = settings.set_strv("browse.info.fields", keys.as_slice());
            true
        }
    });
    row.add_controller(drop);

    remove.connect_clicked({
        let settings = settings.clone();
        move |_| {
            let keys: Vec<String> =
                settings.strv("browse.info.fields").iter().map(|key| key.to_string()).filter(|key| key != field.key()).collect();
            let _ = settings.set_strv("browse.info.fields", keys.as_slice());
        }
    });

    row
}

/// Keeps the info list in step with the `info-fields` setting, and gives the add menu its action.
fn bind_info_fields(settings: &Settings, dialog: &PreferencesDialog) {
    let imp = dialog.imp();
    rebuild_info_list(settings, imp);
    let weak = dialog.downgrade();
    let handler = settings.connect_changed("browse.info.fields", move |settings, _| {
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
            let mut keys: Vec<String> = settings.strv("browse.info.fields").iter().map(|key| key.to_string()).collect();
            if !keys.contains(&key) {
                keys.push(key);
                let _ = settings.set_strv("browse.info.fields", keys.as_slice());
            }
        }
    });
    actions.add_action(&add);
    dialog.insert_action_group("prefs", Some(&actions));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use adw::prelude::ComboRowExt;

    pub(crate) fn run_scenarios() {
        let settings = crate::settings::app_settings();
        settings.set_int("browse.item-size", 96).unwrap();
        let dialog = PreferencesDialog::new(&settings);
        assert_eq!(dialog.imp().item_size_row.value(), 96.0, "the row shows the setting");

        // Changing the row writes the setting, and changing the setting moves the row.
        dialog.imp().item_size_row.set_value(160.0);
        assert_eq!(settings.int("browse.item-size"), 160);
        settings.set_int("browse.item-size", 112).unwrap();
        assert_eq!(dialog.imp().item_size_row.value(), 112.0);

        settings.set_boolean("browse.media-only", false).unwrap();
        assert!(!dialog.imp().media_only_row.is_active());
        settings.set_boolean("browse.media-only", true).unwrap();
        settings.set_int("browse.item-size", 128).unwrap();

        assert!(dialog.imp().image_preloading_row.is_active(), "image preloading is on by default");
        assert!(dialog.imp().thumbnail_cache_row.is_active(), "the thumbnail cache is on by default");
        assert!(dialog.imp().thumbnail_unload_offscreen_row.is_active(), "offscreen thumbnails are unloaded by default");
        let threads = dialog.imp().thumbnail_threads_row.adjustment();
        assert_eq!(threads.lower(), 1.0);
        assert_eq!(threads.upper(), crate::fs::thumbnail::thread_limit() as f64, "at most the system's threads");
        assert_eq!(threads.value(), 4f64.min(threads.upper()), "4 by default, where the system has that many");

        // A port row is insensitive while its transport is off.
        settings.set_boolean("remote.tcp.enabled", false).unwrap();
        assert!(!dialog.imp().ipc_tcp_row.is_sensitive(), "the TCP port is insensitive while TCP is off");
        settings.set_boolean("remote.tcp.enabled", true).unwrap();
        assert!(dialog.imp().ipc_tcp_row.is_sensitive(), "the TCP port is sensitive while TCP is on");

        // The chosen backend's options are shown, and SVP can be changed only with mpv.
        let imp = dialog.imp();
        assert!(imp.mpv_group.get_visible() && !imp.gstreamer_group.get_visible(), "mpv is the default");
        assert!(imp.svp_group.is_sensitive());
        imp.video_backend_row.set_selected(1);
        assert_eq!(settings.string("video.backend"), "gtk");
        assert!(!imp.mpv_group.get_visible() && imp.gstreamer_group.get_visible(), "GStreamer's options replace mpv's");
        assert!(!imp.svp_group.is_sensitive(), "SVP needs mpv");
        assert!(!imp.svp_rate_group.is_sensitive() && !imp.svp_seek_group.is_sensitive(), "as do its frame rate and seeking");
        assert!(imp.svp_banner.is_revealed(), "the banner offers mpv while another backend is chosen");
        imp.svp_banner.emit_by_name::<()>("button-clicked", &[]);
        assert_eq!(settings.string("video.backend"), "mpv", "its button chooses mpv");
        assert!(!imp.svp_banner.is_revealed());
        settings.reset("video.backend");
        assert!(imp.mpv_group.get_visible() && imp.svp_group.is_sensitive());

        // The bundled SVP plugins are used until a folder of SVP's own is chosen.
        assert_eq!(
            imp.svp_plugin_directory_row.subtitle().as_deref(),
            Some("Bundled open-svpflow libraries")
        );
        assert!(!imp.svp_plugin_clear_button.get_visible(), "there is nothing to clear");
        settings.set_string("video.svp.plugin-directory", "/opt/svp/plugins").unwrap();
        assert_eq!(imp.svp_plugin_directory_row.subtitle().as_deref(), Some("/opt/svp/plugins"));
        assert!(imp.svp_plugin_clear_button.get_visible(), "the folder can be cleared");
        imp.svp_plugin_clear_button.emit_clicked();
        assert_eq!(settings.string("video.svp.plugin-directory"), "", "clicking clear uses the bundled plugins");

        // Every section is a stack of tabs under the name the sidebar gives it, and each tab has a title.
        for (name, _, _) in SECTIONS {
            let stack = imp.section_stack.child_by_name(name).and_downcast::<adw::ViewStack>().expect("a section is a stack of tabs");
            assert!(stack.pages().iter::<adw::ViewStackPage>().flatten().all(|tab| tab.title().is_some_and(|title| !title.is_empty())), "{name}");
        }
        dialog.show_section("keybindings");
        assert_eq!(imp.section_stack.visible_child_name().as_deref(), Some("keybindings"), "the keybindings action still finds its section");
        dialog.show_section("general");

        // Every page with groups stacks them with no gap.
        let columns = group_columns(imp.section_stack.upcast_ref());
        assert!(columns.len() >= 8, "a column for each page with groups: {}", columns.len());
        assert!(columns.iter().all(|column| column.layout_manager().is_some_and(|layout| layout.is::<TightLayout>())));

        let bodies = group_bodies(imp.section_stack.upcast_ref());
        assert!(bodies.len() >= 8, "a body for each group: {}", bodies.len());
        assert!(bodies.iter().all(|body| body.layout_manager().is_some_and(|layout| layout.is::<TightLayout>())));

        // The viewing history rows are bound to their settings, and its options go with its switch.
        assert!(imp.history_enabled_row.is_active(), "the viewing history is on by default");
        assert_eq!(settings.int("history.image-seconds"), 5, "a picture counts after five seconds by default");
        assert_eq!(settings.int("history.video-seconds"), 20, "twenty seconds of a video counts as a view by default");
        assert_eq!(settings.int("history.video-percent"), 10, "a tenth of a video counts as a view by default");
        settings.set_boolean("history.enabled", false).unwrap();
        assert!(!imp.history_enabled_row.is_active(), "the row follows history.enabled");
        assert!(!imp.history_images_row.is_sensitive(), "the options are off with the history");
        assert!(!imp.history_video_seconds_row.is_sensitive());
        assert!(!imp.history_video_percent_row.is_sensitive());
        settings.set_boolean("history.enabled", true).unwrap();
        assert!(imp.history_images_row.is_sensitive());
        assert!(imp.history_video_seconds_row.is_sensitive());
        assert!(imp.history_video_percent_row.is_sensitive());
        imp.video_crop_ratio_row.set_selected(2);
        assert_eq!(settings.string("video.crop.ratio"), "16:9");
        settings.reset("video.crop.ratio");
        assert!(imp.video_scaled_times_row.is_active(), "playback times are scaled by default");
        imp.video_scaled_times_row.set_active(false);
        assert!(!settings.boolean("video.scaled-times"));
        settings.reset("video.scaled-times");
        assert!(!imp.thumbnail_crop_video_row.is_active(), "video thumbnails are not cropped by default");
        imp.thumbnail_crop_video_row.set_active(true);
        assert!(settings.boolean("thumbnails.crop-video"));
        settings.reset("thumbnails.crop-video");
        imp.history_image_seconds_row.set_value(4.0);
        assert_eq!(settings.int("history.image-seconds"), 4);
        imp.history_video_seconds_row.set_value(30.0);
        assert_eq!(settings.int("history.video-seconds"), 30);
        imp.history_video_percent_row.set_value(75.0);
        assert_eq!(settings.int("history.video-percent"), 75);
        settings.reset("history.image-seconds");
        settings.reset("history.video-seconds");
        settings.reset("history.video-percent");
        imp.history_show_stats_row.set_active(false);
        assert!(!settings.boolean("history.show-stats"));
        settings.reset("history.show-stats");
        settings.reset("history.enabled");

        // The image rows are bound to their settings.
        imp.image_fit_row.set_selected(3);
        assert_eq!(settings.string("image.fit"), "fill");
        settings.reset("image.fit");
        imp.original_focus_row.set_selected(2);
        assert_eq!(settings.string("image.original-focus"), "cursor");
        settings.reset("image.original-focus");
        assert!(!imp.unlock_minimum_zoom_row.is_active(), "the minimum zoom is locked by default");
        imp.unlock_minimum_zoom_row.set_active(true);
        assert!(settings.boolean("image.unlock-minimum-zoom"));
        settings.reset("image.unlock-minimum-zoom");

        // The Folder Navigation rows are bound to their settings.
        imp.folder_end_row.set_selected(2);
        assert_eq!(settings.string("viewer.folder-end"), "next-folder");
        settings.reset("viewer.folder-end");
        imp.default_sort_by_row.set_selected(1);
        imp.default_sort_order_row.set_selected(1);
        assert_eq!(settings.string("browse.sort.by"), "modified");
        assert_eq!(settings.string("browse.sort.order"), "descending");
        settings.reset("browse.sort.by");
        settings.reset("browse.sort.order");
        imp.remember_folder_sort_row.set_active(true);
        assert!(settings.boolean("browse.sort.remember-per-folder"));
        settings.reset("browse.sort.remember-per-folder");

        // The rows for settings also changed from the header bar and the playlist's menu are bound both ways.
        for (key, row) in [
            ("browse.show-images", &imp.show_images_row),
            ("browse.show-videos", &imp.show_videos_row),
            ("playlist.repeat", &imp.playlist_repeat_row),
            ("playlist.columns.position", &imp.playlist_column_position_row),
            ("playlist.columns.name", &imp.playlist_column_name_row),
            ("playlist.columns.length", &imp.playlist_column_length_row),
            ("playlist.columns.filename", &imp.playlist_column_filename_row),
        ] {
            assert!(row.is_active(), "{key} is on by default");
            row.set_active(false);
            assert!(!settings.boolean(key), "the row writes {key}");
            settings.set_boolean(key, true).unwrap();
            assert!(row.is_active(), "the row follows {key}");
            settings.reset(key);
        }
    }
}

/// The sections in the sidebar, in the order of the stack's pages: name, title and icon.
const SECTIONS: [(&str, &str, &str); 9] = [
    ("general", "General", "preferences-system-symbolic"),
    ("browsing", "Browsing", "folder-symbolic"),
    ("viewer", "Viewer", "applications-multimedia-symbolic"),
    ("images", "Images", "image-x-generic-symbolic"),
    ("videos", "Videos", "video-x-generic-symbolic"),
    ("keybindings", "Controls", "input-keyboard-symbolic"),
    ("integration", "Integration", "utilities-terminal-symbolic"),
    ("performance", "Performance", "applications-engineering-symbolic"),
    ("about", "About", "help-about-symbolic"),
];

/// The space around the keybinding editor, in pixels, so it does not touch the edges of the dialog.
const KEYBOARD_PAGE_MARGIN: i32 = 12;

impl PreferencesDialog {
    /// Puts the keybindings editor on the keybindings page, which it fills. The page is a plain box rather than a
    /// preferences page, as that would keep the editor to a narrow column and scroll it as a whole.
    fn build_keybindings_page(&self) {
        let editor = KeybindingList::new();
        editor.set_hexpand(true);
        editor.set_vexpand(true);
        editor.set_margin_top(KEYBOARD_PAGE_MARGIN);
        editor.set_margin_bottom(KEYBOARD_PAGE_MARGIN);
        editor.set_margin_start(KEYBOARD_PAGE_MARGIN);
        editor.set_margin_end(KEYBOARD_PAGE_MARGIN);
        self.imp().keybindings_page.append(&editor);
    }

    /// Names the sections, lists them in the sidebar, and joins each section's tab row to its stack.
    fn bind_sections(&self) {
        let imp = self.imp();
        self.build_tabs();
        for &(_, title, icon) in &SECTIONS {
            let row = adw::ActionRow::builder().title(title).build();
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            imp.section_list.append(&row);
        }

        let stack = imp.section_stack.clone();
        let tab_stack = imp.tab_stack.clone();
        imp.section_list.add_css_class("preferences-sections");
        imp.section_list.connect_row_selected(move |_, row| {
            if let Some(&(name, _, _)) = row.and_then(|row| SECTIONS.get(row.index() as usize)) {
                stack.set_visible_child_name(name);
                tab_stack.set_visible_child_name(name);
            }
        });

        self.build_search();

        let first = imp.section_list.row_at_index(0);
        imp.section_list.select_row(first.as_ref());
    }

    /// Gives every section a tab row at the top of the content. A section with one page is put in a stack of its
    /// own, so every page shows its tabs in the same place.
    fn build_tabs(&self) {
        let imp = self.imp();
        let children: Vec<gtk::Widget> = imp.section_stack.pages().iter::<gtk::StackPage>().flatten().map(|page| page.child()).collect();
        for child in &children {
            imp.section_stack.remove(child);
        }
        for (child, &(name, _, _)) in children.into_iter().zip(SECTIONS.iter()) {
            let stack = child.downcast::<adw::ViewStack>().unwrap_or_else(|page| {
                let stack = adw::ViewStack::new();
                stack.add_named(&page, None);
                stack
            });
            let switcher = adw::ViewSwitcher::builder().policy(adw::ViewSwitcherPolicy::Wide).build();
            bind_tabs(&switcher, &stack);
            if stack.pages().n_items() == 1 {
                switcher.add_css_class("single-tab");
            }
            imp.section_stack.add_named(&stack, Some(name));
            imp.tab_stack.add_named(&switcher, Some(name));
        }
    }

    /// Closes the dialog when a click lands outside it, on the dimmed background of `window`. A click over the dialog
    /// is ignored. The click is watched in the capture phase, so it is seen before anything under it.
    pub fn close_on_outside_click(&self, window: &gtk::Window) {
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed({
            let dialog = self.downgrade();
            let window_ref = window.downgrade();
            move |_, _, x, y| {
                let (Some(dialog), Some(window)) = (dialog.upgrade(), window_ref.upgrade()) else { return };
                let Some(content) = adw::prelude::AdwDialogExt::child(&dialog) else { return };
                let Some(bounds) = content.compute_bounds(&window) else { return };
                if !bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32)) {
                    adw::prelude::AdwDialogExt::close(&dialog);
                }
            }
        });
        window.add_controller(click.clone());
        // The click handler lives only as long as the dialog is open.
        let window_ref = window.downgrade();
        adw::prelude::AdwDialogExt::connect_closed(self, move |_| {
            if let Some(window) = window_ref.upgrade() {
                window.remove_controller(&click);
            }
        });
    }

    /// Shows the section named `name`, as picking its sidebar row does.
    pub fn show_section(&self, name: &str) {
        let Some(index) = SECTIONS.iter().position(|(section, _, _)| *section == name) else {
            return;
        };
        let imp = self.imp();
        let row = imp.section_list.row_at_index(index as i32);
        imp.section_list.select_row(row.as_ref());
    }
}

/// The box inside each preferences page under `widget` that holds the page's groups.
fn group_columns(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut columns = Vec::new();
    if widget.is::<adw::PreferencesPage>() {
        columns.extend(find_group_column(widget));
        return columns;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        columns.extend(group_columns(&current));
        child = current.next_sibling();
    }
    columns
}

/// Stacks the groups of each preferences page under `widget` with no gap, and sets each group's title in from its rows,
/// as on every page of the dialog.
pub(super) fn tighten(widget: &gtk::Widget) {
    for column in group_columns(widget) {
        column.set_layout_manager(Some(TightLayout::default()));
    }
    for body in group_bodies(widget) {
        body.set_layout_manager(Some(TightLayout::with_first_indent(GROUP_TITLE_INDENT)));
    }
}

/// How far a group's title is set in from its rows, in pixels.
const GROUP_TITLE_INDENT: i32 = 10;

/// The box inside each preferences group under `widget`, which holds the group's header above its rows.
fn group_bodies(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    if widget.is::<adw::PreferencesGroup>() {
        return widget.first_child().into_iter().collect();
    }
    let mut bodies = Vec::new();
    let mut child = widget.first_child();
    while let Some(current) = child {
        bodies.extend(group_bodies(&current));
        child = current.next_sibling();
    }
    bodies
}

/// The first widget under `widget` that has a preferences group as a child.
fn find_group_column(widget: &gtk::Widget) -> Option<gtk::Widget> {
    let children = || std::iter::successors(widget.first_child(), |child| child.next_sibling());
    if children().any(|child| child.is::<adw::PreferencesGroup>()) {
        return Some(widget.clone());
    }
    children().find_map(|child| find_group_column(&child))
}

/// Collects the preference rows under `widget`, including itself.
fn collect_rows(widget: &gtk::Widget, rows: &mut Vec<gtk::Widget>) {
    if widget.is::<adw::PreferencesRow>() {
        rows.push(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        collect_rows(&current, rows);
        child = current.next_sibling();
    }
}

/// The most results the search lists at once.
const MAX_RESULTS: usize = 50;

/// The stack page that lists the search results in place of a section.
const SEARCH_PAGE: &str = "search";

/// One preference in the search: where it lives, and how to reach it.
struct SearchEntry {
    title: String,
    /// The section and tab it is under, shown beneath its title.
    path: String,
    /// The index into `SECTIONS`.
    section: usize,
    /// The browse or media stack and the tab page holding the row, when it is under a tab.
    tab: Option<(adw::ViewStack, gtk::Widget)>,
    row: gtk::Widget,
}

impl PreferencesDialog {
    /// Builds the search: a bar under the header that filters the preferences as the user types. While the bar has
    /// text, the matching preferences replace the section. Activating a result closes the bar and shows its setting.
    fn build_search(&self) {
        let imp = self.imp();
        let entries = Rc::new(index_preferences(&imp.section_stack));
        let matches = Rc::new(RefCell::new(Vec::<usize>::new()));

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        list.add_css_class("boxed-list");
        let results = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&list).build();
        imp.section_stack.add_named(&results, Some(SEARCH_PAGE));

        let bar = imp.search_bar.clone();
        let search = imp.search_entry.clone();
        bar.connect_entry(&search);

        // Enter activates the first result, and Down moves from the field into the list.
        let activate: Rc<dyn Fn(usize)> = {
            let (dialog, entries, matches, bar) = (self.downgrade(), entries.clone(), matches.clone(), bar.clone());
            Rc::new(move |position: usize| {
                let Some(dialog) = dialog.upgrade() else { return };
                let Some(&index) = matches.borrow().get(position) else { return };
                bar.set_search_mode(false);
                dialog.reveal(&entries[index]);
            })
        };
        let activate_row = activate.clone();
        list.connect_row_activated(move |_, row| activate_row(row.index() as usize));
        let activate_first = activate.clone();
        search.connect_activate(move |_| activate_first(0));
        let keys = gtk::EventControllerKey::new();
        let list_for_keys = list.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Down {
                if let Some(row) = list_for_keys.row_at_index(0) {
                    row.grab_focus();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        search.add_controller(keys);

        let dialog_for_text = self.downgrade();
        let (entries_for_text, matches_for_text, list_for_text) = (entries.clone(), matches.clone(), list.clone());
        search.connect_search_changed(move |search| {
            let query = search.text();
            fill_results(&list_for_text, &entries_for_text, &matches_for_text, &query);
            if let Some(dialog) = dialog_for_text.upgrade() {
                dialog.show_section_or_results(!query.is_empty());
            }
        });

        // Closing the bar clears the search and goes back to the section picked in the sidebar.
        let dialog_for_close = self.downgrade();
        let search_for_close = search.clone();
        bar.connect_search_mode_enabled_notify(move |bar| {
            if bar.is_search_mode() {
                return;
            }
            search_for_close.set_text("");
            if let Some(dialog) = dialog_for_close.upgrade() {
                dialog.show_section_or_results(false);
            }
        });
        let bar_for_button = bar.clone();
        imp.search_button.connect_clicked(move |_| bar_for_button.set_search_mode(!bar_for_button.is_search_mode()));

        // Ctrl+F and F3 open the search from anywhere in the dialog.
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let (bar, search) = (bar.clone(), search.clone());
            move |_, key, _, state| {
                let ctrl_f = state.contains(gtk::gdk::ModifierType::CONTROL_MASK) && (key == gtk::gdk::Key::f || key == gtk::gdk::Key::F);
                if !ctrl_f && key != gtk::gdk::Key::F3 {
                    return glib::Propagation::Proceed;
                }
                bar.set_search_mode(true);
                search.grab_focus();
                glib::Propagation::Stop
            }
        });
        if let Some(content) = adw::prelude::AdwDialogExt::child(self) {
            content.add_controller(keys);
        }
    }

    /// Shows the search results while `searching`, or else the section picked in the sidebar.
    fn show_section_or_results(&self, searching: bool) {
        let imp = self.imp();
        if searching {
            imp.section_stack.set_visible_child_name(SEARCH_PAGE);
            return;
        }
        let name = imp.section_list.selected_row().and_then(|row| SECTIONS.get(row.index() as usize));
        if let Some(&(name, _, _)) = name {
            imp.section_stack.set_visible_child_name(name);
        }
    }

    /// Shows a search result's setting: its section and tab, with its row scrolled into view.
    fn reveal(&self, entry: &SearchEntry) {
        self.show_section(SECTIONS[entry.section].0);
        if let Some((stack, page)) = &entry.tab {
            stack.set_visible_child(page);
        }
        let row = entry.row.clone();
        glib::idle_add_local_once(move || scroll_to_row(&row));
    }
}

/// Lists every preference under the sections, in section order, each with the path to its tab.
fn index_preferences(section_stack: &gtk::Stack) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    for (section, page) in section_stack.pages().iter::<gtk::StackPage>().flatten().enumerate() {
        let section_title = SECTIONS[section].1;
        match page.child().downcast::<adw::ViewStack>() {
            Ok(stack) => {
                for tab in stack.pages().iter::<adw::ViewStackPage>().flatten() {
                    let tab_page = tab.child();
                    let Ok(preferences) = tab_page.clone().downcast::<adw::PreferencesPage>() else { continue };
                    let path = if stack.pages().n_items() == 1 { section_title.to_owned() } else { format!("{section_title} › {}", preferences.title()) };
                    index_rows(&mut entries, &tab_page, section, &path, Some((stack.clone(), tab_page.clone())));
                }
            }
            Err(child) => index_rows(&mut entries, &child, section, section_title, None),
        }
    }
    entries
}

/// Adds each titled preference row under `page`.
fn index_rows(entries: &mut Vec<SearchEntry>, page: &gtk::Widget, section: usize, path: &str, tab: Option<(adw::ViewStack, gtk::Widget)>) {
    let mut rows = Vec::new();
    collect_rows(page, &mut rows);
    for row in rows {
        let Some(title) = row.downcast_ref::<adw::PreferencesRow>().map(|row| row.title().to_string()) else { continue };
        if title.is_empty() {
            continue;
        }
        entries.push(SearchEntry { title, path: path.to_owned(), section, tab: tab.clone(), row });
    }
}

/// Fills the result list with the entries matching `query`, best match first.
fn fill_results(list: &gtk::ListBox, entries: &[SearchEntry], matches: &RefCell<Vec<usize>>, query: &str) {
    let mut scored: Vec<(i32, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let by_title = fuzzy_score(query, &entry.title).map(|score| score * 2);
            let by_path = fuzzy_score(query, &entry.path);
            by_title.max(by_path).map(|score| (score, index))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let mut found = matches.borrow_mut();
    found.clear();
    for (_, index) in scored.into_iter().take(MAX_RESULTS) {
        let entry = &entries[index];
        let row = adw::ActionRow::builder()
            .title(entry.title.as_str())
            .subtitle(entry.path.as_str())
            .activatable(true)
            .build();
        list.append(&row);
        found.push(index);
    }
}

/// How well `query` matches `text`. Every character of the query must appear in the text, in order; matches
/// that are consecutive or start a word score higher, and gaps lower. Case is ignored. `None` when it does not match.
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0;
    let mut next = 0;
    let mut previous: Option<usize> = None;
    for wanted in query {
        let found = (next..text.len()).find(|&i| text[i] == wanted)?;
        score += 1;
        if previous.is_some_and(|p| p + 1 == found) {
            score += 4;
        }
        if found == 0 || !text[found - 1].is_alphanumeric() {
            score += 3;
        }
        score -= (found - next).min(5) as i32;
        previous = Some(found);
        next = found + 1;
    }
    Some(score)
}

/// Scrolls the page holding `row` so the row sits near the top, and focuses it.
fn scroll_to_row(row: &gtk::Widget) {
    let Some(scrolled) = row.ancestor(gtk::ScrolledWindow::static_type()).and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok()) else {
        return;
    };
    let Some(content) = scrolled.child() else { return };
    let Some(bounds) = row.compute_bounds(&content) else { return };
    let adjustment = scrolled.vadjustment();
    let top = f64::from(bounds.y()) - 48.0;
    adjustment.set_value(top.clamp(adjustment.lower(), (adjustment.upper() - adjustment.page_size()).max(adjustment.lower())));
    row.grab_focus();
}

/// Joins a section's tab row to its stack, titling and iconing each tab from its page.
fn bind_tabs(switcher: &adw::ViewSwitcher, stack: &adw::ViewStack) {
    switcher.set_stack(Some(stack));
    for page in stack.pages().iter::<adw::ViewStackPage>().flatten() {
        let Ok(child) = page.child().downcast::<adw::PreferencesPage>() else { continue };
        page.set_title(Some(&child.title()));
        page.set_icon_name(child.icon_name().as_deref());
    }
}

/// Binds a combo row to an integer setting that is one of the thumbnail sizes, by position in the row. A value that is not
/// one of them shows as the size `Size::at_most` gives it.
fn bind_thumbnail_size(settings: &Settings, row: &adw::ComboRow, key: &'static str) {
    settings
        .bind(key, row, "selected")
        .mapping(|variant, _| {
            let size = crate::fs::thumbnail::Size::at_most(variant.get::<i32>()?).pixels();
            let index = crate::fs::thumbnail::SIZES.iter().position(|pixels| *pixels == size)?;
            Some((index as u32).to_value())
        })
        .set_mapping(|value, _| {
            let index = value.get::<u32>().ok()? as usize;
            crate::fs::thumbnail::SIZES.get(index).map(|pixels| pixels.to_variant())
        })
        .build();
}

/// Keeps a port row in step with an integer setting, in both directions.
fn bind_port(settings: &Settings, row: &adw::SpinRow, key: &'static str) {
    row.set_value(f64::from(settings.int(key)));
    let settings = settings.clone();
    row.connect_value_notify(move |row| {
        let _ = settings.set_int(key, row.value() as i32);
    });
}

impl PreferencesDialog {
    /// Keeps the rows for what is remembered about each file insensitive while nothing is, and the zoom position's while the
    /// zoom level is not, as a position means nothing without the zoom it goes with.
    fn bind_file_settings(&self, settings: &Settings) {
        let imp = self.imp();
        let rows = [
            imp.remember_position_row.clone(),
            imp.remember_speed_row.clone(),
            imp.remember_volume_row.clone(),
            imp.remember_aspect_row.clone(),
            imp.remember_rotation_row.clone(),
            imp.remember_zoom_row.clone(),
        ];
        let pan = imp.remember_pan_row.clone();
        let update = {
            let settings = settings.clone();
            move || {
                let enabled = settings.boolean("remember.enabled");
                for row in &rows {
                    row.set_sensitive(enabled);
                }
                pan.set_sensitive(enabled && settings.boolean("remember.zoom"));
            }
        };
        update();
        for key in ["remember.enabled", "remember.zoom"] {
            settings.connect_changed(key, {
                let update = update.clone();
                move |_, _| update()
            });
        }
    }

    /// Shows the folder of SVP's own plugin libraries as the row's subtitle, or the bundled ones when none is chosen, and
    /// lets its buttons pick a folder or go back to the bundled libraries.
    fn bind_svp_plugins(&self, settings: &Settings) {
        let imp = self.imp();
        settings
            .bind("video.svp.plugin-directory", &*imp.svp_plugin_directory_row, "subtitle")
            .mapping(|variant, _| {
                let path = variant.str()?;
                Some(if path.is_empty() { "Bundled open-svpflow libraries".to_value() } else { path.to_value() })
            })
            .get_only()
            .build();
        settings
            .bind("video.svp.plugin-directory", &*imp.svp_plugin_clear_button, "visible")
            .mapping(|variant, _| Some((!variant.str()?.is_empty()).to_value()))
            .get_only()
            .build();

        let chooser_settings = settings.clone();
        let weak = self.downgrade();
        imp.svp_plugin_directory_button.connect_clicked(move |_| {
            let Some(dialog) = weak.upgrade() else { return };
            let root = dialog.root().and_downcast::<gtk::Window>();
            let chooser = gtk::FileDialog::builder().title("SVP Plugin Directory").build();
            let current = chooser_settings.string("video.svp.plugin-directory");
            if !current.is_empty() {
                chooser.set_initial_folder(Some(&gio::File::for_path(current.as_str())));
            }
            let settings = chooser_settings.clone();
            chooser.select_folder(root.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let Some(path) = result.ok().and_then(|folder| folder.path()) {
                    let _ = settings.set_string("video.svp.plugin-directory", &path.to_string_lossy());
                }
            });
        });
        imp.svp_plugin_clear_button.connect_clicked({
            let settings = settings.clone();
            move |_| {
                let _ = settings.reset("video.svp.plugin-directory");
            }
        });
    }

    /// Shows the start directory row only for a specific start location, with the chosen folder as
    /// its subtitle, and lets its button pick the folder.
    fn bind_start_directory(&self, settings: &Settings) {
        let imp = self.imp();
        settings
            .bind("start.location", &*imp.start_directory_row, "visible")
            .mapping(|variant, _| Some((variant.str()? == "specific").to_value()))
            .get_only()
            .build();
        settings
            .bind("start.directory", &*imp.start_directory_row, "subtitle")
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
            let current = settings.string("start.directory");
            if !current.is_empty() {
                chooser.set_initial_folder(Some(&gio::File::for_path(current.as_str())));
            }
            let settings = settings.clone();
            chooser.select_folder(root.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let Some(path) = result.ok().and_then(|folder| folder.path()) {
                    let _ = settings.set_string("start.directory", &path.to_string_lossy());
                }
            });
        });
    }
}

#[cfg(test)]
mod search_tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_matches_characters_in_order_only() {
        assert!(fuzzy_score("isz", "Item size").is_some());
        assert!(fuzzy_score("zsi", "Item size").is_none());
        assert!(fuzzy_score("", "Item size").is_some());
    }

    #[test]
    fn consecutive_matches_rank_above_scattered_ones() {
        let together = fuzzy_score("size", "Item size").unwrap();
        let scattered = fuzzy_score("size", "Show items zoomed in every case").unwrap();
        assert!(together > scattered);
    }
}
