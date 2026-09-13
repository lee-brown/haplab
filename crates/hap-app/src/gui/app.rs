//! Main eframe / egui application interface for HapLab.

use super::theme::{
    apply_studio_theme, colors, format_bytes, format_smpte_timecode,
    paint_transparency_checkerboard, render_badge, reveal_in_file_manager,
};
use crate::benchmark::{spawn_benchmark_worker, BenchmarkProgress, BenchmarkScore};
use crate::platform::{open_windows_default_apps, register_mov_association};
use crate::worker::{
    spawn_encode_worker, spawn_export_worker, EncodeJobConfig, WorkerProgress,
};
use crossbeam_channel::Receiver;
use eframe::egui::{
    self, Color32, CornerRadius, Rect, RichText, Stroke, TextureOptions, Vec2,
};
use hap_core::{
    audit_hap_stream, decode_frame_to_rgba, extract_stream_summary, AlphaMode, ColorRange,
    DitherMode, FaultSeverity, HapFormat, QualityPreset, QtHapReader, StreamAudit, StreamSummary,
};
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
    Benchmark,
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
            Self::HapQRecommended => "Hap Q",
            Self::HapRUltra => "Hap R",
            Self::HapQAlphaTransparent => "Hap Q Alpha",
            Self::Hap1Fast => "Hap 1",
            Self::HapAlphaLight => "Hap Alpha",
            Self::Custom => "Custom",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::HapQRecommended => "Scaled YCoCg-DXT5 with Snappy compression. High color fidelity for video playback.",
            Self::HapRUltra => "BC7 UNORM texture compression. Sharp detail and integrated alpha channel.",
            Self::HapQAlphaTransparent => "Dual-stream YCoCg color + BC4 alpha matte for transparent video.",
            Self::Hap1Fast => "DXT1 RGB. Lowest CPU decode overhead.",
            Self::HapAlphaLight => "DXT5 RGBA. Single-stream transparent video.",
            Self::Custom => "Manual configuration of codec format, chunk count, and compression.",
        }
    }
}

pub struct HapLabApp {
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

    // --- Live Stream Evaluation & Fault Audit ---
    last_decode_ms: f32,
    last_packet_bytes: usize,
    playback_fps: f32,
    playback_frames_count: usize,
    playback_timer: Instant,
    stream_summary: Option<StreamSummary>,
    stream_audit: Option<StreamAudit>,
    is_auditing: bool,
    show_audit_view: bool,

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
    enc_color_range: ColorRange,
    enc_alpha_mode: AlphaMode,
    enc_dither_mode: DitherMode,
    enc_quality: QualityPreset,
    enc_start_time: Option<Instant>,
    enc_rx: Option<Receiver<WorkerProgress>>,
    enc_cancel: Option<Arc<AtomicBool>>,
    enc_status: Option<WorkerProgress>,
    enc_last_successful_mov: Option<PathBuf>,

    // --- Hardware Benchmarking State ---
    bench_scores: Vec<BenchmarkScore>,
    bench_rx: Option<Receiver<BenchmarkProgress>>,
    bench_cancel: Option<Arc<AtomicBool>>,
    bench_current_test: Option<String>,
    bench_current_step: usize,
    bench_total_steps: usize,
    bench_current_fps: f32,
    bench_is_running: bool,

    // --- System / Hardware State ---
    gpu_adapter_name: String,
    gpu_backend_name: String,
    gpu_supports_bc: bool,
    system_logs: Vec<String>,
    log_search: String,
}

impl Default for HapLabApp {
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

            last_decode_ms: 0.0,
            last_packet_bytes: 0,
            playback_fps: 0.0,
            playback_frames_count: 0,
            playback_timer: Instant::now(),
            stream_summary: None,
            stream_audit: None,
            is_auditing: false,
            show_audit_view: false,

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
            enc_color_range: ColorRange::Full,
            enc_alpha_mode: AlphaMode::Straight,
            enc_dither_mode: DitherMode::None,
            enc_quality: QualityPreset::Production,
            enc_start_time: None,
            enc_rx: None,
            enc_cancel: None,
            enc_status: None,
            enc_last_successful_mov: None,

            bench_scores: Vec::new(),
            bench_rx: None,
            bench_cancel: None,
            bench_current_test: None,
            bench_current_step: 0,
            bench_total_steps: 0,
            bench_current_fps: 0.0,
            bench_is_running: false,

            gpu_adapter_name: adapter_name,
            gpu_backend_name: backend_name,
            gpu_supports_bc: supports_bc,
            system_logs: Vec::new(),
            log_search: String::new(),
        };

        app.log("HapLab initialized.");
        app.log(&format!(
            "Graphics Device: {} [{}]",
            app.gpu_adapter_name, app.gpu_backend_name
        ));
        app.log(&format!(
            "Hardware BC Texture Uploads: {}",
            if app.gpu_supports_bc {
                "Supported"
            } else {
                "Software Fallback"
            }
        ));

        app
    }
}

