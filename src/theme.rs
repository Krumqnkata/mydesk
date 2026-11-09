//! Colours, fonts and small drawing helpers for the MyDesk look.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Mesh, Pos2, Rect, Sense,
    Shape, Stroke, TextStyle, Ui, Vec2, epaint::Vertex,
};

pub const BG: Color32 = Color32::from_rgb(0x0D, 0x0F, 0x15);
pub const PANEL: Color32 = Color32::from_rgb(0x12, 0x15, 0x1D);
pub const CARD: Color32 = Color32::from_rgb(0x18, 0x1C, 0x26);
pub const CARD_HI: Color32 = Color32::from_rgb(0x20, 0x25, 0x32);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2B, 0x38);
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xEA, 0xF0);
pub const MUTED: Color32 = Color32::from_rgb(0x8E, 0x97, 0xAA);
pub const ACCENT: Color32 = Color32::from_rgb(0x7C, 0x5C, 0xFF);
pub const ACCENT2: Color32 = Color32::from_rgb(0x22, 0xD3, 0xEE);
pub const GOOD: Color32 = Color32::from_rgb(0x22, 0xC5, 0x5E);
pub const WARN: Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B);
pub const BAD: Color32 = Color32::from_rgb(0xEF, 0x44, 0x44);

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

fn load_first(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let regular = load_first(&[
        "C:/Windows/Fonts/segoeui.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ]);
    let semibold = load_first(&[
        "C:/Windows/Fonts/seguisb.ttf",
        "C:/Windows/Fonts/segoeuib.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    ]);
    let fallback: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(r) = regular {
        fonts.font_data.insert("ui".into(), Arc::new(FontData::from_owned(r)));
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "ui".into());
    }
    let mut bold_list = Vec::new();
    if let Some(b) = semibold {
        fonts.font_data.insert("ui-bold".into(), Arc::new(FontData::from_owned(b)));
        bold_list.push("ui-bold".to_string());
    } else if fonts.font_data.contains_key("ui") {
        bold_list.push("ui".to_string());
    }
    bold_list.extend(fallback);
    fonts.families.insert(bold(), bold_list);
    ctx.set_fonts(fonts);

    ctx.global_style_mut(|style| {
        use FontFamily::*;
        style.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, bold())),
            (TextStyle::Body, FontId::new(14.5, Proportional)),
            (TextStyle::Button, FontId::new(14.5, Proportional)),
            (TextStyle::Small, FontId::new(12.0, Proportional)),
            (TextStyle::Monospace, FontId::new(13.5, Monospace)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(14.0, 7.0);
        style.spacing.interact_size.y = 32.0;
        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = Color32::from_rgb(0x0A, 0x0C, 0x11);
        v.faint_bg_color = CARD_HI;
        v.override_text_color = Some(TEXT);
        v.hyperlink_color = ACCENT2;
        v.selection.bg_fill = ACCENT.linear_multiply(0.55);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.window_corner_radius = CornerRadius::same(14);
        v.window_stroke = Stroke::new(1.0, BORDER);
        let r = CornerRadius::same(9);
        v.widgets.noninteractive.bg_fill = CARD;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.noninteractive.corner_radius = r;
        v.widgets.inactive.bg_fill = CARD_HI;
        v.widgets.inactive.weak_bg_fill = CARD_HI;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.inactive.corner_radius = r;
        v.widgets.hovered.bg_fill = Color32::from_rgb(0x2A, 0x30, 0x40);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2A, 0x30, 0x40);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.linear_multiply(0.7));
        v.widgets.hovered.corner_radius = r;
        v.widgets.active.bg_fill = ACCENT.linear_multiply(0.6);
        v.widgets.active.weak_bg_fill = ACCENT.linear_multiply(0.6);
        v.widgets.active.corner_radius = r;
        v.widgets.open.corner_radius = r;
    });
}

