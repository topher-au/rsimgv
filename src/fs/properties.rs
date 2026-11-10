//! What the properties dialog says about files, apart from how it is drawn: what is known of each file, the words for
//! a selection of them, the count of what a folder holds, and the permissions, which follow Nautilus's properties
//! window (`nautilus-properties.c`): a folder is described by what can be done in it, a file by whether it can be read
//! and written, and a row that mixes files that differ shows no choice until one is made.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::{gio, glib};

use gio::prelude::*;

/// The attributes the dialog reads of a file.
const ATTRIBUTES: &str = "standard::display-name,standard::type,standard::size,standard::content-type,\
standard::is-symlink,standard::symlink-target,time::modified,time::access,time::created,unix::mode,unix::uid,\
unix::gid,owner::user,owner::group";

/// How many names a selection of several files lists, before it is only counted.
pub const MAX_LISTED_NAMES: usize = 5;

/// What is known of one file at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileFacts {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub link_target: Option<String>,
    pub content_type: Option<String>,
    pub size: u64,
    /// Seconds since the epoch.
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
    /// The permission bits of the file's mode.
    pub mode: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    /// Whether the user may change the file's permissions: they own it, or are root.
    pub can_set_permissions: bool,
}

impl FileFacts {
    /// Reads the facts of the file at `path`. This reads the disk, so it runs on a worker thread.
    pub fn query(path: &Path) -> Result<Self, String> {
        let info = gio::File::for_path(path)
            .query_info(ATTRIBUTES, gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, gio::Cancellable::NONE)
            .map_err(|err| err.to_string())?;
        let seconds = |attribute: &str| info.has_attribute(attribute).then(|| info.attribute_uint64(attribute) as i64);
        let owner = info.attribute_string("owner::user").map(|owner| owner.to_string());
        let is_symlink = info.is_symlink();
        // A link is described by what it points at, as Nautilus does, except for the link itself.
        let target = if is_symlink { std::fs::metadata(path).ok() } else { None };
        let is_dir = match &target {
            Some(target) => target.is_dir(),
            None => info.file_type() == gio::FileType::Directory,
        };
        let me = glib::user_name().to_string_lossy().into_owned();
        Ok(Self {
            path: path.to_path_buf(),
            name: info.display_name().to_string(),
            is_dir,
            is_symlink,
            link_target: is_symlink.then(|| info.symlink_target()).flatten().map(|target| target.to_string_lossy().into_owned()),
            content_type: info.content_type().map(|kind| kind.to_string()),
            size: info.size().max(0) as u64,
            modified: seconds("time::modified"),
            accessed: seconds("time::access"),
            created: seconds("time::created"),
            mode: info.has_attribute("unix::mode").then(|| info.attribute_uint32("unix::mode") & 0o7777),
            // A link has the permissions of nothing it can change, and the target is another file.
            can_set_permissions: !is_symlink && (owner.as_deref() == Some(me.as_str()) || me == "root"),
            owner,
            group: info.attribute_string("owner::group").map(|group| group.to_string()),
        })
    }
}

/// The name a selection goes by: a lone file's, the list of up to `MAX_LISTED_NAMES`, or how many there are.
pub fn name_summary(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        many if many.len() > MAX_LISTED_NAMES => format!("{} Selected Items", many.len()),
        many => many.join(", "),
    }
}

/// The content type every file has, if they share one.
pub fn shared_content_type(files: &[FileFacts]) -> Option<&str> {
    let first = files.first()?.content_type.as_deref()?;
    files.iter().all(|file| file.content_type.as_deref() == Some(first)).then_some(first)
}

/// A content type in words, with the type itself after it: `JPEG image (image/jpeg)`.
pub fn type_text(content_type: &str) -> String {
    let description = gio::content_type_get_description(content_type);
    if description.as_str() == content_type {
        description.to_string()
    } else {
        format!("{description} ({content_type})")
    }
}

/// `n` with a comma between each three digits.
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A size as people read it, with the exact count of bytes after it once that says more: `1.5 MB (1,500,000 bytes)`.
pub fn size_text(size: u64) -> String {
    let readable = glib::format_size(size);
    if size < 1000 {
        readable.to_string()
    } else {
        format!("{readable} ({} bytes)", group_digits(size))
    }
}

