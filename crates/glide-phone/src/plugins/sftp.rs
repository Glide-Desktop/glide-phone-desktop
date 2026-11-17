//! `kdeconnect.sftp`: browse the phone's files from this computer (KDE Connect's SFTP
//! plugin). We ask with `startBrowsing`, the phone answers with a one-time SFTP login and
//! its storage paths, and the phone is mounted with `sshfs` (or opened as `sftp://`).

use std::path::PathBuf;

use serde_json::{Value as Json, json};

use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_SFTP: &str = "kdeconnect.sftp";
pub const PACKET_TYPE_SFTP_REQUEST: &str = "kdeconnect.sftp.request";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub ip: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// Where to open first ("/" when the phone has several storages).
    pub path: String,
    /// (path, name) of each storage.
    pub storages: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Login(Login),
    Error(String),
    /// `serverRunning: false` and other state-only packets.
    Nothing,
}

#[must_use]
pub fn start_browsing_packet() -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_SFTP_REQUEST, json!({ "startBrowsing": true }))
}

#[must_use]
pub fn parse(body: &Json) -> Reply {
    if let Some(message) = body.get("errorMessage").and_then(Json::as_str) {
        return Reply::Error(message.to_string());
    }
    let text = |key: &str| body.get(key).and_then(Json::as_str).map(str::to_string);
    let (Some(ip), Some(port), Some(user), Some(password)) = (
        text("ip"),
        body.get("port").and_then(Json::as_u64).and_then(|p| u16::try_from(p).ok()),
        text("user"),
        text("password"),
    ) else {
        return Reply::Nothing;
    };
    // Only a plain IP or host name may reach the command line.
    if !ip.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-')) || user.starts_with('-') {
        return Reply::Nothing;
    }
    let paths: Vec<String> = body
        .get("multiPaths")
        .and_then(Json::as_array)
        .map(|a| a.iter().filter_map(Json::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let names: Vec<String> = body
        .get("pathNames")
        .and_then(Json::as_array)
        .map(|a| a.iter().filter_map(Json::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let storages =
        paths.into_iter().enumerate().map(|(i, p)| (p.clone(), names.get(i).cloned().unwrap_or(p))).collect();
    Reply::Login(Login { ip, port, user, password, path: text("path").unwrap_or_else(|| "/".into()), storages })
}

/// The phone's path as plain relative components; None if it climbs out (`..`) or is odd.
#[must_use]
pub fn safe_relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in std::path::Path::new(path).components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    Some(out)
}

/// `$XDG_RUNTIME_DIR/glide-phone/<device>`, where the phone is mounted.
#[must_use]
pub fn mount_point(device_id: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("glide-phone")
        .join(device_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>())
}

fn is_mounted(point: &std::path::Path) -> bool {
    std::fs::read_to_string("/proc/mounts").is_ok_and(|mounts| {
        let point = point.to_string_lossy();
        mounts.lines().any(|line| line.split_whitespace().nth(1) == Some(point.as_ref()))
    })
}

/// Mounts the phone (`sshfs`) and opens its first storage; falls back to an `sftp://` URL.
pub async fn browse(device_id: &str, device_name: &str, login: Login) {
    let point = mount_point(device_id);
    // Only plain components of the phone's path: it must never lead outside the mount.
    let Some(relative) = safe_relative(&login.path) else {
        tracing::warn!("refusing an SFTP path that leaves the phone's files");
        return;
    };
    let open_path = |base: &std::path::Path| base.join(&relative);
    if is_mounted(&point) {
        let _ = tokio::process::Command::new("fusermount3").arg("-u").arg(&point).status().await;
    }
    if std::fs::create_dir_all(&point).is_ok() && mount(&point, &login).await {
        let target = open_path(&point);
        // Open a folder only, never a file the phone serves (it could be a script).
        let target = if target.is_dir() { target } else { point.clone() };
        let _ = glide_sdk::system::open(&target.to_string_lossy(), "glide-phone");
        return;
    }
    // No sshfs: a file manager that speaks SFTP can still open it; it asks for the password.
    let url = format!("sftp://{}@{}:{}/{}", login.user, login.ip, login.port, relative.display());
    if glide_sdk::system::open(&url, "glide-phone").is_err() {
        crate::plugins::announce(
            glide_sdk::system::Notification::new(
                "glide-phone",
                crate::i18n::naming(crate::i18n::Key::BrowseFailed, device_name),
            )
            .body(crate::i18n::t(crate::i18n::Key::InstallSshfs))
            .icon("folder-remote-symbolic"),
        );
    }
}

async fn mount(point: &std::path::Path, login: &Login) -> bool {
    use tokio::io::AsyncWriteExt as _;
    let target = format!("{}@{}:/", login.user, login.ip);
    let child = tokio::process::Command::new("sshfs")
        .arg(&target)
        .arg(point)
        .args(["-p", &login.port.to_string()])
        .args([
            "-o",
            "password_stdin,StrictHostKeyChecking=no,UserKnownHostsFile=/dev/null,PreferredAuthentications=password",
            "-o",
            &format!(
                "reconnect,ServerAliveInterval=30,uid={},gid={}",
                // SAFETY: getuid/getgid cannot fail.
                unsafe { libc::getuid() },
                unsafe { libc::getgid() }
            ),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{}\n", login.password).as_bytes()).await;
    }
    child.wait().await.is_ok_and(|status| status.success()) && is_mounted(point)
}

/// Unmounts the phone when it goes away.
pub async fn unmount(device_id: &str) {
    let point = mount_point(device_id);
    if is_mounted(&point) {
        let _ = tokio::process::Command::new("fusermount3").args(["-u", "-z"]).arg(&point).status().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phones_login_is_read_and_refused_when_odd() {
        let body = json!({"ip": "192.168.1.72", "port": 1739, "user": "kdeconnect", "password": "pw",
                          "path": "/storage/emulated/0", "multiPaths": ["/storage/emulated/0"], "pathNames": ["Пам'ять"]});
        let Reply::Login(login) = parse(&body) else { panic!("login") };
        assert_eq!(login.port, 1739);
        assert_eq!(login.storages, [("/storage/emulated/0".to_string(), "Пам'ять".to_string())]);
        assert_eq!(parse(&json!({"errorMessage": "no access"})), Reply::Error("no access".into()));
        assert_eq!(parse(&json!({"serverRunning": false})), Reply::Nothing);
        let evil = json!({"ip": "1.2.3.4;rm -rf ~", "port": 22, "user": "u", "password": "p"});
        assert_eq!(parse(&evil), Reply::Nothing);
        assert_eq!(safe_relative("/storage/emulated/0"), Some(PathBuf::from("storage/emulated/0")));
        assert_eq!(safe_relative("/"), Some(PathBuf::new()));
        assert_eq!(safe_relative("/storage/../../../etc"), None);
    }
}
