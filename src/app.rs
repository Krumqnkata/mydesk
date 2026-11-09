//! The MyDesk window: home, address book, nearby desks, settings, and remote sessions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align, Align2, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Ui, Vec2};

use crate::client::{self, ConnectRequest, Session, Status};
use crate::config::{AccessMode, Config, Lang, format_id, new_one_time_password, parse_id};
use crate::extras::{ChatLine, clock};
use crate::host::{self, Decision, Host, HostState, Listening, RelayState};
use crate::net::{self, Announce, Peers};
use crate::proto::{self, Codec, Msg, Quality, Resolution, StreamSettings};
use crate::theme::{self, *};
use crate::view;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Book,
    Nearby,
    Settings,
}

pub struct App {
    pub cfg: Config,
    pub host: Host,
    pub peers: Peers,
    announce: Arc<Mutex<Announce>>,
    relay: Arc<Mutex<String>>,
    pub sessions: Vec<Session>,
    pub active: Option<usize>,
    page: Page,
    connect_input: String,
    connect_password: String,
    connect_view_only: bool,
    show_otp: bool,
    new_permanent: String,
    toasts: Vec<(String, Instant)>,
    book_filter: String,
    renaming: Option<(String, String)>,
    host_chat_input: HashMap<u64, String>,
    host_chat_open: Option<u64>,
    pub fullscreen: bool,
    screenshot: Option<PathBuf>,
    shot: Arc<Mutex<Option<Arc<egui::ColorImage>>>>,
    shot_requested: bool,
    frames: u64,
    networking: bool,
}

