//! `GKSession`: GameKit's local multiplayer (Bluetooth / Wi-Fi peer-to-peer),
//! over the host's network, so copies of the port can play Local Match
//! together on one computer, a local network, or (with `--gk-peer=HOST`) the
//! internet.
//!
//! Adapted from the user's touchHLE fork (src/frameworks/game_kit/
//! gk_session.rs, MPL-2.0) and speaking the same protocol, so a port window
//! and a touchHLE window can find and join each other too:
//! - Discovery: an advertising session (server or peer mode) announces itself
//!   once a second over UDP to every port in DISCOVERY_PORTS, on this
//!   computer, as a broadcast on the local network, and to every `--gk-peer=`
//!   host: "touchHLE-GKSession-1\tsession\tpeer\tmode\ttcp port\tname". Each
//!   session listens on the first free port in that range. A browsing session
//!   (client or peer mode) reports announced peers as available, and as
//!   unavailable once their announcements stop.
//! - Connections and data: TCP, frames `[u32 LE length][u8 type][payload]`
//!   (Hello, Accept, Deny, Data, Bye). Both data modes are sent reliably.
//!
//! The sockets are polled from the run loop (`pump`), so delegate and data
//! handler callbacks happen on the game's thread, as on iOS.

use crate::objc::{self, Msg, NativeImp, Obj};
use crate::{Cpu, Host};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// UDP ports used for discovery; each session binds the first free one.
const DISCOVERY_PORTS: std::ops::RangeInclusive<u16> = 41794..=41801;
const ANNOUNCE_EVERY: Duration = Duration::from_millis(1000);
/// A peer that hasn't announced itself for this long becomes unavailable.
const PEER_EXPIRY: Duration = Duration::from_millis(3500);
const ANNOUNCE_MAGIC: &str = "touchHLE-GKSession-1";

// GKSessionMode
const MODE_SERVER: u32 = 0;
const MODE_CLIENT: u32 = 1;
const MODE_PEER: u32 = 2;

// GKPeerConnectionState
const STATE_AVAILABLE: u32 = 0;
const STATE_UNAVAILABLE: u32 = 1;
const STATE_CONNECTED: u32 = 2;
const STATE_DISCONNECTED: u32 = 3;

/// TCP frame types.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Frame {
    /// Connection request: "peer ID \0 display name \0 session ID".
    Hello = 1,
    /// Request accepted: "peer ID \0 display name".
    Accept = 2,
    Deny = 3,
    Data = 4,
    Bye = 5,
    /// The port's own: each window's outfit colours (recolor.rs). Never shown
    /// to the game; touchHLE skips frame types it doesn't know.
    Colors = 6,
}

impl Frame {
    fn from_u8(v: u8) -> Option<Frame> {
        Some(match v {
            1 => Frame::Hello,
            2 => Frame::Accept,
            3 => Frame::Deny,
            4 => Frame::Data,
            5 => Frame::Bye,
            6 => Frame::Colors,
            _ => return None,
        })
    }
}

fn encode(kind: Frame, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&(payload.len() as u32 + 1).to_le_bytes());
    out.push(kind as u8);
    out.extend_from_slice(payload);
    out
}

/// Takes one complete frame off the front of `buf`, if there is one.
fn decode(buf: &mut Vec<u8>) -> Option<(Option<Frame>, Vec<u8>)> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    if len == 0 || buf.len() < 4 + len {
        return None;
    }
    let frame: Vec<u8> = buf.drain(..4 + len).skip(4).collect();
    Some((Frame::from_u8(frame[0]), frame[1..].to_vec()))
}

fn split_nul(payload: &[u8]) -> Vec<String> {
    payload.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
}

/// A TCP connection with read and write buffers.
struct Conn {
    stream: TcpStream,
    rx: Vec<u8>,
    tx: Vec<u8>,
}

