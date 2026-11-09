//! Finding desks: LAN discovery (UDP broadcast) and an optional relay server
//! that connects two desks by ID across the internet. The relay only forwards
//! encrypted bytes; it can never see the screen or keystrokes.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

use crate::host::{self, Host, RelayState};
use crate::proto::{DISCOVERY_PORT, RELAY_PORT, random_bytes};

#[derive(Clone, Debug)]
pub struct Peer {
    pub id: u64,
    pub name: String,
    pub addr: SocketAddr,
    pub os: String,
    pub last_seen: Instant,
}

pub type Peers = Arc<Mutex<HashMap<u64, Peer>>>;

#[derive(Clone)]
pub struct Announce {
    pub id: u64,
    pub name: String,
    pub port: u16,
    pub enabled: bool,
}

fn os_name() -> &'static str {
    if cfg!(windows) { "Windows" } else if cfg!(target_os = "macos") { "macOS" } else { "Linux" }
}

pub fn start_discovery(peers: Peers, me: Arc<Mutex<Announce>>, ctx: egui::Context) {
    // Listener
    {
        let peers = peers.clone();
        let me = me.clone();
        let _ = std::thread::Builder::new().name("discovery-rx".into()).spawn(move || {
            let sock = (|| -> io::Result<UdpSocket> {
                let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
                s.set_reuse_address(true)?;
                s.bind(&SocketAddr::from(([0, 0, 0, 0], DISCOVERY_PORT)).into())?;
                Ok(s.into())
            })();
            let Ok(sock) = sock else { return };
            let mut buf = [0u8; 512];
            loop {
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                let parts: Vec<&str> = text.splitn(6, '|').collect();
                if parts.len() < 6 || parts[0] != "MYDESK" || parts[1] != "1" {
                    continue;
                }
                let (Ok(id), Ok(port)) = (parts[2].parse::<u64>(), parts[3].parse::<u16>()) else { continue };
                if id == me.lock().unwrap().id {
                    continue;
                }
                let peer = Peer {
                    id,
                    name: parts[5].to_string(),
                    addr: SocketAddr::new(from.ip(), port),
                    os: parts[4].to_string(),
                    last_seen: Instant::now(),
                };
                let fresh = peers.lock().unwrap().insert(id, peer).is_none();
                if fresh {
                    ctx.request_repaint();
                }
            }
        });
    }
    // Announcer
    let _ = std::thread::Builder::new().name("discovery-tx".into()).spawn(move || {
        let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else { return };
        let _ = sock.set_broadcast(true);
        loop {
            let a = me.lock().unwrap().clone();
            if a.enabled {
                let msg = format!("MYDESK|1|{}|{}|{}|{}", a.id, a.port, os_name(), a.name);
                let _ = sock.send_to(msg.as_bytes(), ("255.255.255.255", DISCOVERY_PORT));
            }
            std::thread::sleep(Duration::from_secs(2));
            peers.lock().unwrap().retain(|_, p| p.last_seen.elapsed() < Duration::from_secs(12));
        }
    });
}

// ------------------------------------------------------------------ relay

