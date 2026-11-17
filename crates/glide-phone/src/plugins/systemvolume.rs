//! `kdeconnect.systemvolume`: this computer's audio outputs for the phone (KDE Connect's
//! `SystemvolumePlugin`), through `pactl` (`PulseAudio` or `PipeWire`'s pulse server).

use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_SYSTEMVOLUME: &str = "kdeconnect.systemvolume";
pub const PACKET_TYPE_SYSTEMVOLUME_REQUEST: &str = "kdeconnect.systemvolume.request";
/// `PA_VOLUME_NORM`: 100 %.
pub const MAX_VOLUME: i64 = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sink {
    pub name: String,
    pub description: String,
    pub volume: i64,
    pub muted: bool,
    pub default: bool,
}

#[derive(Deserialize)]
struct PactlSink {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: std::collections::BTreeMap<String, PactlVolume>,
}

#[derive(Deserialize)]
struct PactlVolume {
    value: i64,
}

/// Parses `pactl -f json list sinks`; the sink volume is its loudest channel.
#[must_use]
pub fn parse_sinks(json: &str, default_sink: &str) -> Vec<Sink> {
    let sinks: Vec<PactlSink> = serde_json::from_str(json).unwrap_or_default();
    sinks
        .into_iter()
        .map(|sink| Sink {
            default: sink.name == default_sink,
            volume: sink.volume.values().map(|v| v.value).max().unwrap_or(0),
            description: if sink.description.is_empty() { sink.name.clone() } else { sink.description },
            muted: sink.mute,
            name: sink.name,
        })
        .collect()
}

async fn pactl(args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("pactl").args(args).output().await.ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub async fn sinks() -> Vec<Sink> {
    let (Some(json), default) = (pactl(&["-f", "json", "list", "sinks"]).await, pactl(&["get-default-sink"]).await)
    else {
        return Vec::new();
    };
    parse_sinks(&json, default.as_deref().unwrap_or_default().trim())
}

#[must_use]
pub fn sink_list_packet(sinks: &[Sink]) -> NetworkPacket {
    let list: Vec<Json> = sinks
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "volume": s.volume,
                "maxVolume": MAX_VOLUME,
                "muted": s.muted,
                "enabled": s.default,
            })
        })
        .collect();
    NetworkPacket::new(PACKET_TYPE_SYSTEMVOLUME, json!({ "sinkList": list }))
}

/// The `pactl` calls a phone's request asks for.
#[must_use]
pub fn commands_for(body: &Json) -> Vec<Vec<String>> {
    let Some(name) = body.get("name").and_then(Json::as_str) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(volume) = body.get("volume").and_then(Json::as_i64) {
        out.push(vec!["set-sink-volume".into(), name.into(), volume.clamp(0, MAX_VOLUME * 3 / 2).to_string()]);
    }
    if let Some(muted) = body.get("muted").and_then(Json::as_bool) {
        out.push(vec!["set-sink-mute".into(), name.into(), if muted { "1" } else { "0" }.into()]);
    }
    if body.get("enabled").and_then(Json::as_bool) == Some(true) {
        out.push(vec!["set-default-sink".into(), name.into()]);
    }
    out
}

/// Applies a request and answers with the current outputs.
pub async fn handle_request(body: &Json, sender: &crate::state::PacketSender) {
    for command in commands_for(body) {
        let args: Vec<&str> = command.iter().map(String::as_str).collect();
        let _ = pactl(&args).await;
    }
    let wants_list = body.get("requestSinks").and_then(Json::as_bool).unwrap_or(false) || body.get("name").is_some();
    if wants_list {
        let _ = sender.send(sink_list_packet(&sinks().await));
    }
}

/// Re-sends the outputs to phones whenever they change (`pactl subscribe`).
pub async fn watch(on_packet: impl Fn(NetworkPacket) + Send + 'static) {
    use tokio::io::AsyncBufReadExt as _;
    loop {
        let Ok(mut child) = tokio::process::Command::new("pactl")
            .arg("subscribe")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        else {
            tracing::info!("pactl is not available; the computer's volume is not shared");
            return;
        };
        let Some(stdout) = child.stdout.take() else { return };
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let mut last = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if !(line.contains(" sink ") || line.contains(" server ")) {
                continue;
            }
            // Coalesce bursts (a volume drag sends dozens of events).
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let now = sinks().await;
            if now != last {
                on_packet(sink_list_packet(&now));
                last = now;
            }
        }
        let _ = child.wait().await;
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pactl_sinks_become_kde_connect_sinks() {
        let json = r#"[{"name":"hdmi","description":"HDMI","mute":false,
            "volume":{"front-left":{"value":34152},"front-right":{"value":30000}}},
            {"name":"usb","description":"","mute":true,"volume":{"mono":{"value":0}}}]"#;
        let sinks = parse_sinks(json, "hdmi");
        assert_eq!(
            sinks[0],
            Sink { name: "hdmi".into(), description: "HDMI".into(), volume: 34152, muted: false, default: true }
        );
        assert_eq!(sinks[1].description, "usb");
        let packet = sink_list_packet(&sinks);
        assert_eq!(packet.body["sinkList"][0]["maxVolume"], MAX_VOLUME);
        assert_eq!(packet.body["sinkList"][1]["muted"], true);
    }

    #[test]
    fn requests_map_to_pactl_calls() {
        assert_eq!(commands_for(&json!({"name": "hdmi", "volume": 1000})), [["set-sink-volume", "hdmi", "1000"]]);
        assert_eq!(commands_for(&json!({"name": "hdmi", "muted": true})), [["set-sink-mute", "hdmi", "1"]]);
        assert_eq!(commands_for(&json!({"name": "usb", "enabled": true})), [["set-default-sink", "usb"]]);
        assert!(commands_for(&json!({"requestSinks": true})).is_empty());
    }
}
