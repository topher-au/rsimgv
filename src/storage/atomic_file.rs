//! Writes files so that a crash, or a second writer at the same moment, never leaves one half-written.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Writes `data` to `file` through a temporary file beside it, which is then renamed over `file`. The temporary name
/// is unique to this write, so two writes at once do not share one. The data is synced to disk before the rename, and
/// the directory is synced after it, so a crash keeps either the old file or the new one.
pub fn write(file: &Path, data: &[u8]) -> std::io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let name = file.file_name().ok_or(std::io::ErrorKind::InvalidInput)?;
    let mut temporary_name = name.to_os_string();
    temporary_name.push(format!(".{}.{}.tmp", std::process::id(), WRITES.fetch_add(1, Ordering::Relaxed)));
    let temporary = file.with_file_name(temporary_name);

    let written = std::fs::File::create(&temporary).and_then(|mut handle| {
        handle.write_all(data)?;
        handle.sync_all()
    });
    if let Err(err) = written.and_then(|()| std::fs::rename(&temporary, file)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(err);
    }

    // The rename is already done, so a failure to sync the directory only means the rename may not survive a crash;
    // the write still succeeded.
    if let Some(parent) = file.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write;

    #[test]
    fn replaces_the_file_and_leaves_no_temporary_behind() {
        let dir = std::env::temp_dir().join(format!("rsimgv-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.yaml");
        write(&file, b"first").unwrap();
        write(&file, b"second").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"second");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("settings.yaml")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
