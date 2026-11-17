use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedSender, WeakUnboundedSender};

use crate::config::{PairedDevice, PhoneConfig};
use crate::crypto::cert::IdentityCert;
use crate::plugins::battery::{BatteryState, handle_battery_packet};
use crate::plugins::clipboard::{create_clipboard_packet, handle_clipboard_packet};
use crate::plugins::find_device::handle_find_this_device;
use crate::plugins::notifications::{
    ActiveNotification, NotificationEvent, create_action_packet, create_dismiss_packet, create_reply_packet,
    create_request_packet, handle_notification_packet_with_payload,
};
use crate::plugins::presenter::{PresenterEvent, handle_presenter_packet};
use crate::plugins::share::{create_share_text_packet, create_share_url_packet, handle_share_packet};
use crate::protocol::identity::DeviceType;
use crate::protocol::packet::{
    NetworkPacket, PACKET_TYPE_BATTERY, PACKET_TYPE_CLIPBOARD, PACKET_TYPE_FINDMYPHONE_REQUEST,
    PACKET_TYPE_NOTIFICATION, PACKET_TYPE_PAIR, PACKET_TYPE_PING, PACKET_TYPE_PRESENTER, PACKET_TYPE_SHARE_REQUEST,
};
use crate::protocol::pair::{PairBody, PairState};
use crate::sound::AlarmHandle;
use crate::ui::PhoneCardData;

pub type PacketSender = UnboundedSender<NetworkPacket>;

const CONNECTING_COOLDOWN: Duration = Duration::from_secs(5);
pub const MAX_KNOWN_DEVICES: usize = 64;
pub const MAX_DEVICE_NAME_CHARS: usize = 64;
const MAX_DEVICE_ID_BYTES: usize = 128;

fn usable_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_DEVICE_ID_BYTES && !id.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn presentable(name: &str) -> String {
    let visible: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let name: String =
        visible.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_DEVICE_NAME_CHARS).collect();
    if name.is_empty() { "Device".to_string() } else { name }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneDeviceState {
    pub id: String,
    pub name: String,
    pub device_type: DeviceType,
    pub pair_state: PairState,
    pub battery: Option<BatteryState>,
    pub notifications: HashMap<String, ActiveNotification>,
    pub seen_notifications: HashMap<String, (String, String)>,
    pub peer_fingerprint: Option<String>,
    pub connected: bool,
    pub signals: Vec<crate::plugins::connectivity::Signal>,
    /// The phone said its keyboard can't type right now (`mousepad.keyboardstate`).
    pub keyboard_off: bool,
}

pub struct PhoneManager {
    pub config: PhoneConfig,
    pub cert: IdentityCert,
    pub devices: HashMap<String, PhoneDeviceState>,
    alarms: HashMap<String, AlarmHandle>,
    senders: HashMap<String, PacketSender>,
    connecting: HashMap<String, Instant>,
    clipboard: Option<(String, i64)>,
    presenter_events: VecDeque<(String, PresenterEvent)>,
    pub commands: crate::plugins::runcommand::Commands,
    /// The virtual mouse/keyboard the phones drive (created on first use). Only the daemon
    /// turns it on ([`PhoneManager::enable_remote_input`]); tests never touch real input.
    input: Option<crate::input::RemoteInput>,
    remote_input: bool,
    mousepad: crate::plugins::mousepad::Mousepad,
    typist: Option<crate::plugins::mousepad::Typist>,
    digitizer: Option<crate::plugins::digitizer::Digitizer>,
    /// When we asked each phone for its files; its login is only used shortly after.
    browse_requested: HashMap<String, Instant>,
    /// Where each phone was last connected from (for unicast rediscovery).
    last_addresses: HashMap<String, std::net::IpAddr>,
    /// The phones' text messages, by device.
    pub sms: HashMap<String, crate::plugins::sms::Inbox>,
    /// The phones' media players, by device.
    pub phone_media: HashMap<String, std::collections::BTreeMap<String, crate::plugins::remote_media::RemotePlayer>>,
    /// Set when a phone asked to set commands up here; the window opens that section.
    pub commands_setup_requested: bool,
    pub notify: Arc<Notify>,
}

impl PhoneManager {
    #[must_use]
    pub fn new(mut config: PhoneConfig, cert: IdentityCert) -> Self {
        config.paired_devices.retain(|id, paired| {
            let pinned = !paired.fingerprint.is_empty();
            if !pinned {
                tracing::warn!(%id, "a paired phone was saved without its certificate; it has to be paired again");
            }
            pinned
        });
        let commands_path = config.config_path.as_ref().map(|path| path.with_file_name("phone-commands.json"));
        let mut devices = HashMap::new();
        for (id, paired) in &config.paired_devices {
            devices.insert(
                id.clone(),
                PhoneDeviceState {
                    id: id.clone(),
                    name: paired.name.clone(),
                    device_type: DeviceType::Phone,
                    pair_state: PairState::Paired,
                    battery: None,
                    notifications: HashMap::new(),
                    seen_notifications: HashMap::new(),
                    peer_fingerprint: Some(paired.fingerprint.clone()),
                    connected: false,
                    signals: Vec::new(),
                    keyboard_off: false,
                },
            );
        }
        Self {
            config,
            cert,
            devices,
            alarms: HashMap::new(),
            senders: HashMap::new(),
            connecting: HashMap::new(),
            clipboard: None,
            presenter_events: VecDeque::new(),
            commands: crate::plugins::runcommand::Commands::load(commands_path),
            commands_setup_requested: false,
            phone_media: HashMap::new(),
            sms: HashMap::new(),
            last_addresses: HashMap::new(),
            browse_requested: HashMap::new(),
            input: None,
            remote_input: false,
            mousepad: crate::plugins::mousepad::Mousepad::default(),
            typist: None,
            digitizer: None,
            notify: Arc::new(Notify::new()),
        }
    }

