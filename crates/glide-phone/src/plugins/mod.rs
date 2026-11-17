pub mod battery;
pub mod clipboard;
pub mod connectivity;
pub mod find_device;
pub mod mousepad;
pub mod mpris;
pub mod notifications;
pub mod presenter;
pub mod remote_media;
pub mod runcommand;
pub mod send_notifications;
pub mod sftp;
pub mod share;
pub mod systemvolume;
pub mod telephony;

pub use battery::{BatteryPayload, BatteryState, handle_battery_packet, local_battery_packet};
pub use clipboard::{ClipboardPayload, create_clipboard_packet, handle_clipboard_packet};
pub use find_device::{create_ring_packet, handle_find_this_device};
pub use notifications::{
    ActiveNotification, NotificationEvent, NotificationPayload, create_action_packet, create_reply_packet,
    create_request_packet, handle_notification_packet,
};
pub use presenter::{PresenterEvent, handle_presenter_packet};
pub use share::{SharePayload, create_share_text_packet, create_share_url_packet, handle_share_packet};

pub(crate) fn announce(notification: glide_sdk::system::Notification) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = glide_sdk::system::notify(notification).await;
        });
    }
}
