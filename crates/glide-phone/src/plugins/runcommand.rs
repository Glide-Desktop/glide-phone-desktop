//! `kdeconnect.runcommand`: commands of this computer the phone can run (KDE Connect's
//! `RunCommand` plugin), with their output streamed back as `kdeconnect.runcommand.output`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::sync::Mutex;

use crate::protocol::packet::NetworkPacket;
use crate::state::PacketSender;

pub const PACKET_TYPE_RUNCOMMAND: &str = "kdeconnect.runcommand";
pub const PACKET_TYPE_RUNCOMMAND_REQUEST: &str = "kdeconnect.runcommand.request";
pub const PACKET_TYPE_RUNCOMMAND_OUTPUT: &str = "kdeconnect.runcommand.output";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub name: String,
    pub command: String,
}

/// The saved commands; keys are stable random ids, as KDE Connect uses.
#[derive(Debug, Default)]
pub struct Commands {
    path: Option<PathBuf>,
    pub entries: BTreeMap<String, Command>,
    /// Running children by output id, so "stop" can end them.
    running: Arc<Mutex<HashMap<u32, u32>>>,
}

static NEXT_RUN: AtomicU32 = AtomicU32::new(1);

impl Commands {
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, entries, running: Arc::default() }
    }

    fn save(&self) {
        if let Some(path) = &self.path
            && let Ok(text) = serde_json::to_string_pretty(&self.entries)
        {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(err) = glide_config::write_atomic(path, text.as_bytes(), 0o600) {
                tracing::warn!(%err, "could not save the phone commands");
            }
        }
    }

    pub fn add(&mut self, name: &str, command: &str) -> String {
        let key = format!("{:016x}", rand_u64());
        self.entries
            .insert(key.clone(), Command { name: name.trim().to_string(), command: command.trim().to_string() });
        self.save();
        key
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let removed = self.entries.remove(key).is_some();
        if removed {
            self.save();
        }
        removed
    }

    /// The list packet: `commandList` is a JSON string, as KDE Connect sends it.
    #[must_use]
    pub fn list_packet(&self) -> NetworkPacket {
        let list = serde_json::to_string(&self.entries).unwrap_or_else(|_| "{}".into());
        NetworkPacket::new(PACKET_TYPE_RUNCOMMAND, json!({ "commandList": list, "canAddCommand": true }))
    }

    /// Handles a phone's request. Returns true when it asked to set commands up here.
    pub fn handle_request(&self, packet: &NetworkPacket, sender: Option<&PacketSender>) -> bool {
        let body = &packet.body;
        if body.get("requestCommandList").and_then(serde_json::Value::as_bool).unwrap_or(false)
            && let Some(sender) = sender
        {
            let _ = sender.send(self.list_packet());
        }
        if let Some(key) = body.get("key").and_then(serde_json::Value::as_str)
            && let Some(command) = self.entries.get(key)
        {
            self.run(command.command.clone(), sender.cloned());
        }
        if body.get("stop").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            self.stop_all();
        }
        body.get("setup").and_then(serde_json::Value::as_bool).unwrap_or(false)
    }

    fn run(&self, command: String, sender: Option<PacketSender>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let running = self.running.clone();
        tokio::spawn(async move {
            let id = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
            let send = |body: serde_json::Value| {
                if let Some(sender) = &sender {
                    let _ = sender.send(NetworkPacket::new(PACKET_TYPE_RUNCOMMAND_OUTPUT, body));
                }
            };
            send(json!({ "commandStarted": true, "id": id, "command": command }));
            let child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn();
            let Ok(mut child) = child else {
                send(json!({ "commandFinished": true, "id": id, "success": false }));
                return;
            };
            if let Some(pid) = child.id() {
                running.lock().await.insert(id, pid);
            }
            let stdout = child.stdout.take().map(|out| BufReader::new(out).lines());
            let stderr = child.stderr.take().map(|err| BufReader::new(err).lines());
            tokio::join!(pump(stdout, sender.clone(), id, "stdout"), pump(stderr, sender.clone(), id, "stderr"));
            let success = child.wait().await.is_ok_and(|status| status.success());
            running.lock().await.remove(&id);
            send(json!({ "commandFinished": true, "id": id, "success": success }));
        });
    }

    /// Ends every running command (the phone's "stop" button).
    pub fn stop_all(&self) {
        let running = self.running.clone();
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        tokio::spawn(async move {
            for pid in running.lock().await.values() {
                if let Ok(pid) = i32::try_from(*pid) {
                    // SAFETY: plain kill(2) on a child process id we spawned.
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
        });
    }
}

/// Forwards each output line as its own `commandOutput` packet.
async fn pump<R: tokio::io::AsyncRead + Unpin>(
    lines: Option<tokio::io::Lines<BufReader<R>>>, sender: Option<PacketSender>, id: u32, stream: &str,
) {
    let Some(mut lines) = lines else { return };
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(sender) = &sender {
            let _ = sender.send(NetworkPacket::new(
                PACKET_TYPE_RUNCOMMAND_OUTPUT,
                json!({ "commandOutput": true, "id": id, stream: [line] }),
            ));
        }
    }
}

fn rand_u64() -> u64 {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 8];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    u64::from_le_bytes(bytes)
}
