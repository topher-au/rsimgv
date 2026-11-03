mod binding_dialog;
mod browse_view;
mod drag;
mod floating_bar;
mod film_strip;
mod thumbnail_paint;
mod thumbnail_bar;
mod image_canvas;
mod item_grid;
mod kde_blur;
mod keybinding_list;
mod key_handler;
mod keybind_label;
mod keybinds_overlay;
mod media_controls;
mod media_viewer;
pub mod message_overlay;
mod media_overlay;
mod fs_tree;
mod header_bar;
mod overlay_split_view;
mod path_bar;
mod playlist_panel;
mod playlist_toolbar;
mod navigation_bar;
mod preferences;
mod rotated_label;
mod tight_layout;
mod window;

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
pub use window::{Destination, Window};

// GTK records the thread that calls gtk::init(), and libtest runs each #[test] on its own
// thread, so every widget scenario runs from this single test.
#[cfg(test)]
mod tests {
    #[test]
    fn widget_behaviour() {
        // Settings go to a scratch file, not the user's configuration.
        let config = std::env::temp_dir().join(format!("rsimgv-test-{}.conf", std::process::id()));
        let _ = std::fs::remove_file(&config);
        std::env::set_var("RSIMGV_CONFIG", &config);
        crate::settings::open("au.topher.rimgv");
        super::fs_tree::tests::init_gtk();
        // The bundled icon theme resolves once registered.
        crate::register_icons();
        let display = gtk::gdk::Display::default().expect("display");
        assert!(gtk::IconTheme::for_display(&display).has_icon("au.topher.rsimgv"), "bundled app icon");
        super::fs_tree::tests::run_scenarios();
        super::overlay_split_view::tests::run_scenarios();
        super::path_bar::tests::run_scenarios();
        super::image_canvas::tests::run_scenarios();
        super::tight_layout::tests::run_scenarios();
        super::message_overlay::tests::run_scenarios();
        super::navigation_bar::tests::run_scenarios();
        super::preferences::tests::run_scenarios();
        super::browse_view::tests::run_scenarios();
        super::media_viewer::tests::run_scenarios();
        crate::settings::store_layout_tests();
        super::window::tests::run_scenarios();
    }
}
