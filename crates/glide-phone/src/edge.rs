//! Share input devices from the computer (KDE Connect's `shareinputdevices`) without the
//! `InputCapture` portal: a 1 px transparent layer-shell strip on the chosen screen edge notices
//! the pointer arriving there. The pointer is then locked to the strip (pointer constraints),
//! keyboard focus and shortcuts are taken, and relative motion, buttons, scrolling and keys go
//! to the phone until it hands the cursor back (or the user presses Ctrl+Alt+Esc).
//!
//! The Wayland side runs on its own thread; [`Translator`] turns what it reports into KDE
//! Connect packets and is pure, so it is unit-tested.

use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::mpsc;

use serde_json::json;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use xkbcommon::xkb;

use crate::plugins::mousepad::PACKET_TYPE_MOUSEPAD_REQUEST;
use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_SHAREINPUTDEVICES: &str = "kdeconnect.shareinputdevices";
pub const PACKET_TYPE_SHAREINPUTDEVICES_REQUEST: &str = "kdeconnect.shareinputdevices.request";

/// The side of this computer's screen the phone sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.name() == name)
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    /// `Qt::Edge` of the side the cursor leaves this screen by. The phone reads the same bits
    /// mirrored (Qt's `LeftEdge` 0x2 is Android's `RIGHT_EDGE` 0x2), so it enters on its far side.
    #[must_use]
    pub fn kde(self) -> i64 {
        match self {
            Self::Top => 0x1,
            Self::Left => 0x2,
            Self::Right => 0x4,
            Self::Bottom => 0x8,
        }
    }

    fn vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// What the strip noticed while the phone has the cursor.
