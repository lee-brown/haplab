//! Professional dark studio theme, palette, and layout helpers for HapLab.

use eframe::egui::{self, Color32, CornerRadius, Painter, Pos2, Rect, Stroke, Vec2};
use std::path::Path;

#[allow(dead_code)]
pub mod colors {
    use super::Color32;

    pub const BG_APP: Color32 = Color32::from_rgb(12, 12, 12);       // #0C0C0C leebrown.me base canvas
    pub const BG_CARD: Color32 = Color32::from_rgb(20, 20, 20);      // #141414 leebrown.me elevated card surface
    pub const BG_CARD_HOVER: Color32 = Color32::from_rgb(30, 30, 30); // #1E1E1E leebrown.me card hover
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(24, 24, 24);   // #181818 subtle elevation

    pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(30, 30, 30); // #1E1E1E leebrown.me divider rule
    pub const BORDER_ACTIVE: Color32 = Color32::from_rgb(232, 54, 79); // #E8364F leebrown.me signature accent

    pub const ACCENT_CYAN: Color32 = Color32::from_rgb(232, 54, 79);   // Map primary brand accent to #E8364F
    pub const ACCENT_BLUE: Color32 = Color32::from_rgb(232, 54, 79);   // Map primary action buttons to #E8364F
    pub const ACCENT_GREEN: Color32 = Color32::from_rgb(52, 211, 153); // Fresh mint green for smooth decode & success
    pub const ACCENT_AMBER: Color32 = Color32::from_rgb(245, 158, 11);
    pub const ACCENT_RED: Color32 = Color32::from_rgb(232, 54, 79);    // #E8364F signature crimson
    pub const ACCENT_PURPLE: Color32 = Color32::from_rgb(217, 70, 239);

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(232, 228, 223); // #E8E4DF leebrown.me primary text
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(138, 138, 138);   // #8A8A8A leebrown.me mid text
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(74, 74, 74);     // #4A4A4A leebrown.me dim text
}

/// Applies a cohesive, modern dark studio theme to the egui context matching leebrown.me aesthetic.
pub fn apply_studio_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = colors::BG_APP;
    visuals.window_fill = colors::BG_CARD;
    visuals.faint_bg_color = colors::BG_ELEVATED;
    visuals.extreme_bg_color = Color32::from_rgb(8, 8, 8);
    visuals.code_bg_color = Color32::from_rgb(16, 16, 16);

    visuals.widgets.noninteractive.bg_fill = colors::BG_CARD;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors::BORDER_SUBTLE);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colors::TEXT_PRIMARY);

    visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, colors::BORDER_SUBTLE);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, colors::TEXT_PRIMARY);

    visuals.widgets.hovered.bg_fill = colors::BG_CARD_HOVER;
    visuals.widgets.hovered.weak_bg_fill = colors::BG_CARD_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors::BORDER_SUBTLE);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    visuals.widgets.active.bg_fill = colors::ACCENT_RED;
    visuals.widgets.active.weak_bg_fill = colors::ACCENT_RED;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, colors::ACCENT_RED);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    visuals.widgets.open.bg_fill = colors::BG_CARD_HOVER;
    visuals.widgets.open.weak_bg_fill = colors::BG_CARD_HOVER;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, colors::BORDER_SUBTLE);
    visuals.widgets.open.corner_radius = CornerRadius::same(6);

    visuals.selection.bg_fill = colors::ACCENT_RED;
    visuals.selection.stroke = Stroke::NONE;
    visuals.window_stroke = Stroke::new(1.0, colors::BORDER_SUBTLE);
    visuals.window_corner_radius = CornerRadius::same(8);

    ctx.set_visuals(visuals);

    // Global spacing and padding for comfortable, breathable ergonomics
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.button_padding = Vec2::new(14.0, 7.0);
            style.spacing.item_spacing = Vec2::new(12.0, 10.0);
            style.spacing.interact_size.y = 32.0;
            style.spacing.combo_width = 180.0;
            style.spacing.slider_rail_height = 8.0;
            style.spacing.slider_width = 300.0;
        });
    }

    // Initialize custom studio typography once
    if !ctx.data(|d| d.get_temp::<bool>(egui::Id::new("studio_fonts_init")).unwrap_or(false)) {
        setup_studio_fonts(ctx);
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("studio_fonts_init"), true));
    }
}

