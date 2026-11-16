//! The IPC methods, run on the GTK thread against the window. Each takes JSON parameters and returns JSON.
//!
//!   actions.list    {}                       -> {"actions": [{"name", "label", "description", "scope", "contexts", "icon", "defaults",
//!                                                "value", "target"}, ...]}
//!                   value is null unless the action takes a number from actions.run: {"label", "unit", "min", "max",
//!                   "step", "default"}, with the unit seconds, percent, times, degrees or frames. target is null unless
//!                   it takes a target: {"label", "default", "choices": [{"name", "label"}, ...]}.
//!   actions.run     {"name", "value"?, "target"?} -> {}   value and target are passed to an action that takes them
//!                   win.notify takes {"message", "level"?, "seconds"?} instead: it shows the message over the window
//!                   (level is info, success, warning, error or plain; seconds is 4 by default).
//!   window.info     {}                       -> {"current_path", "selected_file", "show_sidebar"}
//!   window.set_show_sidebar {"show"}         -> {}
//!   tabs.list       {}                       -> {"tabs": [{"id", "index", "path", "active"}, ...]}
//!   tabs.current    {}                       -> {"id", "index"}   both null when no tab is open
//!   tabs.open       {"path", "activate"?}    -> {"id", "index"}   activate defaults to false
//!   tabs.activate   {"index"} or {"id"}     -> {}
//!   tabs.close      {"index"}                -> {}
//!   tabs.files      {"tab"}                  -> {"files": [{"name", "path", "is_dir"}, ...]}
//!   tabs.select     {"tab", "file"}          -> {"selected": name}   file is a name or a path
//!   bookmarks.add   {"path", "name"?}        -> {}          name defaults to the folder's own
//!   bookmarks.remove {"path"}                -> {}
//!   bookmarks.list  {}                       -> {"bookmarks": [{"name", "path"}, ...]}
//!   navigate.back   {"n"?}                   -> {"moved"}   n defaults to 1
//!   navigate.forward {"n"?}                  -> {"moved"}
//!   navigate.up     {"n"?}                   -> {"moved"}   stops at the root
//!   navigate.to     {"path"}                 -> {}
//!   navigate.next   {}                       -> {"path"}   the next folder in tree order, or null at the end
//!   navigate.previous {}                     -> {"path"}   the previous folder in tree order, or null at the start
//!   navigate.next_media {}                   -> {"path"}   the next folder with a picture or video, or null
//!   navigate.previous_media {}               -> {"path"}   the previous folder with a picture or video, or null
//!   files.list      {"path"?}                -> {"files": {name: {"path", "is_dir", "size", "modified"}, ...}}
//!                   With no path, the selected tab's items, in the order the tab shows them. With a path, the
//!                   folder's contents from disk, including hidden files, sorted by name.
//!   files.select    {"file"}                 -> {"selected": name}   in the selected tab
//!   files.select_index {"index"}             -> {"selected": name}   negative counts from the end
//!   files.selected  {}                       -> {"name", "index"}   of the selected tab; null when nothing is selected
//!   files.marked    {}                       -> {"paths": [...]}   the items marked in the selected tab, in tab order
//!   files.search    {"query", "path"?, "recursive"?, "limit"?}
//!                                            -> {"path", "total", "files": [{"name", "path", "is_dir", "size", "modified"}, ...],
//!                                                "truncated", "metadata_pending"}
//!                   Finds the items of a folder (the one shown, by default; with its subfolders if recursive) that
//!                   match the search bar's query: keywords, "phrases", and comparisons such as size>10M,
//!                   duration>1:30, width>=1920 or type=video/mp4. total counts all matches and files holds the
//!                   first limit (100 by default, at most 1000), sorted by path. Hidden files are left out. Width,
//!                   height, fps and duration are known only for files whose metadata rsimgv has read: the others
//!                   are asked for and counted in metadata_pending, and match again when the search is repeated.
//!                   The search stops after 20000 items and says so in truncated.
//!   playlist.get    {}                       -> {"total", "current", "shuffled", "items": [{"path", "title", "played", "duration"}, ...]}
//!                   The playlist in the order it plays in. current is the position of the item it is on, or null.
//!   view.screenshot {"format"?, "quality"?, "max_size"?, "crop"?}
//!                                            -> {"format", "mime_type", "width", "height", "source_width", "source_height", "data"}
//!                   The picture or video frame the viewer shows, as base64-encoded PNG at its own size unless asked
//!                   otherwise. format is png or jpeg; quality (1 to 100, 85 by default) applies to jpeg; max_size limits the longest side, never enlarging; crop is {"x", "y", "width", "height"}
//!                   as fractions (0 to 1) of the full picture, made before max_size. width and height are those of
//!                   the result and source_width and source_height those of the picture before cropping.
//!   view.info       {}                       -> {"directory", "filename", "position", "duration", "width", "height",
//!                                                "fps", "speed", "mime_type", "playing", "volume", "muted", "repeat",
//!                                                "slideshow", "rotation", "mirrored", "zoom", "fit", "scale"}
//!                   The item the viewer shows, at its own size in pixels. position and duration are in seconds,
//!                   fps is frames per second, and speed is a multiple of normal. The playback values are null for
//!                   a picture; duration, fps and speed are null when a video does not report them yet.
//!
//!   messages.show  {"text", "level"?, "position"?, "persistent"?, "seconds"?, "dismissible"?} -> {"id"}
//!                   Shows a message over the window. level is info (the default), success, warning, error or plain,
//!                   which is bare text. position is top-left, top-center, top-right, bottom-left, bottom-center (the
//!                   default) or bottom-right. A message goes away by itself after seconds (4 by default), unless
//!                   persistent is true, when it stays until it is dismissed. dismissible puts a close button on it,
//!                   which a persistent message has unless it is false.
//!   messages.update {"id", "text"}           -> {}   changes the text of a message still up
//!   messages.dismiss {"id"}                  -> {}   takes a message down; with no id, every message
//!   thumbnails.get {"paths", "max_size"?, "format"?, "quality"?}
//!                                            -> {"thumbnails": [{"path", "size", "format", "mime_type", "width", "height", "data"}, ...]}
//!                   The still thumbnail of each file, as base64-encoded PNG unless format (png or jpeg) and
//!                   quality (1 to 100) say otherwise, from the thumbnails rsimgv has
//!                   already made for it: the largest at most max_size pixels across (default 256).
//!                   "size" is the pixels of the thumbnail given. "data" is null for a file with no thumbnail yet,
//!                   which the window makes when it shows the file. The thumbnails are read from the disk cache,
//!                   so none are given while the thumbnail cache is turned off.
//!   metadata.get {"paths"}                   -> {"metadata": [{"path", "name", "is_dir", "size", "modified", "mime_type",
//!                                                "media", "media_pending"}, ...]}
//!                   Each file's metadata, in the order of paths. size is in bytes for a file, and modified is in seconds
//!                   since the Unix epoch. media holds a picture's or video's width, height, duration (seconds), frame_rate,
//!                   codec, audio_codec and bit_rate, from what the app has read of it; it is null for a
//!                   folder or other file. A picture or video not read yet is queued, so media is null and media_pending is
//!                   true until a later request finds it. A path that does not exist gives {"path", "error": "no_such_file"}.
//!   files.state     {"path"}                 -> {"path", "hash", "values"}
//!                   The state saved for a file, such as where a video was left, by the file's content hash. values holds
//!                   the keys as saved. A file with no state yet gives {}.
//!   files.set_state {"path", "values"}       -> {"path", "hash", "values"}
//!                   Sets each key in values, or removes it when the value is null, and gives the state afterwards.
//!   files.reset_state {"path", "keys"?}      -> {"path", "hash", "values"}
//!                   Removes each key, or the whole state with no keys, and gives the state afterwards.
//!   settings.list   {}                       -> {"settings": {key: value, ...}}
//!                   Every application setting, by its dotted name, at its current value.
//!   settings.get    {"key"}                  -> {"key", "value"}
//!   settings.set    {"key", "value"}         -> {"key", "value"}   a null value resets the setting to its default
//!   settings.update {"values"}               -> {"settings": {key: value, ...}}   sets or resets each setting
//!   settings.reset  {"keys"?}                -> {}   resets these settings, or every setting with no keys
//!   settings.describe {}                     -> {"settings": [{"key", "type", "description", "default", "value"}, ...]}
//!
//! "tab" in tabs.files and tabs.select is a tab id, or "current" for the selected tab. "index" is a tab's
//! position, counted from 0 in tab order.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use glib::variant::ToVariant;
use gtk::{gio, glib};
use gtk::prelude::*;
use serde_json::{json, Map, Value};
use serde_yaml_ng::{Mapping, Value as YamlValue};

