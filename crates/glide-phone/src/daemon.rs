#![allow(clippy::unused_self)] // zbus properties and methods require &self

use std::sync::Arc;
use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::{connection, interface};

use crate::protocol::pair::PairState;
use crate::state::PhoneManager;

pub const BUS_NAME: &str = "org.glide.Phone";
pub const OBJECT_PATH: &str = "/org/glide/Phone";

pub struct PhoneDaemon {
    manager: Arc<Mutex<PhoneManager>>,
}

impl PhoneDaemon {
    #[must_use]
    pub fn new(manager: Arc<Mutex<PhoneManager>>) -> Self {
        Self { manager }
    }
}

#[interface(name = "org.glide.Phone")]
impl PhoneDaemon {
    pub async fn list_devices_json(&self) -> String {
        let mgr = self.manager.lock().await;
        let cards = mgr.sorted_cards();
        serde_json::to_string(&cards).unwrap_or_else(|_| "[]".to_string())
    }

    pub async fn ring_phone(&self, device_id: &str) -> bool {
        let mgr = self.manager.lock().await;
        if device_id.is_empty() {
            if let Some(dev) = mgr.devices.values().find(|d| d.pair_state == PairState::Paired && d.connected) {
                return mgr.ring_phone(&dev.id).is_ok();
            }
            if let Some(dev) = mgr.devices.values().find(|d| d.pair_state == PairState::Paired) {
                return mgr.ring_phone(&dev.id).is_ok();
            }
            return false;
        }
        mgr.ring_phone(device_id).is_ok()
    }

    pub async fn stop_alarm(&self, device_id: &str) -> bool {
        let mut mgr = self.manager.lock().await;
        if device_id.is_empty() {
            let ids: Vec<String> = mgr.devices.keys().cloned().collect();
            for id in ids {
                mgr.stop_alarm(&id);
            }
        } else {
            mgr.stop_alarm(device_id);
        }
        true
    }

    pub async fn accept_pair(&self, device_id: &str) -> bool {
        let mut mgr = self.manager.lock().await;
        mgr.accept_pair(device_id, String::new()).is_ok()
    }

    pub async fn reject_pair(&self, device_id: &str) -> bool {
        let mut mgr = self.manager.lock().await;
        mgr.reject_pair(device_id).is_ok()
    }

    pub async fn dismiss_notification(&self, device_id: &str, notif_id: &str) -> bool {
        let mut mgr = self.manager.lock().await;
        mgr.dismiss_notification(device_id, notif_id).is_some()
    }

    pub async fn clear_notifications(&self, device_id: &str) -> bool {
        let mut mgr = self.manager.lock().await;
        if device_id.is_empty() {
            let ids: Vec<String> = mgr.devices.keys().cloned().collect();
            let mut cleared = 0;
            for id in ids {
                cleared += mgr.clear_notifications(&id);
            }
            return cleared > 0;
        }
        mgr.clear_notifications(device_id) > 0
    }

    pub async fn set_clipboard(&self, device_id: &str, content: &str) -> bool {
        let mut mgr = self.manager.lock().await;
        mgr.set_clipboard(device_id, content).is_ok()
    }

    pub async fn clipboard_text(&self) -> String {
        let mgr = self.manager.lock().await;
        mgr.clipboard_text().unwrap_or_default()
    }

    pub async fn share_text(&self, device_id: &str, text: &str) -> bool {
        let mgr = self.manager.lock().await;
        mgr.share_text(device_id, text).is_ok()
    }

    pub async fn share_url(&self, device_id: &str, url: &str) -> bool {
        let mgr = self.manager.lock().await;
        mgr.share_url(device_id, url).is_ok()
    }

    /// Sends local files to the phone; returns false if it is not reachable.
    pub async fn share_files(&self, device_id: &str, paths: Vec<String>) -> bool {
        let mgr = self.manager.lock().await;
        mgr.share_files(device_id, paths.into_iter().map(std::path::PathBuf::from).collect()).is_ok()
    }

    pub async fn list_commands_json(&self) -> String {
        let mgr = self.manager.lock().await;
        serde_json::to_string(&mgr.commands.entries).unwrap_or_else(|_| "{}".to_string())
    }

    pub async fn add_command(&self, name: &str, command: &str) -> bool {
        self.manager.lock().await.add_command(name, command).is_some()
    }

    pub async fn remove_command(&self, key: &str) -> bool {
        self.manager.lock().await.remove_command(key)
    }

    /// True once after a phone asked to set commands up here.
    pub async fn take_commands_setup(&self) -> bool {
        std::mem::take(&mut self.manager.lock().await.commands_setup_requested)
    }

    /// The phones' media players: `{device id: {player: state}}`.
    pub async fn list_phone_media_json(&self) -> String {
        serde_json::to_string(&self.manager.lock().await.phone_media).unwrap_or_else(|_| "{}".to_string())
    }

