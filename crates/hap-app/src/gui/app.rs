//! Main eframe / egui application interface with a modern, high-polish dark studio UX.

use super::theme::{
    apply_studio_theme, colors, format_bytes, format_smpte_timecode,
    paint_transparency_checkerboard, render_badge, reveal_in_file_manager,
};
use crate::worker::{
    spawn_encode_worker, spawn_export_worker, EncodeJobConfig, WorkerProgress,
};
use crossbeam_channel::Receiver;
use eframe::egui::{
    self, Color32, CornerRadius, Rect, RichText, Stroke, TextureOptions, Vec2,
};
use hap_core::{decode_frame_to_rgba, HapFormat, QtHapReader};
use image::GenericImageView;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    PlayerInspector,
    Encoder,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelViewMode {
    Rgba,
    AlphaMatte,
    RgbOpaque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundViewMode {
    Checkerboard,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderPreset {
    HapQRecommended,
    HapRUltra,
    HapQAlphaTransparent,
    Hap1Fast,
    HapAlphaLight,
    Custom,
}

impl EncoderPreset {
    pub fn name(&self) -> &'static str {
        match self {
            Self::HapQRecommended => "🌟 Hap Q (Production)",
            Self::HapRUltra => "💎 Hap R (BC7 Ultra)",
            Self::HapQAlphaTransparent => "🎭 Hap Q Alpha (Broadcast)",
            Self::Hap1Fast => "⚡ Hap 1 (Fastest)",
            Self::HapAlphaLight => "🪶 Hap Alpha (DXT5)",
            Self::Custom => "⚙️ Custom Settings",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::HapQRecommended => "Scaled YCoCg-DXT5 with Snappy. The industry standard for high-fidelity stage & projection.",
            Self::HapRUltra => "State-of-the-art BC7 UNORM. Superior quality for UI, graphics, and crisp alpha transparency.",
            Self::HapQAlphaTransparent => "Dual-stream YCoCg color + uncompressed BC4 alpha matte. Perfect for transparent broadcast graphics.",
            Self::Hap1Fast => "Standard DXT1 RGB. Lowest CPU decode overhead for extreme multi-screen playback.",
            Self::HapAlphaLight => "Standard DXT5 RGBA. Compact single-texture transparent video.",
            Self::Custom => "Manually customize codec flavour, threading chunk partitions, and Snappy compression.",
        }
    }
}

pub struct HapStudioApp {
    active_tab: ActiveTab,
    toast: Option<(String, Instant, Color32)>,

    // --- Player / Inspector State ---
    mov_path: Option<PathBuf>,
    reader: Option<QtHapReader>,
    current_frame: usize,
    is_playing: bool,
    loop_playback: bool,
    last_frame_time: Instant,
    preview_texture: Option<egui::TextureHandle>,
    raw_frame_cache: Option<Vec<u8>>,
    channel_mode: ChannelViewMode,
    bg_mode: BackgroundViewMode,
    show_export_panel: bool,
    export_format: String,
    export_rx: Option<Receiver<WorkerProgress>>,
    export_cancel: Option<Arc<AtomicBool>>,
    export_status: Option<WorkerProgress>,

    // --- Encoder State ---
    enc_input_path: Option<PathBuf>,
    enc_output_path: Option<PathBuf>,
    enc_detected_frames: usize,
    enc_detected_w: u32,
    enc_detected_h: u32,
    enc_detected_first_name: Option<String>,
    enc_detected_last_name: Option<String>,
    enc_thumbnail_texture: Option<egui::TextureHandle>,
    enc_preset: EncoderPreset,
    enc_format: HapFormat,
    enc_fps: f32,
    enc_chunks: usize,
    enc_snappy: bool,
    enc_start_time: Option<Instant>,
    enc_rx: Option<Receiver<WorkerProgress>>,
    enc_cancel: Option<Arc<AtomicBool>>,
    enc_status: Option<WorkerProgress>,
    enc_last_successful_mov: Option<PathBuf>,

    // --- System / Hardware State ---
    gpu_adapter_name: String,
    gpu_backend_name: String,
    gpu_supports_bc: bool,
    system_logs: Vec<String>,
    log_search: String,
}

impl Default for HapStudioApp {
    fn default() -> Self {
        let instance = wgpu::Instance::default();
        let adapter_res: Result<wgpu::Adapter, _> = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
        ));
        let (adapter_name, backend_name, supports_bc) = match adapter_res {
            Ok(adapter) => {
                let info = adapter.get_info();
                let has_bc = adapter
                    .features()
                    .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
                (info.name, format!("{:?}", info.backend), has_bc)
            }
            Err(_) => ("Software / Fallback".to_string(), "None".to_string(), false),
        };

        let mut app = Self {
            active_tab: ActiveTab::PlayerInspector,
            toast: None,

            mov_path: None,
            reader: None,
            current_frame: 0,
            is_playing: false,
            loop_playback: true,
            last_frame_time: Instant::now(),
            preview_texture: None,
            raw_frame_cache: None,
            channel_mode: ChannelViewMode::Rgba,
            bg_mode: BackgroundViewMode::Checkerboard,
            show_export_panel: false,
            export_format: "png".to_string(),
            export_rx: None,
            export_cancel: None,
            export_status: None,

            enc_input_path: None,
            enc_output_path: None,
            enc_detected_frames: 0,
            enc_detected_w: 0,
            enc_detected_h: 0,
            enc_detected_first_name: None,
            enc_detected_last_name: None,
            enc_thumbnail_texture: None,
            enc_preset: EncoderPreset::HapQRecommended,
            enc_format: HapFormat::HapY,
            enc_fps: 30.0,
            enc_chunks: 4,
            enc_snappy: true,
            enc_start_time: None,
            enc_rx: None,
            enc_cancel: None,
            enc_status: None,
            enc_last_successful_mov: None,

            gpu_adapter_name: adapter_name,
            gpu_backend_name: backend_name,
            gpu_supports_bc: supports_bc,
            system_logs: Vec::new(),
            log_search: String::new(),
        };

        app.log("HAP Video Studio initialized.");
        app.log(&format!(
            "Hardware GPU: {} [{}]",
            app.gpu_adapter_name, app.gpu_backend_name
        ));
        app.log(&format!(
            "Direct BC Texture Uploads: {}",
            if app.gpu_supports_bc {
                "Supported (wgpu)"
            } else {
                "Pure Rust Fallback"
            }
        ));

        app
    }
}

