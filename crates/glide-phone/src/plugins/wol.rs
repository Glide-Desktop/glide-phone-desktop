//! `glide.wol` (Glide only): tells paired phones this computer's network card addresses so
//! they can wake it with a Wake-on-LAN magic packet while it sleeps or is switched off.

use serde_json::json;

use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_WOL: &str = "glide.wol";

/// Interfaces that can't wake the machine (loopback, bridges, containers, VPNs, ...).
fn virtual_interface(name: &str) -> bool {
    name == "lo"
        || ["docker", "veth", "virbr", "br-", "tun", "tap", "wg", "vmnet", "vboxnet", "tailscale", "zt", "lxc"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6
        && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
        && mac != "00:00:00:00:00:00"
}

/// The MAC addresses of this computer's physical network cards (`/sys/class/net`).
#[must_use]
pub fn local_macs() -> Vec<String> {
    let mut macs = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else { return macs };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Only real hardware has a `device` link.
        if virtual_interface(&name) || !entry.path().join("device").exists() {
            continue;
        }
        if let Ok(mac) = std::fs::read_to_string(entry.path().join("address")) {
            let mac = mac.trim().to_ascii_lowercase();
            if valid_mac(&mac) && !macs.contains(&mac) {
                macs.push(mac);
            }
        }
    }
    macs.sort();
    macs
}

#[must_use]
pub fn packet(macs: &[String]) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_WOL, json!({ "macs": macs }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_addresses_are_offered() {
        assert!(valid_mac("a8:5e:45:01:02:03"));
        assert!(!valid_mac("00:00:00:00:00:00"));
        assert!(!valid_mac("a8:5e:45:01:02"));
        assert!(virtual_interface("docker0") && virtual_interface("lo") && !virtual_interface("enp5s0"));
        for mac in local_macs() {
            assert!(valid_mac(&mac));
        }
        assert_eq!(packet(&["a8:5e:45:01:02:03".into()]).body["macs"][0], "a8:5e:45:01:02:03");
    }
}