/// A time as the dialog shows it, in the user's time zone.
pub fn time_text(seconds: i64) -> String {
    glib::DateTime::from_unix_local(seconds)
        .ok()
        .and_then(|time| time.format("%a %-d %b %Y, %H:%M:%S").ok())
        .map_or_else(String::new, |text| text.to_string())
}

/// What a folder or a selection holds, as counted so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Files and folders, not counting the folders selected themselves.
    pub items: u64,
    /// The bytes in the files.
    pub size: u64,
    /// Whether a folder could not be read.
    pub unreadable: bool,
}

/// The paths in `paths` that are not inside another of them, since the contents of a folder in the selection are
/// counted with it.
fn outermost(paths: &[PathBuf]) -> Vec<&PathBuf> {
    paths
        .iter()
        .filter(|path| !paths.iter().any(|other| other != *path && path.starts_with(other) && other.is_dir()))
        .collect()
}

/// Counts what `paths` hold: a file counts for itself, a folder for everything under it. Links are counted and not
/// followed, so a link back up the tree cannot make the count endless. `progress` is told the count as it grows, and
/// the count stops early if `cancel` is set. This reads the disk, so it runs on a worker thread.
pub fn count_contents(paths: &[PathBuf], cancel: &AtomicBool, progress: &mut dyn FnMut(&Counts)) -> Counts {
    const REPORT_EVERY: u64 = 200;
    let mut counts = Counts::default();
    let mut since_report = 0;
    let mut stack: Vec<PathBuf> = Vec::new();
    for path in outermost(paths) {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            counts.unreadable = true;
            continue;
        };
        if !meta.is_dir() {
            counts.items += 1;
            counts.size += meta.len();
            continue;
        }
        stack.push(path.clone());
        while let Some(folder) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                return counts;
            }
            let Ok(entries) = std::fs::read_dir(&folder) else {
                counts.unreadable = true;
                continue;
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    counts.unreadable = true;
                    continue;
                };
                counts.items += 1;
                since_report += 1;
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                    Ok(_) => counts.size += entry.metadata().map_or(0, |meta| meta.len()),
                    Err(_) => counts.unreadable = true,
                }
                if since_report >= REPORT_EVERY {
                    since_report = 0;
                    progress(&counts);
                    if cancel.load(Ordering::Relaxed) {
                        return counts;
                    }
                }
            }
        }
    }
    counts
}

/// The contents line of the dialog for `counts`. While the count is `running`, nothing is said of an empty one yet.
pub fn contents_text(counts: &Counts, running: bool) -> String {
    if counts.items == 0 {
        return match (running, counts.unreadable) {
            (true, _) => "…".to_owned(),
            (false, true) => "Contents unreadable".to_owned(),
            (false, false) => "Empty folder".to_owned(),
        };
    }
    let size = glib::format_size(counts.size);
    let mut text = if counts.items == 1 {
        format!("1 item, with size {size}")
    } else {
        format!("{} items, totalling {size}", group_digits(counts.items))
    };
    if counts.unreadable {
        text.push_str("\n(some contents unreadable)");
    }
    text
}

/// The tooltip of the contents line: the exact bytes.
pub fn bytes_text(size: u64) -> String {
    if size == 1 {
        "1 byte".to_owned()
    } else {
        format!("{} bytes", group_digits(size))
    }
}

/// Who a permission is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Owner,
    Group,
    Others,
}

impl Who {
    pub const ALL: [Who; 3] = [Who::Owner, Who::Group, Who::Others];

    /// The bits of a mode for this class: read, write and execute.
    fn bits(self) -> (u32, u32, u32) {
        match self {
            Who::Owner => (0o400, 0o200, 0o100),
            Who::Group => (0o040, 0o020, 0o010),
            Who::Others => (0o004, 0o002, 0o001),
        }
    }
}

/// What can be done: read, write, and for a folder, get into it (execute).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

