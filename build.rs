//! Compiles the GSettings backend's GObject subclass (native/backend.c) into the crate.

use std::process::Command;

/// Adds the compile flags from `pkg-config` for `package` to `build`: include paths, and other flags as they are.
fn pkg_config_cflags(build: &mut cc::Build, package: &str) {
    let output = Command::new("pkg-config")
        .args(["--cflags", package])
        .output()
        .unwrap_or_else(|_| panic!("pkg-config is needed for {package}"));
    for flag in String::from_utf8_lossy(&output.stdout).split_whitespace() {
        match flag.strip_prefix("-I") {
            Some(include) => {
                build.include(include);
            }
            None => {
                build.flag(flag);
            }
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=native/backend.c");

    let mut build = cc::Build::new();
    build.file("native/backend.c");
    // The virtual methods take parameters they do not use, as GSettingsBackend defines them.
    build.warnings(false);
    pkg_config_cflags(&mut build, "gio-2.0");
    build.compile("rsimgv_settings_backend");

}
