//! The layout settings files had before the typed format in `storage.rs`, which still loads. Settings were grouped
//! under named mappings, such as `window:` with `width:` in it. A value is read as the type its key has in the schema,
//! so the file's YAML scalars are never taken as the setting's type. The next save writes it in the typed format.

use std::collections::BTreeMap;

use glib::prelude::ToVariant;
use gtk::glib;
use serde_yaml_ng::Value;

use super::storage::SchemaKey;

/// The legacy values, by the group and name each was saved under.
pub type Legacy = BTreeMap<(String, String), Value>;

/// Where each setting is kept in the file: its schema key, then its group and its name in that group.
const LAYOUT: &[(&str, &str, &str)] = &[
    ("show-hidden", "browse", "show-hidden"),
    ("media-only", "browse", "media-only"),
    ("show-images", "browse", "show-images"),
    ("show-videos", "browse", "show-videos"),
    ("show-directories", "browse", "show-directories"),
    ("collapse-on-navigate", "browse", "collapse-on-navigate"),
    ("item-size", "browse", "item-size"),
    ("filename-display", "browse", "filename-display"),
    ("info-layout", "browse", "info-layout"),
    ("info-fields", "browse", "info-fields"),
    ("window-width", "window", "width"),
    ("window-height", "window", "height"),
    ("window-maximized", "window", "maximized"),
    ("sidebar-width", "window", "sidebar-width"),
    ("sidebar-bookmarks-expanded", "window", "sidebar-bookmarks-expanded"),
    ("sidebar-filesystem-expanded", "window", "sidebar-filesystem-expanded"),
    ("folder-end", "navigation", "folder-end"),
    ("remember-folder-sort", "navigation", "remember-folder-sort"),
    ("start-location", "start", "location"),
    ("start-directory", "start", "directory"),
    ("last-directory", "start", "last-directory"),
    ("last-file", "start", "last-file"),
    ("open-fullscreen", "viewer", "open-fullscreen"),
    ("auto-hide-cursor", "viewer", "auto-hide-cursor"),
    ("image-preloading", "viewer", "image-preloading"),
    ("thumbnail-threads", "thumbnails", "threads"),
    ("thumbnail-cache", "thumbnails", "cache"),
    ("thumbnail-aggression", "thumbnails", "aggression"),
    ("thumbnail-unload-offscreen", "thumbnails", "unload-offscreen"),
    ("viewer-blurred-background", "viewer", "blurred-background"),
    ("viewer-thumbnail-bar", "viewer", "thumbnail-bar.side"),
    ("viewer-thumbnail-bar-shown", "viewer", "thumbnail-bar.shown"),
    ("viewer-thumbnail-bar-size", "viewer", "thumbnail-bar.size"),
    ("viewer-thumbnail-bar-overlay", "viewer", "thumbnail-bar.overlay"),
    ("viewer-thumbnail-bar-pinned", "viewer", "thumbnail-bar.pinned"),
    ("image-info-in-title", "viewer", "image-info-in-title"),
    ("image-fit", "image", "fit"),
    ("keep-fit-mode", "image", "keep-fit"),
    ("original-focus", "image", "original-focus"),
    ("unlock-minimum-zoom", "image", "unlock-minimum-zoom"),
    ("zoom-step", "image", "zoom-step"),
    ("expand-up-to-factor", "image", "expand-up-to-factor"),
    ("zoom-indicator", "image", "zoom-indicator"),
    ("smooth-scrolling", "image", "smooth-scrolling"),
    ("mouse-back", "mouse", "back"),
    ("mouse-forward", "mouse", "forward"),
    ("scroll-image", "mouse", "scroll-image"),
    ("scroll-video", "mouse", "scroll-video"),
    ("scroll-speed", "mouse", "scroll-speed"),
    ("scaling-filter", "image", "scaling-filter"),
    ("smooth-upscaling", "image", "smooth-upscaling"),
    ("video-backend", "video", "backend"),
    ("video-start-delay", "video", "start-delay"),
    ("mpv-args", "video", "mpv-args"),
    ("mpv-opengl", "video", "opengl"),
    ("overlay-layout", "viewer", "overlay-layout"),
    ("seek-overlay", "video", "seek-overlay"),
    ("seek-overlay-bar", "video", "seek-overlay-bar"),
    ("seek-overlay-time", "video", "seek-overlay-time"),
    ("seek-overlay-time-format", "video", "seek-overlay-time-format"),
    ("seek-overlay-position", "video", "seek-overlay-position"),
    ("seek-overlay-seconds", "video", "seek-overlay-seconds"),
    ("seek-overlay-width", "video", "seek-overlay-width"),
    ("seek-overlay-always", "video", "seek-overlay-always"),
    ("remember-file-settings", "files", "remember"),
    ("remember-position", "files", "position"),
    ("remember-speed", "files", "speed"),
    ("remember-volume", "files", "volume"),
    ("remember-rotation", "files", "rotation"),
    ("remember-zoom", "files", "zoom"),
    ("remember-pan", "files", "pan"),
    ("sidebar-animations", "window", "sidebar-animations"),
    ("sidebar-animation-ms", "window", "sidebar-animation-ms"),
    ("video-crop", "video", "crop.enabled"),
    ("video-crop-method", "video", "crop.method"),
    ("video-crop-trim", "video", "crop.trim"),
    ("video-backdrop", "video", "backdrop.enabled"),
    ("video-backdrop-zoom", "video", "backdrop.zoom"),
    ("video-backdrop-blur", "video", "backdrop.blur"),
    ("video-backdrop-dim", "video", "backdrop.dim"),
    ("video-controls-locked", "video", "controls.locked"),
    ("video-controls-x", "video", "controls.x"),
    ("video-controls-y", "video", "controls.y"),
    ("video-controls-width", "video", "controls.width"),
    ("ipc-socket", "remote", "socket"),
    ("ipc-socket-path", "remote", "socket-path"),
    ("ipc-tcp-enabled", "remote", "tcp-enabled"),
    ("ipc-tcp-port", "remote", "tcp-port"),
    ("ipc-http-enabled", "remote", "http-enabled"),
    ("ipc-http-port", "remote", "http-port"),
    ("ipc-external-access", "remote", "external-access"),
];