fn read_line(s: &mut TcpStream) -> io::Result<String> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    loop {
        let n = s.read(&mut b)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if b[0] == b'\n' {
            break;
        }
        out.push(b[0]);
        if out.len() > 256 {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

pub fn relay_addr(relay: &str) -> Option<SocketAddr> {
    let relay = relay.trim();
    if relay.is_empty() {
        return None;
    }
    let with_port = if relay.contains(':') { relay.to_string() } else { format!("{relay}:{RELAY_PORT}") };
    with_port.to_socket_addrs().ok()?.next()
}

/// Viewer side: ask the relay to connect us to `id`.
pub fn relay_connect(relay: &str, id: u64) -> io::Result<TcpStream> {
    let addr = relay_addr(relay).ok_or_else(|| io::Error::other("relay address is not valid"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(6))?;
    s.set_read_timeout(Some(Duration::from_secs(20)))?;
    s.write_all(format!("CONN {id}\n").as_bytes())?;
    let reply = read_line(&mut s)?;
    s.set_read_timeout(None)?;
    if reply == "OK" {
        Ok(s)
    } else {
        Err(io::Error::other("offline"))
    }
}

/// Host side: stay registered with the relay and accept connections it announces.
pub fn start_relay_client(host: Host, relay: Arc<Mutex<String>>) {
    let _ = std::thread::Builder::new().name("relay-client".into()).spawn(move || {
        loop {
            let target = relay.lock().unwrap().clone();
            let Some(addr) = relay_addr(&target) else {
                host.lock().unwrap().relay = RelayState::Unused;
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            host.lock().unwrap().relay = RelayState::Connecting;
            let id = host.lock().unwrap().id;
            let result = (|| -> io::Result<()> {
                let mut ctrl = TcpStream::connect_timeout(&addr, Duration::from_secs(6))?;
                ctrl.write_all(format!("REG {id}\n").as_bytes())?;
                ctrl.set_read_timeout(Some(Duration::from_secs(45)))?;
                host.lock().unwrap().relay = RelayState::Online;
                if let Some(c) = host.lock().unwrap().ctx.clone() {
                    c.request_repaint();
                }
                let mut last_ping = Instant::now();
                loop {
                    if *relay.lock().unwrap() != target {
                        return Ok(()); // relay changed in settings
                    }
                    if last_ping.elapsed() > Duration::from_secs(15) {
                        ctrl.write_all(b"PING\n")?;
                        last_ping = Instant::now();
                    }
                    ctrl.set_read_timeout(Some(Duration::from_secs(5)))?;
                    let line = match read_line(&mut ctrl) {
                        Ok(l) => l,
                        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
                        Err(e) => return Err(e),
                    };
                    if let Some(token) = line.strip_prefix("INC ") {
                        let mut data = TcpStream::connect_timeout(&addr, Duration::from_secs(6))?;
                        data.write_all(format!("ACC {token}\n").as_bytes())?;
                        host::serve_stream(data, host.clone());
                    }
                }
            })();
            if let Err(e) = result {
                host.lock().unwrap().relay = RelayState::Error(e.to_string());
            }
            std::thread::sleep(Duration::from_secs(4));
        }
    });
}

/// Runs a relay server (`mydesk --relay [port]`). Blocks forever.
pub fn run_relay_server(port: u16) -> io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    println!("MyDesk relay listening on port {port}");
    type Hosts = Arc<Mutex<HashMap<u64, TcpStream>>>;
    type Waiting = Arc<Mutex<HashMap<String, TcpStream>>>;
    let hosts: Hosts = Arc::default();
    let waiting: Waiting = Arc::default();
    for stream in listener.incoming().flatten() {
        let hosts = hosts.clone();
        let waiting = waiting.clone();
        std::thread::spawn(move || {
            let mut s = stream;
            let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
            let Ok(line) = read_line(&mut s) else { return };
            let mut it = line.split_whitespace();
            match (it.next(), it.next()) {
                (Some("REG"), Some(id)) => {
                    let Ok(id) = id.parse::<u64>() else { return };
                    let Ok(w) = s.try_clone() else { return };
                    hosts.lock().unwrap().insert(id, w);
                    println!("desk {id} online");
                    let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
                    while read_line(&mut s).is_ok() {}
                    hosts.lock().unwrap().remove(&id);
                    println!("desk {id} offline");
                }
                (Some("CONN"), Some(id)) => {
                    let Ok(id) = id.parse::<u64>() else { return };
                    let token = format!("{:016x}", u64::from_le_bytes(random_bytes::<8>()));
                    let announced = hosts
                        .lock()
                        .unwrap()
                        .get_mut(&id)
                        .map(|h| h.write_all(format!("INC {token}\n").as_bytes()).is_ok())
                        .unwrap_or(false);
                    if !announced {
                        let _ = s.write_all(b"ERR offline\n");
                        return;
                    }
                    waiting.lock().unwrap().insert(token.clone(), s);
                    std::thread::sleep(Duration::from_secs(10));
                    if let Some(mut s) = waiting.lock().unwrap().remove(&token) {
                        let _ = s.write_all(b"ERR timeout\n");
                    }
                }
                (Some("ACC"), Some(token)) => {
                    let Some(mut client) = waiting.lock().unwrap().remove(token) else { return };
                    if client.write_all(b"OK\n").is_err() {
                        return;
                    }
                    let _ = s.set_read_timeout(None);
                    let _ = client.set_read_timeout(None);
                    let _ = s.set_nodelay(true);
                    let _ = client.set_nodelay(true);
                    pipe(client, s);
                }
                _ => {}
            }
        });
    }
    Ok(())
}

fn pipe(a: TcpStream, b: TcpStream) {
    let (Ok(mut a2), Ok(mut b2)) = (a.try_clone(), b.try_clone()) else { return };
    let (mut a, mut b) = (a, b);
    let t = std::thread::spawn(move || {
        let _ = io::copy(&mut a2, &mut b2);
        let _ = b2.shutdown(std::net::Shutdown::Both);
    });
    let _ = io::copy(&mut b, &mut a);
    let _ = a.shutdown(std::net::Shutdown::Both);
    let _ = t.join();
}
