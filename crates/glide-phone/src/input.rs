//! A virtual mouse and keyboard on `/dev/uinput`, so the phone can drive this computer
//! under any compositor (the login user gets the device through `uaccess`).

use std::fs::File;
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::sync::mpsc;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const SYN_REPORT: u16 = 0;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_V: u16 = 47;

/// One thing to do with the virtual devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Move(i32, i32),
    Scroll {
        vertical: i32,
        horizontal: i32,
    },
    /// Press and release `code`, with `modifiers` held.
    Tap {
        code: u16,
        modifiers: [Option<u16>; 4],
    },
    Press(u16),
    Release(u16),
}

// _IOW('U', n, int) and friends, as in <linux/uinput.h>.
const fn iow(nr: u64, size: u64) -> u64 {
    (1 << 30) | (size << 16) | ((b'U' as u64) << 8) | nr
}
const fn io(nr: u64) -> u64 {
    ((b'U' as u64) << 8) | nr
}
const UI_SET_EVBIT: u64 = iow(100, 4);
const UI_SET_KEYBIT: u64 = iow(101, 4);
const UI_SET_RELBIT: u64 = iow(102, 4);
const UI_DEV_SETUP: u64 = iow(3, std::mem::size_of::<UinputSetup>() as u64);
const UI_DEV_CREATE: u64 = io(1);

#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

struct Device(File);

impl Device {
    fn open() -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new().write(true).open("/dev/uinput")?;
        let fd = file.as_raw_fd();
        let ioctl = |request: u64, value: libc::c_ulong| -> std::io::Result<()> {
            // SAFETY: uinput ioctls taking an int on an open uinput descriptor.
            let rc = unsafe { libc::ioctl(fd, request as libc::c_ulong, value) };
            if rc < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
        };
        ioctl(UI_SET_EVBIT, libc::c_ulong::from(EV_KEY))?;
        ioctl(UI_SET_EVBIT, libc::c_ulong::from(EV_REL))?;
        for code in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] {
            ioctl(UI_SET_RELBIT, libc::c_ulong::from(code))?;
        }
        for code in (1..=248).chain([BTN_LEFT, BTN_RIGHT, BTN_MIDDLE]) {
            ioctl(UI_SET_KEYBIT, libc::c_ulong::from(code))?;
        }
        let mut setup = UinputSetup {
            bustype: 0x06,
            vendor: 0x1d6b,
            product: 0x0104,
            version: 1,
            name: [0; 80],
            ff_effects_max: 0,
        };
        let name = b"Glide Phone remote input";
        setup.name[..name.len()].copy_from_slice(name);
        // SAFETY: UI_DEV_SETUP with a correctly laid out `uinput_setup`.
        if unsafe { libc::ioctl(fd, UI_DEV_SETUP as libc::c_ulong, &raw const setup) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: UI_DEV_CREATE takes no argument.
        if unsafe { libc::ioctl(fd, UI_DEV_CREATE as libc::c_ulong) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Give the compositor a moment to pick the new device up.
        std::thread::sleep(std::time::Duration::from_millis(200));
        Ok(Self(file))
    }

    fn emit(&mut self, kind: u16, code: u16, value: i32) -> std::io::Result<()> {
        let mut event = [0u8; 24]; // struct input_event: timeval (16), type, code, value
        event[16..18].copy_from_slice(&kind.to_ne_bytes());
        event[18..20].copy_from_slice(&code.to_ne_bytes());
        event[20..24].copy_from_slice(&value.to_ne_bytes());
        self.0.write_all(&event)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.emit(EV_SYN, SYN_REPORT, 0)
    }

    fn run(&mut self, action: Action) -> std::io::Result<()> {
        match action {
            Action::Move(dx, dy) => {
                self.emit(EV_REL, REL_X, dx)?;
                self.emit(EV_REL, REL_Y, dy)?;
            }
            Action::Scroll { vertical, horizontal } => {
                if vertical != 0 {
                    self.emit(EV_REL, REL_WHEEL, vertical)?;
                }
                if horizontal != 0 {
                    self.emit(EV_REL, REL_HWHEEL, horizontal)?;
                }
            }
            Action::Tap { code, modifiers } => {
                for m in modifiers.iter().flatten() {
                    self.emit(EV_KEY, *m, 1)?;
                }
                self.emit(EV_KEY, code, 1)?;
                self.sync()?;
                self.emit(EV_KEY, code, 0)?;
                for m in modifiers.iter().flatten() {
                    self.emit(EV_KEY, *m, 0)?;
                }
            }
            Action::Press(code) => self.emit(EV_KEY, code, 1)?,
            Action::Release(code) => self.emit(EV_KEY, code, 0)?,
        }
        self.sync()
    }
}

