//! Chat history logging.
//!
//! Every message (sent or received) and notable session event is appended as a
//! single JSON object per line (JSON Lines / `.jsonl`) under a hidden config
//! folder in $HOME, so the files read like logs but stay machine-parseable.
//! Because the chat is now a group mesh, history is a single shared log.
//!
//!   ~/.config/rchat/history/group.jsonl          (rolling group history)
//!   ~/.config/rchat/history/session-<ts>.jsonl   (this run only)

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// unix seconds
    pub ts: i64,
    /// human ISO-8601 local time, for eyeballing the log
    pub iso: String,
    /// "in" (received), "out" (sent), or "event"
    pub dir: String,
    pub nick: String,
    pub body: String,
}

/// Thread-safe appender that writes to both the per-peer and per-session files.
pub struct History {
    peer_file: Mutex<File>,
    session_file: Mutex<File>,
    #[allow(dead_code)]
    pub dir: PathBuf,
}

impl History {
    pub fn open(started_ts: i64) -> Result<History> {
        let dir = base_dir().join("history");
        fs::create_dir_all(&dir)?;

        let peer_path = dir.join("group.jsonl");
        let session_path = dir.join(format!("session-{started_ts}.jsonl"));

        let peer_file = OpenOptions::new().create(true).append(true).open(&peer_path)?;
        let session_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&session_path)?;

        Ok(History {
            peer_file: Mutex::new(peer_file),
            session_file: Mutex::new(session_file),
            dir,
        })
    }

    pub fn append(&self, dir: &str, nick: &str, body: &str, ts: i64) {
        let rec = Record {
            ts,
            iso: iso_local(ts),
            dir: dir.to_string(),
            nick: nick.to_string(),
            body: body.to_string(),
        };
        let line = match serde_json::to_string(&rec) {
            Ok(mut s) => {
                s.push('\n');
                s
            }
            Err(_) => return,
        };
        if let Ok(mut f) = self.peer_file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
        if let Ok(mut f) = self.session_file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Load recent group records so a restart shows prior context.
    pub fn load_recent(max: usize) -> Vec<Record> {
        let path = base_dir().join("history").join("group.jsonl");
        let Ok(text) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        let mut recs: Vec<Record> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Record>(l).ok())
            .collect();
        if recs.len() > max {
            recs.drain(0..recs.len() - max);
        }
        recs
    }
}

/// `~/.config/rchat` (honors the invoking user under sudo).
pub fn base_dir() -> PathBuf {
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if !sudo_user.is_empty() {
            return PathBuf::from(format!("/home/{sudo_user}/.config/rchat"));
        }
    }
    if let Some(cfg) = dirs::config_dir() {
        return cfg.join("rchat");
    }
    PathBuf::from(".rchat")
}

fn iso_local(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%Y-%m-%dT%H:%M:%S%z").to_string(),
        None => String::new(),
    }
}
