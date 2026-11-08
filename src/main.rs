#![recursion_limit = "256"]
mod fs;
mod hash;
mod logging;
mod overlay;
mod playlist;
mod media;
mod services;
mod storage;
mod keybindings;
mod mpv;
mod search;
mod settings;
mod ui;

use crate::settings::Settings;

use adw::prelude::*;
use services::{ipc, remote};
use storage::bookmarks;
use gtk::{gdk, gio};
use gio::prelude::ApplicationCommandLineExt;
use std::cell::RefCell;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// The settings about making thumbnails.
const THUMBNAIL_KEYS: [&str; 7] = [
    "thumbnails.threads",
    "thumbnails.cache",
    "thumbnails.unload-offscreen",
    "thumbnails.aggression",
    "thumbnails.max-image-size",
    "thumbnails.max-video-size",
    "thumbnails.crop-video",
];

/// Gives the thumbnails what the settings say about making them.
fn apply_thumbnail_settings(settings: &Settings) {
    let thumbnails = fs::thumbnail::Thumbnails::shared();
    thumbnails.set_thread_count(settings.int("thumbnails.threads").max(1) as usize);
    thumbnails.set_disk_cache(settings.boolean("thumbnails.cache"));
    thumbnails.set_unload_offscreen(settings.boolean("thumbnails.unload-offscreen"));
    thumbnails.set_aggression(fs::thumbnail::Aggression::from_name(&settings.string("thumbnails.aggression")));
    thumbnails.set_max_size(fs::thumbnail::Media::Picture, fs::thumbnail::Size::at_most(settings.int("thumbnails.max-image-size")));
    thumbnails.set_max_size(fs::thumbnail::Media::Video, fs::thumbnail::Size::at_most(settings.int("thumbnails.max-video-size")));
    thumbnails.set_crop_video(settings.boolean("thumbnails.crop-video"));
}

/// The settings that decide which IPC transports run, and on which ports.
const IPC_KEYS: [&str; 7] = [
    "remote.socket.enabled",
    "remote.socket.path",
    "remote.tcp.enabled",
    "remote.tcp.port",
    "remote.http.enabled",
    "remote.http.port",
    "remote.external-access",
];

const APP_ID: &str = "au.topher.rsimgv";

/// The terminal's interrupt signal, which Ctrl+C sends.
const SIGINT: i32 = 2;

/// Set when the interrupt signal arrives, and cleared by the main loop, which then quits the app.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn on_interrupt(_: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Temporary debugging aid: prints where a segmentation fault or abort happened, then aborts.
const SIGSEGV: i32 = 11;
const SIGABRT: i32 = 6;

extern "C" fn on_crash(signum: i32) {
    eprintln!("rsimgv: signal {signum}, backtrace:\n{}", std::backtrace::Backtrace::force_capture());
    std::process::abort();
}

// Path is provided by the Meson build via RSIMGV_GRESOURCE.
static RESOURCES: &[u8] = include_bytes!(env!("RSIMGV_GRESOURCE"));