use crate::fs::metadata::{MediaInfo, MediaMetadata};
use crate::fs::thumbnail;
use crate::settings::{app_settings, Settings};
use crate::storage::bookmarks;
use crate::storage::file_settings::FileSettings;
use crate::services::image_encode::{self, Options};
use crate::services::ipc::Error;
use crate::keybindings::actions::{all, find, target_info, value_info, DEFAULT_NOTIFICATION_SECONDS, NOTIFICATION_LEVELS};
use crate::keybindings::actions::Unit;
use crate::search::query::{self, Facts};
use crate::ui::{Level, Message, Window};
use crate::ui::message_overlay::Position;

pub fn handle(window: &Window, method: &str, params: &Value) -> Result<Value, Error> {
    match method {
        "actions.list" => {
            let actions: Vec<Value> = all()
                .iter()
                .map(|action| {
                    json!({
                        "name": action.name,
                        "label": action.label,
                        "description": action.description,
                        "scope": action.default_context().id(),
                        "contexts": action.contexts().iter().map(|scope| scope.id()).collect::<Vec<_>>(),
                        "icon": action.icon,
                        "defaults": action.defaults,
                        "value": value_info(action.name).map(|info| json!({
                            "label": info.label,
                            "unit": unit_name(info.unit),
                            "min": info.min,
                            "max": info.max,
                            "step": info.step,
                            "default": info.default,
                        })),
                        "target": target_info(action.name).map(|info| json!({
                            "label": info.label,
                            "default": info.default,
                            "choices": info.choices.iter().map(|(name, label)| json!({"name": name, "label": label})).collect::<Vec<_>>(),
                        })),
                    })
                })
                .collect();
            Ok(json!({"actions": actions}))
        }
        "actions.run" => {
            let name = text_param(params, "name")?;
            if find(name).is_none() {
                return Err(Error::new("unknown_action", format!("no action named {name}")));
            }
            if name.starts_with("plugin.") {
                return crate::plugins::invoke_action_sync(
                    name,
                    window,
                    std::time::Duration::from_secs(120),
                )
                .map_err(|err| Error::new("plugin_error", err));
            }
            if find(name).is_some_and(|action| action.takes_notification()) {
                let message = text_param(params, "message")?;
                let level = optional_text(params, "level")?.unwrap_or("info");
                if !NOTIFICATION_LEVELS.contains(&level) {
                    return Err(Error::new("invalid_params", format!("no level named {level}")));
                }
                let seconds = match params.get("seconds").filter(|seconds| !seconds.is_null()) {
                    None => DEFAULT_NOTIFICATION_SECONDS,
                    Some(seconds) => seconds
                        .as_f64()
                        .filter(|seconds| *seconds > 0.0)
                        .ok_or_else(|| Error::new("invalid_params", "the parameter seconds must be a number above 0"))?,
                };
                window.show_notification(message, level, seconds);
                return Ok(json!({}));
            }
            let value = params.get("value").and_then(Value::as_f64);
            let target = optional_text(params, "target")?;
            if let Some(target) = target {
                let takes_target = target_info(name).is_some_and(|info| info.has(target));
                if !takes_target {
                    return Err(Error::new("invalid_params", format!("{name} does not take the target {target}")));
                }
            }
            if window.run_action_with(name, value, target) {
                Ok(json!({}))
            } else {
                Err(Error::new("not_run", format!("the window did not run {name}")))
            }
        }
        "view.info" => {
            let Some(item) = window.viewer_item() else {
                return Err(Error::new("no_media", "the viewer is not open"));
            };
            let playback = item.playback.as_ref();
            Ok(json!({
                "directory": item.path.parent().map(|dir| dir.to_string_lossy().into_owned()),
                "filename": item.path.file_name().map(|name| name.to_string_lossy().into_owned()),
                "position": playback.map(|playback| playback.position),
                "duration": playback.and_then(|playback| playback.duration),
                "width": item.width,
                "height": item.height,
                "fps": playback.and_then(|playback| playback.fps),
                "speed": playback.and_then(|playback| playback.speed),
                "mime_type": mime_type(&item.path),
                "playing": item.state.playing,
                "volume": item.state.volume,
                "muted": item.state.muted,
                "repeat": item.state.repeat,
                "slideshow": item.state.slideshow,
                "rotation": item.state.rotation,
                "mirrored": item.state.mirrored,
                "zoom": item.state.zoom,
                "fit": item.state.fit,
                "scale": item.state.scale,
            }))
        }
        "view.screenshot" => {
            let options = Options::from_params(params)?;
            let Some(texture) = window.viewer_screenshot() else {
                return Err(Error::new("no_media", "the viewer is not showing a picture or a video frame"));
            };
            let png = texture.save_to_png_bytes();
            let picture = image_encode::transcode(&png, &options)?;
            Ok(json!({
                "format": picture.format.name(),
                "mime_type": picture.format.mime_type(),
                "width": picture.width,
                "height": picture.height,
                "source_width": texture.width(),
                "source_height": texture.height(),
                "data": glib::base64_encode(&picture.bytes).as_str(),
            }))
        }
        "upscaled.path" => {
            let original = match optional_text(params, "path")? {
                Some(path) => PathBuf::from(path),
                None => window
                    .viewer_item()
                    .map(|item| item.path)
                    .ok_or_else(|| Error::new("no_media", "the viewer is not open"))?,
            };
            let copy = crate::storage::upscaled::path_for(&original)
                .map_err(|err| Error::new("unreadable", format!("cannot read {}: {err}", original.display())))?;
            Ok(json!({"path": copy.to_string_lossy(), "exists": copy.is_file()}))
        }
        "view.reload" => {
            window.reload_viewer();
            Ok(json!({}))
        }
        "thumbnails.get" => {
            let size = match params.get("max_size").filter(|size| !size.is_null()) {
                None => 256,
                Some(size) => size
                    .as_i64()
                    .filter(|size| (1..=2048).contains(size))
                    .ok_or_else(|| Error::new("invalid_params", "the parameter max_size must be a whole number from 1 to 2048"))?,
            };
            let options = Options::from_params(&json!({
                "format": params.get("format"),
                "quality": params.get("quality"),
            }))?;
            let thumbnails = path_list_param(params)?
                .into_iter()
                .map(|path| match thumbnail::cached_still_png(Path::new(path), size as i32) {
                    Some((pixels, png)) => {
                        let picture = image_encode::transcode(&png, &options)?;
                        Ok(json!({
                            "path": path,
                            "size": pixels,
                            "format": picture.format.name(),
                            "mime_type": picture.format.mime_type(),
                            "width": picture.width,
                            "height": picture.height,
                            "data": glib::base64_encode(&picture.bytes).as_str(),
                        }))
                    }
                    None => Ok(json!({"path": path, "data": null})),
                })
                .collect::<Result<Vec<Value>, Error>>()?;
            Ok(json!({"thumbnails": thumbnails}))
        }
        "metadata.get" => {
            let metadata: Vec<Value> = path_list_param(params)?.into_iter().map(file_metadata).collect();
            Ok(json!({"metadata": metadata}))
        }
        "files.state" | "files.state.get" => {
            let path = text_param(params, "path")?;
            let state = file_state(path)?;
            Ok(state_json(path, &state))
        }
        "files.set_state" | "files.state.set" => {
            let path = text_param(params, "path")?;
            let changes = params
                .get("values")
                .or_else(|| params.get("changes"))
                .and_then(Value::as_object)
                .ok_or_else(|| Error::new("invalid_params", "the parameter values must be an object of file state"))?;
            let mut state = file_state(path)?;
            let changes: Vec<(&str, Option<YamlValue>)> =
                changes.iter().map(|(key, value)| (key.as_str(), (!value.is_null()).then(|| json_to_yaml(value)))).collect();
            state
                .apply(changes)
                .map_err(|err| Error::new("file_error", format!("could not save the state of {path}: {err}")))?;
            Ok(state_json(path, &state))
        }
        "files.reset_state" | "files.state.reset" => {
            let path = text_param(params, "path")?;
            let mut state = file_state(path)?;
            let keys: Vec<String> = match params.get("keys") {
                None | Some(Value::Null) => state.values().keys().filter_map(|key| key.as_str().map(str::to_owned)).collect(),
                Some(Value::String(key)) => vec![key.clone()],
                Some(Value::Array(keys)) => keys
                    .iter()
                    .map(|key| {
                        key.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| Error::new("invalid_params", "each key must be text"))
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err(Error::new("invalid_params", "the parameter keys must be a list of settings")),
            };
            let changes: Vec<(&str, Option<YamlValue>)> = keys.iter().map(|key| (key.as_str(), None)).collect();
            state
                .apply(changes)
                .map_err(|err| Error::new("file_error", format!("could not save the state of {path}: {err}")))?;
            Ok(state_json(path, &state))
        }
        "settings.list" => Ok(json!({"settings": all_settings_json()})),
        "settings.get" => {
            let settings = app_settings();
            let key = text_param(params, "key")?;
            if !settings.has(key) {
                return Err(no_setting(key));
            }
            Ok(json!({"key": key, "value": variant_json(&settings.value(key))}))
        }
        "settings.set" => {
            let settings = app_settings();
            let key = text_param(params, "key")?;
            let value = params.get("value").cloned().unwrap_or(Value::Null);
            let saved = set_setting(&settings, key, &value)?;
            Ok(json!({"key": key, "value": saved}))
        }
        "settings.update" => {
            let settings = app_settings();
            let values = params
                .get("values")
                .and_then(Value::as_object)
                .ok_or_else(|| Error::new("invalid_params", "the parameter values must be an object of settings"))?;
            let mut prepared = Vec::with_capacity(values.len());
            for (key, value) in values {
                let change = prepare_setting(&settings, key, value)?;
                prepared.push((key.as_str(), change));
            }
            let mut saved = Map::new();
            for (key, change) in prepared {
                match change {
                    Some(value) => set_variant(&settings, key, &value)?,
                    None => {
                        if !settings.reset_key(key) {
                            return Err(no_setting(key));
                        }
                    }
                }
                saved.insert(key.to_owned(), variant_json(&settings.value(key)));
            }
            Ok(json!({"settings": saved}))
        }
        "settings.reset" => {
            let settings = app_settings();
            let keys: Vec<String> = match params.get("keys") {
                None | Some(Value::Null) => settings.keys(),
                Some(Value::String(key)) => vec![key.clone()],
                Some(Value::Array(keys)) => keys
                    .iter()
                    .map(|key| {
                        key.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| Error::new("invalid_params", "each key must be text"))
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err(Error::new("invalid_params", "the parameter keys must be a list of settings")),
            };
            for key in &keys {
                if !settings.reset_key(key) {
                    return Err(no_setting(key));
                }
            }
            Ok(json!({}))
        }
        "settings.describe" => {
            let settings = app_settings();
            let settings: Vec<Value> = settings
                .keys()
                .into_iter()
                .map(|key| {
                    json!({
                        "key": key,
                        "type": settings.value_type(&key).map_or_else(String::new, |ty| ty.as_str().to_owned()),
                        "description": settings.description(&key).map_or_else(String::new, |text| text.to_string()),
                        "default": settings.default_value(&key).map_or(Value::Null, |value| variant_json(&value)),
                        "value": variant_json(&settings.value(&key)),
                    })
                })
                .collect();
            Ok(json!({"settings": settings}))
        }
        "window.info" => Ok(json!({
            "current_path": window.current_path().map(|path| path.to_string_lossy().into_owned()),
            "selected_file": window.selected_file(),
            "show_sidebar": window.show_sidebar(),
        })),
        "window.set_show_sidebar" => {
            let shown = bool_param(params, "show")?;
            window.set_show_sidebar(shown);
            Ok(json!({}))
        }
        "tabs.list" => {
            let tabs: Vec<Value> = window
                .tabs()
                .iter()
                .map(|tab| {
                    json!({
                        "id": tab.id,
                        "index": tab.index,
                        "path": tab.path.as_ref().map(|path| path.to_string_lossy()),
                        "active": tab.active,
                    })
                })
                .collect();
            Ok(json!({"tabs": tabs}))
        }
        "tabs.current" => {
            let index = window.current_index();
            let id = index.and_then(|index| window.tab_id_at(index));
            Ok(json!({"id": id, "index": index}))
        }
        "tabs.open" => {
            let path = Path::new(text_param(params, "path")?);
            if !path.is_dir() {
                return Err(Error::new("not_a_folder", format!("{} is not a folder", path.display())));
            }
            let activate = params.get("activate").and_then(Value::as_bool).unwrap_or(false);
            let id = window.open_tab_at(path, activate);
            let index = window.tabs().iter().find(|tab| tab.id == id).map_or(0, |tab| tab.index);
            Ok(json!({"id": id, "index": index}))
        }
        "tabs.activate" => {
            let id = if params.get("id").is_some() { id_param(window, params)? } else { index_param(window, params)? };
            window.activate_tab(id);
            Ok(json!({}))
        }
        "tabs.close" => {
            let id = index_param(window, params)?;
            window.close_tab(id);
            Ok(json!({}))
        }
        "tabs.files" => {
            let id = tab_param(window, params)?;
            let files = window.tab_files(id).ok_or_else(|| no_tab(id))?;
            let files: Vec<Value> = files
                .iter()
                .map(|file| json!({"name": file.name, "path": file.path.to_string_lossy(), "is_dir": file.is_dir}))
                .collect();
            Ok(json!({"files": files}))
        }
        "tabs.select" => {
            let id = tab_param(window, params)?;
            let file = text_param(params, "file")?;
            match window.select_in_tab(id, file) {
                Some(name) => Ok(json!({"selected": name})),
                None => Err(Error::new("no_such_file", format!("no item {file} in that tab"))),
            }
        }
        "navigate.back" | "navigate.forward" => {
            let steps = count_param(params, "n")?;
            let forward = method == "navigate.forward";
            let moved = (0..steps).take_while(|_| window.step_history(forward)).count();
            Ok(json!({"moved": moved}))
        }
        "navigate.up" => {
            let levels = count_param(params, "n")?;
            Ok(json!({"moved": window.go_up_levels(levels)}))
        }
        "navigate.next" | "navigate.previous" => {
            let forward = method == "navigate.next";
            let path = window.step_folder(forward).map(|path| path.to_string_lossy().into_owned());
            Ok(json!({"path": path}))
        }
        "navigate.next_media" | "navigate.previous_media" => {
            let forward = method == "navigate.next_media";
            let path = window.step_media_folder(forward).map(|path| path.to_string_lossy().into_owned());
            Ok(json!({"path": path}))
        }
        "navigate.to" => {
            let path = Path::new(text_param(params, "path")?);
            if window.navigate(path) {
                Ok(json!({}))
            } else {
                Err(Error::new("not_a_folder", format!("{} is not a folder", path.display())))
            }
        }
        "files.list" => {
            let files = match params.get("path") {
                None | Some(Value::Null) => {
                    let id = tab_param(window, &json!({"tab": "current"}))?;
                    let listed = window.tab_files(id).ok_or_else(|| no_tab(id))?;
                    listed.into_iter().map(|file| (file.name, file.path)).collect()
                }
                Some(_) => folder_entries(Path::new(text_param(params, "path")?))?,
            };
            let mut map = serde_json::Map::new();
            for (name, path) in files {
                map.insert(name, file_info(&path));
            }
            Ok(json!({"files": map}))
        }
        "files.select" => {
            let id = tab_param(window, &json!({"tab": "current"}))?;
            let file = text_param(params, "file")?;
            match window.select_in_tab(id, file) {
                Some(name) => Ok(json!({"selected": name})),
                None => Err(Error::new("no_such_file", format!("no item {file} in the selected tab"))),
            }
        }
        "files.selected" => Ok(json!({
            "name": window.selected_file(),
            "index": window.selected_position(),
        })),
        "files.marked" => {
            let paths: Vec<String> = window.marked_paths().iter().map(|path| path.to_string_lossy().into_owned()).collect();
            Ok(json!({"paths": paths}))
        }
        "files.search" => search_files(window, params),
        "playlist.get" => Ok(crate::playlist::with(|playlist| {
            let items: Vec<Value> = playlist
                .order()
                .iter()
                .map(|path| {
                    json!({
                        "path": path.to_string_lossy(),
                        "title": playlist.title(path),
                        "played": playlist.is_played(path),
                        "duration": playlist.duration(path),
                    })
                })
                .collect();
            json!({
                "total": items.len(),
                "current": playlist.current(),
                "shuffled": playlist.is_shuffled(),
                "items": items,
            })
        })),
        "files.select_index" => {
            let index = params
                .get("index")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::new("invalid_params", "the parameter index must be a whole number"))?;
            match window.select_at_index(index) {
                Some(name) => Ok(json!({"selected": name})),
                None => Err(Error::new("no_such_file", format!("no item at index {index}"))),
            }
        }
        "bookmarks.add" => {
            let path = Path::new(text_param(params, "path")?);
            let name = match params.get("name") {
                None | Some(Value::Null) => None,
                Some(_) => Some(text_param(params, "name")?.to_owned()),
            };
            window.add_bookmark(path, name);
            Ok(json!({}))
        }
        "bookmarks.list" => {
            let bookmarks = bookmarks::with(|list| {
                list.iter()
                    .map(|bookmark| json!({"name": bookmark.name, "path": bookmark.path.to_string_lossy()}))
                    .collect::<Vec<Value>>()
            });
            Ok(json!({"bookmarks": bookmarks}))
        }
        "messages.show" => {
            let mut message = if optional_bool(params, "persistent")?.unwrap_or(false) {
                Message::persistent(text_param(params, "text")?)
            } else {
                Message::notification(text_param(params, "text")?)
            };
            if let Some(name) = optional_text(params, "level")? {
                message = message.level(Level::from_name(name).ok_or_else(|| Error::new("invalid_params", format!("no level named {name}")))?);
            }
            if let Some(name) = optional_text(params, "position")? {
                message = message.at(Position::from_name(name).ok_or_else(|| Error::new("invalid_params", format!("no position named {name}")))?);
            }
            if let Some(seconds) = params.get("seconds").filter(|seconds| !seconds.is_null()) {
                let seconds = seconds
                    .as_f64()
                    .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                    .ok_or_else(|| Error::new("invalid_params", "the parameter seconds must be a number above 0"))?;
                message = message.lasting(std::time::Duration::from_secs_f64(seconds));
            }
            if let Some(dismissible) = optional_bool(params, "dismissible")? {
                message = message.dismissible(dismissible);
            }
            Ok(json!({"id": window.messages().show(message)}))
        }
        "messages.update" => {
            let id = message_id(params)?;
            if window.messages().update(id, text_param(params, "text")?) {
                Ok(json!({}))
            } else {
                Err(no_message(id))
            }
        }
        "messages.dismiss" => {
            if params.get("id").is_none_or(Value::is_null) {
                window.messages().dismiss_all();
                return Ok(json!({}));
            }
            let id = message_id(params)?;
            if window.messages().dismiss(id) {
                Ok(json!({}))
            } else {
                Err(no_message(id))
            }
        }
        "bookmarks.remove" => {
            let path = Path::new(text_param(params, "path")?);
            window.remove_bookmark(path);
            Ok(json!({}))
        }
        "plugins.list" => Ok(crate::plugins::list()),
        "plugins.settings" => {
            let id = text_param(params, "id")?;
            let settings: Vec<Value> = crate::plugins::plugin_settings(id)
                .into_iter()
                .map(|setting| {
                    json!({
                        "key": setting.key,
                        "label": setting.label,
                        "description": setting.description,
                        "kind": setting.kind,
                        "default": setting.default,
                        "choices": setting.choices,
                        "min": setting.min,
                        "max": setting.max,
                    })
                })
                .collect();
            Ok(json!({"settings": settings, "values": crate::plugins::plugin_config(id)}))
        }
        "plugins.set-setting" => {
            let id = text_param(params, "id")?.to_owned();
            let key = text_param(params, "key")?.to_owned();
            let value = params.get("value").cloned().unwrap_or(Value::Null);
            crate::plugins::set_plugin_setting(&id, &key, value)
                .map_err(|err| Error::new("plugin_error", err))?;
            Ok(json!({}))
        }
        "plugins.reload" => {
            let id = optional_text(params, "id")?;
            crate::plugins::reload(id).map_err(|err| Error::new("plugin_error", err))?;
            Ok(json!({}))
        }
        _ => Err(Error::new("unknown_method", format!("no method named {method}"))),
    }
}

/// The MIME type of a file, guessed from its name.
fn mime_type(path: &Path) -> Option<String> {
    let (content_type, _) = gio::content_type_guess(Some(path), None::<&[u8]>);
    gio::content_type_get_mime_type(&content_type).map(|mime| mime.to_string())
}

/// The paths a request names in its "paths" list, each as text.
fn path_list_param(params: &Value) -> Result<Vec<&str>, Error> {
    params
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::new("invalid_params", "the parameter paths must be a list of paths"))?
        .iter()
        .map(|path| path.as_str().ok_or_else(|| Error::new("invalid_params", "each path must be text")))
        .collect()
}

