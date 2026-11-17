//! The phone's media players on this computer (KDE Connect's `MprisRemote`): their state
//! from `kdeconnect.mpris`, controls sent back as `kdeconnect.mpris.request`, and each one
//! exported as an MPRIS player so the desktop's media keys and widgets drive it.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::protocol::packet::NetworkPacket;
use crate::state::PacketSender;

pub const PACKET_TYPE_MPRIS: &str = "kdeconnect.mpris";
pub const PACKET_TYPE_MPRIS_REQUEST: &str = "kdeconnect.mpris.request";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // mirrors the protocol's can* flags
pub struct RemotePlayer {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub is_playing: bool,
    /// Milliseconds at `position_at`.
    pub position: i64,
    pub length: i64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
    pub volume: i64,
    pub art_url: String,
    pub art_file: Option<PathBuf>,
    pub loop_status: Option<String>,
    pub shuffle: Option<bool>,
    #[serde(skip)]
    pub position_at: Option<Instant>,
}

impl RemotePlayer {
    fn new(player: &str) -> Self {
        Self {
            player: player.to_string(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            is_playing: false,
            position: 0,
            length: -1,
            can_play: true,
            can_pause: true,
            can_go_next: true,
            can_go_previous: true,
            can_seek: false,
            volume: -1,
            art_url: String::new(),
            art_file: None,
            loop_status: None,
            shuffle: None,
            position_at: None,
        }
    }

    /// Position now, advancing while playing (`MprisPlayer.getPosition`).
    #[must_use]
    pub fn current_position(&self) -> i64 {
        let moved = match (self.is_playing, self.position_at) {
            (true, Some(at)) => self.position + i64::try_from(at.elapsed().as_millis()).unwrap_or(0),
            _ => self.position,
        };
        if self.length > 0 { moved.clamp(0, self.length) } else { moved.max(0) }
    }
}

/// What a `kdeconnect.mpris` packet from the phone changed.
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// New players to ask for their state.
    Players(Vec<String>),
    /// This player's state changed; `Some(url)` asks for its album art.
    State {
        art_to_fetch: Option<String>,
    },
    Nothing,
}

pub fn apply(players: &mut BTreeMap<String, RemotePlayer>, body: &Json) -> Update {
    if let Some(list) = body.get("playerList").and_then(Json::as_array) {
        let names: Vec<String> = list.iter().filter_map(Json::as_str).map(str::to_string).collect();
        players.retain(|name, _| names.contains(name));
        let fresh: Vec<String> = names.iter().filter(|name| !players.contains_key(*name)).cloned().collect();
        for name in &fresh {
            players.insert(name.clone(), RemotePlayer::new(name));
        }
        return Update::Players(fresh);
    }
    let Some(name) = body.get("player").and_then(Json::as_str) else { return Update::Nothing };
    if body.get("transferringAlbumArt").and_then(Json::as_bool).unwrap_or(false) {
        return Update::Nothing; // handled with its payload
    }
    let player = players.entry(name.to_string()).or_insert_with(|| RemotePlayer::new(name));
    let text =
        |key: &str, old: &str| body.get(key).and_then(Json::as_str).map_or_else(|| old.to_string(), str::to_string);
    let flag = |key: &str, old: bool| body.get(key).and_then(Json::as_bool).unwrap_or(old);
    let int = |key: &str, old: i64| body.get(key).and_then(Json::as_i64).unwrap_or(old);
    player.title = text("title", &player.title);
    player.artist = text("artist", &player.artist);
    player.album = text("album", &player.album);
    let was_position = player.current_position();
    player.is_playing = flag("isPlaying", player.is_playing);
    player.position = int("pos", was_position);
    player.position_at = Some(Instant::now());
    player.length = int("length", player.length);
    player.can_play = flag("canPlay", player.can_play);
    player.can_pause = flag("canPause", player.can_pause);
    player.can_go_next = flag("canGoNext", player.can_go_next);
    player.can_go_previous = flag("canGoPrevious", player.can_go_previous);
    player.can_seek = flag("canSeek", player.can_seek);
    player.volume = int("volume", player.volume);
    if let Some(status) = body.get("loopStatus").and_then(Json::as_str) {
        player.loop_status = Some(status.to_string());
    }
    if let Some(shuffle) = body.get("shuffle").and_then(Json::as_bool) {
        player.shuffle = Some(shuffle);
    }
    let mut art_to_fetch = None;
    if let Some(url) = body.get("albumArtUrl").and_then(Json::as_str)
        && url != player.art_url
    {
        player.art_url = url.to_string();
        player.art_file = None;
        if !url.is_empty() {
            art_to_fetch = Some(url.to_string());
        }
    }
    Update::State { art_to_fetch }
}

