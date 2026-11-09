//! Viewer side: connects to a desk, shows its screen and sends our input.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::codec::VideoDecoder;
use crate::config::parse_id;
use crate::extras::{ChatLine, FileEvent, FileReceiver, clock, spawn_clipboard_sync};
use crate::net::{self, Peers};
use crate::proto::{self, Codec, DEFAULT_PORT, DisplayInfo, Msg, Outbox, StreamSettings, Traffic};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Waiting,
    Connected,
    NeedPassword(String),
    Failed(String),
    Closed(String),
}

#[derive(Default)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub seq: u64,
}

pub struct Shared {
    pub status: Status,
    pub peer_name: String,
    pub peer_id: u64,
    pub fingerprint: String,
    pub displays: Vec<DisplayInfo>,
    pub can_control: bool,
    pub frame: Frame,
    pub codec: Codec,
    pub cursor: Option<(f32, f32)>,
    pub chat: Vec<ChatLine>,
    pub unread: usize,
    pub notices: Vec<String>,
    pub fps: f32,
    pub rtt_ms: f32,
    pub decode_ms: f32,
    pub accepted_key: Option<[u8; 32]>,
}

pub struct ConnectRequest {
    pub target: String,
    pub password: Option<String>,
    pub saved_key: Option<[u8; 32]>,
    pub want_control: bool,
    pub settings: StreamSettings,
    pub my_name: String,
    pub my_id: u64,
    pub relay: String,
    pub sync_clipboard: bool,
}

#[allow(dead_code)]
pub struct Session {
    pub sid: u64,
    pub target: String,
    pub shared: Arc<Mutex<Shared>>,
    pub outbox: Arc<Mutex<Option<Outbox>>>,
    pub traffic: Arc<proto::Traffic>,
    pub stop: Arc<AtomicBool>,
    pub clip_on: Arc<AtomicBool>,
    pub settings: StreamSettings,
    pub want_control: bool,
    // UI state
    pub texture: Option<egui::TextureHandle>,
    pub tex_seq: u64,
    pub view_only: bool,
    pub fit: bool,
    pub show_chat: bool,
    pub chat_input: String,
    pub keyboard_captured: bool,
    pub pressed: std::collections::HashSet<String>,
    pub mods: egui::Modifiers,
    pub scroll_acc: egui::Vec2,
    pub last_move: Option<(f32, f32)>,
    pub last_ping: Instant,
    pub mbps: f32,
    pub last_bytes: (Instant, u64),
    pub password_input: String,
    pub file_progress: Arc<Mutex<Option<(String, f32)>>>,
    pub toolbar_open: bool,
    pub toolbar_rect: Option<egui::Rect>,
    pub buttons: std::collections::HashSet<u8>,
}

static NEXT: AtomicU64 = AtomicU64::new(1);

impl Session {
    pub fn send(&self, msg: Msg) {
        if let Some(o) = self.outbox.lock().unwrap().as_ref() {
            o.send(msg);
        }
    }

    pub fn status(&self) -> Status {
        self.shared.lock().unwrap().status.clone()
    }