fn main() -> glib::ExitCode {
    unsafe {
        signal(SIGSEGV, on_crash);
        signal(SIGABRT, on_crash);
    }
    let resource = gio::Resource::from_data(&glib::Bytes::from_static(RESOURCES))
        .expect("failed to load embedded resources");
    gio::resources_register(&resource);

    let settings = match settings::open() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("rsimgv: {err:#}");
            return glib::ExitCode::FAILURE;
        }
    };

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .resource_base_path("/au/topher/rsimgv")
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.add_main_option(
        "version",
        glib::Char::from(b'V'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Show the version and exit",
        None,
    );
    app.add_main_option(
        "verbose",
        glib::Char::from(b'v'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Print debug output",
        None,
    );
    app.add_main_option(
        "new-window",
        glib::Char(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Open the paths, or the default folder, in a new window",
        None,
    );
    app.add_main_option(
        "new-tab",
        glib::Char(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Open the paths, or the default folder, in a new tab",
        None,
    );
    app.add_main_option(
        "select-item",
        glib::Char(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Show each path's containing folder with the path selected, instead of opening the path",
        None,
    );
    app.set_option_context_parameter_string(Some("[FILE_OR_FOLDER...]"));
    app.connect_handle_local_options(|_, options| {
        if options.contains("verbose") {
            logging::set_verbose(true);
        }
        if options.contains("version") {
            println!("rsimgv {}", env!("CARGO_PKG_VERSION"));
            return ControlFlow::Break(glib::ExitCode::SUCCESS);
        }
        ControlFlow::Continue(())
    });
    let (calls, receiver) = mpsc::channel();
    let ipc_server = Rc::new(RefCell::new(ipc::Server::new(calls)));
    serve_ipc(&app, receiver);
    for key in IPC_KEYS {
        settings.connect_changed(key, {
            let settings = settings.clone();
            let ipc_server = ipc_server.clone();
            move |_, _| ipc_server.borrow_mut().apply(&ipc_config(&settings))
        });
    }
    for key in THUMBNAIL_KEYS {
        settings.connect_changed(key, |settings, _| apply_thumbnail_settings(settings));
    }
    let shutdown_server = ipc_server.clone();
    let startup_settings = settings.clone();
    app.connect_startup(move |app| {
        load_css();
        register_icons();
        keybindings::load();
        bookmarks::load();
        playlist::load();
        ipc_server.borrow_mut().apply(&ipc_config(&startup_settings));
        apply_thumbnail_settings(&startup_settings);

        let quit = gio::SimpleAction::new("quit", None);
        let weak = app.downgrade();
        quit.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.quit();
            }
        });
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Ctrl>q"]);
        // Ctrl+C in the terminal that started the app runs the quit action, as Ctrl+Q does, so the app shuts down the
        // same way. The handler only sets a flag, and the main loop checks it.
        // SAFETY: the handler does nothing but store to an atomic, which is safe to do in a signal handler.
        unsafe {
            signal(SIGINT, on_interrupt);
        }
        let weak = app.downgrade();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            if INTERRUPTED.swap(false, Ordering::SeqCst) {
                if let Some(app) = weak.upgrade() {
                    gio::prelude::ActionGroupExt::activate_action(&app, "quit", None);
                }
            }
            glib::ControlFlow::Continue
        });

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
    // The paths are opened in the active window, navigating its current tab to the first and opening the rest
    // in new tabs. With --new-window or --new-tab, every path opens in a new tab, in a new window for the first
    // flag. With a flag and no paths, the new window or tab opens at the default folder.
    let command_line_settings = settings.clone();
    app.connect_command_line(move |app, command_line| {
        let options = command_line.options_dict();
        if options.contains("verbose") {
            logging::set_verbose(true);
        }
        let new_window = options.contains("new-window");
        let new_tab = options.contains("new-tab");
        let select_item = options.contains("select-item");
        let args: Vec<_> = command_line
            .arguments()
            .into_iter()
            .skip(1)
            .filter(|arg| !arg.to_string_lossy().starts_with('-'))
            .collect();
        if args.is_empty() && !new_window && !new_tab {
            app.activate();
            return glib::ExitCode::SUCCESS;
        }
        let existing = app.active_window().and_downcast::<ui::Window>();
        let created = new_window || existing.is_none();
        let window = match existing {
            Some(window) if !new_window => window,
            _ => {
                let window = ui::Window::new(app, &command_line_settings);
                gtk::prelude::GtkWindowExt::present(&window);
                window
            }
        };
        if args.is_empty() {
            // A new window already opens at the default folder.
            if new_tab && !created {
                window.open_new_tab_at_default();
            }
            return glib::ExitCode::SUCCESS;
        }
        // A new window starts with a tab at the default folder, which gives way to the paths opened in it.
        let initial = if created && (new_window || new_tab) { window.tab_id_at(0) } else { None };
        let mut status = glib::ExitCode::SUCCESS;
        let mut opened = 0;
        for (index, arg) in args.iter().enumerate() {
            let destination = if new_window || new_tab {
                ui::Destination::NewTab { activate: index == 0 }
            } else if index == 0 {
                ui::Destination::CurrentTab
            } else {
                ui::Destination::NewTab { activate: false }
            };
            let done = command_line
                .create_file_for_arg(arg)
                .path()
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| window.open_path(&path, destination, select_item));
            if done {
                opened += 1;
            } else {
                command_line.printerr_literal(&format!("rsimgv: {}: no such file or folder\n", arg.to_string_lossy()));
                status = glib::ExitCode::FAILURE;
            }
        }
        if let Some(id) = initial.filter(|_| opened > 0) {
            window.close_tab(id);
        }
        status
    });
    app.connect_activate(move |app| gtk::prelude::GtkWindowExt::present(&ui::Window::new(app, &settings)));
    app.connect_shutdown(move |_| shutdown_server.borrow_mut().stop());
    mpv::remove_stale_sockets();
    app.run()
}

/// The IPC transports the settings ask for.
fn ipc_config(settings: &Settings) -> ipc::Config {
    let socket_path = settings.string("remote.socket.path");
    let socket = settings.boolean("remote.socket.enabled").then(|| {
        if socket_path.is_empty() { ipc::default_socket_path() } else { PathBuf::from(socket_path.as_str()) }
    });
    let port = |enabled: &str, key: &str| {
        if settings.boolean(enabled) { settings.int(key) as u16 } else { 0 }
    };
    ipc::Config {
        socket,
        tcp_port: port("remote.tcp.enabled", "remote.tcp.port"),
        http_port: port("remote.http.enabled", "remote.http.port"),
        external: settings.boolean("remote.external-access"),
    }
}

/// Adds the bundled hicolor icon theme to the default theme, so the app's own icons resolve.
pub fn register_icons() {
    let Some(display) = gdk::Display::default() else { return };
    gtk::IconTheme::for_display(&display).add_resource_path("/au/topher/rsimgv/icons/hicolor");
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_resource("/au/topher/rsimgv/style.css");
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
