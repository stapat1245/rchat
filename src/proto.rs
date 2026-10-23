//! The on-wire chat protocol: newline-delimited JSON, one object per line.
//!
//! TCP gives us ordering and reliability for free, so the protocol stays tiny.
//! Everything is a `Wire` value serialized to a single line.

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// A frame on the wire. `t` is the discriminator tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Wire {
    /// Sent immediately on connect so each side learns the peer's overlay IP
    /// (the mesh identity) and nickname.
    Hello {
        from: Ipv4Addr,
        nick: String,
        ver: u32,
    },
    /// A chat message.
    Msg {
        id: u64,
        ts: i64,
        nick: String,
        body: String,
    },
    /// Keepalive so a dead peer is detected quickly even when idle.
    Ping,
    /// Clean shutdown notice (best effort).
    Bye,
}

pub const PROTO_VER: u32 = 1;

impl Wire {
    /// Serialize to a single newline-terminated JSON line.
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        s
    }

    /// Parse one line (without the trailing newline).
    pub fn from_line(line: &str) -> Option<Wire> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        serde_json::from_str(line).ok()
    }
}