#[must_use]
pub fn request(body: Json) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_MPRIS_REQUEST, body)
}

#[must_use]
pub fn state_request(player: &str) -> NetworkPacket {
    request(json!({ "player": player, "requestNowPlaying": true, "requestVolume": true }))
}

/// Saves album art the phone sent so MPRIS clients can show it as a file.
#[must_use]
pub fn store_art(bytes: &[u8], url: &str) -> Option<PathBuf> {
    use std::hash::{Hash as _, Hasher as _};
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let dir = base.join("glide-phone-art");
    std::fs::create_dir_all(&dir).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let path = dir.join(format!("{:016x}", hasher.finish()));
    glide_config::write_atomic(&path, bytes, 0o600).ok()?;
    Some(path)
}

// ---- MPRIS export ----

type Shared = Arc<Mutex<RemotePlayer>>;

struct Root {
    identity: String,
}

#[allow(clippy::unused_self)] // zbus interface signatures
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}
    fn quit(&self) {}
    #[zbus(property)]
    fn identity(&self) -> String {
        self.identity.clone()
    }
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    state: Shared,
    sender: PacketSender,
}

impl Player {
    fn send(&self, mut body: Json) {
        if let (Json::Object(map), Ok(state)) = (&mut body, self.state.lock()) {
            map.insert("player".into(), json!(state.player));
        }
        let _ = self.sender.send(request(body));
    }

    fn snapshot(&self) -> RemotePlayer {
        self.state.lock().map_or_else(|_| RemotePlayer::new(""), |s| s.clone())
    }
}

