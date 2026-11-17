//! `kdeconnect.mousepad.request`: the phone as this computer's touchpad and keyboard
//! (KDE Connect's mousepad plugin on the desktop), injected through [`crate::input`].

use serde_json::{Value as Json, json};

use crate::input::{
    Action, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, KEY_LEFTALT, KEY_LEFTCTRL, KEY_LEFTMETA, KEY_LEFTSHIFT, KEY_V,
};
use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_MOUSEPAD_REQUEST: &str = "kdeconnect.mousepad.request";
pub const PACKET_TYPE_MOUSEPAD_ECHO: &str = "kdeconnect.mousepad.echo";
pub const PACKET_TYPE_MOUSEPAD_KEYBOARDSTATE: &str = "kdeconnect.mousepad.keyboardstate";
/// Scroll distance (in the phone's units) per wheel notch.
const SCROLL_STEP: f64 = 10.0;

/// KDE Connect's special key numbers -> Linux key codes.
#[must_use]
pub fn special_key(code: u64) -> Option<u16> {
    Some(match code {
        1 => 14,   // Backspace
        2 => 15,   // Tab
        4 => 105,  // Left
        5 => 103,  // Up
        6 => 106,  // Right
        7 => 108,  // Down
        8 => 104,  // Page Up
        9 => 109,  // Page Down
        10 => 102, // Home
        11 => 107, // End
        12 => 28,  // Enter
        13 => 111, // Delete
        14 => 1,   // Escape
        15 => 99,  // SysRq
        16 => 70,  // Scroll Lock
        17 => KEY_LEFTCTRL,
        18 => KEY_LEFTALT,
        19 => KEY_LEFTSHIFT,
        20 => KEY_LEFTMETA,
        21..=30 => 59 + (code - 21) as u16, // F1..F10
        31 => 87,                           // F11
        32 => 88,                           // F12
        _ => return None,
    })
}

/// Fractional pointer and scroll movement waiting for a whole pixel / notch.
#[derive(Debug, Default)]
pub struct Mousepad {
    pointer: (f64, f64),
    scroll: (f64, f64),
}

/// What a request asks for: device actions, and text no key code can type.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub text: Option<String>,
}

impl Mousepad {
    pub fn plan(&mut self, body: &Json) -> Plan {
        let flag = |key: &str| body.get(key).and_then(Json::as_bool).unwrap_or(false);
        let modifiers = [
            flag("ctrl").then_some(KEY_LEFTCTRL),
            flag("alt").then_some(KEY_LEFTALT),
            flag("shift").then_some(KEY_LEFTSHIFT),
            flag("super").then_some(KEY_LEFTMETA),
        ];
        let tap = |code| Action::Tap { code, modifiers: [None; 4] };
        let mut plan = Plan::default();
        if flag("singleclick") {
            plan.actions.push(tap(BTN_LEFT));
        }
        if flag("doubleclick") {
            plan.actions.extend([tap(BTN_LEFT), tap(BTN_LEFT)]);
        }
        if flag("middleclick") {
            plan.actions.push(tap(BTN_MIDDLE));
        }
        if flag("rightclick") {
            plan.actions.push(tap(BTN_RIGHT));
        }
        if flag("singlehold") {
            plan.actions.push(Action::Press(BTN_LEFT));
        }
        if flag("singlerelease") {
            plan.actions.push(Action::Release(BTN_LEFT));
        }
        let dx = body.get("dx").and_then(Json::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
        let dy = body.get("dy").and_then(Json::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
        if flag("scroll") {
            // Fingers moving up (dy > 0) scroll the page down, like the phone's own touchpad.
            self.scroll.0 += dx / SCROLL_STEP;
            self.scroll.1 += -dy / SCROLL_STEP;
            let (h, v) = (self.scroll.0.trunc(), self.scroll.1.trunc());
            self.scroll = (self.scroll.0 - h, self.scroll.1 - v);
            if h != 0.0 || v != 0.0 {
                plan.actions.push(Action::Scroll { vertical: v as i32, horizontal: h as i32 });
            }
        } else if dx != 0.0 || dy != 0.0 {
            self.pointer.0 += dx;
            self.pointer.1 += dy;
            let (x, y) = (self.pointer.0.trunc(), self.pointer.1.trunc());
            self.pointer = (self.pointer.0 - x, self.pointer.1 - y);
            if x != 0.0 || y != 0.0 {
                plan.actions.push(Action::Move(x as i32, y as i32));
            }
        }
        if let Some(code) = body.get("specialKey").and_then(Json::as_u64).and_then(special_key) {
            plan.actions.push(Action::Tap { code, modifiers });
        }
        if let Some(key) = body.get("key").and_then(Json::as_str).filter(|k| !k.is_empty()) {
            let shortcut = modifiers.iter().any(Option::is_some);
            let mut typed = Vec::new();
            let all_ascii = key.chars().all(|c| crate::input::ascii_key(c).is_some());
            if all_ascii {
                for c in key.chars() {
                    if let Some((code, shift)) = crate::input::ascii_key(c) {
                        let mut mods = if shortcut { modifiers } else { [None; 4] };
                        if shift {
                            mods[2] = Some(KEY_LEFTSHIFT);
                        }
                        typed.push(Action::Tap { code, modifiers: mods });
                    }
                }
                plan.actions.extend(typed);
            } else {
                plan.text = Some(key.to_string());
            }
        }
        plan
    }
}

/// The acknowledgement the phone asked for with `sendAck` (its typed-text echo).
#[must_use]
pub fn echo(body: &Json) -> Option<NetworkPacket> {
    if !body.get("sendAck").and_then(Json::as_bool).unwrap_or(false) {
        return None;
    }
    let mut reply = body.clone();
    if let Json::Object(map) = &mut reply {
        map.remove("sendAck");
        map.insert("isAck".into(), json!(true));
    }
    Some(NetworkPacket::new(PACKET_TYPE_MOUSEPAD_ECHO, reply))
}

#[must_use]
pub fn keyboard_state_packet(available: bool) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_MOUSEPAD_KEYBOARDSTATE, json!({ "state": available }))
}