impl Access {
    pub const NONE: Access = Access { read: false, write: false, exec: false };
    pub const READ: Access = Access { read: true, write: false, exec: false };
    pub const READ_WRITE: Access = Access { read: true, write: true, exec: false };
    pub const READ_EXEC: Access = Access { read: true, write: false, exec: true };
    pub const ALL: Access = Access { read: true, write: true, exec: true };

    /// What a mode allows `who`.
    pub fn of(mode: u32, who: Who) -> Self {
        let (read, write, exec) = who.bits();
        Self { read: mode & read != 0, write: mode & write != 0, exec: mode & exec != 0 }
    }

    /// `mode` with the bits for `who` that `mask` names set as `self` has them.
    pub fn applied(self, mode: u32, who: Who, mask: Access) -> u32 {
        let (read, write, exec) = who.bits();
        let mut mode = mode;
        for (bit, in_mask, on) in [(read, mask.read, self.read), (write, mask.write, self.write), (exec, mask.exec, self.exec)] {
            if in_mask {
                mode = if on { mode | bit } else { mode & !bit };
            }
        }
        mode
    }
}

/// The words for an access, in Nautilus's: for a folder what can be done in it, for a file whether it can be read and
/// written. `None` is a mix of several files that differ.
pub fn access_text(access: Option<Access>, folder: bool) -> &'static str {
    let Some(access) = access else { return "---" };
    match (access.read, access.write, folder) {
        (true, true, false) => "Read and Write",
        (true, true, true) if access.exec => "Create and Delete Files",
        (true, true, true) => "Read and Write, No Access",
        (true, false, false) => "Read-Only",
        (true, false, true) if access.exec => "Access Files",
        (true, false, true) => "List Files Only",
        (false, true, true) if !access.exec => "Write-Only, No Access",
        (false, true, _) => "Write-Only",
        (false, false, true) if access.exec => "Access-Only",
        (false, false, _) => "None",
    }
}

/// The accesses a choice lists for `who`, from least to most. The owner cannot give themselves none.
pub fn access_choices(who: Who, folder: bool) -> Vec<Access> {
    let mut choices = Vec::new();
    if who != Who::Owner {
        choices.push(Access::NONE);
    }
    if folder {
        choices.extend([Access::READ, Access::READ_EXEC, Access::ALL]);
    } else {
        choices.extend([Access::READ, Access::READ_WRITE]);
    }
    choices
}

/// The bits a choice for files or for folders changes: read and write, and for folders, execute too.
pub fn access_mask(folder: bool) -> Access {
    Access { read: true, write: true, exec: folder }
}

/// Which of a selection a permission applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Files,
    Folders,
}

impl Kind {
    fn matches(self, file: &FileFacts) -> bool {
        (self == Kind::Folders) == file.is_dir
    }
}

/// Whether the execute bits of files are set for all three classes, some, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Executable {
    No,
    Yes,
    Mixed,
}

/// The permissions of a selection, as the dialog's rows show them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PermissionsSummary {
    /// The access each class has on the folders, for each class `None` if the folders differ. `None` if no folders.
    pub folders: Option<[Option<Access>; 3]>,
    /// The same for the files, without execute.
    pub files: Option<[Option<Access>; 3]>,
    pub executable: Option<Executable>,
    pub can_set_all_folders: bool,
    pub can_set_all_files: bool,
    pub can_set_any_file: bool,
}

/// Merges the permissions of `files`. A file whose mode is unknown is left out.
pub fn summarize_permissions(files: &[FileFacts]) -> PermissionsSummary {
    let mut summary = PermissionsSummary { can_set_all_folders: true, can_set_all_files: true, ..PermissionsSummary::default() };
    for file in files {
        let Some(mode) = file.mode else { continue };
        let mask = access_mask(file.is_dir);
        let accesses = Who::ALL.map(|who| Access::of(mode, who) & mask);
        let slot = if file.is_dir { &mut summary.folders } else { &mut summary.files };
        *slot = Some(match slot.take() {
            None => accesses.map(Some),
            Some(known) => std::array::from_fn(|index| known[index].filter(|known| *known == accesses[index])),
        });
        if file.is_dir {
            summary.can_set_all_folders &= file.can_set_permissions;
        } else {
            summary.can_set_all_files &= file.can_set_permissions;
            summary.can_set_any_file |= file.can_set_permissions;
            let executable = match (mode & 0o111).count_ones() {
                0 => Executable::No,
                3 => Executable::Yes,
                _ => Executable::Mixed,
            };
            summary.executable = Some(match summary.executable {
                Some(known) if known != executable => Executable::Mixed,
                _ => executable,
            });
        }
    }
    summary
}

