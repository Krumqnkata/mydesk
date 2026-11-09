//! Settings, this desk's ID and the address book, saved as JSON in the user's config folder.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::proto::{Codec, DEFAULT_PORT, Quality, Resolution, random_bytes};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Bg,
    En,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMode {
    /// A password lets people straight in; without one, you get an accept window.
    PasswordOrAsk,
    /// Only the accept window.
    AskOnly,
    /// Only a password (unattended access).
    PasswordOnly,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Contact {
    pub target: String,
    pub name: String,
    pub favorite: bool,
    pub last_used: u64,
    /// Remembered password key (never the password itself).
    #[serde(default)]
    pub saved_key: Option<[u8; 32]>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub id: u64,
    pub name: String,
    pub lang: Lang,
    pub port: u16,
    pub allow_incoming: bool,
    pub access: AccessMode,
    /// Key derived from the permanent password, if one is set.
    pub permanent_key: Option<[u8; 32]>,
    pub relay: String,
    pub codec: Codec,
    pub quality: Quality,
    pub fps: u32,
    pub resolution: Resolution,
    pub sync_clipboard: bool,
    pub show_stats: bool,
    pub lan_discovery: bool,
    pub contacts: Vec<Contact>,
}

impl Default for Config {
    fn default() -> Self {
        let r = random_bytes::<8>();
        let id = 100_000_000 + u64::from_le_bytes(r) % 900_000_000;
        Self {
            id,
            name: hostname(),
            lang: Lang::Bg,
            port: DEFAULT_PORT,
            allow_incoming: true,
            access: AccessMode::PasswordOrAsk,
            permanent_key: None,
            relay: String::new(),
            codec: Codec::H264,
            quality: Quality::Balanced,
            fps: 60,
            resolution: Resolution::Native,
            sync_clipboard: true,
            show_stats: true,
            lan_discovery: true,
            contacts: Vec::new(),
        }
    }
}

pub fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "My computer".into())
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("MyDesk").join("config.json"))
}

impl Config {
    pub fn load() -> Self {
        if let Some(p) = path() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(cfg) = serde_json::from_str::<Config>(&text) {
                    return cfg;
                }
            }
        }
        let cfg = Config::default();
        cfg.save();
        cfg
    }

    pub fn save(&self) {
        if let Some(p) = path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(text) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, text);
            }
        }
    }

    pub fn remember(&mut self, target: &str, name: &str, key: Option<[u8; 32]>) {
        let now = crate::proto::now_ms() / 1000;
        if let Some(c) = self.contacts.iter_mut().find(|c| c.target == target) {
            c.last_used = now;
            if !name.is_empty() {
                c.name = name.to_string();
            }
            if key.is_some() {
                c.saved_key = key;
            }
        } else {
            self.contacts.push(Contact {
                target: target.to_string(),
                name: name.to_string(),
                favorite: false,
                last_used: now,
                saved_key: key,
            });
        }
        self.contacts.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        self.save();
    }
}

/// "123456789" -> "123 456 789"
pub fn format_id(id: u64) -> String {
    let s = id.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// Parses a 9-digit desk ID ("123 456 789" or "123-456-789").
pub fn parse_id(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    if digits.len() == 9 && digits.chars().all(|c| c.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}

/// A short, easy-to-read one-time password (no 0/O, 1/l confusion).
pub fn new_one_time_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let r = random_bytes::<8>();
    r.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}
