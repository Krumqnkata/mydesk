//! Clipboard sync, chat lines and file transfer, shared by both sides of a session.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use crate::proto::{Msg, Outbox};

#[derive(Clone, Debug)]
pub struct ChatLine {
    pub mine: bool,
    pub text: String,
    pub time: String,
}

pub fn clock() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// Starts a background thread that keeps both clipboards in sync (text only).
/// Returns a sender used to put text received from the other side on this clipboard.
pub fn spawn_clipboard_sync(outbox: Outbox, stop: Arc<AtomicBool>, enabled: Arc<AtomicBool>) -> Sender<String> {
    let (tx, rx) = channel::<String>();
    let _ = std::thread::Builder::new().name("clipboard".into()).spawn(move || {
        let Ok(mut cb) = arboard::Clipboard::new() else { return };
        let mut last = cb.get_text().unwrap_or_default();
        while !stop.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(600)) {
                Ok(text) => {
                    if enabled.load(Ordering::Relaxed) && text != last {
                        let _ = cb.set_text(text.clone());
                        last = text;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(_) => {}
            }
            if !enabled.load(Ordering::Relaxed) {
                continue;
            }
            if let Ok(now) = cb.get_text() {
                if now != last && !now.is_empty() && now.len() < 4 * 1024 * 1024 {
                    last = now.clone();
                    if !outbox.send(Msg::Clipboard(now)) {
                        break;
                    }
                }
            }
        }
    });
    tx
}

pub fn downloads_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("MyDesk")
}

struct Incoming {
    file: File,
    path: PathBuf,
    size: u64,
    got: u64,
}

#[derive(Default)]
pub struct FileReceiver {
    files: HashMap<u32, Incoming>,
}

#[allow(dead_code)]
pub enum FileEvent {
    Started(String),
    Done(PathBuf),
    Failed(String),
}

impl FileReceiver {
    pub fn handle(&mut self, msg: &Msg) -> Option<FileEvent> {
        match msg {
            Msg::FileStart { id, name, size } => {
                let dir = downloads_dir();
                let _ = std::fs::create_dir_all(&dir);
                let clean: String = Path::new(name)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".into())
                    .chars()
                    .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
                    .collect();
                let mut path = dir.join(&clean);
                let mut n = 1;
                while path.exists() {
                    let stem = Path::new(&clean).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    let ext = Path::new(&clean).extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
                    path = dir.join(format!("{stem} ({n}){ext}"));
                    n += 1;
                }
                match File::create(&path) {
                    Ok(file) => {
                        self.files.insert(*id, Incoming { file, path, size: *size, got: 0 });
                        Some(FileEvent::Started(clean))
                    }
                    Err(e) => Some(FileEvent::Failed(e.to_string())),
                }
            }
            Msg::FileChunk { id, data } => {
                let f = self.files.get_mut(id)?;
                if f.file.write_all(data).is_err() {
                    let f = self.files.remove(id)?;
                    return Some(FileEvent::Failed(f.path.display().to_string()));
                }
                f.got += data.len() as u64;
                let _ = f.size;
                None
            }
            Msg::FileEnd { id } => {
                let f = self.files.remove(id)?;
                drop(f.file);
                Some(FileEvent::Done(f.path))
            }
            _ => None,
        }
    }
}

/// Sends files in the background without flooding the connection.
pub fn send_files(outbox: Outbox, paths: Vec<PathBuf>, progress: Arc<std::sync::Mutex<Option<(String, f32)>>>) {
    let _ = std::thread::Builder::new().name("file-send".into()).spawn(move || {
        for (i, path) in paths.into_iter().enumerate() {
            let Ok(mut file) = File::open(&path) else { continue };
            if path.is_dir() {
                continue;
            }
            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
            let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let id = (crate::proto::now_ms() as u32).wrapping_add(i as u32);
            if !outbox.send(Msg::FileStart { id, name: name.clone(), size }) {
                break;
            }
            let mut sent: u64 = 0;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                while outbox.queued.load(Ordering::SeqCst) > 6 {
                    std::thread::sleep(Duration::from_millis(4));
                }
                let n = match file.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                sent += n as u64;
                if !outbox.send(Msg::FileChunk { id, data: buf[..n].to_vec() }) {
                    return;
                }
                if let Ok(mut p) = progress.lock() {
                    *p = Some((name.clone(), if size > 0 { sent as f32 / size as f32 } else { 1.0 }));
                }
            }
            outbox.send(Msg::FileEnd { id });
        }
        if let Ok(mut p) = progress.lock() {
            *p = None;
        }
    });
}
