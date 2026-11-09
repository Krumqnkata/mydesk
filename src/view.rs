//! The remote session view: the other computer's screen, input forwarding,
//! the floating toolbar, chat, stats and file drop.

use std::sync::atomic::Ordering;

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use crate::app::{App, tr};
use crate::client::{Session, Status};
use crate::config::{Lang, format_id, parse_id};
use crate::extras::{ChatLine, clock, send_files};
use crate::input::is_known_key;
use crate::proto::{Codec, Msg, Quality, Resolution};
use crate::theme::{self, *};

enum Action {
    Close,
    Reconnect(Option<String>),
    Fullscreen,
    Home,
    Toast(String),
}

pub fn session_view(app: &mut App, ui: &mut Ui, idx: usize) {
    let lang = app.cfg.lang;
    let ctx = ui.ctx().clone();
    let status = app.sessions[idx].status();
    let mut actions = Vec::new();
    if status == Status::Connected {
        connected_view(&mut app.sessions[idx], ui, lang, app.cfg.show_stats, app.fullscreen, &mut actions);
    } else {
        status_card(&mut app.sessions[idx], ui, lang, &status, &mut actions);
    }
    for a in actions {
        match a {
            Action::Close => {
                if app.fullscreen {
                    app.fullscreen = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
                app.close_session(idx);
            }
            Action::Reconnect(pw) => app.reconnect(&ctx, idx, pw),
            Action::Fullscreen => {
                app.fullscreen = !app.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.fullscreen));
            }
            Action::Home => {
                if app.fullscreen {
                    app.fullscreen = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
                app.active = None;
            }
            Action::Toast(t) => app.toast(t),
        }
    }
}

pub fn error_text(lang: Lang, e: &str) -> String {
    let t = |bg: &str, en: &str| if lang == Lang::Bg { bg.to_string() } else { en.to_string() };
    match e {
        "" => t("Сесията приключи.", "The session ended."),
        "id-not-found" => t(
            "Този ID не е намерен в локалната мрежа. За връзка през интернет задайте relay сървър в Настройки или въведете IP адрес.",
            "This ID was not found on the local network. To connect over the internet set a relay server in Settings, or enter an IP address.",
        ),
        "offline" => t("Този компютър не е онлайн в момента.", "That computer is not online right now."),
        "rejected" => t("Другата страна отказа връзката (или не отговори в рамките на 60 секунди).", "The other side declined (or did not answer within 60 seconds)."),
        "host-ended" => t("Другата страна прекрати сесията.", "The other side ended the session."),
        "closed" => t("Сесията беше затворена.", "The session was closed."),
        "bad-address" => t("Адресът не е валиден.", "That address is not valid."),
        "not a MyDesk peer" => t("На този адрес няма MyDesk.", "There is no MyDesk at that address."),
        other => {
            let lower = other.to_lowercase();
            if lower.contains("refused") || lower.contains("10061") {
                t("Връзката е отказана — MyDesk не работи там или защитната стена блокира порта.", "Connection refused — MyDesk isn't running there or a firewall blocks the port.")
            } else if lower.contains("timed out") || lower.contains("10060") {
                t("Няма отговор (изтече времето). Проверете адреса и мрежата.", "No answer (timed out). Check the address and network.")
            } else {
                other.to_string()
            }
        }
    }
}

