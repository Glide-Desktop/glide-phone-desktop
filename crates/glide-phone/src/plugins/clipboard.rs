use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::protocol::packet::{NetworkPacket, PACKET_TYPE_CLIPBOARD};

/// Sent once on connect so the newer clipboard wins on both sides.
pub const PACKET_TYPE_CLIPBOARD_CONNECT: &str = "kdeconnect.clipboard.connect";

pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardPayload {
    pub content: String,
    #[serde(default)]
    pub timestamp: Option<i64>,
}

#[must_use]
pub fn create_clipboard_packet(content: &str, timestamp: i64) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_CLIPBOARD, json!({ "content": content, "timestamp": timestamp }))
}

#[must_use]
pub fn create_connect_packet(content: &str, timestamp: i64) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_CLIPBOARD_CONNECT, json!({ "content": content, "timestamp": timestamp }))
}

pub fn handle_clipboard_packet(packet: &NetworkPacket) -> Result<ClipboardPayload, &'static str> {
    if packet.packet_type != PACKET_TYPE_CLIPBOARD && packet.packet_type != PACKET_TYPE_CLIPBOARD_CONNECT {
        return Err("unexpected clipboard packet");
    }
    let content = packet.body.get("content").and_then(serde_json::Value::as_str).ok_or("clipboard content missing")?;
    if content.len() > MAX_CLIPBOARD_BYTES {
        return Err("clipboard content exceeds size limit");
    }
    Ok(ClipboardPayload {
        content: content.to_string(),
        timestamp: packet.body.get("timestamp").and_then(serde_json::Value::as_i64),
    })
}

pub fn apply(content: &str) {
    if content.is_empty() || tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let text = content.to_string();
    tokio::spawn(async move {
        if !crate::plugins::share::copy_text(&text).await {
            tracing::warn!("the phone's clipboard was not applied: wl-copy is missing or was refused");
        }
    });
}

/// Watches this computer's clipboard (Wayland, `wl-paste --watch`) and reports each new
/// text to `on_change`. Secrets from password managers (`x-kde-passwordManagerHint`) are
/// skipped, like KDE Connect does.
pub fn watch(on_change: impl Fn(String) + Send + 'static) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt as _;
        let script = "wl-paste -l 2>/dev/null | grep -q x-kde-passwordManagerHint && { cat >/dev/null; exit 0; }; \
                      cat; printf '\\0'";
        loop {
            let child = tokio::process::Command::new("wl-paste")
                .args(["--type", "text", "--watch", "sh", "-c", script])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn();
            let Ok(mut child) = child else {
                tracing::info!("wl-paste is not available; the computer's clipboard is not shared");
                return;
            };
            if let Some(stdout) = child.stdout.take() {
                let mut reader = tokio::io::BufReader::new(stdout);
                let mut chunk = Vec::new();
                while reader.read_until(0, &mut chunk).await.is_ok_and(|read| read > 0) {
                    if chunk.last() == Some(&0) {
                        chunk.pop();
                    }
                    if chunk.len() <= MAX_CLIPBOARD_BYTES
                        && let Ok(text) = String::from_utf8(std::mem::take(&mut chunk))
                        && !text.is_empty()
                    {
                        on_change(text);
                    }
                    chunk.clear();
                }
            }
            let _ = child.wait().await;
            // The compositor went away or restarted: try again shortly.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_round_trip_preserves_timestamp() {
        let packet = create_clipboard_packet("Привіт", 42);
        assert_eq!(
            handle_clipboard_packet(&packet).unwrap(),
            ClipboardPayload { content: "Привіт".into(), timestamp: Some(42) }
        );
    }
}
