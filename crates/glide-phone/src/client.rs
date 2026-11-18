use zbus::proxy;

#[proxy(interface = "org.glide.Phone", default_service = "org.glide.Phone", default_path = "/org/glide/Phone")]
pub trait PhoneDaemon {
    fn list_devices_json(&self) -> zbus::Result<String>;
    fn ring_phone(&self, device_id: &str) -> zbus::Result<bool>;
    fn stop_alarm(&self, device_id: &str) -> zbus::Result<bool>;
    fn accept_pair(&self, device_id: &str) -> zbus::Result<bool>;
    fn reject_pair(&self, device_id: &str) -> zbus::Result<bool>;
    fn dismiss_notification(&self, device_id: &str, notif_id: &str) -> zbus::Result<bool>;
    fn set_clipboard(&self, device_id: &str, content: &str) -> zbus::Result<bool>;
    fn clipboard_text(&self) -> zbus::Result<String>;
    fn share_text(&self, device_id: &str, text: &str) -> zbus::Result<bool>;
    fn share_url(&self, device_id: &str, url: &str) -> zbus::Result<bool>;
    fn share_files(&self, device_id: &str, paths: Vec<String>) -> zbus::Result<bool>;
    fn list_commands_json(&self) -> zbus::Result<String>;
    fn add_command(&self, name: &str, command: &str) -> zbus::Result<bool>;
    fn remove_command(&self, key: &str) -> zbus::Result<bool>;
    fn take_commands_setup(&self) -> zbus::Result<bool>;
    fn list_phone_media_json(&self) -> zbus::Result<String>;
    fn list_phone_volumes_json(&self) -> zbus::Result<String>;
    fn computer_files_allowed(&self) -> zbus::Result<bool>;
    fn send_screenshots(&self) -> zbus::Result<bool>;
    fn list_phone_actions_json(&self) -> zbus::Result<String>;
    fn run_phone_action(&self, device_id: &str, key: &str) -> zbus::Result<bool>;
    fn set_send_screenshots(&self, enabled: bool) -> zbus::Result<bool>;
    fn set_computer_files_allowed(&self, allowed: bool) -> zbus::Result<bool>;
    fn set_phone_volume(&self, device_id: &str, name: &str, volume: i32) -> zbus::Result<bool>;
    fn set_phone_muted(&self, device_id: &str, name: &str, muted: bool) -> zbus::Result<bool>;
    fn phone_media_action(&self, device_id: &str, player: &str, action: &str) -> zbus::Result<bool>;
    fn browse_phone(&self, device_id: &str) -> zbus::Result<bool>;
    fn control_phone_input(&self, device_id: &str, body_json: &str) -> zbus::Result<bool>;
    fn ping(&self, device_id: &str) -> zbus::Result<bool>;
    fn share_input_edge(&self) -> zbus::Result<String>;
    fn set_share_input_edge(&self, edge: &str) -> zbus::Result<bool>;
    fn list_sms_json(&self) -> zbus::Result<String>;
    fn request_sms_conversations(&self, device_id: &str) -> zbus::Result<bool>;
    fn request_sms_thread(&self, device_id: &str, thread: i64) -> zbus::Result<bool>;
    fn send_sms(&self, device_id: &str, addresses: Vec<String>, text: &str) -> zbus::Result<bool>;
    fn take_presenter_event(&self) -> zbus::Result<String>;
    fn is_ringing(&self) -> zbus::Result<bool>;
}
