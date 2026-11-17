//! `kdeconnect.mpris`: this computer's media players for the phone (KDE Connect's
//! `MprisControl` on the desktop): the player list, now-playing state, controls, volume,
//! loop/shuffle, seeking and album art, all through MPRIS on the session bus.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use futures_util::StreamExt as _;
use serde_json::{Map, Value as Json, json};
use tokio::sync::Mutex;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream, Proxy};

use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_MPRIS: &str = "kdeconnect.mpris";
pub const PACKET_TYPE_MPRIS_REQUEST: &str = "kdeconnect.mpris.request";
const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const ROOT: &str = "org.mpris.MediaPlayer2";
const MAX_ART_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
struct Players {
    /// Shown name -> bus name. Names come from `Identity`, made unique like KDE Connect.
    by_name: BTreeMap<String, String>,
    /// Unique bus name (signal sender) -> well-known bus name.
    owners: HashMap<String, String>,
}

/// Known players per bus (keyed by the bus's GUID), so separate buses never mix.
fn registry() -> &'static Mutex<HashMap<String, Players>> {
    static PLAYERS: OnceLock<Mutex<HashMap<String, Players>>> = OnceLock::new();
    PLAYERS.get_or_init(Mutex::default)
}

fn bus_key(conn: &Connection) -> String {
    conn.server_guid().to_string()
}

/// The session bus, shared by every MPRIS task.
pub async fn session() -> Option<Connection> {
    static SESSION: tokio::sync::OnceCell<Option<Connection>> = tokio::sync::OnceCell::const_new();
    SESSION.get_or_init(|| async { Connection::session().await.ok() }).await.clone()
}

/// Where packets for one phone go; art needs the payload channel.
pub struct Sink {
    pub sender: crate::state::PacketSender,
    pub cert: crate::crypto::cert::IdentityCert,
    pub fingerprint: Option<String>,
}

async fn proxy<'a>(conn: &Connection, bus: &'a str, iface: &'a str) -> zbus::Result<Proxy<'a>> {
    Proxy::new(conn, bus.to_string(), PATH, iface).await
}

/// Rebuilds the player list from the bus and returns the shown names.
pub async fn refresh(conn: &Connection) -> Vec<String> {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else { return Vec::new() };
    let names = dbus.list_names().await.unwrap_or_default();
    let mut by_name = BTreeMap::new();
    let mut owners = HashMap::new();
    for name in names.iter().map(ToString::to_string).filter(|name| name.starts_with(PREFIX)) {
        let identity = match proxy(conn, &name, ROOT).await {
            Ok(p) => p.get_property::<String>("Identity").await.ok(),
            Err(_) => None,
        }
        .filter(|identity| !identity.trim().is_empty())
        .unwrap_or_else(|| name.trim_start_matches(PREFIX).split('.').next().unwrap_or("player").to_string());
        let mut shown = identity.clone();
        let mut n = 2;
        while by_name.contains_key(&shown) {
            shown = format!("{identity} [{n}]");
            n += 1;
        }
        if let Ok(bus_name) = zbus::names::BusName::try_from(name.as_str())
            && let Ok(owner) = dbus.get_name_owner(bus_name).await
        {
            owners.insert(owner.to_string(), name.clone());
        }
        by_name.insert(shown, name);
    }
    let shown: Vec<String> = by_name.keys().cloned().collect();
    registry().lock().await.insert(bus_key(conn), Players { by_name, owners });
    shown
}

#[must_use]
pub fn list_packet(names: &[String]) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_MPRIS, json!({ "playerList": names, "supportAlbumArtPayload": true }))
}

async fn bus_of(conn: &Connection, name: &str) -> Option<String> {
    registry().lock().await.get(&bus_key(conn))?.by_name.get(name).cloned()
}

async fn name_of_owner(conn: &Connection, owner: &str) -> Option<String> {
    let guard = registry().lock().await;
    let players = guard.get(&bus_key(conn))?;
    let bus = players.owners.get(owner)?;
    players.by_name.iter().find(|(_, b)| *b == bus).map(|(name, _)| name.clone())
}

fn text(value: &Value<'_>) -> Option<String> {
    match value {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        Value::Value(inner) => text(inner),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(text).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => None,
    }
}

fn number(value: &Value<'_>) -> Option<i64> {
    match value {
        Value::I64(v) => Some(*v),
        Value::U64(v) => i64::try_from(*v).ok(),
        Value::I32(v) => Some(i64::from(*v)),
        Value::U32(v) => Some(i64::from(*v)),
        Value::F64(v) => Some(*v as i64),
        Value::Value(inner) => number(inner),
        _ => None,
    }
}