fn status_card(s: &mut Session, ui: &mut Ui, lang: Lang, status: &Status, actions: &mut Vec<Action>) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, BG);
    let (peer, fp) = {
        let sh = s.shared.lock().unwrap();
        (sh.peer_name.clone(), sh.fingerprint.clone())
    };
    let target = parse_id(&s.target).map(format_id).unwrap_or(s.target.clone());
    let card = Rect::from_center_size(rect.center(), Vec2::new(440.0, 330.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(card));
    theme::card_frame().inner_margin(egui::Margin::same(28)).show(&mut child, |ui| {
        ui.set_width(384.0);
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
            let (a, b) = theme::palette(if peer.is_empty() { &s.target } else { &peer });
            theme::gradient(ui, r, 18.0, a, b, true);
            theme::paint_monitor(ui, r.shrink(16.0), Color32::WHITE);
            ui.add_space(8.0);
            ui.label(RichText::new(if peer.is_empty() { target.clone() } else { peer.clone() }).font(egui::FontId::new(20.0, theme::bold())));
            if !peer.is_empty() {
                ui.label(RichText::new(&target).color(MUTED).size(12.5));
            }
            ui.add_space(12.0);
            match status {
                Status::Connecting | Status::Waiting => {
                    ui.add(egui::Spinner::new().size(26.0).color(ACCENT2));
                    ui.add_space(6.0);
                    ui.label(if *status == Status::Connecting {
                        tr(lang, "Свързване…", "Connecting…")
                    } else {
                        tr(lang, "Изчаква се другата страна да приеме…", "Waiting for the other side to accept…")
                    });
                    if !fp.is_empty() {
                        ui.label(RichText::new(format!("{} {fp}", tr(lang, "Код за сигурност:", "Security code:"))).size(12.0).color(MUTED));
                    }
                    ui.add_space(10.0);
                    if ui.button(tr(lang, "Отказ", "Cancel")).clicked() {
                        actions.push(Action::Close);
                    }
                }
                Status::NeedPassword(reason) => {
                    ui.label(RichText::new(if reason == "wrong-password" { tr(lang, "🔒 Грешна парола", "🔒 Wrong password") } else { tr(lang, "🔒 Нужна е парола", "🔒 Password required") }).color(if reason == "wrong-password" { BAD } else { TEXT }));
                    ui.add_space(6.0);
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut s.password_input)
                            .password(true)
                            .hint_text(tr(lang, "Парола", "Password"))
                            .margin(egui::Margin::symmetric(12, 8))
                            .desired_width(280.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(60.0);
                        if theme::primary_button(ui, tr(lang, "Свържи се", "Connect"), 150.0, !s.password_input.is_empty()).clicked() || enter {
                            actions.push(Action::Reconnect(Some(std::mem::take(&mut s.password_input))));
                        }
                        if ui.add_sized([100.0, 42.0], egui::Button::new(tr(lang, "Отказ", "Cancel"))).clicked() {
                            actions.push(Action::Close);
                        }
                    });
                }
                Status::Failed(e) | Status::Closed(e) => {
                    ui.label(RichText::new(error_text(lang, e)).color(if matches!(status, Status::Failed(_)) { BAD } else { MUTED }));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.add_space(70.0);
                        if theme::primary_button(ui, tr(lang, "Опитай пак", "Try again"), 140.0, true).clicked() {
                            actions.push(Action::Reconnect(None));
                        }
                        if ui.add_sized([100.0, 42.0], egui::Button::new(tr(lang, "Затвори", "Close"))).clicked() {
                            actions.push(Action::Close);
                        }
                    });
                }
                Status::Connected => {}
            }
        });
    });
    ui.allocate_rect(rect, Sense::hover());
}

fn update_texture(s: &mut Session, ctx: &egui::Context) -> Option<(egui::TextureId, Vec2)> {
    {
        let sh = s.shared.lock().unwrap();
        let f = &sh.frame;
        if f.seq != s.tex_seq && f.width > 0 && f.rgba.len() == (f.width * f.height * 4) as usize {
            let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba);
            s.tex_seq = f.seq;
            drop(sh);
            match &mut s.texture {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => s.texture = Some(ctx.load_texture(format!("remote-{}", s.sid), img, egui::TextureOptions::LINEAR)),
            }
        }
    }
    s.texture.as_ref().map(|t| (t.id(), t.size_vec2()))
}

