//! GSettings for the application, stored in `~/.config/rsimgv/rsimgv.conf` (or `RSIMGV_CONFIG`) as YAML
//! through the `yaml` backend (backend.c, store.rs).

mod store;

pub use store::config_path;

/// Checks how settings are laid out in the file.
#[cfg(test)]
pub(crate) fn store_layout_tests() {
    store::tests::run_scenarios();
}

extern "C" {
    fn rsimgv_settings_backend_register();
}

/// Opens the application's settings. The first call registers the YAML backend and selects it, so
/// every `gio::Settings` in the process reads and writes the configuration file.
pub fn open(app_id: &str) -> gio::Settings {
    std::env::set_var("GSETTINGS_BACKEND", "yaml");
    unsafe { rsimgv_settings_backend_register() };
    gio::Settings::new(app_id)
}
