//! Wire protocol: framing, handshake and the encrypted message channel.
//!
//! Every connection starts with a plain-text handshake (X25519 key exchange),
//! after which all messages are sealed with ChaCha20-Poly1305 using one key per
//! direction. Passwords never travel over the wire: the client proves that it
//! knows the password with an HMAC over the handshake transcript.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_PORT: u16 = 47800;
pub const DISCOVERY_PORT: u16 = 47801;
pub const RELAY_PORT: u16 = 47900;
const MAGIC: [u8; 6] = *b"MYDESK";
const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Smooth video (H.264), best for games and motion.
    H264,
    /// Pixel-perfect changed tiles (LZ4), best for text and office work.
    Lossless,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Speed,
    Balanced,
    Best,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Native,
    Max1080,
    Max720,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DisplayInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StreamSettings {
    pub display: usize,
    pub codec: Codec,
    pub quality: Quality,
    pub fps: u32,
    pub resolution: Resolution,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            display: 0,
            codec: Codec::H264,
            quality: Quality::Balanced,
            fps: 60,
            resolution: Resolution::Native,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Msg {
    // --- session setup ---
    Auth { proof: Option<[u8; 32]>, want_control: bool },
    AuthResult { ok: bool, reason: String, can_control: bool, displays: Vec<DisplayInfo> },
    // --- video ---
    Stream(StreamSettings),
    VideoFormat { display: usize, width: u32, height: u32, codec: Codec },
    Video {
        codec: Codec,
        key: bool,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    RequestKeyframe,
    Cursor { x: f32, y: f32 },
    // --- input ---
    MouseMove { x: f32, y: f32 },
    MouseButton { button: u8, down: bool },
    Scroll { dx: i32, dy: i32 },
    Key { code: String, down: bool },
    ReleaseAll,
    CtrlAltDel,
    // --- extras ---
    Clipboard(String),
    Chat(String),
    Ping(u64),
    Pong(u64),
    Permissions { can_control: bool },
    FileStart { id: u32, name: String, size: u64 },
    FileChunk {
        id: u32,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    FileEnd { id: u32 },
    Notice(String),
    Bye(String),
}

impl Msg {
    pub fn is_video(&self) -> bool {
        matches!(self, Msg::Video { .. })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Hello {
    pub magic: [u8; 6],
    pub version: u32,
    pub name: String,
    pub id: u64,
    pub public: [u8; 32],
    pub nonce: [u8; 16],
}

// ---------------------------------------------------------------- framing

pub fn write_frame(w: &mut impl Write, data: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(data.len() + 4);
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_frame(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut data = vec![0u8; len];
    r.read_exact(&mut data)?;
    Ok(data)
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

// ---------------------------------------------------------------- crypto

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("system random generator");
    b
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

pub fn password_key(password: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"mydesk-password-v1:");
    h.update(password.trim().as_bytes());
    h.finalize().into()
}

pub fn password_proof(password_key: &[u8; 32], transcript: &[u8; 32]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(password_key).expect("hmac key");
    mac.update(b"mydesk-auth");
    mac.update(transcript);
    mac.finalize().into_bytes().into()
}

/// Constant-time comparison.
pub fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub struct Sealer {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl Sealer {
    fn new(key: [u8; 32]) -> Self {
        Self { cipher: ChaCha20Poly1305::new(Key::from_slice(&key)), counter: 0 }
    }
    fn nonce(&mut self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.counter.to_le_bytes());
        self.counter += 1;
        n
    }
    pub fn seal(&mut self, data: &[u8]) -> Vec<u8> {
        let n = self.nonce();
        self.cipher.encrypt(Nonce::from_slice(&n), data).expect("encrypt")
    }
    pub fn open(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        let n = self.nonce();
        self.cipher
            .decrypt(Nonce::from_slice(&n), data)
            .map_err(|_| bad("message failed authentication"))
    }
}

/// Result of a successful handshake.
pub struct Secured {
    pub stream: TcpStream,
    pub sealer: Sealer,
    pub opener: Sealer,
    pub peer: Hello,
    pub transcript: [u8; 32],
    /// Short code both sides can compare to rule out a man in the middle.
    pub fingerprint: String,
}

impl Secured {
    pub fn send(&mut self, msg: &Msg) -> io::Result<()> {
        let bytes = bincode::serialize(msg).map_err(|e| bad(&e.to_string()))?;
        let sealed = self.sealer.seal(&bytes);
        write_frame(&mut self.stream, &sealed)
    }
    pub fn recv(&mut self) -> io::Result<Msg> {
        let frame = read_frame(&mut self.stream)?;
        let plain = self.opener.open(&frame)?;
        bincode::deserialize(&plain).map_err(|e| bad(&e.to_string()))
    }
}

fn derive(shared: &[u8; 32], client: &Hello, host: &Hello) -> ([u8; 32], [u8; 32], [u8; 32]) {
    let mut h = Sha256::new();
    h.update(b"mydesk-session-v1");
    h.update(shared);
    h.update(client.public);
    h.update(client.nonce);
    h.update(host.public);
    h.update(host.nonce);
    let base: [u8; 32] = h.finalize().into();
    let k = |label: &[u8]| {
        let mut h = Sha256::new();
        h.update(base);
        h.update(label);
        let out: [u8; 32] = h.finalize().into();
        out
    };
    let mut th = Sha256::new();
    th.update(b"transcript");
    th.update(client.public);
    th.update(client.nonce);
    th.update(host.public);
    th.update(host.nonce);
    (k(b"client->host"), k(b"host->client"), th.finalize().into())
}

fn fingerprint(transcript: &[u8; 32], shared_base: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(transcript);
    h.update(shared_base);
    let d: [u8; 32] = h.finalize().into();
    format!("{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}", d[0], d[1], d[2], d[3], d[4], d[5])
}

fn make_hello(name: &str, id: u64) -> (StaticSecret, Hello) {
    let secret = StaticSecret::from(random_bytes::<32>());
    let public = PublicKey::from(&secret);
    let hello = Hello {
        magic: MAGIC,
        version: PROTOCOL_VERSION,
        name: name.to_string(),
        id,
        public: *public.as_bytes(),
        nonce: random_bytes::<16>(),
    };
    (secret, hello)
}

fn read_hello(stream: &mut TcpStream) -> io::Result<Hello> {
    let frame = read_frame(stream)?;
    let hello: Hello = bincode::deserialize(&frame).map_err(|_| bad("not a MyDesk peer"))?;
    if hello.magic != MAGIC {
        return Err(bad("not a MyDesk peer"));
    }
    if hello.version != PROTOCOL_VERSION {
        return Err(bad("incompatible MyDesk version"));
    }
    Ok(hello)
}

fn tune(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let sock = socket2::SockRef::from(stream);
    let _ = sock.set_send_buffer_size(4 * 1024 * 1024);
    let _ = sock.set_recv_buffer_size(4 * 1024 * 1024);
    let _ = sock.set_keepalive(true);
}

pub fn client_handshake(mut stream: TcpStream, name: &str, id: u64) -> io::Result<Secured> {
    tune(&stream);
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    let (secret, hello) = make_hello(name, id);
    write_frame(&mut stream, &bincode::serialize(&hello).unwrap())?;
    let host = read_hello(&mut stream)?;
    let shared = secret.diffie_hellman(&PublicKey::from(host.public));
    let shared = *shared.as_bytes();
    let (c2h, h2c, transcript) = derive(&shared, &hello, &host);
    let fp = fingerprint(&transcript, &sha256(&shared));
    stream.set_read_timeout(None)?;
    Ok(Secured {
        stream,
        sealer: Sealer::new(c2h),
        opener: Sealer::new(h2c),
        peer: host,
        transcript,
        fingerprint: fp,
    })
}

pub fn host_handshake(mut stream: TcpStream, name: &str, id: u64) -> io::Result<Secured> {
    tune(&stream);
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    let client = read_hello(&mut stream)?;
    let (secret, hello) = make_hello(name, id);
    write_frame(&mut stream, &bincode::serialize(&hello).unwrap())?;
    let shared = secret.diffie_hellman(&PublicKey::from(client.public));
    let shared = *shared.as_bytes();
    let (c2h, h2c, transcript) = derive(&shared, &client, &hello);
    let fp = fingerprint(&transcript, &sha256(&shared));
    stream.set_read_timeout(None)?;
    Ok(Secured {
        stream,
        sealer: Sealer::new(h2c),
        opener: Sealer::new(c2h),
        peer: client,
        transcript,
        fingerprint: fp,
    })
}

// ---------------------------------------------------------------- channel

/// Traffic counters shared between the network threads and the UI.
#[derive(Default)]
pub struct Traffic {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
}

/// Sending side of a session: a writer thread owns the socket and seals messages.
#[derive(Clone)]
pub struct Outbox {
    tx: Sender<Msg>,
    pub video_in_flight: Arc<AtomicUsize>,
    pub queued: Arc<AtomicUsize>,
}

impl Outbox {
    pub fn send(&self, msg: Msg) -> bool {
        if msg.is_video() {
            self.video_in_flight.fetch_add(1, Ordering::SeqCst);
        }
        self.queued.fetch_add(1, Ordering::SeqCst);
        self.tx.send(msg).is_ok()
    }
    pub fn video_busy(&self, limit: usize) -> bool {
        self.video_in_flight.load(Ordering::SeqCst) >= limit
    }
}

/// Splits a secured connection into a writer thread (returned [`Outbox`]) and a reader.
pub fn split(secured: Secured, traffic: Arc<Traffic>) -> io::Result<(Outbox, Reader)> {
    let Secured { stream, sealer, opener, .. } = secured;
    let write_stream = stream.try_clone()?;
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let in_flight2 = in_flight.clone();
    let queued = Arc::new(AtomicUsize::new(0));
    let queued2 = queued.clone();
    let traffic2 = traffic.clone();
    std::thread::Builder::new()
        .name("net-writer".into())
        .spawn(move || {
            let mut sealer = sealer;
            let mut stream = write_stream;
            while let Ok(msg) = rx.recv() {
                let video = msg.is_video();
                let bye = matches!(msg, Msg::Bye(_));
                let Ok(bytes) = bincode::serialize(&msg) else { continue };
                let sealed = sealer.seal(&bytes);
                let ok = write_frame(&mut stream, &sealed).is_ok();
                traffic2.bytes_out.fetch_add(sealed.len() as u64 + 4, Ordering::Relaxed);
                if video {
                    in_flight2.fetch_sub(1, Ordering::SeqCst);
                }
                queued2.fetch_sub(1, Ordering::SeqCst);
                if !ok || bye {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    break;
                }
            }
        })?;
    Ok((
        Outbox { tx, video_in_flight: in_flight, queued },
        Reader { stream, opener, traffic },
    ))
}

pub struct Reader {
    stream: TcpStream,
    opener: Sealer,
    traffic: Arc<Traffic>,
}

impl Reader {
    pub fn recv(&mut self) -> io::Result<Msg> {
        let frame = read_frame(&mut self.stream)?;
        self.traffic.bytes_in.fetch_add(frame.len() as u64 + 4, Ordering::Relaxed);
        let plain = self.opener.open(&frame)?;
        bincode::deserialize(&plain).map_err(|e| bad(&e.to_string()))
    }
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
    pub fn try_clone_stream(&self) -> io::Result<TcpStream> {
        self.stream.try_clone()
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