#[derive(Debug, Clone, PartialEq)]
pub enum Captured {
    /// The pointer reached the edge at `along` pixels from its start (top or left).
    Entered {
        along: i32,
    },
    Motion {
        dx: f64,
        dy: f64,
    },
    /// Linux button code (`BTN_LEFT` = 0x110, ...).
    Button {
        code: u32,
        pressed: bool,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
    /// A key press: its keysym, the text it types now, the text without modifiers.
    Key {
        sym: u32,
        text: String,
        base: String,
        ctrl: bool,
        alt: bool,
        shift: bool,
        logo: bool,
    },
    /// The user took the cursor back (Ctrl+Alt+Esc) or the compositor ended the lock.
    TakenBack,
}

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// KDE Connect's special key number for a keysym.
#[must_use]
pub fn special_key(sym: u32) -> Option<u64> {
    Some(match sym {
        xkb::keysyms::KEY_BackSpace => 1,
        xkb::keysyms::KEY_Tab | xkb::keysyms::KEY_ISO_Left_Tab => 2,
        xkb::keysyms::KEY_Left => 4,
        xkb::keysyms::KEY_Up => 5,
        xkb::keysyms::KEY_Right => 6,
        xkb::keysyms::KEY_Down => 7,
        xkb::keysyms::KEY_Page_Up => 8,
        xkb::keysyms::KEY_Page_Down => 9,
        xkb::keysyms::KEY_Home => 10,
        xkb::keysyms::KEY_End => 11,
        xkb::keysyms::KEY_Return | xkb::keysyms::KEY_KP_Enter => 12,
        xkb::keysyms::KEY_Delete => 13,
        xkb::keysyms::KEY_Escape => 14,
        xkb::keysyms::KEY_Print | xkb::keysyms::KEY_Sys_Req => 15,
        xkb::keysyms::KEY_Scroll_Lock => 16,
        f @ xkb::keysyms::KEY_F1..=xkb::keysyms::KEY_F12 => 21 + u64::from(f - xkb::keysyms::KEY_F1),
        _ => return None,
    })
}

/// Turns captured input into packets for the phone.
#[derive(Debug, Default)]
pub struct Translator {
    left_down: bool,
    dragging: bool,
}

impl Translator {
    #[must_use]
    pub fn packets(&mut self, event: &Captured, edge: Edge) -> Vec<NetworkPacket> {
        let pad = |body: serde_json::Value| NetworkPacket::new(PACKET_TYPE_MOUSEPAD_REQUEST, body);
        match event {
            Captured::Entered { along } => {
                *self = Self::default();
                let (dx, dy) = if edge.vertical() { (0, *along) } else { (*along, 0) };
                vec![NetworkPacket::new(
                    PACKET_TYPE_SHAREINPUTDEVICES_REQUEST,
                    json!({ "exitEdge": edge.kde(), "deltax": dx, "deltay": dy }),
                )]
            }
            Captured::Motion { dx, dy } => {
                let mut out = Vec::new();
                // A press that moves is a drag: hold, move, release (the phone's swipe).
                if self.left_down && !self.dragging {
                    self.dragging = true;
                    out.push(pad(json!({ "singlehold": true })));
                }
                out.push(pad(json!({ "dx": dx, "dy": dy })));
                out
            }
            Captured::Button { code, pressed } => match (*code, *pressed) {
                (BTN_LEFT, true) => {
                    self.left_down = true;
                    Vec::new()
                }
                (BTN_LEFT, false) => {
                    let was_drag = std::mem::take(&mut self.dragging);
                    self.left_down = false;
                    vec![pad(if was_drag { json!({ "singlerelease": true }) } else { json!({ "singleclick": true }) })]
                }
                (BTN_RIGHT, true) => vec![pad(json!({ "rightclick": true }))],
                (BTN_MIDDLE, true) => vec![pad(json!({ "middleclick": true }))],
                (BTN_SIDE, true) => vec![pad(json!({ "backclick": true }))],
                (BTN_EXTRA, true) => vec![pad(json!({ "forwardclick": true }))],
                _ => Vec::new(),
            },
            Captured::Scroll { dx, dy } => vec![pad(json!({ "scroll": true, "dx": dx, "dy": dy }))],
            Captured::Key { sym, text, base, ctrl, alt, shift, logo } => {
                let mods = |mut body: serde_json::Value| {
                    for (name, on) in [("ctrl", *ctrl), ("alt", *alt), ("shift", *shift), ("super", *logo)] {
                        if on {
                            body[name] = json!(true);
                        }
                    }
                    body
                };
                let printable = |t: &str| !t.is_empty() && !t.chars().any(char::is_control);
                if let Some(code) = special_key(*sym) {
                    vec![pad(mods(json!({ "specialKey": code })))]
                } else if (*ctrl || *alt || *logo) && printable(base) {
                    // Shortcuts go by the key itself (Ctrl+C, not the control character).
                    vec![pad(mods(json!({ "key": base.to_lowercase() })))]
                } else if printable(text) {
                    vec![pad(json!({ "key": text }))]
                } else {
                    Vec::new() // modifiers alone, dead keys, ...
                }
            }
            Captured::TakenBack => Vec::new(),
        }
    }
}

enum Control {
    Release { along: i32 },
    Stop,
}

/// The running strip; dropping it removes the strip.
pub struct EdgeCapture {
    control: mpsc::Sender<Control>,
    wake: OwnedFd,
    pub edge: Edge,
}

impl EdgeCapture {
    /// Puts the strip on `edge`; None without a Wayland session or the needed protocols.
    pub fn start(edge: Edge, emit: impl Fn(Captured) + Send + 'static) -> Option<Self> {
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills two fresh descriptors we then own.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return None;
        }
        // SAFETY: both descriptors were just created and are owned here only.
        let (wake_read, wake) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let (control, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("glide-phone-edge".into())
            .spawn(move || run(edge, Box::new(emit), &receiver, &wake_read, &ready_tx))
            .ok()?;
        // Wait until the strip exists (or failed), so callers know whether it works.
        ready_rx.recv_timeout(std::time::Duration::from_secs(5)).ok().filter(|ok| *ok)?;
        Some(Self { control, wake, edge })
    }

    /// The phone gave the cursor back at `along` pixels along the edge.
    pub fn release(&self, along: i32) {
        let _ = self.control.send(Control::Release { along });
        self.poke();
    }

    fn poke(&self) {
        // SAFETY: a one-byte write to our own pipe.
        unsafe {
            libc::write(self.wake.as_raw_fd(), [1u8].as_ptr().cast(), 1);
        }
    }
}

impl Drop for EdgeCapture {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Stop);
        self.poke();
    }
}

