//! Compiles the GSettings backend's GObject subclass (src/settings/backend.c) into the crate, and KWin's blur
//! protocol: its client code is generated from the protocol description, beside the blur's C wrapper.

use std::env;
use std::path::PathBuf;
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
    println!("cargo:rerun-if-changed=src/settings/backend.c");

    let mut build = cc::Build::new();
    build.file("src/settings/backend.c");
    // The virtual methods take parameters they do not use, as GSettingsBackend defines them.
    build.warnings(false);
    pkg_config_cflags(&mut build, "gio-2.0");
    build.compile("rsimgv_settings_backend");

    println!("cargo:rerun-if-changed=src/ui/kde_blur.c");
    println!("cargo:rerun-if-changed=data/protocols/kde-blur.xml");
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let protocol = "data/protocols/kde-blur.xml";
    for (mode, file) in [("client-header", "kde-blur-client.h"), ("private-code", "kde-blur-protocol.c")] {
        let status = Command::new("wayland-scanner")
            .args([mode, protocol])
            .arg(out.join(file))
            .status()
            .expect("wayland-scanner is needed for the KDE blur protocol");
        assert!(status.success(), "wayland-scanner failed on {protocol}");
    }
    let mut build = cc::Build::new();
    build.file("src/ui/kde_blur.c").file(out.join("kde-blur-protocol.c")).include(&out);
    pkg_config_cflags(&mut build, "wayland-client");
    build.compile("rsimgv_kde_blur");
    for flag in ["wayland-client"] {
        println!("cargo:rustc-link-lib={flag}");
    }
}
