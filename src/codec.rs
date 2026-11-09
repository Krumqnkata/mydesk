//! Video codecs: H.264 for smooth motion, and a lossless tile codec that only
//! sends the parts of the screen that changed (crisp text, very low CPU).

use std::time::Instant;

use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
    RateControlMode, UsageType,
};
use openh264::formats::{YUVBuffer, YUVSource};

use crate::capture::RawFrame;
use crate::proto::{Codec, Quality};

const TILE: usize = 64;

pub fn bitrate_for(quality: Quality, w: u32, h: u32, fps: u32) -> u32 {
    let base_mbps = match quality {
        Quality::Speed => 5.0,
        Quality::Balanced => 12.0,
        Quality::Best => 28.0,
    };
    let pixels = (w as f64 * h as f64) / (1920.0 * 1080.0);
    let motion = (fps as f64 / 60.0).sqrt();
    let mbps = (base_mbps * pixels.max(0.25) * motion).clamp(1.5, 80.0);
    (mbps * 1_000_000.0) as u32
}

pub enum VideoEncoder {
    H264 { enc: Encoder, width: u32, height: u32, start: Instant },
    Tiles { prev: Vec<u8>, width: u32, height: u32 },
}

impl VideoEncoder {
    pub fn new(codec: Codec, width: u32, height: u32, quality: Quality, fps: u32) -> Option<Self> {
        match codec {
            Codec::H264 => {
                let threads = std::thread::available_parallelism()
                    .map(|n| n.get().clamp(2, 8) as u16)
                    .unwrap_or(2);
                let cfg = EncoderConfig::new()
                    .bitrate(BitRate::from_bps(bitrate_for(quality, width, height, fps)))
                    .max_frame_rate(FrameRate::from_hz(fps as f32))
                    .usage_type(UsageType::ScreenContentRealTime)
                    .rate_control_mode(RateControlMode::Bitrate)
                    .complexity(if quality == Quality::Best { Complexity::Medium } else { Complexity::Low })
                    .skip_frames(true)
                    .adaptive_quantization(false)
                    .background_detection(false)
                    .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 30))
                    .num_threads(threads);
                // Size-limited slices let OpenH264 spread one frame over several cores.
                let cfg = cfg.max_slice_len(16_000);
                let enc = Encoder::with_api_config(OpenH264API::from_source(), cfg).ok()?;
                Some(Self::H264 { enc, width, height, start: Instant::now() })
            }
            Codec::Lossless => Some(Self::Tiles { prev: Vec::new(), width, height }),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        match self {
            Self::H264 { width, height, .. } | Self::Tiles { width, height, .. } => (*width, *height),
        }
    }

    pub fn codec(&self) -> Codec {
        match self {
            Self::H264 { .. } => Codec::H264,
            Self::Tiles { .. } => Codec::Lossless,
        }
    }

    /// Encodes one frame. Returns the payload and whether it is a key frame,
    /// or `None` when there is nothing to send.
    pub fn encode(&mut self, frame: &RawFrame, force_key: bool) -> Option<(Vec<u8>, bool)> {
        match self {
            Self::H264 { enc, width, height, start } => {
                if frame.width != *width || frame.height != *height {
                    return None;
                }
                if force_key {
                    enc.force_intra_frame();
                }
                let yuv = YUVBuffer::from_vec(rgba_to_i420(&frame.rgba, frame.width as usize, frame.height as usize), frame.width as usize, frame.height as usize);
                let ts = openh264::Timestamp::from_millis(start.elapsed().as_millis() as u64);
                let bs = enc.encode_at(&yuv, ts).ok()?;
                let ft = bs.frame_type();
                if matches!(ft, FrameType::Skip | FrameType::Invalid) {
                    return None;
                }
                let data = bs.to_vec();
                if data.is_empty() {
                    return None;
                }
                Some((data, matches!(ft, FrameType::IDR | FrameType::I)))
            }
            Self::Tiles { prev, width, height } => {
                if frame.width != *width || frame.height != *height {
                    return None;
                }
                let key = force_key || prev.len() != frame.rgba.len();
                let (w, h) = (*width as usize, *height as usize);
                let mut body: Vec<u8> = Vec::new();
                let mut count: u32 = 0;
                for ty in (0..h).step_by(TILE) {
                    for tx in (0..w).step_by(TILE) {
                        let tw = TILE.min(w - tx);
                        let th = TILE.min(h - ty);
                        let changed = key
                            || (ty..ty + th).any(|y| {
                                let s = (y * w + tx) * 4;
                                frame.rgba[s..s + tw * 4] != prev[s..s + tw * 4]
                            });
                        if !changed {
                            continue;
                        }
                        count += 1;
                        for v in [tx as u16, ty as u16, tw as u16, th as u16] {
                            body.extend_from_slice(&v.to_le_bytes());
                        }
                        for y in ty..ty + th {
                            let s = (y * w + tx) * 4;
                            for px in frame.rgba[s..s + tw * 4].chunks_exact(4) {
                                body.extend_from_slice(&px[..3]);
                            }
                        }
                    }
                }
                if count == 0 {
                    return None;
                }
                prev.clear();
                prev.extend_from_slice(&frame.rgba);
                let mut raw = Vec::with_capacity(body.len() + 12);
                raw.extend_from_slice(&(*width).to_le_bytes());
                raw.extend_from_slice(&(*height).to_le_bytes());
                raw.extend_from_slice(&count.to_le_bytes());
                raw.extend_from_slice(&body);
                Some((lz4_flex::compress_prepend_size(&raw), key))
            }
        }
    }
}

