//! `kdeconnect.contacts.*`: the phone's address book on this computer (KDE Connect's
//! contacts plugin). vCards land in `~/.local/share/kpeoplevcard/glide-phone-<device>/`,
//! where address books that read `KPeople` vCards find them, and names are looked up for
//! SMS and calls.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Value as Json, json};

use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_REQUEST_UIDS: &str = "kdeconnect.contacts.request_all_uids_timestamps";
pub const PACKET_TYPE_REQUEST_VCARDS: &str = "kdeconnect.contacts.request_vcards_by_uid";
pub const PACKET_TYPE_RESPONSE_UIDS: &str = "kdeconnect.contacts.response_uids_timestamps";
pub const PACKET_TYPE_RESPONSE_VCARDS: &str = "kdeconnect.contacts.response_vcards";

#[must_use]
pub fn request_uids_packet() -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_REQUEST_UIDS, json!({}))
}

fn safe(text: &str) -> String {
    text.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).take(64).collect()
}

/// Where one phone's vCards are kept.
#[must_use]
pub fn book_dir(device_id: &str) -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    data.join("kpeoplevcard").join(format!("glide-phone-{}", safe(device_id)))
}

fn index_path(dir: &std::path::Path) -> PathBuf {
    dir.join(".timestamps.json")
}

fn load_index(dir: &std::path::Path) -> HashMap<String, i64> {
    std::fs::read_to_string(index_path(dir)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_index(dir: &std::path::Path, index: &HashMap<String, i64>) {
    if let Ok(text) = serde_json::to_string(index) {
        let _ = glide_config::write_atomic(&index_path(dir), text.as_bytes(), 0o600);
    }
}

/// At most this many contacts per phone; anything beyond is ignored.
pub const MAX_CONTACTS: usize = 20_000;
const MAX_VCARD_BYTES: usize = 256 * 1024;

/// The phone listed its contacts: drop the ones it no longer has, and return the request
/// for the new or changed ones (None when everything is current).
pub fn on_uids(dir: &std::path::Path, body: &Json) -> Option<NetworkPacket> {
    let uids: Vec<String> =
        body.get("uids")?.as_array()?.iter().filter_map(Json::as_str).take(MAX_CONTACTS).map(str::to_string).collect();
    let listed: std::collections::HashSet<&str> = uids.iter().map(String::as_str).collect();
    std::fs::create_dir_all(dir).ok()?;
    let mut index = load_index(dir);
    let stamp = |uid: &str| body.get(uid).and_then(Json::as_i64).unwrap_or(0);
    let wanted: Vec<String> = uids
        .iter()
        .filter(|uid| index.get(uid.as_str()) != Some(&stamp(uid)) || !dir.join(format!("{}.vcf", safe(uid))).is_file())
        .cloned()
        .collect();
    index.retain(|uid, _| {
        let keep = listed.contains(uid.as_str());
        if !keep {
            let _ = std::fs::remove_file(dir.join(format!("{}.vcf", safe(uid))));
        }
        keep
    });
    // Remember every listed timestamp (also the ones about to be fetched, so a later list
    // doesn't fetch them twice); only uids in the index may be stored by on_vcards.
    for uid in &uids {
        index.insert(uid.clone(), stamp(uid));
    }
    save_index(dir, &index);
    (!wanted.is_empty()).then(|| NetworkPacket::new(PACKET_TYPE_REQUEST_VCARDS, json!({ "uids": wanted })))
}

/// The phone sent vCards: store the ones it listed (never unrequested extras).
pub fn on_vcards(dir: &std::path::Path, body: &Json) -> usize {
    let Some(uids) = body.get("uids").and_then(Json::as_array) else { return 0 };
    let index = load_index(dir);
    let mut stored = 0;
    for uid in uids.iter().filter_map(Json::as_str).filter(|uid| index.contains_key(*uid)) {
        if let Some(vcard) = body.get(uid).and_then(Json::as_str)
            && vcard.len() <= MAX_VCARD_BYTES
            && glide_config::write_atomic(&dir.join(format!("{}.vcf", safe(uid))), vcard.as_bytes(), 0o600).is_ok()
        {
            stored += 1;
        }
    }
    if stored > 0 {
        save_index(dir, &index); // marks the book as changed for the names cache
    }
    stored
}

/// The trailing digits that identify a number across formats (+380…, 0…).
#[must_use]
pub fn number_key(number: &str) -> String {
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    digits.chars().rev().take(9).collect::<Vec<_>>().into_iter().rev().collect()
}

/// Name for each phone number in a phone's address book (cached until the book changes).
#[must_use]
pub fn names(dir: &std::path::Path) -> HashMap<String, String> {
    use std::sync::{Mutex, OnceLock};
    type Cache = HashMap<PathBuf, (Option<std::time::SystemTime>, HashMap<String, String>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let changed = std::fs::metadata(index_path(dir)).and_then(|m| m.modified()).ok();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Ok(guard) = cache.lock()
        && let Some((stamp, names)) = guard.get(dir)
        && *stamp == changed
    {
        return names.clone();
    }
    let fresh = read_names(dir);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(dir.to_path_buf(), (changed, fresh.clone()));
    }
    fresh
}

fn read_names(dir: &std::path::Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let unfolded = text.replace("\r\n ", "").replace("\n ", "");
        let mut name = None;
        let mut numbers = Vec::new();
        for line in unfolded.lines() {
            let Some((key, value)) = line.split_once(':') else { continue };
            let key = key.split(';').next().unwrap_or_default().to_ascii_uppercase();
            match key.as_str() {
                "FN" => name = Some(value.trim().to_string()),
                "TEL" => numbers.push(value.trim().to_string()),
                _ => {}
            }
        }
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            for number in numbers {
                let key = number_key(&number);
                if !key.is_empty() {
                    out.insert(key, name.clone());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_and_changed_contacts_are_fetched_and_removed_ones_go() {
        let dir = std::env::temp_dir().join(format!("glide-contacts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = on_uids(&dir, &json!({"uids": ["a", "b"], "a": 1, "b": 2})).expect("fetch both");
        assert_eq!(first.body["uids"], json!(["a", "b"]));
        on_vcards(
            &dir,
            &json!({"uids": ["a", "b"],
            "a": "BEGIN:VCARD\nFN:Мама\nTEL;TYPE=CELL:+380 50 123 4567\nEND:VCARD",
            "b": "BEGIN:VCARD\nFN:Тато\nEND:VCARD"}),
        );
        assert_eq!(names(&dir).get(&number_key("0501234567")).map(String::as_str), Some("Мама"));
        // a changed, b deleted on the phone.
        // Unrequested vCards are not stored.
        on_vcards(&dir, &json!({"uids": ["zzz"], "zzz": "BEGIN:VCARD\nFN:X\nEND:VCARD"}));
        assert!(!dir.join("zzz.vcf").exists());
        let second = on_uids(&dir, &json!({"uids": ["a"], "a": 5})).expect("fetch a again");
        assert_eq!(second.body["uids"], json!(["a"]));
        assert!(!dir.join("b.vcf").exists());
        assert!(on_uids(&dir, &json!({"uids": ["a"], "a": 5})).is_none(), "nothing new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