struct Strip {
    edge: Edge,
    emit: Box<dyn Fn(Captured) + Send>,
    qh: QueueHandle<Strip>,
    shm: wl_shm::WlShm,
    seat: wl_seat::WlSeat,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    constraints: ZwpPointerConstraintsV1,
    relative_manager: ZwpRelativePointerManagerV1,
    inhibit_manager: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    size: (u32, u32),
    buffer: Option<wl_buffer::WlBuffer>,
    locked: Option<ZwpLockedPointerV1>,
    relative: Option<ZwpRelativePointerV1>,
    inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    capturing: bool,
    /// Re-armed once the pointer leaves the strip after a hand-back.
    armed: bool,
    along: i32,
    closed: bool,
    xkb_context: xkb::Context,
    xkb: Option<(xkb::State, xkb::State)>,
}

fn run(
    edge: Edge, emit: Box<dyn Fn(Captured) + Send>, control: &mpsc::Receiver<Control>, wake: &OwnedFd,
    ready: &mpsc::Sender<bool>,
) {
    let Some((connection, mut queue, mut strip)) = setup(edge, emit) else {
        let _ = ready.send(false);
        return;
    };
    // The compositor answers the first commit with a size; without it there is no strip.
    if queue.roundtrip(&mut strip).is_err() || strip.size == (0, 0) || strip.closed {
        let _ = ready.send(false);
        return;
    }
    let _ = ready.send(true);
    loop {
        let _ = connection.flush();
        if let Some(guard) = queue.prepare_read() {
            let mut fds = [
                libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            // SAFETY: poll on two valid descriptors we hold for the duration of the call.
            unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if fds[0].revents & libc::POLLIN != 0 {
                if guard.read().is_err() {
                    return;
                }
            } else {
                drop(guard);
            }
            let mut sink = [0u8; 64];
            // SAFETY: draining our own non-blocking pipe into a local buffer.
            while unsafe { libc::read(wake.as_raw_fd(), sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
        }
        if queue.dispatch_pending(&mut strip).is_err() || strip.closed {
            return;
        }
        while let Ok(message) = control.try_recv() {
            match message {
                Control::Release { along } => strip.end_capture(along),
                Control::Stop => return,
            }
        }
    }
}

type Setup = (Connection, wayland_client::EventQueue<Strip>, Strip);

fn setup(edge: Edge, emit: Box<dyn Fn(Captured) + Send>) -> Option<Setup> {
    let connection = Connection::connect_to_env().ok()?;
    let (globals, queue) = registry_queue_init::<Strip>(&connection).ok()?;
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 4..=6, ()).ok()?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).ok()?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 5..=9, ()).ok()?;
    let layer_shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).ok()?;
    let constraints: ZwpPointerConstraintsV1 = globals.bind(&qh, 1..=1, ()).ok()?;
    let relative_manager: ZwpRelativePointerManagerV1 = globals.bind(&qh, 1..=1, ()).ok()?;
    let inhibit_manager: Option<ZwpKeyboardShortcutsInhibitManagerV1> = globals.bind(&qh, 1..=1, ()).ok();

    let surface = compositor.create_surface(&qh, ());
    let layer = layer_shell.get_layer_surface(&surface, None, Layer::Overlay, "glide-phone-edge".into(), &qh, ());
    let anchor = match edge {
        Edge::Left => Anchor::Left | Anchor::Top | Anchor::Bottom,
        Edge::Right => Anchor::Right | Anchor::Top | Anchor::Bottom,
        Edge::Top => Anchor::Top | Anchor::Left | Anchor::Right,
        Edge::Bottom => Anchor::Bottom | Anchor::Left | Anchor::Right,
    };
    layer.set_anchor(anchor);
    if edge.vertical() {
        layer.set_size(1, 0);
    } else {
        layer.set_size(0, 1);
    }
    // Reach the very edge, also under panels that reserve space.
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    surface.commit();

    let strip = Strip {
        edge,
        emit,
        qh,
        shm,
        seat,
        surface,
        layer,
        constraints,
        relative_manager,
        inhibit_manager,
        pointer: None,
        keyboard: None,
        size: (0, 0),
        buffer: None,
        locked: None,
        relative: None,
        inhibitor: None,
        capturing: false,
        armed: true,
        along: 0,
        closed: false,
        xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
        xkb: None,
    };
    Some((connection, queue, strip))
}