fn connected_view(s: &mut Session, ui: &mut Ui, lang: Lang, show_stats: bool, fullscreen: bool, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    // Chat panel on the right.
    if s.show_chat {
        let chat: Vec<ChatLine> = {
            let mut sh = s.shared.lock().unwrap();
            sh.unread = 0;
            sh.chat.clone()
        };
        egui::Panel::right(egui::Id::new(("chat", s.sid)))
            .exact_size(300.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::same(14)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr(lang, "💬 Чат", "💬 Chat")).font(egui::FontId::new(16.0, theme::bold())));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            s.show_chat = false;
                        }
                    });
                });
                ui.separator();
                let h = ui.available_height() - 50.0;
                if let Some(text) = chat_box(ui, &chat, &mut s.chat_input, lang, h) {
                    s.send(Msg::Chat(text.clone()));
                    s.shared.lock().unwrap().chat.push(ChatLine { mine: true, text, time: clock() });
                }
            });
    }

    let area = ui.available_rect_before_wrap();
    let area_resp = ui.allocate_rect(area, Sense::click_and_drag());
    ui.painter().rect_filled(area, CornerRadius::ZERO, Color32::BLACK);
    let tex = update_texture(s, &ctx);
    let ppp = ctx.pixels_per_point();
    let img_rect = match tex {
        Some((_, size)) => {
            let (w, h) = (size.x, size.y);
            if s.fit {
                let scale = (area.width() / w).min(area.height() / h);
                Rect::from_center_size(area.center(), Vec2::new(w * scale, h * scale))
            } else {
                // 1:1 pixels; pan by moving the mouse towards the edges.
                let disp = Vec2::new(w / ppp, h / ppp);
                let mut min = area.center() - disp / 2.0;
                if let Some(p) = ctx.pointer_hover_pos().filter(|p| area.contains(*p)) {
                    let t = (p - area.min) / area.size();
                    if disp.x > area.width() {
                        min.x = area.left() - (disp.x - area.width()) * t.x;
                    }
                    if disp.y > area.height() {
                        min.y = area.top() - (disp.y - area.height()) * t.y;
                    }
                } else {
                    if disp.x > area.width() {
                        min.x = area.left();
                    }
                    if disp.y > area.height() {
                        min.y = area.top();
                    }
                }
                Rect::from_min_size(min, disp)
            }
        }
        None => Rect::from_center_size(area.center(), Vec2::ZERO),
    };
    if let Some((id, _)) = tex {
        ui.painter().with_clip_rect(area).image(id, img_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    } else {
        ui.painter().text(area.center(), Align2::CENTER_CENTER, tr(lang, "Получаване на картината…", "Receiving the picture…"), egui::FontId::proportional(16.0), MUTED);
        ui.put(Rect::from_center_size(area.center() + Vec2::new(0.0, 34.0), Vec2::splat(24.0)), egui::Spinner::new().color(ACCENT2));
    }

    let (can_control, cursor, displays, fps, rtt, decode_ms, codec, unread, fingerprint, peer) = {
        let sh = s.shared.lock().unwrap();
        (sh.can_control, sh.cursor, sh.displays.clone(), sh.fps, sh.rtt_ms, sh.decode_ms, sh.codec, sh.unread, sh.fingerprint.clone(), sh.peer_name.clone())
    };
    let controlling = can_control && !s.view_only;

    handle_input(s, ui, area, img_rect, controlling, actions);

    // Remote cursor (when we are only watching, or the local pointer is elsewhere).
    if let Some((cx, cy)) = cursor {
        let p = Pos2::new(img_rect.left() + cx * img_rect.width(), img_rect.top() + cy * img_rect.height());
        let local_near = ctx.pointer_hover_pos().is_some_and(|lp| lp.distance(p) < 3.0);
        if area.contains(p) && (!controlling || !local_near) && img_rect.width() > 0.0 {
            let pts = vec![p, p + Vec2::new(0.0, 17.0), p + Vec2::new(4.5, 13.0), p + Vec2::new(8.0, 20.0), p + Vec2::new(10.5, 19.0), p + Vec2::new(7.0, 12.0), p + Vec2::new(12.5, 12.0)];
            ui.painter().add(egui::Shape::convex_polygon(pts.clone(), Color32::WHITE, Stroke::new(1.2, Color32::BLACK)));
        }
    }

    // Files dropped onto the window are sent to the remote computer.
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
    if !dropped.is_empty() {
        if let Some(o) = s.outbox.lock().unwrap().clone() {
            actions.push(Action::Toast(format!("{} {}", tr(lang, "📤 Изпращане:", "📤 Sending:"), dropped.len())));
            send_files(o, dropped, s.file_progress.clone());
        }
    }
    if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
        ui.painter().rect_filled(area.shrink(20.0), CornerRadius::same(18), ACCENT.linear_multiply(0.25));
        ui.painter().text(area.center(), Align2::CENTER_CENTER, tr(lang, "Пуснете файловете, за да ги изпратите", "Drop files to send them"), egui::FontId::new(20.0, theme::bold()), Color32::WHITE);
    }
    if let Some((name, pct)) = s.file_progress.lock().unwrap().clone() {
        let r = Rect::from_min_size(area.left_bottom() + Vec2::new(16.0, -60.0), Vec2::new(280.0, 44.0));
        ui.painter().rect_filled(r, CornerRadius::same(10), Color32::from_rgba_unmultiplied(20, 23, 32, 230));
        ui.painter().text(r.left_top() + Vec2::new(12.0, 7.0), Align2::LEFT_TOP, format!("📤 {name}"), egui::FontId::proportional(12.5), TEXT);
        let bar = Rect::from_min_size(r.left_bottom() + Vec2::new(12.0, -14.0), Vec2::new(256.0, 5.0));
        ui.painter().rect_filled(bar, CornerRadius::same(3), CARD_HI);
        ui.painter().rect_filled(Rect::from_min_size(bar.min, Vec2::new(256.0 * pct, 5.0)), CornerRadius::same(3), ACCENT2);
        ctx.request_repaint();
    }

    // Stats overlay.
    if show_stats {
        let (w, h) = s.texture.as_ref().map(|t| (t.size()[0], t.size()[1])).unwrap_or((0, 0));
        let rtt_color = if rtt < 30.0 { GOOD } else if rtt < 80.0 { WARN } else { BAD };
        let text = format!(
            "{:.0} fps   {:.0} ms   {:.1} Mbps   {}×{}   {}   dec {:.1} ms",
            fps,
            rtt,
            s.mbps,
            w,
            h,
            if codec == Codec::H264 { "H.264" } else { "Lossless" },
            decode_ms
        );
        let galley = ui.painter().layout_no_wrap(text, egui::FontId::monospace(12.0), Color32::from_rgb(0xD0, 0xD6, 0xE2));
        let r = Rect::from_min_size(area.left_bottom() + Vec2::new(12.0, -36.0), galley.size() + Vec2::new(30.0, 12.0));
        ui.painter().rect_filled(r, CornerRadius::same(8), Color32::from_rgba_unmultiplied(10, 12, 18, 200));
        ui.painter().circle_filled(r.left_center() + Vec2::new(12.0, 0.0), 4.0, rtt_color);
        ui.painter().galley(r.left_top() + Vec2::new(22.0, 6.0), galley, Color32::WHITE);
    }

    // Floating toolbar.
    toolbar(s, ui, area, lang, can_control, &displays, unread, &fingerprint, &peer, fullscreen, actions);
    let _ = area_resp;
}

