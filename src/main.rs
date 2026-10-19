mod fs;
mod settings;
mod ui;

use adw::prelude::*;
use gtk::{gdk, gio};

const APP_ID: &str = "au.topher.rimgv";

// Path is provided by the Meson build via RSIMGV_GRESOURCE.
static RESOURCES: &[u8] = include_bytes!(env!("RSIMGV_GRESOURCE"));

fn main() -> glib::ExitCode {
    let resource = gio::Resource::from_data(&glib::Bytes::from_static(RESOURCES))
        .expect("failed to load embedded resources");
    gio::resources_register(&resource);

    let settings = settings::open(APP_ID);

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .resource_base_path("/au/topher/rimgv")
        .build();
    app.connect_startup(|app| {
        load_css();
        register_icons();

        let quit = gio::SimpleAction::new("quit", None);
        let weak = app.downgrade();
        quit.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.quit();
            }
        });
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Ctrl>q"]);

        let preferences = gio::SimpleAction::new("preferences", None);
        let weak = app.downgrade();
        preferences.connect_activate(move |_, _| {
            let Some(app) = weak.upgrade() else { return };
            if let Some(window) = app.active_window().and_downcast::<ui::Window>() {
                window.show_preferences();
            }
        });
        app.add_action(&preferences);
    });
    app.connect_activate(move |app| gtk::prelude::GtkWindowExt::present(&ui::Window::new(app, &settings)));
    app.run()
}

/// Adds the bundled hicolor icon theme to the default theme, so the app's own icons resolve.
pub fn register_icons() {
    let Some(display) = gdk::Display::default() else { return };
    gtk::IconTheme::for_display(&display).add_resource_path("/au/topher/rimgv/icons/hicolor");
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_resource("/au/topher/rimgv/style.css");
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("no display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
