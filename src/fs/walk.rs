//! Next and previous folder, in the depth-first order of the folder tree: a folder's first subfolder comes after
//! it, and its last subfolder's deepest last descendant comes before it. Pure apart from `child_directories`, which
//! reads the disk; the traversal takes the child lookup as a parameter so it can be tested on a made-up tree.

use std::path::{Path, PathBuf};

use super::natural_cmp;

/// The subfolders of `folder`, in the order the tree shows them. Hidden folders are left out unless `show_hidden`.
pub fn child_directories(folder: &Path, show_hidden: bool) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut children: Vec<(String, PathBuf)> = read
        .flatten()
        // A symbolic link is not followed, so a link back up the tree cannot make the walk endless.
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
        .filter(|(name, _)| show_hidden || !name.starts_with('.'))
        .collect();
    children.sort_by(|(a, _), (b, _)| natural_cmp(a, b));
    children.into_iter().map(|(_, path)| path).collect()
}

/// The folder after `current` in depth-first order: its first subfolder if it has one, otherwise the next sibling of
/// it or of the nearest ancestor that has one. `None` when the traversal is at its end.
pub fn next_folder(current: &Path, children: &dyn Fn(&Path) -> Vec<PathBuf>) -> Option<PathBuf> {
    if let Some(first) = children(current).into_iter().next() {
        return Some(first);
    }
    let mut current = current.to_path_buf();
    while let Some(parent) = current.parent().map(Path::to_path_buf) {
        let siblings = children(&parent);
        let index = siblings.iter().position(|sibling| *sibling == current)?;
        if let Some(next) = siblings.get(index + 1) {
            return Some(next.clone());
        }
        current = parent;
    }
    None
}

/// The folder before `current` in depth-first order: the deepest last descendant of its previous sibling, or the
/// parent when it is the first sibling. `None` at the root.
pub fn previous_folder(current: &Path, children: &dyn Fn(&Path) -> Vec<PathBuf>) -> Option<PathBuf> {
    let parent = current.parent()?.to_path_buf();
    let siblings = children(&parent);
    let index = siblings.iter().position(|sibling| sibling == current)?;
    if index == 0 {
        return Some(parent);
    }
    let mut folder = siblings[index - 1].clone();
    while let Some(last) = children(&folder).pop() {
        folder = last;
    }
    Some(folder)
}

/// Whether a file name is a picture or a video, judged by its content type as the browse view judges it.
pub fn is_media_name(name: &str) -> bool {
    let (kind, _) = gtk::gio::content_type_guess(Some(name), None::<&[u8]>);
    kind.starts_with("image/") || kind.starts_with("video/")
}

/// Whether `folder` holds at least one picture or video. Folders in it do not count.
pub fn has_media(folder: &Path) -> bool {
    let Ok(read) = std::fs::read_dir(folder) else { return false };
    read.flatten().any(|entry| entry.path().is_file() && is_media_name(&entry.file_name().to_string_lossy()))
}

/// The pictures and videos in `folder` and the folders under it, depth first, each folder's in name order. Hidden files
/// and folders are left out unless `show_hidden`. This reads the disk, so it runs on a worker thread.
pub fn media_files(folder: &Path, show_hidden: bool) -> Vec<PathBuf> {
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
        .filter(|(name, path)| (show_hidden || !name.starts_with('.')) && is_media_name(name) && path.is_file())
        .collect();
    files.sort_by(|(a, _), (b, _)| natural_cmp(a, b));
    let mut found: Vec<PathBuf> = files.into_iter().map(|(_, path)| path).collect();
    for child in child_directories(folder, show_hidden) {
        found.extend(media_files(&child, show_hidden));
    }
    found
}

/// The next folder in depth-first order that holds a picture or video. Folders without media are passed over.
pub fn next_media_folder(
    current: &Path,
    children: &dyn Fn(&Path) -> Vec<PathBuf>,
    has_media: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let mut folder = current.to_path_buf();
    loop {
        folder = next_folder(&folder, children)?;
        if has_media(&folder) {
            return Some(folder);
        }
    }
}

/// The previous folder in depth-first order that holds a picture or video. Folders without media are passed over.
pub fn previous_media_folder(
    current: &Path,
    children: &dyn Fn(&Path) -> Vec<PathBuf>,
    has_media: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let mut folder = current.to_path_buf();
    loop {
        folder = previous_folder(&folder, children)?;
        if has_media(&folder) {
            return Some(folder);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// The example tree from the specification.
    fn example_tree() -> HashMap<PathBuf, Vec<PathBuf>> {
        let home = PathBuf::from("/home");
        let user1 = home.join("user1");
        let user2 = home.join("user2");
        let user3 = home.join("user3");
        let mut tree = HashMap::new();
        tree.insert(PathBuf::from("/"), vec![home.clone()]);
        tree.insert(home, vec![user1.clone(), user2.clone(), user3]);
        tree.insert(
            user1,
            ["Desktop", "Documents", "Downloads", "Music", "Pictures", "Videos"]
                .iter()
                .map(|name| PathBuf::from("/home/user1").join(name))
                .collect(),
        );
        tree
    }

    fn lookup(tree: &HashMap<PathBuf, Vec<PathBuf>>) -> impl Fn(&Path) -> Vec<PathBuf> + '_ {
        move |folder: &Path| tree.get(folder).cloned().unwrap_or_default()
    }

    #[test]
    fn next_walks_the_tree_in_depth_first_order() {
        let tree = example_tree();
        let children = lookup(&tree);
        let mut folder = PathBuf::from("/home");
        let mut order = Vec::new();
        while let Some(next) = next_folder(&folder, &children) {
            order.push(next.clone());
            folder = next;
        }
        let expected: Vec<PathBuf> = [
            "/home/user1",
            "/home/user1/Desktop",
            "/home/user1/Documents",
            "/home/user1/Downloads",
            "/home/user1/Music",
            "/home/user1/Pictures",
            "/home/user1/Videos",
            "/home/user2",
            "/home/user3",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn previous_reverses_the_next_order() {
        let tree = example_tree();
        let children = lookup(&tree);
        assert_eq!(previous_folder(Path::new("/home/user2"), &children), Some(PathBuf::from("/home/user1/Videos")));
        assert_eq!(previous_folder(Path::new("/home/user1/Desktop"), &children), Some(PathBuf::from("/home/user1")));
        assert_eq!(previous_folder(Path::new("/home/user1"), &children), Some(PathBuf::from("/home")));
        assert_eq!(previous_folder(Path::new("/"), &children), None);
        assert_eq!(next_folder(Path::new("/home/user3"), &children), None);
    }
}