/// The now-playing packet for `name` (`MprisControlPlugin::sendPlayerStatus`).
pub async fn state_packet(conn: &Connection, name: &str) -> Option<NetworkPacket> {
    let bus = bus_of(conn, name).await?;
    let player = proxy(conn, &bus, PLAYER).await.ok()?;
    let mut body = Map::new();
    body.insert("player".into(), json!(name));
    if let Ok(metadata) = player.get_property::<OwnedValue>("Metadata").await
        && let Value::Dict(dict) = &*metadata
    {
        let mut fields: HashMap<String, Value<'_>> = HashMap::new();
        for (key, value) in dict.iter() {
            if let Some(key) = text(key) {
                fields.insert(key, value.try_clone().unwrap_or(Value::Bool(false)));
            }
        }
        let get = |key: &str| fields.get(key).and_then(text);
        body.insert("title".into(), json!(get("xesam:title").unwrap_or_default()));
        body.insert("artist".into(), json!(get("xesam:artist").unwrap_or_default()));
        body.insert("album".into(), json!(get("xesam:album").unwrap_or_default()));
        body.insert("url".into(), json!(get("xesam:url").unwrap_or_default()));
        body.insert("albumArtUrl".into(), json!(get("mpris:artUrl").unwrap_or_default()));
        let length = fields.get("mpris:length").and_then(number).unwrap_or(-1);
        body.insert("length".into(), json!(if length > 0 { length / 1000 } else { -1 }));
    }
    let status = player.get_property::<String>("PlaybackStatus").await.unwrap_or_default();
    body.insert("isPlaying".into(), json!(status == "Playing"));
    if let Ok(position) = player.get_property::<i64>("Position").await {
        body.insert("pos".into(), json!(position / 1000));
    }
    for (property, key) in [
        ("CanPlay", "canPlay"),
        ("CanPause", "canPause"),
        ("CanGoNext", "canGoNext"),
        ("CanGoPrevious", "canGoPrevious"),
        ("CanSeek", "canSeek"),
    ] {
        body.insert(key.into(), json!(player.get_property::<bool>(property).await.unwrap_or(false)));
    }
    let volume = player.get_property::<f64>("Volume").await.map_or(-1, |v| (v * 100.0).round() as i64);
    body.insert("volume".into(), json!(volume));
    if let Ok(loop_status) = player.get_property::<String>("LoopStatus").await {
        body.insert("loopStatus".into(), json!(loop_status));
    }
    if let Ok(shuffle) = player.get_property::<bool>("Shuffle").await {
        body.insert("shuffle".into(), json!(shuffle));
    }
    Some(NetworkPacket::new(PACKET_TYPE_MPRIS, Json::Object(body)))
}

/// Applies a phone's `kdeconnect.mpris.request` and answers it.
pub async fn handle_request(conn: &Connection, body: &Json, sink: &Sink) {
    if body.get("requestPlayerList").and_then(Json::as_bool).unwrap_or(false) {
        let names = refresh(conn).await;
        let _ = sink.sender.send(list_packet(&names));
    }
    let Some(name) = body.get("player").and_then(Json::as_str) else { return };
    let Some(bus) = bus_of(conn, name).await else { return };
    let Ok(player) = proxy(conn, &bus, PLAYER).await else { return };
    if let Some(action) = body.get("action").and_then(Json::as_str)
        && matches!(action, "Play" | "Pause" | "PlayPause" | "Next" | "Previous" | "Stop")
    {
        let _ = player.call_method(action, &()).await;
    }
    if let Some(volume) = body.get("setVolume").and_then(Json::as_i64) {
        let _ = player.set_property("Volume", (volume.clamp(0, 100) as f64) / 100.0).await;
    }
    if let Some(offset) = body.get("Seek").and_then(Json::as_i64) {
        let _ = player.call_method("Seek", &(offset,)).await; // microseconds
    }
    if let Some(position) = body.get("SetPosition").and_then(Json::as_i64) {
        set_position(&player, position).await;
    }
    if let Some(status) = body.get("setLoopStatus").and_then(Json::as_str)
        && matches!(status, "None" | "Track" | "Playlist")
    {
        let _ = player.set_property("LoopStatus", status).await;
    }
    if let Some(shuffle) = body.get("setShuffle").and_then(Json::as_bool) {
        let _ = player.set_property("Shuffle", shuffle).await;
    }
    if let Some(url) = body.get("albumArtUrl").and_then(Json::as_str) {
        send_album_art(name, url, sink).await;
    }
    let wants_state = body.get("requestNowPlaying").and_then(Json::as_bool).unwrap_or(false)
        || body.get("requestVolume").and_then(Json::as_bool).unwrap_or(false)
        || body.get("action").is_some()
        || body.get("SetPosition").is_some()
        || body.get("Seek").is_some();
    if wants_state && let Some(packet) = state_packet(conn, name).await {
        let _ = sink.sender.send(packet);
    }
}

