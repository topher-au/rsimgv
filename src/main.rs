mod fs;
mod ipc;
mod remote;
mod bookmarks;
mod file_settings;
mod keybindings;
mod mpv;
mod settings;
mod ui;

use adw::prelude::*;
use gtk::{gdk, gio};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

/// The settings that decide which IPC transports run, and on which ports.
const IPC_KEYS: [&str; 4] = ["ipc-socket", "ipc-tcp-port", "ipc-http-port", "ipc-external-access"];

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
    let (calls, receiver) = mpsc::channel();
    let ipc_server = Rc::new(RefCell::new(ipc::Server::new(calls)));
    serve_ipc(&app, receiver);
    for key in IPC_KEYS {
        settings.connect_changed(Some(key), {
            let settings = settings.clone();
            let ipc_server = ipc_server.clone();
            move |_, _| ipc_server.borrow_mut().apply(&ipc_config(&settings))
        });
    }
    let startup_settings = settings.clone();
    app.connect_startup(move |app| {
        load_css();
        register_icons();
        keybindings::load();
        bookmarks::load();
        ipc_server.borrow_mut().apply(&ipc_config(&startup_settings));

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
    app.connect_shutdown(|_| ipc::remove_socket());
    mpv::remove_stale_sockets();
    app.run()
}

/// The IPC transports the settings ask for.
fn ipc_config(settings: &gio::Settings) -> ipc::Config {
    ipc::Config {
        socket: settings.boolean("ipc-socket"),
        tcp_port: settings.int("ipc-tcp-port") as u16,
        http_port: settings.int("ipc-http-port") as u16,
        external: settings.boolean("ipc-external-access"),
    }
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

/// Carries out the IPC calls from the transports on the active window, here on the GTK thread, which polls for them.
fn serve_ipc(app: &adw::Application, calls: mpsc::Receiver<ipc::Call>) {
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
