//! Glide only: new screenshots taken on this computer go to the phones (off until the user
//! turns it on), the counterpart of the phone's screenshot sync. Glide saves them into the
//! Pictures folder as `glide-screenshot-*.png`; other tools use `Pictures/Screenshots`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Pictures folder: `XDG_PICTURES_DIR`, `user-dirs.dirs`, else `~/Pictures` (as Glide does).
#[must_use]
pub fn pictures_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if let Some(dir) = std::env::var_os("XDG_PICTURES_DIR") {
        return Some(PathBuf::from(dir));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    let from_user_dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok().and_then(|text| {
        let value =
            text.lines().find_map(|l| l.trim().strip_prefix("XDG_PICTURES_DIR="))?.trim().trim_matches('"').to_string();
        if let Some(rest) = value.strip_prefix("$HOME") {
            Some(home.join(rest.trim_start_matches('/')))
        } else {
            value.starts_with('/').then(|| PathBuf::from(value))
        }
    });
    Some(from_user_dirs.unwrap_or_else(|| home.join("Pictures")))
}

/// Whether `path` (found in `dir`) is a screenshot.
#[must_use]
pub fn is_screenshot(path: &Path, in_screenshots_folder: bool) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let image = [".png", ".jpg", ".jpeg", ".webp"].iter().any(|ext| name.ends_with(ext));
    image && (in_screenshots_folder || name.starts_with("glide-screenshot-") || name.starts_with("screenshot"))
}

/// Files that look like screenshots in the folders, with their sizes.
fn scan(folders: &[(PathBuf, bool)]) -> HashMap<PathBuf, u64> {
    let mut found = HashMap::new();
    for (dir, screenshots_folder) in folders {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_screenshot(&path, *screenshots_folder)
                && let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                found.insert(path, meta.len());
            }
        }
    }
    found
}

/// Calls `new_screenshot` for every screenshot that appears in `pictures` (or its Screenshots
/// folder) from now on, once it has stopped growing.
pub async fn watch(pictures: PathBuf, interval: Duration, new_screenshot: impl Fn(PathBuf)) {
    let folders = [(pictures.clone(), false), (pictures.join("Screenshots"), true)];
    let mut known = scan(&folders);
    let mut growing: HashMap<PathBuf, u64> = HashMap::new();
    loop {
        tokio::time::sleep(interval).await;
        let now = scan(&folders);
        for (path, size) in &now {
            if known.contains_key(path) {
                continue;
            }
            // Written in one go only when the size held still since the last look.
            if growing.get(path) == Some(size) && *size > 0 {
                growing.remove(path);
                known.insert(path.clone(), *size);
                new_screenshot(path.clone());
            } else {
                growing.insert(path.clone(), *size);
            }
        }
        known.retain(|path, _| now.contains_key(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshots_are_told_apart_from_other_pictures() {
        assert!(is_screenshot(Path::new("/p/glide-screenshot-1700.png"), false));
        assert!(is_screenshot(Path::new("/p/Screenshot From 2026-11-18.png"), false));
        assert!(!is_screenshot(Path::new("/p/wallpaper.png"), false));
        assert!(is_screenshot(Path::new("/p/Screenshots/anything.jpg"), true));
        assert!(!is_screenshot(Path::new("/p/Screenshots/notes.txt"), true));
    }

    #[tokio::test]
    async fn only_new_finished_screenshots_are_reported() {
        let dir = std::env::temp_dir().join(format!("glide-shots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("glide-screenshot-1.png"), b"old").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let watcher = tokio::spawn(watch(dir.clone(), Duration::from_millis(50), move |p| {
            let _ = tx.send(p);
        }));
        tokio::time::sleep(Duration::from_millis(80)).await;
        std::fs::write(dir.join("wallpaper.png"), b"not a screenshot").unwrap();
        std::fs::write(dir.join("glide-screenshot-2.png"), b"new").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.expect("reported").unwrap();
        assert_eq!(got.file_name().unwrap(), "glide-screenshot-2.png");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(rx.try_recv().is_err(), "the old one and the wallpaper are not reported");
        watcher.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