impl HapLabApp {
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
                self.enc_color_range = ColorRange::Full;
                self.enc_alpha_mode = AlphaMode::Straight;
                self.enc_dither_mode = DitherMode::None;
                self.enc_quality = QualityPreset::Production;
            }
            EncoderPreset::HapRUltra => {
                self.enc_format = HapFormat::Hap7;
                self.enc_chunks = 8;
                self.enc_snappy = true;
                self.enc_color_range = ColorRange::Full;
                self.enc_alpha_mode = AlphaMode::Straight;
                self.enc_dither_mode = DitherMode::None;
                self.enc_quality = QualityPreset::Production;
            }
            EncoderPreset::HapQAlphaTransparent => {
                self.enc_format = HapFormat::HapM;
                self.enc_chunks = 4;
                self.enc_snappy = true;
                self.enc_color_range = ColorRange::Full;
                self.enc_alpha_mode = AlphaMode::Straight;
                self.enc_dither_mode = DitherMode::None;
                self.enc_quality = QualityPreset::Production;
            }
            EncoderPreset::Hap1Fast => {
                self.enc_format = HapFormat::Hap1;
                self.enc_chunks = 4;
                self.enc_snappy = false;
                self.enc_color_range = ColorRange::Full;
                self.enc_alpha_mode = AlphaMode::Discard;
                self.enc_dither_mode = DitherMode::None;
                self.enc_quality = QualityPreset::Draft;
            }
            EncoderPreset::HapAlphaLight => {
                self.enc_format = HapFormat::Hap5;
                self.enc_chunks = 4;
                self.enc_snappy = true;
                self.enc_color_range = ColorRange::Full;
                self.enc_alpha_mode = AlphaMode::Straight;
                self.enc_dither_mode = DitherMode::None;
                self.enc_quality = QualityPreset::Production;
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
            Ok(mut reader) => {
                let filename = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.log(&format!(
                    "Opened file: {} ({}x{}, {:.2} fps, {} frames, {})",
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
                let file_size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                self.stream_summary = extract_stream_summary(&mut reader, file_size).ok();
                self.stream_audit = None;
                self.show_audit_view = false;

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
                self.last_packet_bytes = packet.len();
                let decode_start = Instant::now();
                if let Ok(mut rgba) = decode_frame_to_rgba(&packet, width, height) {
                    self.last_decode_ms = decode_start.elapsed().as_secs_f32() * 1000.0;
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

impl eframe::App for HapLabApp {
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

        // 2. Keyboard Shortcuts (Player)
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
                            format!("Loop: {}", if self.loop_playback { "On" } else { "Off" }),
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

                    self.playback_frames_count += 1;
                    let elapsed = self.playback_timer.elapsed().as_secs_f32();
                    if elapsed >= 0.5 {
                        self.playback_fps = (self.playback_frames_count as f32) / elapsed;
                        self.playback_frames_count = 0;
                        self.playback_timer = Instant::now();
                    }
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

        let bench_msgs: Vec<_> = if let Some(ref rx) = self.bench_rx {
            let mut msgs = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                msgs.push(msg);
            }
            msgs
        } else {
            Vec::new()
        };

        if !bench_msgs.is_empty() {
            for msg in bench_msgs {
                match msg {
                    BenchmarkProgress::Started { test_name } => {
                        self.bench_current_test = Some(test_name);
                        self.bench_current_step = 0;
                        self.bench_total_steps = 1;
                        self.bench_current_fps = 0.0;
                    }
                    BenchmarkProgress::StepProgress {
                        test_name,
                        current,
                        total,
                        current_fps,
                    } => {
                        self.bench_current_test = Some(test_name);
                        self.bench_current_step = current;
                        self.bench_total_steps = total;
                        self.bench_current_fps = current_fps;
                    }
                    BenchmarkProgress::TestCompleted(score) => {
                        self.log(&format!(
                            "Benchmark [{}] complete: {:.1} FPS ({:.2} ms), {:.2} GB/s",
                            score.test_name, score.fps, score.frame_time_ms, score.bandwidth_gbps
                        ));
                        self.bench_scores.retain(|s| s.test_name != score.test_name);
                        self.bench_scores.push(score);
                    }
                    BenchmarkProgress::AllFinished { summary } => {
                        self.bench_scores = summary;
                        self.bench_is_running = false;
                        self.bench_current_test = None;
                        self.notify("Benchmark suite completed successfully!", colors::ACCENT_GREEN);
                        self.bench_rx = None;
                        break;
                    }
                    BenchmarkProgress::Error(err) => {
                        self.bench_is_running = false;
                        self.bench_current_test = None;
                        self.log(&format!("Benchmark error: {}", err));
                        self.notify(format!("Benchmark failed: {}", err), colors::ACCENT_RED);
                        self.bench_rx = None;
                        break;
                    }
                }
            }
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let is_dragging = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());

        let outer_frame = egui::Frame::new().inner_margin(egui::Margin::symmetric(20, 14));

        outer_frame.show(ui, |ui| {
            ui.vertical(|ui| {
                // --- TOP TITLE BAR ---
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.heading(
                        RichText::new("HapLab")
                            .size(20.0)
                            .color(Color32::WHITE)
                            .strong(),
                    );
                    ui.label(RichText::new("v0.1.0").color(colors::TEXT_FAINT));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.gpu_supports_bc {
                            render_badge(ui, "GPU Acceleration Active", Color32::from_rgb(16, 40, 30), colors::ACCENT_GREEN);
                        } else {
                            render_badge(ui, "CPU Mode", Color32::from_rgb(38, 35, 25), colors::TEXT_MUTED);
                        }
                    });
                });

                ui.add_space(8.0);

                // --- NAVIGATION TABS ---
                ui.horizontal(|ui| {
                    let tab_btn = |ui: &mut egui::Ui, active: bool, title: &str, badge_count: Option<usize>| {
                        let bg = if active { colors::BG_CARD_HOVER } else { Color32::TRANSPARENT };
                        let border = if active { Stroke::new(1.5, colors::ACCENT_CYAN) } else { Stroke::NONE };
                        let fg = if active { Color32::WHITE } else { colors::TEXT_MUTED };

                        let btn = egui::Button::new(RichText::new(title).size(13.5).color(fg).strong())
                            .min_size(Vec2::new(140.0, 36.0))
                            .fill(bg)
                            .stroke(border)
                            .corner_radius(CornerRadius::same(7));
                        let resp = ui.add(btn);
                        if let Some(cnt) = badge_count {
                            if cnt > 0 {
                                render_badge(ui, &format!("{}", cnt), Color32::from_rgb(30, 45, 65), colors::ACCENT_CYAN);
                            }
                        }
                        resp.clicked()
                    };

                    let mov_loaded = self.reader.is_some();
                    if tab_btn(ui, self.active_tab == ActiveTab::PlayerInspector, "Player & Inspector", if mov_loaded { Some(1) } else { None }) {
                        self.active_tab = ActiveTab::PlayerInspector;
                    }

                    let frames_detected = self.enc_detected_frames;
                    if tab_btn(ui, self.active_tab == ActiveTab::Encoder, "Encoder", if frames_detected > 0 { Some(frames_detected) } else { None }) {
                        self.active_tab = ActiveTab::Encoder;
                    }

                    let bench_badge = if self.bench_is_running {
                        Some(1)
                    } else if !self.bench_scores.is_empty() {
                        Some(self.bench_scores.len())
                    } else {
                        None
                    };
                    if tab_btn(ui, self.active_tab == ActiveTab::Benchmark, "Benchmark", bench_badge) {
                        self.active_tab = ActiveTab::Benchmark;
                    }

                    if tab_btn(ui, self.active_tab == ActiveTab::Diagnostics, "Diagnostics", None) {
                        self.active_tab = ActiveTab::Diagnostics;
                    }
                });

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                // Drag & Drop Hover Border
                if is_dragging {
                    ui.painter().rect_stroke(
                        ui.max_rect(),
                        CornerRadius::same(8),
                        Stroke::new(2.0, colors::ACCENT_CYAN),
                        egui::StrokeKind::Inside,
                    );
                }

                // --- TAB CONTENT ---
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(6.0);
                        match self.active_tab {
                            ActiveTab::PlayerInspector => self.show_player_tab(ui),
                            ActiveTab::Encoder => self.show_encoder_tab(ui),
                            ActiveTab::Benchmark => self.show_benchmark_tab(ui),
                            ActiveTab::Diagnostics => self.show_diagnostics_tab(ui),
                        }
                        ui.add_space(32.0);
                    });

                // --- FOOTER STATUS ---
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if let Some((ref msg, time, color)) = self.toast {
                            if time.elapsed().as_secs_f32() < 4.0 {
                                ui.label(RichText::new(msg).color(color).strong());
                            } else {
                                self.toast = None;
                            }
                        } else if let Some(ref path) = self.mov_path {
                            ui.label(RichText::new(format!("File: {}", path.display())).color(colors::TEXT_MUTED));
                        } else {
                            ui.label(RichText::new("Ready. Drop a MOV file to inspect, or an image folder to encode.").color(colors::TEXT_FAINT));
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format!("Device: {}", self.gpu_adapter_name)).color(colors::TEXT_FAINT).size(11.0));
                        });
                    });
                });
            });
        });
    }
}

// ---------------------------------------------------------------------------
// TAB 1: PLAYER & INSPECTOR
// ---------------------------------------------------------------------------
impl HapLabApp {
    fn show_player_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // Drop Zone when no file is loaded
        if self.reader.is_none() {
            ui.add_space(24.0);
            let frame = egui::Frame::canvas(ui.style())
                .fill(colors::BG_CARD)
                .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(36, 32));