impl std::ops::BitAnd for Access {
    type Output = Access;

    fn bitand(self, mask: Access) -> Access {
        Access { read: self.read && mask.read, write: self.write && mask.write, exec: self.exec && mask.exec }
    }
}

/// The new mode of each of `files` of `kind` once `who` is given `access`. Files that already have the mode, or whose
/// permissions the user cannot change, are left out.
pub fn modes_for_access(files: &[FileFacts], kind: Kind, who: Who, access: Access) -> Vec<(PathBuf, u32)> {
    let mask = access_mask(kind == Kind::Folders);
    changed_modes(files, kind, |mode| access.applied(mode, who, mask))
}

/// The new mode of each of `files` once their execute bits are all set or all cleared.
pub fn modes_for_executable(files: &[FileFacts], on: bool) -> Vec<(PathBuf, u32)> {
    changed_modes(files, Kind::Files, |mode| if on { mode | 0o111 } else { mode & !0o111 })
}

fn changed_modes(files: &[FileFacts], kind: Kind, change: impl Fn(u32) -> u32) -> Vec<(PathBuf, u32)> {
    files
        .iter()
        .filter(|file| kind.matches(file) && file.can_set_permissions)
        .filter_map(|file| {
            let mode = file.mode?;
            let new = change(mode);
            (new != mode).then(|| (file.path.clone(), new))
        })
        .collect()
}

/// Sets the permission bits of the file at `path` to `mode`. This writes the disk, so it runs on a worker thread.
pub fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    gio::File::for_path(path)
        .set_attribute_uint32("unix::mode", mode, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        .map_err(|err| err.to_string())
}

/// The bytes free on the filesystem the file at `path` is on, if that is known. This reads the disk, so it runs on a
/// worker thread.
pub fn free_space(path: &Path) -> Option<u64> {
    let info = gio::File::for_path(path).query_filesystem_info("filesystem::free", gio::Cancellable::NONE).ok()?;
    info.has_attribute("filesystem::free").then(|| info.attribute_uint64("filesystem::free"))
}