    pub fn disconnect(&mut self) {
        self.send(Msg::ReleaseAll);
        self.send(Msg::Bye("closed".into()));
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn title(&self) -> String {
        let s = self.shared.lock().unwrap();
        if !s.peer_name.is_empty() { s.peer_name.clone() } else { self.target.clone() }
    }
}

pub fn connect(req: ConnectRequest, peers: Peers, ctx: egui::Context) -> Session {
    let shared = Arc::new(Mutex::new(Shared {
        status: Status::Connecting,
        peer_name: String::new(),
        peer_id: 0,
        fingerprint: String::new(),
        displays: Vec::new(),
        can_control: false,
        frame: Frame::default(),
        codec: req.settings.codec,
        cursor: None,
        chat: Vec::new(),
        unread: 0,
        notices: Vec::new(),
        fps: 0.0,
        rtt_ms: 0.0,
        decode_ms: 0.0,
        accepted_key: None,
    }));
    let outbox: Arc<Mutex<Option<Outbox>>> = Arc::default();
    let traffic = Arc::new(Traffic::default());
    let stop = Arc::new(AtomicBool::new(false));
    let clip_on = Arc::new(AtomicBool::new(req.sync_clipboard));
    let session = Session {
        sid: NEXT.fetch_add(1, Ordering::SeqCst),
        target: req.target.clone(),
        shared: shared.clone(),
        outbox: outbox.clone(),
        traffic: traffic.clone(),
        stop: stop.clone(),
        clip_on: clip_on.clone(),
        settings: req.settings.clone(),
        want_control: req.want_control,
        texture: None,
        tex_seq: 0,
        view_only: !req.want_control,
        fit: true,
        show_chat: false,
        chat_input: String::new(),
        keyboard_captured: false,
        pressed: Default::default(),
        mods: Default::default(),
        scroll_acc: egui::Vec2::ZERO,
        last_move: None,
        last_ping: Instant::now(),
        mbps: 0.0,
        last_bytes: (Instant::now(), 0),
        password_input: String::new(),
        file_progress: Arc::default(),
        toolbar_open: true,
        toolbar_rect: None,
        buttons: Default::default(),
    };
    let _ = std::thread::Builder::new().name("client".into()).spawn(move || {
        let result = run(req, peers, shared.clone(), outbox, traffic, stop, clip_on, ctx.clone());
        let mut s = shared.lock().unwrap();
        if let Err(e) = result {
            if matches!(s.status, Status::Connecting | Status::Waiting) {
                s.status = Status::Failed(e);
            } else if s.status == Status::Connected {
                s.status = Status::Closed(e);
            }
        } else if s.status == Status::Connected {
            s.status = Status::Closed(String::new());
        }
        drop(s);
        ctx.request_repaint();
    });
    session
}

fn open_stream(target: &str, peers: &Peers, relay: &str) -> Result<TcpStream, String> {
    let t = target.trim();
    if let Some(id) = parse_id(t) {
        let lan = peers.lock().unwrap().get(&id).map(|p| p.addr);
        if let Some(addr) = lan {
            if let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_secs(4)) {
                return Ok(s);
            }
        }
        if relay.trim().is_empty() {
            return Err("id-not-found".into());
        }
        return net::relay_connect(relay, id).map_err(|e| {
            if e.to_string() == "offline" { "offline".to_string() } else { format!("relay: {e}") }
        });
    }
    let with_port = if t.parse::<SocketAddr>().is_ok() || (t.matches(':').count() == 1 && !t.starts_with('[')) {
        t.to_string()
    } else if t.starts_with('[') {
        t.to_string()
    } else {
        format!("{t}:{DEFAULT_PORT}")
    };
    let addrs: Vec<SocketAddr> = with_port.to_socket_addrs().map_err(|_| "bad-address".to_string())?.collect();
    let mut last = "unreachable".to_string();
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(6)) {
            Ok(s) => return Ok(s),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

#[allow(clippy::too_many_arguments)]
fn run(
    req: ConnectRequest,
    peers: Peers,
    shared: Arc<Mutex<Shared>>,
    outbox_slot: Arc<Mutex<Option<Outbox>>>,
    traffic: Arc<Traffic>,
    stop: Arc<AtomicBool>,
    clip_on: Arc<AtomicBool>,
    ctx: egui::Context,
) -> Result<(), String> {
    let stream = open_stream(&req.target, &peers, &req.relay)?;
    let mut sec = proto::client_handshake(stream, &req.my_name, req.my_id).map_err(|e| e.to_string())?;
    {
        let mut s = shared.lock().unwrap();
        s.peer_name = sec.peer.name.clone();
        s.peer_id = sec.peer.id;
        s.fingerprint = sec.fingerprint.clone();
        s.status = Status::Waiting;
    }
    ctx.request_repaint();
    let key = req.password.as_deref().filter(|p| !p.trim().is_empty()).map(proto::password_key).or(req.saved_key);
    let proof = key.map(|k| proto::password_proof(&k, &sec.transcript));
    sec.send(&Msg::Auth { proof, want_control: req.want_control }).map_err(|e| e.to_string())?;
    let reply = sec.recv().map_err(|e| e.to_string())?;
    let Msg::AuthResult { ok, reason, can_control, displays } = reply else {
        return Err("protocol".into());
    };
    if !ok {
        let mut s = shared.lock().unwrap();
        s.status = if reason == "password-required" || reason == "wrong-password" {
            Status::NeedPassword(reason)
        } else {
            Status::Failed(reason)
        };
        ctx.request_repaint();
        return Ok(());
    }
    let (outbox, mut reader) = proto::split(sec, traffic).map_err(|e| e.to_string())?;
    outbox.send(Msg::Stream(req.settings.clone()));
    *outbox_slot.lock().unwrap() = Some(outbox.clone());
    {
        let mut s = shared.lock().unwrap();
        s.status = Status::Connected;
        s.can_control = can_control;
        s.displays = displays;
        s.accepted_key = key;
    }
    ctx.request_repaint();
    let clip_tx = spawn_clipboard_sync(outbox.clone(), stop.clone(), clip_on);

    // Close the socket when the UI asks us to stop.
    {
        let stop = stop.clone();
        let stream = reader.try_clone_stream().map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
            }
            std::thread::sleep(Duration::from_millis(150)); // let "Bye" go out
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
    }

    let mut decoder = VideoDecoder::new();
    let mut files = FileReceiver::default();
    let mut frames = 0u32;
    let mut fps_t = Instant::now();
    let mut decode_total = 0f32;
    let mut asked_key = Instant::now() - Duration::from_secs(5);
    loop {
        let msg = match reader.recv() {
            Ok(m) => m,
            Err(e) => {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                return Err(e.to_string());
            }
        };
        match msg {
            Msg::VideoFormat { codec, .. } => {
                decoder.reset();
                shared.lock().unwrap().codec = codec;
            }
            Msg::Video { codec, data, .. } => {
                let t0 = Instant::now();
                match decoder.decode(codec, &data) {
                    Ok(true) => {
                        decode_total += t0.elapsed().as_secs_f32() * 1000.0;
                        frames += 1;
                        let mut s = shared.lock().unwrap();
                        let f = &mut s.frame;
                        f.width = decoder.width;
                        f.height = decoder.height;
                        f.rgba.clear();
                        f.rgba.extend_from_slice(&decoder.rgba);
                        f.seq += 1;
                        drop(s);
                        ctx.request_repaint();
                    }
                    Ok(false) => {}
                    Err(()) => {
                        if asked_key.elapsed() > Duration::from_millis(300) {
                            outbox.send(Msg::RequestKeyframe);
                            asked_key = Instant::now();
                        }
                    }
                }
                if fps_t.elapsed() >= Duration::from_secs(1) {
                    let secs = fps_t.elapsed().as_secs_f32();
                    let mut s = shared.lock().unwrap();
                    s.fps = frames as f32 / secs;
                    s.decode_ms = if frames > 0 { decode_total / frames as f32 } else { 0.0 };
                    frames = 0;
                    decode_total = 0.0;
                    fps_t = Instant::now();
                }
            }
            Msg::Cursor { x, y } => {
                shared.lock().unwrap().cursor = Some((x, y));
                ctx.request_repaint();
            }
            Msg::Pong(t) => {
                let rtt = proto::now_ms().saturating_sub(t) as f32;
                let mut s = shared.lock().unwrap();
                s.rtt_ms = if s.rtt_ms == 0.0 { rtt } else { s.rtt_ms * 0.6 + rtt * 0.4 };
            }
            Msg::Clipboard(text) => {
                let _ = clip_tx.send(text);
            }
            Msg::Chat(text) => {
                let mut s = shared.lock().unwrap();
                s.chat.push(ChatLine { mine: false, text, time: clock() });
                s.unread += 1;
                drop(s);
                ctx.request_repaint();
            }
            Msg::Permissions { can_control } => {
                shared.lock().unwrap().can_control = can_control;
                ctx.request_repaint();
            }
            Msg::Notice(n) => {
                shared.lock().unwrap().notices.push(n);
                ctx.request_repaint();
            }
            m @ (Msg::FileStart { .. } | Msg::FileChunk { .. } | Msg::FileEnd { .. }) => {
                if let Some(FileEvent::Done(p)) = files.handle(&m) {
                    shared.lock().unwrap().notices.push(format!("file-received:{}", p.display()));
                    ctx.request_repaint();
                }
            }
            Msg::Bye(reason) => {
                shared.lock().unwrap().status = Status::Closed(reason);
                ctx.request_repaint();
                return Ok(());
            }
            _ => {}
        }
    }
}