impl Conn {
    fn new(stream: TcpStream) -> Conn {
        let _ = stream.set_nonblocking(true);
        let _ = stream.set_nodelay(true);
        Conn { stream, rx: Vec::new(), tx: Vec::new() }
    }
    fn send(&mut self, kind: Frame, payload: &[u8]) {
        self.tx.extend_from_slice(&encode(kind, payload));
        self.flush();
    }
    /// Writes as much queued output as possible. False if the connection failed.
    fn flush(&mut self) -> bool {
        while !self.tx.is_empty() {
            match self.stream.write(&self.tx) {
                Ok(0) => return false,
                Ok(n) => {
                    self.tx.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            }
        }
        true
    }
    /// Reads whatever has arrived. False if the connection closed.
    fn fill(&mut self) -> bool {
        let mut buf = [0u8; 16384];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => return false,
                Ok(n) => self.rx.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            }
        }
    }
}

struct Peer {
    name: String,
    /// Where its TCP listener is, if it announced itself.
    tcp_addr: Option<SocketAddr>,
    last_seen: Instant,
    /// Reported to the delegate as available (and not yet unavailable).
    available: bool,
    conn: Option<Conn>,
    connected: bool,
    /// We asked to connect and are waiting for an answer.
    outgoing: bool,
    /// It asked to connect and the game hasn't answered yet.
    incoming: bool,
}

impl Peer {
    fn new(name: String) -> Peer {
        Peer {
            name,
            tcp_addr: None,
            last_seen: Instant::now(),
            available: false,
            conn: None,
            connected: false,
            outgoing: false,
            incoming: false,
        }
    }
}

enum Event {
    StateChanged(String, u32),
    ConnectionRequest(String),
    ConnectionFailed(String),
    Data(String, Vec<u8>),
}

struct Session {
    session_id: String,
    display_name: String,
    peer_id: String,
    mode: u32,
    available: bool,
    /// Not retained, like UIKit/GameKit delegates.
    delegate: u32,
    handler: u32,
    context: u32,
    disconnect_timeout: f64,
    udp: Option<UdpSocket>,
    listener: Option<TcpListener>,
    tcp_port: u16,
    last_announce: Option<Instant>,
    peers: HashMap<String, Peer>,
    /// Incoming connections that haven't said who they are yet.
    unidentified: Vec<Conn>,
    /// Events to report on the next poll.
    queued: Vec<Event>,
    in_callback: bool,
}

/// Live sessions by object address.
static SESSIONS: Mutex<Option<HashMap<u32, Session>>> = Mutex::new(None);
/// `--gk-peer=HOST` hosts, announced to besides this computer and the LAN.
static EXTRA_HOSTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Reads `--gk-peer=HOST` options.
pub fn parse_args(args: &[String]) {
    let hosts: Vec<String> = args.iter().filter_map(|a| a.strip_prefix("--gk-peer=").map(String::from)).collect();
    *EXTRA_HOSTS.lock().unwrap() = hosts;
}

fn with<R>(o: u32, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    let mut g = SESSIONS.lock().unwrap();
    g.get_or_insert_with(HashMap::new).get_mut(&o).map(f)
}

fn new_peer_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    // A positive 31-bit decimal number, like GameKit's peer IDs.
    let mixed = (nanos as u64) ^ ((std::process::id() as u64) << 21);
    format!("{}", (mixed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33) as u32 | 1)
}

fn arg_string(h: &Host, c: &Cpu, m: &Msg, i: u32) -> String {
    objc::string_of(h, m.arg(h, c, i)).unwrap_or_default()
}

fn new_str(h: &mut Host, s: &str) -> u32 {
    objc::new_string(h, s.to_string())
}

fn data_bytes(h: &Host, o: u32) -> Vec<u8> {
    match h.objc.objs.get(&o) {
        Some(Obj::Data { ptr, len }) => (0..*len).map(|k| h.rd8(ptr + k)).collect(),
        _ => Vec::new(),
    }
}

/// The NSStrings in an NSArray (or a single NSString).
fn strings_in(h: &Host, o: u32) -> Vec<String> {
    match h.objc.objs.get(&o) {
        Some(Obj::Array(items)) => items.iter().filter_map(|&i| objc::string_of(h, i)).collect(),
        _ => objc::string_of(h, o).into_iter().collect(),
    }
}