/// Where settings were kept before the layout in `LAYOUT`, for the keys that have moved. A file in the old
/// layout still loads them, and the next save writes them where `LAYOUT` puts them.
const PREVIOUS: &[(&str, &str, &str)] = &[
    ("collapse-on-navigate", "tree", "collapse-on-navigate"),
    ("start-location", "general", "start-location"),
    ("start-directory", "general", "start-directory"),
    ("last-directory", "general", "last-directory"),
    ("last-file", "general", "last-file"),
    ("open-fullscreen", "general", "open-fullscreen"),
    ("auto-hide-cursor", "general", "auto-hide-cursor"),
    ("smooth-scrolling", "general", "smooth-scrolling"),
    ("image-info-in-title", "general", "image-info-in-title"),
    ("zoom-indicator", "general", "zoom-indicator"),
    ("image-fit", "view", "image-fit"),
    ("keep-fit-mode", "view", "keep-fit-mode"),
    ("expand-up-to-factor", "view", "expand-up-to-factor"),
    ("zoom-step", "view", "zoom-step"),
    ("video-backend", "view", "video-backend"),
    ("mpv-args", "view", "mpv-args"),
    ("scaling-filter", "view", "scaling-filter"),
    ("smooth-upscaling", "view", "smooth-upscaling"),
    ("viewer-blurred-background", "view", "blurred-background"),
    ("viewer-thumbnail-bar", "view", "thumbnail-bar.side"),
    ("viewer-thumbnail-bar-shown", "view", "thumbnail-bar.shown"),
    ("viewer-thumbnail-bar-size", "view", "thumbnail-bar.size"),
    ("viewer-thumbnail-bar-overlay", "view", "thumbnail-bar.overlay"),
    ("viewer-thumbnail-bar-pinned", "view", "thumbnail-bar.pinned"),
    ("ipc-socket", "ipc", "socket"),
    ("ipc-tcp-port", "ipc", "tcp-port"),
    ("ipc-http-port", "ipc", "http-port"),
    ("ipc-external-access", "ipc", "external-access"),
];

/// The group for a setting that is not in `LAYOUT`. Its name is the schema key.
const FALLBACK_GROUP: &str = "settings";

/// The group and name a schema key was saved under.
fn location(key: &str) -> (String, String) {
    LAYOUT
        .iter()
        .find(|(schema_key, _, _)| *schema_key == key)
        .map_or_else(
            || (FALLBACK_GROUP.to_owned(), key.to_owned()),
            |(_, group, name)| ((*group).to_owned(), (*name).to_owned()),
        )
}