/// What the file system and the app say about the file at `path`. A picture or video whose metadata is not read yet is
/// queued to be read, so its "media" is null and "media_pending" is true until a later request finds it.
fn file_metadata(path: &str) -> Value {
    let file = Path::new(path);
    let Ok(metadata) = std::fs::metadata(file) else {
        return json!({"path": path, "error": "no_such_file"});
    };
    let is_dir = metadata.is_dir();
    let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok());
    let mime = mime_type(file);
    let mut info = json!({
        "path": path,
        "name": file.file_name().map(|name| name.to_string_lossy().into_owned()),
        "is_dir": is_dir,
        "size": (!is_dir).then(|| metadata.len()),
        "modified": modified.map(|since| since.as_secs_f64()),
        "mime_type": mime,
        "media": null,
        "media_pending": false,
    });
    let picture_or_video = mime.as_deref().is_some_and(|mime| mime.starts_with("image/") || mime.starts_with("video/"));
    if !is_dir && picture_or_video {
        // The playlist keys its metadata by the modification time in whole seconds, and the size.
        let mtime = modified.map_or(0, |since| since.as_secs() as i64);
        let media_info = MediaInfo::shared();
        match media_info.get(file, mtime, metadata.len()) {
            Some(read) => info["media"] = media_json(&read),
            None => {
                media_info.request(file, mtime, metadata.len(), || true, || {});
                info["media_pending"] = json!(true);
            }
        }
    }
    info
}

