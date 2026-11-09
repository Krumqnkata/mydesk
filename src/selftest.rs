//! `mydesk --selftest`: hosts and connects to itself over loopback to prove the
//! whole pipeline works (handshake, passwords, capture, codecs, relay). View-only:
//! it never moves the mouse or types.

use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::codec::VideoDecoder;
use crate::config::AccessMode;
use crate::host::{self, Host, HostState, Listening, RelayState};
use crate::proto::{self, Codec, Msg, Quality, Resolution, StreamSettings, Traffic};

const PASSWORD: &str = "selftest-pass";

fn make_host() -> Host {
    Arc::new(Mutex::new(HostState {
        id: 123_456_789,
        name: "selftest-host".into(),
        allow_incoming: true,
        access: AccessMode::PasswordOnly,
        one_time_password: PASSWORD.into(),
        permanent_key: None,
        sync_clipboard: false,
        sessions: Vec::new(),
        pending: Vec::new(),
        toasts: Vec::new(),
        listening: Listening::Starting,
        relay: RelayState::Unused,
        ctx: None,
    }))
}

fn session(stream: TcpStream, password: &str, codec: Codec, secs: f32) -> Result<String, String> {
    let mut sec = proto::client_handshake(stream, "selftest-viewer", 987_654_321).map_err(|e| e.to_string())?;
    let fp = sec.fingerprint.clone();
    let proof = proto::password_proof(&proto::password_key(password), &sec.transcript);
    sec.send(&Msg::Auth { proof: Some(proof), want_control: false }).map_err(|e| e.to_string())?;
    let Msg::AuthResult { ok, reason, displays, .. } = sec.recv().map_err(|e| e.to_string())? else {
        return Err("no auth result".into());
    };
    if !ok {
        return Err(format!("rejected: {reason}"));
    }
    let traffic = Arc::new(Traffic::default());
    let (outbox, mut reader) = proto::split(sec, traffic.clone()).map_err(|e| e.to_string())?;
    outbox.send(Msg::Stream(StreamSettings { display: 0, codec, quality: Quality::Balanced, fps: 60, resolution: Resolution::Native }));
    let mut dec = VideoDecoder::new();
    let start = Instant::now();
    let mut frames = 0;
    let mut keys = 0;
    let mut first_ms = None;
    let mut decode_ms = 0.0f32;
    let mut pong = None;
    outbox.send(Msg::Ping(proto::now_ms()));
    while start.elapsed().as_secs_f32() < secs {
        let msg = reader.recv().map_err(|e| e.to_string())?;
        match msg {
            Msg::Video { codec, key, data } => {
                let t = Instant::now();
                if dec.decode(codec, &data).map_err(|_| "decode failed")? {
                    frames += 1;
                    decode_ms += t.elapsed().as_secs_f32() * 1000.0;
                    first_ms.get_or_insert(start.elapsed().as_millis());
                }
                if key {
                    keys += 1;
                }
            }
            Msg::Pong(t) => pong = Some(proto::now_ms() - t),
            _ => {}
        }
    }
    outbox.send(Msg::Bye("done".into()));
    reader.shutdown();
    let mb = traffic.bytes_in.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1_000_000.0;
    Ok(format!(
        "{} displays, picture {}x{}, {frames} frames ({keys} key), first picture after {} ms, avg decode {:.1} ms, {:.2} MB received, ping {} ms, code {fp}",
        displays.len(),
        dec.width,
        dec.height,
        first_ms.unwrap_or(0),
        if frames > 0 { decode_ms / frames as f32 } else { 0.0 },
        mb,
        pong.map(|p| p.to_string()).unwrap_or("-".into())
    ))
}

fn bench() -> String {
    use crate::capture::RawFrame;
    use crate::codec::VideoEncoder;
    let (w, h) = (1920u32, 1080u32);
    let mut enc = VideoEncoder::new(Codec::H264, w, h, Quality::Balanced, 60).expect("encoder");
    let mut dec = VideoDecoder::new();
    let mut enc_ms = 0.0;
    let mut bytes = 0usize;
    let n = 60;
    for i in 0..n {
        // A moving gradient with a sliding box: lots of motion, like a game or video.
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = (y * w as usize + x) * 4;
                let inside = (x + i * 24) % 700 < 300 && (y + i * 9) % 500 < 200;
                rgba[p] = ((x + i * 7) % 256) as u8;
                rgba[p + 1] = if inside { 240 } else { ((y + i * 3) % 256) as u8 };
                rgba[p + 2] = (((x ^ y) + i) % 256) as u8;
                rgba[p + 3] = 255;
            }
        }
        let frame = RawFrame { width: w, height: h, rgba };
        let t = Instant::now();
        if let Some((data, _)) = enc.encode(&frame, i == 0) {
            enc_ms += t.elapsed().as_secs_f32() * 1000.0;
            bytes += data.len();
            let _ = dec.decode(Codec::H264, &data);
        }
    }
    let avg = enc_ms / n as f32;
    format!("1080p H.264 encode {avg:.1} ms/frame (~{:.0} fps max on this CPU), {:.1} Mbps at 60 fps", 1000.0 / avg.max(0.1), bytes as f32 * 8.0 / (n as f32 / 60.0) / 1e6)
}

pub fn run() -> i32 {
    let mut failures = 0;
    println!("[INFO] {}", bench());
    let host = make_host();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    {
        let host = host.clone();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                host::serve_stream(s, host.clone());
            }
        });
    }
    let mut check = |name: &str, r: Result<String, String>, expect_ok: bool| {
        let pass = r.is_ok() == expect_ok;
        if !pass {
            failures += 1;
        }
        println!("[{}] {name}: {}", if pass { "PASS" } else { "FAIL" }, r.unwrap_or_else(|e| e));
    };

    check("H.264 stream (direct)", session(TcpStream::connect(addr).unwrap(), PASSWORD, Codec::H264, 3.0), true);
    check("Lossless stream (direct)", session(TcpStream::connect(addr).unwrap(), PASSWORD, Codec::Lossless, 2.0), true);
    check("wrong password is refused", session(TcpStream::connect(addr).unwrap(), "nope", Codec::H264, 1.0), false);

    // Relay: run a relay, register the host with it, connect by ID through it.
    let relay_port = 47990;
    std::thread::spawn(move || {
        let _ = crate::net::run_relay_server(relay_port);
    });
    std::thread::sleep(Duration::from_millis(300));
    let relay = Arc::new(Mutex::new(format!("127.0.0.1:{relay_port}")));
    crate::net::start_relay_client(host.clone(), relay.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.lock().unwrap().relay != RelayState::Online && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(200));
    let via_relay = crate::net::relay_connect(&relay.lock().unwrap(), 123_456_789)
        .map_err(|e| e.to_string())
        .and_then(|s| session(s, PASSWORD, Codec::H264, 2.0));
    check("H.264 stream through relay (by ID)", via_relay, true);
    check(
        "unknown ID through relay is offline",
        crate::net::relay_connect(&relay.lock().unwrap(), 111_111_111).map(|_| String::new()).map_err(|e| e.to_string()),
        false,
    );

    println!("{}", if failures == 0 { "ALL PASSED" } else { "SOME CHECKS FAILED" });
    if failures == 0 { 0 } else { 1 }
}
