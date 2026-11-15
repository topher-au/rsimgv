//! Upscaled copies of pictures, kept in the app's data directory instead of beside the originals. A copy is named by
//! a hash of the original's content, so it is still found after the original is moved or renamed, and an edited
//! original, whose content differs, does not show a stale copy. Pure: no GTK.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use xxhash_rust::xxh3::Xxh3;

/// A file's hash, with the size and modification time it was made at, so it is made again only when they change.
type Remembered = (u64, SystemTime, String);

/// The folder the copies are in: `upscaled` in the user's data directory, or in `RSIMGV_DATA` when it is set.
pub fn folder() -> PathBuf {
    let root = std::env::var_os("RSIMGV_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| glib::user_data_dir().join("rsimgv"));
    root.join("upscaled")
}

/// Where the upscaled copy of `original` is, or would be: a PNG, named by the hash of the original's content. The
/// original must be readable.
pub fn path_for(original: &Path) -> std::io::Result<PathBuf> {
    path_in(&folder(), original)
}

/// The upscaled copy of `original`, if one has been made.
pub fn find(original: &Path) -> Option<PathBuf> {
    path_for(original).ok().filter(|copy| copy.is_file())
}

fn path_in(folder: &Path, original: &Path) -> std::io::Result<PathBuf> {
    Ok(folder.join(format!("{}.png", content_key(original)?)))
}

/// The hash of the content of the file at `path`, as 32 lowercase hex digits.
fn content_key(path: &Path) -> std::io::Result<String> {
    static KEYS: OnceLock<Mutex<HashMap<PathBuf, Remembered>>> = OnceLock::new();
    let keys = KEYS.get_or_init(Default::default);

    let metadata = std::fs::metadata(path)?;
    let (size, modified) = (metadata.len(), metadata.modified()?);
    if let Some((known_size, known_modified, key)) = keys.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(path) {
        if *known_size == size && *known_modified == modified {
            return Ok(key.clone());
        }
    }

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Xxh3::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let key = format!("{:032x}", hasher.digest128());
    keys.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(path.to_path_buf(), (size, modified, key.clone()));
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rsimgv-upscaled-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch folder");
        dir
    }

    #[test]
    fn a_copy_follows_the_content_not_the_name() {
        let dir = scratch("content");
        let (first, second, other) = (dir.join("first.jpg"), dir.join("second.jpg"), dir.join("other.jpg"));
        std::fs::write(&first, b"same pixels").unwrap();
        std::fs::write(&second, b"same pixels").unwrap();
        std::fs::write(&other, b"other pixels").unwrap();
        let store = dir.join("store");

        let copy = path_in(&store, &first).unwrap();
        assert_eq!(copy, path_in(&store, &second).unwrap(), "a renamed original finds the same copy");
        assert_ne!(copy, path_in(&store, &other).unwrap(), "different content has its own copy");
        assert_eq!(copy.extension().and_then(|extension| extension.to_str()), Some("png"));
    }

    #[test]
    fn an_edited_original_does_not_find_the_old_copy() {
        let dir = scratch("edited");
        let original = dir.join("photo.jpg");
        std::fs::write(&original, b"before").unwrap();
        let before = path_in(&dir, &original).unwrap();
        // A different length, so the remembered hash is not reused even where the modification time is coarse.
        std::fs::write(&original, b"after the edit").unwrap();
        assert_ne!(before, path_in(&dir, &original).unwrap());
    }

    #[test]
    fn a_missing_original_has_no_copy() {
        assert!(path_in(&scratch("missing"), Path::new("/nonexistent/picture.jpg")).is_err());
    }
}