fn media_json(media: &MediaMetadata) -> Value {
    json!({
        "width": media.width,
        "height": media.height,
        "duration": media.duration,
        "frame_rate": media.frame_rate,
        "codec": media.codec,
        "audio_codec": media.audio_codec,
        "bit_rate": media.bit_rate,
    })
}

/// The state saved for the file at `path`. Reading it hashes the file, so it follows the file by content.
fn file_state(path: &str) -> Result<FileSettings, Error> {
    FileSettings::for_path(Path::new(path))
        .map_err(|err| Error::new("file_error", format!("could not read the state of {path}: {err}")))
}

/// What the IPC says about a file's state.
fn state_json(path: &str, state: &FileSettings) -> Value {
    json!({"path": path, "hash": state.hash(), "values": yaml_mapping_json(state.values())})
}

fn yaml_mapping_json(mapping: &Mapping) -> Value {
    let mut values = Map::new();
    for (key, value) in mapping {
        let key = key.as_str().map_or_else(|| format!("{key:?}"), str::to_owned);
        values.insert(key, yaml_to_json(value));
    }
    Value::Object(values)
}

fn yaml_to_json(value: &YamlValue) -> Value {
    match value {
        YamlValue::Null => Value::Null,
        YamlValue::Bool(flag) => Value::Bool(*flag),
        YamlValue::Number(number) => number
            .as_i64()
            .map(Value::from)
            .or_else(|| number.as_u64().map(Value::from))
            .or_else(|| number.as_f64().and_then(serde_json::Number::from_f64).map(Value::Number))
            .unwrap_or(Value::Null),
        YamlValue::String(text) => Value::String(text.clone()),
        YamlValue::Sequence(items) => Value::Array(items.iter().map(yaml_to_json).collect()),
        YamlValue::Mapping(mapping) => yaml_mapping_json(mapping),
        YamlValue::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

fn json_to_yaml(value: &Value) -> YamlValue {
    match value {
        Value::Null => YamlValue::Null,
        Value::Bool(flag) => YamlValue::Bool(*flag),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                YamlValue::Number(integer.into())
            } else if let Some(unsigned) = number.as_u64() {
                YamlValue::Number(unsigned.into())
            } else {
                YamlValue::Number(number.as_f64().unwrap_or(0.0).into())
            }
        }
        Value::String(text) => YamlValue::String(text.clone()),
        Value::Array(items) => YamlValue::Sequence(items.iter().map(json_to_yaml).collect()),
        Value::Object(values) => YamlValue::Mapping(
            values.iter().map(|(key, value)| (YamlValue::String(key.clone()), json_to_yaml(value))).collect(),
        ),
    }
}