pub fn tr(lang: Lang, bg: &'static str, en: &'static str) -> &'static str {
    match lang {
        Lang::Bg => bg,
        Lang::En => en,
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<PathBuf>) -> Self {
        theme::install(&cc.egui_ctx);
        let mut cfg = Config::load();
        // MYDESK_DOCS: neutral name/ID for public screenshots (never saved).
        if std::env::var("MYDESK_DOCS").is_ok() {
            cfg.name = "Office-PC".into();
            cfg.id = 482_913_057;
        }
        let host: Host = Arc::new(Mutex::new(HostState {
            id: cfg.id,
            name: cfg.name.clone(),
            allow_incoming: cfg.allow_incoming,
            access: cfg.access,
            one_time_password: new_one_time_password(),
            permanent_key: cfg.permanent_key,
            sync_clipboard: cfg.sync_clipboard,
            sessions: Vec::new(),
            pending: Vec::new(),
            toasts: Vec::new(),
            listening: Listening::Starting,
            relay: RelayState::Unused,
            ctx: Some(cc.egui_ctx.clone()),
        }));
        let peers: Peers = Arc::default();
        let announce = Arc::new(Mutex::new(Announce {
            id: cfg.id,
            name: cfg.name.clone(),
            port: cfg.port,
            enabled: cfg.allow_incoming && cfg.lan_discovery,
        }));
        let relay = Arc::new(Mutex::new(cfg.relay.clone()));
        // A screenshot run must not open ports or trigger firewall prompts.
        let networking = screenshot.is_none();
        if networking {
            host::start_listener(host.clone(), cfg.port);
            net::start_discovery(peers.clone(), announce.clone(), cc.egui_ctx.clone());
            net::start_relay_client(host.clone(), relay.clone());
        } else {
            host.lock().unwrap().listening = Listening::On(cfg.port);
        }
        let mut app = Self {
            cfg,
            host,
            peers,
            announce,
            relay,
            sessions: Vec::new(),
            active: None,
            // MYDESK_PAGE lets docs screenshots open a specific page.
            page: match std::env::var("MYDESK_PAGE").as_deref() {
                Ok("settings") => Page::Settings,
                Ok("book") => Page::Book,
                Ok("nearby") => Page::Nearby,
                _ => Page::Home,
            },
            connect_input: String::new(),
            connect_password: String::new(),
            connect_view_only: false,
            show_otp: true,
            new_permanent: String::new(),
            toasts: Vec::new(),
            book_filter: String::new(),
            renaming: None,
            host_chat_input: HashMap::new(),
            host_chat_open: None,
            fullscreen: false,
            screenshot,
            shot: Arc::default(),
            shot_requested: false,
            frames: 0,
            networking,
        };
        // Developer preview: MYDESK_DEMO=1 hosts on loopback and opens a view-only session to itself.
        if std::env::var("MYDESK_DEMO").is_ok() {
            if let Ok(l) = std::net::TcpListener::bind("127.0.0.1:0") {
                let port = l.local_addr().map(|a| a.port()).unwrap_or(0);
                let h = app.host.clone();
                h.lock().unwrap().access = AccessMode::PasswordOnly;
                let pw = h.lock().unwrap().one_time_password.clone();
                std::thread::spawn(move || {
                    for s in l.incoming().flatten() {
                        host::serve_stream(s, h.clone());
                    }
                });
                app.open_session(&cc.egui_ctx, &format!("127.0.0.1:{port}"), Some(pw), true);
                app.cfg.contacts.retain(|c| !c.target.starts_with("127.0.0.1"));
                app.cfg.save();
            }
        }
        app
    }

    pub fn t(&self, bg: &'static str, en: &'static str) -> &'static str {
        tr(self.cfg.lang, bg, en)
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push((text.into(), Instant::now()));
    }

    fn sync_host_settings(&mut self) {
        {
            let mut h = self.host.lock().unwrap();
            h.name = self.cfg.name.clone();
            h.allow_incoming = self.cfg.allow_incoming;
            h.access = self.cfg.access;
            h.permanent_key = self.cfg.permanent_key;
            h.sync_clipboard = self.cfg.sync_clipboard;
        }
        {
            let mut a = self.announce.lock().unwrap();
            a.name = self.cfg.name.clone();
            a.enabled = self.cfg.allow_incoming && self.cfg.lan_discovery;
            if let Listening::On(p) = self.host.lock().unwrap().listening {
                a.port = p;
            }
        }
        *self.relay.lock().unwrap() = self.cfg.relay.clone();
        self.cfg.save();
    }

    pub fn default_stream(&self) -> StreamSettings {
        StreamSettings {
            display: 0,
            codec: self.cfg.codec,
            quality: self.cfg.quality,
            fps: self.cfg.fps,
            resolution: self.cfg.resolution,
        }
    }

    pub fn open_session(&mut self, ctx: &egui::Context, target: &str, password: Option<String>, view_only: bool) {
        let target = target.trim().to_string();
        if target.is_empty() {
            return;
        }
        if parse_id(&target) == Some(self.cfg.id) {
            self.toast(self.t("Това е вашият собствен ID 🙂", "That is your own ID 🙂"));
            return;
        }
        let saved_key = self.cfg.contacts.iter().find(|c| c.target == target).and_then(|c| c.saved_key);
        let req = ConnectRequest {
            target: target.clone(),
            password,
            saved_key,
            want_control: !view_only,
            settings: self.default_stream(),
            my_name: self.cfg.name.clone(),
            my_id: self.cfg.id,
            relay: self.cfg.relay.clone(),
            sync_clipboard: self.cfg.sync_clipboard,
        };
        let session = client::connect(req, self.peers.clone(), ctx.clone());
        self.sessions.push(session);
        self.active = Some(self.sessions.len() - 1);
        let name = self.cfg.contacts.iter().find(|c| c.target == target).map(|c| c.name.clone()).unwrap_or_default();
        self.cfg.remember(&target, &name, None);
    }

    pub fn reconnect(&mut self, ctx: &egui::Context, idx: usize, password: Option<String>) {
        let (target, view_only) = {
            let s = &mut self.sessions[idx];
            s.disconnect();
            (s.target.clone(), s.view_only)
        };
        self.sessions.remove(idx);
        self.open_session(ctx, &target, password, view_only);
        let new_idx = self.sessions.len() - 1;
        let s = self.sessions.remove(new_idx);
        self.sessions.insert(idx, s);
        self.active = Some(idx);
    }

    pub fn close_session(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        self.sessions[idx].disconnect();
        self.sessions.remove(idx);
        self.active = match self.active {
            Some(a) if a == idx => {
                if self.sessions.is_empty() { None } else { Some(idx.saturating_sub(1).min(self.sessions.len() - 1)) }
            }
            Some(a) if a > idx => Some(a - 1),
            other => other,
        };
    }

    // ------------------------------------------------------------ frame

    fn housekeeping(&mut self, ctx: &egui::Context) {
        // Remember passwords that worked and names of desks we reached.
        for s in &self.sessions {
            let (name, key, connected) = {
                let sh = s.shared.lock().unwrap();
                (sh.peer_name.clone(), sh.accepted_key, sh.status == Status::Connected)
            };
            if connected {
                if let Some(c) = self.cfg.contacts.iter().find(|c| c.target == s.target) {
                    if (c.name != name && !name.is_empty()) || (key.is_some() && c.saved_key != key) {
                        let target = s.target.clone();
                        self.cfg.remember(&target, &name, key);
                    }
                }
            }
        }
        // Pings and notices.
        let lang = self.cfg.lang;
        let mut new_toasts = Vec::new();
        for s in &mut self.sessions {
            if s.status() == Status::Connected && s.last_ping.elapsed() > Duration::from_secs(1) {
                s.send(Msg::Ping(proto::now_ms()));
                s.last_ping = Instant::now();
                let total = s.traffic.bytes_in.load(Ordering::Relaxed);
                let dt = s.last_bytes.0.elapsed().as_secs_f32().max(0.001);
                s.mbps = (total - s.last_bytes.1) as f32 * 8.0 / dt / 1_000_000.0;
                s.last_bytes = (Instant::now(), total);
            }
            let notices: Vec<String> = std::mem::take(&mut s.shared.lock().unwrap().notices);
            for n in notices {
                let text = if let Some(f) = n.strip_prefix("file-saved:") {
                    format!("{} {f}", tr(lang, "✔ Файлът е изпратен:", "✔ File delivered:"))
                } else if let Some(f) = n.strip_prefix("file-received:") {
                    format!("{} {f}", tr(lang, "📁 Получен файл:", "📁 File received:"))
                } else if let Some(e) = n.strip_prefix("file-failed:") {
                    format!("{} {e}", tr(lang, "Файлът не беше записан:", "File could not be saved:"))
                } else {
                    n
                };
                new_toasts.push(text);
            }
        }
        let host_toasts: Vec<String> = std::mem::take(&mut self.host.lock().unwrap().toasts);
        new_toasts.extend(host_toasts);
        for t in new_toasts {
            self.toast(t);
        }
        if let Listening::On(p) = self.host.lock().unwrap().listening {
            self.announce.lock().unwrap().port = p;
        }
        self.toasts.retain(|(_, t)| t.elapsed() < Duration::from_secs(5));
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        // Keep the stats fresh.
        if self.sessions.iter().any(|s| s.status() == Status::Connected) {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if self.active.is_some_and(|a| a >= self.sessions.len()) {
            self.active = None;
        }
    }

    fn screenshot_logic(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else { return };
        self.frames += 1;
        ctx.request_repaint();
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        if self.frames >= 120 && !self.shot_requested {
            self.shot_requested = true;
            let slot = self.shot.clone();
            let c2 = ctx.clone();
            ctx.request_screenshot(move |img| {
                *slot.lock().unwrap() = Some(img);
                c2.request_repaint();
            });
        }
        if self.frames > 600 && self.shot.lock().unwrap().is_none() {
            self.shot_requested = false;
            self.frames = 100;
        }
        let img = self.shot.lock().unwrap().take();
        if let Some(img) = img {
            let [w, h] = img.size;
            let mut buf = Vec::with_capacity(w * h * 4);
            for p in &img.pixels {
                buf.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Some(im) = image::RgbaImage::from_raw(w as u32, h as u32, buf) {
                let _ = im.save(&path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // ------------------------------------------------------------ layout

    fn sidebar(&mut self, ui: &mut Ui) {
        egui::Panel::left("nav")
            .exact_size(232.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin { left: 16, right: 16, top: 20, bottom: 16 }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::logo(ui, 38.0);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.add_space(1.0);
                        ui.label(RichText::new("MyDesk").font(egui::FontId::new(19.0, theme::bold())));
                        ui.label(RichText::new(self.t("Отдалечен работен плот", "Remote desktop")).size(11.5).color(MUTED));
                    });
                });
                ui.add_space(26.0);
                let nearby = self.peers.lock().unwrap().len();
                let items = [
                    (Page::Home, "🏠", self.t("Начало", "Home"), 0usize),
                    (Page::Book, "📖", self.t("Адресна книга", "Address book"), self.cfg.contacts.len()),
                    (Page::Nearby, "📡", self.t("Наблизо", "Nearby"), nearby),
                    (Page::Settings, "⚙", self.t("Настройки", "Settings"), 0),
                ];
                for (page, icon, label, count) in items {
                    let selected = self.page == page && self.active.is_none();
                    if nav_item(ui, icon, label, count, selected).clicked() {
                        self.page = page;
                        self.active = None;
                    }
                }
                if !self.sessions.is_empty() {
                    ui.add_space(18.0);
                    ui.label(RichText::new(self.t("АКТИВНИ СЕСИИ", "ACTIVE SESSIONS")).size(11.0).color(MUTED));
                    ui.add_space(4.0);
                    let mut go = None;
                    for (i, s) in self.sessions.iter().enumerate() {
                        let st = s.status();
                        let dot = match st {
                            Status::Connected => GOOD,
                            Status::Connecting | Status::Waiting => WARN,
                            _ => BAD,
                        };
                        let resp = nav_item(ui, "🖥", &s.title(), 0, self.active == Some(i));
                        ui.painter().circle_filled(resp.rect.right_center() - Vec2::new(14.0, 0.0), 4.0, dot);
                        if resp.clicked() {
                            go = Some(i);
                        }
                    }
                    if let Some(i) = go {
                        self.active = Some(i);
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(RichText::new(format!("v{}  ·  {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS)).size(11.0).color(MUTED));
                    ui.add_space(4.0);
                    let (text, color) = self.status_line();
                    theme::chip(ui, &text, color);
                });
            });
    }

    fn status_line(&self) -> (String, Color32) {
        let h = self.host.lock().unwrap();
        if !h.allow_incoming {
            return (self.t("Входящите връзки са изключени", "Incoming connections off").into(), MUTED);
        }
        match (&h.listening, &h.relay) {
            (Listening::Error(e), _) => (format!("{} ({e})", self.t("Грешка", "Error")), BAD),
            (_, RelayState::Online) => (self.t("Онлайн · интернет и LAN", "Online · internet & LAN").into(), GOOD),
            (Listening::On(_), _) => (self.t("Готов · локална мрежа", "Ready · local network").into(), GOOD),
            _ => (self.t("Стартиране…", "Starting…").into(), WARN),
        }
    }

    fn home(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new(self.t("Добре дошли 👋", "Welcome 👋")).font(egui::FontId::new(26.0, theme::bold())));
            ui.label(
                RichText::new(self.t(
                    "Споделете вашия ID, за да ви помогнат, или се свържете с друг компютър.",
                    "Share your ID to get help, or connect to another computer.",
                ))
                .color(MUTED),
            );
            ui.add_space(16.0);
            let avail = ui.available_width();
            let card_w = ((avail - 18.0) / 2.0).max(320.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                ui.allocate_ui_with_layout(Vec2::new(card_w, 300.0), Layout::top_down(Align::Min), |ui| self.this_desk_card(ui, card_w));
                ui.allocate_ui_with_layout(Vec2::new(card_w, 300.0), Layout::top_down(Align::Min), |ui| self.connect_card(ui, &ctx, card_w));
            });
            ui.add_space(22.0);
            self.nearby_strip(ui, &ctx);
            ui.add_space(14.0);
            self.recent_grid(ui, &ctx, false);
        });
    }

    fn this_desk_card(&mut self, ui: &mut Ui, w: f32) {
        theme::card_frame().show(ui, |ui| {
            ui.set_width(w - 42.0);
            ui.set_min_height(318.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(self.t("ТОВА РАБОТНО МЯСТО", "THIS DESK")).size(12.0).color(MUTED));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let on = self.cfg.allow_incoming;
                    theme::chip(ui, if on { self.t("Приема връзки", "Accepting") } else { self.t("Изключено", "Off") }, if on { GOOD } else { MUTED });
                });
            });
            ui.add_space(6.0);
            ui.label(RichText::new(self.t("Вашият ID", "Your ID")).color(MUTED).size(13.0));
            ui.horizontal(|ui| {
                let id = format_id(self.cfg.id);
                ui.label(RichText::new(&id).font(egui::FontId::new(36.0, theme::bold())).color(TEXT));
                if icon_button(ui, "📋", self.t("Копирай ID", "Copy ID")).clicked() {
                    ui.ctx().copy_text(id.replace(' ', ""));
                    self.toast(self.t("ID е копиран", "ID copied"));
                }
            });
            ui.add_space(8.0);
            ui.label(RichText::new(self.t("Еднократна парола", "One-time password")).color(MUTED).size(13.0));
            let otp = self.host.lock().unwrap().one_time_password.clone();
            ui.horizontal(|ui| {
                let shown = if self.show_otp { otp.clone() } else { "••••••••".into() };
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x10, 0x13, 0x1A))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.label(RichText::new(shown).font(egui::FontId::monospace(19.0)).color(ACCENT2));
                    });
                if icon_button(ui, if self.show_otp { "👁" } else { "🔒" }, self.t("Покажи / скрий", "Show / hide")).clicked() {
                    self.show_otp = !self.show_otp;
                }
                if icon_button(ui, "📋", self.t("Копирай паролата", "Copy password")).clicked() {
                    ui.ctx().copy_text(otp.clone());
                    self.toast(self.t("Паролата е копирана", "Password copied"));
                }
                if icon_button(ui, "🔄", self.t("Нова парола", "New password")).clicked() {
                    self.host.lock().unwrap().one_time_password = new_one_time_password();
                }
            });
            ui.add_space(10.0);
            let mode = match self.cfg.access {
                AccessMode::PasswordOrAsk => self.t("С парола веднага, без парола — след ваше одобрение.", "With the password: instant. Without: after you approve."),
                AccessMode::AskOnly => self.t("Всяка връзка изисква вашето одобрение.", "Every connection needs your approval."),
                AccessMode::PasswordOnly => self.t("Само с парола (без надзор).", "Password only (unattended)."),
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("🔐").color(GOOD));
                ui.label(RichText::new(mode).size(12.5).color(MUTED));
            });
            ui.label(RichText::new(self.t("Връзката е криптирана от край до край (X25519 + ChaCha20-Poly1305).", "End-to-end encrypted (X25519 + ChaCha20-Poly1305).")).size(12.0).color(MUTED));
        });
    }

    fn connect_card(&mut self, ui: &mut Ui, ctx: &egui::Context, w: f32) {
        theme::card_frame().show(ui, |ui| {
            ui.set_width(w - 42.0);
            ui.set_min_height(318.0);
            ui.label(RichText::new(self.t("ОТДАЛЕЧЕНО РАБОТНО МЯСТО", "REMOTE DESK")).size(12.0).color(MUTED));
            ui.add_space(6.0);
            ui.label(RichText::new(self.t("ID или адрес на другия компютър", "ID or address of the other computer")).color(MUTED).size(13.0));
            let hint = self.t("напр. 123 456 789 или 192.168.1.20", "e.g. 123 456 789 or 192.168.1.20");
            let input = egui::TextEdit::singleline(&mut self.connect_input)
                .hint_text(hint)
                .font(egui::FontId::new(20.0, theme::bold()))
                .margin(egui::Margin::symmetric(12, 9))
                .desired_width(f32::INFINITY);
            let r1 = ui.add(input);
            ui.add_space(4.0);
            let hint = self.t("Парола (по желание)", "Password (optional)");
            let pw = egui::TextEdit::singleline(&mut self.connect_password)
                .hint_text(hint)
                .password(true)
                .margin(egui::Margin::symmetric(12, 7))
                .desired_width(f32::INFINITY);
            let r2 = ui.add(pw);
            let enter = (r1.lost_focus() || r2.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(6.0);
            let label = self.t("Само преглед (без управление)", "View only (no control)");
            ui.checkbox(&mut self.connect_view_only, label);
            ui.add_space(8.0);
            let can = !self.connect_input.trim().is_empty();
            let clicked = theme::primary_button(ui, self.t("Свържи се  →", "Connect  →"), ui.available_width(), can).clicked();
            if can && (clicked || enter) {
                let target = self.connect_input.clone();
                let pw = std::mem::take(&mut self.connect_password);
                let pw = if pw.trim().is_empty() { None } else { Some(pw) };
                self.open_session(ctx, &target, pw, self.connect_view_only);
            }
        });
    }

    fn nearby_strip(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut peers: Vec<net::Peer> = self.peers.lock().unwrap().values().cloned().collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name));
        section_title(ui, self.t("Наблизо в мрежата", "Nearby on your network"), peers.len());
        if peers.is_empty() {
            empty_hint(
                ui,
                "📡",
                if self.networking {
                    self.t("Търсене… Други компютри с MyDesk в същата мрежа ще се появят тук.", "Searching… Other MyDesk computers on this network will appear here.")
                } else {
                    self.t("Други компютри с MyDesk в същата мрежа ще се появят тук.", "Other MyDesk computers on this network will appear here.")
                },
            );
            return;
        }
        let mut go = None;
        ui.horizontal_wrapped(|ui| {
            for p in &peers {
                let resp = desk_tile(ui, &p.name, &format_id(p.id), &format!("{} · {}", p.os, p.addr.ip()), false);
                if resp.clicked() {
                    go = Some(p.id.to_string());
                }
            }
        });
        if let Some(t) = go {
            self.open_session(ctx, &t, None, false);
        }
    }

    fn recent_grid(&mut self, ui: &mut Ui, ctx: &egui::Context, full: bool) {
        let filter = self.book_filter.to_lowercase();
        let mut contacts: Vec<_> = self
            .cfg
            .contacts
            .iter()
            .filter(|c| filter.is_empty() || c.name.to_lowercase().contains(&filter) || c.target.contains(&filter))
            .cloned()
            .collect();
        contacts.sort_by(|a, b| b.favorite.cmp(&a.favorite).then(b.last_used.cmp(&a.last_used)));
        if !full {
            contacts.truncate(8);
            section_title(ui, self.t("Последни сесии", "Recent sessions"), self.cfg.contacts.len());
        }
        if contacts.is_empty() {
            empty_hint(ui, "🕘", self.t("Тук ще виждате компютрите, с които сте се свързвали.", "Computers you connect to will show up here."));
            return;
        }
        let mut connect = None;
        let mut toggle_fav = None;
        let mut remove = None;
        let mut rename = None;
        ui.horizontal_wrapped(|ui| {
            for c in &contacts {
                let title = if c.name.is_empty() { c.target.clone() } else { c.name.clone() };
                let sub = parse_id(&c.target).map(format_id).unwrap_or(c.target.clone());
                let resp = desk_tile(ui, &title, &sub, if c.saved_key.is_some() { "🔑" } else { "" }, c.favorite);
                if resp.clicked() {
                    connect = Some(c.target.clone());
                }
                resp.context_menu(|ui| {
                    if ui.button(self.t("▶ Свържи се", "▶ Connect")).clicked() {
                        connect = Some(c.target.clone());
                    }
                    if ui.button(self.t("👁 Само преглед", "👁 View only")).clicked() {
                        connect = Some(format!("view:{}", c.target));
                    }
                    if ui.button(if c.favorite { self.t("☆ Махни от любими", "☆ Unfavourite") } else { self.t("⭐ Любим", "⭐ Favourite") }).clicked() {
                        toggle_fav = Some(c.target.clone());
                    }
                    if ui.button(self.t("✏ Преименувай", "✏ Rename")).clicked() {
                        rename = Some((c.target.clone(), c.name.clone()));
                    }
                    if ui.button(self.t("🗑 Премахни", "🗑 Remove")).clicked() {
                        remove = Some(c.target.clone());
                    }
                });
            }
        });
        if let Some(t) = connect {
            if let Some(t) = t.strip_prefix("view:") {
                self.open_session(ctx, t, None, true);
            } else {
                self.open_session(ctx, &t, None, false);
            }
        }
        if let Some(t) = toggle_fav {
            if let Some(c) = self.cfg.contacts.iter_mut().find(|c| c.target == t) {
                c.favorite = !c.favorite;
            }
            self.cfg.save();
        }
        if let Some(t) = remove {
            self.cfg.contacts.retain(|c| c.target != t);
            self.cfg.save();
        }
        if rename.is_some() {
            self.renaming = rename;
        }
        ui.add_space(6.0);
        ui.label(RichText::new(self.t("Десен бутон върху плочка за още действия.", "Right-click a tile for more actions.")).size(12.0).color(MUTED));
    }

    fn book(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(6.0);
        ui.label(RichText::new(self.t("Адресна книга", "Address book")).font(egui::FontId::new(26.0, theme::bold())));
        ui.add_space(8.0);
        let hint = self.t("🔍 Търсене по име или ID", "🔍 Search by name or ID");
        ui.add(
            egui::TextEdit::singleline(&mut self.book_filter)
                .hint_text(hint)
                .margin(egui::Margin::symmetric(12, 8))
                .desired_width(360.0),
        );
        ui.add_space(14.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.recent_grid(ui, &ctx, true);
        });
    }

    fn nearby(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(6.0);
        ui.label(RichText::new(self.t("Наблизо", "Nearby")).font(egui::FontId::new(26.0, theme::bold())));
        ui.label(RichText::new(self.t("Компютри с MyDesk в същата локална мрежа. Връзката е директна — без сървъри, с минимално забавяне.", "MyDesk computers on the same local network. Direct connection — no servers, lowest latency.")).color(MUTED));
        ui.add_space(14.0);
        self.nearby_strip(ui, &ctx);
    }

    fn settings(&mut self, ui: &mut Ui) {
        let mut changed = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new(self.t("Настройки", "Settings")).font(egui::FontId::new(26.0, theme::bold())));
            ui.add_space(12.0);
            let w = ui.available_width().min(760.0);

            settings_card(ui, w, self.t("Общи", "General"), |ui| {
                ui.horizontal(|ui| {
                    ui.label(self.t("Име на този компютър", "This computer's name"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::TextEdit::singleline(&mut self.cfg.name).desired_width(240.0)).lost_focus() {
                            changed = true;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(self.t("Език", "Language"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        changed |= ui.selectable_value(&mut self.cfg.lang, Lang::En, "English").changed();
                        changed |= ui.selectable_value(&mut self.cfg.lang, Lang::Bg, "Български").changed();
                    });
                });
            });

            let t = |bg, en| tr(self.cfg.lang, bg, en);
            settings_card(ui, w, t("Сигурност и достъп", "Security & access"), |ui| {
                changed |= toggle_row(ui, t("Приемай входящи връзки", "Accept incoming connections"), &mut self.cfg.allow_incoming);
                ui.add_space(4.0);
                ui.label(RichText::new(t("Кой може да влезе", "Who can get in")).color(MUTED).size(13.0));
                changed |= ui.radio_value(&mut self.cfg.access, AccessMode::PasswordOrAsk, t("С парола веднага, иначе питай мен (препоръчително)", "Password lets in, otherwise ask me (recommended)")).changed();
                changed |= ui.radio_value(&mut self.cfg.access, AccessMode::AskOnly, t("Винаги питай мен", "Always ask me")).changed();
                changed |= ui.radio_value(&mut self.cfg.access, AccessMode::PasswordOnly, t("Само с парола (достъп без надзор)", "Password only (unattended access)")).changed();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(t("Постоянна парола", "Permanent password"));
                    if self.cfg.permanent_key.is_some() {
                        theme::chip(ui, t("зададена", "set"), GOOD);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.new_permanent).password(true).hint_text(t("нова парола (мин. 8 знака)", "new password (min. 8 chars)")).desired_width(260.0));
                    if ui.add_enabled(self.new_permanent.trim().len() >= 8, egui::Button::new(t("Задай", "Set"))).clicked() {
                        self.cfg.permanent_key = Some(proto::password_key(&self.new_permanent));
                        self.new_permanent.clear();
                        changed = true;
                    }
                    if self.cfg.permanent_key.is_some() && ui.button(t("Премахни", "Remove")).clicked() {
                        self.cfg.permanent_key = None;
                        changed = true;
                    }
                });
            });

            settings_card(ui, w, t("Мрежа", "Network"), |ui| {
                let (listen, relay_state) = {
                    let h = self.host.lock().unwrap();
                    (h.listening.clone(), h.relay.clone())
                };
                ui.horizontal(|ui| {
                    ui.label(t("Директни връзки (TCP порт)", "Direct connections (TCP port)"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| match listen {
                        Listening::On(p) => theme::chip(ui, &p.to_string(), GOOD),
                        Listening::Error(e) => theme::chip(ui, &e, BAD),
                        _ => theme::chip(ui, "…", WARN),
                    });
                });
                changed |= toggle_row(ui, t("Видим в локалната мрежа", "Visible on the local network"), &mut self.cfg.lan_discovery);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(t("Relay сървър (за връзка през интернет по ID)", "Relay server (connect by ID over the internet)"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &relay_state {
                        RelayState::Online => theme::chip(ui, t("онлайн", "online"), GOOD),
                        RelayState::Connecting => theme::chip(ui, t("свързване…", "connecting…"), WARN),
                        RelayState::Error(_) => theme::chip(ui, t("недостъпен", "unreachable"), BAD),
                        RelayState::Unused => theme::chip(ui, t("не е зададен", "not set"), MUTED),
                    });
                });
                if ui
                    .add(egui::TextEdit::singleline(&mut self.cfg.relay).hint_text("relay.example.com:47900").desired_width(f32::INFINITY))
                    .lost_focus()
                {
                    changed = true;
                }
                ui.label(
                    RichText::new(t(
                        "Пуснете „mydesk --relay“ на сървър с публичен адрес и въведете адреса му тук на двата компютъра. Relay-ят само препраща криптирани данни.",
                        "Run \"mydesk --relay\" on a server with a public address and enter it here on both computers. The relay only forwards encrypted data.",
                    ))
                    .size(12.0)
                    .color(MUTED),
                );
            });

            settings_card(ui, w, t("Картина и производителност", "Picture & performance"), |ui| {
                ui.horizontal(|ui| {
                    ui.label(t("Кодек", "Codec"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        changed |= ui.selectable_value(&mut self.cfg.codec, Codec::Lossless, t("Без загуби (текст)", "Lossless (text)")).changed();
                        changed |= ui.selectable_value(&mut self.cfg.codec, Codec::H264, t("H.264 (плавно)", "H.264 (smooth)")).changed();
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(t("Качество", "Quality"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        changed |= ui.selectable_value(&mut self.cfg.quality, Quality::Best, t("Най-добро", "Best")).changed();
                        changed |= ui.selectable_value(&mut self.cfg.quality, Quality::Balanced, t("Баланс", "Balanced")).changed();
                        changed |= ui.selectable_value(&mut self.cfg.quality, Quality::Speed, t("Скорост", "Speed")).changed();
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(t("Кадри в секунда", "Frames per second"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        for f in [144u32, 120, 60, 30] {
                            changed |= ui.selectable_value(&mut self.cfg.fps, f, f.to_string()).changed();
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(t("Резолюция", "Resolution"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        changed |= ui.selectable_value(&mut self.cfg.resolution, Resolution::Max720, "720p").changed();
                        changed |= ui.selectable_value(&mut self.cfg.resolution, Resolution::Max1080, "1080p").changed();
                        changed |= ui.selectable_value(&mut self.cfg.resolution, Resolution::Native, t("Оригинална", "Native")).changed();
                    });
                });
                changed |= toggle_row(ui, t("Синхронизирай клипборда", "Sync clipboard"), &mut self.cfg.sync_clipboard);
                changed |= toggle_row(ui, t("Показвай статистика по време на сесия", "Show stats during a session"), &mut self.cfg.show_stats);
            });

            settings_card(ui, w, t("Файлове", "Files"), |ui| {
                ui.label(t("Получените файлове се записват в:", "Received files are saved to:"));
                ui.label(RichText::new(crate::extras::downloads_dir().display().to_string()).color(ACCENT2).monospace());
            });
        });
        if changed {
            self.sync_host_settings();
        }
    }

    // ------------------------------------------------------------ host overlays

    fn host_overlays(&mut self, ctx: &egui::Context) {
        let lang = self.cfg.lang;
        // Incoming connection prompt.
        let pending: Vec<(u64, String, u64, String, String, bool)> = {
            let h = self.host.lock().unwrap();
            h.pending
                .iter()
                .filter(|p| p.decision.is_none())
                .map(|p| (p.pid, p.peer_name.clone(), p.peer_id, p.peer_addr.clone(), p.fingerprint.clone(), p.want_control))
                .collect()
        };
        if let Some((pid, name, pid_peer, addr, fp, want_control)) = pending.first().cloned() {
            let mut decision = None;
            egui::Modal::new(egui::Id::new(("incoming", pid)))
                .frame(theme::card_frame().inner_margin(egui::Margin::same(26)))
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
                        let (a, b) = theme::palette(&name);
                        theme::gradient(ui, r, 14.0, a, b, true);
                        theme::paint_monitor(ui, r.shrink(13.0), Color32::WHITE);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(tr(lang, "Входяща връзка", "Incoming connection")).size(12.5).color(MUTED));
                            ui.label(RichText::new(&name).font(egui::FontId::new(20.0, theme::bold())));
                            ui.label(RichText::new(format!("ID {} · {}", format_id(pid_peer), addr)).size(12.5).color(MUTED));
                        });
                    });
                    ui.add_space(10.0);
                    ui.label(if want_control {
                        tr(lang, "Иска да вижда и управлява този компютър.", "Wants to see and control this computer.")
                    } else {
                        tr(lang, "Иска само да вижда екрана.", "Only wants to see the screen.")
                    });
                    ui.label(RichText::new(format!("{} {fp}", tr(lang, "Код за сигурност:", "Security code:"))).size(12.5).color(MUTED));
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if theme::primary_button(ui, tr(lang, "Приеми", "Accept"), 130.0, true).clicked() {
                            decision = Some(Decision::Accept { control: true });
                        }
                        if want_control && ui.add_sized([130.0, 42.0], egui::Button::new(tr(lang, "Само преглед", "View only"))).clicked() {
                            decision = Some(Decision::Accept { control: false });
                        }
                        if ui.add_sized([110.0, 42.0], egui::Button::new(RichText::new(tr(lang, "Откажи", "Decline")).color(BAD))).clicked() {
                            decision = Some(Decision::Reject);
                        }
                    });
                });
            if let Some(d) = decision {
                let mut h = self.host.lock().unwrap();
                if let Some(p) = h.pending.iter_mut().find(|p| p.pid == pid) {
                    p.decision = Some(d);
                }
            }
        }

        // Active incoming sessions: floating control card.
        let sessions: Vec<(u64, String, String, Duration, bool, Vec<ChatLine>, usize)> = {
            let h = self.host.lock().unwrap();
            h.sessions
                .iter()
                .map(|s| (s.sid, s.peer_name.clone(), s.peer_addr.clone(), s.started.elapsed(), s.can_control, s.chat.clone(), s.unread))
                .collect()
        };
        if sessions.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        egui::Area::new(egui::Id::new("host-sessions"))
            .anchor(Align2::RIGHT_BOTTOM, [-18.0, -18.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for (sid, name, addr, dur, can_control, chat, unread) in sessions {
                    theme::card_frame().inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                        ui.set_width(330.0);
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 5.0, BAD);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&name).font(egui::FontId::new(15.0, theme::bold())));
                                let secs = dur.as_secs();
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {} · {:02}:{:02}",
                                        if can_control { tr(lang, "управлява", "controlling") } else { tr(lang, "гледа", "watching") },
                                        addr,
                                        secs / 60,
                                        secs % 60
                                    ))
                                    .size(12.0)
                                    .color(MUTED),
                                );
                            });
                        });
                        ui.horizontal(|ui| {
                            let label = if can_control { tr(lang, "🖱 Спри управлението", "🖱 Block control") } else { tr(lang, "🖱 Разреши управление", "🖱 Allow control") };
                            if ui.button(label).clicked() {
                                let mut h = self.host.lock().unwrap();
                                if let Some(s) = h.sessions.iter_mut().find(|s| s.sid == sid) {
                                    s.can_control = !s.can_control;
                                    s.outbox.send(Msg::Permissions { can_control: s.can_control });
                                }
                            }
                            let chat_label = if unread > 0 { format!("💬 {unread}") } else { "💬".to_string() };
                            if ui.button(chat_label).clicked() {
                                self.host_chat_open = if self.host_chat_open == Some(sid) { None } else { Some(sid) };
                                let mut h = self.host.lock().unwrap();
                                if let Some(s) = h.sessions.iter_mut().find(|s| s.sid == sid) {
                                    s.unread = 0;
                                }
                            }
                            if ui.button(RichText::new(tr(lang, "Прекъсни", "Disconnect")).color(BAD)).clicked() {
                                let h = self.host.lock().unwrap();
                                if let Some(s) = h.sessions.iter().find(|s| s.sid == sid) {
                                    s.outbox.send(Msg::Bye("host-ended".into()));
                                    s.kick.store(true, Ordering::SeqCst);
                                }
                            }
                        });
                        if self.host_chat_open == Some(sid) {
                            ui.separator();
                            let input = self.host_chat_input.entry(sid).or_default();
                            if let Some(text) = view::chat_box(ui, &chat, input, lang, 180.0) {
                                let mut h = self.host.lock().unwrap();
                                if let Some(s) = h.sessions.iter_mut().find(|s| s.sid == sid) {
                                    s.outbox.send(Msg::Chat(text.clone()));
                                    s.chat.push(ChatLine { mine: true, text, time: clock() });
                                    s.unread = 0;
                                }
                            }
                        }
                    });
                    ui.add_space(8.0);
                }
            });
    }

    fn toasts_overlay(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::CENTER_BOTTOM, [0.0, -24.0])
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                for (t, _) in &self.toasts {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(0x22, 0x27, 0x35))
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(egui::Margin::symmetric(16, 10))
                        .show(ui, |ui| {
                            ui.label(t);
                        });
                    ui.add_space(6.0);
                }
            });
    }

    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some((target, mut name)) = self.renaming.clone() else { return };
        let mut done = None;
        egui::Modal::new(egui::Id::new("rename")).frame(theme::card_frame()).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new(self.t("Ново име", "New name")).font(egui::FontId::new(17.0, theme::bold())));
            let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
            r.request_focus();
            ui.horizontal(|ui| {
                if ui.button(self.t("Запази", "Save")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = Some(true);
                }
                if ui.button(self.t("Отказ", "Cancel")).clicked() {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                if let Some(c) = self.cfg.contacts.iter_mut().find(|c| c.target == target) {
                    c.name = name.trim().to_string();
                }
                self.cfg.save();
                self.renaming = None;
            }
            Some(false) => self.renaming = None,
            None => self.renaming = Some((target, name)),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.housekeeping(&ctx);
        self.screenshot_logic(&ctx);

        if !self.fullscreen || self.active.is_none() {
            self.sidebar(ui);
        }
        let bg = if self.active.is_some() { Color32::BLACK } else { BG };
        let margin = if self.active.is_some() { egui::Margin::ZERO } else { egui::Margin { left: 30, right: 30, top: 24, bottom: 16 } };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(bg).inner_margin(margin))
            .show(ui, |ui| {
                if let Some(idx) = self.active {
                    view::session_view(self, ui, idx);
                } else {
                    match self.page {
                        Page::Home => self.home(ui),
                        Page::Book => self.book(ui),
                        Page::Nearby => self.nearby(ui),
                        Page::Settings => self.settings(ui),
                    }
                }
            });
        self.host_overlays(&ctx);
        self.rename_dialog(&ctx);
        self.toasts_overlay(&ctx);
    }

    fn on_exit(&mut self) {
        for s in &mut self.sessions {
            s.disconnect();
        }
        self.cfg.save();
    }
}