    #[must_use]
    pub fn is_connected(&self, device_id: &str) -> bool {
        self.senders.contains_key(device_id)
    }

    #[must_use]
    pub fn has_any_connection(&self) -> bool {
        !self.senders.is_empty()
    }

    #[must_use]
    pub fn is_connecting(&self, device_id: &str) -> bool {
        self.connecting.get(device_id).is_some_and(|since| since.elapsed() < CONNECTING_COOLDOWN)
    }

    pub fn mark_connecting(&mut self, device_id: &str) {
        self.connecting.insert(device_id.to_string(), Instant::now());
    }

    pub fn clear_connecting(&mut self, device_id: &str) {
        self.connecting.remove(device_id);
    }

    pub fn register_discovered(&mut self, id: String, name: &str, device_type: DeviceType) -> bool {
        if !usable_id(&id) {
            return false;
        }
        let name = presentable(name);
        let paired_fp = self.config.paired_devices.get(&id).map(|p| p.fingerprint.clone());
        let is_paired = paired_fp.is_some();
        if !is_paired && !self.devices.contains_key(&id) && self.devices.len() >= MAX_KNOWN_DEVICES {
            return false;
        }
        let pair_state = if is_paired { PairState::Paired } else { PairState::Unpaired };

        let entry = self.devices.entry(id.clone()).or_insert_with(|| PhoneDeviceState {
            id,
            name: name.clone(),
            device_type,
            pair_state,
            battery: None,
            notifications: HashMap::new(),
            seen_notifications: HashMap::new(),
            peer_fingerprint: paired_fp.filter(|s| !s.is_empty()),
            connected: false,
            signals: Vec::new(),
            keyboard_off: false,
        });
        let changed = entry.name != name
            || entry.device_type != device_type
            || (is_paired && entry.pair_state != PairState::Paired);
        entry.name = name;
        entry.device_type = device_type;
        if is_paired {
            entry.pair_state = PairState::Paired;
        }
        if changed {
            self.notify.notify_waiters();
        }
        true
    }

    pub fn register_connection(&mut self, device_id: String, sender: PacketSender, peer_fingerprint: Option<String>) {
        self.connecting.remove(&device_id);
        if let Some(dev) = self.devices.get_mut(&device_id) {
            dev.connected = true;
            if let Some(fp) = peer_fingerprint.filter(|s| !s.is_empty()) {
                let swapped = dev.peer_fingerprint.as_deref().is_some_and(|known| known != fp);
                if swapped && matches!(dev.pair_state, PairState::RequestedByLocal | PairState::RequestedByPeer) {
                    tracing::warn!(%device_id, "the phone's certificate changed while pairing; pairing starts over");
                    dev.pair_state = PairState::Unpaired;
                }
                dev.peer_fingerprint = Some(fp);
            }
        }
        if self.is_paired(&device_id) {
            for packet in self.greeting() {
                let _ = sender.send(packet);
            }
            Self::greet_async(sender.clone());
        }
        self.senders.insert(device_id, sender);
        self.notify.notify_waiters();
    }

    pub fn remember_address(&mut self, device_id: &str, ip: std::net::IpAddr) {
        // ::ffff:a.b.c.d from a dual-stack socket is the plain IPv4 address.
        self.last_addresses.insert(device_id.to_string(), ip.to_canonical());
    }

    /// Last addresses of paired phones that are not connected now.
    #[must_use]
    pub fn away_paired_addresses(&self) -> Vec<std::net::IpAddr> {
        self.last_addresses
            .iter()
            .filter(|(id, _)| self.is_paired(id) && !self.is_connected(id))
            .map(|(_, ip)| *ip)
            .collect()
    }

    /// Lets phones drive this computer's pointer and keyboard when `/dev/uinput` is usable.
    pub fn enable_remote_input(&mut self) {
        self.remote_input = crate::input::RemoteInput::available();
        if !self.remote_input {
            tracing::info!("/dev/uinput is not writable; phones can only send presenter keys");
        }
    }

    /// What our plugins send when a paired phone connects (their `onCreate` in KDE Connect).
    fn greeting(&self) -> Vec<NetworkPacket> {
        let mut packets = vec![
            self.commands.list_packet(),
            crate::plugins::mousepad::keyboard_state_packet(self.remote_input),
            crate::plugins::contacts::request_uids_packet(),
            crate::plugins::wol::packet(&crate::plugins::wol::local_macs()),
        ];
        // Only paired phones are greeted, so the battery never reaches a stranger.
        packets.extend(crate::plugins::battery::local_battery_packet());
        if let Some((content, timestamp)) = &self.clipboard
            && *timestamp > 0
        {
            packets.push(crate::plugins::clipboard::create_connect_packet(content, *timestamp));
        }
        packets
    }

    /// This computer's clipboard changed: remember it and send it to connected phones.
    pub fn local_clipboard_changed(&mut self, content: String) {
        if self.clipboard.as_ref().is_some_and(|(current, _)| *current == content) {
            return; // what we just applied from the phone, or no real change
        }
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        let packet = create_clipboard_packet(&content, timestamp);
        self.clipboard = Some((content, timestamp));
        for (id, sender) in &self.senders {
            if self.is_paired(id) {
                let _ = sender.send(packet.clone());
            }
        }
        self.notify.notify_waiters();
    }