            frame.show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.heading(RichText::new("Drop a HAP QuickTime file (.mov) here").size(19.0).color(Color32::WHITE).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new("Playback and inspect Hap 1, Hap Alpha, Hap Q, Hap Q Alpha, Hap R, and Hap HDR files.").color(colors::TEXT_MUTED));
                    ui.add_space(18.0);

                    ui.horizontal(|ui| {
                        ui.add_space(ui.available_width() * 0.5 - 130.0);
                        if ui.add(egui::Button::new(RichText::new("Open File...").size(14.0)).min_size(Vec2::new(120.0, 36.0))).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("QuickTime HAP Video", &["mov", "mp4"])
                                .pick_file()
                            {
                                self.open_mov_file(path, &ctx);
                            }
                        }

                        if ui.add(egui::Button::new(RichText::new("Switch to Encoder").size(14.0)).min_size(Vec2::new(140.0, 36.0))).clicked() {
                            self.active_tab = ActiveTab::Encoder;
                        }
                    });
                    ui.add_space(8.0);
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
            if ui.add(egui::Button::new("Open File...").min_size(Vec2::new(96.0, 32.0))).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("QuickTime HAP Video", &["mov", "mp4"])
                    .pick_file()
                {
                    self.open_mov_file(path, &ctx);
                }
            }

            if let Some(ref path) = self.mov_path {
                if ui.add(egui::Button::new("Show in Explorer").min_size(Vec2::new(120.0, 32.0))).clicked() {
                    reveal_in_file_manager(path);
                }
            }

            let export_text = if self.show_export_panel { "Hide Exporter" } else { "Export Frames..." };
            if ui.add(egui::Button::new(export_text).min_size(Vec2::new(120.0, 32.0))).clicked() {
                self.show_export_panel = !self.show_export_panel;
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            // Channel Mode Selector
            ui.label(RichText::new("Channels:").color(colors::TEXT_MUTED));
            let old_mode = self.channel_mode;
            ui.selectable_value(&mut self.channel_mode, ChannelViewMode::Rgba, "RGBA");
            ui.selectable_value(&mut self.channel_mode, ChannelViewMode::AlphaMatte, "Alpha (Matte)");
            ui.selectable_value(&mut self.channel_mode, ChannelViewMode::RgbOpaque, "RGB");
            if old_mode != self.channel_mode {
                if let Some(ref mut raw) = self.raw_frame_cache {
                    let mut copy = raw.clone();
                    self.rebuild_texture_from_cache(&ctx, width as usize, height as usize, &mut copy);
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            // Background Mode Selector
            ui.label(RichText::new("Background:").color(colors::TEXT_MUTED));
            ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Checkerboard, "Checkerboard");
            ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Dark, "Dark");
            ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Light, "Light");
        });

        // --- EXPORT PANEL (Collapsible) ---
        if self.show_export_panel {
            ui.add_space(8.0);
            let frame = egui::Frame::canvas(ui.style())
                .fill(colors::BG_CARD)
                .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(16));

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

                    if ui.add(egui::Button::new(RichText::new("Choose Destination Folder & Export").strong()).min_size(Vec2::new(220.0, 32.0))).clicked() {
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
                    ui.add_space(8.0);
                    match status {
                        WorkerProgress::Started { total } => {
                            ui.label(format!("Exporting {} frames...", total));
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

        ui.add_space(8.0);

        // --- LIVE STATS HUD BAR ---
        let hud_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(14, 8));

        hud_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("TELEMETRY:").size(11.0).color(colors::TEXT_FAINT).strong());
                ui.separator();

                // Rendered Playback FPS
                ui.label(RichText::new("Playback FPS:").size(12.0).color(colors::TEXT_MUTED));
                if self.is_playing {
                    let fps_color = if self.playback_fps >= (fps - 1.0) {
                        colors::ACCENT_GREEN
                    } else if self.playback_fps >= (fps * 0.75) {
                        colors::ACCENT_AMBER
                    } else {
                        colors::ACCENT_RED
                    };
                    ui.label(RichText::new(format!("{:.1} FPS", self.playback_fps)).size(12.5).color(fps_color).strong());
                } else {
                    ui.label(RichText::new("Paused").size(12.0).color(colors::TEXT_FAINT));
                }

                ui.separator();

                // Frame Decode Latency
                ui.label(RichText::new("Frame Latency:").size(12.0).color(colors::TEXT_MUTED));
                let lat_color = if self.last_decode_ms < 5.0 {
                    colors::ACCENT_GREEN
                } else if self.last_decode_ms < 16.6 {
                    colors::ACCENT_CYAN
                } else {
                    colors::ACCENT_AMBER
                };
                ui.label(RichText::new(format!("{:.2} ms", self.last_decode_ms)).size(12.5).color(lat_color).strong());

                ui.separator();

                // Packet Size
                ui.label(RichText::new("Packet Size:").size(12.0).color(colors::TEXT_MUTED));
                ui.label(RichText::new(format_bytes(self.last_packet_bytes as u64)).size(12.0).color(colors::TEXT_PRIMARY));

                ui.separator();

                // Compression Savings vs uncompressed RGBA
                let uncompressed = (width as usize) * (height as usize) * 4;
                if uncompressed > 0 && self.last_packet_bytes > 0 {
                    let savings = (1.0 - (self.last_packet_bytes as f32 / uncompressed as f32)) * 100.0;
                    ui.label(RichText::new("Savings:").size(12.0).color(colors::TEXT_MUTED));
                    ui.label(RichText::new(format!("{:.1}%", savings)).size(12.0).color(colors::ACCENT_GREEN).strong());
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(ref summary) = self.stream_summary {
                        render_badge(ui, &format!("{} Chunks", summary.chunk_count), colors::BG_ELEVATED, colors::ACCENT_CYAN);
                        if summary.uses_snappy {
                            render_badge(ui, "Snappy", colors::BG_ELEVATED, colors::ACCENT_BLUE);
                        }
                    }
                });
            });
        });

        ui.add_space(8.0);

        // --- MAIN VIEWPORT (Video Display) ---
        let avail_size = ui.available_size();
        let target_height = (avail_size.y - 230.0).clamp(200.0, 680.0);

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

        ui.add_space(10.0);

        // --- TIMELINE & TRANSPORT CONTROLS ---
        let frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(20, 14));

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
                ui.add_sized([ui.available_width() - 170.0, 24.0], slider);

                let pct = if count > 0 { (self.current_frame as f32 / count as f32) * 100.0 } else { 0.0 };
                ui.monospace(format!("{}/{} ({:.0}%)", self.current_frame + 1, count, pct));
            });

            if old_frame != self.current_frame {
                self.update_preview_frame(&ctx);
            }

            ui.add_space(8.0);

            // Transport Buttons
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("|<").min_size(Vec2::new(38.0, 32.0))).on_hover_text("Jump to Start (Home)").clicked() {
                    self.current_frame = 0;
                    self.update_preview_frame(&ctx);
                }

                if ui.add(egui::Button::new("-10").min_size(Vec2::new(44.0, 32.0))).on_hover_text("Step -10 Frames (Shift+Left)").clicked() {
                    self.current_frame = self.current_frame.saturating_sub(10);
                    self.update_preview_frame(&ctx);
                }

                if ui.add(egui::Button::new("<").min_size(Vec2::new(38.0, 32.0))).on_hover_text("Step -1 Frame (Left)").clicked() {
                    self.current_frame = self.current_frame.saturating_sub(1);
                    self.update_preview_frame(&ctx);
                }

                // Center Play/Pause button
                let play_text = if self.is_playing { "Pause" } else { "Play" };
                let play_btn = egui::Button::new(RichText::new(play_text).strong().size(14.0))
                    .fill(if self.is_playing { colors::ACCENT_AMBER } else { colors::ACCENT_BLUE })
                    .corner_radius(CornerRadius::same(6));

                if ui.add_sized([90.0, 34.0], play_btn).clicked() {
                    self.is_playing = !self.is_playing;
                    self.last_frame_time = Instant::now();
                }

                if ui.add(egui::Button::new(">").min_size(Vec2::new(38.0, 32.0))).on_hover_text("Step +1 Frame (Right)").clicked() {
                    if self.current_frame + 1 < count {
                        self.current_frame += 1;
                        self.update_preview_frame(&ctx);
                    }
                }

                if ui.add(egui::Button::new("+10").min_size(Vec2::new(44.0, 32.0))).on_hover_text("Step +10 Frames (Shift+Right)").clicked() {
                    self.current_frame = (self.current_frame + 10).min(count.saturating_sub(1));
                    self.update_preview_frame(&ctx);
                }

                if ui.add(egui::Button::new(">|").min_size(Vec2::new(38.0, 32.0))).on_hover_text("Jump to End (End)").clicked() {
                    self.current_frame = count.saturating_sub(1);
                    self.update_preview_frame(&ctx);
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                let loop_text = if self.loop_playback { "Loop: On" } else { "Loop: Off" };
                if ui.add(egui::Button::new(loop_text).min_size(Vec2::new(85.0, 32.0))).on_hover_text("Toggle Playback Looping (L)").clicked() {
                    self.loop_playback = !self.loop_playback;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Space: Play/Pause | Left/Right: Step | L: Loop").color(colors::TEXT_FAINT).size(11.5));
                });
            });
        });

        ui.add_space(14.0);

        // --- TECHNICAL STREAM INSPECTOR CARD ---
        let inspector_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        inspector_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Stream Information & Technical Metrics").size(15.0).color(colors::TEXT_PRIMARY));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Copy Summary").min_size(Vec2::new(105.0, 30.0))).clicked() {
                        let report = if let Some(ref s) = self.stream_summary {
                            format!(
                                "HAP Stream Information\nFile: {:?}\nFormat: {} [{}]\nResolution: {}x{}\nFrame Rate: {:.2} fps\nTotal Frames: {}\nDuration: {:.2}s\nBitrate: {:.2} Mbps\nAvg Frame: {}\nCompression Ratio: {:.2}:1 ({:.1}% saved)\nChunks: {}\nSnappy: {}\nTexture Type: {}",
                                self.mov_path, format.name(), String::from_utf8_lossy(&format.fourcc()),
                                width, height, fps, count, duration,
                                s.avg_bitrate_mbps, format_bytes(s.avg_frame_bytes as u64),
                                s.avg_compression_ratio, s.savings_percent,
                                s.chunk_count, s.uses_snappy, s.texture_type_name
                            )
                        } else {
                            format!(
                                "HAP Stream Information\nFile: {:?}\nFormat: {} [{}]\nResolution: {}x{}\nFrame Rate: {:.2} fps\nTotal Frames: {}\nDuration: {:.2}s\nAlpha: {}\nCompression: Snappy",
                                self.mov_path, format.name(), String::from_utf8_lossy(&format.fourcc()),
                                width, height, fps, count, duration,
                                if format.has_alpha() { "Yes" } else { "No" }
                            )
                        };
                        ui.copy_text(report);
                        self.notify("Stream information copied to clipboard", colors::ACCENT_CYAN);
                    }

                    let audit_btn_text = if self.is_auditing {
                        "Auditing..."
                    } else if self.stream_audit.is_some() {
                        "Re-run Health Audit"
                    } else {
                        "Audit Stream Health"
                    };
                    if ui.add(egui::Button::new(RichText::new(audit_btn_text).color(colors::ACCENT_CYAN).strong()).min_size(Vec2::new(145.0, 30.0))).clicked() {
                        if let Some(ref mut reader) = self.reader {
                            self.is_auditing = true;
                            let audit = audit_hap_stream(reader, 120);
                            self.stream_audit = Some(audit);
                            self.show_audit_view = true;
                            self.is_auditing = false;
                            self.log("Stream health audit completed.");
                            self.notify("Stream compliance audit complete.", colors::ACCENT_GREEN);
                        }
                    }
                });
            });

            ui.add_space(10.0);

            egui::Grid::new("stream_specs_grid").striped(true).spacing([32.0, 10.0]).show(ui, |ui| {
                ui.label(RichText::new("Codec Format:").color(colors::TEXT_MUTED));
                ui.horizontal(|ui| {
                    render_badge(ui, format.name(), colors::BG_ELEVATED, colors::ACCENT_CYAN);
                    ui.label(format!("(FourCC: {})", String::from_utf8_lossy(&format.fourcc())));
                });

                ui.label(RichText::new("File Size:").color(colors::TEXT_MUTED));
                let size_str = self
                    .mov_path
                    .as_ref()
                    .and_then(|p| fs::metadata(p).ok())
                    .map(|m| format_bytes(m.len()))
                    .unwrap_or_else(|| "Unknown".to_string());
                ui.label(size_str);
                ui.end_row();

                ui.label(RichText::new("Resolution:").color(colors::TEXT_MUTED));
                ui.label(format!("{} × {} ({:.2}:1)", width, height, width as f32 / height as f32));

                ui.label(RichText::new("Bitrate:").color(colors::TEXT_MUTED));
                if let Some(ref s) = self.stream_summary {
                    ui.label(format!("{:.2} Mbps", s.avg_bitrate_mbps));
                } else {
                    ui.label("Calculating...");
                }
                ui.end_row();

                ui.label(RichText::new("Duration:").color(colors::TEXT_MUTED));
                ui.label(format!("{} frames ({:.2}s @ {:.2} fps)", count, duration, fps));

                ui.label(RichText::new("Frame Packet Size:").color(colors::TEXT_MUTED));
                if let Some(ref s) = self.stream_summary {
                    ui.label(format!("{} avg (min: {}, max: {})", format_bytes(s.avg_frame_bytes as u64), format_bytes(s.min_frame_bytes as u64), format_bytes(s.max_frame_bytes as u64)));
                } else {
                    ui.label(format_bytes(self.last_packet_bytes as u64));
                }
                ui.end_row();

                ui.label(RichText::new("Alpha Channel:").color(colors::TEXT_MUTED));
                ui.label(if format.has_alpha() {
                    RichText::new("Present").color(colors::ACCENT_GREEN)
                } else {
                    RichText::new("None (Opaque)").color(colors::TEXT_MUTED)
                });

                ui.label(RichText::new("Compression Savings:").color(colors::TEXT_MUTED));
                if let Some(ref s) = self.stream_summary {
                    ui.label(RichText::new(format!("{:.1}% saved ({:.1}:1 ratio)", s.savings_percent, s.avg_compression_ratio)).color(colors::ACCENT_GREEN));
                } else {
                    ui.label("—");
                }
                ui.end_row();

                ui.label(RichText::new("GPU Texture Target:").color(colors::TEXT_MUTED));
                if let Some(ref s) = self.stream_summary {
                    ui.label(s.texture_type_name);
                } else {
                    ui.label("BC Compressed Texture");
                }

                ui.label(RichText::new("Parallelism:").color(colors::TEXT_MUTED));
                if let Some(ref s) = self.stream_summary {
                    ui.label(format!("{} Chunks (Snappy: {})", s.chunk_count, if s.uses_snappy { "Enabled" } else { "None" }));
                } else {
                    ui.label("Standard");
                }
                ui.end_row();

                ui.label(RichText::new("Container:").color(colors::TEXT_MUTED));
                ui.label("QuickTime MOV");

                ui.label(RichText::new("Decoding Mode:").color(colors::TEXT_MUTED));
                ui.label(if self.gpu_supports_bc {
                    RichText::new("Direct GPU Texture Upload").color(colors::ACCENT_GREEN)
                } else {
                    RichText::new("Software Decode (Rayon)").color(colors::ACCENT_AMBER)
                });
                ui.end_row();
            });
        });

        // --- STREAM FAULT AUDIT REPORT CARD ---
        if self.show_audit_view {
            if let Some(ref audit) = self.stream_audit {
                ui.add_space(12.0);
                let (border_color, status_title, status_bg, status_fg) = if audit.has_critical_errors() {
                    (colors::ACCENT_RED, "CRITICAL FAULTS DETECTED", Color32::from_rgb(45, 20, 20), colors::ACCENT_RED)
                } else if audit.has_warnings() {
                    (colors::ACCENT_AMBER, "STREAM WARNINGS / ADVISORIES", Color32::from_rgb(45, 38, 15), colors::ACCENT_AMBER)
                } else {
                    (colors::ACCENT_GREEN, "STREAM AUDIT PASSED (100% SPECIFICATION COMPLIANT)", Color32::from_rgb(16, 40, 25), colors::ACCENT_GREEN)
                };

                let audit_frame = egui::Frame::canvas(ui.style())
                    .fill(colors::BG_CARD)
                    .stroke(Stroke::new(1.2, border_color))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(18));

                audit_frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        render_badge(ui, status_title, status_bg, status_fg);
                        ui.label(RichText::new(format!("({} frames sampled)", audit.frames_scanned)).color(colors::TEXT_FAINT));

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new("Dismiss").min_size(Vec2::new(75.0, 26.0))).clicked() {
                                self.show_audit_view = false;
                            }
                        });
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    for check in &audit.checks {
                        ui.horizontal(|ui| {
                            match check.severity {
                                FaultSeverity::Passed => {
                                    render_badge(ui, "PASS", Color32::from_rgb(16, 40, 25), colors::ACCENT_GREEN);
                                }
                                FaultSeverity::Warning => {
                                    render_badge(ui, "WARN", Color32::from_rgb(45, 38, 15), colors::ACCENT_AMBER);
                                }
                                FaultSeverity::Critical => {
                                    render_badge(ui, "FAIL", Color32::from_rgb(45, 20, 20), colors::ACCENT_RED);
                                }
                            }

                            ui.strong(check.check_name);
                            ui.label(&check.message);
                        });

                        if let Some(ref rec) = check.recommendation {
                            ui.horizontal(|ui| {
                                ui.add_space(60.0);
                                ui.label(RichText::new(format!("↳ Recommendation: {}", rec)).color(colors::ACCENT_CYAN));
                            });
                        }
                        ui.add_space(4.0);
                    }
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TAB 2: ENCODER
// ---------------------------------------------------------------------------
impl HapLabApp {
    fn show_encoder_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        ui.horizontal(|ui| {
            ui.heading(RichText::new("Video Encoder").size(19.0).color(Color32::WHITE));
            ui.label(RichText::new("Encode an image sequence to a QuickTime HAP MOV file.").color(colors::TEXT_MUTED));
        });
        ui.add_space(10.0);