impl HapStudioApp {
    fn log(&mut self, msg: &str) {
        let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();
        self.system_logs.push(format!("[{}] {}", timestamp, msg));
    }

    fn notify(&mut self, msg: impl Into<String>, color: Color32) {
        self.toast = Some((msg.into(), Instant::now(), color));
    }

    fn apply_preset(&mut self, preset: EncoderPreset) {
        self.enc_preset = preset;
        match preset {
            EncoderPreset::HapQRecommended => {
                self.enc_format = HapFormat::HapY;
                self.enc_chunks = 4;
                self.enc_snappy = true;
            }
            EncoderPreset::HapRUltra => {
                self.enc_format = HapFormat::Hap7;
                self.enc_chunks = 8;
                self.enc_snappy = true;
            }
            EncoderPreset::HapQAlphaTransparent => {
                self.enc_format = HapFormat::HapM;
                self.enc_chunks = 4;
                self.enc_snappy = true;
            }
            EncoderPreset::Hap1Fast => {
                self.enc_format = HapFormat::Hap1;
                self.enc_chunks = 4;
                self.enc_snappy = false;
            }
            EncoderPreset::HapAlphaLight => {
                self.enc_format = HapFormat::Hap5;
                self.enc_chunks = 4;
                self.enc_snappy = true;
            }
            EncoderPreset::Custom => {}
        }
        self.update_suggested_output_filename();
    }

    fn update_suggested_output_filename(&mut self) {
        if let Some(ref in_path) = self.enc_input_path {
            let base_stem = if in_path.is_dir() {
                in_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "output".to_string())
            } else {
                in_path
                    .file_stem()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "output".to_string())
            };

            let suffix = match self.enc_format {
                HapFormat::Hap1 => "_Hap1",
                HapFormat::Hap5 => "_HapAlpha",
                HapFormat::HapY => "_HapQ",
                HapFormat::HapM => "_HapQAlpha",
                HapFormat::HapA => "_HapAlphaOnly",
                HapFormat::Hap7 => "_HapR",
                HapFormat::HapH => "_HapHDR",
            };

            let parent_dir = if in_path.is_dir() {
                in_path.clone()
            } else {
                in_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."))
            };

            self.enc_output_path =
                Some(parent_dir.join(format!("{}{}.mov", base_stem, suffix)));
        }
    }

    pub fn open_mov_file(&mut self, path: PathBuf, ctx: &egui::Context) {
        match QtHapReader::open(&path) {
            Ok(reader) => {
                let filename = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.log(&format!(
                    "Opened MOV: {} ({}x{}, {:.2} fps, {} frames, {})",
                    filename,
                    reader.width(),
                    reader.height(),
                    reader.fps(),
                    reader.frame_count(),
                    reader.format().name()
                ));
                self.notify(
                    format!("Loaded {}: {} frames", filename, reader.frame_count()),
                    colors::ACCENT_GREEN,
                );
                self.reader = Some(reader);
                self.mov_path = Some(path);
                self.current_frame = 0;
                self.is_playing = false;
                self.raw_frame_cache = None;
                self.update_preview_frame(ctx);
            }
            Err(e) => {
                let err_msg = format!("Failed to open MOV: {}", e);
                self.log(&err_msg);
                self.notify(err_msg, colors::ACCENT_RED);
            }
        }
    }

    fn update_preview_frame(&mut self, ctx: &egui::Context) {
        if let Some(ref mut reader) = self.reader {
            let width = reader.width() as usize;
            let height = reader.height() as usize;
            let frame_idx = self
                .current_frame
                .min(reader.frame_count().saturating_sub(1));

            if let Ok(packet) = reader.read_frame_packet(frame_idx) {
                if let Ok(mut rgba) = decode_frame_to_rgba(&packet, width, height) {
                    self.raw_frame_cache = Some(rgba.clone());
                    self.rebuild_texture_from_cache(ctx, width, height, &mut rgba);
                }
            }
        }
    }

    fn rebuild_texture_from_cache(
        &mut self,
        ctx: &egui::Context,
        width: usize,
        height: usize,
        rgba: &mut [u8],
    ) {
        match self.channel_mode {
            ChannelViewMode::Rgba => {}
            ChannelViewMode::AlphaMatte => {
                for chunk in rgba.chunks_exact_mut(4) {
                    let a = chunk[3];
                    chunk[0] = a;
                    chunk[1] = a;
                    chunk[2] = a;
                    chunk[3] = 255;
                }
            }
            ChannelViewMode::RgbOpaque => {
                for chunk in rgba.chunks_exact_mut(4) {
                    chunk[3] = 255;
                }
            }
        }

        let color_img = egui::ColorImage::from_rgba_unmultiplied([width, height], rgba);
        self.preview_texture = Some(ctx.load_texture(
            "video-preview",
            color_img,
            TextureOptions::LINEAR,
        ));
    }

    fn scan_encoder_input(&mut self, ctx: &egui::Context) {
        if let Some(ref path) = self.enc_input_path {
            let mut count = 0;
            let mut first_file = None;
            let mut last_file = None;

            if path.is_dir() {
                if let Ok(entries) = fs::read_dir(path) {
                    let mut files: Vec<PathBuf> = entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.extension()
                                .and_then(|e| e.to_str())
                                .map(|e| {
                                    matches!(
                                        e.to_lowercase().as_str(),
                                        "png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" | "webp"
                                    )
                                })
                                .unwrap_or(false)
                        })
                        .collect();
                    files.sort();
                    count = files.len();
                    first_file = files.first().cloned();
                    last_file = files.last().cloned();
                }
            } else if path.is_file() {
                count = 1;
                first_file = Some(path.clone());
                last_file = Some(path.clone());
            }

            self.enc_detected_frames = count;
            self.enc_detected_first_name = first_file
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|f| f.to_string_lossy().to_string());
            self.enc_detected_last_name = last_file
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|f| f.to_string_lossy().to_string());

            if let Some(ref first) = first_file {
                if let Ok(img) = image::open(first) {
                    let (w, h) = img.dimensions();
                    self.enc_detected_w = w;
                    self.enc_detected_h = h;

                    let thumb = img.thumbnail(180, 180).to_rgba8();
                    let (tw, th) = thumb.dimensions();
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(
                        [tw as usize, th as usize],
                        &thumb,
                    );
                    self.enc_thumbnail_texture = Some(ctx.load_texture(
                        "enc-thumb",
                        color_img,
                        TextureOptions::LINEAR,
                    ));
                }
            }

            self.update_suggested_output_filename();
            self.notify(
                format!("Detected {} frames ({}x{})", count, self.enc_detected_w, self.enc_detected_h),
                colors::ACCENT_CYAN,
            );
        }
    }
}

