//! Screen capture. On Windows the desktop-duplication recorder gives us frames
//! only when something changed; elsewhere (and as a fallback) we poll.

use std::time::{Duration, Instant};

use crate::proto::{DisplayInfo, Resolution};

pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn list_displays() -> Vec<DisplayInfo> {
    let Ok(monitors) = xcap::Monitor::all() else { return Vec::new() };
    let mut out: Vec<DisplayInfo> = monitors
        .iter()
        .enumerate()
        .map(|(i, m)| DisplayInfo {
            name: m
                .friendly_name()
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| m.name().ok())
                .unwrap_or_else(|| format!("Display {}", i + 1)),
            x: m.x().unwrap_or(0),
            y: m.y().unwrap_or(0),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
            primary: m.is_primary().unwrap_or(i == 0),
        })
        .collect();
    // Primary display first, so index 0 is always the main screen.
    out.sort_by_key(|d| !d.primary);
    out
}

fn monitor_for(display: &DisplayInfo) -> Option<xcap::Monitor> {
    let monitors = xcap::Monitor::all().ok()?;
    monitors
        .into_iter()
        .find(|m| m.x().ok() == Some(display.x) && m.y().ok() == Some(display.y))
}

pub struct Capturer {
    monitor: xcap::Monitor,
    pub display: DisplayInfo,
    #[cfg(windows)]
    recorder: Option<(xcap::VideoRecorder, std::sync::mpsc::Receiver<xcap::Frame>)>,
    #[cfg(windows)]
    recorder_retry: Instant,
    last_poll: Instant,
}

impl Capturer {
    pub fn new(display: &DisplayInfo) -> Option<Self> {
        let monitor = monitor_for(display)?;
        Some(Self {
            monitor,
            display: display.clone(),
            #[cfg(windows)]
            recorder: None,
            #[cfg(windows)]
            recorder_retry: Instant::now(),
            last_poll: Instant::now() - Duration::from_secs(1),
        })
    }

    /// Waits up to `timeout` for a new frame. `None` means the screen did not change.
    pub fn grab(&mut self, timeout: Duration, force: bool) -> Option<RawFrame> {
        #[cfg(windows)]
        {
            if self.recorder.is_none() && Instant::now() >= self.recorder_retry {
                match self.monitor.video_recorder() {
                    Ok((rec, rx)) => {
                        if rec.start().is_ok() {
                            self.recorder = Some((rec, rx));
                        }
                    }
                    Err(_) => self.recorder_retry = Instant::now() + Duration::from_secs(3),
                }
            }
            if !force {
                if let Some((_, rx)) = &self.recorder {
                    match rx.recv_timeout(timeout) {
                        Ok(f) => {
                            return Some(RawFrame { width: f.width, height: f.height, rgba: f.raw });
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return None,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            // Desktop switched (lock screen, UAC, resolution change): rebuild soon.
                            self.recorder = None;
                            self.recorder_retry = Instant::now() + Duration::from_millis(500);
                        }
                    }
                }
            }
        }
        // Polling path: respect the requested frame interval.
        let since = self.last_poll.elapsed();
        if since < timeout && !force {
            std::thread::sleep(timeout - since);
        }
        self.last_poll = Instant::now();
        let img = self.monitor.capture_image().ok()?;
        let (w, h) = (img.width(), img.height());
        Some(RawFrame { width: w, height: h, rgba: img.into_raw() })
    }
}

/// Target size for a resolution preset (keeps aspect ratio, even dimensions).
pub fn target_size(w: u32, h: u32, res: Resolution) -> (u32, u32) {
    let max_h = match res {
        Resolution::Native => u32::MAX,
        Resolution::Max1080 => 1080,
        Resolution::Max720 => 720,
    };
    let (mut tw, mut th) = if h > max_h {
        let s = max_h as f64 / h as f64;
        ((w as f64 * s).round() as u32, max_h)
    } else {
        (w, h)
    };
    tw &= !1;
    th &= !1;
    (tw.max(2), th.max(2))
}

/// Fast bilinear resize of an RGBA buffer.
pub fn resize_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if sw == dw && sh == dh {
        return src.to_vec();
    }
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    let sx = sw as f32 / dw as f32;
    let sy = sh as f32 / dh as f32;
    let stride = sw as usize * 4;
    for y in 0..dh as usize {
        let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let y0 = (fy as usize).min(sh as usize - 1);
        let y1 = (y0 + 1).min(sh as usize - 1);
        let wy = ((fy - y0 as f32) * 256.0) as u32;
        let row = y * dw as usize * 4;
        for x in 0..dw as usize {
            let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
            let x0 = (fx as usize).min(sw as usize - 1);
            let x1 = (x0 + 1).min(sw as usize - 1);
            let wx = ((fx - x0 as f32) * 256.0) as u32;
            let p00 = y0 * stride + x0 * 4;
            let p01 = y0 * stride + x1 * 4;
            let p10 = y1 * stride + x0 * 4;
            let p11 = y1 * stride + x1 * 4;
            for c in 0..3 {
                let top = src[p00 + c] as u32 * (256 - wx) + src[p01 + c] as u32 * wx;
                let bot = src[p10 + c] as u32 * (256 - wx) + src[p11 + c] as u32 * wx;
                out[row + x * 4 + c] = ((top * (256 - wy) + bot * wy) >> 16) as u8;
            }
            out[row + x * 4 + 3] = 255;
        }
    }
    out
}

/// Crops an RGBA buffer to even dimensions (needed by the video encoder).
pub fn crop_even(frame: RawFrame) -> RawFrame {
    let (w, h) = (frame.width & !1, frame.height & !1);
    if w == frame.width && h == frame.height {
        return frame;
    }
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let start = y * frame.width as usize * 4;
        out.extend_from_slice(&frame.rgba[start..start + w as usize * 4]);
    }
    RawFrame { width: w, height: h, rgba: out }
}
