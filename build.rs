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

    if std::env::var_os("CARGO_FEATURE_PYTHON").is_some() {
        build_pygobject_bridge();
    }
}

/// Compiles the PyGObject bridge with the Python feature. The interpreter is chosen exactly as PyO3 chooses
/// it, so the headers and the linked libpython belong to the same Python installation.
fn build_pygobject_bridge() {
    println!("cargo:rerun-if-changed=native/pygobject_bridge.c");
    println!("cargo:rerun-if-env-changed=PYO3_PYTHON");

    let python = std::env::var("PYO3_PYTHON").unwrap_or_else(|_| "python3".to_owned());
    let output = Command::new(&python)
        .args(["-c", "import sysconfig; print(sysconfig.get_paths()['include'])"])
        .output()
        .unwrap_or_else(|err| panic!("could not run {python} to find its include directory: {err}"));
    if !output.status.success() {
        panic!("{python} could not report its include directory: {}", String::from_utf8_lossy(&output.stderr));
    }
    let include = String::from_utf8_lossy(&output.stdout).trim().to_owned();

    let mut build = cc::Build::new();
    build.file("native/pygobject_bridge.c");
    build.include(include);
    // PyGObject's API table is a process-wide pointer. Keeping this copy hidden stops it from clashing
    // with the table the gi extension modules hold.
    build.flag("-fvisibility=hidden");
    build.warnings(false);
    pkg_config_cflags(&mut build, "gtk4");
    pkg_config_cflags(&mut build, "pygobject-3.0");
    build.compile("rsimgv_pygobject_bridge");
}
