mod browse_view;
mod fs_tree;
mod header_bar;
mod overlay_split_view;
mod path_bar;
mod navigation_bar;
mod preferences;
mod window;

pub use browse_view::BrowseView;
pub use fs_tree::FsTree;
pub use header_bar::HeaderBar;
pub use navigation_bar::NavigationBar;
pub use path_bar::PathBar;
pub use preferences::PreferencesDialog;
pub use overlay_split_view::OverlaySplitView;
pub use window::Window;

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
        super::navigation_bar::tests::run_scenarios();
        super::preferences::tests::run_scenarios();
        super::browse_view::tests::run_scenarios();
        super::window::tests::run_scenarios();
    }
}
