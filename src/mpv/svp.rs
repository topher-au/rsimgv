//! SVP (SmoothVideo Project) frame interpolation for videos played through libmpv, as svpctl does it: a VapourSynth
//! script interpolates the video to a target rate, and mpv loads it through its `vapoursynth` filter. The target
//! follows the playback speed in the display and variable modes, so slow motion and fast forward keep the output
//! rate in range.
//!
//! mpv reloads the script after every seek, as the filter drops VapourSynth when it is reset. So that skipping through a
//! video does not wait on that each time, the filter can be left out until the skipping has stopped.

use crate::settings::Settings;

use std::collections::hash_map::DefaultHasher;
use std::ffi::{c_char, c_int, c_void, CString};
use std::fs::{DirBuilder, Permissions};
use std::hash::{Hash, Hasher};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use gtk::glib;
use serde_json::{json, Value};

/// The whole frame rates the output is rounded to.
const ROUND_TO: f64 = 1.0;
/// The label of the filter in mpv's chain, and the frames the filter buffers.
pub const FILTER_LABEL: &str = "@svp";
const BUFFERED_FRAMES: u32 = 4;
const CONCURRENT_FRAMES: u32 = 8;

/// How the output rate is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The source's own rate, within the range.
    Default,
    /// The target rate, within the range.
    Fixed,
    /// The target rate divided by the speed, so faster playback does not raise the output past what can be made.
    Variable,
    /// The display's rate divided by the speed, so the frames shown a second match the display.
    Display,
}

impl Mode {
    pub fn from_name(name: &str) -> Self {
        match name {
            "fixed" => Mode::Fixed,
            "variable" => Mode::Variable,
            "display" => Mode::Display,
            _ => Mode::Default,
        }
    }
}

/// The output rate settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Rate {
    pub mode: Mode,
    /// The fixed and variable modes' rate, or 0 for the source's.
    pub target: f64,
    /// The output rate is kept within these.
    pub minimum: f64,
    pub maximum: f64,
}

/// The output rate for a video of `source` frames a second, played at `speed`, on a display of `display` hertz when
/// it is known. Without a display rate the display mode keeps the source's rate.
pub fn target_fps(rate: &Rate, source: f64, speed: f64, display: Option<f64>) -> f64 {
    let speed = speed.max(1e-6);
    let base = if rate.target > 0.0 { rate.target } else { source };
    let target = match rate.mode {
        Mode::Default => source,
        Mode::Fixed => base,
        Mode::Variable => base / speed,
        Mode::Display => display.map_or(source, |display| display / speed),
    };
    ((target / ROUND_TO + 0.5).floor() * ROUND_TO).clamp(rate.minimum, rate.maximum)
}

/// The plugin libraries of open-svpflow, which are built beside rsimgv and embedded in it, so the plugins used are always
/// these and never ones installed elsewhere on the system.
static SVPFLOW1: &[u8] = include_bytes!(concat!(env!("RSIMGV_SVP_PLUGINS"), "/libsvpflow1_vs.so"));
static SVPFLOW2: &[u8] = include_bytes!(concat!(env!("RSIMGV_SVP_PLUGINS"), "/libsvpflow2_vs.so"));

const RTLD_NOW: c_int = 2;
const RTLD_NODELETE: c_int = 0x1000;

unsafe extern "C" {
    fn dlopen(file: *const c_char, flags: c_int) -> *mut c_void;
}

/// The paths of the plugin libraries, which are in the script directory.
#[derive(Debug, PartialEq)]
pub struct Plugins {
    pub svp1: PathBuf,
    pub svp2: PathBuf,
}

/// Keeps the plugin at `path` loaded for the rest of the process. VapourSynth unloads a plugin with its core, which goes
/// each time mpv reloads the script, and the plugins never release the OpenCL context and programs they keep in statics:
/// unloaded, those are lost in VRAM, and the plugin makes new ones when it is next loaded.
fn keep_loaded(path: &Path) {
    let Ok(file) = CString::new(path.as_os_str().as_bytes()) else { return };
    // SAFETY: the path is a valid string, and the plugins do nothing on being loaded but set up their own statics.
    if unsafe { dlopen(file.as_ptr(), RTLD_NOW | RTLD_NODELETE) }.is_null() {
        eprintln!("rsimgv: {} could not be kept loaded", path.display());
    }
}

/// The plugins, written out to the script directory the first time they are needed. Each is written under a name of its
/// own and renamed into place, so a plugin that mpv has already loaded is not changed under it.
pub fn plugins() -> Result<&'static Plugins, &'static str> {
    static PLUGINS: OnceLock<Result<Plugins, String>> = OnceLock::new();
    PLUGINS
        .get_or_init(|| {
            let directory = script_directory();
            create_private_directory(&directory).map_err(|err| err.to_string())?;
            let write = |name: &str, bytes: &[u8]| -> std::io::Result<PathBuf> {
                let temporary = directory.join(format!("{name}.{}", std::process::id()));
                let path = directory.join(name);
                std::fs::write(&temporary, bytes)?;
                std::fs::rename(&temporary, &path)?;
                keep_loaded(&path);
                Ok(path)
            };
            Ok(Plugins {
                svp1: write("libsvpflow1_vs.so", SVPFLOW1).map_err(|err| err.to_string())?,
                svp2: write("libsvpflow2_vs.so", SVPFLOW2).map_err(|err| err.to_string())?,
            })
        })
        .as_ref()
        .map_err(String::as_str)
}