fn all_settings_json() -> Value {
    let settings = app_settings();
    let mut values = Map::new();
    for key in settings.keys() {
        values.insert(key.clone(), variant_json(&settings.value(&key)));
    }
    Value::Object(values)
}

/// The JSON form of a setting's value. A type without a JSON equivalent is its GVariant text.
fn variant_json(value: &glib::Variant) -> Value {
    match value.type_().as_str() {
        "b" => value.get::<bool>().map_or(Value::Null, Value::Bool),
        "y" => value.get::<u8>().map_or(Value::Null, Value::from),
        "n" => value.get::<i16>().map_or(Value::Null, Value::from),
        "q" => value.get::<u16>().map_or(Value::Null, Value::from),
        "i" => value.get::<i32>().map_or(Value::Null, Value::from),
        "u" => value.get::<u32>().map_or(Value::Null, Value::from),
        "x" => value.get::<i64>().map_or(Value::Null, Value::from),
        "t" => value.get::<u64>().map_or(Value::Null, Value::from),
        "d" => value.get::<f64>().and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number),
        "s" => value.str().map_or(Value::Null, |text| Value::String(text.to_owned())),
        _ if value.type_().is_array() || value.type_().is_tuple() => {
            Value::Array(value.iter().map(|item| variant_json(&item)).collect())
        }
        _ => Value::String(value.print(false).to_string()),
    }
}

