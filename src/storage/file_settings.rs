//! Settings kept for each file, such as where a video was left. They are found by the file's contents rather
//! than its path, so they follow a file that is moved or renamed.
//!
//! A file is known by its content hash (see `content_hash`), and its settings are one YAML file,
//! `file_settings/<first character>/<hash>.yaml` in the config directory, with every value at its root:
//!
//! ```yaml
//! position: 93.5
//! speed: 1.25
//! ```
//!
//! The viewer keeps a video's play position, speed, volume (`volume`, and `muted` if it was) and a picture's or video's
//! turn and mirror (`rotation` in degrees, and `mirrored`) here, and a picture's fit mode, zoom and pan. The preferences
//! choose which of these are kept.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_yaml_ng::{Mapping, Value};
use xxhash_rust::xxh3::xxh3_128;

use crate::settings::config_path;

/// How many bytes are hashed from each end of the file, for the movie hash.
const CHUNK: u64 = 65536;

/// How many bytes are read at each sample from the middle of the file.
const SAMPLE: u64 = 65536;

/// How many samples are mixed into the content hash.
const ROUNDS: usize = 8;

/// The file's content hash, as 32 lowercase hex digits. It starts from the movie hash (see `movie_hash_of`), which
/// seeds an XXH3-128 state. Then up to `ROUNDS` samples of `SAMPLE` bytes from the middle of the file are mixed in,
/// each at an offset drawn from the state. An offset that was already sampled is redrawn after folding it into the
/// state. A file too small to hold one sample is hashed from its movie hash alone.
pub fn content_hash(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut state = mix(&[&movie_hash_of(&mut file, size)?.to_le_bytes()]);
    // The sample offsets are CHUNK, CHUNK + SAMPLE, and so on up to the last one that ends before the final chunk.
    // A short file has fewer slots than rounds, and every one of its slots is sampled.
    let slots = size.checked_sub(2 * CHUNK + SAMPLE).map_or(0, |span| span / SAMPLE + 1);
    let mut seen = HashSet::new();
    for _ in 0..slots.min(ROUNDS as u64) {
        let mut offset = sample_offset(&state, slots);
        while !seen.insert(offset) {
            state = mix(&[&state, b"again", &offset.to_le_bytes()]);
            offset = sample_offset(&state, slots);
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut sample = vec![0u8; SAMPLE as usize];
        file.read_exact(&mut sample)?;
        state = mix(&[&state, &offset.to_le_bytes(), &sample]);
    }
    Ok(format!("{:032x}", u128::from_be_bytes(state)))
}

/// The movie hash of the file: its size plus every little-endian 64-bit word in its first and last 64 KiB, wrapping.
/// Each chunk is padded with zeros to 64 KiB, and a file shorter than that is hashed from the start twice, as the
/// reference implementation does.
fn movie_hash_of(file: &mut File, size: u64) -> std::io::Result<u64> {
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
    Ok(hash)
}

/// The XXH3-128 digest of the parts, in canonical big-endian order.
fn mix(parts: &[&[u8]]) -> [u8; 16] {
    xxh3_128(&parts.concat()).to_be_bytes()
}

/// The offset of a sample: one of `slots` offsets, each a sample apart, chosen by the state's first eight bytes.
fn sample_offset(state: &[u8; 16], slots: u64) -> u64 {
    let draw = u64::from_le_bytes(state[..8].try_into().expect("8 bytes"));
    CHUNK + (draw % slots) * SAMPLE
}

/// Where the settings for the file with content hash `hash` are kept.
pub fn settings_file(hash: &str) -> PathBuf {
    let first = hash.chars().next().unwrap_or('0').to_string();
    config_path().with_file_name("file_settings").join(first).join(format!("{hash}.yaml"))
}

/// One file's settings, read from its settings file and written back on each change.
pub struct FileSettings {
    file: PathBuf,
    values: Mapping,
    /// The settings as the file last had them, from this read or write. The keys that differ from it are the ones
    /// this instance changed.
    base: Mapping,
}

impl FileSettings {
    /// The settings of the file at `path`. A file with none yet starts empty. `Err` if the file cannot be read.
    pub fn for_path(path: &Path) -> std::io::Result<Self> {
        Ok(Self::at(settings_file(&content_hash(path)?)))
    }

    /// The settings kept in `file`. A missing or unreadable file gives none.
    fn at(file: PathBuf) -> Self {
        let values = read_mapping(&file);
        Self { base: values.clone(), file, values }
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

    /// Sets each key to its value, or unsets it for `None`, then saves the settings once.
    pub fn apply<'a>(&mut self, changes: impl IntoIterator<Item = (&'a str, Option<Value>)>) -> std::io::Result<()> {
        let before = self.values.clone();
        for (key, value) in changes {
            match value {
                Some(value) => {
                    self.values.insert(Value::String(key.to_owned()), value);
                }
                None => {
                    self.values.remove(key);
                }
            }
        }
        if self.values == before {
            return Ok(());
        }
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
    /// the file is removed instead. Another window may have saved other keys of the file since this one read it, so
    /// only the keys this instance changed are written onto what the file holds now.
    fn save(&mut self) -> std::io::Result<()> {
        let mut merged = read_mapping(&self.file);
        let keys: Vec<Value> = self.values.keys().chain(self.base.keys()).cloned().collect();
        for key in keys {
            if self.values.get(&key) == self.base.get(&key) {
                continue;
            }
            match self.values.get(&key) {
                Some(value) => {
                    merged.insert(key, value.clone());
                }
                None => {
                    merged.remove(&key);
                }
            }
        }
        self.values = merged.clone();
        self.base = merged.clone();
        if merged.is_empty() {
            return match std::fs::remove_file(&self.file) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            };
        }
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_yaml_ng::to_string(&Value::Mapping(merged)).map_err(std::io::Error::other)?;
        crate::storage::atomic_file::write(&self.file, text.as_bytes())
    }
}