pub struct VideoDecoder {
    h264: Option<Decoder>,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl VideoDecoder {
    pub fn new() -> Self {
        Self { h264: None, width: 0, height: 0, rgba: Vec::new() }
    }

    pub fn reset(&mut self) {
        self.h264 = None;
    }

    /// Returns Ok(true) when the picture changed, Err when a key frame is needed.
    pub fn decode(&mut self, codec: Codec, data: &[u8]) -> Result<bool, ()> {
        match codec {
            Codec::H264 => {
                if self.h264.is_none() {
                    self.h264 = Decoder::new().ok();
                }
                let dec = self.h264.as_mut().ok_or(())?;
                match dec.decode(data) {
                    Ok(Some(yuv)) => {
                        let (w, h) = yuv.dimensions();
                        if w as u32 != self.width || h as u32 != self.height || self.rgba.len() != w * h * 4 {
                            self.width = w as u32;
                            self.height = h as u32;
                            self.rgba = vec![0u8; w * h * 4];
                        }
                        yuv.write_rgba8(&mut self.rgba);
                        Ok(true)
                    }
                    Ok(None) => Ok(false),
                    Err(_) => {
                        self.h264 = None;
                        Err(())
                    }
                }
            }
            Codec::Lossless => {
                let raw = lz4_flex::decompress_size_prepended(data).map_err(|_| ())?;
                if raw.len() < 12 {
                    return Err(());
                }
                let rd = |o: usize| u32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
                let (w, h, count) = (rd(0), rd(4), rd(8));
                if w != self.width || h != self.height || self.rgba.len() != (w * h * 4) as usize {
                    self.width = w;
                    self.height = h;
                    self.rgba = vec![0u8; (w * h * 4) as usize];
                    for px in self.rgba.chunks_exact_mut(4) {
                        px[3] = 255;
                    }
                }
                let mut o = 12;
                let wu = w as usize;
                for _ in 0..count {
                    if o + 8 > raw.len() {
                        return Err(());
                    }
                    let r16 = |o: usize| u16::from_le_bytes([raw[o], raw[o + 1]]) as usize;
                    let (tx, ty, tw, th) = (r16(o), r16(o + 2), r16(o + 4), r16(o + 6));
                    o += 8;
                    if tx + tw > wu || ty + th > h as usize || o + tw * th * 3 > raw.len() {
                        return Err(());
                    }
                    for y in ty..ty + th {
                        let d = (y * wu + tx) * 4;
                        for (i, px) in raw[o..o + tw * 3].chunks_exact(3).enumerate() {
                            let p = d + i * 4;
                            self.rgba[p..p + 3].copy_from_slice(px);
                            self.rgba[p + 3] = 255;
                        }
                        o += tw * 3;
                    }
                }
                Ok(true)
            }
        }
    }
}

/// Fast RGBA -> I420 (BT.601, limited range), split across CPU cores.
pub fn rgba_to_i420(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (cw, ch) = (w / 2, h / 2);
    let mut out = vec![0u8; w * h + 2 * cw * ch];
    let (y_plane, uv) = out.split_at_mut(w * h);
    let (u_plane, v_plane) = uv.split_at_mut(cw * ch);
    let threads = std::thread::available_parallelism().map(|n| n.get().clamp(1, 8)).unwrap_or(4);
    let rows_per = (ch + threads - 1) / threads; // in chroma rows (= 2 luma rows)
    std::thread::scope(|scope| {
        let y_chunks = y_plane.chunks_mut(rows_per * 2 * w);
        let u_chunks = u_plane.chunks_mut(rows_per * cw);
        let v_chunks = v_plane.chunks_mut(rows_per * cw);
        for (i, ((yc, uc), vc)) in y_chunks.zip(u_chunks).zip(v_chunks).enumerate() {
            scope.spawn(move || {
                let first = i * rows_per;
                let rows = uc.len() / cw.max(1);
                for r in 0..rows {
                    let cy = first + r;
                    let src0 = &rgba[(cy * 2) * w * 4..(cy * 2 + 1) * w * 4];
                    let src1 = &rgba[(cy * 2 + 1) * w * 4..(cy * 2 + 2) * w * 4];
                    let (y0, y1) = yc[r * 2 * w..(r * 2 + 2) * w].split_at_mut(w);
                    for cx in 0..cw {
                        let mut rs = 0i32;
                        let mut gs = 0i32;
                        let mut bs = 0i32;
                        for (src, yrow) in [(src0, &mut *y0), (src1, &mut *y1)] {
                            for dx in 0..2 {
                                let x = cx * 2 + dx;
                                let p = x * 4;
                                let (r_, g_, b_) = (src[p] as i32, src[p + 1] as i32, src[p + 2] as i32);
                                yrow[x] = (((66 * r_ + 129 * g_ + 25 * b_ + 128) >> 8) + 16) as u8;
                                rs += r_;
                                gs += g_;
                                bs += b_;
                            }
                        }
                        let (r_, g_, b_) = (rs >> 2, gs >> 2, bs >> 2);
                        uc[r * cw + cx] = (((-38 * r_ - 74 * g_ + 112 * b_ + 128) >> 8) + 128).clamp(0, 255) as u8;
                        vc[r * cw + cx] = (((112 * r_ - 94 * g_ - 18 * b_ + 128) >> 8) + 128).clamp(0, 255) as u8;
                    }
                }
            });
        }
    });
    out
}