// ------------------------------------------------------------ widgets

fn nav_item(ui: &mut Ui, icon: &str, label: &str, count: usize, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CornerRadius::same(10), CARD_HI);
        let bar = egui::Rect::from_min_size(rect.min + Vec2::new(0.0, 10.0), Vec2::new(3.0, 20.0));
        p.rect_filled(bar, CornerRadius::same(2), ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(10), CARD);
    }
    let color = if selected { TEXT } else { Color32::from_rgb(0xB4, 0xBB, 0xC9) };
    p.text(rect.left_center() + Vec2::new(16.0, 0.0), Align2::LEFT_CENTER, icon, egui::FontId::proportional(16.0), if selected { ACCENT2 } else { MUTED });
    let mut text = label.to_string();
    if text.chars().count() > 20 {
        text = text.chars().take(19).collect::<String>() + "…";
    }
    p.text(rect.left_center() + Vec2::new(44.0, 0.0), Align2::LEFT_CENTER, text, egui::FontId::new(14.5, if selected { theme::bold() } else { egui::FontFamily::Proportional }), color);
    if count > 0 {
        p.text(rect.right_center() - Vec2::new(12.0, 0.0), Align2::RIGHT_CENTER, count.to_string(), egui::FontId::proportional(12.0), MUTED);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

pub fn icon_button(ui: &mut Ui, icon: &str, tip: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(icon).size(15.0)).min_size(Vec2::new(34.0, 32.0))).on_hover_text(tip)
}