/// SVP's parameter string: compact JSON, with no quotes or spaces, as SVP reads it.
fn serialize(params: &Value) -> String {
    params.to_string().chars().filter(|c| *c != '"' && !c.is_whitespace()).collect()
}

/// The VapourSynth script for a video of `source` frames a second, interpolated to `target`. It uses SVP's optical flow
/// on the NVIDIA GPU when `nvof` is set, and its CPU analysis otherwise.
pub fn script(plugins: &Plugins, source: f64, target: f64, nvof: bool) -> String {
    let smooth = serialize(&json!({
        "rate": {"num": (target * 10000.0).round() as i64, "den": 10000, "abs": true},
        "algo": 23,
        "gpuid": 0,
        "gpu_qn": 2,
        "mask": {"cover": 50, "area": 80},
        "scene": {"mode": 0, "blend": true},
    }));
    let analyse = serialize(&json!({
        "block": {"overlap": 0},
        "main": {"search": {"coarse": {"distance": -8, "width": 530}, "type": 2}},
        "refine": [{"thsad": 4000}],
    }));
    let supers = serialize(&json!({"scale": {"up": 0}, "gpu": 1, "rc": true}));
    let threads = std::thread::available_parallelism().map_or(4, |count| count.get());

    let mut loads = String::new();
    for (name, path) in [("svp1", &plugins.svp1), ("svp2", &plugins.svp2)] {
        loads.push_str(&format!(
            "if not hasattr(core, '{name}'):\n    core.std.LoadPlugin('{}')\n",
            path.display()
        ));
    }
    let interpolate = if nvof {
        format!(
            "    smooth = core.svp2.SmoothFps_NVOF(input_m, smoothfps_params, vec_src=input_m8, src=input_m, fps=src_fps)"
        )
    } else {
        format!(
            "    super = core.svp1.Super(input_m8, super_params)\n    vectors = core.svp1.Analyse(super['clip'], super['data'], input_m8, analyze_params)\n    smooth = core.svp2.SmoothFps(input_m, super['clip'], super['data'], vectors['clip'], vectors['data'], smoothfps_params, src=input_m, fps=src_fps)"
        )
    };
    format!(
        "import math\n\
         import vapoursynth as vs\n\
         \n\
         core = vs.core\n\
         core.num_threads = {threads}\n\
         \n\
         src_fps = {source:.6}\n\
         nvof = {nvof}\n\
         nvof_block = 8\n\
         smoothfps_params = '{smooth}'\n\
         super_params = '{supers}'\n\
         analyze_params = '{analyse}'\n\
         \n\
         {loads}\
         clip = video_in.std.Trim(length=5000000)\n\
         \n\
         def interpolate(clip):\n\
         \x20   input_um = clip.resize.Point(format=vs.YUV420P8, dither_type='random')\n\
         \x20   # SVP's optical flow analyses a picture exactly half the size of the video, at least 160x128, so the video is\n\
         \x20   # made a whole number of 8 pixels across and high, and scaled up first if it is too small for that.\n\
         \x20   scale = max(1, 2 * 160 / input_um.width, 2 * 128 / input_um.height)\n\
         \x20   width = math.ceil(input_um.width * scale / nvof_block) * nvof_block\n\
         \x20   height = math.ceil(input_um.height * scale / nvof_block) * nvof_block\n\
         \x20   if scale > 1:\n\
         \x20       input_m = input_um.resize.Bicubic(width=width, height=height, format=vs.YUV420P8)\n\
         \x20   else:\n\
         \x20       width = input_um.width // nvof_block * nvof_block\n\
         \x20       height = input_um.height // nvof_block * nvof_block\n\
         \x20       input_m = input_um.std.Crop(right=input_um.width - width, bottom=input_um.height - height)\n\
         \x20   input_m8 = input_m.resize.Bicubic(width=width // 2, height=height // 2, format=vs.YUV420P8)\n\
         {interpolate}\n\
         \x20   return smooth\n\
         \n\
         smooth = interpolate(clip)\n\
         smooth.set_output()\n",
        nvof = i32::from(nvof),
    )
}

/// The file name for `script`, which differs with its text. mpv keeps a filter whose options have not changed, so a
/// script with new contents has to be at a new path for mpv to load it, and one with the same contents is not reloaded.
pub fn script_name(script: &str) -> String {
    let mut hasher = DefaultHasher::new();
    script.hash(&mut hasher);
    format!("video-{:016x}.py", hasher.finish())
}

/// The mpv filter that runs the script at `path`. The path has mpv's `%length%` quoting, as it can hold the `:` and `,`
/// that otherwise end a filter option.
pub fn filter(path: &Path) -> String {
    let path = path.display().to_string();
    format!(
        "{FILTER_LABEL}:vapoursynth=file=%{}%{path}:buffered-frames={BUFFERED_FRAMES}:concurrent-frames={CONCURRENT_FRAMES}",
        path.len()
    )
}