impl eframe::App for HapStudioApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_studio_theme(ctx);

        // 1. Drag & Drop Handling
        ctx.input(|i| {
            if let Some(dropped) = i.raw.dropped_files.first() {
                if let Some(ref path) = dropped.path {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        let ext_lower = ext.to_lowercase();
                        if ext_lower == "mov" || ext_lower == "mp4" {
                            self.open_mov_file(path.clone(), ctx);
                            self.active_tab = ActiveTab::PlayerInspector;
                        } else if matches!(
                            ext_lower.as_str(),
                            "png" | "jpg" | "jpeg" | "tiff" | "bmp" | "webp"
                        ) {
                            self.enc_input_path = Some(path.clone());
                            self.scan_encoder_input(ctx);
                            self.active_tab = ActiveTab::Encoder;
                        }
                    } else if path.is_dir() {
                        self.enc_input_path = Some(path.clone());
                        self.scan_encoder_input(ctx);
                        self.active_tab = ActiveTab::Encoder;
                    }
                }
            }
        });

        // 2. Keyboard Shortcuts (Global & Player)
        if self.active_tab == ActiveTab::PlayerInspector && self.reader.is_some() {
            ctx.input(|i| {
                let frame_count = self.reader.as_ref().map(|r| r.frame_count()).unwrap_or(0);
                if frame_count > 0 {
                    if i.key_pressed(egui::Key::Space) {
                        self.is_playing = !self.is_playing;
                        self.last_frame_time = Instant::now();
                    }
                    if i.key_pressed(egui::Key::ArrowLeft) {
                        let step = if i.modifiers.shift { 10 } else { 1 };
                        self.current_frame = self.current_frame.saturating_sub(step);
                        self.update_preview_frame(ctx);
                    }
                    if i.key_pressed(egui::Key::ArrowRight) {
                        let step = if i.modifiers.shift { 10 } else { 1 };
                        self.current_frame = (self.current_frame + step).min(frame_count.saturating_sub(1));
                        self.update_preview_frame(ctx);
                    }
                    if i.key_pressed(egui::Key::Home) {
                        self.current_frame = 0;
                        self.update_preview_frame(ctx);
                    }
                    if i.key_pressed(egui::Key::End) {
                        self.current_frame = frame_count.saturating_sub(1);
                        self.update_preview_frame(ctx);
                    }
                    if i.key_pressed(egui::Key::L) {
                        self.loop_playback = !self.loop_playback;
                        self.notify(
                            format!("Loop: {}", if self.loop_playback { "Enabled" } else { "Disabled" }),
                            colors::ACCENT_CYAN,
                        );
                    }
                }
            });
        }

        // 3. Playback Frame Interval Tick
        if self.is_playing {
            if let Some(ref reader) = self.reader {
                let fps = reader.fps().max(1.0);
                let frame_interval = 1.0 / fps;
                if self.last_frame_time.elapsed().as_secs_f32() >= frame_interval {
                    let count = reader.frame_count();
                    if count > 0 {
                        if self.current_frame + 1 < count {
                            self.current_frame += 1;
                        } else if self.loop_playback {
                            self.current_frame = 0;
                        } else {
                            self.is_playing = false;
                        }
                    }
                    self.last_frame_time = Instant::now();
                    self.update_preview_frame(ctx);
                }
                ctx.request_repaint();
            }
        }

        // 4. Background Workers Polling
        if let Some(ref rx) = self.enc_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerProgress::Finished { ref message } => {
                        self.log(message);
                        self.notify(message.clone(), colors::ACCENT_GREEN);
                        self.enc_last_successful_mov = self.enc_output_path.clone();
                        self.enc_status = Some(msg);
                        self.enc_rx = None;
                        break;
                    }
                    WorkerProgress::Error(ref err) => {
                        self.log(&format!("Encode error: {}", err));
                        self.notify(format!("Encode failed: {}", err), colors::ACCENT_RED);
                        self.enc_status = Some(msg);
                        self.enc_rx = None;
                        break;
                    }
                    _ => {
                        self.enc_status = Some(msg);
                    }
                }
            }
            ctx.request_repaint();
        }

        if let Some(ref rx) = self.export_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerProgress::Finished { ref message } => {
                        self.log(message);
                        self.notify(message.clone(), colors::ACCENT_GREEN);
                        self.export_status = Some(msg);
                        self.export_rx = None;
                        break;
                    }
                    WorkerProgress::Error(ref err) => {
                        self.log(&format!("Export error: {}", err));
                        self.notify(format!("Export failed: {}", err), colors::ACCENT_RED);
                        self.export_status = Some(msg);
                        self.export_rx = None;
                        break;
                    }
                    _ => {
                        self.export_status = Some(msg);
                    }
                }
            }
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let is_dragging = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());

        ui.vertical(|ui| {
            // --- TOP TITLE BAR ---
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(
                    RichText::new("⚡ HAP Video Studio")
                        .size(20.0)
                        .color(colors::ACCENT_CYAN)
                        .strong(),
                );
                ui.label(RichText::new("v0.1.0 • Pure Rust • Zero FFmpeg").color(colors::TEXT_FAINT));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.gpu_supports_bc {
                        render_badge(ui, "⚡ GPU BC ACCELERATED", Color32::from_rgb(16, 50, 35), colors::ACCENT_GREEN);
                    } else {
                        render_badge(ui, "🖥 CPU FALLBACK", Color32::from_rgb(50, 45, 20), colors::ACCENT_AMBER);
                    }
                });
            });

            ui.add_space(4.0);

            // --- NAVIGATION TABS ---
            ui.horizontal(|ui| {
                let tab_btn = |ui: &mut egui::Ui, active: bool, title: &str, badge_count: Option<usize>| {
                    let bg = if active { colors::BG_CARD_HOVER } else { Color32::TRANSPARENT };
                    let border = if active { Stroke::new(1.5, colors::ACCENT_CYAN) } else { Stroke::NONE };
                    let fg = if active { Color32::WHITE } else { colors::TEXT_MUTED };

                    let btn = egui::Button::new(RichText::new(title).color(fg).strong())
                        .fill(bg)
                        .stroke(border)
                        .corner_radius(CornerRadius::same(6));
                    let resp = ui.add(btn);
                    if let Some(cnt) = badge_count {
                        if cnt > 0 {
                            render_badge(ui, &format!("{}", cnt), Color32::from_rgb(30, 45, 65), colors::ACCENT_CYAN);
                        }
                    }
                    resp.clicked()
                };

                let mov_loaded = self.reader.is_some();
                if tab_btn(ui, self.active_tab == ActiveTab::PlayerInspector, "🎬 Player & Inspector", if mov_loaded { Some(1) } else { None }) {
                    self.active_tab = ActiveTab::PlayerInspector;
                }

                let frames_detected = self.enc_detected_frames;
                if tab_btn(ui, self.active_tab == ActiveTab::Encoder, "🚀 Video Encoder", if frames_detected > 0 { Some(frames_detected) } else { None }) {
                    self.active_tab = ActiveTab::Encoder;
                }

                if tab_btn(ui, self.active_tab == ActiveTab::Diagnostics, "🛠 Diagnostics & Logs", None) {
                    self.active_tab = ActiveTab::Diagnostics;
                }
            });

            ui.separator();

            // Drag & Drop Hover Border Overlay
            if is_dragging {
                ui.painter().rect_stroke(
                    ui.max_rect(),
                    CornerRadius::same(8),
                    Stroke::new(2.5, colors::ACCENT_CYAN),
                    egui::StrokeKind::Inside,
                );
            }

            // --- TAB CONTENT ---
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    match self.active_tab {
                        ActiveTab::PlayerInspector => self.show_player_tab(ui),
                        ActiveTab::Encoder => self.show_encoder_tab(ui),
                        ActiveTab::Diagnostics => self.show_diagnostics_tab(ui),
                    }
                    ui.add_space(24.0);
                });

            // --- FOOTER STATUS & TOAST ---
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.separator();
                ui.horizontal(|ui| {
                    if let Some((ref msg, time, color)) = self.toast {
                        if time.elapsed().as_secs_f32() < 4.0 {
                            render_badge(ui, "●", Color32::TRANSPARENT, color);
                            ui.label(RichText::new(msg).color(color).strong());
                        } else {
                            self.toast = None;
                        }
                    } else if let Some(ref path) = self.mov_path {
                        ui.label(RichText::new(format!("Active: {}", path.display())).color(colors::TEXT_MUTED));
                    } else {
                        ui.label(RichText::new("Drop a HAP .mov to play & inspect, or image files to encode.").color(colors::TEXT_FAINT));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("GPU: {}", self.gpu_adapter_name)).color(colors::TEXT_FAINT).size(11.0));
                    });
                });
            });
        });
    }
}

