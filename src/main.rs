mod fs;
mod ipc;
mod remote;
mod bookmarks;
mod keybindings;
mod settings;
mod ui;

use adw::prelude::*;
use gtk::{gdk, gio};
use std::time::Duration;

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
    let startup_settings = settings.clone();
    app.connect_startup(move |app| {
        load_css();
        register_icons();
        keybindings::load();
        bookmarks::load();
        let config = ipc::Config {
            socket: startup_settings.boolean("ipc-socket"),
            tcp_port: startup_settings.int("ipc-tcp-port") as u16,
            http_port: startup_settings.int("ipc-http-port") as u16,
        };
        if config.any_enabled() {
            serve_ipc(app, &config);
        }

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

/// Starts the transports `config` enables, and carries out their calls on the active window, here on the GTK
/// thread, which polls for them.
fn serve_ipc(app: &adw::Application, config: &ipc::Config) {
    let calls = ipc::start(config);
    let weak = app.downgrade();
    glib::timeout_add_local(Duration::from_millis(20), move || {
        let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
        while let Ok(call) = calls.try_recv() {
            let answer = match app.active_window().and_downcast::<ui::Window>() {
                Some(window) => remote::handle(&window, &call.method, &call.params),
                None => Err(ipc::Error::new("no_window", "no window is open")),
            };
            call.respond(answer);
        }
        glib::ControlFlow::Continue
    });
}