        // --- SOURCE SEQUENCE CARD ---
        let input_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        input_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("Source Sequence").size(15.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Choose Folder...").min_size(Vec2::new(125.0, 34.0))).clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            self.enc_input_path = Some(folder);
                            self.scan_encoder_input(&ctx);
                        }
                    }
                    if ui.add(egui::Button::new("Select File...").min_size(Vec2::new(110.0, 34.0))).clicked() {
                        if let Some(file) = rfd::FileDialog::new().pick_file() {
                            self.enc_input_path = Some(file);
                            self.scan_encoder_input(&ctx);
                        }
                    }
                });
            });

            ui.add_space(10.0);

            if let Some(ref path) = self.enc_input_path {
                ui.horizontal(|ui| {
                    if let Some(ref thumb) = self.enc_thumbnail_texture {
                        let (rect, _response) = ui.allocate_exact_size(Vec2::new(88.0, 88.0), egui::Sense::hover());
                        paint_transparency_checkerboard(ui.painter(), rect);
                        ui.painter().image(thumb.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        ui.painter().rect_stroke(rect, 0, Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
                    }

                    ui.add_space(8.0);

                    ui.vertical(|ui| {
                        ui.monospace(format!("Path: {}", path.display()));
                        ui.add_space(6.0);

                        ui.horizontal(|ui| {
                            render_badge(ui, &format!("{} frames", self.enc_detected_frames), colors::BG_ELEVATED, colors::ACCENT_CYAN);
                            render_badge(ui, &format!("{}x{}", self.enc_detected_w, self.enc_detected_h), colors::BG_ELEVATED, colors::TEXT_PRIMARY);

                            if let (Some(ref first), Some(ref last)) = (&self.enc_detected_first_name, &self.enc_detected_last_name) {
                                ui.label(RichText::new(format!("Range: {} ... {}", first, last)).color(colors::TEXT_FAINT));
                            }
                        });
                    });
                });
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new("Drop an image folder here, or click 'Choose Folder...'").color(colors::TEXT_MUTED));
                    ui.add_space(12.0);
                });
            }
        });

        ui.add_space(14.0);

        // --- PRESETS ---
        let presets_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        presets_frame.show(ui, |ui| {
            ui.strong(RichText::new("Presets").size(15.0));
            ui.add_space(8.0);

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

                    let btn = egui::Button::new(RichText::new(preset.name()).size(13.5).color(if is_sel { Color32::WHITE } else { colors::TEXT_MUTED }).strong())
                        .min_size(Vec2::new(110.0, 36.0))
                        .fill(bg)
                        .stroke(stroke)
                        .corner_radius(CornerRadius::same(6));

                    if ui.add(btn).clicked() {
                        self.apply_preset(preset);
                    }
                }
            });

            ui.add_space(6.0);
            ui.label(RichText::new(self.enc_preset.description()).color(colors::TEXT_MUTED).size(12.5));
        });

        ui.add_space(14.0);

        // --- CODEC SETTINGS ---
        let settings_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        settings_frame.show(ui, |ui| {
            ui.strong(RichText::new("Codec Settings").size(15.0));
            ui.add_space(10.0);

            egui::Grid::new("enc_params_grid").spacing([32.0, 14.0]).show(ui, |ui| {
                ui.label(RichText::new("Format:").color(colors::TEXT_MUTED));
                egui::ComboBox::from_id_salt("flavour_combo")
                    .selected_text(self.enc_format.name())
                    .show_ui(ui, |ui| {
                        let old_f = self.enc_format;
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapY, "Hap Q (Scaled YCoCg)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap7, "Hap R (BC7)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapM, "Hap Q Alpha (Color + Alpha)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap1, "Hap 1 (DXT1)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap5, "Hap Alpha (DXT5)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapA, "Hap Alpha-Only (BC4)");
                        if old_f != self.enc_format {
                            self.enc_preset = EncoderPreset::Custom;
                            self.update_suggested_output_filename();
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("Frame Rate:").color(colors::TEXT_MUTED));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.enc_fps).speed(0.1).range(1.0..=120.0));
                    for f in [24.0, 25.0, 29.97, 30.0, 50.0, 60.0] {
                        if ui.add(egui::Button::new(format!("{:.0}", f)).min_size(Vec2::new(38.0, 30.0))).clicked() {
                            self.enc_fps = f;
                        }
                    }
                });
                ui.end_row();

                ui.label(RichText::new("Chunks:").color(colors::TEXT_MUTED));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("chunks_picker_box")
                        .selected_text(format!("{} Chunks", self.enc_chunks))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.enc_chunks, 1, "1 Chunk");
                            ui.selectable_value(&mut self.enc_chunks, 2, "2 Chunks");
                            ui.selectable_value(&mut self.enc_chunks, 4, "4 Chunks");
                            ui.selectable_value(&mut self.enc_chunks, 8, "8 Chunks");
                            ui.selectable_value(&mut self.enc_chunks, 16, "16 Chunks");
                        });

                    ui.add_space(8.0);
                    ui.checkbox(&mut self.enc_snappy, "Snappy Compression");
                });
                ui.end_row();

                ui.label(RichText::new("Output Destination:").color(colors::TEXT_MUTED));
                ui.horizontal(|ui| {
                    if let Some(ref out) = self.enc_output_path {
                        ui.monospace(format!("{}", out.display()));
                    } else {
                        ui.label(RichText::new("Not set").color(colors::TEXT_FAINT));
                    }

                    if ui.add(egui::Button::new("Change...").min_size(Vec2::new(96.0, 30.0))).clicked() {
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

        ui.add_space(14.0);

        // --- COLOR & PROCESSING SETTINGS CARD ---
        let color_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        color_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("Color & Processing Settings").size(15.0));
                ui.label(RichText::new("Professional levels, alpha transparency, and dithering controls.").color(colors::TEXT_MUTED).size(12.0));
            });
            ui.add_space(10.0);

            egui::Grid::new("enc_color_grid").spacing([32.0, 14.0]).show(ui, |ui| {
                ui.label(RichText::new("Color Levels:").color(colors::TEXT_MUTED));
                egui::ComboBox::from_id_salt("color_range_combo")
                    .selected_text(match self.enc_color_range {
                        ColorRange::Full => "Full Range (0–255 PC / Graphics)",
                        ColorRange::Limited => "Limited Range (16–235 Studio Video -> Expand)",
                    })
                    .show_ui(ui, |ui| {
                        if ui.selectable_value(&mut self.enc_color_range, ColorRange::Full, "Full Range (0–255 PC / Graphics / Unreal)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                        if ui.selectable_value(&mut self.enc_color_range, ColorRange::Limited, "Limited Range (16–235 Studio Video -> Expand to 0–255)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("Alpha Channel:").color(colors::TEXT_MUTED));
                egui::ComboBox::from_id_salt("alpha_mode_combo")
                    .selected_text(match self.enc_alpha_mode {
                        AlphaMode::Straight => "Straight (Unassociated, As-Is)",
                        AlphaMode::Premultiply => "Premultiply (RGB × Alpha, Shader-ready)",
                        AlphaMode::Demultiply => "Demultiply (Strip black fringes)",
                        AlphaMode::Discard => "Discard Alpha (Force Opaque)",
                    })
                    .show_ui(ui, |ui| {
                        if ui.selectable_value(&mut self.enc_alpha_mode, AlphaMode::Straight, "Straight (Unassociated, Keep As-Is)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                        if ui.selectable_value(&mut self.enc_alpha_mode, AlphaMode::Premultiply, "Premultiply (RGB × Alpha, Fixes live blending halos)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                        if ui.selectable_value(&mut self.enc_alpha_mode, AlphaMode::Demultiply, "Demultiply (Un-premultiply, Removes existing dark halos)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                        if ui.selectable_value(&mut self.enc_alpha_mode, AlphaMode::Discard, "Discard Alpha (Force completely opaque)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("Chroma Dither:").color(colors::TEXT_MUTED));
                egui::ComboBox::from_id_salt("dither_mode_combo")
                    .selected_text(match self.enc_dither_mode {
                        DitherMode::None => "None (Fastest)",
                        DitherMode::Bayer4x4 => "Bayer 4×4 Spatial (Mitigates banding on LED walls)",
                    })
                    .show_ui(ui, |ui| {
                        if ui.selectable_value(&mut self.enc_dither_mode, DitherMode::None, "None (Fastest)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                        if ui.selectable_value(&mut self.enc_dither_mode, DitherMode::Bayer4x4, "Bayer 4×4 Spatial (Eliminates step banding on LED walls / Projectors)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("Quality Preset:").color(colors::TEXT_MUTED));
                egui::ComboBox::from_id_salt("quality_combo")
                    .selected_text(match self.enc_quality {
                        QualityPreset::Production => "Production Master (ClusterFit, Best Quality)",
                        QualityPreset::Draft => "Draft / Rush (RangeFit, ~3x Faster)",
                    })
                    .show_ui(ui, |ui| {
                        if ui.selectable_value(&mut self.enc_quality, QualityPreset::Production, "Production Master (ClusterFit, Optimal RMS error)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                        if ui.selectable_value(&mut self.enc_quality, QualityPreset::Draft, "Draft / Rush (RangeFit, Real-time fast ingest)").clicked() {
                            self.enc_preset = EncoderPreset::Custom;
                        }
                    });
                ui.end_row();
            });
        });

        ui.add_space(16.0);

        // --- ENCODE ACTIONS ---
        let can_start = self.enc_input_path.is_some()
            && self.enc_output_path.is_some()
            && self.enc_detected_frames > 0
            && self.enc_rx.is_none();

        ui.horizontal(|ui| {
            let encode_btn = egui::Button::new(RichText::new("Start Encoding").size(15.0).strong())
                .min_size(Vec2::new(160.0, 40.0))
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
                        color_range: self.enc_color_range,
                        alpha_mode: self.enc_alpha_mode,
                        dither_mode: self.enc_dither_mode,
                        quality: self.enc_quality,
                    };

                    self.log(&format!("Started encode: {:?} -> {:?}", in_p, out_p));
                    spawn_encode_worker(cfg, cancel_flag, tx);
                }
            }

            if self.enc_rx.is_some() {
                let cancel_btn = egui::Button::new(RichText::new("Cancel").size(15.0).color(colors::ACCENT_RED))
                    .min_size(Vec2::new(100.0, 40.0));
                if ui.add(cancel_btn).clicked() {
                    if let Some(ref cancel) = self.enc_cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            }
        });

        // Live Progress & Telemetry Card
        let mut load_into_player: Option<PathBuf> = None;

        if let Some(ref status) = self.enc_status {
            ui.add_space(12.0);
            let progress_frame = egui::Frame::canvas(ui.style())
                .fill(colors::BG_CARD)
                .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(18));

            progress_frame.show(ui, |ui| {
                match status {
                    WorkerProgress::Started { total } => {
                        ui.label(format!("Starting encode of {} frames...", total));
                    }
                    WorkerProgress::Progress { current, total, fps, percent } => {
                        ui.horizontal(|ui| {
                            ui.strong(format!("Encoding [{} / {}] ({:.1}%)", current, total, percent));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(format!("{:.1} FPS", fps)).color(colors::ACCENT_CYAN).strong());
                            });
                        });
                        ui.add_space(6.0);
                        ui.add(egui::ProgressBar::new(*percent / 100.0).show_percentage());

                        if let Some(start) = self.enc_start_time {
                            let elapsed = start.elapsed().as_secs_f32();
                            let remaining = if *current > 0 {
                                (elapsed / *current as f32) * (total.saturating_sub(*current) as f32)
                            } else {
                                0.0
                            };
                            ui.add_space(4.0);
                            ui.label(RichText::new(format!("Elapsed: {:.1}s • ETA: {:.1}s", elapsed, remaining)).color(colors::TEXT_FAINT));
                        }
                    }
                    WorkerProgress::Finished { message } => {
                        ui.label(RichText::new(message).color(colors::ACCENT_GREEN).strong());

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if let Some(ref mov) = self.enc_last_successful_mov {
                                if ui.add(egui::Button::new(RichText::new("Open in Player").strong().color(colors::ACCENT_CYAN)).min_size(Vec2::new(130.0, 36.0))).clicked() {
                                    load_into_player = Some(mov.clone());
                                }
                                if ui.add(egui::Button::new("Show in Explorer").min_size(Vec2::new(130.0, 36.0))).clicked() {
                                    reveal_in_file_manager(mov);
                                }
                            }
                        });
                    }
                    WorkerProgress::Error(err) => {
                        ui.label(RichText::new(format!("Encode error: {}", err)).color(colors::ACCENT_RED).strong());
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
// TAB 3: HARDWARE BENCHMARK
// ---------------------------------------------------------------------------
impl HapLabApp {
    fn show_benchmark_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Hardware & Codec Benchmark").size(19.0).color(Color32::WHITE));
            ui.label(RichText::new("Measure real-world HAP decompression latency, 4K multi-threaded decode, real-time encoding speed, and texture streaming bandwidth.").color(colors::TEXT_MUTED));
        });
        ui.add_space(10.0);

        // --- HARDWARE SPEC & BENCHMARK CONTROLS CARD ---
        let ctrl_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        ctrl_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.strong(RichText::new("System Hardware Configuration").size(14.5));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        render_badge(ui, &format!("{} Rayon Threads", rayon::current_num_threads()), colors::BG_ELEVATED, colors::ACCENT_CYAN);
                        render_badge(ui, &self.gpu_adapter_name, colors::BG_ELEVATED, colors::TEXT_PRIMARY);
                        if self.gpu_supports_bc {
                            render_badge(ui, "BC Hardware Textures", Color32::from_rgb(16, 40, 25), colors::ACCENT_GREEN);
                        } else {
                            render_badge(ui, "CPU Fallback", Color32::from_rgb(45, 38, 15), colors::TEXT_MUTED);
                        }
                    });
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.bench_is_running {
                        let cancel_btn = egui::Button::new(RichText::new("Cancel Benchmark").size(14.0).strong().color(colors::ACCENT_RED))
                            .min_size(Vec2::new(150.0, 36.0));
                        if ui.add(cancel_btn).clicked() {
                            if let Some(ref cancel) = self.bench_cancel {
                                cancel.store(true, Ordering::Relaxed);
                            }
                            self.bench_is_running = false;
                            self.bench_current_test = None;
                            self.log("Benchmark cancelled by user.");
                        }
                    } else {
                        let run_btn = egui::Button::new(RichText::new("Run Benchmark Suite").size(14.0).strong())
                            .min_size(Vec2::new(170.0, 36.0))
                            .fill(colors::ACCENT_BLUE)
                            .corner_radius(CornerRadius::same(6));

                        if ui.add(run_btn).clicked() {
                            let cancel_flag = Arc::new(AtomicBool::new(false));
                            let (tx, rx) = crossbeam_channel::unbounded();
                            self.bench_cancel = Some(cancel_flag.clone());
                            self.bench_rx = Some(rx);
                            self.bench_is_running = true;
                            self.bench_scores.clear();
                            self.bench_current_test = Some("Initializing benchmark...".to_string());
                            self.bench_current_step = 0;
                            self.bench_total_steps = 1;
                            self.bench_current_fps = 0.0;
                            self.log("Hardware benchmark suite initiated.");
                            spawn_benchmark_worker(cancel_flag, tx);
                        }
                    }

                    if !self.bench_scores.is_empty() && !self.bench_is_running {
                        if ui.add(egui::Button::new("Clear Results").min_size(Vec2::new(100.0, 36.0))).clicked() {
                            self.bench_scores.clear();
                        }
                    }
                });
            });
        });

        // --- ACTIVE TEST PROGRESS CARD ---
        if self.bench_is_running {
            ui.add_space(12.0);
            let prog_frame = egui::Frame::canvas(ui.style())
                .fill(colors::BG_CARD)
                .stroke(Stroke::new(1.0, colors::ACCENT_CYAN))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(18));

            prog_frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    let test_title = self.bench_current_test.as_deref().unwrap_or("Running test...");
                    ui.strong(RichText::new(test_title).size(15.0).color(colors::ACCENT_CYAN));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.bench_current_fps > 0.0 {
                            ui.label(RichText::new(format!("{:.1} FPS", self.bench_current_fps)).size(15.0).color(colors::ACCENT_GREEN).strong());
                        }
                    });
                });

                ui.add_space(8.0);

                let progress = if self.bench_total_steps > 0 {
                    (self.bench_current_step as f32 / self.bench_total_steps as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ui.add(egui::ProgressBar::new(progress).show_percentage());

                ui.add_space(4.0);
                ui.label(RichText::new(format!("Progress: {} of {} frames processed", self.bench_current_step, self.bench_total_steps)).color(colors::TEXT_FAINT));
            });
        }

        ui.add_space(14.0);

        // --- RESULTS SCORECARD CARD ---
        let results_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        results_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("Benchmark Scorecard & Hardware Capability").size(15.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.bench_scores.is_empty() {
                        if ui.add(egui::Button::new("Copy Results (Markdown)").min_size(Vec2::new(160.0, 30.0))).clicked() {
                            let mut md = String::from("| Benchmark Test | Resolution | Speed (FPS) | Latency (ms) | Throughput (GB/s) | Rating |\n|---|---|---|---|---|---|\n");
                            for s in &self.bench_scores {
                                md.push_str(&format!("| {} | {} | {:.1} | {:.2} ms | {:.2} GB/s | {} |\n", s.test_name, s.resolution, s.fps, s.frame_time_ms, s.bandwidth_gbps, s.performance_rating));
                            }
                            ui.copy_text(md);
                            self.notify("Benchmark scorecard copied to clipboard.", colors::ACCENT_CYAN);
                        }
                    }
                });
            });

            ui.add_space(10.0);

            if self.bench_scores.is_empty() && !self.bench_is_running {
                ui.vertical_centered(|ui| {
                    ui.add_space(16.0);
                    ui.label(RichText::new("No benchmark data recorded yet.").color(colors::TEXT_MUTED).size(14.0));
                    ui.label(RichText::new("Click 'Run Benchmark Suite' above to evaluate your CPU decoding and encoding performance.").color(colors::TEXT_FAINT));
                    ui.add_space(16.0);
                });
            } else {
                egui::Grid::new("bench_scorecard_grid")
                    .striped(true)
                    .spacing([24.0, 10.0])
                    .show(ui, |ui| {
                        // Table Headers
                        ui.label(RichText::new("Benchmark Test").strong().color(colors::TEXT_PRIMARY));
                        ui.label(RichText::new("Resolution").strong().color(colors::TEXT_PRIMARY));
                        ui.label(RichText::new("Throughput (FPS)").strong().color(colors::TEXT_PRIMARY));
                        ui.label(RichText::new("Frame Latency").strong().color(colors::TEXT_PRIMARY));
                        ui.label(RichText::new("Bandwidth").strong().color(colors::TEXT_PRIMARY));
                        ui.label(RichText::new("Production Capability").strong().color(colors::TEXT_PRIMARY));
                        ui.end_row();

                        for score in &self.bench_scores {
                            ui.label(RichText::new(&score.test_name).strong());
                            ui.label(RichText::new(&score.resolution).color(colors::TEXT_MUTED))
                                .on_hover_text(format!("{} frames measured in {:.2}s", score.frame_count, score.elapsed_secs));

                            let fps_color = if score.fps >= 120.0 {
                                colors::ACCENT_GREEN
                            } else if score.fps >= 60.0 {
                                colors::ACCENT_CYAN
                            } else if score.fps >= 30.0 {
                                colors::ACCENT_AMBER
                            } else {
                                colors::ACCENT_RED
                            };
                            ui.label(RichText::new(format!("{:.1} FPS", score.fps)).color(fps_color).strong());

                            ui.label(format!("{:.2} ms", score.frame_time_ms));
                            ui.label(format!("{:.2} GB/s", score.bandwidth_gbps));

                            render_badge(ui, score.performance_rating, colors::BG_ELEVATED, colors::ACCENT_GREEN);
                            ui.end_row();
                        }
                    });
            }
        });

        ui.add_space(14.0);

        // --- HARDWARE REFERENCE GUIDE CARD ---
        let guide_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        guide_frame.show(ui, |ui| {
            ui.strong(RichText::new("Media Server Performance Thresholds & Guidelines").size(14.5));
            ui.add_space(8.0);
            ui.label(RichText::new("• 1080p60 Real-Time: Requires frame decode time <= 16.6 ms (>= 60 FPS). Hap 1 / Hap Q typically achieves 300-800 FPS on modern multi-core CPUs.").color(colors::TEXT_MUTED));
            ui.add_space(4.0);
            ui.label(RichText::new("• 4K UHD 60FPS: Requires frame decode time <= 16.6 ms. Multi-chunk encoding (4 to 8 chunks) is essential to utilize all CPU threads in parallel.").color(colors::TEXT_MUTED));
            ui.add_space(4.0);
            ui.label(RichText::new("• 4K UHD 120FPS / Multi-Head: Requires frame decode time <= 8.3 ms (>= 120 FPS). Essential for ultra-smooth LED wall rendering and XR virtual production stages.").color(colors::TEXT_MUTED));
            ui.add_space(4.0);
            ui.label(RichText::new("• Real-Time Fast Ingest: Draft quality preset allows 60+ FPS live sequence encoding for instant turnaround in studio workflows.").color(colors::TEXT_MUTED));
        });
    }
}