/// The class of the user among a file's: the owner if they own it, else the group if it is the group they run in,
/// else the others. The process's own ids are those of its `/proc` entry.
pub fn class_of(file: &FileFacts) -> Who {
    let me = glib::user_name().to_string_lossy().into_owned();
    if file.owner.as_deref() == Some(me.as_str()) {
        return Who::Owner;
    }
    let group = std::fs::metadata("/proc/self").ok().map(|meta| meta.gid());
    let file_group = file.path.symlink_metadata().ok().map(|meta| meta.gid());
    if group.is_some() && group == file_group {
        Who::Group
    } else {
        Who::Others
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, is_dir: bool, mode: u32) -> FileFacts {
        FileFacts {
            path: PathBuf::from("/d").join(name),
            name: name.to_owned(),
            is_dir,
            mode: Some(mode),
            can_set_permissions: true,
            ..FileFacts::default()
        }
    }

    #[test]
    fn a_selection_is_named_by_its_files_until_there_are_too_many() {
        let names = |n: usize| (1..=n).map(|i| format!("f{i}")).collect::<Vec<_>>();
        assert_eq!(name_summary(&names(1)), "f1");
        assert_eq!(name_summary(&names(3)), "f1, f2, f3");
        assert_eq!(name_summary(&names(5)), "f1, f2, f3, f4, f5");
        assert_eq!(name_summary(&names(6)), "6 Selected Items");
    }

    #[test]
    fn numbers_are_grouped_by_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[test]
    fn a_size_gives_its_bytes_once_they_say_more() {
        assert!(size_text(10).starts_with("10 bytes"));
        assert!(!size_text(10).contains('('));
        assert!(size_text(1_500_000).ends_with("(1,500,000 bytes)"));
        assert_eq!(bytes_text(1), "1 byte");
        assert_eq!(bytes_text(2048), "2,048 bytes");
    }

    #[test]
    fn a_shared_type_is_found_only_when_every_file_has_it() {
        let typed = |kind: &str| FileFacts { content_type: Some(kind.to_owned()), ..FileFacts::default() };
        assert_eq!(shared_content_type(&[typed("image/png"), typed("image/png")]), Some("image/png"));
        assert_eq!(shared_content_type(&[typed("image/png"), typed("video/mp4")]), None);
        assert_eq!(shared_content_type(&[]), None);
    }

    #[test]
    fn the_contents_line_reads_like_nautilus() {
        let counts = |items, size, unreadable| Counts { items, size, unreadable };
        assert_eq!(contents_text(&counts(0, 0, false), false), "Empty folder");
        assert_eq!(contents_text(&counts(0, 0, false), true), "…");
        assert_eq!(contents_text(&counts(0, 0, true), false), "Contents unreadable");
        assert!(contents_text(&counts(1, 10, false), false).starts_with("1 item, with size"));
        assert!(contents_text(&counts(1500, 10, false), false).starts_with("1,500 items, totalling"));
        assert!(contents_text(&counts(2, 10, true), false).ends_with("(some contents unreadable)"));
    }

    #[test]
    fn counting_goes_through_the_folders_and_skips_what_is_inside_another_selected_folder() {
        let root = std::env::temp_dir().join(format!("rsimgv-props-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/one.txt"), b"12345").unwrap();
        std::fs::write(root.join("a/b/two.txt"), b"123").unwrap();
        std::fs::write(root.join("loose.txt"), b"12").unwrap();
        std::os::unix::fs::symlink(&root, root.join("a/loop")).unwrap();
        let cancel = AtomicBool::new(false);

        // a holds b, one.txt, b/two.txt and loop, which is a link: counted, not followed.
        let folder = count_contents(&[root.join("a")], &cancel, &mut |_| {});
        assert_eq!(folder.items, 4);
        assert_eq!(folder.size - root.join("a/loop").symlink_metadata().unwrap().len(), 8);
        assert!(!folder.unreadable);

        // The file in the folder selected with it is not counted again, and a loose file counts for one.
        let both = count_contents(&[root.join("a"), root.join("a/one.txt"), root.join("loose.txt")], &cancel, &mut |_| {});
        assert_eq!(both.items, folder.items + 1);
        assert_eq!(both.size, folder.size + 2);

        cancel.store(true, Ordering::Relaxed);
        assert_eq!(count_contents(&[root.join("a")], &cancel, &mut |_| {}).items, 0, "a cancelled count stops");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_words_for_an_access_follow_nautilus() {
        assert_eq!(access_text(Some(Access::READ_WRITE), false), "Read and Write");
        assert_eq!(access_text(Some(Access::READ), false), "Read-Only");
        assert_eq!(access_text(Some(Access::NONE), false), "None");
        assert_eq!(access_text(Some(Access::ALL), true), "Create and Delete Files");
        assert_eq!(access_text(Some(Access::READ_EXEC), true), "Access Files");
        assert_eq!(access_text(Some(Access::READ), true), "List Files Only");
        assert_eq!(access_text(Some(Access { read: false, write: false, exec: true }), true), "Access-Only");
        assert_eq!(access_text(Some(Access { read: true, write: true, exec: false }), true), "Read and Write, No Access");
        assert_eq!(access_text(None, true), "---");
    }

    #[test]
    fn the_choices_never_leave_the_owner_without_access() {
        assert_eq!(access_choices(Who::Owner, false), [Access::READ, Access::READ_WRITE]);
        assert_eq!(access_choices(Who::Group, false), [Access::NONE, Access::READ, Access::READ_WRITE]);
        assert_eq!(access_choices(Who::Others, true), [Access::NONE, Access::READ, Access::READ_EXEC, Access::ALL]);
    }

    #[test]
    fn an_access_is_read_from_and_written_to_a_mode() {
        assert_eq!(Access::of(0o640, Who::Owner), Access::READ_WRITE);
        assert_eq!(Access::of(0o640, Who::Group), Access::READ);
        assert_eq!(Access::of(0o640, Who::Others), Access::NONE);
        // Writing read and write for the group leaves its execute bit and the other classes alone.
        assert_eq!(Access::READ_WRITE.applied(0o751, Who::Group, access_mask(false)), 0o771);
        assert_eq!(Access::READ_EXEC.applied(0o700, Who::Others, access_mask(true)), 0o705);
        assert_eq!(Access::NONE.applied(0o777, Who::Others, access_mask(true)), 0o770);
    }

    #[test]
    fn a_selection_that_differs_shows_a_mix_and_only_that_row() {
        let summary = summarize_permissions(&[file("a", false, 0o644), file("b", false, 0o664), file("d", true, 0o755)]);
        assert_eq!(summary.files, Some([Some(Access::READ_WRITE), None, Some(Access::READ)]));
        assert_eq!(summary.folders, Some([Some(Access::ALL), Some(Access::READ_EXEC), Some(Access::READ_EXEC)]));
        assert_eq!(summary.executable, Some(Executable::No));
        assert!(summary.can_set_all_files && summary.can_set_all_folders);
        assert_eq!(summarize_permissions(&[file("x", false, 0o755), file("y", false, 0o644)]).executable, Some(Executable::Mixed));
        assert_eq!(summarize_permissions(&[file("x", false, 0o755)]).executable, Some(Executable::Yes));
        assert_eq!(summarize_permissions(&[file("x", false, 0o710)]).executable, Some(Executable::Mixed));
        assert_eq!(summarize_permissions(&[]).files, None);
    }

    #[test]
    fn a_change_touches_only_the_files_it_is_for_and_only_what_changes() {
        let mut locked = file("locked", false, 0o600);
        locked.can_set_permissions = false;
        let files = [file("a", false, 0o600), file("b", false, 0o660), file("d", true, 0o700), locked];

        // Group gets read and write on files: b already has it, locked cannot be changed, folders are not files.
        assert_eq!(modes_for_access(&files, Kind::Files, Who::Group, Access::READ_WRITE), [(PathBuf::from("/d/a"), 0o660)]);
        // Others get to read and enter folders; files are left alone.
        assert_eq!(modes_for_access(&files, Kind::Folders, Who::Others, Access::READ_EXEC), [(PathBuf::from("/d/d"), 0o705)]);
        assert_eq!(
            modes_for_executable(&files, true),
            [(PathBuf::from("/d/a"), 0o711), (PathBuf::from("/d/b"), 0o771)],
            "executable sets all three bits, on files only"
        );
        assert!(modes_for_executable(&[file("x", false, 0o755)], true).is_empty());
        assert_eq!(modes_for_executable(&[file("x", false, 0o755)], false), [(PathBuf::from("/d/x"), 0o644)]);
    }

    #[test]
    fn facts_are_read_from_the_disk_and_a_mode_is_set_through_gio() {
        let dir = std::env::temp_dir().join(format!("rsimgv-props-facts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.txt");
        std::fs::write(&path, b"hello").unwrap();
        std::os::unix::fs::symlink(&path, dir.join("link.txt")).unwrap();

        let facts = FileFacts::query(&path).unwrap();
        assert_eq!((facts.name.as_str(), facts.size, facts.is_dir, facts.is_symlink), ("note.txt", 5, false, false));
        assert_eq!(facts.content_type.as_deref(), Some("text/plain"));
        assert!(facts.modified.is_some() && facts.mode.is_some());
        assert!(facts.can_set_permissions, "the user owns what they just made");
        assert_eq!(class_of(&facts), Who::Owner);

        set_mode(&path, 0o640).unwrap();
        assert_eq!(FileFacts::query(&path).unwrap().mode, Some(0o640));

        let link = FileFacts::query(&dir.join("link.txt")).unwrap();
        assert!(link.is_symlink && !link.is_dir);
        assert_eq!(link.link_target.as_deref(), Some(path.to_string_lossy().as_ref()));
        assert!(FileFacts::query(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
