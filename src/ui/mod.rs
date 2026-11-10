mod backdrop;
mod binding_dialog;
mod browse_search;
mod browse_view;
mod crop_editor;
mod drag;
mod floating_bar;
mod film_strip;
mod thumbnail_paint;
mod video_preview;
mod thumbnail_bar;
mod image_canvas;
mod item_grid;
mod keybinding_list;
mod key_handler;
mod keybind_label;
mod keybinds_overlay;
mod media_controls;
mod media_viewer;
pub mod message_overlay;
mod media_overlay;
mod overlay_editor;
mod fs_tree;
mod header_bar;
mod history_dialog;
mod overflow_box;
mod overlay_split_view;
mod path_bar;
mod playlist_panel;
mod playlist_toolbar;
mod navigation_bar;
mod preferences;
mod properties;
mod rotated_label;
mod tight_layout;
mod window;

pub use browse_search::BrowseSearch;
pub use browse_view::BrowseView;
pub use floating_bar::FloatingBar;
pub use item_grid::ItemGrid;
pub use media_viewer::{MediaViewer, ShownItem};
pub use message_overlay::{Level, Message, MessageOverlay};
pub use fs_tree::FsTree;
pub use header_bar::HeaderBar;
pub use navigation_bar::NavigationBar;
pub use path_bar::PathBar;
pub use preferences::PreferencesDialog;
pub use overlay_split_view::OverlaySplitView;
pub use overflow_box::OverflowBox;
pub use window::{Destination, Window};

// GTK records the thread that calls `gtk::init()`, and libtest runs each `#[test]` on its own thread. The widget tests
// therefore share one GTK thread: each test sends its scenario to that thread and waits for it to finish.
#[cfg(test)]
mod tests {
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{mpsc, Mutex, OnceLock};
    use std::thread;

    type Job = Box<dyn FnOnce() + Send + 'static>;

    fn gtk_thread() -> &'static Mutex<mpsc::Sender<Job>> {
        static SENDER: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
        SENDER.get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<Job>();
            let (ready, ready_for) = mpsc::channel::<()>();
            thread::spawn(move || {
                // Settings go to a scratch file, not the user's configuration, and the metadata database to a scratch
                // data directory, not the user's.
                let config = std::env::temp_dir().join(format!("rsimgv-test-{}.conf", std::process::id()));
                let _ = std::fs::remove_file(&config);
                std::env::set_var("RSIMGV_CONFIG", &config);
                let data = std::env::temp_dir().join(format!("rsimgv-test-data-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&data);
                let _ = std::fs::create_dir_all(&data);
                std::env::set_var("XDG_DATA_HOME", &data);
                std::env::set_var("RSIMGV_DATA", &data);
                crate::settings::open().expect("the settings open");
                super::fs_tree::tests::init_gtk();
                // The bundled icon theme resolves once registered.
                crate::register_icons();
                let display = gtk::gdk::Display::default().expect("display");
                assert!(gtk::IconTheme::for_display(&display).has_icon("au.topher.rsimgv"), "bundled app icon");
                let _ = ready.send(());
                for job in receiver {
                    job();
                }
            });
            ready_for.recv().expect("the GTK test thread starts");
            Mutex::new(sender)
        })
    }

    /// Runs `scenario` on the shared GTK thread, so each widget test can run on its own.
    fn run_on_gtk_thread(scenario: impl FnOnce() + Send + 'static) {
        let (done, result) = mpsc::channel();
        gtk_thread()
            .lock()
            .expect("the GTK test thread is running")
            .send(Box::new(move || {
                let _ = done.send(panic::catch_unwind(AssertUnwindSafe(scenario)));
            }))
            .expect("the GTK test thread is running");
        match result.recv().expect("the GTK test thread answers") {
            Ok(()) => {}
            Err(payload) => panic::resume_unwind(payload),
        }
    }

    #[test]
    fn fs_tree_widget() {
        run_on_gtk_thread(|| super::fs_tree::tests::run_scenarios());
    }

    #[test]
    fn properties_widget() {
        run_on_gtk_thread(|| super::properties::tests::run_scenarios());
    }

    #[test]
    fn overlay_split_view_widget() {
        run_on_gtk_thread(|| super::overlay_split_view::tests::run_scenarios());
    }

    #[test]
    fn overflow_box_widget() {
        run_on_gtk_thread(|| super::overflow_box::tests::run_scenarios());
    }

    #[test]
    fn path_bar_widget() {
        run_on_gtk_thread(|| super::path_bar::tests::run_scenarios());
    }

    #[test]
    fn image_canvas_widget() {
        run_on_gtk_thread(|| super::image_canvas::tests::run_scenarios());
    }

    #[test]
    fn tight_layout_widget() {
        run_on_gtk_thread(|| super::tight_layout::tests::run_scenarios());
    }

    #[test]
    fn message_overlay_widget() {
        run_on_gtk_thread(|| super::message_overlay::tests::run_scenarios());
    }

    #[test]
    fn navigation_bar_widget() {
        run_on_gtk_thread(|| super::navigation_bar::tests::run_scenarios());
    }

    #[test]
    fn preferences_widget() {
        run_on_gtk_thread(|| super::preferences::tests::run_scenarios());
    }

    #[test]
    fn overlay_editor_widget() {
        run_on_gtk_thread(|| super::overlay_editor::tests::run_scenarios());
    }

    #[test]
    fn browse_view_widget() {
        run_on_gtk_thread(|| super::browse_view::tests::run_scenarios());
    }

    #[test]
    fn media_viewer_widget() {
        run_on_gtk_thread(|| super::media_viewer::tests::run_scenarios());
    }

    #[test]
    fn window_widget() {
        run_on_gtk_thread(|| super::window::tests::run_scenarios());
    }
}
