//! `kdeconnect.digitizer`: the phone as a drawing tablet (KDE Connect's digitizer plugin),
//! one virtual tablet per drawing session.

use std::sync::mpsc;

use serde_json::Value as Json;

use crate::input::{PenEvent, Tablet};

pub const PACKET_TYPE_DIGITIZER_SESSION: &str = "kdeconnect.digitizer.session";
pub const PACKET_TYPE_DIGITIZER: &str = "kdeconnect.digitizer";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Start { width: i32, height: i32, resolution_x: i32, resolution_y: i32 },
    End,
}

#[must_use]
pub fn parse_session(body: &Json) -> Option<Session> {
    let int = |key: &str| body.get(key).and_then(Json::as_i64).and_then(|v| i32::try_from(v).ok());
    match body.get("action").and_then(Json::as_str)? {
        "start" => Some(Session::Start {
            width: int("width")?.clamp(1, 1 << 16),
            height: int("height")?.clamp(1, 1 << 16),
            resolution_x: int("resolutionX").unwrap_or(1).max(1),
            resolution_y: int("resolutionY").unwrap_or(1).max(1),
        }),
        "end" => Some(Session::End),
        _ => None,
    }
}

#[must_use]
pub fn parse_event(body: &Json) -> PenEvent {
    let int = |key: &str| body.get(key).and_then(Json::as_i64).and_then(|v| i32::try_from(v).ok());
    PenEvent {
        active: body.get("active").and_then(Json::as_bool),
        touching: body.get("touching").and_then(Json::as_bool),
        rubber: body.get("tool").and_then(Json::as_str).map(|tool| tool == "Rubber"),
        x: int("x"),
        y: int("y"),
        pressure: body.get("pressure").and_then(Json::as_f64).filter(|p| p.is_finite()),
    }
}

enum Command {
    Session(Session),
    Pen(PenEvent),
}

/// Owns the tablets on a thread of its own (writes to /dev/uinput block).
pub struct Digitizer {
    queue: Option<mpsc::SyncSender<Command>>,
}

impl Digitizer {
    #[must_use]
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Command>(512);
        let spawned = std::thread::Builder::new().name("glide-phone-tablet".into()).spawn(move || {
            let mut tablet: Option<Tablet> = None;
            while let Ok(command) = receiver.recv() {
                match command {
                    Command::Session(Session::Start { width, height, resolution_x, resolution_y }) => {
                        tablet = None; // a new session replaces the old tablet
                        match Tablet::open(width, height, resolution_x, resolution_y) {
                            Ok(created) => tablet = Some(created),
                            Err(err) => tracing::warn!(%err, "cannot create the virtual drawing tablet"),
                        }
                    }
                    Command::Session(Session::End) => tablet = None,
                    Command::Pen(event) => {
                        if let Some(t) = tablet.as_mut()
                            && t.report(event).is_err()
                        {
                            tablet = None;
                        }
                    }
                }
            }
        });
        Self { queue: spawned.ok().map(|_| sender) }
    }

    pub fn session(&self, session: Session) {
        if let Some(queue) = &self.queue {
            let _ = queue.try_send(Command::Session(session)); // never block the runtime thread
        }
    }

    pub fn pen(&self, event: PenEvent) {
        if let Some(queue) = &self.queue {
            let _ = queue.try_send(Command::Pen(event));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sessions_and_pen_reports_read_like_kde_connect() {
        assert_eq!(
            parse_session(
                &json!({"action": "start", "width": 1080, "height": 2000, "resolutionX": 16, "resolutionY": 16})
            ),
            Some(Session::Start { width: 1080, height: 2000, resolution_x: 16, resolution_y: 16 })
        );
        assert_eq!(parse_session(&json!({"action": "end"})), Some(Session::End));
        let pen =
            parse_event(&json!({"active": true, "touching": true, "tool": "Rubber", "x": 5, "y": 6, "pressure": 0.5}));
        assert_eq!(pen.rubber, Some(true));
        assert_eq!((pen.x, pen.y, pen.pressure), (Some(5), Some(6), Some(0.5)));
        assert_eq!(parse_event(&json!({"active": false})), PenEvent { active: Some(false), ..PenEvent::default() });
    }
}