impl Strip {
    fn start_capture(&mut self, serial: u32, along: i32) {
        let Some(pointer) = self.pointer.clone() else { return };
        self.locked =
            Some(self.constraints.lock_pointer(&self.surface, &pointer, None, Lifetime::Persistent, &self.qh, ()));
        self.relative = Some(self.relative_manager.get_relative_pointer(&pointer, &self.qh, ()));
        self.layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        self.inhibitor =
            self.inhibit_manager.as_ref().map(|m| m.inhibit_shortcuts(&self.surface, &self.seat, &self.qh, ()));
        self.surface.commit();
        pointer.set_cursor(serial, None, 0, 0);
        self.capturing = true;
        self.armed = false;
        self.along = along;
        (self.emit)(Captured::Entered { along });
    }

    fn end_capture(&mut self, along: i32) {
        if !self.capturing {
            return;
        }
        self.capturing = false;
        if let Some(locked) = self.locked.take() {
            // Put the cursor back where the phone let it go.
            let (w, h) = (f64::from(self.size.0.max(1)), f64::from(self.size.1.max(1)));
            let along = f64::from(along.max(0));
            let (x, y) = if self.edge.vertical() { (0.5, along.min(h - 1.0)) } else { (along.min(w - 1.0), 0.5) };
            locked.set_cursor_position_hint(x, y);
            self.surface.commit();
            locked.destroy();
        }
        if let Some(relative) = self.relative.take() {
            relative.destroy();
        }
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.destroy();
        }
        self.layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        self.surface.commit();
    }

    fn draw(&mut self) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let stride = w * 4;
        let len = (stride * h) as usize;
        // SAFETY: memfd_create with a static NUL-terminated name.
        let raw = unsafe { libc::memfd_create(c"glide-edge".as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return;
        }
        // SAFETY: `raw` is a fresh descriptor we own.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // A zero-filled file is fully transparent ARGB; it still receives input.
        if std::fs::File::from(fd.try_clone().expect("dup")).set_len(len as u64).is_err() {
            return;
        }
        let pool = self.shm.create_pool(fd.as_fd(), i32::try_from(len).unwrap_or(i32::MAX), &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            i32::try_from(w).unwrap_or(1),
            i32::try_from(h).unwrap_or(1),
            i32::try_from(stride).unwrap_or(4),
            wl_shm::Format::Argb8888,
            &self.qh,
            (),
        );
        pool.destroy();
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.surface.commit();
        if let Some(old) = self.buffer.replace(buffer) {
            old.destroy();
        }
    }

    fn key(&mut self, key: u32) {
        let Some((state, base)) = &self.xkb else { return };
        let code = xkb::Keycode::new(key + 8);
        let sym = state.key_get_one_sym(code).raw();
        let active = |name: &str| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
        let (ctrl, alt, shift, logo) = (
            active(xkb::MOD_NAME_CTRL),
            active(xkb::MOD_NAME_ALT),
            active(xkb::MOD_NAME_SHIFT),
            active(xkb::MOD_NAME_LOGO),
        );
        if ctrl && alt && sym == xkb::keysyms::KEY_Escape {
            // The way out that always works, even if the phone never answers.
            let along = self.along;
            self.end_capture(along);
            (self.emit)(Captured::TakenBack);
            return;
        }
        let event = Captured::Key {
            sym,
            text: state.key_get_utf8(code),
            base: base.key_get_utf8(code),
            ctrl,
            alt,
            shift,
            logo,
        };
        (self.emit)(event);
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Strip {
    fn event(
        _: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Strip: wl_compositor::WlCompositor);
delegate_noop!(Strip: ignore wl_shm::WlShm);
delegate_noop!(Strip: wl_shm_pool::WlShmPool);
delegate_noop!(Strip: ignore wl_buffer::WlBuffer);
delegate_noop!(Strip: ignore wl_surface::WlSurface);
delegate_noop!(Strip: ZwlrLayerShellV1);
delegate_noop!(Strip: ZwpPointerConstraintsV1);
delegate_noop!(Strip: ZwpRelativePointerManagerV1);
delegate_noop!(Strip: ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(Strip: ignore ZwpKeyboardShortcutsInhibitorV1);

impl Dispatch<wl_seat::WlSeat, ()> for Strip {
    fn event(
        strip: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, (): &(), _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) && strip.pointer.is_none() {
                strip.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) && strip.keyboard.is_none() {
                strip.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Strip {
    fn event(
        strip: &mut Self, layer: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, (): &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                let size = (width.max(1), height.max(1));
                if size == strip.size {
                    strip.surface.commit();
                } else {
                    strip.size = size;
                    strip.draw();
                }
            }
            zwlr_layer_surface_v1::Event::Closed => strip.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Strip {
    fn event(
        strip: &mut Self, pointer: &wl_pointer::WlPointer, event: wl_pointer::Event, (): &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } if surface == strip.surface => {
                if strip.capturing {
                    pointer.set_cursor(serial, None, 0, 0);
                } else if strip.armed {
                    #[allow(clippy::cast_possible_truncation)]
                    let along = if strip.edge.vertical() { surface_y } else { surface_x } as i32;
                    strip.start_capture(serial, along);
                }
            }
            wl_pointer::Event::Leave { .. } if !strip.capturing => strip.armed = true,
            wl_pointer::Event::Button { button, state: WEnum::Value(state), .. } if strip.capturing => {
                (strip.emit)(Captured::Button { code: button, pressed: state == wl_pointer::ButtonState::Pressed });
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(axis), value, .. } if strip.capturing => {
                let (dx, dy) = if axis == wl_pointer::Axis::HorizontalScroll { (value, 0.0) } else { (0.0, value) };
                (strip.emit)(Captured::Scroll { dx, dy });
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Strip {
    fn event(
        strip: &mut Self, _: &ZwpRelativePointerV1, event: zwp_relative_pointer_v1::Event, (): &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, .. } = event
            && strip.capturing
        {
            (strip.emit)(Captured::Motion { dx, dy });
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for Strip {
    fn event(
        strip: &mut Self, _: &ZwpLockedPointerV1, event: zwp_locked_pointer_v1::Event, (): &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The compositor ended the lock (another window took focus, ...): stop sending.
        if matches!(event, zwp_locked_pointer_v1::Event::Unlocked) && strip.capturing {
            let along = strip.along;
            strip.end_capture(along);
            (strip.emit)(Captured::TakenBack);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Strip {
    fn event(
        strip: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, (): &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                // SAFETY: the compositor's keymap fd, mapped read-only for `size` bytes.
                let keymap = unsafe {
                    xkb::Keymap::new_from_fd(
                        &strip.xkb_context,
                        fd,
                        size as usize,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                };
                if let Ok(Some(keymap)) = keymap {
                    strip.xkb = Some((xkb::State::new(&keymap), xkb::State::new(&keymap)));
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some((state, base)) = strip.xkb.as_mut() {
                    state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                    base.update_mask(0, 0, 0, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. }
                if strip.capturing =>
            {
                strip.key(key);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Puts the strip on the running compositor's right edge for a moment (don't touch that
    /// edge meanwhile: it would take the cursor).
    #[test]
    #[ignore = "talks to the running compositor; run by hand"]
    fn the_strip_appears_on_the_compositor() {
        let capture = EdgeCapture::start(Edge::Right, |event| eprintln!("{event:?}"));
        assert!(capture.is_some(), "layer-shell strip refused");
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    fn bodies(t: &mut Translator, events: &[Captured]) -> Vec<serde_json::Value> {
        events.iter().flat_map(|e| t.packets(e, Edge::Left)).map(|p| p.body).collect()
    }

    /// End to end on the running compositor with a virtual mouse: pushing the cursor into the
    /// right edge hands it over, motion then arrives relative, and a release gives it back.
    /// Moves the real cursor for about a second.
    #[test]
    #[ignore = "moves the real cursor; run by hand"]
    fn pushing_into_the_edge_takes_the_cursor_until_released() {
        use crate::input::{Action, RemoteInput};
        use std::time::Duration;
        let (tx, rx) = mpsc::channel();
        let capture = EdgeCapture::start(Edge::Right, move |event| {
            let _ = tx.send(event);
        })
        .expect("strip");
        let mouse = RemoteInput::start();
        mouse.send(vec![Action::Move(1, 0)]); // creates the device
        std::thread::sleep(Duration::from_millis(1500)); // the compositor picks it up
        for _ in 0..20 {
            mouse.send(vec![Action::Move(400, 0)]);
            std::thread::sleep(Duration::from_millis(20));
        }
        let entered = rx.recv_timeout(Duration::from_secs(3)).expect("the edge noticed the cursor");
        assert!(matches!(entered, Captured::Entered { .. }), "{entered:?}");
        while rx.try_recv().is_ok() {}
        mouse.send(vec![Action::Move(-30, 5)]);
        let moved = rx.recv_timeout(Duration::from_secs(2)).expect("relative motion while captured");
        assert!(matches!(moved, Captured::Motion { dx, .. } if dx < 0.0), "{moved:?}");
        capture.release(200);
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        mouse.send(vec![Action::Move(-300, 0)]);
        std::thread::sleep(Duration::from_millis(300));
        assert!(rx.try_recv().is_err(), "after the release the cursor is this computer's again");
    }

    #[test]
    fn crossing_the_edge_tells_the_phone_where_to_enter() {
        let packets = Translator::default().packets(&Captured::Entered { along: 300 }, Edge::Left);
        assert_eq!(packets[0].packet_type, PACKET_TYPE_SHAREINPUTDEVICES_REQUEST);
        assert_eq!(packets[0].body, json!({ "exitEdge": 2, "deltax": 0, "deltay": 300 }));
        let top = Translator::default().packets(&Captured::Entered { along: 50 }, Edge::Top);
        assert_eq!(top[0].body, json!({ "exitEdge": 1, "deltax": 50, "deltay": 0 }));
        assert_eq!(Edge::parse("right"), Some(Edge::Right));
        assert_eq!(Edge::Bottom.kde(), 8);
    }

    #[test]
    fn clicks_drags_and_scrolls_become_mousepad_requests() {
        let mut t = Translator::default();
        let press = Captured::Button { code: BTN_LEFT, pressed: true };
        let release = Captured::Button { code: BTN_LEFT, pressed: false };
        assert_eq!(bodies(&mut t, &[press.clone(), release.clone()]), [json!({ "singleclick": true })]);
        let drag = bodies(&mut t, &[press, Captured::Motion { dx: 3.0, dy: 1.5 }, release]);
        assert_eq!(
            drag,
            [json!({ "singlehold": true }), json!({ "dx": 3.0, "dy": 1.5 }), json!({ "singlerelease": true })]
        );
        assert_eq!(
            bodies(
                &mut t,
                &[Captured::Button { code: BTN_RIGHT, pressed: true }, Captured::Scroll { dx: 0.0, dy: 10.0 }]
            ),
            [json!({ "rightclick": true }), json!({ "scroll": true, "dx": 0.0, "dy": 10.0 })]
        );
    }

    #[test]
    fn keys_become_text_special_keys_or_shortcuts() {
        let mut t = Translator::default();
        let key = |sym, text: &str, base: &str, ctrl| Captured::Key {
            sym,
            text: text.into(),
            base: base.into(),
            ctrl,
            alt: false,
            shift: false,
            logo: false,
        };
        let sent = bodies(
            &mut t,
            &[
                key(0x6ef, "ї", "ї", false),                      // Cyrillic text as typed
                key(xkb::keysyms::KEY_Return, "\r", "\r", false), // Enter
                key(xkb::keysyms::KEY_F5, "", "", false),
                key(0x63, "\u{3}", "c", true),                 // Ctrl+C
                key(xkb::keysyms::KEY_Shift_L, "", "", false), // a modifier alone: nothing
            ],
        );
        assert_eq!(
            sent,
            [
                json!({ "key": "ї" }),
                json!({ "specialKey": 12 }),
                json!({ "specialKey": 25 }),
                json!({ "key": "c", "ctrl": true }),
            ]
        );
    }
}