/// Types text no US key code covers (e.g. Cyrillic): `wtype`, else paste it.
pub fn type_text(text: String, input: &crate::input::RemoteInput) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let paste = [Action::Tap { code: KEY_V, modifiers: [Some(KEY_LEFTCTRL), None, None, None] }];
    let queue = input.clone_queue();
    tokio::spawn(async move {
        let typed = tokio::process::Command::new("wtype").arg("--").arg(&text).status().await;
        if typed.is_ok_and(|status| status.success()) {
            return;
        }
        if crate::plugins::share::copy_text(&text).await {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            if let Some(queue) = queue {
                let _ = queue.send(paste.to_vec());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_moves_and_keys_become_device_actions() {
        let mut pad = Mousepad::default();
        assert_eq!(
            pad.plan(&json!({"singleclick": true})).actions,
            [Action::Tap { code: BTN_LEFT, modifiers: [None; 4] }]
        );
        assert_eq!(pad.plan(&json!({"singlehold": true})).actions, [Action::Press(BTN_LEFT)]);
        // Fractions carry over until a whole pixel.
        assert!(pad.plan(&json!({"dx": 0.6, "dy": 0.0})).actions.is_empty());
        assert_eq!(pad.plan(&json!({"dx": 0.6, "dy": -2.0})).actions, [Action::Move(1, -2)]);
        assert_eq!(
            pad.plan(&json!({"scroll": true, "dx": 0.0, "dy": 25.0})).actions,
            [Action::Scroll { vertical: -2, horizontal: 0 }]
        );
        let alt_f4 = pad.plan(&json!({"specialKey": 24, "alt": true})).actions;
        assert_eq!(alt_f4, [Action::Tap { code: 62, modifiers: [None, Some(KEY_LEFTALT), None, None] }]);
        let ctrl_c = pad.plan(&json!({"key": "c", "ctrl": true})).actions;
        assert_eq!(ctrl_c, [Action::Tap { code: 46, modifiers: [Some(KEY_LEFTCTRL), None, None, None] }]);
        assert_eq!(pad.plan(&json!({"key": "Привіт"})).text.as_deref(), Some("Привіт"));
    }

    #[test]
    fn acks_echo_the_key_back() {
        let reply = echo(&json!({"key": "a", "sendAck": true})).expect("ack");
        assert_eq!(reply.packet_type, PACKET_TYPE_MOUSEPAD_ECHO);
        assert_eq!(reply.body, json!({"key": "a", "isAck": true}));
        assert!(echo(&json!({"key": "a"})).is_none());
    }
}
