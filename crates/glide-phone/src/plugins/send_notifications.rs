//! Forwards this computer's notifications to the phone (KDE Connect's `SendNotifications`):
//! a monitor connection sees every `org.freedesktop.Notifications.Notify` call.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt as _;
use serde_json::json;
use zbus::zvariant::OwnedValue;

use crate::protocol::packet::{NetworkPacket, PACKET_TYPE_NOTIFICATION};

/// Apps whose notifications never go to the phone: ours (they came from the phone).
const OWN_APPS: [&str; 3] = ["glide-phone", "Phone Link", "Підключення телефону"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopNotification {
    pub app: String,
    pub summary: String,
    pub body: String,
    pub icon: Option<std::path::PathBuf>,
    pub transient: bool,
}

/// The `Notify(susssasa{sv}i)` arguments we care about.
type NotifyArgs = (String, u32, String, String, String, Vec<String>, HashMap<String, OwnedValue>, i32);

#[must_use]
pub fn from_args(args: &NotifyArgs) -> DesktopNotification {
    let (app, _replaces, app_icon, summary, body, _actions, hints, _timeout) = args;
    let hint_text = |key: &str| hints.get(key).and_then(|v| <&str>::try_from(&**v).ok().map(str::to_string));
    let transient = hints.get("transient").is_some_and(|v| bool::try_from(&**v).unwrap_or(false));
    let icon = hint_text("image-path")
        .or_else(|| hint_text("image_path"))
        .or_else(|| (!app_icon.is_empty()).then(|| app_icon.clone()))
        .map(|path| std::path::PathBuf::from(path.strip_prefix("file://").unwrap_or(&path).to_string()))
        .filter(|path| path.is_absolute() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")));
    DesktopNotification { app: app.clone(), summary: summary.clone(), body: body.clone(), icon, transient }
}

/// Whether a notification goes to the phone.
#[must_use]
pub fn forwardable(n: &DesktopNotification) -> bool {
    !n.transient && !OWN_APPS.contains(&n.app.as_str()) && (!n.summary.trim().is_empty() || !n.body.trim().is_empty())
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A `kdeconnect.notification` for the phone's "receive notifications" plugin.
#[must_use]
pub fn packet(n: &DesktopNotification) -> NetworkPacket {
    let id = format!("glide-desktop-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let ticker = match (n.summary.trim().is_empty(), n.body.trim().is_empty()) {
        (false, false) => format!("{}: {}", n.summary, n.body),
        (false, true) => n.summary.clone(),
        _ => n.body.clone(),
    };
    NetworkPacket::new(
        PACKET_TYPE_NOTIFICATION,
        json!({
            "id": id,
            "appName": n.app,
            "title": n.summary,
            "text": n.body,
            "ticker": ticker,
            "isClearable": true,
        }),
    )
}

/// Watches notifications on the session bus and hands forwardable ones to `on_notification`.
pub async fn watch(on_notification: impl Fn(DesktopNotification) + Send + 'static) {
    watch_bus(None, on_notification).await;
}

/// [`watch`] on the bus at `address` (the session bus when `None`).
pub async fn watch_bus(address: Option<String>, on_notification: impl Fn(DesktopNotification) + Send + 'static) {
    let conn = match address {
        Some(address) => match zbus::connection::Builder::address(address.as_str()) {
            Ok(builder) => builder.build().await,
            Err(err) => Err(err),
        },
        None => zbus::Connection::session().await,
    };
    let Ok(conn) = conn else { return };
    let rule = "type='method_call',interface='org.freedesktop.Notifications',member='Notify'";
    let monitoring =
        zbus::Proxy::new(&conn, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus.Monitoring")
            .await;
    let Ok(monitoring) = monitoring else { return };
    if let Err(err) = monitoring.call_method("BecomeMonitor", &(vec![rule], 0u32)).await {
        tracing::info!(%err, "cannot watch desktop notifications; they are not sent to the phone");
        return;
    }
    let mut stream = zbus::MessageStream::from(&conn);
    while let Some(Ok(message)) = stream.next().await {
        let header = message.header();
        if header.member().is_none_or(|m| m.as_str() != "Notify") {
            continue;
        }
        if let Ok(args) = message.body().deserialize::<NotifyArgs>() {
            let notification = from_args(&args);
            if forwardable(&notification) {
                on_notification(notification);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(app: &str, summary: &str, body: &str, transient: bool) -> NotifyArgs {
        let mut hints = HashMap::new();
        if transient {
            hints.insert("transient".to_string(), OwnedValue::from(true));
        }
        (app.into(), 0, String::new(), summary.into(), body.into(), Vec::new(), hints, -1)
    }

    #[test]
    fn our_own_and_transient_notifications_stay_here() {
        assert!(forwardable(&from_args(&args("Firefox", "Download", "done", false))));
        assert!(!forwardable(&from_args(&args("glide-phone", "From phone", "x", false))));
        assert!(!forwardable(&from_args(&args("Volume", "50%", "", true))));
        assert!(!forwardable(&from_args(&args("App", " ", "", false))));
    }

    #[test]
    fn the_packet_reads_like_kde_connects() {
        let p = packet(&from_args(&args("Firefox", "Download", "done", false)));
        assert_eq!(p.packet_type, "kdeconnect.notification");
        assert_eq!(p.body["appName"], "Firefox");
        assert_eq!(p.body["ticker"], "Download: done");
        assert!(p.body["id"].as_str().unwrap().starts_with("glide-desktop-"));
    }
}
