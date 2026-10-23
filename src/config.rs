//! Discovery of our own rnet overlay IP and the set of mesh peers.
//!
//! rnet assigns every node an address on `10.42.0.0/24` via `mynet0`, and
//! records every trusted member in `~/.config/mynet/peers.toml`. rchat reads
//! that file to learn the full group, so it scales to the whole mesh (up to
//! 254 nodes) rather than a single peer.

use anyhow::{bail, Context, Result};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::Command;

/// Who we are on the overlay.
#[derive(Debug, Clone)]
pub struct Session {
    pub nick: String,
    pub iface: String,
    pub own_ip: Ipv4Addr,
    pub port: u16,
    /// When set, overrides on-disk discovery (used by tests).
    pub peers_override: Option<Vec<Ipv4Addr>>,
}

impl Session {
    pub fn discover(nick: Option<String>, iface: &str, port: u16) -> Result<Self> {
        let own_ip = find_overlay_ip(iface)
            .with_context(|| format!("could not find the rnet overlay IP on `{iface}`"))?;
        let nick = nick
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(default_nick);
        Ok(Session {
            nick,
            iface: iface.to_string(),
            own_ip,
            port,
            peers_override: None,
        })
    }

    /// Current set of mesh peers (excluding ourselves). Re-read on demand so new
    /// members that join the rnet network are picked up live.
    pub fn peer_ips(&self) -> Vec<Ipv4Addr> {
        if let Some(over) = &self.peers_override {
            return over.clone();
        }
        let mut v = read_peer_ips();
        // Fallback for the classic 2-peer case before peers.toml is populated.
        if v.is_empty() {
            if let Some(other) = classic_other(self.own_ip) {
                v.push(other);
            }
        }
        v.retain(|ip| *ip != self.own_ip);
        v.sort();
        v.dedup();
        v
    }
}

/// Default display name: $USER (or $USERNAME), falling back to "anon".
pub fn default_nick() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "anon".to_string())
}

/// Find the IPv4 address rnet assigned to the overlay interface.
/// Primary: `ip -4 -o addr show dev <iface>`. Fallback: rnet's `local.toml`.
pub fn find_overlay_ip(iface: &str) -> Result<Ipv4Addr> {
    if let Some(ip) = ip_from_iface(iface) {
        return Ok(ip);
    }
    if let Some(ip) = ip_from_rnet_local() {
        return Ok(ip);
    }
    bail!(
        "rnet overlay not found.\n\
         Interface `{iface}` has no IPv4 address and `~/.config/mynet/local.toml` \
         has no virtual_ip.\n\
         Start the overlay first:  sudo rnet start   (or)   sudo rnet connect <secret>"
    )
}

fn ip_from_iface(iface: &str) -> Option<Ipv4Addr> {
    let out = Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", iface])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for tok in text.split_whitespace() {
        if let Some(addr) = tok.strip_suffix("/24").or_else(|| tok.split('/').next()) {
            if let Ok(ip) = addr.parse::<Ipv4Addr>() {
                return Some(ip);
            }
        }
    }
    None
}

/// Read our own virtual_ip from rnet's local.toml (fallback path).
fn ip_from_rnet_local() -> Option<Ipv4Addr> {
    for path in mynet_file_candidates("local.toml") {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(ip) = first_virtual_ip(&text) {
            return Some(ip);
        }
    }
    None
}

/// Read every peer `virtual_ip` from rnet's peers.toml.
fn read_peer_ips() -> Vec<Ipv4Addr> {
    for path in mynet_file_candidates("peers.toml") {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let ips = all_virtual_ips(&text);
        if !ips.is_empty() {
            return ips;
        }
    }
    Vec::new()
}

fn first_virtual_ip(toml: &str) -> Option<Ipv4Addr> {
    all_virtual_ips(toml).into_iter().next()
}

/// Minimal line parser: pull every `virtual_ip = "x.x.x.x"` value.
fn all_virtual_ips(toml: &str) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    for line in toml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("virtual_ip") {
            if let Some(q) = rest.split('"').nth(1) {
                if let Ok(ip) = q.parse::<Ipv4Addr>() {
                    out.push(ip);
                }
            }
        }
    }
    out
}

/// Candidate paths for an rnet config file, honoring the sudo rule.
fn mynet_file_candidates(name: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if !sudo_user.is_empty() {
            v.push(PathBuf::from(format!("/home/{sudo_user}/.config/mynet/{name}")));
        }
    }
    if let Some(cfg) = dirs::config_dir() {
        v.push(cfg.join("mynet").join(name));
    }
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".config/mynet").join(name));
    }
    v
}

/// Classic 2-peer fallback: the other of `.1` / `.2`.
fn classic_other(own: Ipv4Addr) -> Option<Ipv4Addr> {
    let o = own.octets();
    let peer_host = match o[3] {
        1 => 2,
        2 => 1,
        _ => return None,
    };
    Some(Ipv4Addr::new(o[0], o[1], o[2], peer_host))
}
