//! Full-mesh networking for N peers.
//!
//! Every node listens on `:<port>` and accepts inbound connections. To avoid a
//! duplicate connection per pair, a node only *dials* peers whose overlay IP is
//! **lower** than its own; the higher-IP side always initiates. Result: exactly
//! one TCP session between each pair, forming a full mesh.
//!
//! A `Hub` holds a sender per live connection. A chat message from the UI is
//! broadcast to every connection. Inbound frames from any peer are forwarded to
//! the UI as `NetEvent`s tagged with the peer's overlay IP.

use crate::config::Session;
use crate::proto::{Wire, PROTO_VER};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Events the network layer pushes up to the UI.
#[derive(Debug, Clone)]
pub enum NetEvent {
    /// A peer's transport connected (nick not yet known).
    PeerUp(Ipv4Addr),
    /// A peer announced / updated its nickname.
    PeerNick(Ipv4Addr, String),
    /// Inbound chat message: (ts, nick, body).
    Message(i64, String, String),
    /// A peer's transport dropped.
    PeerDown(Ipv4Addr),
    /// Informational line for the chat pane.
    Info(String),
}

/// Shared connection registry.
struct Hub {
    own_ip: Ipv4Addr,
    nick: String,
    ev_tx: Sender<NetEvent>,
    conns: Mutex<HashMap<Ipv4Addr, Sender<Wire>>>,
    dialing: Mutex<HashSet<Ipv4Addr>>,
}

impl Hub {
    fn broadcast(&self, w: Wire) {
        let map = self.conns.lock().unwrap();
        for tx in map.values() {
            let _ = tx.send(w.clone());
        }
    }

    /// Register a connection keyed by the peer's declared overlay IP.
    /// Returns false if one already exists (dedup races).
    fn register(&self, ip: Ipv4Addr, tx: Sender<Wire>) -> bool {
        let mut map = self.conns.lock().unwrap();
        if map.contains_key(&ip) {
            return false;
        }
        map.insert(ip, tx);
        true
    }

    fn unregister(&self, ip: Ipv4Addr) {
        self.conns.lock().unwrap().remove(&ip);
    }

    fn is_connected(&self, ip: Ipv4Addr) -> bool {
        self.conns.lock().unwrap().contains_key(&ip)
    }

    /// Claim a dial slot for `ip`; false if we're already dialing/connected.
    fn start_dial(&self, ip: Ipv4Addr) -> bool {
        if self.is_connected(ip) {
            return false;
        }
        self.dialing.lock().unwrap().insert(ip)
    }

    fn end_dial(&self, ip: Ipv4Addr) {
        self.dialing.lock().unwrap().remove(&ip);
    }
}

/// Handle the UI uses to talk to the network layer.
pub struct NetHandle {
    pub events: Receiver<NetEvent>,
    hub: Arc<Hub>,
}

impl NetHandle {
    /// Broadcast a chat message to all connected peers.
    pub fn send_msg(&self, id: u64, ts: i64, nick: &str, body: &str) {
        self.hub.broadcast(Wire::Msg {
            id,
            ts,
            nick: nick.to_string(),
            body: body.to_string(),
        });
    }

    /// Best-effort goodbye to everyone on quit.
    pub fn send_bye(&self) {
        self.hub.broadcast(Wire::Bye);
    }
}

/// Spawn the mesh: a listener thread plus a dialer thread.
pub fn spawn(session: Session) -> NetHandle {
    let (ev_tx, ev_rx) = mpsc::channel::<NetEvent>();
    let hub = Arc::new(Hub {
        own_ip: session.own_ip,
        nick: session.nick.clone(),
        ev_tx,
        conns: Mutex::new(HashMap::new()),
        dialing: Mutex::new(HashSet::new()),
    });

    let own_ip = session.own_ip;
    let port = session.port;

    // Listener: accept inbound connections from any peer.
    match TcpListener::bind(SocketAddrV4::new(own_ip, port)) {
        Ok(listener) => {
            let _ = hub.ev_tx.send(NetEvent::Info(format!(
                "listening on {own_ip}:{port} — mesh ready"
            )));
            let hub_l = hub.clone();
            thread::spawn(move || accept_loop(listener, hub_l));
        }
        Err(e) => {
            let _ = hub
                .ev_tx
                .send(NetEvent::Info(format!("bind failed on {own_ip}:{port}: {e}")));
        }
    }

    // Dialer: connect to peers with a lower overlay IP; refresh membership.
    let hub_d = hub.clone();
    thread::spawn(move || dialer_loop(session, own_ip, port, hub_d));

    NetHandle {
        events: ev_rx,
        hub,
    }
}

fn accept_loop(listener: TcpListener, hub: Arc<Hub>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let hub = hub.clone();
        // Inbound: identity is unknown until the peer's Hello arrives.
        thread::spawn(move || handle_conn(hub, stream, None));
    }
}

