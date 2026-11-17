//! Sending payloads (files, album art, icons) the KDE Connect way: the sender listens on a
//! port of 1739–1764 as the TLS server, announces it in `payloadTransferInfo`, and the
//! receiver connects as the TLS client presenting its pinned certificate.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::crypto::cert::IdentityCert;
use crate::crypto::tls::make_server_config;
use crate::protocol::packet::NetworkPacket;

pub const MIN_PAYLOAD_PORT: u16 = 1739;
pub const MAX_PAYLOAD_PORT: u16 = 1764;
/// How long the receiver has to connect once the packet went out.
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_mins(1);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A listening payload slot; drop it to give the port back.
pub struct PayloadListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
}

impl PayloadListener {
    /// Binds the first free port of the payload range, accepting only `peer_fingerprint`.
    pub async fn bind(cert: &IdentityCert, peer_fingerprint: Option<String>) -> std::io::Result<Self> {
        let config = make_server_config(cert, peer_fingerprint).map_err(std::io::Error::other)?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let mut last = None;
        for port in MIN_PAYLOAD_PORT..=MAX_PAYLOAD_PORT {
            match TcpListener::bind(("0.0.0.0", port)).await {
                Ok(listener) => return Ok(Self { listener, acceptor }),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no payload port")))
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.listener.local_addr().map_or(0, |addr| addr.port())
    }

    /// Waits for the receiver and streams `size` bytes of `source`; true when all were sent.
    pub async fn serve(self, mut source: impl AsyncRead + Unpin, size: u64, accept_timeout: Duration) -> bool {
        // Anyone on the network can connect first; keep accepting until the pinned receiver
        // completes the TLS handshake, so a stranger can't take the slot.
        let deadline = tokio::time::Instant::now() + accept_timeout;
        let mut tls = loop {
            let Ok(Ok((stream, _))) = tokio::time::timeout_at(deadline, self.listener.accept()).await else {
                return false;
            };
            let handshake = deadline.min(tokio::time::Instant::now() + HANDSHAKE_TIMEOUT);
            if let Ok(Ok(tls)) = tokio::time::timeout_at(handshake, self.acceptor.accept(stream)).await {
                break tls;
            }
        };
        drop(self.listener);
        let mut sent = 0u64;
        let mut chunk = vec![0u8; 64 * 1024];
        while sent < size {
            let want = usize::try_from((size - sent).min(chunk.len() as u64)).unwrap_or(chunk.len());
            let Ok(Ok(read)) = tokio::time::timeout(IDLE_TIMEOUT, source.read(&mut chunk[..want])).await else {
                return false;
            };
            if read == 0 {
                break;
            }
            if tokio::time::timeout(IDLE_TIMEOUT, tls.write_all(&chunk[..read])).await.map_or(true, |r| r.is_err()) {
                return false;
            }
            sent += read as u64;
        }
        let _ = tokio::time::timeout(IDLE_TIMEOUT, tls.flush()).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), tls.shutdown()).await;
        sent == size
    }
}

/// Attaches the payload fields KDE Connect expects to `packet`.
pub fn attach(packet: &mut NetworkPacket, size: u64, port: u16) {
    packet.payload_size = i64::try_from(size).ok();
    packet.payload_transfer_info = Some(json!({ "port": port }));
}

/// `kdeconnect.share.request` announcing one file of a batch (`SharePlugin`'s format).
#[must_use]
pub fn share_file_packet(path: &Path, number_of_files: usize, total_size: u64) -> NetworkPacket {
    let filename = path.file_name().map_or_else(|| "file".to_string(), |name| name.to_string_lossy().into_owned());
    let mut body = json!({
        "filename": filename,
        "numberOfFiles": number_of_files,
        "totalPayloadSize": total_size,
    });
    if let Some(modified) = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        && let Value::Object(map) = &mut body
    {
        map.insert("lastModified".into(), json!(modified.as_millis() as u64));
    }
    NetworkPacket::new(crate::protocol::packet::PACKET_TYPE_SHARE_REQUEST, body)
}
