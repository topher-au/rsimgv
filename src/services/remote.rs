//! The IPC methods, run on the GTK thread against the window. Each takes JSON parameters and returns JSON.
//!
//!   actions.list    {}                       -> {"actions": [{"name", "label", "description", "scope", "contexts", "icon", "defaults"}, ...]}
//!   actions.run     {"name", "value"?}       -> {}   value is passed to an action that takes one
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
//!   view.screenshot {}                       -> {"format": "png", "width", "height", "data"}
//!                   The picture or video frame the viewer shows, at its own size, as base64-encoded PNG.
//!   view.info       {}                       -> {"directory", "filename", "position", "duration", "width", "height",
//!                                                "fps", "speed", "mime_type"}
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
//!   thumbnails.get {"paths", "max_size"?}   -> {"thumbnails": [{"path", "size", "format", "data"}, ...]}
//!                   The still thumbnail of each file, as base64-encoded PNG, from the thumbnails rsimgv has
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
//!
//! "tab" in tabs.files and tabs.select is a tab id, or "current" for the selected tab. "index" is a tab's
//! position, counted from 0 in tab order.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use gtk::{gio, glib};
use gtk::prelude::*;
use serde_json::{json, Value};

use crate::fs::metadata::{MediaInfo, MediaMetadata};
use crate::fs::thumbnail;
use crate::storage::bookmarks;
use crate::services::ipc::Error;
use crate::keybindings::actions::{find, ACTIONS, DEFAULT_NOTIFICATION_SECONDS, NOTIFICATION_LEVELS};
use crate::ui::{Level, Message, Window};
use crate::ui::message_overlay::Position;

pub fn handle(window: &Window, method: &str, params: &Value) -> Result<Value, Error> {
    match method {
        "actions.list" => {
            let actions: Vec<Value> = ACTIONS
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
            if window.run_action(name, value) {
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
            }))
        }
        "view.screenshot" => {
            let Some(texture) = window.viewer_screenshot() else {
                return Err(Error::new("no_media", "the viewer is not showing a picture or a video frame"));
            };
            let png = texture.save_to_png_bytes();
            Ok(json!({
                "format": "png",
                "width": texture.width(),
                "height": texture.height(),
                "data": glib::base64_encode(&png).as_str(),
            }))
        }
        "thumbnails.get" => {
            let size = match params.get("max_size").filter(|size| !size.is_null()) {
                None => 256,
                Some(size) => size
                    .as_i64()
                    .filter(|size| (1..=2048).contains(size))
                    .ok_or_else(|| Error::new("invalid_params", "the parameter max_size must be a whole number from 1 to 2048"))?,
            };
            let thumbnails: Vec<Value> = path_list_param(params)?
                .into_iter()
                .map(|path| match thumbnail::cached_still_png(Path::new(path), size as i32) {
                    Some((pixels, png)) => json!({
                        "path": path,
                        "size": pixels,
                        "format": "png",
                        "data": glib::base64_encode(&png).as_str(),
                    }),
                    None => json!({"path": path, "data": null}),
                })
                .collect();
            Ok(json!({"thumbnails": thumbnails}))
        }
        "metadata.get" => {
            let metadata: Vec<Value> = path_list_param(params)?.into_iter().map(file_metadata).collect();
            Ok(json!({"metadata": metadata}))
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

fn no_tab(id: u64) -> Error {
    Error::new("no_such_tab", format!("no tab {id}"))
}