// ---------------------------------------------------------------------------
// TAB 1: PLAYER & INSPECTOR
// ---------------------------------------------------------------------------
impl HapStudioApp {
    fn show_player_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // If no file loaded, display prominent Drop Zone Hero Card
        if self.reader.is_none() {
            ui.add_space(20.0);
            let frame = egui::Frame::canvas(ui.style())
                .fill(colors::BG_CARD)
                .stroke(Stroke::new(1.5, colors::BORDER_SUBTLE))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::same(30));

            frame.show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("🎬").size(48.0));
                    ui.add_space(10.0);
                    ui.heading(RichText::new("Drag & Drop any HAP Video (.mov)").size(22.0).color(Color32::WHITE).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new("Instant hardware-accelerated playback and technical stream inspection.").color(colors::TEXT_MUTED));
                    ui.label(RichText::new("Supports Hap 1, Hap Alpha, Hap Q, Hap Q Alpha, Hap R (BC7), and Hap HDR (BC6H).").color(colors::TEXT_FAINT).size(12.0));
                    ui.add_space(16.0);

                    ui.horizontal(|ui| {
                        ui.add_space(ui.available_width() * 0.5 - 130.0);
                        if ui.button(RichText::new("📂 Open HAP File...").size(15.0).strong()).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("QuickTime HAP Video", &["mov", "mp4"])
                                .pick_file()
                            {
                                self.open_mov_file(path, &ctx);
                            }
                        }

                        if ui.button(RichText::new("🚀 Switch to Encoder").size(15.0)).clicked() {
                            self.active_tab = ActiveTab::Encoder;
                        }
                    });
                });
            });
            return;
        }

        let (format, width, height, fps, count, duration) = {
            let r = self.reader.as_ref().unwrap();
            (r.format(), r.width(), r.height(), r.fps(), r.frame_count(), r.duration())
        };

        // --- TOP TOOLBAR ---
        ui.horizontal(|ui| {
            if ui.button("📂 Open Another...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("QuickTime HAP Video", &["mov", "mp4"])
                    .pick_file()
                {
                    self.open_mov_file(path, &ctx);
                }
            }

            if let Some(ref path) = self.mov_path {
                if ui.button("📂 Reveal in Explorer").clicked() {
                    reveal_in_file_manager(path);
                }
            }

            let export_text = if self.show_export_panel { "▲ Hide Exporter" } else { "📤 Export PNG Sequence..." };
            if ui.selectable_label(self.show_export_panel, export_text).clicked() {
                self.show_export_panel = !self.show_export_panel;
            }

            ui.separator();

            // Channel Mode Selector
            ui.label(RichText::new("Channels:").color(colors::TEXT_MUTED));
            let old_mode = self.channel_mode;
            ui.selectable_value(&mut self.channel_mode, ChannelViewMode::Rgba, "🎨 RGBA");
            ui.selectable_value(&mut self.channel_mode, ChannelViewMode::AlphaMatte, "👁 Alpha Mask");
            ui.selectable_value(&mut self.channel_mode, ChannelViewMode::RgbOpaque, "🌈 RGB Only");
            if old_mode != self.channel_mode {
                if let Some(ref mut raw) = self.raw_frame_cache {
                    let mut copy = raw.clone();
                    self.rebuild_texture_from_cache(&ctx, width as usize, height as usize, &mut copy);
                }
            }

            ui.separator();

            // Background Mode Selector
            ui.label(RichText::new("BG:").color(colors::TEXT_MUTED));
            ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Checkerboard, "🔲 Check");
            ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Dark, "⬛ Dark");
            ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Light, "⬜ Light");
        });

        // --- EXPORT PANEL (Collapsible) ---
        if self.show_export_panel {
            ui.add_space(4.0);
            let frame = egui::Frame::canvas(ui.style())
                .fill(colors::BG_CARD)
                .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(12));

            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Frame Exporter:");
                    ui.label("Format:");
                    egui::ComboBox::from_id_salt("export_fmt_box")
                        .selected_text(&self.export_format)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.export_format, "png".into(), "PNG (.png)");
                            ui.selectable_value(&mut self.export_format, "jpg".into(), "JPEG (.jpg)");
                            ui.selectable_value(&mut self.export_format, "tiff".into(), "TIFF (.tiff)");
                        });

                    if ui.button(RichText::new("📁 Choose Output Folder & Start Export").strong()).clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            if let Some(ref path) = self.mov_path {
                                let cancel_flag = Arc::new(AtomicBool::new(false));
                                let (tx, rx) = crossbeam_channel::unbounded();
                                self.export_rx = Some(rx);
                                self.export_cancel = Some(cancel_flag.clone());
                                spawn_export_worker(path.clone(), folder, self.export_format.clone(), cancel_flag, tx);
                            }
                        }
                    }
                });

                if let Some(ref status) = self.export_status {
                    ui.add_space(6.0);
                    match status {
                        WorkerProgress::Started { total } => {
                            ui.label(format!("Starting export of {} frames...", total));
                        }
                        WorkerProgress::Progress { current, total, fps, percent } => {
                            ui.horizontal(|ui| {
                                ui.label(format!("Exporting: [{}/{}] ({:.1} fps)", current, total, fps));
                                ui.add(egui::ProgressBar::new(*percent / 100.0).show_percentage());
                                if let Some(ref cancel) = self.export_cancel {
                                    if ui.button("Cancel").clicked() {
                                        cancel.store(true, Ordering::Relaxed);
                                    }
                                }
                            });
                        }
                        WorkerProgress::Finished { message } => {
                            ui.label(RichText::new(message).color(colors::ACCENT_GREEN).strong());
                        }
                        WorkerProgress::Error(err) => {
                            ui.label(RichText::new(format!("Export Error: {}", err)).color(colors::ACCENT_RED).strong());
                        }
                    }
                }
            });
        }

        ui.add_space(6.0);

        // --- MAIN VIEWPORT (Video Display) ---
        let avail_size = ui.available_size();
        let target_height = (avail_size.y - 210.0).clamp(200.0, 680.0);

        if let Some(ref texture) = self.preview_texture {
            let aspect = texture.aspect_ratio();
            let mut display_w = target_height * aspect;
            let mut display_h = target_height;

            if display_w > avail_size.x {
                display_w = avail_size.x;
                display_h = display_w / aspect;
            }

            ui.vertical_centered(|ui| {
                let (rect, _response) = ui.allocate_exact_size(Vec2::new(display_w, display_h), egui::Sense::hover());

                match self.bg_mode {
                    BackgroundViewMode::Checkerboard => {
                        paint_transparency_checkerboard(ui.painter(), rect);
                    }
                    BackgroundViewMode::Dark => {
                        ui.painter().rect_filled(rect, 0, Color32::from_rgb(10, 10, 10));
                    }
                    BackgroundViewMode::Light => {
                        ui.painter().rect_filled(rect, 0, Color32::from_rgb(240, 240, 240));
                    }
                }

                ui.painter().image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                ui.painter().rect_stroke(rect, 0, Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
            });
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(target_height * 0.4);
                ui.label(RichText::new("Rendering frame...").color(colors::TEXT_MUTED));
            });
        }

        ui.add_space(6.0);

        // --- TIMELINE & TRANSPORT CONTROLS ---
        let frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(14, 8));

        frame.show(ui, |ui| {
            // Timeline Scrubber
            let old_frame = self.current_frame;
            let max_frame = count.saturating_sub(1);

            ui.horizontal(|ui| {
                let timecode = format_smpte_timecode(self.current_frame, fps);
                ui.label(RichText::new(timecode).monospace().size(16.0).color(colors::ACCENT_CYAN).strong());

                let slider = egui::Slider::new(&mut self.current_frame, 0..=max_frame)
                    .show_value(false)
                    .trailing_fill(true);
                ui.add_sized([ui.available_width() - 150.0, 20.0], slider);

                let pct = if count > 0 { (self.current_frame as f32 / count as f32) * 100.0 } else { 0.0 };
                ui.monospace(format!("{}/{} ({:.0}%)", self.current_frame + 1, count, pct));
            });

            if old_frame != self.current_frame {
                self.update_preview_frame(&ctx);
            }

            ui.add_space(4.0);

            // Transport Buttons
            ui.horizontal(|ui| {
                if ui.button(RichText::new("⏮").size(14.0)).on_hover_text("Jump to First Frame (Home)").clicked() {
                    self.current_frame = 0;
                    self.update_preview_frame(&ctx);
                }

                if ui.button(RichText::new("⏪").size(14.0)).on_hover_text("Step -10 Frames (Shift+Left)").clicked() {
                    self.current_frame = self.current_frame.saturating_sub(10);
                    self.update_preview_frame(&ctx);
                }

                if ui.button(RichText::new("◀").size(14.0)).on_hover_text("Step -1 Frame (Left)").clicked() {
                    self.current_frame = self.current_frame.saturating_sub(1);
                    self.update_preview_frame(&ctx);
                }

                // Center Play/Pause button
                let play_text = if self.is_playing { "⏸ Pause" } else { "▶ Play" };
                let play_btn = egui::Button::new(RichText::new(play_text).strong().size(15.0))
                    .fill(if self.is_playing { colors::ACCENT_AMBER } else { colors::ACCENT_BLUE })
                    .corner_radius(CornerRadius::same(6));

                if ui.add_sized([90.0, 24.0], play_btn).clicked() {
                    self.is_playing = !self.is_playing;
                    self.last_frame_time = Instant::now();
                }

                if ui.button(RichText::new("▶").size(14.0)).on_hover_text("Step +1 Frame (Right)").clicked() {
                    if self.current_frame + 1 < count {
                        self.current_frame += 1;
                        self.update_preview_frame(&ctx);
                    }
                }

                if ui.button(RichText::new("⏩").size(14.0)).on_hover_text("Step +10 Frames (Shift+Right)").clicked() {
                    self.current_frame = (self.current_frame + 10).min(count.saturating_sub(1));
                    self.update_preview_frame(&ctx);
                }

                if ui.button(RichText::new("⏭").size(14.0)).on_hover_text("Jump to Last Frame (End)").clicked() {
                    self.current_frame = count.saturating_sub(1);
                    self.update_preview_frame(&ctx);
                }

                ui.separator();

                let loop_text = if self.loop_playback { "🔁 Loop: ON" } else { "➡️ Loop: OFF" };
                if ui.selectable_label(self.loop_playback, loop_text).on_hover_text("Toggle Playback Looping (L)").clicked() {
                    self.loop_playback = !self.loop_playback;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Shortcuts: Space (Play) | ←/→ (Step) | Shift+←/→ (±10) | L (Loop)").color(colors::TEXT_FAINT).size(11.0));
                });
            });
        });

        ui.add_space(8.0);

        // --- TECHNICAL STREAM INSPECTOR CARD ---
        let inspector_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(12));

        inspector_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("📊 Stream Technical Inspection").size(14.0).color(colors::TEXT_PRIMARY));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("📋 Copy Media Info").clicked() {
                        let report = format!(
                            "HAP Stream Information\nFile: {:?}\nFormat: {} [{}]\nResolution: {}x{}\nFrame Rate: {:.2} fps\nTotal Frames: {}\nDuration: {:.2}s\nAlpha Support: {}\nSecond-Stage Compression: Snappy",
                            self.mov_path, format.name(), String::from_utf8_lossy(&format.fourcc()),
                            width, height, fps, count, duration,
                            if format.has_alpha() { "Yes" } else { "No" }
                        );
                        ui.copy_text(report);
                        self.notify("Media specs copied to clipboard!", colors::ACCENT_CYAN);
                    }
                });
            });

            ui.add_space(6.0);

            egui::Grid::new("stream_specs_grid").striped(true).spacing([24.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new("Codec Flavour:").color(colors::TEXT_MUTED));
                ui.horizontal(|ui| {
                    render_badge(ui, format.name(), colors::BG_ELEVATED, colors::ACCENT_CYAN);
                    ui.label(format!("(FourCC: {})", String::from_utf8_lossy(&format.fourcc())));
                });

                ui.label(RichText::new("File Size on Disk:").color(colors::TEXT_MUTED));
                let size_str = self
                    .mov_path
                    .as_ref()
                    .and_then(|p| fs::metadata(p).ok())
                    .map(|m| format_bytes(m.len()))
                    .unwrap_or_else(|| "Unknown".to_string());
                ui.label(size_str);
                ui.end_row();

                ui.label(RichText::new("Native Resolution:").color(colors::TEXT_MUTED));
                ui.label(format!("{} × {} ({:.2}:1)", width, height, width as f32 / height as f32));
                ui.end_row();

                ui.label(RichText::new("Duration & Frames:").color(colors::TEXT_MUTED));
                ui.label(format!("{} frames ({:.2} seconds @ {:.2} fps)", count, duration, fps));

                ui.label(RichText::new("Alpha Channel:").color(colors::TEXT_MUTED));
                ui.label(if format.has_alpha() {
                    RichText::new("Available (Transparent Matte)").color(colors::ACCENT_GREEN)
                } else {
                    RichText::new("None (Opaque RGB)").color(colors::TEXT_MUTED)
                });
                ui.end_row();

                ui.label(RichText::new("Container Architecture:").color(colors::TEXT_MUTED));
                ui.label("QuickTime MOV (Pure Rust Demuxer)");

                ui.label(RichText::new("Hardware Acceleration:").color(colors::TEXT_MUTED));
                ui.label(if self.gpu_supports_bc {
                    RichText::new("Direct VRAM Texture Upload (wgpu)").color(colors::ACCENT_GREEN)
                } else {
                    RichText::new("Multi-Core Rayon SIMD (CPU)").color(colors::ACCENT_AMBER)
                });
                ui.end_row();
            });
        });
    }
}