fn dialer_loop(session: Session, own_ip: Ipv4Addr, port: u16, hub: Arc<Hub>) {
    loop {
        for peer in session.peer_ips() {
            // Higher IP dials lower IP -> one connection per pair.
            if peer < own_ip && hub.start_dial(peer) {
                let hub = hub.clone();
                thread::spawn(move || {
                    let addr = SocketAddrV4::new(peer, port);
                    if let Ok(stream) =
                        TcpStream::connect_timeout(&addr.into(), Duration::from_secs(3))
                    {
                        handle_conn(hub.clone(), stream, Some(peer));
                    }
                    hub.end_dial(peer);
                });
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Own one live connection. Identity is the overlay IP the peer declares in its
/// Hello (robust to NAT/loopback source-address quirks). `_dialed` is only for
/// readability about who initiated.
fn handle_conn(hub: Arc<Hub>, stream: TcpStream, _dialed: Option<Ipv4Addr>) {
    let _ = stream.set_nodelay(true);

    let read_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };

    // Writer sends our Hello first, then serves outbound frames.
    let (tx, rx) = mpsc::channel::<Wire>();
    let hello = Wire::Hello {
        from: hub.own_ip,
        nick: hub.nick.clone(),
        ver: PROTO_VER,
    };
    let writer = thread::spawn(move || writer_loop(stream, rx, hello));

    // Read frames; the first Hello establishes identity and registers us.
    let peer_ip = reader_loop(read_stream, &hub, tx);

    if let Some(ip) = peer_ip {
        hub.unregister(ip);
        let _ = hub.ev_tx.send(NetEvent::PeerDown(ip));
    }
    let _ = writer.join();
}

fn writer_loop(mut stream: TcpStream, rx: Receiver<Wire>, hello: Wire) {
    if stream.write_all(hello.to_line().as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();
    loop {
        match rx.recv_timeout(Duration::from_secs(8)) {
            Ok(frame) => {
                let bye = matches!(frame, Wire::Bye);
                if stream.write_all(frame.to_line().as_bytes()).is_err() {
                    break;
                }
                let _ = stream.flush();
                if bye {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // Keepalive — also how we notice a dead socket while idle.
                if stream.write_all(Wire::Ping.to_line().as_bytes()).is_err() {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Read frames until EOF. Returns the peer's overlay IP once registered (so the
/// caller can unregister), or None if the peer never identified / was a dup.
fn reader_loop(stream: TcpStream, hub: &Arc<Hub>, tx: Sender<Wire>) -> Option<Ipv4Addr> {
    let reader = BufReader::new(stream);
    let mut peer_ip: Option<Ipv4Addr> = None;
    let mut tx = Some(tx);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        match Wire::from_line(&line) {
            Some(Wire::Hello { from, nick, .. }) => match peer_ip {
                None => {
                    // First Hello: claim identity and register the connection.
                    let sender = tx.take().unwrap();
                    if !hub.register(from, sender) {
                        break; // already connected via another socket; drop this
                    }
                    peer_ip = Some(from);
                    let _ = hub.ev_tx.send(NetEvent::PeerUp(from));
                    let _ = hub.ev_tx.send(NetEvent::PeerNick(from, nick));
                }
                Some(ip) => {
                    let _ = hub.ev_tx.send(NetEvent::PeerNick(ip, nick));
                }
            },
            Some(Wire::Msg { ts, nick, body, .. }) => {
                let _ = hub.ev_tx.send(NetEvent::Message(ts, nick, body));
            }
            Some(Wire::Ping) => {}
            Some(Wire::Bye) => break,
            None => {}
        }
    }
    peer_ip
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sess(own: [u8; 4], peers: Vec<[u8; 4]>, port: u16) -> Session {
        Session {
            nick: format!("n{}", own[3]),
            iface: "lo".into(),
            own_ip: Ipv4Addr::from(own),
            port,
            peers_override: Some(peers.into_iter().map(Ipv4Addr::from).collect()),
        }
    }

    /// Three loopback nodes (127.0.0.1/.2/.3) form a mesh; a message from the
    /// highest fans out to the other two.
    #[test]
    fn mesh_broadcast_three() {
        let port = 48231;
        let a = spawn(sess([127, 0, 0, 1], vec![], port)); // lowest: dials nobody
        let b = spawn(sess([127, 0, 0, 2], vec![[127, 0, 0, 1]], port));
        let c = spawn(sess([127, 0, 0, 3], vec![[127, 0, 0, 1], [127, 0, 0, 2]], port));

        // Let the mesh converge: a has 2 peers up, b has 2, c has 2.
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut up = [0usize; 3];
        while Instant::now() < deadline && up.iter().any(|&n| n < 2) {
            for (i, h) in [&a, &b, &c].iter().enumerate() {
                if let Ok(ev) = h.events.recv_timeout(Duration::from_millis(100)) {
                    if let NetEvent::PeerUp(_) = ev {
                        up[i] += 1;
                    }
                }
            }
        }
        assert!(
            up.iter().all(|&n| n >= 2),
            "each node should see 2 peers up, got {up:?}"
        );

        // c broadcasts; a and b must both receive it.
        c.send_msg(1, 777, "n3", "hi mesh");
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut a_got, mut b_got) = (false, false);
        while Instant::now() < deadline && !(a_got && b_got) {
            if let Ok(NetEvent::Message(_, nick, body)) =
                a.events.recv_timeout(Duration::from_millis(100))
            {
                assert_eq!((nick.as_str(), body.as_str()), ("n3", "hi mesh"));
                a_got = true;
            }
            if let Ok(NetEvent::Message(_, nick, body)) =
                b.events.recv_timeout(Duration::from_millis(100))
            {
                assert_eq!((nick.as_str(), body.as_str()), ("n3", "hi mesh"));
                b_got = true;
            }
        }
        assert!(a_got && b_got, "broadcast should reach both peers");
    }
}