    pub fn greet(&self, device_id: &str) {
        for packet in self.greeting() {
            self.send_to_device(device_id, packet);
        }
        if let Some(sender) = self.senders.get(device_id) {
            Self::greet_async(sender.clone());
        }
    }

    /// The part of the greeting that needs the session bus (the player list).
    fn greet_async(sender: PacketSender) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        tokio::spawn(async move {
            if let Some(conn) = crate::plugins::mpris::session().await {
                let names = crate::plugins::mpris::refresh(&conn).await;
                let _ = sender.send(crate::plugins::mpris::list_packet(&names));
            }
            let sinks = crate::plugins::systemvolume::sinks().await;
            if !sinks.is_empty() {
                let _ = sender.send(crate::plugins::systemvolume::sink_list_packet(&sinks));
            }
        });
    }

    /// A desktop notification for the phones; with a PNG icon it goes as a payload.
    pub fn forward_desktop_notification(&self, notification: &crate::plugins::send_notifications::DesktopNotification) {
        let packet = crate::plugins::send_notifications::packet(notification);
        let icon = notification.icon.clone().filter(|_| tokio::runtime::Handle::try_current().is_ok());
        for id in self.senders.keys().filter(|id| self.is_paired(id)) {
            let Some(sender) = self.senders.get(id).cloned() else { continue };
            let Some(path) = icon.clone() else {
                let _ = sender.send(packet.clone());
                continue;
            };
            let (cert, fingerprint) =
                (self.cert.clone(), self.devices.get(id).and_then(|d| d.peer_fingerprint.clone()));
            let mut packet = packet.clone();
            tokio::spawn(async move {
                let bytes = tokio::fs::read(&path).await.ok().filter(|b| b.len() <= 1024 * 1024);
                let listener = match &bytes {
                    Some(_) => crate::payload::PayloadListener::bind(&cert, fingerprint).await.ok(),
                    None => None,
                };
                if let (Some(bytes), Some(listener)) = (bytes, listener) {
                    crate::payload::attach(&mut packet, bytes.len() as u64, listener.port());
                    if sender.send(packet).is_ok() {
                        listener
                            .serve(
                                std::io::Cursor::new(bytes.clone()),
                                bytes.len() as u64,
                                crate::payload::ACCEPT_TIMEOUT,
                            )
                            .await;
                    }
                } else {
                    let _ = sender.send(packet);
                }
            });
        }
    }

    /// Drives the phone's pointer or keyboard (`kdeconnect.mousepad.request` to the phone).
    pub fn control_phone_input(
        &self, device_id: &str, body: serde_json::Value,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        if !body.is_object() {
            return Err("not a request".into());
        }
        self.send_to_device(
            device_id,
            NetworkPacket::new(crate::plugins::mousepad::PACKET_TYPE_MOUSEPAD_REQUEST, body),
        );
        Ok(())
    }

    pub fn ping(&self, device_id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        self.send_to_device(device_id, NetworkPacket::new(PACKET_TYPE_PING, serde_json::json!({})));
        Ok(())
    }

    /// Asks the phone for its files (they open once it answers).
    pub fn browse_phone(&mut self, device_id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        self.browse_requested.insert(device_id.to_string(), Instant::now());
        self.send_to_device(device_id, crate::plugins::sftp::start_browsing_packet());
        Ok(())
    }

    /// Sends `packet` to every connected, paired phone.
    pub fn broadcast(&self, packet: &NetworkPacket) {
        for (id, sender) in &self.senders {
            if self.is_paired(id) {
                let _ = sender.send(packet.clone());
            }
        }
    }

    /// Commands changed here: every connected phone gets the new list.
    fn broadcast_commands(&self) {
        let packet = self.commands.list_packet();
        for (id, sender) in &self.senders {
            if self.is_paired(id) {
                let _ = sender.send(packet.clone());
            }
        }
    }

    pub fn add_command(&mut self, name: &str, command: &str) -> Option<String> {
        if name.trim().is_empty() || command.trim().is_empty() {
            return None;
        }
        let key = self.commands.add(name, command);
        self.broadcast_commands();
        self.notify.notify_waiters();
        Some(key)
    }

    pub fn remove_command(&mut self, key: &str) -> bool {
        let removed = self.commands.remove(key);
        if removed {
            self.broadcast_commands();
            self.notify.notify_waiters();
        }
        removed
    }

    #[must_use]
    pub fn presents(&self, device_id: &str, fingerprint: Option<&str>) -> bool {
        let known = self.devices.get(device_id).and_then(|device| device.peer_fingerprint.as_deref());
        known.is_some() && known == fingerprint
    }

    pub fn unregister_connection(&mut self, device_id: &str, sender: &WeakUnboundedSender<NetworkPacket>) {
        if !sender
            .upgrade()
            .is_some_and(|sender| self.senders.get(device_id).is_some_and(|current| current.same_channel(&sender)))
        {
            return;
        }
        self.senders.remove(device_id);
        self.connecting.remove(device_id);
        self.phone_media.remove(device_id);
        if tokio::runtime::Handle::try_current().is_ok() {
            let id = device_id.to_string();
            tokio::spawn(async move { crate::plugins::sftp::unmount(&id).await });
        }
        self.presenter_events.retain(|(id, _)| id != device_id);
        if let Some(dev) = self.devices.get_mut(device_id) {
            dev.connected = false;
            dev.signals.clear();
            if dev.pair_state == PairState::RequestedByPeer {
                dev.pair_state = PairState::Unpaired;
            }
        }
        self.notify.notify_waiters();
    }

    #[must_use]
    pub fn sender_for(&self, device_id: &str) -> Option<PacketSender> {
        self.is_paired(device_id).then(|| self.senders.get(device_id).cloned()).flatten()
    }

    pub fn send_to_device(&self, device_id: &str, packet: NetworkPacket) {
        if let Some(sender) = self.senders.get(device_id) {
            let _ = sender.send(packet);
        }
    }

    pub fn handle_packet(
        &mut self, device_id: &str, packet: &NetworkPacket,
    ) -> Result<Option<NetworkPacket>, Box<dyn std::error::Error + Send + Sync>> {
        self.handle_packet_with_payload(device_id, packet, None)
    }

    pub fn handle_packet_with_payload(
        &mut self, device_id: &str, packet: &NetworkPacket, payload_png: Option<&[u8]>,
    ) -> Result<Option<NetworkPacket>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(device) = self.devices.get(device_id) else {
            return Ok(None);
        };
        let device_name = device.name.clone();

        if device.pair_state != PairState::Paired && packet.packet_type != PACKET_TYPE_PAIR {
            return Ok(None);
        }

        if self.handle_aux_packet(device_id, packet, &device_name, payload_png) {
            return Ok(None);
        }

        let Some(device) = self.devices.get_mut(device_id) else {
            return Ok(None);
        };

        match packet.packet_type.as_str() {
            PACKET_TYPE_BATTERY => {
                if let Ok(battery_state) = handle_battery_packet(packet)
                    && device.battery != Some(battery_state)
                {
                    if crate::plugins::battery::turned_low(device.battery, battery_state) {
                        crate::plugins::battery::announce_low(&device_name, battery_state.charge);
                    }
                    if crate::plugins::battery::turned_full(device.battery, battery_state) {
                        crate::plugins::battery::announce_full(&device_name);
                    }
                    device.battery = Some(battery_state);
                    self.notify.notify_waiters();
                }
            }
            PACKET_TYPE_NOTIFICATION => {
                if let Ok(Some(event)) = handle_notification_packet_with_payload(
                    &device_name,
                    packet,
                    payload_png,
                    &mut device.notifications,
                    &mut device.seen_notifications,
                ) {
                    match event {
                        NotificationEvent::Added(notification) => {
                            device.notifications.insert(notification.id.clone(), notification);
                        }
                        NotificationEvent::Dismissed(id) => {
                            device.notifications.remove(&id);
                        }
                    }
                    self.notify.notify_waiters();
                }
            }
            PACKET_TYPE_FINDMYPHONE_REQUEST => {
                if let Some(ringing) = self.alarms.remove(device_id).filter(AlarmHandle::is_active) {
                    ringing.stop();
                } else {
                    self.alarms.insert(device_id.to_string(), handle_find_this_device(&device_name));
                }
                self.notify.notify_waiters();
            }
            PACKET_TYPE_PING => {
                let message = packet.body.get("message").and_then(serde_json::Value::as_str);
                let summary = crate::i18n::naming(crate::i18n::Key::Ping, &device_name);
                crate::plugins::announce(
                    glide_sdk::system::Notification::new("glide-phone", summary)
                        .body(message.unwrap_or_else(|| crate::i18n::t(crate::i18n::Key::PingBody)))
                        .icon("phone-symbolic"),
                );
            }
            PACKET_TYPE_PAIR => self.handle_pair_packet(device_id, device_name, packet),
            _ => {}
        }

        Ok(None)
    }

    fn handle_pair_packet(&mut self, device_id: &str, device_name: String, packet: &NetworkPacket) {
        let (Ok(pair), Some(device)) = (PairBody::from_packet(packet), self.devices.get_mut(device_id)) else {
            return;
        };
        if !pair.pair {
            device.pair_state = PairState::Unpaired;
            device.notifications.clear();
            device.seen_notifications.clear();
            self.presenter_events.retain(|(id, _)| id != device_id);
            self.config.paired_devices.remove(device_id);
            self.config.save_if_configured();
        } else if device.pair_state == PairState::RequestedByLocal {
            let Some(fingerprint) = device.peer_fingerprint.clone().filter(|fingerprint| !fingerprint.is_empty())
            else {
                tracing::warn!(%device_id, "a phone accepted pairing before showing a certificate");
                return;
            };
            device.pair_state = PairState::Paired;
            let paired = PairedDevice { id: device_id.to_string(), name: device_name, fingerprint };
            self.config.paired_devices.insert(device_id.to_string(), paired);
            self.config.save_if_configured();
            self.send_to_device(device_id, create_request_packet());
            self.greet(device_id);
        } else if pair.timestamp.is_some()
            && !matches!(device.pair_state, PairState::Paired | PairState::RequestedByPeer)
        {
            device.pair_state = PairState::RequestedByPeer;
            let summary = crate::i18n::naming(crate::i18n::Key::PairRequest, &device_name);
            crate::plugins::announce(
                glide_sdk::system::Notification::new("glide-phone", summary)
                    .body(crate::i18n::t(crate::i18n::Key::PairRequestHint))
                    .icon("phone-symbolic"),
            );
        }
        self.notify.notify_waiters();
    }

    fn handle_phone_media(&mut self, device_id: &str, packet: &NetworkPacket, payload: Option<&[u8]>) {
        use crate::plugins::remote_media::{Update, apply, request, state_request, store_art};
        let players = self.phone_media.entry(device_id.to_string()).or_default();
        if packet.body.get("transferringAlbumArt").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            let url = packet.body.get("albumArtUrl").and_then(serde_json::Value::as_str).unwrap_or_default();
            if let (Some(bytes), Some(name)) = (payload, packet.body.get("player").and_then(serde_json::Value::as_str))
                && let Some(player) = players.get_mut(name).filter(|p| p.art_url == url)
            {
                player.art_file = store_art(bytes, &format!("{device_id}/{name}"), url);
                self.notify.notify_waiters();
            }
            return;
        }
        match apply(players, &packet.body) {
            Update::Players(fresh) => {
                for name in fresh {
                    self.send_to_device(device_id, state_request(&name));
                }
            }
            Update::State { art_to_fetch: Some(url) } if !url.starts_with("http") => {
                let player = packet.body.get("player").cloned().unwrap_or_default();
                self.send_to_device(device_id, request(serde_json::json!({ "player": player, "albumArtUrl": url })));
            }
            _ => {}
        }
        self.notify.notify_waiters();
    }

    /// Controls one of a phone's players (`action`, `setVolume`, `SetPosition`, ...).
    #[must_use]
    pub fn control_phone_media(&self, device_id: &str, player: &str, mut body: serde_json::Value) -> bool {
        if !self.is_paired(device_id) || !self.is_connected(device_id) {
            return false;
        }
        if let serde_json::Value::Object(map) = &mut body {
            map.insert("player".into(), serde_json::json!(player));
        }
        self.send_to_device(device_id, crate::plugins::remote_media::request(body));
        true
    }

    fn handle_sms(&mut self, device_id: &str, packet: &NetworkPacket) {
        // New messages already reach this computer as the SMS app's notification (with its
        // reply action), so they are only stored here, not announced a second time.
        self.sms.entry(device_id.to_string()).or_default().merge(&packet.body);
        self.notify.notify_waiters();
    }

    /// Asks the phone for its conversations (when the window shows messages).
    pub fn request_sms_conversations(&self, device_id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        self.send_to_device(device_id, crate::plugins::sms::request_conversations_packet());
        Ok(())
    }

    /// Asks the phone for a thread's history.
    pub fn request_sms_thread(
        &self, device_id: &str, thread: i64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        self.send_to_device(device_id, crate::plugins::sms::request_thread_packet(thread, 100));
        Ok(())
    }

    /// Sends a text message through the phone.
    pub fn send_sms(
        &self, device_id: &str, addresses: &[String], text: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        if addresses.is_empty() || text.trim().is_empty() {
            return Err("nothing to send".into());
        }
        let sub_id = self
            .sms
            .get(device_id)
            .and_then(|inbox| inbox.threads.values().flatten().find(|m| m.addresses == addresses))
            .map_or(-1, |m| m.sub_id);
        self.send_to_device(device_id, crate::plugins::sms::send_packet(addresses, text.trim(), sub_id));
        Ok(())
    }

    fn handle_call(&self, device_id: &str, device_name: &str, packet: &NetworkPacket) {
        // Name the caller from the synced address book when the phone sent only a number.
        let mut body = packet.body.clone();
        if body.get("contactName").and_then(serde_json::Value::as_str).is_none_or(str::is_empty)
            && let Some(number) = body.get("phoneNumber").and_then(serde_json::Value::as_str)
            && let Some(name) = crate::plugins::contacts::names(&crate::plugins::contacts::book_dir(device_id))
                .get(&crate::plugins::contacts::number_key(number))
                .cloned()
            && let serde_json::Value::Object(map) = &mut body
        {
            map.insert("contactName".into(), serde_json::json!(name));
        }
        crate::plugins::telephony::handle(device_id, device_name, &body, self.senders.get(device_id).cloned());
    }

    /// Packets of the plugins beyond the original set (run command, media, volume, calls).
    /// Drawing tablet, keyboard state and share batch updates.
    fn handle_input_packet(&mut self, device_id: &str, packet: &NetworkPacket) -> bool {
        match packet.packet_type.as_str() {
            crate::plugins::digitizer::PACKET_TYPE_DIGITIZER_SESSION if self.remote_input => {
                if let Some(session) = crate::plugins::digitizer::parse_session(&packet.body) {
                    self.digitizer.get_or_insert_with(crate::plugins::digitizer::Digitizer::start).session(session);
                }
                true
            }
            crate::plugins::digitizer::PACKET_TYPE_DIGITIZER if self.remote_input => {
                if let Some(digitizer) = &self.digitizer {
                    digitizer.pen(crate::plugins::digitizer::parse_event(&packet.body));
                }
                true
            }
            crate::plugins::mousepad::PACKET_TYPE_MOUSEPAD_KEYBOARDSTATE => {
                if let Some(device) = self.devices.get_mut(device_id) {
                    device.keyboard_off =
                        !packet.body.get("state").and_then(serde_json::Value::as_bool).unwrap_or(true);
                    self.notify.notify_waiters();
                }
                true
            }
            // Later files of a batch only update the totals; each file arrives on its own.
            "kdeconnect.share.request.update" => true,
            _ => false,
        }
    }

    /// Contacts sync; file work runs off the runtime and outside the manager lock.
    fn handle_contacts(&mut self, device_id: &str, packet: &NetworkPacket) {
        use crate::plugins::contacts;
        let dir = contacts::book_dir(device_id);
        let body = packet.body.clone();
        let is_list = packet.packet_type == contacts::PACKET_TYPE_RESPONSE_UIDS;
        let sender = self.senders.get(device_id).cloned();
        let notify = self.notify.clone();
        let work = move || {
            if is_list {
                if let (Some(request), Some(sender)) = (contacts::on_uids(&dir, &body), sender) {
                    let _ = sender.send(request);
                }
            } else if contacts::on_vcards(&dir, &body) > 0 {
                notify.notify_waiters();
            }
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::spawn_blocking(work);
        } else {
            work();
        }
    }

    fn handle_feature_packet(
        &mut self, device_id: &str, packet: &NetworkPacket, device_name: &str, payload: Option<&[u8]>,
    ) -> bool {
        match packet.packet_type.as_str() {
            crate::plugins::remote_media::PACKET_TYPE_MPRIS => {
                self.handle_phone_media(device_id, packet, payload);
                true
            }
            crate::plugins::sms::PACKET_TYPE_SMS_MESSAGES => {
                self.handle_sms(device_id, packet);
                true
            }
            crate::plugins::contacts::PACKET_TYPE_RESPONSE_UIDS
            | crate::plugins::contacts::PACKET_TYPE_RESPONSE_VCARDS => {
                self.handle_contacts(device_id, packet);
                true
            }
            crate::plugins::connectivity::PACKET_TYPE_CONNECTIVITY_REPORT => {
                if let Some(device) = self.devices.get_mut(device_id) {
                    device.signals = crate::plugins::connectivity::parse(&packet.body);
                    self.notify.notify_waiters();
                }
                true
            }
            crate::plugins::sftp::PACKET_TYPE_SFTP => {
                match crate::plugins::sftp::parse(&packet.body) {
                    // Only the answer to our own request; mount where the phone is connected from,
                    // never a host the packet names (KDE Connect may name another interface).
                    crate::plugins::sftp::Reply::Login(mut login)
                        if tokio::runtime::Handle::try_current().is_ok()
                            && self
                                .browse_requested
                                .remove(device_id)
                                .is_some_and(|asked| asked.elapsed() < Duration::from_mins(1)) =>
                    {
                        if let Some(ip) = self.last_addresses.get(device_id) {
                            login.ip = ip.to_string();
                        }
                        let (id, name) = (device_id.to_string(), device_name.to_string());
                        tokio::spawn(async move { crate::plugins::sftp::browse(&id, &name, login).await });
                    }
                    crate::plugins::sftp::Reply::Error(message) => crate::plugins::announce(
                        glide_sdk::system::Notification::new(
                            "glide-phone",
                            crate::i18n::naming(crate::i18n::Key::BrowseFailed, device_name),
                        )
                        .body(message)
                        .icon("folder-remote-symbolic"),
                    ),
                    _ => {}
                }
                true
            }
            crate::plugins::telephony::PACKET_TYPE_TELEPHONY => {
                self.handle_call(device_id, device_name, packet);
                true
            }
            crate::plugins::systemvolume::PACKET_TYPE_SYSTEMVOLUME_REQUEST => {
                if let Some(sender) = self.senders.get(device_id).cloned()
                    && tokio::runtime::Handle::try_current().is_ok()
                {
                    let body = packet.body.clone();
                    tokio::spawn(async move { crate::plugins::systemvolume::handle_request(&body, &sender).await });
                }
                true
            }
            crate::plugins::mpris::PACKET_TYPE_MPRIS_REQUEST => {
                if let Some(sender) = self.senders.get(device_id).cloned()
                    && tokio::runtime::Handle::try_current().is_ok()
                {
                    let sink = crate::plugins::mpris::Sink {
                        sender,
                        cert: self.cert.clone(),
                        fingerprint: self.devices.get(device_id).and_then(|d| d.peer_fingerprint.clone()),
                    };
                    let body = packet.body.clone();
                    tokio::spawn(async move {
                        if let Some(conn) = crate::plugins::mpris::session().await {
                            crate::plugins::mpris::handle_request(&conn, &body, &sink).await;
                        }
                    });
                }
                true
            }
            crate::plugins::runcommand::PACKET_TYPE_RUNCOMMAND_REQUEST => {
                if self.commands.handle_request(packet, self.senders.get(device_id)) {
                    self.commands_setup_requested = true;
                    self.notify.notify_waiters();
                }
                true
            }
            _ => self.handle_input_packet(device_id, packet),
        }
    }

    fn handle_aux_packet(
        &mut self, device_id: &str, packet: &NetworkPacket, device_name: &str, payload: Option<&[u8]>,
    ) -> bool {
        match packet.packet_type.as_str() {
            PACKET_TYPE_CLIPBOARD | crate::plugins::clipboard::PACKET_TYPE_CLIPBOARD_CONNECT => {
                if let Ok(clipboard) = handle_clipboard_packet(packet) {
                    // On connect an unknown (0) timestamp never wins.
                    let connect = packet.packet_type == crate::plugins::clipboard::PACKET_TYPE_CLIPBOARD_CONNECT;
                    if connect && clipboard.timestamp.unwrap_or(0) <= 0 {
                        return true;
                    }
                    let timestamp = clipboard.timestamp.unwrap_or(packet.id);
                    if self.clipboard.as_ref().is_none_or(|(_, current)| timestamp >= *current) {
                        if self.clipboard.as_ref().is_none_or(|(current, _)| *current != clipboard.content) {
                            crate::plugins::clipboard::apply(&clipboard.content);
                        }
                        self.clipboard = Some((clipboard.content, timestamp));
                        self.notify.notify_waiters();
                    }
                }
                true
            }
            crate::plugins::mousepad::PACKET_TYPE_MOUSEPAD_REQUEST if self.remote_input => {
                let plan = self.mousepad.plan(&packet.body);
                let input = self.input.get_or_insert_with(crate::input::RemoteInput::start);
                input.send(plan.actions);
                if let Some(text) = plan.text {
                    let queue = input.clone_queue();
                    self.typist.get_or_insert_with(|| crate::plugins::mousepad::Typist::start(queue)).type_text(text);
                }
                if let Some(echo) = crate::plugins::mousepad::echo(&packet.body) {
                    self.send_to_device(device_id, echo);
                }
                true
            }
            PACKET_TYPE_PRESENTER | "kdeconnect.mousepad.request" => {
                if let Some(event) = handle_presenter_packet(packet) {
                    if let (
                        PresenterEvent::Move { dx, dy },
                        Some((id, PresenterEvent::Move { dx: pending_x, dy: pending_y })),
                    ) = (event, self.presenter_events.back_mut())
                        && id == device_id
                        && *pending_x * dx >= 0.0
                        && *pending_y * dy >= 0.0
                    {
                        *pending_x = (*pending_x + dx).clamp(-1.0, 1.0);
                        *pending_y = (*pending_y + dy).clamp(-1.0, 1.0);
                        self.notify.notify_waiters();
                        return true;
                    }
                    if self.presenter_events.len() >= 256 {
                        if let Some(index) = self
                            .presenter_events
                            .iter()
                            .position(|(_, event)| matches!(event, PresenterEvent::Move { .. }))
                        {
                            self.presenter_events.remove(index);
                        } else {
                            self.presenter_events.pop_front();
                        }
                    }
                    self.presenter_events.push_back((device_id.to_string(), event));
                    self.notify.notify_waiters();
                }
                true
            }
            PACKET_TYPE_SHARE_REQUEST => {
                if let Ok(share) = handle_share_packet(packet) {
                    crate::plugins::share::deliver(device_name, share);
                }
                true
            }
            _ => self.handle_feature_packet(device_id, packet, device_name, payload),
        }
    }

    pub fn accept_pair(
        &mut self, device_id: &str, fingerprint: String,
    ) -> Result<NetworkPacket, Box<dyn std::error::Error + Send + Sync>> {
        if self.devices.get(device_id).is_some_and(|device| device.pair_state == PairState::RequestedByPeer)
            && !self.is_connected(device_id)
            && fingerprint.is_empty()
        {
            return Err("phone disconnected before pairing could be accepted".into());
        }
        let device = self.devices.get_mut(device_id).ok_or_else(|| format!("device not found: {device_id}"))?;

        let has_explicit_fp = !fingerprint.is_empty();
        let resolved_fp = if fingerprint.is_empty() {
            device.peer_fingerprint.clone().unwrap_or_default()
        } else {
            device.peer_fingerprint = Some(fingerprint.clone());
            fingerprint
        };

        let should_finalize = device.pair_state == PairState::RequestedByPeer || has_explicit_fp;
        if should_finalize && resolved_fp.is_empty() {
            return Err("the phone has not shown a certificate to pair with".into());
        }
        let pair_body = if should_finalize {
            device.pair_state = PairState::Paired;
            let dev_name = device.name.clone();
            self.config.paired_devices.insert(
                device_id.to_string(),
                PairedDevice { id: device_id.to_string(), name: dev_name, fingerprint: resolved_fp },
            );
            self.config.save_if_configured();
            PairBody::accept()
        } else {
            device.pair_state = PairState::RequestedByLocal;
            PairBody::request()
        };

        let is_now_paired = device.pair_state == PairState::Paired;
        let packet = pair_body.to_packet()?;
        self.send_to_device(device_id, packet.clone());
        if is_now_paired {
            self.send_to_device(device_id, create_request_packet());
            self.greet(device_id);
        }
        self.notify.notify_waiters();
        Ok(packet)
    }

    pub fn send_pending_pair_request(&self, device_id: &str) {
        if self.devices.get(device_id).is_some_and(|device| device.pair_state == PairState::RequestedByLocal)
            && let Ok(packet) = PairBody::request().to_packet()
        {
            self.send_to_device(device_id, packet);
        }
    }

    pub fn reject_pair(&mut self, device_id: &str) -> Result<NetworkPacket, Box<dyn std::error::Error + Send + Sync>> {
        self.presenter_events.retain(|(id, _)| id != device_id);
        if let Some(device) = self.devices.get_mut(device_id) {
            device.pair_state = PairState::Unpaired;
            device.notifications.clear();
            device.seen_notifications.clear();
        }
        self.config.paired_devices.remove(device_id);
        self.config.save_if_configured();
        let pair_body = PairBody::reject();
        let packet = pair_body.to_packet()?;
        self.send_to_device(device_id, packet.clone());
        self.notify.notify_waiters();
        Ok(packet)
    }

    pub fn dismiss_notification(&mut self, device_id: &str, notif_id: &str) -> Option<NetworkPacket> {
        let device = self.devices.get_mut(device_id)?;
        device.notifications.remove(notif_id)?;
        let packet = create_dismiss_packet(notif_id);
        self.send_to_device(device_id, packet.clone());
        self.notify.notify_waiters();
        Some(packet)
    }

    pub fn clear_notifications(&mut self, device_id: &str) -> usize {
        let ids: Vec<String> = if let Some(device) = self.devices.get_mut(device_id) {
            device.notifications.drain().map(|(id, _)| id).collect()
        } else {
            return 0;
        };
        let count = ids.len();
        for id in ids {
            let packet = create_dismiss_packet(&id);
            self.send_to_device(device_id, packet);
        }
        if count > 0 {
            self.notify.notify_waiters();
        }
        count
    }

    #[must_use]
    pub fn sorted_cards(&self) -> Vec<PhoneCardData> {
        let mut cards: Vec<PhoneCardData> = self
            .devices
            .values()
            .map(|s| {
                let mut notifications: Vec<ActiveNotification> = s.notifications.values().cloned().collect();
                notifications.sort_by(|a, b| b.seq.cmp(&a.seq).then_with(|| a.id.cmp(&b.id)));
                PhoneCardData {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    device_type: s.device_type,
                    pair_state: s.pair_state,
                    battery: s.battery,
                    notifications,
                    peer_fingerprint: s.peer_fingerprint.clone(),
                    connected: s.connected,
                    signals: s.signals.clone(),
                    keyboard_off: s.keyboard_off,
                }
            })
            .collect();
        cards.sort_by(|a, b| {
            let rank = |d: &PhoneCardData| match (d.pair_state, d.connected) {
                (PairState::Paired, true) => 0,
                (PairState::RequestedByPeer, _) => 1,
                (PairState::RequestedByLocal, _) => 2,
                (PairState::Paired, false) => 3,
                (PairState::Unpaired, _) => 4,
            };
            rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)).then_with(|| a.id.cmp(&b.id))
        });
        cards
    }

    fn is_paired(&self, device_id: &str) -> bool {
        self.devices.get(device_id).is_some_and(|device| device.pair_state == PairState::Paired)
    }

    fn reachable(&self, device_id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.is_paired(device_id) {
            return Err(format!("device is not paired: {device_id}").into());
        }
        if !self.is_connected(device_id) {
            return Err(format!("device is not connected: {device_id}").into());
        }
        Ok(())
    }

    pub fn ring_phone(&self, device_id: &str) -> Result<NetworkPacket, Box<dyn std::error::Error + Send + Sync>> {
        if !self.devices.contains_key(device_id) {
            return Err(format!("unknown device {device_id}").into());
        }
        if !self.is_paired(device_id) {
            return Err(format!("device is not paired: {device_id}").into());
        }
        let packet = crate::plugins::find_device::create_ring_packet();
        self.send_to_device(device_id, packet.clone());
        self.notify.notify_waiters();
        Ok(packet)
    }

    pub fn stop_alarm(&mut self, device_id: &str) {
        if let Some(handle) = self.alarms.remove(device_id) {
            handle.stop();
            self.notify.notify_waiters();
        }
    }

    #[must_use]
    pub fn is_ringing(&self) -> bool {
        self.alarms.values().any(AlarmHandle::is_active)
    }

    #[must_use]
    pub fn reply_notification(&self, device_id: &str, reply_id: &str, message: &str) -> NetworkPacket {
        let packet = create_reply_packet(reply_id, message);
        if self.is_paired(device_id) {
            self.send_to_device(device_id, packet.clone());
        }
        packet
    }

    #[must_use]
    pub fn trigger_notification_action(&self, device_id: &str, key: &str, action: &str) -> NetworkPacket {
        let packet = create_action_packet(key, action);
        if self.is_paired(device_id) {
            self.send_to_device(device_id, packet.clone());
        }
        packet
    }

    pub fn set_clipboard(
        &mut self, device_id: &str, content: &str,
    ) -> Result<NetworkPacket, Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis().min(i64::MAX as u128) as i64;
        let packet = create_clipboard_packet(content, timestamp);
        self.clipboard = Some((content.to_string(), timestamp));
        self.send_to_device(device_id, packet.clone());
        self.notify.notify_waiters();
        Ok(packet)
    }

    #[must_use]
    pub fn clipboard_text(&self) -> Option<String> {
        self.clipboard.as_ref().map(|(content, _)| content.clone())
    }

    #[must_use]
    pub fn take_presenter_event(&mut self) -> Option<PresenterEvent> {
        self.presenter_events.pop_front().map(|(_, event)| event)
    }

    pub fn share_text(
        &self, device_id: &str, text: &str,
    ) -> Result<NetworkPacket, Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        let packet = create_share_text_packet(text);
        self.send_to_device(device_id, packet.clone());
        Ok(packet)
    }

    pub fn share_url(
        &self, device_id: &str, url: &str,
    ) -> Result<NetworkPacket, Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        let packet = create_share_url_packet(url);
        self.send_to_device(device_id, packet.clone());
        Ok(packet)
    }

    /// Sends files one after another as payloads (`SharePlugin`'s batch), returning at once.
    pub fn share_files(
        &self, device_id: &str, paths: Vec<std::path::PathBuf>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.reachable(device_id)?;
        let files: Vec<(std::path::PathBuf, u64)> = paths
            .into_iter()
            .filter_map(|path| {
                std::fs::metadata(&path).ok().filter(std::fs::Metadata::is_file).map(|m| (path, m.len()))
            })
            .collect();
        if files.is_empty() {
            return Err("nothing to send".into());
        }
        let sender = self.senders.get(device_id).cloned().ok_or("device is not connected")?;
        let cert = self.cert.clone();
        let fingerprint = self.devices.get(device_id).and_then(|d| d.peer_fingerprint.clone());
        let device_name = self.devices.get(device_id).map(|d| d.name.clone()).unwrap_or_default();
        let total: u64 = files.iter().map(|(_, size)| size).sum();
        let count = files.len();
        tokio::spawn(async move {
            let mut failed = 0usize;
            for (path, size) in &files {
                let sent = async {
                    let listener = crate::payload::PayloadListener::bind(&cert, fingerprint.clone()).await.ok()?;
                    let file = tokio::fs::File::open(path).await.ok()?;
                    let mut packet = crate::payload::share_file_packet(path, count, total);
                    crate::payload::attach(&mut packet, *size, listener.port());
                    sender.send(packet).ok()?;
                    listener.serve(file, *size, crate::payload::ACCEPT_TIMEOUT).await.then_some(())
                }
                .await;
                if sent.is_none() {
                    failed += 1;
                }
            }
            crate::plugins::share::announce_sent(&device_name, count, failed);
        });
        Ok(())
    }
}
