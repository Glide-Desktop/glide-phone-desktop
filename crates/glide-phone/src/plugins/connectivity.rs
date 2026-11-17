//! `kdeconnect.connectivity_report`: the phone's mobile network per SIM (KDE Connect's
//! connectivity report), shown on its card.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

pub const PACKET_TYPE_CONNECTIVITY_REPORT: &str = "kdeconnect.connectivity_report";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    /// e.g. "LTE", "5G", "HSPA", "Unknown".
    pub network_type: String,
    /// 0 (none) to 4 (full).
    pub strength: i64,
}

/// One entry per SIM, by subscription id order.
#[must_use]
pub fn parse(body: &Json) -> Vec<Signal> {
    let Some(map) = body.get("signalStrengths").and_then(Json::as_object) else { return Vec::new() };
    let mut entries: Vec<(&String, &Json)> = map.iter().collect();
    entries.sort_by_key(|(id, _)| id.parse::<i64>().unwrap_or(i64::MAX));
    entries
        .into_iter()
        .map(|(_, v)| Signal {
            network_type: v.get("networkType").and_then(Json::as_str).unwrap_or("Unknown").to_string(),
            strength: v.get("signalStrength").and_then(Json::as_i64).unwrap_or(0).clamp(0, 4),
        })
        .collect()
}

/// "LTE ▂▄▆_" — bars like the phone's status bar.
#[must_use]
pub fn label(signal: &Signal) -> String {
    let bars: String = ["▂", "▄", "▆", "█"]
        .iter()
        .enumerate()
        .map(|(i, bar)| if (i as i64) < signal.strength { *bar } else { "_" })
        .collect();
    format!("{} {bars}", signal.network_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sims_are_read_in_order_and_drawn_as_bars() {
        let body = json!({"signalStrengths": {"2": {"networkType": "5G", "signalStrength": 4},
                                              "1": {"networkType": "LTE", "signalStrength": 2}}});
        let sims = parse(&body);
        assert_eq!(sims[0].network_type, "LTE");
        assert_eq!(label(&sims[0]), "LTE ▂▄__");
        assert_eq!(label(&sims[1]), "5G ▂▄▆█");
    }
}
