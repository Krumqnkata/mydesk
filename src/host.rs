//! Hosting: lets other people connect to this desk, after a password or the
//! person's OK, then streams the screen and replays their input.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::capture::{self, Capturer, RawFrame};
use crate::codec::VideoEncoder;
use crate::config::AccessMode;
use crate::extras::{ChatLine, FileEvent, FileReceiver, clock, spawn_clipboard_sync};
use crate::input::Injector;
use crate::proto::{self, DisplayInfo, Msg, Outbox, StreamSettings, Traffic};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    Accept { control: bool },
    Reject,
}

#[allow(dead_code)]
pub struct Pending {
    pub pid: u64,
    pub peer_name: String,
    pub peer_id: u64,
    pub peer_addr: String,
    pub fingerprint: String,
    pub want_control: bool,
    pub decision: Option<Decision>,
    pub since: Instant,
}

#[allow(dead_code)]
pub struct HostSession {
    pub sid: u64,
    pub peer_name: String,
    pub peer_id: u64,
    pub peer_addr: String,
    pub fingerprint: String,
    pub started: Instant,
    pub can_control: bool,
    pub outbox: Outbox,
    pub chat: Vec<ChatLine>,
    pub unread: usize,
    pub kick: Arc<AtomicBool>,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum Listening {
    Starting,
    On(u16),
    Off,
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum RelayState {
    Unused,
    Connecting,
    Online,
    Error(String),
}

pub struct HostState {
    pub id: u64,
    pub name: String,
    pub allow_incoming: bool,
    pub access: AccessMode,
    pub one_time_password: String,
    pub permanent_key: Option<[u8; 32]>,
    pub sync_clipboard: bool,
    pub sessions: Vec<HostSession>,
    pub pending: Vec<Pending>,
    pub toasts: Vec<String>,
    pub listening: Listening,
    pub relay: RelayState,
    pub ctx: Option<egui::Context>,
}

pub type Host = Arc<Mutex<HostState>>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn repaint(host: &Host) {
    if let Some(ctx) = host.lock().unwrap().ctx.clone() {
        ctx.request_repaint();
    }
}

pub fn toast(host: &Host, text: String) {
    host.lock().unwrap().toasts.push(text);
    repaint(host);
}

pub fn start_listener(host: Host, port: u16) {
    std::thread::Builder::new()
        .name("listener".into())
        .spawn(move || {
            let listener = (0..10).find_map(|i| TcpListener::bind(("0.0.0.0", port + i)).ok());
            let Some(listener) = listener else {
                host.lock().unwrap().listening = Listening::Error(format!("port {port} busy"));
                repaint(&host);
                return;
            };
            let actual = listener.local_addr().map(|a| a.port()).unwrap_or(port);
            host.lock().unwrap().listening = Listening::On(actual);
            repaint(&host);
            for stream in listener.incoming().flatten() {
                let host = host.clone();
                let _ = std::thread::Builder::new().name("host-session".into()).spawn(move || {
                    let _ = serve(stream, host);
                });
            }
        })
        .expect("spawn listener");
}

pub fn serve_stream(stream: TcpStream, host: Host) {
    let _ = std::thread::Builder::new().name("host-session".into()).spawn(move || {
        let _ = serve(stream, host);
    });
}

fn serve(stream: TcpStream, host: Host) -> std::io::Result<()> {
    let peer_addr = stream.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let (name, id, allow) = {
        let h = host.lock().unwrap();
        (h.name.clone(), h.id, h.allow_incoming)
    };
    if !allow {
        return Ok(());
    }
    let mut sec = proto::host_handshake(stream, &name, id)?;
    sec.stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let Msg::Auth { proof, want_control } = sec.recv()? else { return Ok(()) };
    sec.stream.set_read_timeout(None)?;

    let (access, one_time, permanent) = {
        let h = host.lock().unwrap();
        (h.access, proto::password_key(&h.one_time_password), h.permanent_key)
    };
    let password_ok = proof.is_some_and(|p| {
        proto::same(&p, &proto::password_proof(&one_time, &sec.transcript))
            || permanent.is_some_and(|k| proto::same(&p, &proto::password_proof(&k, &sec.transcript)))
    });

    let decision = match (access, password_ok) {
        (AccessMode::PasswordOnly | AccessMode::PasswordOrAsk, true) => Decision::Accept { control: want_control },
        (AccessMode::PasswordOnly, false) => {
            std::thread::sleep(Duration::from_millis(1200)); // slow down guessing
            let reason = if proof.is_some() { "wrong-password" } else { "password-required" };
            sec.send(&Msg::AuthResult { ok: false, reason: reason.into(), can_control: false, displays: vec![] })?;
            return Ok(());
        }
        _ => {
            if proof.is_some() && !password_ok {
                std::thread::sleep(Duration::from_millis(1200));
            }
            ask(&host, &sec.peer.name, sec.peer.id, &peer_addr, &sec.fingerprint, want_control)
        }
    };
    let control = match decision {
        Decision::Accept { control } => control && want_control,
        Decision::Reject => {
            sec.send(&Msg::AuthResult { ok: false, reason: "rejected".into(), can_control: false, displays: vec![] })?;
            return Ok(());
        }
    };

    let displays = capture::list_displays();
    sec.send(&Msg::AuthResult { ok: true, reason: String::new(), can_control: control, displays: displays.clone() })?;
    let peer_name = sec.peer.name.clone();
    let peer_id = sec.peer.id;
    let fingerprint = sec.fingerprint.clone();
    let traffic = Arc::new(Traffic::default());
    let (outbox, mut reader) = proto::split(sec, traffic)?;

    let sid = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let kick = Arc::new(AtomicBool::new(false));
    {
        let mut h = host.lock().unwrap();
        h.sessions.push(HostSession {
            sid,
            peer_name: peer_name.clone(),
            peer_id,
            peer_addr,
            fingerprint,
            started: Instant::now(),
            can_control: control,
            outbox: outbox.clone(),
            chat: Vec::new(),
            unread: 0,
            kick: kick.clone(),
        });
    }
    repaint(&host);

    // Streaming thread.
    let ctl = Arc::new(StreamCtl {
        settings: Mutex::new(StreamSettings::default()),
        keyframe: AtomicBool::new(true),
        stop: Arc::new(AtomicBool::new(false)),
    });
    {
        let ctl = ctl.clone();
        let outbox = outbox.clone();
        let displays = displays.clone();
        let _ = std::thread::Builder::new().name("streamer".into()).spawn(move || stream_loop(ctl, outbox, displays));
    }
    let clip_on = Arc::new(AtomicBool::new(host.lock().unwrap().sync_clipboard));
    let clip_tx = spawn_clipboard_sync(outbox.clone(), ctl.stop.clone(), clip_on.clone());

    // Watch for "disconnect" pressed on this side.
    {
        let kick = kick.clone();
        let stream = reader.try_clone_stream()?;
        let ctl = ctl.clone();
        let _ = std::thread::spawn(move || {
            while !ctl.stop.load(Ordering::Relaxed) {
                if kick.load(Ordering::Relaxed) {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }

    let mut injector = Injector::new();
    let mut files = FileReceiver::default();
    loop {
        let msg = match reader.recv() {
            Ok(m) => m,
            Err(_) => break,
        };
        let can_control = {
            let h = host.lock().unwrap();
            clip_on.store(h.sync_clipboard, Ordering::Relaxed);
            h.sessions.iter().find(|s| s.sid == sid).map(|s| s.can_control).unwrap_or(false)
        };
        match msg {
            Msg::Stream(s) => {
                *ctl.settings.lock().unwrap() = s;
                ctl.keyframe.store(true, Ordering::SeqCst);
            }
            Msg::RequestKeyframe => ctl.keyframe.store(true, Ordering::SeqCst),
            Msg::Ping(t) => {
                outbox.send(Msg::Pong(t));
            }
            Msg::MouseMove { x, y } if can_control => {
                let idx = ctl.settings.lock().unwrap().display;
                if let Some(d) = displays.get(idx).or(displays.first()) {
                    injector.move_to(d, x, y);
                }
            }
            Msg::MouseButton { button, down } if can_control => injector.button(button, down),
            Msg::Scroll { dx, dy } if can_control => injector.scroll(dx, dy),
            Msg::Key { code, down } if can_control => injector.key(&code, down),
            Msg::CtrlAltDel if can_control => injector.ctrl_alt_del(),
            Msg::ReleaseAll => injector.release_all(),
            Msg::Clipboard(text) => {
                let _ = clip_tx.send(text);
            }
            Msg::Chat(text) => {
                let mut h = host.lock().unwrap();
                if let Some(s) = h.sessions.iter_mut().find(|s| s.sid == sid) {
                    s.chat.push(ChatLine { mine: false, text, time: clock() });
                    s.unread += 1;
                }
                drop(h);
                repaint(&host);
            }
            m @ (Msg::FileStart { .. } | Msg::FileChunk { .. } | Msg::FileEnd { .. }) => {
                match files.handle(&m) {
                    Some(FileEvent::Done(path)) => {
                        let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        outbox.send(Msg::Notice(format!("file-saved:{name}")));
                        toast(&host, format!("📁 {peer_name}: {}", path.display()));
                    }
                    Some(FileEvent::Failed(e)) => {
                        outbox.send(Msg::Notice(format!("file-failed:{e}")));
                    }
                    _ => {}
                }
            }
            Msg::Bye(_) => break,
            _ => {}
        }
    }
    injector.release_all();
    ctl.stop.store(true, Ordering::SeqCst);
    outbox.send(Msg::Bye(String::new()));
    reader.shutdown();
    host.lock().unwrap().sessions.retain(|s| s.sid != sid);
    repaint(&host);
    Ok(())
}

fn ask(host: &Host, name: &str, peer_id: u64, addr: &str, fp: &str, want_control: bool) -> Decision {
    let pid = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    host.lock().unwrap().pending.push(Pending {
        pid,
        peer_name: name.to_string(),
        peer_id,
        peer_addr: addr.to_string(),
        fingerprint: fp.to_string(),
        want_control,
        decision: None,
        since: Instant::now(),
    });
    repaint(host);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let mut h = host.lock().unwrap();
        let decided = h.pending.iter().find(|p| p.pid == pid).and_then(|p| p.decision);
        if let Some(d) = decided {
            h.pending.retain(|p| p.pid != pid);
            return d;
        }
        if Instant::now() > deadline {
            h.pending.retain(|p| p.pid != pid);
            drop(h);
            repaint(host);
            return Decision::Reject;
        }
    }
}

struct StreamCtl {
    settings: Mutex<StreamSettings>,
    keyframe: AtomicBool,
    stop: Arc<AtomicBool>,
}

fn stream_loop(ctl: Arc<StreamCtl>, outbox: Outbox, displays: Vec<DisplayInfo>) {
    let cursor_probe = Injector::new();
    let mut current: Option<StreamSettings> = None;
    let mut capturer: Option<Capturer> = None;
    let mut encoder: Option<VideoEncoder> = None;
    let mut last_raw: Option<RawFrame> = None;
    let mut last_change = Instant::now();
    let mut refinements = 0u32;
    let mut last_cursor = (f32::NAN, f32::NAN);

    while !ctl.stop.load(Ordering::Relaxed) {
        let settings = ctl.settings.lock().unwrap().clone();
        if current.as_ref() != Some(&settings) {
            let display_changed = current.as_ref().map(|c| c.display) != Some(settings.display);
            if display_changed || capturer.is_none() {
                let d = displays.get(settings.display).or(displays.first());
                capturer = d.and_then(Capturer::new);
                last_raw = None;
            }
            encoder = None;
            current = Some(settings.clone());
        }
        let Some(cap) = capturer.as_mut() else {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        let fps = settings.fps.clamp(5, 240);
        let interval = Duration::from_micros(1_000_000 / fps as u64);
        let started = Instant::now();

        // Cursor position (relative to the shown display).
        if let Some((cx, cy)) = cursor_probe.cursor() {
            let d = &cap.display;
            let nx = (cx - d.x) as f32 / d.width.max(1) as f32;
            let ny = (cy - d.y) as f32 / d.height.max(1) as f32;
            if (nx, ny) != last_cursor {
                last_cursor = (nx, ny);
                outbox.send(Msg::Cursor { x: nx, y: ny });
            }
        }

        // Do not queue video faster than the network can carry it.
        if outbox.video_busy(2) {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        let force = ctl.keyframe.swap(false, Ordering::SeqCst) || encoder.is_none();
        let grabbed = cap.grab(interval, force);
        let frame = match grabbed {
            Some(f) => {
                last_change = Instant::now();
                refinements = 0;
                Some(f)
            }
            None => {
                // Screen is still: re-send the last picture a few times so H.264 sharpens it.
                if refinements < 3
                    && last_change.elapsed() > Duration::from_millis(150 * (refinements as u64 + 1))
                    && settings.codec == proto::Codec::H264
                {
                    refinements += 1;
                    last_raw.as_ref().map(|f| RawFrame { width: f.width, height: f.height, rgba: f.rgba.clone() })
                } else {
                    None
                }
            }
        };
        let Some(frame) = frame else {
            if force {
                ctl.keyframe.store(true, Ordering::SeqCst);
            }
            continue;
        };
        let (tw, th) = capture::target_size(frame.width, frame.height, settings.resolution);
        let frame = if (tw, th) != (frame.width, frame.height) {
            if tw == frame.width & !1 && th == frame.height & !1 {
                capture::crop_even(frame)
            } else {
                RawFrame { width: tw, height: th, rgba: capture::resize_rgba(&frame.rgba, frame.width, frame.height, tw, th) }
            }
        } else {
            frame
        };
        let mut force_key = force;
        if encoder.as_ref().map(|e| e.size() != (frame.width, frame.height) || e.codec() != settings.codec).unwrap_or(true) {
            encoder = VideoEncoder::new(settings.codec, frame.width, frame.height, settings.quality, fps);
            outbox.send(Msg::VideoFormat {
                display: settings.display,
                width: frame.width,
                height: frame.height,
                codec: settings.codec,
            });
            force_key = true;
        }
        if let Some(enc) = encoder.as_mut() {
            if let Some((data, key)) = enc.encode(&frame, force_key) {
                if !outbox.send(Msg::Video { codec: enc.codec(), key, data }) {
                    break;
                }
            }
        }
        last_raw = Some(frame);
        let spent = started.elapsed();
        if spent < interval {
            std::thread::sleep(interval - spent);
        }
    }
}
