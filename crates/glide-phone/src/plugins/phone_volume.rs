//! Glide only: this computer sets the phone's volumes (media, ringer, notifications, alarm).
//! The Glide Phone app offers its audio streams as KDE Connect `systemvolume` sinks, the packets
//! the phone uses for this computer's outputs, sent the other way round.

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::plugins::systemvolume::{PACKET_TYPE_SYSTEMVOLUME, PACKET_TYPE_SYSTEMVOLUME_REQUEST};
use crate::protocol::packet::NetworkPacket;

/// At most this many streams per phone.
const MAX_SINKS: usize = 16;

/// One of the phone's audio streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneSink {
    pub name: String,
    pub description: String,
    /// 0..=`max_volume`.
    pub volume: i64,
    pub max_volume: i64,
    pub muted: bool,
}

fn text(value: Option<&Json>, limit: usize) -> Option<String> {
    let text: String = value?.as_str()?.chars().filter(|c| !c.is_control()).take(limit).collect();
    (!text.trim().is_empty()).then_some(text)
}

/// The phone's `kdeconnect.systemvolume` `sinkList`; None when the packet carries no list.
#[must_use]
pub fn parse(body: &Json) -> Option<Vec<PhoneSink>> {
    let list = body.get("sinkList")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|sink| {
                let max_volume = sink.get("maxVolume").and_then(Json::as_i64).unwrap_or(100).clamp(1, 65_536);
                Some(PhoneSink {
                    name: text(sink.get("name"), 64)?,
                    description: text(sink.get("description"), 64).unwrap_or_default(),
                    volume: sink.get("volume").and_then(Json::as_i64).unwrap_or(0).clamp(0, max_volume),
                    max_volume,
                    muted: sink.get("muted").and_then(Json::as_bool).unwrap_or(false),
                })
            })
            .take(MAX_SINKS)
            .collect(),
    )
}

/// Asks the phone for its streams.
#[must_use]
pub fn request_packet() -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_SYSTEMVOLUME_REQUEST, json!({ "requestSinks": true }))
}

#[must_use]
pub fn volume_packet(name: &str, volume: i64) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_SYSTEMVOLUME_REQUEST, json!({ "name": name, "volume": volume }))
}

#[must_use]
pub fn mute_packet(name: &str, muted: bool) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_SYSTEMVOLUME_REQUEST, json!({ "name": name, "muted": muted }))
}

/// Whether a `kdeconnect.systemvolume` packet is the phone's own list (not for us to apply).
#[must_use]
pub fn is_phone_list(packet: &NetworkPacket) -> bool {
    packet.packet_type == PACKET_TYPE_SYSTEMVOLUME && packet.body.get("sinkList").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phones_streams_are_read_with_limits() {
        let body = json!({ "sinkList": [
            { "name": "phone.media", "description": "Медіа", "volume": 40, "maxVolume": 100, "muted": false },
            { "name": "phone.ring", "description": "Дзвінок", "volume": 900, "maxVolume": 100, "muted": true },
            { "description": "no name" },
        ]});
        let sinks = parse(&body).expect("a list");
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[0].description, "Медіа");
        assert_eq!(sinks[1].volume, 100, "clamped to the maximum");
        assert!(sinks[1].muted);
        assert!(parse(&json!({ "volume": 3 })).is_none());
        assert_eq!(volume_packet("phone.media", 55).body, json!({ "name": "phone.media", "volume": 55 }));
    }
}