/// `SetPosition` takes the current track id and an absolute position in microseconds.
async fn set_position(player: &Proxy<'_>, position_ms: i64) {
    let Ok(metadata) = player.get_property::<OwnedValue>("Metadata").await else { return };
    let Value::Dict(dict) = &*metadata else { return };
    let track = dict.iter().find(|(key, _)| text(key).as_deref() == Some("mpris:trackid")).and_then(|(_, v)| text(v));
    if let Some(track) = track
        && let Ok(path) = zbus::zvariant::ObjectPath::try_from(track.as_str())
    {
        let _ = player.call_method("SetPosition", &(path, position_ms.max(0) * 1000)).await;
    }
}

/// Local art (`file://`) goes to the phone as a payload, like `AlbumArtCache` expects.
async fn send_album_art(name: &str, url: &str, sink: &Sink) {
    let Some(path) = url.strip_prefix("file://").map(|p| glide_sdk::chooser::uri_path(&format!("file://{p}"))) else {
        return; // http(s) art the phone downloads itself
    };
    let Some(path) = path else { return };
    let Ok(meta) = tokio::fs::metadata(&path).await else { return };
    if !meta.is_file() || meta.len() > MAX_ART_BYTES {
        return;
    }
    let Ok(file) = tokio::fs::File::open(&path).await else { return };
    let Ok(listener) = crate::payload::PayloadListener::bind(&sink.cert, sink.fingerprint.clone()).await else {
        return;
    };
    let mut packet = NetworkPacket::new(
        PACKET_TYPE_MPRIS,
        json!({ "player": name, "transferringAlbumArt": true, "albumArtUrl": url }),
    );
    crate::payload::attach(&mut packet, meta.len(), listener.port());
    if sink.sender.send(packet).is_ok() {
        listener.serve(file, meta.len(), crate::payload::ACCEPT_TIMEOUT).await;
    }
}

/// Follows players appearing, leaving and changing; `on_packet` gets what to send.
pub async fn watch(conn: Connection, on_packet: impl Fn(NetworkPacket) + Send + Sync + 'static) {
    let owners = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus")
        .and_then(|b| b.member("NameOwnerChanged"))
        .and_then(|b| b.arg0ns("org.mpris.MediaPlayer2"))
        .map(zbus::match_rule::Builder::build);
    let changes = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")
        .and_then(|b| b.member("PropertiesChanged"))
        .and_then(|b| b.path(PATH))
        .map(zbus::match_rule::Builder::build);
    let (Ok(owners), Ok(changes)) = (owners, changes) else { return };
    let (Ok(owners), Ok(changes)) = (
        MessageStream::for_match_rule(owners, &conn, Some(64)).await,
        MessageStream::for_match_rule(changes, &conn, Some(256)).await,
    ) else {
        return;
    };
    refresh(&conn).await;
    let mut events = futures_util::stream::select(owners.map(|m| (true, m)), changes.map(|m| (false, m)));
    while let Some((owner_change, message)) = events.next().await {
        let Ok(message) = message else { continue };
        if owner_change {
            on_packet(list_packet(&refresh(&conn).await));
            continue;
        }
        let sender = message.header().sender().map(ToString::to_string);
        if let Some(sender) = sender
            && let Some(name) = name_of_owner(&conn, &sender).await
            && let Some(packet) = state_packet(&conn, &name).await
        {
            on_packet(packet);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_values_are_read_like_kde_connect() {
        let artists = Value::from(vec!["A", "B"]);
        assert_eq!(text(&artists).as_deref(), Some("A, B"));
        assert_eq!(number(&Value::U64(180_000_000)), Some(180_000_000));
        assert_eq!(number(&Value::Value(Box::new(Value::I64(5)))), Some(5));
        let list = list_packet(&["VLC".into()]);
        assert_eq!(list.body["playerList"][0], "VLC");
        assert_eq!(list.body["supportAlbumArtPayload"], true);
    }
}