/// The GVariant of `value` as type `ty`, if it fits.
fn json_to_variant(value: &Value, ty: &glib::VariantTy) -> Option<glib::Variant> {
    match ty.as_str() {
        "b" => value.as_bool().map(|flag| flag.to_variant()),
        "y" => value.as_u64().and_then(|number| u8::try_from(number).ok()).map(|number| number.to_variant()),
        "n" => value.as_i64().and_then(|number| i16::try_from(number).ok()).map(|number| number.to_variant()),
        "q" => value.as_u64().and_then(|number| u16::try_from(number).ok()).map(|number| number.to_variant()),
        "i" => value.as_i64().and_then(|number| i32::try_from(number).ok()).map(|number| number.to_variant()),
        "u" => value.as_u64().and_then(|number| u32::try_from(number).ok()).map(|number| number.to_variant()),
        "x" => value.as_i64().map(|number| number.to_variant()),
        "t" => value.as_u64().map(|number| number.to_variant()),
        "d" => value.as_f64().map(|number| number.to_variant()),
        "s" => value.as_str().map(|text| text.to_variant()),
        _ if ty.is_array() => {
            let items: Option<Vec<_>> = value.as_array()?.iter().map(|item| json_to_variant(item, ty.element())).collect();
            Some(glib::Variant::array_from_iter_with_type(ty.element(), items?))
        }
        _ if ty.is_tuple() => {
            let items = value.as_array()?;
            if items.len() != ty.n_items() {
                return None;
            }
            let items: Option<Vec<_>> = items.iter().zip(members(ty)).map(|(item, member)| json_to_variant(item, member)).collect();
            Some(glib::Variant::tuple_from_iter(items?))
        }
        _ => glib::Variant::parse(Some(ty), value.as_str()?).ok(),
    }
}