/// The legacy value of the schema key `key`, from the place it is saved now or the place it was saved before.
fn find<'a>(legacy: &'a Legacy, key: &str) -> Option<&'a Value> {
    legacy.get(&location(key)).or_else(|| {
        PREVIOUS
            .iter()
            .find(|(schema_key, _, _)| *schema_key == key)
            .and_then(|(_, group, name)| legacy.get(&((*group).to_owned(), (*name).to_owned())))
    })
}

/// The typed values the legacy settings give for `keys`, by full key path. A value that does not fit its key's
/// type is left out, and the key takes its schema default.
pub fn migrate(legacy: &Legacy, keys: &[SchemaKey]) -> Vec<(String, glib::Variant)> {
    let mut migrated = Vec::new();
    for key in keys {
        let Some(value) = find(legacy, &key.name) else { continue };
        let Ok(ty) = glib::VariantTy::new(&key.ty) else { continue };
        match from_yaml(value, ty) {
            Some(variant) => migrated.push((key.path.clone(), variant)),
            None => eprintln!("rsimgv: the saved value of {} does not fit its type {}; using the default", key.name, key.ty),
        }
    }
    // A port used to turn its transport on, so a file that sets one without the toggle keeps that transport on.
    for (toggle, port) in [("ipc-tcp-enabled", "ipc-tcp-port"), ("ipc-http-enabled", "ipc-http-port")] {
        let port_set = find(legacy, port).and_then(Value::as_i64).is_some_and(|port| port > 0);
        if !port_set || find(legacy, toggle).is_some() {
            continue;
        }
        if let Some(key) = keys.iter().find(|key| key.name == toggle) {
            migrated.push((key.path.clone(), true.to_variant()));
        }
    }
    migrated
}

/// A YAML value as the GVariant of type `ty`, if it fits that type.
fn from_yaml(value: &Value, ty: &glib::VariantTy) -> Option<glib::Variant> {
    match ty.as_str() {
        "b" => value.as_bool().map(|b| b.to_variant()),
        "i" => value.as_i64().and_then(|n| i32::try_from(n).ok()).map(|n| n.to_variant()),
        "u" => value.as_u64().and_then(|n| u32::try_from(n).ok()).map(|n| n.to_variant()),
        "x" => value.as_i64().map(|n| n.to_variant()),
        "t" => value.as_u64().map(|n| n.to_variant()),
        "d" => value.as_f64().map(|n| n.to_variant()),
        "s" => value.as_str().map(|s| s.to_variant()),
        "as" => value
            .as_sequence()
            .and_then(|items| items.iter().map(|item| item.as_str().map(str::to_owned)).collect::<Option<Vec<String>>>())
            .map(|list| list.to_variant())
            // Lists saved as GVariant text by earlier versions still load.
            .or_else(|| value.as_str().and_then(|text| glib::Variant::parse(Some(ty), text).ok())),
        // Anything else was saved as GVariant text.
        _ => value.as_str().and_then(|text| glib::Variant::parse(Some(ty), text).ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str, name: &str, ty: &str) -> SchemaKey {
        SchemaKey { path: path.to_owned(), name: name.to_owned(), ty: ty.to_owned() }
    }

    fn legacy_of(entries: &[(&str, &str, Value)]) -> Legacy {
        entries.iter().map(|(group, name, value)| (((*group).to_owned(), (*name).to_owned()), value.clone())).collect()
    }

    #[test]
    fn a_legacy_value_takes_the_type_of_its_key() {
        let legacy = legacy_of(&[
            ("window", "width", Value::Number(960.into())),
            ("view", "thumbnail-bar.size", Value::Number(96.into())),
        ]);
        let keys = [key("/p/window-width", "window-width", "i"), key("/p/viewer-thumbnail-bar-size", "viewer-thumbnail-bar-size", "u")];
        let migrated = migrate(&legacy, &keys);
        assert_eq!(migrated, vec![("/p/window-width".to_owned(), 960i32.to_variant()), ("/p/viewer-thumbnail-bar-size".to_owned(), 96u32.to_variant())]);
    }

    #[test]
    fn a_legacy_value_that_does_not_fit_its_type_is_left_out() {
        let legacy = legacy_of(&[("window", "width", Value::String("wide".to_owned()))]);
        assert!(migrate(&legacy, &[key("/p/window-width", "window-width", "i")]).is_empty());
    }
}