fn write_nil(h: &Host, ptr: u32) {
    if ptr != 0 {
        h.wr32(ptr, 0);
    }
}

pub fn native_imp(class: &str, meta: bool, s: &str) -> Option<NativeImp> {
    if class != "GKSession" || meta {
        return None;
    }
    Some(match s {
        "initWithSessionID:displayName:sessionMode:" => |h, c, m| {
            let id = objc::string_of(h, m.arg(h, c, 0)).unwrap_or_else(|| "jp.co.capcom.biovsus".into());
            let name = objc::string_of(h, m.arg(h, c, 1)).unwrap_or_else(|| "RE VS".into());
            let mode = m.arg(h, c, 2);
            let listener = TcpListener::bind(("0.0.0.0", 0));
            let (listener, tcp_port) = match listener {
                Ok(l) => {
                    let _ = l.set_nonblocking(true);
                    let port = l.local_addr().map(|a| a.port()).unwrap_or(0);
                    (Some(l), port)
                }
                Err(e) => {
                    println!("GKSession: couldn't open a TCP port: {e}");
                    (None, 0)
                }
            };
            let udp = DISCOVERY_PORTS.clone().find_map(|port| UdpSocket::bind(("0.0.0.0", port)).ok());
            if let Some(u) = &udp {
                let _ = u.set_nonblocking(true);
                let _ = u.set_broadcast(true);
            } else {
                println!("GKSession: no free discovery port in {DISCOVERY_PORTS:?}");
            }
            let peer_id = new_peer_id();
            println!(
                "GKSession {:#x}: session ID {id:?}, name {name:?}, mode {mode}, peer ID {peer_id}, TCP port {tcp_port}, discovery {:?}",
                m.this,
                udp.as_ref().and_then(|u| u.local_addr().ok())
            );
            let s = Session {
                session_id: id,
                display_name: name,
                peer_id,
                mode,
                available: false,
                delegate: 0,
                handler: 0,
                context: 0,
                disconnect_timeout: 20.0,
                udp,
                listener,
                tcp_port,
                last_announce: None,
                peers: HashMap::new(),
                unidentified: Vec::new(),
                queued: Vec::new(),
                in_callback: false,
            };
            SESSIONS.lock().unwrap().get_or_insert_with(HashMap::new).insert(m.this, s);
            m.this
        },
        "dealloc" => |_, _, m| {
            if let Some(mut s) = SESSIONS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&m.this) {
                for p in s.peers.values_mut() {
                    if let Some(conn) = &mut p.conn {
                        conn.send(Frame::Bye, &[]);
                    }
                }
            }
            0
        },
        "delegate" => |_, _, m| with(m.this, |s| s.delegate).unwrap_or(0),
        "setDelegate:" => |h, c, m| {
            let d = m.arg(h, c, 0);
            with(m.this, |s| s.delegate = d);
            0
        },
        "sessionID" => |h, _, m| {
            let v = with(m.this, |s| s.session_id.clone()).unwrap_or_default();
            new_str(h, &v)
        },
        "displayName" => |h, _, m| {
            let v = with(m.this, |s| s.display_name.clone()).unwrap_or_default();
            new_str(h, &v)
        },
        "peerID" => |h, _, m| {
            let v = with(m.this, |s| s.peer_id.clone()).unwrap_or_default();
            new_str(h, &v)
        },
        "sessionMode" => |_, _, m| with(m.this, |s| s.mode).unwrap_or(MODE_PEER),
        "isAvailable" => |_, _, m| with(m.this, |s| s.available).unwrap_or(false) as u32,
        "setAvailable:" => |h, c, m| {
            let on = m.arg(h, c, 0) & 0xFF != 0;
            println!("GKSession {:#x}: setAvailable:{on}", m.this);
            with(m.this, |s| {
                s.available = on;
                s.last_announce = None; // announce right away
            });
            0
        },
        "disconnectTimeout" => |_, c, m| {
            // A double return: r0/r1.
            let t = with(m.this, |s| s.disconnect_timeout).unwrap_or(20.0);
            let bits = t.to_bits();
            c.r[1] = (bits >> 32) as u32;
            bits as u32
        },
        "setDisconnectTimeout:" => |h, c, m| {
            let bits = (m.arg(h, c, 1) as u64) << 32 | m.arg(h, c, 0) as u64;
            with(m.this, |s| s.disconnect_timeout = f64::from_bits(bits));
            0
        },
        "setDataReceiveHandler:withContext:" => |h, c, m| {
            let (handler, context) = (m.arg(h, c, 0), m.arg(h, c, 1));
            with(m.this, |s| {
                s.handler = handler;
                s.context = context;
            });
            0
        },
        "displayNameForPeer:" => |h, c, m| {
            let peer = arg_string(h, c, m, 0);
            let name = with(m.this, |s| {
                if peer == s.peer_id {
                    Some(s.display_name.clone())
                } else {
                    s.peers.get(&peer).map(|p| p.name.clone())
                }
            })
            .flatten();
            match name {
                Some(n) => new_str(h, &n),
                None => 0,
            }
        },
        "peersWithConnectionState:" => |h, c, m| {
            let state = m.arg(h, c, 0);
            let ids = with(m.this, |s| {
                s.peers
                    .iter()
                    .filter(|(_, p)| match state {
                        STATE_AVAILABLE => p.available && !p.connected,
                        STATE_CONNECTED => p.connected,
                        _ => false,
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
            let items = ids.iter().map(|i| new_str(h, i)).collect();
            objc::new_array(h, items)
        },
        "connectToPeer:withTimeout:" => |h, c, m| {
            let peer_id = arg_string(h, c, m, 0);
            // The timeout is a double in r2/r3.
            let bits = (m.arg(h, c, 2) as u64) << 32 | m.arg(h, c, 1) as u64;
            let timeout = f64::from_bits(bits);
            println!("GKSession {:#x}: connectToPeer:{peer_id} withTimeout:{timeout}", m.this);
            with(m.this, |s| {
                let hello = [s.peer_id.as_bytes(), b"\0", s.display_name.as_bytes(), b"\0", s.session_id.as_bytes()].concat();
                let Some(peer) = s.peers.get_mut(&peer_id) else {
                    s.queued.push(Event::ConnectionFailed(peer_id));
                    return;
                };
                let wait = Duration::from_secs_f64(if timeout.is_finite() { timeout.clamp(0.5, 3.0) } else { 3.0 });
                match peer.tcp_addr.map(|addr| TcpStream::connect_timeout(&addr, wait)) {
                    Some(Ok(stream)) => {
                        let mut conn = Conn::new(stream);
                        conn.send(Frame::Hello, &hello);
                        peer.conn = Some(conn);
                        peer.outgoing = true;
                    }
                    other => {
                        println!("GKSession: connecting to {peer_id} failed: {:?}", other.map(|r| r.err()));
                        s.queued.push(Event::ConnectionFailed(peer_id));
                    }
                }
            });
            0
        },
        "cancelConnectToPeer:" => |h, c, m| {
            let peer_id = arg_string(h, c, m, 0);
            with(m.this, |s| {
                if let Some(p) = s.peers.get_mut(&peer_id) {
                    if p.outgoing && !p.connected {
                        p.conn = None;
                        p.outgoing = false;
                    }
                }
            });
            0
        },
        "acceptConnectionFromPeer:error:" => |h, c, m| {
            let peer_id = arg_string(h, c, m, 0);
            write_nil(h, m.arg(h, c, 1));
            println!("GKSession {:#x}: acceptConnectionFromPeer:{peer_id}", m.this);
            with(m.this, |s| {
                let accept = [s.peer_id.as_bytes(), b"\0", s.display_name.as_bytes()].concat();
                let Some(p) = s.peers.get_mut(&peer_id) else { return false };
                let Some(conn) = &mut p.conn else { return false };
                conn.send(Frame::Accept, &accept);
                p.incoming = false;
                p.connected = true;
                s.queued.push(Event::StateChanged(peer_id, STATE_CONNECTED));
                true
            })
            .unwrap_or(false) as u32
        },
        "denyConnectionFromPeer:" => |h, c, m| {
            let peer_id = arg_string(h, c, m, 0);
            println!("GKSession {:#x}: denyConnectionFromPeer:{peer_id}", m.this);
            with(m.this, |s| {
                if let Some(p) = s.peers.get_mut(&peer_id) {
                    if let Some(conn) = &mut p.conn {
                        conn.send(Frame::Deny, &[]);
                    }
                    p.conn = None;
                    p.incoming = false;
                }
            });
            0
        },
        "sendData:toPeers:withDataMode:error:" => |h, c, m| {
            let bytes = data_bytes(h, m.arg(h, c, 0));
            let targets = strings_in(h, m.arg(h, c, 1));
            write_nil(h, m.arg(h, c, 3));
            with(m.this, |s| {
                let mut ok = true;
                for t in targets {
                    match s.peers.get_mut(&t) {
                        Some(Peer { connected: true, conn: Some(conn), .. }) => conn.send(Frame::Data, &bytes),
                        _ => ok = false,
                    }
                }
                ok
            })
            .unwrap_or(false) as u32
        },
        "sendDataToAllPeers:withDataMode:error:" => |h, c, m| {
            let bytes = data_bytes(h, m.arg(h, c, 0));
            write_nil(h, m.arg(h, c, 2));
            with(m.this, |s| {
                for p in s.peers.values_mut() {
                    if let Peer { connected: true, conn: Some(conn), .. } = p {
                        conn.send(Frame::Data, &bytes);
                    }
                }
            });
            1
        },
        "disconnectFromAllPeers" => |_, _, m| {
            println!("GKSession {:#x}: disconnectFromAllPeers", m.this);
            with(m.this, |s| {
                let mut events = Vec::new();
                for (id, p) in s.peers.iter_mut() {
                    if let Some(conn) = &mut p.conn {
                        conn.send(Frame::Bye, &[]);
                    }
                    if p.connected {
                        events.push(Event::StateChanged(id.clone(), STATE_DISCONNECTED));
                    }
                    p.conn = None;
                    p.connected = false;
                    p.outgoing = false;
                    p.incoming = false;
                }
                s.queued.extend(events);
            });
            0
        },
        "disconnectPeerFromAllPeers:" => |h, c, m| {
            let peer_id = arg_string(h, c, m, 0);
            with(m.this, |s| {
                if let Some(p) = s.peers.get_mut(&peer_id) {
                    if let Some(conn) = &mut p.conn {
                        conn.send(Frame::Bye, &[]);
                    }
                    let was = p.connected;
                    p.conn = None;
                    p.connected = false;
                    if was {
                        s.queued.push(Event::StateChanged(peer_id, STATE_DISCONNECTED));
                    }
                }
            });
            0
        },
        _ => return None,
    })
}

/// Network work for one session: announcing, discovery, accepting, reading
/// and writing. Returns the events to report to the game.
fn poll(s: &mut Session, extra_hosts: &[String]) -> Vec<Event> {
    let mut events = std::mem::take(&mut s.queued);
    let now = Instant::now();
    let advertising = s.available && s.mode != MODE_CLIENT;
    let browsing = s.available && s.mode != MODE_SERVER;

    // Announce ourselves.
    if advertising && s.last_announce.is_none_or(|t| now.duration_since(t) >= ANNOUNCE_EVERY) {
        s.last_announce = Some(now);
        if let Some(udp) = &s.udp {
            let msg = format!(
                "{ANNOUNCE_MAGIC}\t{}\t{}\t{}\t{}\t{}",
                s.session_id, s.peer_id, s.mode, s.tcp_port, s.display_name
            );
            let own = udp.local_addr().ok().map(|a| a.port());
            for port in DISCOVERY_PORTS {
                if Some(port) != own {
                    let _ = udp.send_to(msg.as_bytes(), SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
                }
                let _ = udp.send_to(msg.as_bytes(), SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), port));
                for extra in extra_hosts {
                    if let Ok(addrs) = (extra.as_str(), port).to_socket_addrs() {
                        for addr in addrs {
                            let _ = udp.send_to(msg.as_bytes(), addr);
                        }
                    }
                }
            }
        }
    }

    // Discover others.
    if let Some(udp) = &s.udp {
        let mut buf = [0u8; 1024];
        while let Ok((n, from)) = udp.recv_from(&mut buf) {
            let text = String::from_utf8_lossy(&buf[..n]);
            let parts: Vec<&str> = text.splitn(6, '\t').collect();
            let [magic, session_id, peer_id, mode, tcp_port, name] = parts[..] else { continue };
            if magic != ANNOUNCE_MAGIC || session_id != s.session_id || peer_id == s.peer_id || !browsing {
                continue;
            }
            let (Ok(mode), Ok(tcp_port)) = (mode.parse::<u32>(), tcp_port.parse::<u16>()) else { continue };
            if mode == MODE_CLIENT {
                continue;
            }
            let peer = s.peers.entry(peer_id.to_string()).or_insert_with(|| Peer::new(name.to_string()));
            peer.name = name.to_string();
            peer.tcp_addr = Some(SocketAddr::new(from.ip(), tcp_port));
            peer.last_seen = now;
            if !peer.available && !peer.connected {
                peer.available = true;
                println!("GKSession: found peer {peer_id} ({name:?}) at {}", from.ip());
                events.push(Event::StateChanged(peer_id.to_string(), STATE_AVAILABLE));
            }
        }
    }
    // Forget peers that stopped announcing.
    for (id, p) in s.peers.iter_mut() {
        if p.available && !p.connected && now.duration_since(p.last_seen) > PEER_EXPIRY {
            p.available = false;
            events.push(Event::StateChanged(id.clone(), STATE_UNAVAILABLE));
        }
    }

    // New incoming connections.
    if let Some(listener) = &s.listener {
        while let Ok((stream, _)) = listener.accept() {
            s.unidentified.push(Conn::new(stream));
        }
    }
    // Incoming connections introduce themselves with Hello.
    let mut still = Vec::new();
    for mut conn in std::mem::take(&mut s.unidentified) {
        let alive = conn.fill();
        match decode(&mut conn.rx) {
            Some((Some(Frame::Hello), payload)) => {
                let fields = split_nul(&payload);
                let peer_id = fields.first().cloned().unwrap_or_default();
                let name = fields.get(1).cloned().unwrap_or_default();
                let session_ok = fields.get(2).is_none_or(|x| *x == s.session_id);
                if !session_ok || !advertising {
                    conn.send(Frame::Deny, &[]);
                    continue;
                }
                let peer = s.peers.entry(peer_id.clone()).or_insert_with(|| Peer::new(name.clone()));
                peer.name = name;
                peer.last_seen = now;
                peer.conn = Some(conn);
                peer.incoming = true;
                events.push(Event::ConnectionRequest(peer_id));
            }
            Some(_) => {} // unexpected: drop the connection
            None if alive => still.push(conn),
            None => {}
        }
    }
    s.unidentified = still;

    // Traffic on established (or pending outgoing) connections.
    for (id, p) in s.peers.iter_mut() {
        let Some(conn) = &mut p.conn else { continue };
        let mut alive = conn.fill() && conn.flush();
        while let Some((kind, payload)) = decode(&mut conn.rx) {
            match kind {
                Some(Frame::Accept) if p.outgoing => {
                    p.outgoing = false;
                    p.connected = true;
                    if let Some(name) = split_nul(&payload).get(1) {
                        p.name = name.clone();
                    }
                    events.push(Event::StateChanged(id.clone(), STATE_CONNECTED));
                }
                Some(Frame::Deny) => {
                    p.outgoing = false;
                    events.push(Event::ConnectionFailed(id.clone()));
                    alive = false;
                }
                Some(Frame::Data) if p.connected => events.push(Event::Data(id.clone(), payload)),
                Some(Frame::Colors) if p.connected => crate::recolor::on_sync(&payload),
                Some(Frame::Bye) => alive = false,
                _ => {}
            }
        }
        if !alive {
            p.conn = None;
            if p.connected {
                p.connected = false;
                events.push(Event::StateChanged(id.clone(), STATE_DISCONNECTED));
            } else if p.outgoing {
                p.outgoing = false;
                events.push(Event::ConnectionFailed(id.clone()));
            }
            p.incoming = false;
        }
    }
    events
}

/// Called by the run loop: the network work of every live session, then the
/// game's delegate methods and data handler for what happened.
pub fn pump(h: &mut Host, c: &mut Cpu) {
    send_colors();
    let extra = EXTRA_HOSTS.lock().unwrap().clone();
    let ids: Vec<u32> = SESSIONS.lock().unwrap().as_ref().map_or(Vec::new(), |m| m.keys().copied().collect());
    for session in ids {
        let polled = with(session, |s| {
            if s.in_callback {
                return None;
            }
            let events = poll(s, &extra);
            if events.is_empty() {
                return None;
            }
            s.in_callback = true;
            Some((events, s.delegate, s.handler, s.context))
        })
        .flatten();
        let Some((events, delegate, handler, context)) = polled else { continue };
        for e in events {
            if c.halt != 0 {
                break;
            }
            dispatch(h, c, session, delegate, handler, context, e);
        }
        with(session, |s| s.in_callback = false);
    }
}

/// The outfit colours to every connected window: at once when one changed
/// here, else once a second (a window that joins late catches up).
fn send_colors() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let due = last.is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
    if !crate::recolor::take_changed() && !due {
        return;
    }
    *last = Some(Instant::now());
    let mut g = SESSIONS.lock().unwrap();
    let Some(sessions) = g.as_mut() else { return };
    let mut payload = None;
    for s in sessions.values_mut() {
        for p in s.peers.values_mut() {
            if let Peer { connected: true, conn: Some(conn), .. } = p {
                conn.send(Frame::Colors, payload.get_or_insert_with(crate::recolor::sync_payload));
            }
        }
    }
}

fn dispatch(h: &mut Host, c: &mut Cpu, session: u32, delegate: u32, handler: u32, context: u32, e: Event) {
    match e {
        Event::StateChanged(peer, state) => {
            println!("GKSession {session:#x}: peer {peer} state {state}");
            if objc::responds(h, delegate, "session:peer:didChangeState:") {
                let p = new_str(h, &peer);
                objc::send_n(h, c, delegate, "session:peer:didChangeState:", &[session, p, state]);
            }
        }
        Event::ConnectionRequest(peer) => {
            println!("GKSession {session:#x}: connection request from {peer}");
            if objc::responds(h, delegate, "session:didReceiveConnectionRequestFromPeer:") {
                let p = new_str(h, &peer);
                objc::send_n(h, c, delegate, "session:didReceiveConnectionRequestFromPeer:", &[session, p]);
            }
        }
        Event::ConnectionFailed(peer) => {
            println!("GKSession {session:#x}: connection with {peer} failed");
            if objc::responds(h, delegate, "session:connectionWithPeerFailed:withError:") {
                let p = new_str(h, &peer);
                objc::send_n(h, c, delegate, "session:connectionWithPeerFailed:withError:", &[session, p, 0]);
            }
        }
        Event::Data(peer, bytes) => {
            if handler == 0 {
                return;
            }
            let data = crate::nsfile::new_data(h, "NSData", &bytes);
            let p = new_str(h, &peer);
            objc::send_n(h, c, handler, "receiveData:fromPeer:inSession:context:", &[data, p, session, context]);
        }
    }
}