// ---------------------------------------------------------------------------
// TAB 4: DIAGNOSTICS & SYSTEM
// ---------------------------------------------------------------------------
impl HapLabApp {
    fn show_diagnostics_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("System & Diagnostics").size(19.0));
        ui.label(RichText::new("GPU capabilities, file associations, threading, and runtime event log.").color(colors::TEXT_MUTED));
        ui.add_space(10.0);

        let diag_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        diag_frame.show(ui, |ui| {
            egui::Grid::new("diag_grid").striped(true).spacing([32.0, 12.0]).show(ui, |ui| {
                ui.strong("GPU Adapter:");
                ui.label(&self.gpu_adapter_name);
                ui.end_row();

                ui.strong("Graphics API:");
                ui.label(&self.gpu_backend_name);
                ui.end_row();

                ui.strong("BC Texture Compression:");
                ui.horizontal(|ui| {
                    if self.gpu_supports_bc {
                        render_badge(ui, "Hardware Supported", Color32::from_rgb(16, 40, 30), colors::ACCENT_GREEN);
                        ui.label("Direct VRAM upload enabled");
                    } else {
                        render_badge(ui, "CPU Fallback", Color32::from_rgb(38, 35, 25), colors::TEXT_MUTED);
                        ui.label("Software decompression");
                    }
                });
                ui.end_row();

                ui.strong("Worker Threads:");
                ui.label(format!("{} threads (Rayon)", rayon::current_num_threads()));
                ui.end_row();
            });
        });

        // Windows File Integration Card
        ui.add_space(14.0);
        let assoc_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        assoc_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("Windows File Integration & Default Associations").size(15.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    render_badge(ui, "QuickTime .MOV Handler", colors::BG_ELEVATED, colors::ACCENT_CYAN);
                });
            });

            ui.add_space(8.0);
            ui.label(RichText::new(
                "Register HapLab with Windows Explorer to automatically open .MOV files and enable seamless playback and inspection."
            ).color(colors::TEXT_MUTED));

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Associate .MOV with HapLab").strong().color(colors::ACCENT_CYAN)).min_size(Vec2::new(210.0, 36.0))).clicked() {
                    match register_mov_association() {
                        Ok(msg) => {
                            self.log(&msg);
                            self.notify(msg, colors::ACCENT_GREEN);
                        }
                        Err(err) => {
                            let err_msg = format!("File association error: {}", err);
                            self.log(&err_msg);
                            self.notify(err_msg, colors::ACCENT_RED);
                        }
                    }
                }

                if ui.add(egui::Button::new("Open Windows Default Apps Settings").min_size(Vec2::new(240.0, 36.0))).clicked() {
                    open_windows_default_apps();
                    self.log("Opened Windows Default Apps configuration page.");
                }
            });

            ui.add_space(6.0);
            ui.label(RichText::new(
                "Note: On Windows 10 and 11, Microsoft requires user confirmation in the Default Apps control panel. Clicking 'Associate .MOV with HapLab' writes the registry handler, and 'Open Windows Default Apps Settings' allows you to select HapLab as the default player."
            ).color(colors::TEXT_FAINT).size(11.5));
        });

        ui.add_space(14.0);

        // Activity Log Card
        let log_frame = egui::Frame::canvas(ui.style())
            .fill(colors::BG_CARD)
            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(18));

        log_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("Activity Log").size(15.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Clear").min_size(Vec2::new(80.0, 30.0))).clicked() {
                        self.system_logs.clear();
                    }
                    if ui.add(egui::Button::new("Copy Logs").min_size(Vec2::new(110.0, 30.0))).clicked() {
                        let text = self.system_logs.join("\n");
                        ui.copy_text(text);
                        self.notify("Logs copied to clipboard", colors::ACCENT_CYAN);
                    }
                });
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_sized(Vec2::new(260.0, 32.0), egui::TextEdit::singleline(&mut self.log_search).hint_text("Filter logs..."));
                if !self.log_search.is_empty() && ui.add(egui::Button::new("Clear filter").min_size(Vec2::new(90.0, 32.0))).clicked() {
                    self.log_search.clear();
                }
            });
            ui.add_space(8.0);

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