fn members(ty: &glib::VariantTy) -> impl Iterator<Item = &glib::VariantTy> {
    std::iter::successors(ty.first(), |member| member.next())
}

/// The variant to set the setting `key` to, or None to reset it. A null value resets it.
fn prepare_setting(settings: &Settings, key: &str, value: &Value) -> Result<Option<glib::Variant>, Error> {
    if !settings.has(key) {
        return Err(no_setting(key));
    }
    if value.is_null() {
        return Ok(None);
    }
    let Some(ty) = settings.value_type(key) else { return Err(no_setting(key)) };
    let variant = json_to_variant(value, &ty)
        .ok_or_else(|| Error::new("invalid_params", format!("the setting {key} takes a value of type {}", ty.as_str())))?;
    if !settings.range_check(key, &variant) {
        return Err(Error::new("invalid_params", format!("the value for the setting {key} is out of range")));
    }
    Ok(Some(variant))
}

fn set_variant(settings: &Settings, key: &str, value: &glib::Variant) -> Result<(), Error> {
    settings.set_value(key, value).map_err(|err| Error::new("invalid_params", err.to_string()))
}

/// Sets the setting `key` to `value`, or resets it for a null, and gives the value saved.
fn set_setting(settings: &Settings, key: &str, value: &Value) -> Result<Value, Error> {
    match prepare_setting(settings, key, value)? {
        Some(value) => set_variant(settings, key, &value)?,
        None => {
            if !settings.reset_key(key) {
                return Err(no_setting(key));
            }
        }
    }
    Ok(variant_json(&settings.value(key)))
}

fn no_setting(key: &str) -> Error {
    Error::new("no_such_setting", format!("no setting named {key}"))
}

fn text_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, Error> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("invalid_params", format!("the parameter {key} must be text")))
}

fn optional_text<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>, Error> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => text_param(params, key).map(Some),
    }
}

fn optional_bool(params: &Value, key: &str) -> Result<Option<bool>, Error> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => bool_param(params, key).map(Some),
    }
}

fn message_id(params: &Value) -> Result<u64, Error> {
    params
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::new("invalid_params", "the parameter id must be a message's id"))
}

fn no_message(id: u64) -> Error {
    Error::new("no_such_message", format!("no message with id {id} is up"))
}

fn bool_param(params: &Value, key: &str) -> Result<bool, Error> {
    params
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::new("invalid_params", format!("the parameter {key} must be true or false")))
}

/// The id of the tab at the position given by the request's "index".
fn index_param(window: &Window, params: &Value) -> Result<u64, Error> {
    let index = params
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::new("invalid_params", "the parameter index must be a tab position"))?;
    window
        .tab_id_at(index as usize)
        .ok_or_else(|| Error::new("no_such_tab", format!("no tab at index {index}")))
}

/// The id of the tab named by the request's "id", which must be an open tab.
fn id_param(window: &Window, params: &Value) -> Result<u64, Error> {
    let id = params
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::new("invalid_params", "the parameter id must be a tab id"))?;
    if window.has_tab(id) {
        Ok(id)
    } else {
        Err(no_tab(id))
    }
}