    /// Sends `action` (`Play`, `Pause`, `PlayPause`, `Next`, `Previous`, `Stop`) to a phone's player.
    pub async fn phone_media_action(&self, device_id: &str, player: &str, action: &str) -> bool {
        matches!(action, "Play" | "Pause" | "PlayPause" | "Next" | "Previous" | "Stop")
            && self.manager.lock().await.control_phone_media(device_id, player, serde_json::json!({ "action": action }))
    }

    /// Opens the phone's files (mounted over SFTP).
    pub async fn browse_phone(&self, device_id: &str) -> bool {
        self.manager.lock().await.browse_phone(device_id).is_ok()
    }

    /// Sends a `kdeconnect.mousepad.request` body (JSON) to drive the phone.
    pub async fn control_phone_input(&self, device_id: &str, body_json: &str) -> bool {
        let Ok(body) = serde_json::from_str(body_json) else { return false };
        self.manager.lock().await.control_phone_input(device_id, body).is_ok()
    }

    pub async fn ping(&self, device_id: &str) -> bool {
        self.manager.lock().await.ping(device_id).is_ok()
    }

    /// The screen edge where the cursor goes over to the phone ("" when off).
    pub async fn share_input_edge(&self) -> String {
        self.manager.lock().await.share_input_edge().map_or_else(String::new, |e| e.name().to_string())
    }

    /// "left", "right", "top", "bottom", or "" to turn it off; false if the strip can't be shown.
    pub async fn set_share_input_edge(&self, edge: &str) -> bool {
        let parsed = crate::edge::Edge::parse(edge);
        if parsed.is_none() && !edge.is_empty() {
            return false;
        }
        self.manager.lock().await.set_share_input_edge(parsed)
    }

    /// The phones' messages: `{device id: inbox}`.
    pub async fn list_sms_json(&self) -> String {
        let mgr = self.manager.lock().await;
        let recent: std::collections::HashMap<&String, crate::plugins::sms::Inbox> =
            mgr.sms.iter().map(|(device, inbox)| (device, inbox.recent(20))).collect();
        serde_json::to_string(&recent).unwrap_or_else(|_| "{}".to_string())
    }

    pub async fn request_sms_conversations(&self, device_id: &str) -> bool {
        self.manager.lock().await.request_sms_conversations(device_id).is_ok()
    }

    pub async fn request_sms_thread(&self, device_id: &str, thread: i64) -> bool {
        self.manager.lock().await.request_sms_thread(device_id, thread).is_ok()
    }

    pub async fn send_sms(&self, device_id: &str, addresses: Vec<String>, text: &str) -> bool {
        self.manager.lock().await.send_sms(device_id, &addresses, text).is_ok()
    }

    pub async fn take_presenter_event(&self) -> String {
        let mut mgr = self.manager.lock().await;
        mgr.take_presenter_event().and_then(|event| serde_json::to_string(&event).ok()).unwrap_or_default()
    }

    pub async fn is_ringing(&self) -> bool {
        let mgr = self.manager.lock().await;
        mgr.is_ringing()
    }

    #[zbus(signal)]
    pub async fn state_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

pub async fn start_dbus_service(
    manager: Arc<Mutex<PhoneManager>>,
) -> Result<connection::Connection, Box<dyn std::error::Error + Send + Sync>> {
    start_dbus_service_on(connection::Builder::session()?, manager).await
}

/// [`start_dbus_service`] on any bus. Fails when another daemon already owns the name: zbus
/// reports a taken name as `Ok(Exists)`, not as an error, so it is checked here.
pub async fn start_dbus_service_on(
    builder: connection::Builder<'_>, manager: Arc<Mutex<PhoneManager>>,
) -> Result<connection::Connection, Box<dyn std::error::Error + Send + Sync>> {
    let daemon = PhoneDaemon::new(manager.clone());

    let conn = builder.serve_at(OBJECT_PATH, daemon)?.build().await?;
    let reply = conn.request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into()).await?;
    if !matches!(reply, zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner) {
        return Err(format!("{BUS_NAME} is already owned ({reply:?})").into());
    }

    let notify = {
        let mgr = manager.lock().await;
        mgr.notify.clone()
    };

    let conn_clone = conn.clone();
    tokio::spawn(async move {
        // Register for the next change before emitting, so a change that happens while the
        // signal is going out (a battery update right after a connect) isn't lost.
        let mut changed = Box::pin(notify.notified());
        changed.as_mut().enable();
        loop {
            changed.as_mut().await;
            changed = Box::pin(notify.notified());
            changed.as_mut().enable();
            let _ = conn_clone.emit_signal(None::<()>, OBJECT_PATH, BUS_NAME, "StateChanged", &()).await;
        }
    });

    Ok(conn)
}
