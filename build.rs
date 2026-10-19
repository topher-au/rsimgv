//! Compiles the GSettings backend's GObject subclass (src/settings/backend.c) into the crate.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/settings/backend.c");

    let output = Command::new("pkg-config")
        .args(["--cflags", "gio-2.0"])
        .output()
        .expect("pkg-config is needed to build the settings backend");
    let flags = String::from_utf8_lossy(&output.stdout).into_owned();

    let mut build = cc::Build::new();
    build.file("src/settings/backend.c");
    // The virtual methods take parameters they do not use, as GSettingsBackend defines them.
    build.warnings(false);
    for flag in flags.split_whitespace() {
        match flag.strip_prefix("-I") {
            Some(include) => {
                build.include(include);
            }
            None => {
                build.flag(flag);
            }
        }
    }
    build.compile("rsimgv_settings_backend");
}
