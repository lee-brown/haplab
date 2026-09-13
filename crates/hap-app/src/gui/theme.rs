//! Professional dark studio theme, palette, and layout helpers for HapLab.

use eframe::egui::{self, Color32, CornerRadius, Painter, Rect, Stroke, Vec2};
use std::path::Path;

#[allow(dead_code)]
pub mod colors {
    use super::Color32;

    pub const BG_APP: Color32 = Color32::from_rgb(15, 18, 24);
    pub const BG_CARD: Color32 = Color32::from_rgb(24, 28, 38);
    pub const BG_CARD_HOVER: Color32 = Color32::from_rgb(32, 38, 52);
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(20, 24, 34);

    pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(40, 48, 64);
    pub const BORDER_ACTIVE: Color32 = Color32::from_rgb(56, 189, 248);

    pub const ACCENT_CYAN: Color32 = Color32::from_rgb(56, 189, 248);
    pub const ACCENT_BLUE: Color32 = Color32::from_rgb(2, 132, 199);
    pub const ACCENT_GREEN: Color32 = Color32::from_rgb(16, 185, 129);
    pub const ACCENT_AMBER: Color32 = Color32::from_rgb(245, 158, 11);
    pub const ACCENT_RED: Color32 = Color32::from_rgb(239, 68, 68);
    pub const ACCENT_PURPLE: Color32 = Color32::from_rgb(168, 85, 247);

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(248, 250, 252);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(100, 116, 139);
}

/// Applies a cohesive, modern dark studio theme to the egui context.
pub fn apply_studio_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = colors::BG_APP;
    visuals.window_fill = colors::BG_CARD;
    visuals.faint_bg_color = colors::BG_ELEVATED;
    visuals.extreme_bg_color = Color32::from_rgb(11, 13, 18);
    visuals.code_bg_color = Color32::from_rgb(18, 22, 30);

    visuals.widgets.noninteractive.bg_fill = colors::BG_CARD;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors::BORDER_SUBTLE);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colors::TEXT_PRIMARY);

    visuals.widgets.inactive.bg_fill = Color32::from_rgb(28, 34, 46);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, colors::BORDER_SUBTLE);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, colors::TEXT_PRIMARY);

    visuals.widgets.hovered.bg_fill = colors::BG_CARD_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors::BORDER_ACTIVE);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    visuals.widgets.active.bg_fill = colors::ACCENT_BLUE;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, colors::BORDER_ACTIVE);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    visuals.selection.bg_fill = colors::ACCENT_BLUE;
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
        });
    }
}

/// Renders a subtle checkerboard pattern on the canvas for transparent textures.
pub fn paint_transparency_checkerboard(painter: &Painter, rect: Rect) {
    painter.rect_filled(rect, 0, Color32::from_rgb(20, 24, 32));
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
            painter.rect_filled(tile, 0, Color32::from_rgb(28, 34, 46));
            x += cell * 2.0;
        }
        y += cell;
        row += 1;
    }
}

/// Renders a styled chip/badge pill.
pub fn render_badge(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32) {
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font_id, fg);
    let padding = Vec2::new(10.0, 4.0);
    let desired_size = galley.size() + padding * 2.0;
    let (rect, _response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());

    ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
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