fn handle_input(s: &mut Session, ui: &mut Ui, area: Rect, img: Rect, controlling: bool, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let events = ctx.input(|i| i.events.clone());
    let widget_focused = ctx.memory(|m| m.focused().is_some());
    let toolbar_rect = s.toolbar_rect;
    let over_toolbar = |p: Pos2| toolbar_rect.is_some_and(|r| r.expand(4.0).contains(p));
    let norm = |p: Pos2| -> (f32, f32) {
        (((p.x - img.left()) / img.width().max(1.0)).clamp(0.0, 1.0), ((p.y - img.top()) / img.height().max(1.0)).clamp(0.0, 1.0))
    };
    let mods = ctx.input(|i| i.modifiers);
    // Fullscreen hotkey that is never forwarded: Ctrl+Alt+Shift+F
    if mods.ctrl && mods.alt && mods.shift && ctx.input(|i| i.key_pressed(egui::Key::F)) {
        actions.push(Action::Fullscreen);
        return;
    }
    for ev in &events {
        match ev {
            egui::Event::PointerMoved(p) => {
                if controlling && area.contains(*p) && img.contains(*p) && !over_toolbar(*p) {
                    let n = norm(*p);
                    if s.last_move != Some(n) {
                        s.last_move = Some(n);
                        s.send(Msg::MouseMove { x: n.0, y: n.1 });
                    }
                }
            }
            egui::Event::PointerButton { pos, button, pressed, .. } => {
                let inside = area.contains(*pos) && !over_toolbar(*pos);
                if *pressed {
                    if inside {
                        s.keyboard_captured = true;
                        ctx.memory_mut(|m| {
                            if let Some(id) = m.focused() {
                                m.surrender_focus(id);
                            }
                        });
                    } else if s.keyboard_captured {
                        s.keyboard_captured = false;
                        release_keys(s);
                    }
                }
                if controlling {
                    let b = match button {
                        egui::PointerButton::Primary => 0u8,
                        egui::PointerButton::Secondary => 1,
                        egui::PointerButton::Middle => 2,
                        egui::PointerButton::Extra1 => 3,
                        egui::PointerButton::Extra2 => 4,
                    };
                    if *pressed && inside && img.contains(*pos) {
                        let n = norm(*pos);
                        s.send(Msg::MouseMove { x: n.0, y: n.1 });
                        s.send(Msg::MouseButton { button: b, down: true });
                        s.buttons.insert(b);
                    } else if !*pressed && s.buttons.remove(&b) {
                        s.send(Msg::MouseButton { button: b, down: false });
                    }
                }
            }
            egui::Event::MouseWheel { unit, delta, .. } => {
                let hovered = ctx.pointer_hover_pos().is_some_and(|p| area.contains(p) && !over_toolbar(p));
                if controlling && hovered {
                    let scale = match unit {
                        egui::MouseWheelUnit::Point => 1.0 / 50.0,
                        egui::MouseWheelUnit::Line => 1.0,
                        egui::MouseWheelUnit::Page => 3.0,
                    };
                    s.scroll_acc += *delta * scale;
                    let steps = Vec2::new(s.scroll_acc.x.trunc(), s.scroll_acc.y.trunc());
                    s.scroll_acc -= steps;
                    if steps != Vec2::ZERO {
                        s.send(Msg::Scroll { dx: -steps.x as i32, dy: -steps.y as i32 });
                    }
                }
            }
            egui::Event::Key { key, physical_key, pressed, .. } => {
                if controlling && s.keyboard_captured && !widget_focused {
                    let name = physical_key.unwrap_or(*key).name();
                    if is_known_key(name) {
                        if *pressed {
                            s.pressed.insert(name.to_string());
                            s.send(Msg::Key { code: name.to_string(), down: true });
                        } else if s.pressed.remove(name) {
                            s.send(Msg::Key { code: name.to_string(), down: false });
                        }
                    }
                }
            }
            egui::Event::Copy | egui::Event::Cut => {
                if controlling && s.keyboard_captured && !widget_focused {
                    let k = if matches!(ev, egui::Event::Copy) { "C" } else { "X" };
                    s.send(Msg::Key { code: k.into(), down: true });
                    s.send(Msg::Key { code: k.into(), down: false });
                }
            }
            egui::Event::Paste(text) => {
                if controlling && s.keyboard_captured && !widget_focused {
                    s.send(Msg::Clipboard(text.clone()));
                    s.send(Msg::Key { code: "V".into(), down: true });
                    s.send(Msg::Key { code: "V".into(), down: false });
                }
            }
            egui::Event::WindowFocused(false) => {
                release_keys(s);
            }
            _ => {}
        }
    }
    // Keep modifier keys in sync even if the platform did not report them as keys.
    if controlling && s.keyboard_captured && !widget_focused {
        for (on, left, right) in [(mods.shift, "ShiftLeft", "ShiftRight"), (mods.ctrl, "ControlLeft", "ControlRight"), (mods.alt, "AltLeft", "AltRight")] {
            let any = s.pressed.contains(left) || s.pressed.contains(right);
            if on && !any {
                s.pressed.insert(left.to_string());
                s.send(Msg::Key { code: left.into(), down: true });
            } else if !on && any {
                for k in [left, right] {
                    if s.pressed.remove(k) {
                        s.send(Msg::Key { code: k.into(), down: false });
                    }
                }
            }
        }
        // Keep egui from using Tab etc. for its own focus navigation while captured.
        ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_))));
    }
    if s.keyboard_captured && controlling {
        let hint_rect = Rect::from_min_size(area.right_top() + Vec2::new(-26.0, 10.0), Vec2::splat(16.0));
        ui.painter().text(hint_rect.center(), Align2::CENTER_CENTER, "⌨", egui::FontId::proportional(14.0), ACCENT2.linear_multiply(0.8));
    }
}