/// The tab a request names: its id, or the selected tab for "current".
fn tab_param(window: &Window, params: &Value) -> Result<u64, Error> {
    match params.get("tab") {
        Some(Value::String(text)) if text.as_str() == "current" => {
            let index = window.current_index().ok_or_else(|| Error::new("no_such_tab", "no tab is open"))?;
            window.tab_id_at(index).ok_or_else(|| Error::new("no_such_tab", "no tab is open"))
        }
        Some(value) => {
            let id = value
                .as_u64()
                .ok_or_else(|| Error::new("invalid_params", "tab must be an id or \"current\""))?;
            if window.has_tab(id) {
                Ok(id)
            } else {
                Err(no_tab(id))
            }
        }
        None => Err(Error::new("invalid_params", "the parameter tab is required")),
    }
}

/// A count parameter, such as the steps of a navigation. Defaults to 1 when absent.
fn count_param(params: &Value, key: &str) -> Result<u32, Error> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(1),
        Some(value) => value
            .as_u64()
            .and_then(|count| u32::try_from(count).ok())
            .ok_or_else(|| Error::new("invalid_params", format!("the parameter {key} must be a whole number"))),
    }
}

/// The name and path of each item in a folder on disk, sorted by name, hidden items included.
fn folder_entries(folder: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
    let read = std::fs::read_dir(folder)
        .map_err(|err| Error::new("not_a_folder", format!("cannot list {}: {err}", folder.display())))?;
    let mut entries: Vec<(String, PathBuf)> = read
        .flatten()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
        .collect();
    entries.sort();
    Ok(entries)
}

/// What the IPC says about one item: its path, whether it is a folder, its size in bytes for a file, and when it
/// was last modified, in seconds since the Unix epoch.
fn file_info(path: &Path) -> Value {
    let metadata = std::fs::metadata(path).ok();
    let is_dir = metadata.as_ref().is_some_and(|metadata| metadata.is_dir());
    let size = metadata.as_ref().filter(|metadata| !metadata.is_dir()).map(|metadata| metadata.len());
    let modified = metadata
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs_f64());
    json!({"path": path.to_string_lossy(), "is_dir": is_dir, "size": size, "modified": modified})
}

/// The most items a search looks at, because it runs on the window's thread.
const SEARCH_SCAN_LIMIT: usize = 20_000;
const SEARCH_DEFAULT_LIMIT: usize = 100;
const SEARCH_MAX_LIMIT: usize = 1000;

fn unit_name(unit: Unit) -> &'static str {
    match unit {
        Unit::Seconds => "seconds",
        Unit::Percent => "percent",
        Unit::Times => "times",
        Unit::Degrees => "degrees",
        Unit::Frames => "frames",
    }
}

/// `files.search`: the items under a folder that match a search bar query.
fn search_files(window: &Window, params: &Value) -> Result<Value, Error> {
    let text = text_param(params, "query")?;
    let tokens = query::parse(text);
    if tokens.is_empty() {
        return Err(Error::new("invalid_params", "the parameter query must not be empty"));
    }
    let recursive = optional_bool(params, "recursive")?.unwrap_or(false);
    let limit = match params.get("limit").filter(|limit| !limit.is_null()) {
        None => SEARCH_DEFAULT_LIMIT,
        Some(limit) => limit
            .as_u64()
            .filter(|limit| (1..=SEARCH_MAX_LIMIT as u64).contains(limit))
            .ok_or_else(|| Error::new("invalid_params", format!("the parameter limit must be a whole number from 1 to {SEARCH_MAX_LIMIT}")))?
            as usize,
    };
    let root = match optional_text(params, "path")? {
        Some(path) => PathBuf::from(path),
        None => window.current_path().ok_or_else(|| Error::new("no_folder", "no folder is shown"))?,
    };
    if !root.is_dir() {
        return Err(Error::new("not_a_folder", format!("{} is not a folder", root.display())));
    }

    let needs_metadata = tokens.iter().any(query::Token::needs_metadata);
    let media_info = MediaInfo::shared();
    let mut pending = 0usize;
    let mut scanned = 0usize;
    let mut truncated = false;
    let mut matches: Vec<PathBuf> = Vec::new();
    let mut folders = vec![root.clone()];
    'walk: while let Some(folder) = folders.pop() {
        let Ok(read) = std::fs::read_dir(&folder) else { continue };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            scanned += 1;
            if scanned > SEARCH_SCAN_LIMIT {
                truncated = true;
                break 'walk;
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::metadata(&path) else { continue };
            if metadata.is_dir() && recursive {
                folders.push(path.clone());
            }
            let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |since| since.as_secs() as i64);
            let mime = mime_type(&path).unwrap_or_default();
            let mut facts = Facts {
                name,
                content_type: mime.clone(),
                size: if metadata.is_dir() { 0 } else { metadata.len() },
                modified,
                ..Facts::default()
            };
            let picture_or_video = mime.starts_with("image/") || mime.starts_with("video/");
            if needs_metadata && !metadata.is_dir() && picture_or_video {
                match media_info.get(&path, modified, metadata.len()) {
                    Some(read) => {
                        facts.width = read.width;
                        facts.height = read.height;
                        facts.fps = read.frame_rate;
                        facts.duration = read.duration;
                    }
                    None => {
                        media_info.request(&path, modified, metadata.len(), || true, || {});
                        pending += 1;
                    }
                }
            }
            if query::matches_all(&tokens, &facts) {
                matches.push(path);
            }
        }
    }
    matches.sort();
    let total = matches.len();
    let files: Vec<Value> = matches
        .iter()
        .take(limit)
        .map(|path| {
            let mut info = file_info(path);
            info["name"] = json!(path.file_name().map(|name| name.to_string_lossy().into_owned()));
            info
        })
        .collect();
    Ok(json!({
        "path": root.to_string_lossy(),
        "total": total,
        "files": files,
        "truncated": truncated,
        "metadata_pending": pending,
    }))
}

fn no_tab(id: u64) -> Error {
    Error::new("no_such_tab", format!("no tab {id}"))
}