/// Fills a rounded rectangle with a left-to-right (or top-to-bottom) gradient.
pub fn gradient(ui: &Ui, rect: Rect, radius: f32, a: Color32, b: Color32, horizontal: bool) {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let mut pts: Vec<Pos2> = Vec::new();
    let corners = [
        (Pos2::new(rect.right() - r, rect.top() + r), -90.0f32),
        (Pos2::new(rect.right() - r, rect.bottom() - r), 0.0),
        (Pos2::new(rect.left() + r, rect.bottom() - r), 90.0),
        (Pos2::new(rect.left() + r, rect.top() + r), 180.0),
    ];
    for (c, start) in corners {
        for i in 0..=6 {
            let ang = (start + i as f32 * 15.0).to_radians();
            pts.push(Pos2::new(c.x + r * ang.cos(), c.y + r * ang.sin()));
        }
    }
    let color_at = |p: Pos2| {
        let t = if horizontal {
            (p.x - rect.left()) / rect.width().max(1.0)
        } else {
            (p.y - rect.top()) / rect.height().max(1.0)
        };
        lerp(a, b, t)
    };
    let mut mesh = Mesh::default();
    let center = rect.center();
    mesh.vertices.push(Vertex { pos: center, uv: egui::epaint::WHITE_UV, color: color_at(center) });
    for p in &pts {
        mesh.vertices.push(Vertex { pos: *p, uv: egui::epaint::WHITE_UV, color: color_at(*p) });
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % n]);
    }
    ui.painter().add(Shape::mesh(mesh));
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// A gradient call-to-action button.
pub fn primary_button(ui: &mut Ui, text: &str, width: f32, enabled: bool) -> egui::Response {
    let size = Vec2::new(width, 42.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let hovered = resp.hovered() && enabled;
    let (a, b) = if enabled {
        if hovered {
            (lerp(ACCENT, Color32::WHITE, 0.12), lerp(ACCENT2, Color32::WHITE, 0.12))
        } else {
            (ACCENT, Color32::from_rgb(0x4F, 0x8B, 0xF5))
        }
    } else {
        (CARD_HI, CARD_HI)
    };
    gradient(ui, rect, 10.0, a, b, true);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::new(15.0, bold()),
        if enabled { Color32::WHITE } else { MUTED },
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(egui::Margin::same(20))
}

pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.16))
        .corner_radius(CornerRadius::same(20))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, color);
                ui.label(egui::RichText::new(text).size(12.5).color(color));
            });
        });
}

/// The MyDesk logo: gradient tile with a small monitor.
pub fn logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    gradient(ui, rect, size * 0.28, ACCENT, ACCENT2, true);
    paint_monitor(ui, rect.shrink(size * 0.22), Color32::WHITE);
}

pub fn paint_monitor(ui: &Ui, r: Rect, color: Color32) {
    let p = ui.painter();
    let w = r.width();
    let screen = Rect::from_min_max(r.min, Pos2::new(r.max.x, r.min.y + r.height() * 0.68));
    p.rect_stroke(screen, CornerRadius::same((w * 0.08) as u8), Stroke::new((w * 0.09).max(1.5), color), egui::StrokeKind::Inside);
    let stand_top = Pos2::new(r.center().x, screen.bottom());
    let stand_bot = Pos2::new(r.center().x, r.bottom() - r.height() * 0.08);
    p.line_segment([stand_top, stand_bot], Stroke::new((w * 0.09).max(1.5), color));
    p.line_segment(
        [Pos2::new(r.left() + w * 0.25, r.bottom() - r.height() * 0.06), Pos2::new(r.right() - w * 0.25, r.bottom() - r.height() * 0.06)],
        Stroke::new((w * 0.09).max(1.5), color),
    );
}

/// Pleasant gradient pair picked from a string (used for contact thumbnails).
pub fn palette(seed: &str) -> (Color32, Color32) {
    const P: &[(Color32, Color32)] = &[
        (Color32::from_rgb(0x7C, 0x5C, 0xFF), Color32::from_rgb(0x22, 0xD3, 0xEE)),
        (Color32::from_rgb(0xF4, 0x3F, 0x5E), Color32::from_rgb(0xF5, 0x9E, 0x0B)),
        (Color32::from_rgb(0x10, 0xB9, 0x81), Color32::from_rgb(0x3B, 0x82, 0xF6)),
        (Color32::from_rgb(0xA8, 0x55, 0xF7), Color32::from_rgb(0xEC, 0x48, 0x99)),
        (Color32::from_rgb(0x06, 0xB6, 0xD4), Color32::from_rgb(0x84, 0xCC, 0x16)),
        (Color32::from_rgb(0xF9, 0x73, 0x16), Color32::from_rgb(0xEF, 0x44, 0x44)),
    ];
    let h = seed.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    P[h as usize % P.len()]
}

/// Builds the window icon (gradient tile with a monitor) in memory.
pub fn window_icon() -> egui::IconData {
    let s = 64usize;
    let mut rgba = vec![0u8; s * s * 4];
    let radius = 16.0f32;
    for y in 0..s {
        for x in 0..s {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(radius, s as f32 - radius);
            let cy = fy.clamp(radius, s as f32 - radius);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let alpha = (radius + 0.5 - d).clamp(0.0, 1.0);
            let c = lerp(ACCENT, ACCENT2, fx / s as f32);
            let i = (y * s + x) * 4;
            rgba[i] = c.r();
            rgba[i + 1] = c.g();
            rgba[i + 2] = c.b();
            rgba[i + 3] = (alpha * 255.0) as u8;
        }
    }
    // monitor outline
    let mut put = |x: usize, y: usize| {
        let i = (y * s + x) * 4;
        rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
    };
    for x in 14..50 {
        for t in 0..4 {
            put(x, 15 + t);
            put(x, 39 + t);
        }
    }
    for y in 15..43 {
        for t in 0..4 {
            put(14 + t, y);
            put(46 + t, y);
        }
    }
    for y in 43..50 {
        for t in 0..4 {
            put(30 + t, y);
        }
    }
    for x in 22..42 {
        for t in 0..3 {
            put(x, 49 + t);
        }
    }
    egui::IconData { rgba, width: s as u32, height: s as u32 }
}
