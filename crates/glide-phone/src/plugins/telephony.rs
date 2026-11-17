//! `kdeconnect.telephony`: calls on the phone (KDE Connect's telephony plugin), with a
//! "Mute" button sending `kdeconnect.telephony.request_mute`, and media paused while a call
//! rings or runs (KDE Connect's "pause media during calls").

use std::collections::HashMap;
use std::sync::OnceLock;

use futures_util::StreamExt as _;
use serde_json::{Value as Json, json};
use tokio::sync::Mutex;

use crate::protocol::packet::NetworkPacket;
use crate::state::PacketSender;

pub const PACKET_TYPE_TELEPHONY: &str = "kdeconnect.telephony";
pub const PACKET_TYPE_TELEPHONY_REQUEST_MUTE: &str = "kdeconnect.telephony.request_mute";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Ringing { caller: String },
    Talking { caller: String },
    Missed { caller: String },
    Ended,
}

/// Reads a telephony packet; the caller is the contact's name, else the number.
#[must_use]
pub fn parse(body: &Json) -> Option<Call> {
    let caller = body
        .get("contactName")
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| body.get("phoneNumber").and_then(Json::as_str).filter(|s| !s.is_empty()))
        .unwrap_or_default()
        .to_string();
    if body.get("isCancel").and_then(Json::as_bool).unwrap_or(false) {
        return Some(Call::Ended);
    }
    match body.get("event").and_then(Json::as_str)? {
        "ringing" => Some(Call::Ringing { caller }),
        "talking" => Some(Call::Talking { caller }),
        "missedCall" => Some(Call::Missed { caller }),
        _ => None,
    }
}

#[must_use]
pub fn mute_packet() -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_TELEPHONY_REQUEST_MUTE, json!({}))
}

#[derive(Default)]
struct CallState {
    notification: Option<u32>,
    paused: Vec<String>,
}

fn calls() -> &'static Mutex<HashMap<String, CallState>> {
    static CALLS: OnceLock<Mutex<HashMap<String, CallState>>> = OnceLock::new();
    CALLS.get_or_init(Mutex::default)
}

fn thumbnail(body: &Json, device_id: &str) -> Option<String> {
    let encoded = body.get("phoneThumbnail").and_then(Json::as_str)?;
    let bytes = decode_base64(encoded)?;
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let path = base.join(format!(
        "glide-phone-caller-{}.jpg",
        device_id.chars().filter(char::is_ascii_alphanumeric).collect::<String>()
    ));
    glide_config::write_atomic(&path, &bytes, 0o600).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Standard base64 (the phone's `Base64.NO_WRAP` thumbnails).
#[must_use]
pub fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// Shows, updates or withdraws the call notification and pauses or resumes media.
pub fn handle(device_id: &str, device_name: &str, body: &Json, sender: Option<PacketSender>) {
    let Some(call) = parse(body) else { return };
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let (device_id, device_name, picture) =
        (device_id.to_string(), device_name.to_string(), thumbnail(body, device_id));
    tokio::spawn(async move {
        let mut guard = calls().lock().await;
        let state = guard.entry(device_id.clone()).or_default();
        if let Some(id) = state.notification.take() {
            let _ = glide_sdk::system::withdraw(id).await;
        }
        match &call {
            Call::Ringing { caller } | Call::Talking { caller } => {
                if state.paused.is_empty() {
                    state.paused = pause_media().await;
                }
                if let Call::Ringing { .. } = call {
                    let summary = crate::i18n::t(crate::i18n::Key::IncomingCall).replace("{caller}", caller);
                    let mut note = glide_sdk::system::Notification::new("glide-phone", summary)
                        .origin_device(&device_name)
                        .icon("call-start-symbolic")
                        .urgency(glide_sdk::system::Urgency::Critical)
                        .action("mute", crate::i18n::t(crate::i18n::Key::MuteCall));
                    if let Some(picture) = &picture {
                        note = note.image(picture);
                    }
                    if let Ok(id) = glide_sdk::system::notify(note).await {
                        state.notification = Some(id);
                        drop(guard);
                        wait_for_mute(id, sender).await;
                    }
                }
            }
            Call::Missed { caller } => {
                let summary = crate::i18n::t(crate::i18n::Key::MissedCall).replace("{caller}", caller);
                let mut note = glide_sdk::system::Notification::new("glide-phone", summary)
                    .origin_device(&device_name)
                    .icon("call-missed-symbolic");
                if let Some(picture) = &picture {
                    note = note.image(picture);
                }
                let _ = glide_sdk::system::notify(note).await;
                resume_media(std::mem::take(&mut state.paused)).await;
            }
            Call::Ended => resume_media(std::mem::take(&mut state.paused)).await,
        }
    });
}

async fn wait_for_mute(id: u32, sender: Option<PacketSender>) {
    let mut events = glide_sdk::system::notification_events();
    while let Ok(Some(event)) = tokio::time::timeout(std::time::Duration::from_mins(2), events.next()).await {
        match event {
            glide_sdk::system::NotificationEvent::Action { id: shown, key } if shown == id && key == "mute" => {
                if let Some(sender) = &sender {
                    let _ = sender.send(mute_packet());
                }
                return;
            }
            glide_sdk::system::NotificationEvent::Closed { id: shown, .. } if shown == id => return,
            _ => {}
        }
    }
}

/// Pauses every playing local player and returns their bus names.
async fn pause_media() -> Vec<String> {
    let Some(conn) = crate::plugins::mpris::session().await else { return Vec::new() };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else { return Vec::new() };
    let mut paused = Vec::new();
    for name in dbus.list_names().await.unwrap_or_default() {
        let name = name.to_string();
        // Never the phone's own players we publish: pausing them would pause the call's audio.
        if !name.starts_with("org.mpris.MediaPlayer2.") || name.starts_with("org.mpris.MediaPlayer2.glidephone.") {
            continue;
        }
        let Ok(player) =
            zbus::Proxy::new(&conn, name.clone(), "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player").await
        else {
            continue;
        };
        if player.get_property::<String>("PlaybackStatus").await.is_ok_and(|s| s == "Playing")
            && player.call_method("Pause", &()).await.is_ok()
        {
            paused.push(name);
        }
    }
    paused
}

async fn resume_media(players: Vec<String>) {
    let Some(conn) = crate::plugins::mpris::session().await else { return };
    for name in players {
        if let Ok(player) =
            zbus::Proxy::new(&conn, name, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player").await
        {
            let _ = player.call_method("Play", &()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_read_like_kde_connect() {
        assert_eq!(
            parse(&json!({"event": "ringing", "contactName": "Мама", "phoneNumber": "+380"})),
            Some(Call::Ringing { caller: "Мама".into() })
        );
        assert_eq!(
            parse(&json!({"event": "missedCall", "phoneNumber": "+380"})),
            Some(Call::Missed { caller: "+380".into() })
        );
        assert_eq!(parse(&json!({"event": "ringing", "isCancel": true})), Some(Call::Ended));
        assert_eq!(parse(&json!({"event": "sms"})), None);
    }

    #[test]
    fn thumbnails_decode() {
        assert_eq!(decode_base64("aGVsbG8=").as_deref(), Some(b"hello".as_slice()));
        assert_eq!(decode_base64("!!"), None);
    }
}