fn release_keys(s: &mut Session) {
    for k in std::mem::take(&mut s.pressed) {
        s.send(Msg::Key { code: k, down: false });
    }
    for b in std::mem::take(&mut s.buttons) {
        s.send(Msg::MouseButton { button: b, down: false });
    }
    s.send(Msg::ReleaseAll);
}

#[allow(clippy::too_many_arguments)]
fn toolbar(
    s: &mut Session,
    ui: &mut Ui,
    area: Rect,
    lang: Lang,
    can_control: bool,
    displays: &[crate::proto::DisplayInfo],
    unread: usize,
    fingerprint: &str,
    peer: &str,
    fullscreen: bool,
    actions: &mut Vec<Action>,
) {
    let ctx = ui.ctx().clone();
    let resp = egui::Area::new(egui::Id::new(("toolbar", s.sid)))
        .fixed_pos(area.center_top() + Vec2::new(0.0, 10.0))
        .pivot(Align2::CENTER_TOP)
        .order(egui::Order::Foreground)
        .show(&ctx, |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgba_unmultiplied(22, 25, 34, 236))
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .shadow(egui::Shadow { offset: [0, 4], blur: 18, spread: 0, color: Color32::from_black_alpha(120) })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if !s.toolbar_open {
                            if painted_tool(ui, tr(lang, "Покажи лентата", "Show toolbar"), false, Icon::Down).clicked() {
                                s.toolbar_open = true;
                            }
                            return;
                        }
                        if tool(ui, "🏠", tr(lang, "Към началния екран (сесията остава)", "Back to home (session stays open)"), false).clicked() {
                            actions.push(Action::Home);
                        }
                        ui.label(RichText::new("🔒").color(GOOD).size(13.0)).on_hover_text(format!(
                            "{}\n{} {fingerprint}",
                            tr(lang, "Криптирана връзка от край до край", "End-to-end encrypted connection"),
                            tr(lang, "Код за сигурност:", "Security code:")
                        ));
                        let mut name = peer.to_string();
                        if name.chars().count() > 18 {
                            name = name.chars().take(17).collect::<String>() + "…";
                        }
                        ui.label(RichText::new(name).font(egui::FontId::new(14.0, theme::bold())));
                        ui.add_space(6.0);
                        sep(ui);

                        // Display picker
                        if displays.len() > 1 {
                            let current = s.settings.display;
                            for (i, d) in displays.iter().enumerate() {
                                let label = format!("🖥 {}", i + 1);
                                if tool(ui, &label, &format!("{} ({}×{})", d.name, d.width, d.height), current == i).clicked() && current != i {
                                    s.settings.display = i;
                                    s.send(Msg::Stream(s.settings.clone()));
                                }
                            }
                            sep(ui);
                        }

                        // Picture settings
                        ui.menu_button(RichText::new("⚙").size(15.0), |ui| {
                            ui.set_min_width(230.0);
                            let mut changed = false;
                            ui.label(RichText::new(tr(lang, "Кодек", "Codec")).color(MUTED).size(12.0));
                            changed |= ui.radio_value(&mut s.settings.codec, Codec::H264, tr(lang, "H.264 — плавно (игри, видео)", "H.264 — smooth (games, video)")).changed();
                            changed |= ui.radio_value(&mut s.settings.codec, Codec::Lossless, tr(lang, "Без загуби — остър текст", "Lossless — sharp text")).changed();
                            ui.separator();
                            ui.label(RichText::new(tr(lang, "Качество", "Quality")).color(MUTED).size(12.0));
                            changed |= ui.radio_value(&mut s.settings.quality, Quality::Speed, tr(lang, "Скорост", "Speed")).changed();
                            changed |= ui.radio_value(&mut s.settings.quality, Quality::Balanced, tr(lang, "Баланс", "Balanced")).changed();
                            changed |= ui.radio_value(&mut s.settings.quality, Quality::Best, tr(lang, "Най-добро", "Best")).changed();
                            ui.separator();
                            ui.label(RichText::new(tr(lang, "Кадри в секунда", "Frames per second")).color(MUTED).size(12.0));
                            ui.horizontal(|ui| {
                                for f in [30u32, 60, 120, 144] {
                                    changed |= ui.selectable_value(&mut s.settings.fps, f, f.to_string()).changed();
                                }
                            });
                            ui.label(RichText::new(tr(lang, "Резолюция", "Resolution")).color(MUTED).size(12.0));
                            ui.horizontal(|ui| {
                                changed |= ui.selectable_value(&mut s.settings.resolution, Resolution::Native, tr(lang, "Оригинална", "Native")).changed();
                                changed |= ui.selectable_value(&mut s.settings.resolution, Resolution::Max1080, "1080p").changed();
                                changed |= ui.selectable_value(&mut s.settings.resolution, Resolution::Max720, "720p").changed();
                            });
                            if changed {
                                s.send(Msg::Stream(s.settings.clone()));
                            }
                        })
                        .response
                        .on_hover_text(tr(lang, "Картина и качество", "Picture & quality"));

                        if tool(ui, if s.fit { "🔍" } else { "🔎" }, if s.fit { tr(lang, "Мащаб: побиране (кликни за 1:1)", "Scale: fit (click for 1:1)") } else { tr(lang, "Мащаб: 1:1 (кликни за побиране)", "Scale: 1:1 (click to fit)") }, !s.fit).clicked() {
                            s.fit = !s.fit;
                        }
                        if can_control {
                            if tool(ui, "🖱", if s.view_only { tr(lang, "Само преглед — кликни за управление", "View only — click to control") } else { tr(lang, "Управление — кликни за само преглед", "Controlling — click for view only") }, !s.view_only).clicked() {
                                s.view_only = !s.view_only;
                                release_keys(s);
                            }
                            ui.menu_button(RichText::new("⌨").size(15.0), |ui| {
                                if ui.button("Ctrl + Alt + Del").clicked() {
                                    s.send(Msg::CtrlAltDel);
                                    ui.close();
                                }
                                for (label, keys) in [
                                    ("Alt + Tab", &["AltLeft", "Tab"][..]),
                                    ("Win", &["SuperLeft"][..]),
                                    ("Win + D", &["SuperLeft", "D"][..]),
                                    ("Alt + F4", &["AltLeft", "F4"][..]),
                                    ("Print Screen", &["PrintScreen"][..]),
                                ] {
                                    if ui.button(label).clicked() {
                                        for k in keys {
                                            s.send(Msg::Key { code: (*k).into(), down: true });
                                        }
                                        for k in keys.iter().rev() {
                                            s.send(Msg::Key { code: (*k).into(), down: false });
                                        }
                                        ui.close();
                                    }
                                }
                            })
                            .response
                            .on_hover_text(tr(lang, "Специални клавиши", "Special keys"));
                        } else {
                            ui.label(RichText::new("👁").color(MUTED)).on_hover_text(tr(lang, "Другата страна позволи само преглед", "The other side allowed viewing only"));
                        }
                        let clip_on = s.clip_on.load(Ordering::Relaxed);
                        if tool(ui, "📋", if clip_on { tr(lang, "Споделен клипборд: вкл.", "Shared clipboard: on") } else { tr(lang, "Споделен клипборд: изкл.", "Shared clipboard: off") }, clip_on).clicked() {
                            s.clip_on.store(!clip_on, Ordering::Relaxed);
                        }
                        if tool(ui, "📁", tr(lang, "Изпрати файлове (или ги пуснете върху екрана)", "Send files (or drop them on the screen)"), false).clicked() {
                            if let Some(files) = rfd::FileDialog::new().pick_files() {
                                if let Some(o) = s.outbox.lock().unwrap().clone() {
                                    send_files(o, files, s.file_progress.clone());
                                }
                            }
                        }
                        let chat_label = if unread > 0 && !s.show_chat { format!("💬{unread}") } else { "💬".into() };
                        if tool(ui, &chat_label, tr(lang, "Чат", "Chat"), s.show_chat).clicked() {
                            s.show_chat = !s.show_chat;
                        }
                        if painted_tool(ui, tr(lang, "Цял екран (Ctrl+Alt+Shift+F)", "Full screen (Ctrl+Alt+Shift+F)"), fullscreen, Icon::Fullscreen).clicked() {
                            actions.push(Action::Fullscreen);
                        }
                        sep(ui);
                        if painted_tool(ui, tr(lang, "Прекъсни", "Disconnect"), false, Icon::Power).clicked() {
                            actions.push(Action::Close);
                        }
                        if painted_tool(ui, tr(lang, "Скрий лентата", "Hide toolbar"), false, Icon::Up).clicked() {
                            s.toolbar_open = false;
                        }
                    });
                });
        });
    s.toolbar_rect = Some(resp.response.rect);
}