/// The mapping in the settings `file`, or none if it is missing or does not parse.
fn read_mapping(file: &Path) -> Mapping {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_yaml_ng::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_mapping().cloned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_keeps_the_keys_another_window_saved() {
        let file = std::env::temp_dir().join(format!("rsimgv-merge-{}", std::process::id())).join("ab.yaml");
        let mut first = FileSettings::at(file.clone());
        let mut second = FileSettings::at(file.clone());
        first.set("position", 12.5).unwrap();
        second.set("speed", 1.5).unwrap();
        let saved = FileSettings::at(file.clone());
        assert_eq!(saved.get_f64("position"), Some(12.5), "the other window's key is kept");
        assert_eq!(saved.get_f64("speed"), Some(1.5));
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// Writes `data` to a temporary file, hashes it with `hash`, and removes the file.
    fn hashed<T>(name: &str, data: &[u8], hash: impl Fn(&Path) -> std::io::Result<T>) -> T {
        let path = std::env::temp_dir().join(format!("rsimgv-content-hash-{}-{name}", std::process::id()));
        std::fs::write(&path, data).unwrap();
        let hash = hash(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        hash
    }

    fn movie_hex(path: &Path) -> std::io::Result<String> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        Ok(format!("{:016x}", movie_hash_of(&mut file, size)?))
    }

    #[test]
    fn movie_hashes_match_the_reference_implementation() {
        // The expected hashes are from the Python reference.
        assert_eq!(hashed("empty", b"", movie_hex), "0000000000000000");
        assert_eq!(hashed("short", b"hello world", movie_hex), "deee40ded9a1a3bf");
        let big: Vec<u8> = (0..200_000u32).map(|i| ((i * 7 + i / 3) % 256) as u8).collect();
        assert_eq!(hashed("big", &big, movie_hex), "b5345e8b0a374deb");
    }

    #[test]
    fn content_hashes_match_the_reference_implementation() {
        // The expected hashes are from the Python reference. The 1 MiB file has enough room for every sample, the
        // 500,000- and 200,000-byte files have room for fewer than that, and the tiny one gets none.
        let sampled: Vec<u8> = (0..1_048_576u32).map(|i| ((i * 7 + i / 3) % 256) as u8).collect();
        assert_eq!(hashed("content-sampled", &sampled, content_hash), "298066986b8c77949906f41c1ff35562");
        let short: Vec<u8> = (0..500_000u32).map(|i| ((i * 7 + i / 3) % 256) as u8).collect();
        assert_eq!(hashed("content-short", &short, content_hash), "451d1ba129fd6be04a84d6afe71ab7ce");
        let tiny: Vec<u8> = (0..200_000u32).map(|i| ((i * 7 + i / 3) % 256) as u8).collect();
        assert_eq!(hashed("content-tiny", &tiny, content_hash), "7e15106d564eebf110d7445a280dfa26");
        assert_eq!(hashed("content-none", b"hello world", content_hash), "03649688e5ae62c86767443494f2c847");
    }

    #[test]
    fn settings_are_kept_under_the_hash_characters() {
        let file = settings_file("b5345e8b0a374deb2f1c9d4e6a8b0c3d");
        assert!(file.ends_with("file_settings/b/b5345e8b0a374deb2f1c9d4e6a8b0c3d.yaml"));
    }

    #[test]
    fn settings_round_trip_and_an_empty_file_is_removed() {
        let file = std::env::temp_dir().join(format!("rsimgv-file-settings-{}", std::process::id())).join("a/ab.yaml");
        let mut settings = FileSettings::at(file.clone());
        settings.set("position", 93.5).unwrap();
        settings.set("note", "kept").unwrap();
        let loaded = FileSettings::at(file.clone());
        assert_eq!(loaded.get_f64("position"), Some(93.5));
        assert_eq!(loaded.get_str("note"), Some("kept"));
        settings.remove("position").unwrap();
        settings.remove("note").unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(file.parent().unwrap().parent().unwrap());
    }

}