/// Sets up premium, sleek system typography (Segoe UI on Windows).
pub fn setup_studio_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
        fonts.font_data.insert(
            "SegoeUI".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(data)),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "SegoeUI".to_owned());
    }
    if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguisb.ttf") {
        fonts.font_data.insert(
            "SegoeUISemibold".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(data)),
        );
    }
    if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguisym.ttf") {
        fonts.font_data.insert(
            "SegoeUISymbol".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(data)),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("SegoeUISymbol".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Renders a subtle checkerboard pattern on the canvas for transparent textures.
pub fn paint_transparency_checkerboard(painter: &Painter, rect: Rect) {
    painter.rect_filled(rect, 0, Color32::from_rgb(14, 14, 14));
    let cell = 16.0;
    let mut y = rect.min.y;
    let mut row = 0;
    while y < rect.max.y {
        let mut x = rect.min.x + if row % 2 == 1 { cell } else { 0.0 };
        while x < rect.max.x {
            let tile = Rect::from_min_size(
                egui::pos2(x, y),
                Vec2::new(cell.min(rect.max.x - x), cell.min(rect.max.y - y)),
            );
            painter.rect_filled(tile, 0, Color32::from_rgb(22, 22, 22));
            x += cell * 2.0;
        }
        y += cell;
        row += 1;
    }
}

/// Renders a styled chip/badge pill with leebrown.me border and typography.
pub fn render_badge(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32) {
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font_id, fg);
    let padding = Vec2::new(10.0, 4.0);
    let desired_size = galley.size() + padding * 2.0;
    let (rect, _response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
        ui.painter().rect_stroke(rect, CornerRadius::same(5), Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
    }
    let text_pos = rect.min + padding;
    ui.painter().galley(text_pos, galley, fg);
}

/// Formats a frame number and framerate into SMPTE timecode (HH:MM:SS:FF).
pub fn format_smpte_timecode(frame: usize, fps: f32) -> String {
    let fps_calc = fps.max(1.0);
    let total_secs = (frame as f32 / fps_calc) as u32;
    let ff = (frame as f32 % fps_calc) as u32;
    let ss = total_secs % 60;
    let mm = (total_secs / 60) % 60;
    let hh = total_secs / 3600;
    format!("{:02}:{:02}:{:02}:{:02}", hh, mm, ss, ff)
}

/// Formats byte sizes into human readable units (e.g. 14.2 MB).
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Reveals a file or directory in the native desktop file manager (Windows Explorer, Finder, etc.).
pub fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        if path.is_file() {
            let _ = std::process::Command::new("explorer")
                .arg(format!("/select,\"{}\"", path.display()))
                .spawn();
        } else {
            let _ = std::process::Command::new("explorer")
                .arg(path)
                .spawn();
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(if path.is_file() { "-R" } else { "" })
            .arg(path)
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let target = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let _ = std::process::Command::new("xdg-open")
            .arg(target)
            .spawn();
    }
}

/// Paints a sophisticated, organic ambient light glow effect with leebrown.me crimson and ivory flares.
/// Uses GPU-interpolated radial gradient mesh fans for 60 FPS rendering with near-zero CPU cost.
pub fn paint_ambient_glow(painter: &Painter, rect: Rect, time: f32, is_hovered: bool) {
    // Fill base deep dark-matter canvas across the entire rect matching leebrown.me
    painter.rect_filled(rect, 0.0, Color32::from_rgb(12, 12, 12));

    let cx = rect.center().x;
    let cy = rect.center().y;

    // Scale flares with window size so they seamlessly envelop the entire background
    let diag = (rect.width().powi(2) + rect.height().powi(2)).sqrt();
    let base_scale = (diag / 1100.0).max(0.85);

    // Helper to draw a soft luminous radial gradient flare mesh
    let draw_flare = |center: Pos2, radius: f32, color: Color32| {
        let segments = 36;
        let mut mesh = egui::Mesh::default();
        mesh.vertices.reserve(segments + 1);
        mesh.indices.reserve(segments * 3);

        let center_idx = mesh.vertices.len() as u32;
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center,
            uv: Pos2::ZERO,
            color,
        });

        for i in 0..segments {
            let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
            let p = center + Vec2::new(angle.cos() * radius, angle.sin() * radius);
            mesh.vertices.push(egui::epaint::Vertex {
                pos: p,
                uv: Pos2::ZERO,
                color: Color32::TRANSPARENT,
            });

            let next_i = (i + 1) % segments;
            mesh.indices.push(center_idx);
            mesh.indices.push(center_idx + 1 + i as u32);
            mesh.indices.push(center_idx + 1 + next_i as u32);
        }

        painter.add(egui::Shape::mesh(mesh));
    };

    let hover_scale: f32 = if is_hovered { 1.25 } else { 1.0 };
    let hover_alpha_mult: f32 = if is_hovered { 1.4 } else { 1.0 };
    let total_scale = base_scale * hover_scale;

    // 1. Signature Crimson Red Flare (#E8364F from leebrown.me)
    let p1_x = cx + (time * 0.45).sin() * 90.0;
    let p1_y = cy + (time * 0.35).cos() * 45.0;
    let r1 = (440.0 + (time * 0.8).sin() * 40.0) * total_scale;
    let a1 = ((48.0 * hover_alpha_mult).min(90.0)) as u8;
    draw_flare(Pos2::new(p1_x, p1_y), r1, Color32::from_rgba_premultiplied(232, 54, 79, a1));

    // 2. Secondary Warm Coral / Deep Ruby Flare
    let p2_x = cx - (time * 0.38).cos() * 100.0;
    let p2_y = cy - (time * 0.52).sin() * 50.0;
    let r2 = (380.0 + (time * 0.65).cos() * 30.0) * total_scale;
    let a2 = ((36.0 * hover_alpha_mult).min(75.0)) as u8;
    draw_flare(Pos2::new(p2_x, p2_y), r2, Color32::from_rgba_premultiplied(190, 40, 65, a2));

    // 3. Warm Ivory Flare (leebrown.me #E8E4DF Moonlight Core)
    let p3_x = cx + (time * 0.6).cos() * 38.0;
    let p3_y = cy + (time * 0.7).sin() * 26.0;
    let r3 = (240.0 + (time * 1.1).sin() * 25.0) * total_scale;
    let a3 = ((40.0 * hover_alpha_mult).min(75.0)) as u8;
    draw_flare(Pos2::new(p3_x, p3_y), r3, Color32::from_rgba_premultiplied(232, 228, 223, a3));

    // 4. Subtle Ambient Ruby Halo
    let p4_x = cx + (time * 0.25).sin() * 120.0;
    let p4_y = cy - (time * 0.3).cos() * 50.0;
    let r4 = 520.0 * total_scale;
    let a4 = ((22.0 * hover_alpha_mult).min(45.0)) as u8;
    draw_flare(Pos2::new(p4_x, p4_y), r4, Color32::from_rgba_premultiplied(140, 25, 45, a4));
}