#[allow(clippy::unused_self, clippy::needless_pass_by_value)] // zbus interface signatures
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play(&self) {
        self.send(json!({ "action": "Play" }));
    }
    fn pause(&self) {
        self.send(json!({ "action": "Pause" }));
    }
    fn play_pause(&self) {
        self.send(json!({ "action": "PlayPause" }));
    }
    fn stop(&self) {
        self.send(json!({ "action": "Stop" }));
    }
    fn next(&self) {
        self.send(json!({ "action": "Next" }));
    }
    fn previous(&self) {
        self.send(json!({ "action": "Previous" }));
    }
    fn seek(&self, offset: i64) {
        self.send(json!({ "Seek": offset }));
    }
    fn set_position(&self, track: ObjectPath<'_>, position: i64) {
        let _ = track; // one track at a time on the phone
        self.send(json!({ "SetPosition": position / 1000 }));
    }
    #[zbus(property)]
    fn playback_status(&self) -> String {
        if self.snapshot().is_playing { "Playing" } else { "Paused" }.into()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let s = self.snapshot();
        let mut map = HashMap::new();
        let put = |map: &mut HashMap<String, OwnedValue>, key: &str, value: Value<'_>| {
            if let Ok(value) = OwnedValue::try_from(value) {
                map.insert(key.to_string(), value);
            }
        };
        if let Ok(path) = ObjectPath::try_from("/org/glide/phone/track") {
            put(&mut map, "mpris:trackid", Value::from(path));
        }
        put(&mut map, "xesam:title", Value::from(s.title.as_str()));
        if !s.artist.is_empty() {
            put(&mut map, "xesam:artist", Value::from(vec![s.artist.as_str()]));
        }
        if !s.album.is_empty() {
            put(&mut map, "xesam:album", Value::from(s.album.as_str()));
        }
        if s.length > 0 {
            put(&mut map, "mpris:length", Value::from(s.length * 1000));
        }
        if let Some(file) = &s.art_file {
            put(&mut map, "mpris:artUrl", Value::from(format!("file://{}", file.display())));
        } else if s.art_url.starts_with("http") {
            put(&mut map, "mpris:artUrl", Value::from(s.art_url.as_str()));
        }
        map
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        self.snapshot().current_position() * 1000
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        let volume = self.snapshot().volume;
        if volume < 0 { 1.0 } else { volume as f64 / 100.0 }
    }
    #[zbus(property)]
    fn set_volume(&self, value: f64) {
        self.send(json!({ "setVolume": (value.clamp(0.0, 1.0) * 100.0).round() as i64 }));
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.snapshot().can_go_next
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.snapshot().can_go_previous
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.snapshot().can_play
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.snapshot().can_pause
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.snapshot().can_seek
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

/// One phone player published on the session bus.
pub struct Exported {
    connection: zbus::Connection,
    state: Shared,
}

impl Exported {
    /// `org.mpris.MediaPlayer2.glidephone.<device>.<player>`, as KDE Connect names its remotes.
    pub async fn publish(
        address: Option<&str>, device_id: &str, device_name: &str, player: &RemotePlayer, sender: PacketSender,
    ) -> zbus::Result<Self> {
        let state: Shared = Arc::new(Mutex::new(player.clone()));
        let name = format!("org.mpris.MediaPlayer2.glidephone.d{}.p{}", bus_safe(device_id), bus_safe(&player.player));
        let builder = match address {
            Some(address) => zbus::connection::Builder::address(address)?,
            None => zbus::connection::Builder::session()?,
        };
        let connection = builder
            .name(name)?
            .serve_at("/org/mpris/MediaPlayer2", Root { identity: format!("{} ({device_name})", player.player) })?
            .serve_at("/org/mpris/MediaPlayer2", Player { state: state.clone(), sender })?
            .build()
            .await?;
        Ok(Self { connection, state })
    }

    /// Copies the new state in and tells MPRIS clients what changed.
    pub async fn update(&self, player: &RemotePlayer) {
        if let Ok(mut state) = self.state.lock() {
            *state = player.clone();
        }
        let Ok(iface) = self.connection.object_server().interface::<_, Player>("/org/mpris/MediaPlayer2").await else {
            return;
        };
        let emitter = iface.signal_emitter();
        let player = iface.get().await;
        let _ = player.playback_status_changed(emitter).await;
        let _ = player.metadata_changed(emitter).await;
        let _ = player.volume_changed(emitter).await;
        let _ = player.can_go_next_changed(emitter).await;
        let _ = player.can_go_previous_changed(emitter).await;
        let _ = player.can_seek_changed(emitter).await;
    }
}

fn bus_safe(text: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

/// (device, player), its state, the device's name and where its requests go.
type Wanted = ((String, String), RemotePlayer, String, Option<PacketSender>);

/// Keeps one exported MPRIS player per phone player, following the manager's state.
pub async fn run_exports(manager: Arc<tokio::sync::Mutex<crate::state::PhoneManager>>, address: Option<String>) {
    let notify = manager.lock().await.notify.clone();
    let mut exported: HashMap<(String, String), (Exported, RemotePlayer)> = HashMap::new();
    loop {
        let changed = notify.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let wanted: Vec<Wanted> = {
            let mgr = manager.lock().await;
            mgr.phone_media
                .iter()
                .flat_map(|(device, players)| {
                    let name = mgr.devices.get(device).map(|d| d.name.clone()).unwrap_or_default();
                    let sender = mgr.sender_for(device);
                    players
                        .values()
                        .map(move |p| ((device.clone(), p.player.clone()), p.clone(), name.clone(), sender.clone()))
                })
                .collect()
        };
        let keys: Vec<(String, String)> = wanted.iter().map(|(key, ..)| key.clone()).collect();
        exported.retain(|key, _| keys.contains(key));
        for (key, player, device_name, sender) in wanted {
            match exported.get_mut(&key) {
                Some((export, last)) if *last != player => {
                    export.update(&player).await;
                    *last = player;
                }
                Some(_) => {}
                None => {
                    let Some(sender) = sender else { continue };
                    if let Ok(export) =
                        Exported::publish(address.as_deref(), &key.0, &device_name, &player, sender).await
                    {
                        exported.insert(key, (export, player));
                    }
                }
            }
        }
        changed.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_player_list_then_state_builds_the_phones_players() {
        let mut players = BTreeMap::new();
        assert_eq!(apply(&mut players, &json!({"playerList": ["Spotify"]})), Update::Players(vec!["Spotify".into()]));
        let update = apply(
            &mut players,
            &json!({"player": "Spotify", "title": "Пісня", "isPlaying": true, "pos": 1000, "length": 5000,
                    "albumArtUrl": "kdeArtHash://abc", "volume": 40}),
        );
        assert_eq!(update, Update::State { art_to_fetch: Some("kdeArtHash://abc".into()) });
        let spotify = &players["Spotify"];
        assert_eq!(spotify.title, "Пісня");
        assert!(spotify.current_position() >= 1000);
        assert_eq!(spotify.volume, 40);
        assert_eq!(apply(&mut players, &json!({"playerList": []})), Update::Players(Vec::new()));
        assert!(players.is_empty());
    }
}