/// The virtual devices, created on first use and fed from one thread.
pub struct RemoteInput {
    queue: Option<mpsc::Sender<Vec<Action>>>,
}

impl RemoteInput {
    #[must_use]
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::channel::<Vec<Action>>();
        let spawned = std::thread::Builder::new().name("glide-phone-input".into()).spawn(move || {
            let mut device = None;
            while let Ok(actions) = receiver.recv() {
                if device.is_none() {
                    match Device::open() {
                        Ok(created) => device = Some(created),
                        Err(err) => {
                            tracing::warn!(%err, "cannot create the virtual mouse/keyboard (/dev/uinput)");
                            continue;
                        }
                    }
                }
                if let Some(dev) = device.as_mut() {
                    for action in actions {
                        if dev.run(action).is_err() {
                            device = None;
                            break;
                        }
                    }
                }
            }
        });
        Self { queue: spawned.ok().map(|_| sender) }
    }

    /// Whether `/dev/uinput` can be used by this user.
    #[must_use]
    pub fn available() -> bool {
        std::fs::OpenOptions::new().write(true).open("/dev/uinput").is_ok()
    }

    /// A handle to queue actions from another task.
    #[must_use]
    pub fn clone_queue(&self) -> Option<mpsc::Sender<Vec<Action>>> {
        self.queue.clone()
    }

    pub fn send(&self, actions: Vec<Action>) {
        if let Some(queue) = &self.queue
            && !actions.is_empty()
        {
            let _ = queue.send(actions);
        }
    }
}

/// US-layout key for an ASCII character: (keycode, needs shift).
#[must_use]
pub fn ascii_key(c: char) -> Option<(u16, bool)> {
    const ROW: &[(char, u16)] = &[
        ('1', 2),
        ('2', 3),
        ('3', 4),
        ('4', 5),
        ('5', 6),
        ('6', 7),
        ('7', 8),
        ('8', 9),
        ('9', 10),
        ('0', 11),
        ('-', 12),
        ('=', 13),
        ('q', 16),
        ('w', 17),
        ('e', 18),
        ('r', 19),
        ('t', 20),
        ('y', 21),
        ('u', 22),
        ('i', 23),
        ('o', 24),
        ('p', 25),
        ('[', 26),
        (']', 27),
        ('a', 30),
        ('s', 31),
        ('d', 32),
        ('f', 33),
        ('g', 34),
        ('h', 35),
        ('j', 36),
        ('k', 37),
        ('l', 38),
        (';', 39),
        ('\'', 40),
        ('`', 41),
        ('\\', 43),
        ('z', 44),
        ('x', 45),
        ('c', 46),
        ('v', 47),
        ('b', 48),
        ('n', 49),
        ('m', 50),
        (',', 51),
        ('.', 52),
        ('/', 53),
        (' ', 57),
        ('\n', 28),
        ('\t', 15),
    ];
    const SHIFTED: &[(char, char)] = &[
        ('!', '1'),
        ('@', '2'),
        ('#', '3'),
        ('$', '4'),
        ('%', '5'),
        ('^', '6'),
        ('&', '7'),
        ('*', '8'),
        ('(', '9'),
        (')', '0'),
        ('_', '-'),
        ('+', '='),
        ('{', '['),
        ('}', ']'),
        (':', ';'),
        ('"', '\''),
        ('~', '`'),
        ('|', '\\'),
        ('<', ','),
        ('>', '.'),
        ('?', '/'),
    ];
    let lookup = |c: char| ROW.iter().find(|(k, _)| *k == c).map(|(_, code)| *code);
    if c.is_ascii_uppercase() {
        return lookup(c.to_ascii_lowercase()).map(|code| (code, true));
    }
    if let Some(code) = lookup(c) {
        return Some((code, false));
    }
    SHIFTED.iter().find(|(k, _)| *k == c).and_then(|(_, base)| lookup(*base)).map(|code| (code, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates the device and drops it without sending a single event.
    #[test]
    #[ignore = "creates a real input device; run by hand"]
    fn the_virtual_device_can_be_created() {
        assert!(Device::open().is_ok());
    }

    #[test]
    fn ascii_maps_onto_a_us_keyboard() {
        assert_eq!(ascii_key('a'), Some((30, false)));
        assert_eq!(ascii_key('A'), Some((30, true)));
        assert_eq!(ascii_key('?'), Some((53, true)));
        assert_eq!(ascii_key('ї'), None);
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
    }
}
