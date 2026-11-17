//! `kdeconnect.sms.*`: the phone's text messages on this computer (KDE Connect's SMS app):
//! conversations, a thread's history and sending (new messages arrive as the phone's SMS app
//! notification, with its reply action).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::protocol::packet::NetworkPacket;

pub const PACKET_TYPE_SMS_MESSAGES: &str = "kdeconnect.sms.messages";
pub const PACKET_TYPE_SMS_REQUEST: &str = "kdeconnect.sms.request";
pub const PACKET_TYPE_SMS_REQUEST_CONVERSATIONS: &str = "kdeconnect.sms.request_conversations";
pub const PACKET_TYPE_SMS_REQUEST_CONVERSATION: &str = "kdeconnect.sms.request_conversation";
/// `Telephony.Sms.MESSAGE_TYPE_INBOX`.
pub const TYPE_INBOX: i64 = 1;
const MAX_PER_THREAD: usize = 500;
/// Conversations kept per phone.
pub const MAX_THREADS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub uid: i64,
    pub thread: i64,
    pub date: i64,
    pub kind: i64,
    pub read: bool,
    pub body: String,
    pub addresses: Vec<String>,
    pub sub_id: i64,
}

impl Message {
    fn from_json(value: &Json) -> Option<Self> {
        let int = |key: &str| value.get(key).and_then(Json::as_i64);
        Some(Self {
            uid: int("_id")?,
            thread: int("thread_id")?,
            date: int("date").unwrap_or(0),
            kind: int("type").unwrap_or(0),
            read: int("read").unwrap_or(1) != 0,
            body: value.get("body").and_then(Json::as_str).unwrap_or_default().to_string(),
            addresses: value
                .get("addresses")
                .and_then(Json::as_array)
                .map(|a| a.iter().filter_map(|x| x.get("address")?.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            sub_id: int("sub_id").unwrap_or(-1),
        })
    }

    #[must_use]
    pub fn incoming(&self) -> bool {
        self.kind == TYPE_INBOX
    }
}

/// One phone's messages, by thread, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbox {
    pub threads: BTreeMap<i64, Vec<Message>>,
    /// Newest message date seen; later incoming ones are announced.
    pub newest: i64,
    /// Set once the conversation list arrived, so the initial sync is not announced.
    pub synced: bool,
}

impl Inbox {
    /// Merges a `kdeconnect.sms.messages` packet and returns the new incoming messages.
    pub fn merge(&mut self, body: &Json) -> Vec<Message> {
        let Some(list) = body.get("messages").and_then(Json::as_array) else { return Vec::new() };
        let mut fresh = Vec::new();
        for message in list.iter().filter_map(Message::from_json) {
            if !self.threads.contains_key(&message.thread) && self.threads.len() >= MAX_THREADS {
                // Make room by dropping the conversation that was quiet the longest.
                let stalest = self
                    .threads
                    .iter()
                    .min_by_key(|(_, messages)| messages.last().map_or(i64::MIN, |m| m.date))
                    .map(|(id, _)| *id);
                if let Some(id) = stalest {
                    self.threads.remove(&id);
                }
            }
            let thread = self.threads.entry(message.thread).or_default();
            if thread.iter().any(|m| m.uid == message.uid) {
                continue;
            }
            if self.synced && message.incoming() && message.date > self.newest {
                fresh.push(message.clone());
            }
            self.newest = self.newest.max(message.date);
            thread.push(message);
            thread.sort_by_key(|m| m.date);
            if thread.len() > MAX_PER_THREAD {
                thread.drain(..thread.len() - MAX_PER_THREAD);
            }
        }
        fresh
    }

    /// Conversations newest first: (thread, last message).
    #[must_use]
    pub fn conversations(&self) -> Vec<(i64, &Message)> {
        let mut out: Vec<(i64, &Message)> =
            self.threads.iter().filter_map(|(t, m)| m.last().map(|last| (*t, last))).collect();
        out.sort_by_key(|(_, m)| std::cmp::Reverse(m.date));
        out
    }
}

#[must_use]
pub fn request_conversations_packet() -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_SMS_REQUEST_CONVERSATIONS, json!({}))
}

#[must_use]
pub fn request_thread_packet(thread: i64, count: i64) -> NetworkPacket {
    NetworkPacket::new(PACKET_TYPE_SMS_REQUEST_CONVERSATION, json!({ "threadID": thread, "numberToRequest": count }))
}

/// `kdeconnect.sms.request` (version 2: every recipient, the SIM to send from).
#[must_use]
pub fn send_packet(addresses: &[String], text: &str, sub_id: i64) -> NetworkPacket {
    let list: Vec<Json> = addresses.iter().map(|a| json!({ "address": a })).collect();
    NetworkPacket::new(
        PACKET_TYPE_SMS_REQUEST,
        json!({ "version": 2, "addresses": list, "messageBody": text, "subID": sub_id }),
    )
}

impl Inbox {
    /// The same inbox with only each thread's newest `per_thread` messages (for the window).
    #[must_use]
    pub fn recent(&self, per_thread: usize) -> Self {
        let threads = self
            .threads
            .iter()
            .map(|(id, messages)| (*id, messages[messages.len().saturating_sub(per_thread)..].to_vec()))
            .collect();
        Self { threads, newest: self.newest, synced: self.synced }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sms(uid: i64, thread: i64, date: i64, kind: i64, body: &str) -> Json {
        json!({"_id": uid, "thread_id": thread, "date": date, "type": kind, "read": 0, "body": body,
               "addresses": [{"address": "+380501234567"}], "sub_id": 1, "event": 1})
    }

    #[test]
    fn the_first_sync_is_quiet_and_later_incoming_messages_are_new() {
        let mut inbox = Inbox::default();
        assert!(inbox.merge(&json!({"messages": [sms(1, 7, 100, 1, "привіт")], "version": 2})).is_empty());
        inbox.synced = true;
        let fresh = inbox.merge(&json!({"messages": [sms(2, 7, 200, 1, "як справи?"), sms(1, 7, 100, 1, "привіт")]}));
        assert_eq!(fresh.len(), 1, "duplicates are ignored");
        assert_eq!(fresh[0].body, "як справи?");
        assert!(
            inbox.merge(&json!({"messages": [sms(3, 7, 300, 2, "добре")]})).is_empty(),
            "sent ones aren't announced"
        );
        assert_eq!(inbox.conversations()[0].1.body, "добре");
        let send = send_packet(&["+380501234567".into()], "ок", 1);
        assert_eq!(send.body["addresses"][0]["address"], "+380501234567");
        assert_eq!(send.body["version"], 2);
    }
}