// ---------------------------------------------------------------------------
// TAB 2: ENCODER
// ---------------------------------------------------------------------------
impl HapStudioApp {
    fn show_encoder_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        ui.horizontal(|ui| {
            ui.heading(RichText::new("🚀 HAP Video Encoder").size(18.0).color(Color32::WHITE));
            ui.label(RichText::new("Encode PNG, JPEG, TIFF or WebP image sequences into high-performance HAP MOV files.").color(colors::TEXT_MUTED));
        });
        ui.add_space(8.0);

        // --- 1. INPUT SEQUENCE CARD ---
        let input_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(12));

        input_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("1. Source Image Sequence").size(15.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("📄 Select Single File...").clicked() {
                        if let Some(file) = rfd::FileDialog::new().pick_file() {
                            self.enc_input_path = Some(file);
                            self.scan_encoder_input(&ctx);
                        }
                    }
                    if ui.button("📁 Choose Folder...").clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            self.enc_input_path = Some(folder);
                            self.scan_encoder_input(&ctx);
                        }
                    }
                });
            });

            ui.add_space(6.0);

            if let Some(ref path) = self.enc_input_path {
                ui.horizontal(|ui| {
                    if let Some(ref thumb) = self.enc_thumbnail_texture {
                        let (rect, _response) = ui.allocate_exact_size(Vec2::new(72.0, 72.0), egui::Sense::hover());
                        paint_transparency_checkerboard(ui.painter(), rect);
                        ui.painter().image(thumb.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        ui.painter().rect_stroke(rect, 0, Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
                    }

                    ui.vertical(|ui| {
                        ui.monospace(format!("Path: {}", path.display()));
                        ui.add_space(2.0);

                        ui.horizontal(|ui| {
                            render_badge(ui, &format!("{} FRAMES", self.enc_detected_frames), colors::BG_ELEVATED, colors::ACCENT_CYAN);
                            render_badge(ui, &format!("{} × {}", self.enc_detected_w, self.enc_detected_h), colors::BG_ELEVATED, colors::TEXT_PRIMARY);

                            if let (Some(ref first), Some(ref last)) = (&self.enc_detected_first_name, &self.enc_detected_last_name) {
                                ui.label(RichText::new(format!("Range: {} ... {}", first, last)).color(colors::TEXT_FAINT));
                            }
                        });
                    });
                });
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("Drag & Drop an image folder here, or click 'Choose Folder...' to begin.").color(colors::TEXT_MUTED));
                    ui.add_space(10.0);
                });
            }
        });

        ui.add_space(8.0);

        // --- 2. PRESETS SELECTION ---
        let presets_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(12));

        presets_frame.show(ui, |ui| {
            ui.strong(RichText::new("2. Select Encoding Preset").size(15.0));
            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                let presets = [
                    EncoderPreset::HapQRecommended,
                    EncoderPreset::HapRUltra,
                    EncoderPreset::HapQAlphaTransparent,
                    EncoderPreset::Hap1Fast,
                    EncoderPreset::HapAlphaLight,
                    EncoderPreset::Custom,
                ];

                for preset in presets {
                    let is_sel = self.enc_preset == preset;
                    let bg = if is_sel { colors::BG_CARD_HOVER } else { colors::BG_ELEVATED };
                    let stroke = if is_sel { Stroke::new(1.5, colors::ACCENT_CYAN) } else { Stroke::new(1.0, colors::BORDER_SUBTLE) };

                    let btn = egui::Button::new(RichText::new(preset.name()).color(if is_sel { Color32::WHITE } else { colors::TEXT_MUTED }).strong())
                        .fill(bg)
                        .stroke(stroke)
                        .corner_radius(CornerRadius::same(6));

                    if ui.add(btn).clicked() {
                        self.apply_preset(preset);
                    }
                }
            });

            ui.add_space(4.0);
            ui.label(RichText::new(self.enc_preset.description()).color(colors::TEXT_MUTED).size(12.0));
        });

        ui.add_space(8.0);

        // --- 3. PARAMETERS & SETTINGS ---
        let settings_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(12));

        settings_frame.show(ui, |ui| {
            ui.strong(RichText::new("3. Codec & Output Parameters").size(15.0));
            ui.add_space(6.0);

            egui::Grid::new("enc_params_grid").spacing([24.0, 8.0]).show(ui, |ui| {
                ui.label(RichText::new("Target Flavour:").color(colors::TEXT_MUTED));
                egui::ComboBox::from_id_salt("flavour_combo")
                    .selected_text(self.enc_format.name())
                    .show_ui(ui, |ui| {
                        let old_f = self.enc_format;
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapY, "Hap Q - Scaled YCoCg DXT5 (High Quality)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap7, "Hap R (Hap 7) - BC7 UNORM (Ultra Quality)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapM, "Hap Q Alpha - Dual Stream Color + Alpha");
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap1, "Hap 1 - RGB DXT1 (Smallest / Fast)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap5, "Hap Alpha - RGBA DXT5 (Transparency)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapA, "Hap Alpha-Only - BC4 Single Channel");
                        if old_f != self.enc_format {
                            self.enc_preset = EncoderPreset::Custom;
                            self.update_suggested_output_filename();
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("Frame Rate (FPS):").color(colors::TEXT_MUTED));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.enc_fps).speed(0.1).range(1.0..=120.0));
                    for f in [24.0, 25.0, 29.97, 30.0, 50.0, 60.0] {
                        if ui.selectable_label((self.enc_fps - f).abs() < 0.01, format!("{:.0}", f)).clicked() {
                            self.enc_fps = f;
                        }
                    }
                });
                ui.end_row();

                ui.label(RichText::new("Threading Chunks:").color(colors::TEXT_MUTED));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("chunks_picker_box")
                        .selected_text(format!("{} Chunks", self.enc_chunks))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.enc_chunks, 1, "1 Chunk (Single Core)");
                            ui.selectable_value(&mut self.enc_chunks, 2, "2 Chunks");
                            ui.selectable_value(&mut self.enc_chunks, 4, "4 Chunks (Recommended for 1080p)");
                            ui.selectable_value(&mut self.enc_chunks, 8, "8 Chunks (Recommended for 4K)");
                            ui.selectable_value(&mut self.enc_chunks, 16, "16 Chunks");
                        });

                    ui.checkbox(&mut self.enc_snappy, "Apply Snappy Compression (Recommended)");
                });
                ui.end_row();

                ui.label(RichText::new("Destination File:").color(colors::TEXT_MUTED));
                ui.horizontal(|ui| {
                    if let Some(ref out) = self.enc_output_path {
                        ui.monospace(format!("{}", out.display()));
                    } else {
                        ui.label(RichText::new("Not set").color(colors::TEXT_FAINT));
                    }

                    if ui.button("💾 Change Destination...").clicked() {
                        if let Some(dest) = rfd::FileDialog::new()
                            .add_filter("QuickTime Movie", &["mov"])
                            .save_file()
                        {
                            self.enc_output_path = Some(dest);
                        }
                    }
                });
                ui.end_row();
            });
        });

        ui.add_space(10.0);

        // --- 4. START ACTION & LIVE TELEMETRY ---
        let can_start = self.enc_input_path.is_some()
            && self.enc_output_path.is_some()
            && self.enc_detected_frames > 0
            && self.enc_rx.is_none();

        ui.horizontal(|ui| {
            let encode_btn = egui::Button::new(RichText::new("🚀 Start Encoding").size(17.0).strong())
                .fill(colors::ACCENT_BLUE)
                .corner_radius(CornerRadius::same(6));

            if ui.add_enabled(can_start, encode_btn).clicked() {
                if let (Some(ref in_p), Some(ref out_p)) = (&self.enc_input_path, &self.enc_output_path) {
                    let cancel_flag = Arc::new(AtomicBool::new(false));
                    let (tx, rx) = crossbeam_channel::unbounded();
                    self.enc_rx = Some(rx);
                    self.enc_cancel = Some(cancel_flag.clone());
                    self.enc_start_time = Some(Instant::now());

                    let cfg = EncodeJobConfig {
                        input_dir: in_p.clone(),
                        output_file: out_p.clone(),
                        format: self.enc_format,
                        fps: self.enc_fps,
                        chunks: self.enc_chunks,
                        snappy: self.enc_snappy,
                    };

                    self.log(&format!("Started encode: {:?} -> {:?}", in_p, out_p));
                    spawn_encode_worker(cfg, cancel_flag, tx);
                }
            }

            if self.enc_rx.is_some() {
                if ui.button(RichText::new("🛑 Cancel Encode").color(colors::ACCENT_RED)).clicked() {
                    if let Some(ref cancel) = self.enc_cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            }
        });

        // Live Progress & Telemetry Card
        let mut load_into_player: Option<PathBuf> = None;

        if let Some(ref status) = self.enc_status {
            ui.add_space(8.0);
            let progress_frame = egui::Frame::canvas(ui.style())
                .fill(colors::BG_CARD)
                .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(12));

            progress_frame.show(ui, |ui| {
                match status {
                    WorkerProgress::Started { total } => {
                        ui.label(format!("Starting encoding of {} frames...", total));
                    }
                    WorkerProgress::Progress { current, total, fps, percent } => {
                        ui.horizontal(|ui| {
                            ui.strong(format!("Encoding [{} / {}] ({:.1}%)", current, total, percent));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(format!("{:.1} FPS", fps)).color(colors::ACCENT_CYAN).strong());
                            });
                        });
                        ui.add_space(4.0);
                        ui.add(egui::ProgressBar::new(*percent / 100.0).show_percentage());

                        if let Some(start) = self.enc_start_time {
                            let elapsed = start.elapsed().as_secs_f32();
                            let remaining = if *current > 0 {
                                (elapsed / *current as f32) * (total.saturating_sub(*current) as f32)
                            } else {
                                0.0
                            };
                            ui.label(RichText::new(format!("Elapsed: {:.1}s • ETA: {:.1}s", elapsed, remaining)).color(colors::TEXT_FAINT));
                        }
                    }
                    WorkerProgress::Finished { message } => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("✅").size(18.0));
                            ui.label(RichText::new(message).color(colors::ACCENT_GREEN).strong());
                        });

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if let Some(ref mov) = self.enc_last_successful_mov {
                                if ui.button(RichText::new("▶ Load into Player & Inspect").strong().color(colors::ACCENT_CYAN)).clicked() {
                                    load_into_player = Some(mov.clone());
                                }
                                if ui.button("📂 Reveal in Explorer").clicked() {
                                    reveal_in_file_manager(mov);
                                }
                            }
                        });
                    }
                    WorkerProgress::Error(err) => {
                        ui.label(RichText::new(format!("❌ Encode Error: {}", err)).color(colors::ACCENT_RED).strong());
                    }
                }
            });
        }

        if let Some(mov) = load_into_player {
            self.open_mov_file(mov, &ctx);
            self.active_tab = ActiveTab::PlayerInspector;
        }
    }
}