fn tool(ui: &mut Ui, icon: &str, tip: &str, active: bool) -> egui::Response {
    let btn = egui::Button::new(RichText::new(icon).size(15.0).color(if active { ACCENT2 } else { TEXT }))
        .min_size(Vec2::new(32.0, 30.0))
        .fill(if active { ACCENT.linear_multiply(0.25) } else { Color32::TRANSPARENT })
        .stroke(Stroke::NONE);
    ui.add(btn).on_hover_text(tip)
}

#[derive(Clone, Copy)]
pub enum Icon {
    Fullscreen,
    Power,
    Up,
    Down,
}

/// Toolbar button with an icon drawn from lines (for symbols fonts often lack).
pub fn painted_tool(ui: &mut Ui, tip: &str, active: bool, icon: Icon) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(32.0, 30.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, CornerRadius::same(8), ACCENT.linear_multiply(0.25));
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(8), CARD_HI);
    }
    let c = match icon {
        Icon::Power => BAD,
        _ if active => ACCENT2,
        _ => TEXT,
    };
    let st = Stroke::new(1.8, c);
    let m = rect.center();
    match icon {
        Icon::Fullscreen => {
            let r = Rect::from_center_size(m, Vec2::splat(14.0));
            let l = 4.5;
            for (corner, dx, dy) in [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.left_bottom(), 1.0, -1.0), (r.right_bottom(), -1.0, -1.0)] {
                p.line_segment([corner, corner + Vec2::new(l * dx, 0.0)], st);
                p.line_segment([corner, corner + Vec2::new(0.0, l * dy)], st);
            }
        }
        Icon::Power => {
            let r = 6.5f32;
            let pts: Vec<Pos2> = (0..=28)
                .map(|i| {
                    let a = (-50.0f32 + i as f32 * (280.0 / 28.0)).to_radians();
                    Pos2::new(m.x + r * a.cos(), m.y + 1.0 + r * a.sin())
                })
                .collect();
            p.add(egui::Shape::line(pts, st));
            p.line_segment([m + Vec2::new(0.0, -7.5), m + Vec2::new(0.0, 0.5)], st);
        }
        Icon::Up | Icon::Down => {
            let s = if matches!(icon, Icon::Up) { -1.0 } else { 1.0 };
            p.line_segment([m + Vec2::new(-5.0, -2.5 * s), m + Vec2::new(0.0, 2.5 * s)], st);
            p.line_segment([m + Vec2::new(0.0, 2.5 * s), m + Vec2::new(5.0, -2.5 * s)], st);
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tip)
}

