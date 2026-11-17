//! Layout-independent typing through the Wayland virtual keyboard
//! (`zwp_virtual_keyboard_v1`, as `wtype` does): each text gets a keymap whose keys produce
//! exactly its characters, so Cyrillic or emoji type correctly whatever layout is active.

use std::fmt::Write as _;
use std::io::Write as _;
use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

pub struct VirtualKeyboard {
    queue: EventQueue<State>,
    keyboard: ZwpVirtualKeyboardV1,
    _connection: Connection,
}

/// The xkb keysym name for a character (`U0444` works for any code point).
#[must_use]
pub fn keysym_name(c: char) -> String {
    match c {
        '\n' => "Return".into(),
        '\t' => "Tab".into(),
        ' ' => "space".into(),
        _ => format!("U{:04X}", u32::from(c)),
    }
}

/// A keymap where key N (evdev N) types `chars[N - 1]`.
#[must_use]
pub fn keymap(chars: &[char]) -> String {
    let mut codes = String::new();
    let mut symbols = String::new();
    for (i, c) in chars.iter().enumerate() {
        let code = i + 9; // xkb keycode = evdev + 8, evdev starts at 1
        let _ = writeln!(codes, "<K{i}> = {code};");
        let _ = writeln!(symbols, "key <K{i}> {{ [ {} ] }};", keysym_name(*c));
    }
    format!(
        "xkb_keymap {{\nxkb_keycodes \"glide\" {{ minimum = 8; maximum = {max};\n{codes}}};\n\
         xkb_types \"glide\" {{ include \"complete\" }};\nxkb_compatibility \"glide\" {{ include \"complete\" }};\n\
         xkb_symbols \"glide\" {{\n{symbols}}};\n}};\n",
        max = chars.len() + 9,
    )
}

impl VirtualKeyboard {
    /// Connects to the compositor; None when there is no Wayland session or no protocol.
    pub fn connect() -> Option<Self> {
        let connection = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection).ok()?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).ok()?;
        let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).ok()?;
        let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        queue.roundtrip(&mut State).ok()?;
        Some(Self { queue, keyboard, _connection: connection })
    }

    /// Types `text`; false when the compositor went away.
    pub fn type_text(&mut self, text: &str) -> bool {
        let mut chars: Vec<char> = Vec::new();
        for c in text.chars() {
            if !chars.contains(&c) {
                chars.push(c);
            }
        }
        // Keep keymaps small: type long texts in chunks of distinct characters.
        if chars.len() > 200 {
            let split = text.char_indices().nth(text.chars().count() / 2).map_or(text.len(), |(i, _)| i);
            return self.type_text(&text[..split]) && self.type_text(&text[split..]);
        }
        let map = keymap(&chars);
        let Some(fd) = memfd(map.as_bytes()) else { return false };
        let size = u32::try_from(map.len() + 1).unwrap_or(u32::MAX);
        self.keyboard.keymap(1 /* XKB_V1 */, fd.as_fd(), size);
        let mut time = 0u32;
        for c in text.chars() {
            let Some(index) = chars.iter().position(|x| *x == c) else { continue };
            let key = u32::try_from(index + 1).unwrap_or(1);
            self.keyboard.key(time, key, 1);
            time += 1;
            self.keyboard.key(time, key, 0);
            time += 1;
        }
        self.queue.roundtrip(&mut State).is_ok()
    }
}

/// An anonymous file holding the keymap (with its terminating NUL, as xkb expects).
fn memfd(bytes: &[u8]) -> Option<OwnedFd> {
    // SAFETY: memfd_create with a static NUL-terminated name.
    let raw = unsafe { libc::memfd_create(c"glide-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return None;
    }
    // SAFETY: `raw` is a fresh descriptor we own.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes).ok()?;
    file.write_all(&[0]).ok()?;
    Some(OwnedFd::from(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connects and creates the keyboard without typing anything.
    #[test]
    #[ignore = "talks to the running compositor; run by hand"]
    fn the_virtual_keyboard_is_available() {
        let connection = Connection::connect_to_env().expect("wayland");
        let (globals, _queue) = registry_queue_init::<State>(&connection).expect("registry");
        let names: Vec<String> =
            globals.contents().clone_list().into_iter().map(|g| format!("{} v{}", g.interface, g.version)).collect();
        eprintln!("globals: {names:?}");
        // Some compositors (Glide today) keep the protocol for privileged clients only.
        eprintln!("virtual keyboard available: {}", VirtualKeyboard::connect().is_some());
    }

    #[test]
    fn every_character_gets_its_own_key() {
        let map = keymap(&['п', 'a', '\n']);
        assert!(map.contains("key <K0> { [ U043F ] };"));
        assert!(map.contains("key <K1> { [ U0061 ] };"));
        assert!(map.contains("key <K2> { [ Return ] };"));
        assert!(map.contains("<K2> = 11;"));
    }
}