// ---------------------------------------------------------------------------
// TAB 3: DIAGNOSTICS & SYSTEM
// ---------------------------------------------------------------------------
impl HapStudioApp {
    fn show_diagnostics_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("🛠 System & Hardware Diagnostics").size(18.0));
        ui.label(RichText::new("Real-time GPU capability telemetry and application event logs.").color(colors::TEXT_MUTED));
        ui.add_space(8.0);

        let diag_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(12));

        diag_frame.show(ui, |ui| {
            egui::Grid::new("diag_grid").striped(true).spacing([24.0, 8.0]).show(ui, |ui| {
                ui.strong("GPU Adapter Name:");
                ui.label(&self.gpu_adapter_name);
                ui.end_row();

                ui.strong("Graphics Backend API:");
                ui.label(&self.gpu_backend_name);
                ui.end_row();

                ui.strong("Hardware BC Texture Compression:");
                ui.horizontal(|ui| {
                    if self.gpu_supports_bc {
                        render_badge(ui, "SUPPORTED", Color32::from_rgb(16, 50, 35), colors::ACCENT_GREEN);
                        ui.label("Direct VRAM upload enabled (zero-copy hardware decompression)");
                    } else {
                        render_badge(ui, "UNSUPPORTED", Color32::from_rgb(50, 45, 20), colors::ACCENT_AMBER);
                        ui.label("Using pure Rust CPU SIMD decompression");
                    }
                });
                ui.end_row();

                ui.strong("Rayon Thread Pool:");
                ui.label(format!("{} worker threads", rayon::current_num_threads()));
                ui.end_row();
            });
        });

        ui.add_space(10.0);

        // Activity Log Card
        let log_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(12));

        log_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("Application Event & Activity Log").size(15.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🗑 Clear Logs").clicked() {
                        self.system_logs.clear();
                    }
                    if ui.button("📋 Copy Logs").clicked() {
                        let text = self.system_logs.join("\n");
                        ui.copy_text(text);
                        self.notify("Logs copied to clipboard!", colors::ACCENT_CYAN);
                    }
                });
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.log_search).hint_text("🔍 Filter logs..."));
                if !self.log_search.is_empty() && ui.button("✖").clicked() {
                    self.log_search.clear();
                }
            });
            ui.add_space(6.0);

            egui::ScrollArea::vertical()
                .max_height(320.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let search_lower = self.log_search.to_lowercase();
                    for line in &self.system_logs {
                        if search_lower.is_empty() || line.to_lowercase().contains(&search_lower) {
                            ui.monospace(RichText::new(line).color(colors::TEXT_MUTED));
                        }
                    }
                });
        });
    }
}