fn sep(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(9.0, 22.0), Sense::hover());
    ui.painter().line_segment([r.center_top(), r.center_bottom()], Stroke::new(1.0, BORDER));
}

/// A chat transcript with an input line. Returns text to send.
pub fn chat_box(ui: &mut Ui, chat: &[ChatLine], input: &mut String, lang: Lang, height: f32) -> Option<String> {
    let mut out = None;
    egui::ScrollArea::vertical()
        .max_height(height.max(80.0))
        .stick_to_bottom(true)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if chat.is_empty() {
                ui.label(RichText::new(tr(lang, "Няма съобщения.", "No messages yet.")).color(MUTED).size(12.5));
            }
            for line in chat {
                let layout = if line.mine { egui::Layout::right_to_left(egui::Align::Min) } else { egui::Layout::left_to_right(egui::Align::Min) };
                ui.with_layout(layout, |ui| {
                    egui::Frame::new()
                        .fill(if line.mine { ACCENT.linear_multiply(0.55) } else { CARD_HI })
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_max_width(210.0);
                            ui.vertical(|ui| {
                                ui.label(&line.text);
                                ui.label(RichText::new(&line.time).size(10.5).color(Color32::from_white_alpha(140)));
                            });
                        });
                });
                ui.add_space(4.0);
            }
        });
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(input).hint_text(tr(lang, "Съобщение…", "Message…")).desired_width(ui.available_width() - 44.0));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("→").clicked() || enter) && !input.trim().is_empty() {
            out = Some(std::mem::take(input).trim().to_string());
            r.request_focus();
        }
    });
    out
}