fn section_title(ui: &mut Ui, title: &str, count: usize) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(egui::FontId::new(17.0, theme::bold())));
        if count > 0 {
            theme::chip(ui, &count.to_string(), ACCENT2);
        }
    });
    ui.add_space(6.0);
}

fn empty_hint(ui: &mut Ui, icon: &str, text: &str) {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).size(20.0).color(MUTED));
                ui.label(RichText::new(text).color(MUTED));
            });
        });
}

fn desk_tile(ui: &mut Ui, title: &str, sub: &str, extra: &str, favorite: bool) -> egui::Response {
    let size = Vec2::new(212.0, 168.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(14), if hovered { CARD_HI } else { CARD }, Stroke::new(1.0, if hovered { ACCENT.linear_multiply(0.8) } else { BORDER }), egui::StrokeKind::Inside);
    let thumb = egui::Rect::from_min_size(rect.min + Vec2::new(10.0, 10.0), Vec2::new(size.x - 20.0, 92.0));
    let (a, b) = theme::palette(title);
    theme::gradient(ui, thumb, 10.0, a.linear_multiply(0.85), b.linear_multiply(0.85), true);
    // faux windows on the thumbnail
    let w1 = egui::Rect::from_min_size(thumb.min + Vec2::new(16.0, 14.0), Vec2::new(86.0, 52.0));
    ui.painter().rect_filled(w1, CornerRadius::same(5), Color32::from_rgba_unmultiplied(255, 255, 255, 46));
    let w2 = egui::Rect::from_min_size(thumb.min + Vec2::new(78.0, 34.0), Vec2::new(94.0, 46.0));
    ui.painter().rect_filled(w2, CornerRadius::same(5), Color32::from_rgba_unmultiplied(255, 255, 255, 70));
    ui.painter().rect_filled(egui::Rect::from_min_size(thumb.left_bottom() - Vec2::new(-1.0, 12.0), Vec2::new(thumb.width() - 2.0, 11.0)), CornerRadius { nw: 0, ne: 0, sw: 10, se: 10 }, Color32::from_rgba_unmultiplied(0, 0, 0, 70));
    if favorite {
        ui.painter().text(thumb.right_top() + Vec2::new(-10.0, 8.0), Align2::RIGHT_TOP, "⭐", egui::FontId::proportional(14.0), Color32::from_rgb(0xFA, 0xCC, 0x15));
    }
    let mut t = title.to_string();
    if t.chars().count() > 22 {
        t = t.chars().take(21).collect::<String>() + "…";
    }
    ui.painter().text(rect.left_top() + Vec2::new(14.0, 112.0), Align2::LEFT_TOP, t, egui::FontId::new(14.5, theme::bold()), TEXT);
    ui.painter().text(rect.left_top() + Vec2::new(14.0, 134.0), Align2::LEFT_TOP, sub, egui::FontId::proportional(12.5), MUTED);
    if !extra.is_empty() {
        ui.painter().text(rect.right_top() + Vec2::new(-12.0, 134.0), Align2::RIGHT_TOP, extra, egui::FontId::proportional(11.5), MUTED);
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn settings_card(ui: &mut Ui, w: f32, title: &str, body: impl FnOnce(&mut Ui)) {
    theme::card_frame().show(ui, |ui| {
        ui.set_width(w - 42.0);
        ui.label(RichText::new(title).font(egui::FontId::new(16.0, theme::bold())));
        ui.add_space(6.0);
        body(ui);
    });
    ui.add_space(12.0);
}

fn toggle_row(ui: &mut Ui, label: &str, value: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed = toggle(ui, value).changed();
        });
    });
    changed
}

pub fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(42.0, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let bg = theme::lerp(Color32::from_rgb(0x33, 0x39, 0x48), ACCENT, t);
    ui.painter().rect_filled(rect, CornerRadius::same(12), bg);
    let x = egui::lerp(rect.left() + 12.0..=rect.right() - 12.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 9.0, Color32::WHITE);
    resp
}
