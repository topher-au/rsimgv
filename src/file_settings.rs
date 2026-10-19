//! Settings kept for each file, such as where a video was left. They are found by the file's contents rather
//! than its path, so they follow a file that is moved or renamed.
//!
//! A file is known by its movie hash (see `moviehash`), and its settings are a YAML mapping in
//! `file_settings/<first character>/<second character>/<hash>` in the config directory:
//!
//! ```yaml
//! position: 93.5
//! speed: 1.25
//! ```

// Nothing reads or writes per-file settings yet; the first caller removes this.
#![allow(dead_code)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_yaml_ng::{Mapping, Value};

use crate::settings::config_path;

/// How many bytes are hashed from each end of the file.
const CHUNK: u64 = 65536;

/// The file's movie hash, as 16 lowercase hex digits: its size plus every little-endian 64-bit word in its first
/// and last 64 KiB, wrapping. Each chunk is padded with zeros to 64 KiB, and a file shorter than that is hashed
/// from the start twice, as the reference implementation does.
pub fn moviehash(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut hash = size;
    let mut chunk = vec![0u8; CHUNK as usize];
    for offset in [0, size.saturating_sub(CHUNK)] {
        file.seek(SeekFrom::Start(offset))?;
        chunk.fill(0);
        let mut filled = 0;
        while filled < chunk.len() {
            match file.read(&mut chunk[filled..])? {
                0 => break,
                read => filled += read,
            }
        }
        for word in chunk.chunks_exact(8) {
            hash = hash.wrapping_add(u64::from_le_bytes(word.try_into().expect("8 bytes")));
        }
    }
    Ok(format!("{hash:016x}"))
}

/// Where the settings for the file with movie hash `hash` are kept.
pub fn settings_file(hash: &str) -> PathBuf {
    let mut characters = hash.chars();
    let first = characters.next().unwrap_or('0').to_string();
    let second = characters.next().unwrap_or('0').to_string();
    config_path().with_file_name("file_settings").join(first).join(second).join(hash)
}

/// One file's settings, read from its settings file and written back on each change.
pub struct FileSettings {
    file: PathBuf,
    values: Mapping,
}

impl FileSettings {
    /// The settings of the file at `path`. A file with none yet starts empty. `Err` if the file cannot be read.
    pub fn for_path(path: &Path) -> std::io::Result<Self> {
        Ok(Self::at(settings_file(&moviehash(path)?)))
    }

    /// The settings kept in `file`. A missing or unreadable file gives none.
    fn at(file: PathBuf) -> Self {
        let values = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_yaml_ng::from_str::<Value>(&text).ok())
            .and_then(|value| value.as_mapping().cloned())
            .unwrap_or_default();
        Self { file, values }
    }

    /// The value of `key`, if it is set.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Sets `key` to `value` and saves the settings.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) -> std::io::Result<()> {
        self.values.insert(Value::String(key.to_owned()), value.into());
        self.save()
    }

    /// Unsets `key` and saves the settings. A file left with no settings is removed.
    pub fn remove(&mut self, key: &str) -> std::io::Result<()> {
        if self.values.remove(key).is_none() {
            return Ok(());
        }
        self.save()
    }

    /// Writes the settings through a temporary file, so a crash never leaves them half-written. With none left,
    /// the file is removed instead.
    fn save(&self) -> std::io::Result<()> {
        if self.values.is_empty() {
            return match std::fs::remove_file(&self.file) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            };
        }
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_yaml_ng::to_string(&Value::Mapping(self.values.clone())).map_err(std::io::Error::other)?;
        let temp = self.file.with_extension("tmp");
        std::fs::write(&temp, text)?;
        std::fs::rename(temp, &self.file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(name: &str, data: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!("rsimgv-moviehash-{}-{name}", std::process::id()));
        std::fs::write(&path, data).unwrap();
        let hash = moviehash(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        hash
    }

    #[test]
    fn hashes_match_the_reference_implementation() {
        // The expected hashes are from the Python reference.
        assert_eq!(hash_of("empty", b""), "0000000000000000");
        assert_eq!(hash_of("short", b"hello world"), "deee40ded9a1a3bf");
        let big: Vec<u8> = (0..200_000u32).map(|i| ((i * 7 + i / 3) % 256) as u8).collect();
        assert_eq!(hash_of("big", &big), "b5345e8b0a374deb");
    }

    #[test]
    fn settings_are_kept_under_the_hash_characters() {
        let file = settings_file("b5345e8b0a374deb");
        assert!(file.ends_with("file_settings/b/5/b5345e8b0a374deb"));
    }

    #[test]
    fn settings_round_trip_and_an_empty_file_is_removed() {
        let file = std::env::temp_dir().join(format!("rsimgv-file-settings-{}", std::process::id())).join("a/b/ab");
        let mut settings = FileSettings::at(file.clone());
        settings.set("position", 93.5).unwrap();
        settings.set("note", "kept").unwrap();
        let loaded = FileSettings::at(file.clone());
        assert_eq!(loaded.get_f64("position"), Some(93.5));
        assert_eq!(loaded.get_str("note"), Some("kept"));
        settings.remove("position").unwrap();
        settings.remove("note").unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(file.parent().unwrap().parent().unwrap().parent().unwrap());
    }
}