/// Where the scripts are written: in the user's runtime directory, as mpv runs them as Python and another user must not
/// be able to supply them.
pub fn script_directory() -> PathBuf {
    glib::user_runtime_dir().join("rsimgv").join("svp")
}

/// Creates `directory` for only the user to reach, along with any parents it lacks. One that already exists is closed
/// to others as well.
pub fn create_private_directory(directory: &Path) -> std::io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(directory)?;
    std::fs::set_permissions(directory, Permissions::from_mode(0o700))
}

/// The SVP settings: whether it is on and how the output rate is chosen.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub enabled: bool,
    pub rate: Rate,
    pub nvof: bool,
    /// How long after a seek the filter stays out, or `None` to keep it in through seeks.
    pub seek_bypass: Option<Duration>,
}

impl Config {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.boolean("video.svp.enabled"),
            rate: Rate {
                mode: Mode::from_name(&settings.string("video.svp.fps.mode")),
                target: settings.double("video.svp.fps.target"),
                minimum: f64::from(settings.int("video.svp.fps.minimum")),
                maximum: f64::from(settings.int("video.svp.fps.maximum")),
            },
            nvof: settings.boolean("video.svp.nvof"),
            seek_bypass: settings
                .boolean("video.svp.seek-bypass.enabled")
                .then(|| Duration::from_secs_f64(settings.double("video.svp.seek-bypass.delay"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(mode: Mode) -> Rate {
        Rate { mode, target: 0.0, minimum: 60.0, maximum: 240.0 }
    }

    #[test]
    fn display_mode_divides_the_display_rate_by_the_speed() {
        // 60 Hz at half speed outputs 120 fps, so 60 frames a second are shown.
        assert_eq!(target_fps(&rate(Mode::Display), 24.0, 0.5, Some(60.0)), 120.0);
        // Without a display rate the source's rate is used, within the range.
        assert_eq!(target_fps(&rate(Mode::Display), 24.0, 1.0, None), 60.0);
    }

    #[test]
    fn variable_mode_scales_the_target_by_the_inverse_speed() {
        let fixed = Rate { mode: Mode::Variable, target: 120.0, minimum: 60.0, maximum: 240.0 };
        assert_eq!(target_fps(&fixed, 30.0, 2.0, None), 60.0);
        assert_eq!(target_fps(&fixed, 30.0, 1.0, None), 120.0);
    }

    #[test]
    fn the_output_stays_in_range_and_whole() {
        assert_eq!(target_fps(&rate(Mode::Default), 300.0, 1.0, None), 240.0);
        assert_eq!(target_fps(&rate(Mode::Display), 24.0, 0.7, Some(60.0)), 86.0);
    }

    #[test]
    fn the_script_carries_the_rate_and_the_plugins() {
        let plugins = Plugins { svp1: PathBuf::from("/p/svp1.so"), svp2: PathBuf::from("/p/svp2.so") };
        let text = script(&plugins, 23.976, 120.0, false);
        assert!(text.contains("src_fps = 23.976000"));
        assert!(text.contains("\"num\":1200000") || text.contains("num:1200000"));
        assert!(text.contains("LoadPlugin('/p/svp1.so')"));
        assert!(text.contains("core.svp1.Analyse"));
        let nvof = script(&plugins, 30.0, 60.0, true);
        assert!(nvof.contains("SmoothFps_NVOF"));
    }

    #[test]
    fn the_filter_quotes_the_script_path_by_its_length_in_bytes() {
        let text = filter(Path::new("/home/é,a:b/video.py"));
        assert!(text.contains(":vapoursynth=file=%21%/home/é,a:b/video.py:buffered-frames="), "{text}");
    }

    #[test]
    fn scripts_are_kept_in_the_runtime_directory() {
        assert!(script_directory().starts_with(glib::user_runtime_dir()));
    }

    #[test]
    fn the_private_directory_is_only_open_to_the_user() {
        let root = std::env::temp_dir().join(format!("rsimgv-svp-private-{}", std::process::id()));
        let directory = root.join("a/b");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        create_private_directory(&directory).unwrap();
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&directory), 0o700);

        // One that was left open is closed.
        std::fs::set_permissions(&directory, Permissions::from_mode(0o755)).unwrap();
        create_private_directory(&directory).unwrap();
        assert_eq!(mode(&directory), 0o700);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scripts_are_named_by_their_contents() {
        let plugins = Plugins { svp1: PathBuf::from("/p/svp1.so"), svp2: PathBuf::from("/p/svp2.so") };
        let name =|target| script_name(&script(&plugins, 24.0, target, false));
        assert_eq!(name(60.0), name(60.0));
        assert_ne!(name(60.0), name(120.0), "mpv only reloads a script at a new path");
    }

    #[test]
    fn parameters_have_no_quotes_or_spaces() {
        assert_eq!(serialize(&json!({"a": {"b": 1}})), "{a:{b:1}}");
    }
}
