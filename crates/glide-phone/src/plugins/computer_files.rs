//! Glide only: the phone browses this computer's home folder and downloads files from it, the
//! counterpart of this computer browsing the phone over SFTP. Off until the user turns it on.
//!
//! `glide.files.request {path}` lists a folder, `{path, fetch: true}` sends that file the way a
//! shared file goes (`kdeconnect.share.request`). Paths are relative to the home folder; `..`,
//! hidden entries and links that lead outside it are refused.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value as Json, json};

use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_FILES: &str = "glide.files";
pub const PACKET_TYPE_FILES_REQUEST: &str = "glide.files.request";
const MAX_ENTRIES: usize = 2000;

/// The folder the phone may see.
#[must_use]
pub fn root() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_dir())
}

/// `relative` under `root`, as a real path that stays inside it; None otherwise.
#[must_use]
pub fn resolve(root: &Path, relative: &str) -> Option<PathBuf> {
    let parts = crate::plugins::sftp::safe_relative(relative)?;
    if parts.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.')) {
        return None; // hidden files and folders (keys, configs) stay private
    }
    let base = root.canonicalize().ok()?;
    let real = base.join(parts).canonicalize().ok()?;
    real.starts_with(&base).then_some(real)
}

fn relative_name(relative: &str) -> String {
    crate::plugins::sftp::safe_relative(relative).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The listing of a folder, or an error the phone shows.
#[must_use]
pub fn listing(root: &Path, relative: &str) -> NetworkPacket {
    let path = relative_name(relative);
    let Some(dir) = resolve(root, relative).filter(|p| p.is_dir()) else {
        return error_packet(&path, "missing");
    };
    let Ok(read) = std::fs::read_dir(&dir) else { return error_packet(&path, "unreadable") };
    let mut entries: Vec<(bool, String, Json)> = read
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            // Follow links only when they stay inside the home folder.
            let target = resolve(root, &format!("{path}/{name}"))?;
            let meta = std::fs::metadata(&target).ok()?;
            let modified =
                meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis());
            let dir = meta.is_dir();
            Some((
                dir,
                name.to_lowercase(),
                json!({ "name": name, "dir": dir, "size": meta.len(), "modified": modified as u64 }),
            ))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let truncated = entries.len() > MAX_ENTRIES;
    let list: Vec<Json> = entries.into_iter().take(MAX_ENTRIES).map(|(_, _, e)| e).collect();
    NetworkPacket::new(PACKET_TYPE_FILES, json!({ "path": path, "entries": list, "truncated": truncated }))
}

/// `off`: the user hasn't allowed it; `missing`, `unreadable`: what the path is.
#[must_use]
pub fn error_packet(path: &str, error: &str) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_FILES, json!({ "path": relative_name(path), "error": error }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phone_sees_the_home_folder_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("glide-files-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Документи/вкладене")).unwrap();
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        std::fs::write(root.join("Документи/звіт.txt"), b"abc").unwrap();
        std::fs::write(root.join(".ssh/id_ed25519"), b"secret").unwrap();
        let outside = std::env::temp_dir().join(format!("glide-files-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let _ = std::os::unix::fs::symlink(&outside, root.join("escape"));

        let top = listing(&root, "");
        let names: Vec<&str> =
            top.body["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Документи"], "no hidden folders, no link out of home");
        let docs = listing(&root, "/Документи");
        assert_eq!(docs.body["path"], "Документи");
        assert_eq!(docs.body["entries"][0]["name"], "вкладене", "folders first");
        assert_eq!(docs.body["entries"][1]["size"], 3);

        assert!(resolve(&root, "Документи/звіт.txt").is_some());
        assert!(resolve(&root, "../etc/passwd").is_none());
        assert!(resolve(&root, ".ssh/id_ed25519").is_none());
        assert!(resolve(&root, "escape").is_none());
        assert_eq!(listing(&root, "../..").body["error"], "missing");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
