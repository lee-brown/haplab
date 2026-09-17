//! Main eframe / egui application interface for HapLab.
//! Restructured into a player-first studio application with native menu bar,
//! edge-to-edge media canvas, and non-intrusive utility windows.

use super::theme::{
    apply_studio_theme, colors, format_bytes, format_smpte_timecode,
    paint_ambient_glow, paint_transparency_checkerboard, render_badge, reveal_in_file_manager,
};
use crate::benchmark::{spawn_benchmark_worker, BenchmarkProgress, BenchmarkScore};
use crate::platform::{open_windows_default_apps, register_mov_association};
use crate::worker::{
    export_image_to_file, export_image_to_hap_mov, is_video_container,
    probe_video_input, spawn_encode_worker, spawn_export_worker, spawn_media_loader,
    EncodeJobConfig, GenericVideoPlayer, MediaLoadResult, WorkerProgress,
};
use crossbeam_channel::Receiver;

/// Information about a loaded still image.
#[derive(Debug, Clone)]
pub struct StillImageInfo {
    pub path: PathBuf,
    pub width: usize,
    pub height: usize,
}
use eframe::egui::{
    self, Color32, CornerRadius, Rect, RichText, Stroke, TextureOptions, Vec2,
};
use hap_core::{
    audit_hap_stream, decode_frame_to_rgba, AlphaMode, ColorRange,
    DitherMode, FaultSeverity, HapFormat, QualityPreset, QtHapReader, StreamAudit, StreamSummary,
};
use image::GenericImageView;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

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
    toast: Option<(String, Instant, Color32)>,

    // --- Modal Window Visibility States ---
    pub show_transcode_window: bool,
    pub show_benchmark_window: bool,
    pub show_audit_window: bool,
    pub show_diagnostics_window: bool,
    pub show_shortcuts_window: bool,
    pub show_about_window: bool,
    pub show_hud_overlay: bool,

    // --- Player / Media State ---
    mov_path: Option<PathBuf>,
    reader: Option<QtHapReader>,
    generic_player: Option<GenericVideoPlayer>,
    seek_rx: Option<Receiver<(usize, Vec<u8>)>>,
    pending_seek_frame: Option<usize>,
    was_playing_before_scrub: bool,
    is_fullscreen: bool,
    playback_start_instant: Instant,
    playback_start_frame: usize,
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
    last_export_dir: Option<PathBuf>,

    // --- Async Media Loading & Still Image Conversion ---
    media_load_rx: Option<Receiver<MediaLoadResult>>,
    is_loading_media: bool,
    loading_filename: String,
    still_image_info: Option<StillImageInfo>,
    still_image_export_format: String,

    // --- Live Stream Evaluation & Fault Audit ---
    last_decode_ms: f32,
    last_packet_bytes: usize,
    playback_fps: f32,
    playback_frames_count: usize,
    playback_timer: Instant,
    stream_summary: Option<StreamSummary>,
    stream_audit: Option<StreamAudit>,
    is_auditing: bool,

    // --- Encoder / Ingest State ---
    enc_input_path: Option<PathBuf>,
    enc_output_path: Option<PathBuf>,
    enc_detected_frames: usize,
    enc_detected_w: u32,
    enc_detected_h: u32,
    enc_detected_first_name: Option<String>,
    enc_detected_last_name: Option<String>,
    enc_detected_codec: Option<String>,
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
    app_icon_texture: Option<egui::TextureHandle>,

    // --- Ambient Glow & Interaction State ---
    ambient_start: Instant,
    show_ambient_glow: bool,
    is_drag_hovered: bool,
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
            toast: None,

            show_transcode_window: false,
            show_benchmark_window: false,
            show_audit_window: false,
            show_diagnostics_window: false,
            show_shortcuts_window: false,
            show_about_window: false,
            show_hud_overlay: true,

            mov_path: None,
            reader: None,
            generic_player: None,
            seek_rx: None,
            pending_seek_frame: None,
            was_playing_before_scrub: false,
            is_fullscreen: false,
            playback_start_instant: Instant::now(),
            playback_start_frame: 0,
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
            last_export_dir: None,

            media_load_rx: None,
            is_loading_media: false,
            loading_filename: String::new(),
            still_image_info: None,
            still_image_export_format: "mov".to_string(),

            last_decode_ms: 0.0,
            last_packet_bytes: 0,
            playback_fps: 0.0,
            playback_frames_count: 0,
            playback_timer: Instant::now(),
            stream_summary: None,
            stream_audit: None,
            is_auditing: false,

            enc_input_path: None,
            enc_output_path: None,
            enc_detected_frames: 0,
            enc_detected_w: 0,
            enc_detected_h: 0,
            enc_detected_first_name: None,
            enc_detected_last_name: None,
            enc_detected_codec: None,
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
            app_icon_texture: None,

            ambient_start: Instant::now(),
            show_ambient_glow: true,
            is_drag_hovered: false,
        };

        app.log("HapLab initialized in player-first studio layout.");
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

    pub fn close_media(&mut self) {
        if let Some(ref mut player) = self.generic_player {
            player.stop_playback();
        }
        self.generic_player = None;
        self.seek_rx = None;
        self.pending_seek_frame = None;
        self.was_playing_before_scrub = false;
        self.reader = None;
        self.mov_path = None;
        self.raw_frame_cache = None;
        self.preview_texture = None;
        self.stream_summary = None;
        self.stream_audit = None;
        self.enc_input_path = None;
        self.enc_thumbnail_texture = None;
        self.enc_detected_frames = 0;
        self.enc_detected_w = 0;
        self.enc_detected_h = 0;
        self.enc_detected_codec = None;
        self.still_image_info = None;
        self.is_loading_media = false;
        self.media_load_rx = None;
        self.current_frame = 0;
        self.is_playing = false;
        self.notify("Media closed", colors::TEXT_MUTED);
    }

    pub fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.is_fullscreen = !self.is_fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
    }

    /// Asynchronously opens a media file or directory without freezing the UI thread.
    pub fn open_media_file(&mut self, path: PathBuf, ctx: &egui::Context) {
        let filename = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "media".to_string());

        self.is_loading_media = true;
        self.loading_filename = filename;
        let (tx, rx) = crossbeam_channel::unbounded();
        self.media_load_rx = Some(rx);
        spawn_media_loader(path, tx);
        ctx.request_repaint();
    }

    /// Quickly launch HAP transcode of loaded source video and auto-play immediately on completion.
    #[allow(dead_code)]
    pub fn start_quick_encode_and_play(&mut self) {
        if let (Some(in_path), Some(out_path)) = (&self.enc_input_path, &self.enc_output_path) {
            let cancel_flag = Arc::new(AtomicBool::new(false));
            let (tx, rx) = crossbeam_channel::unbounded();
            self.enc_rx = Some(rx);
            self.enc_cancel = Some(cancel_flag.clone());
            let config = EncodeJobConfig {
                input_dir: in_path.clone(),
                output_file: out_path.clone(),
                format: self.enc_format,
                fps: self.enc_fps,
                chunks: self.enc_chunks,
                snappy: self.enc_snappy,
                color_range: self.enc_color_range,
                alpha_mode: self.enc_alpha_mode,
                dither_mode: self.enc_dither_mode,
                quality: self.enc_quality,
                video_dimensions: if self.enc_detected_w > 0 && self.enc_detected_h > 0 {
                    Some((self.enc_detected_w as usize, self.enc_detected_h as usize))
                } else {
                    None
                },
                total_frames: if self.enc_detected_frames > 0 {
                    Some(self.enc_detected_frames)
                } else {
                    None
                },
            };
            self.enc_start_time = Some(Instant::now());
            spawn_encode_worker(config, cancel_flag, tx);
            self.notify("Preparing video for high-speed HAP playback...", colors::ACCENT_BLUE);
        }
    }

    pub fn has_video_loaded(&self) -> bool {
        self.reader.is_some() || self.generic_player.is_some()
    }

    pub fn video_width(&self) -> usize {
        if let Some(ref r) = self.reader {
            r.width() as usize
        } else if let Some(ref p) = self.generic_player {
            p.play_width
        } else if let Some(ref img) = self.still_image_info {
            img.width
        } else {
            self.enc_detected_w as usize
        }
    }

    pub fn video_height(&self) -> usize {
        if let Some(ref r) = self.reader {
            r.height() as usize
        } else if let Some(ref p) = self.generic_player {
            p.play_height
        } else if let Some(ref img) = self.still_image_info {
            img.height
        } else {
            self.enc_detected_h as usize
        }
    }

    pub fn video_fps(&self) -> f32 {
        if let Some(ref r) = self.reader {
            r.fps().max(1.0)
        } else if let Some(ref p) = self.generic_player {
            p.fps.max(1.0)
        } else if self.enc_fps > 0.0 {
            self.enc_fps
        } else {
            30.0
        }
    }

    pub fn video_total_frames(&self) -> usize {
        if let Some(ref r) = self.reader {
            r.frame_count()
        } else if let Some(ref p) = self.generic_player {
            p.total_frames
        } else if self.still_image_info.is_some() {
            1
        } else {
            self.enc_detected_frames
        }
    }

    pub fn toggle_playback(&mut self, ctx: &egui::Context) {
        if self.is_playing {
            self.is_playing = false;
            if let Some(ref mut player) = self.generic_player {
                player.stop_playback();
            }
        } else {
            let total = self.video_total_frames();
            if total > 0 {
                if self.current_frame + 1 >= total {
                    self.current_frame = 0;
                }
                self.is_playing = true;
                self.playback_start_instant = Instant::now();
                self.playback_start_frame = self.current_frame;
                self.last_frame_time = Instant::now();
                self.playback_timer = Instant::now();
                self.playback_frames_count = 0;
                if let Some(ref mut player) = self.generic_player {
                    player.start_playback(self.current_frame);
                } else if self.reader.is_some() {
                    self.update_preview_frame(ctx);
                }
            }
        }
        ctx.request_repaint();
    }

    pub fn seek_to_frame(&mut self, frame: usize, ctx: &egui::Context) {
        let total = self.video_total_frames();
        let target = if total > 0 { frame.min(total.saturating_sub(1)) } else { 0 };
        self.current_frame = target;

        if let Some(ref mut player) = self.generic_player {
            if self.is_playing {
                player.start_playback(target);
                self.playback_start_instant = Instant::now();
                self.playback_start_frame = target;
                self.last_frame_time = Instant::now();
            } else if self.seek_rx.is_some() {
                // Seek process is already active, queue this target to prevent process flooding
                self.pending_seek_frame = Some(target);
            } else {
                let (tx, rx) = crossbeam_channel::bounded(1);
                self.seek_rx = Some(rx);
                let path = player.path.clone();
                let pw = player.play_width;
                let ph = player.play_height;
                let orig_w = player.original_width;
                let orig_h = player.original_height;
                let fps = player.fps;

                std::thread::Builder::new()
                    .name("player-seek".to_string())
                    .spawn(move || {
                        let sec = (target as f64) / (fps as f64);
                        let ffmpeg_bin = crate::worker::find_ffmpeg_binary();
                        let mut cmd = std::process::Command::new(&ffmpeg_bin);
                        cmd.stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped());

                        cmd.args(&[
                            "-nostdin", "-an", "-sn", "-v", "error",
                            "-hwaccel", "auto",
                            "-threads", "0",
                            "-sws_flags", "fast_bilinear",
                            "-ss", &format!("{:.3}", sec),
                            "-i",
                        ])
                        .arg(&path);

                        if pw != orig_w || ph != orig_h {
                            cmd.args(&["-vf", &format!("scale={}:{}:flags=fast_bilinear", pw, ph)]);
                        }

                        cmd.args(&[
                            "-vframes", "1",
                            "-f", "rawvideo",
                            "-pix_fmt", "rgba",
                            "-",
                        ]);

                        #[cfg(windows)]
                        {
                            use std::os::windows::process::CommandExt;
                            cmd.creation_flags(0x08000000);
                        }

                        if let Ok(out) = cmd.output() {
                            let expected = pw * ph * 4;
                            if out.stdout.len() >= expected {
                                let mut data = out.stdout;
                                data.truncate(expected);
                                let _ = tx.send((target, data));
                            }
                        }
                    })
                    .ok();
            }
        } else if self.reader.is_some() {
            self.playback_start_instant = Instant::now();
            self.playback_start_frame = target;
            self.update_preview_frame(ctx);
        }
        ctx.request_repaint();
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
                    self.rebuild_texture_from_cache(ctx, width, height, &mut rgba);
                    self.raw_frame_cache = Some(rgba);
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

        let color_img = egui::ColorImage::from_rgba_premultiplied([width, height], rgba);
        if let Some(ref mut tex) = self.preview_texture {
            if tex.size() == [width, height] {
                tex.set(color_img, TextureOptions::LINEAR);
            } else {
                self.preview_texture = Some(ctx.load_texture("player-frame", color_img, TextureOptions::LINEAR));
            }
        } else {
            self.preview_texture = Some(ctx.load_texture("player-frame", color_img, TextureOptions::LINEAR));
        }
    }

    fn refresh_channel_view(&mut self, ctx: &egui::Context) {
        if let Some(ref raw) = self.raw_frame_cache {
            let w = self.video_width();
            let h = self.video_height();
            if w > 0 && h > 0 {
                let mut copy = raw.clone();
                self.rebuild_texture_from_cache(ctx, w, h, &mut copy);
            }
        }
    }

    #[allow(dead_code)]
    fn scan_encoder_input(&mut self, ctx: &egui::Context) {
        let Some(path) = self.enc_input_path.clone() else {
            return;
        };
        let mut count = 0;
        let mut first_file = None;
        let mut last_file = None;

        if path.is_dir() {
            if let Ok(entries) = fs::read_dir(&path) {
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
                self.enc_detected_codec = Some("Image Sequence".to_string());
            }
        } else if path.is_file() {
            if is_video_container(&path) {
                match probe_video_input(&path) {
                    Ok(probe) => {
                        count = probe.frame_count;
                        first_file = Some(path.clone());
                        last_file = Some(path.clone());
                        self.enc_detected_w = probe.width as u32;
                        self.enc_detected_h = probe.height as u32;
                        self.enc_fps = probe.fps;
                        self.enc_detected_codec = Some(probe.codec.clone());
                        self.log(&format!(
                            "Video detected [{}]: {}x{} @ {:.2} fps, {} frames ({:.2}s)",
                            probe.codec, probe.width, probe.height, probe.fps, probe.frame_count, probe.duration_secs
                        ));

                        if let Some((tw, th, rgba)) = probe.thumbnail_rgba {
                            let color_img = egui::ColorImage::from_rgba_unmultiplied(
                                [tw as usize, th as usize],
                                &rgba,
                            );
                            self.enc_thumbnail_texture = Some(ctx.load_texture(
                                "enc-thumb",
                                color_img,
                                TextureOptions::LINEAR,
                            ));
                        }
                    }
                    Err(err) => {
                        self.notify(err.clone(), colors::ACCENT_AMBER);
                        self.log(&format!("Video probe note: {}", err));
                        self.enc_detected_codec = Some("Video".to_string());
                        count = 1;
                        first_file = Some(path.clone());
                        last_file = Some(path.clone());
                    }
                }
            } else {
                count = 1;
                first_file = Some(path.clone());
                last_file = Some(path.clone());
                self.enc_detected_codec = Some("Still Image".to_string());
            }
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

        if self.enc_thumbnail_texture.is_none() {
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
        }

        self.update_suggested_output_filename();
        self.notify(
            format!("Detected {} frames ({}x{})", count, self.enc_detected_w, self.enc_detected_h),
            colors::ACCENT_CYAN,
        );
    }
}

impl eframe::App for HapLabApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_studio_theme(ctx);

        // 1. Drag & Drop File Handling
        let dropped_file_path = ctx.input_mut(|i| {
            self.is_drag_hovered = !i.raw.hovered_files.is_empty();
            if !i.raw.dropped_files.is_empty() {
                let dropped = std::mem::take(&mut i.raw.dropped_files);
                dropped.into_iter().find_map(|f| f.path)
            } else {
                None
            }
        });

        if let Some(path) = dropped_file_path {
            if !self.is_loading_media {
                self.open_media_file(path, ctx);
            }
        }

        // 2. Global & Player Keyboard Shortcuts
        ctx.input(|i| {
            // Hotkeys with Ctrl / Command
            if i.modifiers.command {
                if i.key_pressed(egui::Key::O) {
                    if i.modifiers.shift {
                        // Ctrl+Shift+O: Open Folder
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            self.open_media_file(folder, ctx);
                        }
                    } else {
                        // Ctrl+O: Open Media File
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("All Media", &["mov", "mp4", "mkv", "avi", "webm", "m4v", "mxf", "ts", "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                            .add_filter("QuickTime HAP Videos (*.mov)", &["mov"])
                            .add_filter("Video Files (*.mp4, *.mkv, *.webm, *.mxf, *.avi)", &["mp4", "mkv", "avi", "webm", "m4v", "mxf", "ts"])
                            .add_filter("Image Sequences (*.png, *.tiff, *.jpg)", &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                            .pick_file()
                        {
                            self.open_media_file(path, ctx);
                        }
                    }
                }
                if i.key_pressed(egui::Key::E) {
                    self.show_transcode_window = !self.show_transcode_window;
                }
                if i.key_pressed(egui::Key::B) {
                    self.show_benchmark_window = !self.show_benchmark_window;
                }
                if i.key_pressed(egui::Key::T) {
                    self.show_audit_window = !self.show_audit_window;
                }
                if i.key_pressed(egui::Key::D) {
                    self.show_diagnostics_window = !self.show_diagnostics_window;
                }
                if i.key_pressed(egui::Key::W) {
                    self.close_media();
                }
            }

            // Function keys
            if i.key_pressed(egui::Key::F1) {
                self.show_shortcuts_window = !self.show_shortcuts_window;
            }
            if i.key_pressed(egui::Key::I) {
                self.show_hud_overlay = !self.show_hud_overlay;
            }
            if i.key_pressed(egui::Key::F11) {
                self.toggle_fullscreen(ctx);
            }
            if i.key_pressed(egui::Key::Escape) && self.is_fullscreen {
                self.toggle_fullscreen(ctx);
            }

            if i.key_pressed(egui::Key::Space) {
                if self.has_video_loaded() {
                    self.toggle_playback(ctx);
                }
            }

            // Playback controls (when any video is loaded)
            if self.has_video_loaded() {
                let frame_count = self.video_total_frames();
                if frame_count > 0 {
                    if i.key_pressed(egui::Key::ArrowLeft) {
                        let step = if i.modifiers.shift { 10 } else { 1 };
                        let target = self.current_frame.saturating_sub(step);
                        self.seek_to_frame(target, ctx);
                    }
                    if i.key_pressed(egui::Key::ArrowRight) {
                        let step = if i.modifiers.shift { 10 } else { 1 };
                        let target = (self.current_frame + step).min(frame_count.saturating_sub(1));
                        self.seek_to_frame(target, ctx);
                    }
                    if i.key_pressed(egui::Key::Home) {
                        self.seek_to_frame(0, ctx);
                    }
                    if i.key_pressed(egui::Key::End) {
                        self.seek_to_frame(frame_count.saturating_sub(1), ctx);
                    }
                    if i.key_pressed(egui::Key::L) {
                        self.loop_playback = !self.loop_playback;
                        self.notify(
                            format!("Loop: {}", if self.loop_playback { "On" } else { "Off" }),
                            colors::ACCENT_CYAN,
                        );
                    }
                    if i.key_pressed(egui::Key::Num1) {
                        self.channel_mode = ChannelViewMode::Rgba;
                        self.refresh_channel_view(ctx);
                    }
                    if i.key_pressed(egui::Key::Num2) {
                        self.channel_mode = ChannelViewMode::RgbOpaque;
                        self.refresh_channel_view(ctx);
                    }
                    if i.key_pressed(egui::Key::Num3) {
                        self.channel_mode = ChannelViewMode::AlphaMatte;
                        self.refresh_channel_view(ctx);
                    }
                }
            }
        });

        // 3. Playback Frame Clock Synchronization
        if self.is_playing {
            if let Some(ref reader) = self.reader {
                let fps = reader.fps().max(1.0);
                let count = reader.frame_count();
                let elapsed_secs = self.playback_start_instant.elapsed().as_secs_f64();
                let mut target_frame = self.playback_start_frame + (elapsed_secs * fps as f64).floor() as usize;

                if count > 0 {
                    if target_frame >= count {
                        if self.loop_playback {
                            self.playback_start_instant = Instant::now();
                            self.playback_start_frame = 0;
                            target_frame = 0;
                        } else {
                            self.is_playing = false;
                            target_frame = count.saturating_sub(1);
                        }
                    }
                }

                if target_frame != self.current_frame {
                    self.current_frame = target_frame;
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
            } else if let Some(ref mut player) = self.generic_player {
                let fps = player.fps.max(1.0);
                let total = player.total_frames;
                let elapsed_secs = self.playback_start_instant.elapsed().as_secs_f64();
                let mut target_frame = self.playback_start_frame + (elapsed_secs * fps as f64).floor() as usize;

                if total > 0 && target_frame >= total {
                    if self.loop_playback {
                        self.playback_start_instant = Instant::now();
                        self.playback_start_frame = 0;
                        self.current_frame = 0;
                        player.start_playback(0);
                        target_frame = 0;
                    } else {
                        self.is_playing = false;
                        player.stop_playback();
                        target_frame = total.saturating_sub(1);
                    }
                }

                // Drain frames up to target_frame from player channel so we never lag behind wall clock
                let mut latest_frame: Option<(usize, Vec<u8>)> = None;
                let mut frames_drained = 0usize;
                loop {
                    match player.try_recv_frame() {
                        Ok((frame_idx, rgba)) => {
                            if frame_idx <= target_frame {
                                frames_drained += 1;
                                latest_frame = Some((frame_idx, rgba));
                            } else {
                                player.unrecv_frame((frame_idx, rgba));
                                break;
                            }
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            if self.loop_playback && total > 0 {
                                self.playback_start_instant = Instant::now();
                                self.playback_start_frame = 0;
                                self.current_frame = 0;
                                player.start_playback(0);
                            } else {
                                self.is_playing = false;
                                player.stop_playback();
                            }
                            break;
                        }
                    }
                }

                if let Some((frame_idx, mut rgba)) = latest_frame {
                    self.current_frame = frame_idx;
                    let w = player.play_width;
                    let h = player.play_height;
                    self.rebuild_texture_from_cache(ctx, w, h, &mut rgba);
                    self.raw_frame_cache = Some(rgba);

                    self.playback_frames_count += frames_drained.max(1);
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

        // 3b. Asynchronous Seek Frame Polling
        if let Some(ref rx) = self.seek_rx {
            match rx.try_recv() {
                Ok((_target_frame, mut rgba)) => {
                    let w = self.video_width();
                    let h = self.video_height();
                    self.raw_frame_cache = Some(rgba.clone());
                    self.rebuild_texture_from_cache(ctx, w, h, &mut rgba);
                    self.seek_rx = None;

                    if let Some(next_target) = self.pending_seek_frame.take() {
                        self.seek_to_frame(next_target, ctx);
                    }
                    ctx.request_repaint();
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.seek_rx = None;
                    if let Some(next_target) = self.pending_seek_frame.take() {
                        self.seek_to_frame(next_target, ctx);
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    ctx.request_repaint();
                }
            }
        }

        // 4. Background Workers Polling
        if let Some(ref rx) = self.enc_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerProgress::Finished { ref message } => {
                        self.log(message);
                        self.notify(message.clone(), colors::ACCENT_GREEN);
                        if let Some(ref out_mov) = self.enc_output_path {
                            if out_mov.exists() {
                                self.enc_last_successful_mov = Some(out_mov.clone());
                                // Auto-open newly encoded HAP MOV in player
                                self.open_media_file(out_mov.clone(), ctx);
                                self.show_transcode_window = false;
                            }
                        }
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
                        self.bench_scores.push(score);
                    }
                    BenchmarkProgress::AllFinished { summary } => {
                        if !summary.is_empty() {
                            self.bench_scores = summary;
                        }
                        self.bench_is_running = false;
                        self.bench_current_test = None;
                        self.log("Benchmark suite completed successfully.");
                        self.notify("Benchmark suite completed", colors::ACCENT_GREEN);
                        self.bench_rx = None;
                        break;
                    }
                    BenchmarkProgress::Error(err) => {
                        self.bench_is_running = false;
                        self.bench_current_test = None;
                        self.log(&format!("Benchmark failed: {}", err));
                        self.notify(format!("Benchmark failed: {}", err), colors::ACCENT_RED);
                        self.bench_rx = None;
                        break;
                    }
                }
            }
            ctx.request_repaint();
        }

        // 4c. Async Media Loader Polling
        if let Some(ref rx) = self.media_load_rx {
            if let Ok(result) = rx.try_recv() {
                self.is_loading_media = false;
                self.media_load_rx = None;
                match result {
                    MediaLoadResult::HapVideo {
                        path,
                        reader,
                        summary,
                        first_frame_rgba,
                        first_frame_decode_ms,
                        first_packet_bytes,
                    } => {
                        let fname = path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let total_frames = reader.frame_count();
                        let w = reader.width() as usize;
                        let h = reader.height() as usize;
                        self.log(&format!(
                            "Opened HAP MOV: {} ({}x{}, {:.2} fps, {} frames, {})",
                            fname,
                            w,
                            h,
                            reader.fps(),
                            total_frames,
                            reader.format().name()
                        ));

                        if let Some(ref mut player) = self.generic_player {
                            player.stop_playback();
                        }
                        self.generic_player = None;
                        self.reader = Some(reader);
                        self.mov_path = Some(path.clone());
                        self.enc_input_path = Some(path);
                        self.stream_summary = summary;
                        self.stream_audit = None;
                        self.still_image_info = None;
                        self.current_frame = 0;
                        self.is_playing = true;
                        self.last_frame_time = Instant::now();
                        self.playback_timer = Instant::now();
                        self.playback_frames_count = 0;
                        self.last_decode_ms = first_frame_decode_ms;
                        self.last_packet_bytes = first_packet_bytes;

                        if let Some(mut rgba) = first_frame_rgba {
                            self.raw_frame_cache = Some(rgba.clone());
                            self.rebuild_texture_from_cache(ctx, w, h, &mut rgba);
                        } else {
                            self.update_preview_frame(ctx);
                        }

                        self.notify(
                            format!("Playing {}: {} frames", fname, total_frames),
                            colors::ACCENT_GREEN,
                        );
                    }
                    MediaLoadResult::StillImage {
                        path,
                        width,
                        height,
                        mut rgba,
                    } => {
                        let fname = path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if let Some(ref mut player) = self.generic_player {
                            player.stop_playback();
                        }
                        self.generic_player = None;
                        self.reader = None;
                        self.mov_path = None;
                        self.stream_summary = None;
                        self.stream_audit = None;
                        self.enc_input_path = Some(path.clone());
                        self.still_image_info = Some(StillImageInfo {
                            path,
                            width,
                            height,
                        });
                        self.current_frame = 0;
                        self.is_playing = false;
                        self.raw_frame_cache = Some(rgba.clone());
                        self.rebuild_texture_from_cache(ctx, width, height, &mut rgba);

                        self.notify(
                            format!("Loaded image: {} ({}x{}). Ready to convert or view.", fname, width, height),
                            colors::ACCENT_CYAN,
                        );
                    }
                    MediaLoadResult::GenericVideo { path, probe } => {
                        let fname = path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if let Some(ref mut player) = self.generic_player {
                            player.stop_playback();
                        }
                        self.generic_player = None;
                        self.reader = None;
                        self.mov_path = None;
                        self.stream_summary = None;
                        self.stream_audit = None;
                        self.still_image_info = None;
                        self.enc_input_path = Some(path.clone());
                        self.enc_detected_frames = probe.frame_count;
                        self.enc_detected_w = probe.width as u32;
                        self.enc_detected_h = probe.height as u32;
                        self.enc_fps = probe.fps;
                        self.enc_detected_codec = Some(probe.codec.clone());

                        if let Some((tw, th, ref rgba)) = probe.thumbnail_rgba {
                            self.raw_frame_cache = Some(rgba.clone());
                            let color_img = egui::ColorImage::from_rgba_unmultiplied(
                                [tw as usize, th as usize],
                                rgba,
                            );
                            self.preview_texture = Some(ctx.load_texture(
                                "enc-thumb",
                                color_img,
                                TextureOptions::LINEAR,
                            ));
                        }

                        let mut player = GenericVideoPlayer::new(path, &probe);
                        player.start_playback(0);
                        self.generic_player = Some(player);
                        self.current_frame = 0;
                        self.is_playing = true;
                        self.last_frame_time = Instant::now();
                        self.playback_timer = Instant::now();
                        self.playback_frames_count = 0;

                        self.update_suggested_output_filename();
                        self.notify(
                            format!("Playing {}: {} frames ({})", fname, probe.frame_count, probe.codec.to_uppercase()),
                            colors::ACCENT_GREEN,
                        );
                    }
                    MediaLoadResult::ImageSequence {
                        path,
                        count,
                        width,
                        height,
                        first_file,
                        last_file,
                        thumbnail_rgba,
                    } => {
                        if let Some(ref mut player) = self.generic_player {
                            player.stop_playback();
                        }
                        self.generic_player = None;
                        self.reader = None;
                        self.mov_path = None;
                        self.stream_summary = None;
                        self.stream_audit = None;
                        self.still_image_info = None;
                        self.enc_input_path = Some(path);
                        self.enc_detected_frames = count;
                        self.enc_detected_w = width as u32;
                        self.enc_detected_h = height as u32;
                        self.enc_detected_codec = Some("Image Sequence".to_string());
                        self.enc_detected_first_name = first_file.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string());
                        self.enc_detected_last_name = last_file.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string());

                        if let Some((tw, th, rgba)) = thumbnail_rgba {
                            let color_img = egui::ColorImage::from_rgba_unmultiplied(
                                [tw, th],
                                &rgba,
                            );
                            self.preview_texture = Some(ctx.load_texture(
                                "seq-thumb",
                                color_img,
                                TextureOptions::LINEAR,
                            ));
                        }

                        self.update_suggested_output_filename();
                        self.notify(
                            format!("Loaded image sequence: {} frames", count),
                            colors::ACCENT_CYAN,
                        );
                    }
                    MediaLoadResult::Failed { path, error } => {
                        let fname = path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        self.log(&format!("Failed loading {}: {}", fname, error));
                        self.notify(format!("Could not open {}: {}", fname, error), colors::ACCENT_RED);
                    }
                }
            }
            ctx.request_repaint();
        }

        // 5. Ambient Glow 60 FPS Repaint Tick
        // Request continuous 60 FPS repaints when loading, or when idle (no media loaded, not playing) and ambient glow is enabled.
        if self.is_loading_media || (!self.is_playing && self.reader.is_none() && self.show_ambient_glow) {
            ctx.request_repaint_after_secs(1.0 / 60.0);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let has_media = self.reader.is_some() || self.generic_player.is_some() || self.preview_texture.is_some() || self.still_image_info.is_some();

        // 0. FULL-WINDOW AMBIENT GLOW (When no media is loaded)
        // Seamlessly blankets the entire window background with zero borders or outlines.
        if !has_media && self.show_ambient_glow {
            let time = self.ambient_start.elapsed().as_secs_f32();
            paint_ambient_glow(&ctx.layer_painter(egui::LayerId::background()), ctx.viewport_rect(), time, self.is_drag_hovered);
        }

        // Lazily load embedded 256px neon cyan app icon
        if self.app_icon_texture.is_none() {
            if let Ok(img) = image::load_from_memory(include_bytes!("../../../../assets/icon_256.png")) {
                let rgba = img.to_rgba8();
                let color_img = egui::ColorImage::from_rgba_unmultiplied(
                    [rgba.width() as usize, rgba.height() as usize],
                    &rgba,
                );
                self.app_icon_texture = Some(ctx.load_texture("app-icon", color_img, TextureOptions::LINEAR));
            }
        }

        // ===================================================================
        // 1. TOP MENU BAR & QUICK ACTION BUTTONS
        // ===================================================================
        let menu_frame = if has_media {
            egui::Frame::new()
                .fill(Color32::from_rgba_premultiplied(18, 22, 32, 225))
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::symmetric(14, 6))
        } else {
            egui::Frame::new()
                .fill(Color32::from_rgba_premultiplied(12, 15, 22, 150))
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::symmetric(14, 7))
        };

        egui::Panel::top("top_menu_panel")
            .show_separator_line(false)
            .frame(menu_frame)
            .show(ui, |ui: &mut egui::Ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    // --- BRANDING ---
                    if let Some(ref icon) = self.app_icon_texture {
                        let (rect, _response) = ui.allocate_exact_size(Vec2::new(18.0, 18.0), egui::Sense::hover());
                        ui.painter().image(
                            icon.id(),
                            rect,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                        ui.add_space(2.0);
                    }
                    ui.label(RichText::new("HapLab").strong().size(14.0).color(Color32::WHITE));
                    ui.add_space(14.0);

                    // --- MENU: FILE ---
                    ui.menu_button("File", |ui: &mut egui::Ui| {
                        if ui.button("Open Media File... (Ctrl+O)").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                 .add_filter("All Media", &["mov", "mp4", "mkv", "avi", "webm", "m4v", "mxf", "ts", "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                                .add_filter("QuickTime HAP Videos (*.mov)", &["mov"])
                                .add_filter("Video Files (*.mp4, *.mkv, *.webm, *.mxf, *.avi)", &["mp4", "mkv", "avi", "webm", "m4v", "mxf", "ts"])
                                .add_filter("Image Sequences (*.png, *.tiff, *.jpg)", &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                                .pick_file()
                            {
                                self.open_media_file(path, &ctx);
                            }
                            ui.close();
                        }

                        if ui.button("Open Image Sequence Folder... (Ctrl+Shift+O)").clicked() {
                            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                self.open_media_file(folder, &ctx);
                            }
                            ui.close();
                        }

                        ui.separator();

                        let can_transcode = self.enc_input_path.is_some() || self.mov_path.is_some();
                        if ui.add_enabled(can_transcode, egui::Button::new("Transcode to HAP MOV... (Ctrl+E)")).clicked() {
                            self.show_transcode_window = true;
                            ui.close();
                        }

                        let can_export = self.reader.is_some();
                        if ui.add_enabled(can_export, egui::Button::new("Export Frame Sequence...")).clicked() {
                            self.show_export_panel = !self.show_export_panel;
                            ui.close();
                        }

                        ui.separator();

                        if ui.button("Set as Default Player for .mov").clicked() {
                            match register_mov_association() {
                                Ok(msg) => {
                                    self.log(&msg);
                                    self.notify(msg, colors::ACCENT_GREEN);
                                    let _ = open_windows_default_apps();
                                }
                                Err(err) => {
                                    self.log(&err);
                                    self.notify(err, colors::ACCENT_AMBER);
                                }
                            }
                            ui.close();
                        }

                        ui.separator();

                        let has_media = self.reader.is_some() || self.enc_input_path.is_some();
                        if ui.add_enabled(has_media, egui::Button::new("Close Media (Ctrl+W)")).clicked() {
                            self.close_media();
                            ui.close();
                        }

                        if ui.button("Exit (Alt+F4)").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });

                    // --- MENU: PLAYBACK ---
                    ui.menu_button("Playback", |ui: &mut egui::Ui| {
                        let is_loaded = self.has_video_loaded();
                        let play_label = if self.is_playing { "Pause (Space)" } else { "Play (Space)" };
                        if ui.add_enabled(is_loaded, egui::Button::new(play_label)).clicked() {
                            self.toggle_playback(&ctx);
                            ui.close();
                        }

                        ui.separator();

                        if ui.add_enabled(is_loaded, egui::Button::new("Step -1 Frame (Left)")).clicked() {
                            self.seek_to_frame(self.current_frame.saturating_sub(1), &ctx);
                            ui.close();
                        }

                        if ui.add_enabled(is_loaded, egui::Button::new("Step +1 Frame (Right)")).clicked() {
                            self.seek_to_frame(self.current_frame + 1, &ctx);
                            ui.close();
                        }

                        if ui.add_enabled(is_loaded, egui::Button::new("Step -10 Frames (Shift+Left)")).clicked() {
                            self.seek_to_frame(self.current_frame.saturating_sub(10), &ctx);
                            ui.close();
                        }

                        if ui.add_enabled(is_loaded, egui::Button::new("Step +10 Frames (Shift+Right)")).clicked() {
                            self.seek_to_frame(self.current_frame + 10, &ctx);
                            ui.close();
                        }

                        ui.separator();

                        if ui.add_enabled(is_loaded, egui::Button::new("Jump to Start (Home)")).clicked() {
                            self.seek_to_frame(0, &ctx);
                            ui.close();
                        }

                        if ui.add_enabled(is_loaded, egui::Button::new("Jump to End (End)")).clicked() {
                            self.seek_to_frame(self.video_total_frames().saturating_sub(1), &ctx);
                            ui.close();
                        }

                        ui.separator();

                        let loop_str = if self.loop_playback { "Loop: On (L)" } else { "Loop: Off (L)" };
                        if ui.button(loop_str).clicked() {
                            self.loop_playback = !self.loop_playback;
                            ui.close();
                        }
                    });

                    // --- MENU: VIEW ---
                    ui.menu_button("View", |ui: &mut egui::Ui| {
                        ui.label(RichText::new("Color Channels").color(colors::TEXT_MUTED).size(11.0));
                        if ui.selectable_value(&mut self.channel_mode, ChannelViewMode::Rgba, "RGBA Composite (1)").clicked() {
                            self.refresh_channel_view(&ctx);
                        }
                        if ui.selectable_value(&mut self.channel_mode, ChannelViewMode::RgbOpaque, "RGB Only (2)").clicked() {
                            self.refresh_channel_view(&ctx);
                        }
                        if ui.selectable_value(&mut self.channel_mode, ChannelViewMode::AlphaMatte, "Alpha Matte (3)").clicked() {
                            self.refresh_channel_view(&ctx);
                        }

                        ui.separator();
                        ui.label(RichText::new("Canvas Background").color(colors::TEXT_MUTED).size(11.0));
                        ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Checkerboard, "Checkerboard");
                        ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Dark, "Carbon Dark");
                        ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Light, "Studio Light");

                        ui.separator();
                        let hud_text = if self.show_hud_overlay { "Hide Telemetry HUD (I)" } else { "Show Telemetry HUD (I)" };
                        if ui.button(hud_text).clicked() {
                            self.show_hud_overlay = !self.show_hud_overlay;
                            ui.close();
                        }

                        ui.separator();
                        ui.checkbox(&mut self.show_ambient_glow, "Ambient Light Glow");
                    });

                    // --- MENU: TOOLS ---
                    ui.menu_button("Tools", |ui: &mut egui::Ui| {
                        if ui.button("Transcode to HAP MOV... (Ctrl+E)").clicked() {
                            self.show_transcode_window = true;
                            ui.close();
                        }

                        if ui.button("Hardware Benchmark Suite... (Ctrl+B)").clicked() {
                            self.show_benchmark_window = true;
                            ui.close();
                        }

                        let can_audit = self.reader.is_some();
                        if ui.add_enabled(can_audit, egui::Button::new("Stream Integrity & Fault Audit (Ctrl+T)")).clicked() {
                            self.show_audit_window = true;
                            ui.close();
                        }

                        if ui.button("System Diagnostics & GPU (Ctrl+D)").clicked() {
                            self.show_diagnostics_window = true;
                            ui.close();
                        }
                    });

                    // --- MENU: HELP ---
                    ui.menu_button("Help", |ui: &mut egui::Ui| {
                        if ui.button("Keyboard Shortcuts (F1)").clicked() {
                            self.show_shortcuts_window = true;
                            ui.close();
                        }
                        if ui.button("About HapLab").clicked() {
                            self.show_about_window = true;
                            ui.close();
                        }
                    });

                    // --- RIGHT-ALIGNED STATUS & QUICK BUTTONS ---
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                        // Quick action buttons
                        if ui.add(egui::Button::new(RichText::new("Diagnostics").size(12.0)).min_size(Vec2::new(82.0, 26.0))).clicked() {
                            self.show_diagnostics_window = !self.show_diagnostics_window;
                        }
                        if ui.add(egui::Button::new(RichText::new("Benchmark").size(12.0)).min_size(Vec2::new(80.0, 26.0))).clicked() {
                            self.show_benchmark_window = !self.show_benchmark_window;
                        }
                        if self.reader.is_some() {
                            if ui.add(egui::Button::new(RichText::new("Audit").size(12.0)).min_size(Vec2::new(55.0, 26.0))).clicked() {
                                self.show_audit_window = !self.show_audit_window;
                            }
                        }

                        let trans_btn = egui::Button::new(RichText::new("Transcode").strong().size(12.5).color(Color32::WHITE))
                            .min_size(Vec2::new(86.0, 26.0))
                            .fill(colors::ACCENT_BLUE)
                            .corner_radius(CornerRadius::same(5));
                        if ui.add(trans_btn).on_hover_text("Open Transcode & Ingest Panel (Ctrl+E)").clicked() {
                            self.show_transcode_window = !self.show_transcode_window;
                        }

                        ui.add_space(12.0);

                        // Active media status badges
                        if let Some(ref reader) = self.reader {
                            render_badge(ui, reader.format().name(), Color32::TRANSPARENT, colors::ACCENT_CYAN);
                            render_badge(ui, &format!("{}x{}", reader.width(), reader.height()), Color32::TRANSPARENT, colors::TEXT_PRIMARY);
                            render_badge(ui, &format!("{:.0} FPS", reader.fps()), Color32::TRANSPARENT, colors::TEXT_MUTED);
                        } else if let Some(ref player) = self.generic_player {
                            render_badge(ui, &player.codec.to_uppercase(), Color32::TRANSPARENT, colors::ACCENT_CYAN);
                            render_badge(ui, &format!("{}x{}", player.original_width, player.original_height), Color32::TRANSPARENT, colors::TEXT_PRIMARY);
                            render_badge(ui, &format!("{:.0} FPS", player.fps), Color32::TRANSPARENT, colors::TEXT_MUTED);
                        } else if let Some(ref img) = self.still_image_info {
                            render_badge(ui, "Still Image", Color32::TRANSPARENT, colors::ACCENT_CYAN);
                            render_badge(ui, &format!("{}x{}", img.width, img.height), Color32::TRANSPARENT, colors::TEXT_PRIMARY);
                        } else if let Some(ref codec) = self.enc_detected_codec {
                            render_badge(ui, codec, Color32::TRANSPARENT, colors::ACCENT_CYAN);
                            if self.enc_detected_w > 0 {
                                render_badge(ui, &format!("{}x{}", self.enc_detected_w, self.enc_detected_h), Color32::TRANSPARENT, colors::TEXT_PRIMARY);
                            }
                        }
                    });
                });
            });

        // ===================================================================
        // 2. BOTTOM TRANSPORT BAR & STATUS (PERMANENT)
        // DOCKED FIRMLY TO BOTTOM VIEWPORT WITH TRANSLUCENT FROSTED GLASS
        // ===================================================================
        let transport_frame = if has_media {
            egui::Frame::new()
                .fill(Color32::from_rgba_premultiplied(16, 20, 30, 215)) // Frosted translucent dark glass
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::symmetric(18, 8))
        } else {
            egui::Frame::new()
                .fill(Color32::from_rgba_premultiplied(14, 18, 26, 175)) // Translucent glass showing ambient glow
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::symmetric(18, 8))
        };

        egui::Panel::bottom("bottom_transport_panel")
            .resizable(false)
            .show_separator_line(false)
            .frame(transport_frame)
            .show(ui, |ui| {
                let has_reader = self.reader.is_some();
                let has_video = self.has_video_loaded();
                let is_still_image = self.still_image_info.is_some();
                let count = self.video_total_frames();
                let fps = self.video_fps();
                let max_frame = count.saturating_sub(1);

                // Scrubber slider across top of bottom panel
                let old_frame = self.current_frame;
                let avail_w = ui.available_width();
                let slider = egui::Slider::new(&mut self.current_frame, 0..=max_frame)
                    .show_value(false)
                    .trailing_fill(true);
                let slider_resp = ui.add_enabled_ui(has_video && count > 0, |ui| {
                    ui.spacing_mut().slider_width = avail_w;
                    ui.spacing_mut().interact_size.y = 18.0;
                    ui.add(slider)
                }).inner;

                if has_video {
                    if slider_resp.drag_started() {
                        if self.is_playing {
                            self.was_playing_before_scrub = true;
                            self.is_playing = false;
                            if let Some(ref mut player) = self.generic_player {
                                player.stop_playback();
                            }
                        }
                    }
                    if slider_resp.dragged() && old_frame != self.current_frame {
                        self.seek_to_frame(self.current_frame, &ctx);
                    }
                    if slider_resp.drag_stopped() {
                        if self.was_playing_before_scrub {
                            self.was_playing_before_scrub = false;
                            self.is_playing = true;
                            self.playback_start_instant = Instant::now();
                            self.playback_start_frame = self.current_frame;
                            self.last_frame_time = Instant::now();
                            self.playback_timer = Instant::now();
                            self.playback_frames_count = 0;
                            if let Some(ref mut player) = self.generic_player {
                                player.start_playback(self.current_frame);
                            }
                        } else {
                            self.seek_to_frame(self.current_frame, &ctx);
                        }
                    } else if !slider_resp.dragged() && old_frame != self.current_frame {
                        self.seek_to_frame(self.current_frame, &ctx);
                    }
                }

                ui.add_space(4.0);

                // Transport controls & details row
                ui.horizontal(|ui| {
                    // Left: SMPTE timecode and frame counter
                    let timecode_str = if has_video {
                        let cur_tc = format_smpte_timecode(self.current_frame, fps);
                        let total_tc = format_smpte_timecode(count.saturating_sub(1), fps);
                        format!("{} / {}", cur_tc, total_tc)
                    } else if is_still_image {
                        "00:00:00:01 / 00:00:00:01".to_string()
                    } else {
                        "00:00:00:00 / 00:00:00:00".to_string()
                    };
                    let tc_color = if has_video || is_still_image { colors::ACCENT_CYAN } else { colors::TEXT_FAINT };
                    ui.label(RichText::new(timecode_str).monospace().size(13.0).color(tc_color).strong());

                    if has_video {
                        let pct = if count > 0 { (self.current_frame as f32 / count as f32) * 100.0 } else { 0.0 };
                        ui.monospace(format!("{}/{} frames ({:.0}%)", self.current_frame + 1, count, pct));
                    } else if is_still_image {
                        ui.monospace("1/1 (Image)");
                    } else if count > 0 {
                        ui.monospace(format!("0/{} frames", count));
                    } else {
                        ui.monospace("0/0 (0%)");
                    }

                    ui.add_space(10.0);

                    // Center: Controls based on media state
                    if is_still_image {
                        // Still Image: Instant Conversion & Export
                        ui.label("Format:");
                        egui::ComboBox::from_id_salt("img_export_combo")
                            .selected_text(match self.still_image_export_format.as_str() {
                                "mov" => "HAP MOV (.mov)",
                                "png" => "PNG (.png)",
                                "jpg" => "JPEG (.jpg)",
                                "tiff" => "TIFF (.tiff)",
                                "webp" => "WebP (.webp)",
                                "bmp" => "BMP (.bmp)",
                                _ => "HAP MOV (.mov)",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.still_image_export_format, "mov".into(), "HAP MOV (.mov)");
                                ui.selectable_value(&mut self.still_image_export_format, "png".into(), "PNG (.png)");
                                ui.selectable_value(&mut self.still_image_export_format, "jpg".into(), "JPEG (.jpg)");
                                ui.selectable_value(&mut self.still_image_export_format, "tiff".into(), "TIFF (.tiff)");
                                ui.selectable_value(&mut self.still_image_export_format, "webp".into(), "WebP (.webp)");
                                ui.selectable_value(&mut self.still_image_export_format, "bmp".into(), "BMP (.bmp)");
                            });

                        let convert_btn = egui::Button::new(RichText::new("Convert / Save Image...").strong().color(Color32::WHITE))
                            .min_size(Vec2::new(170.0, 26.0))
                            .fill(colors::ACCENT_BLUE);
                        if ui.add(convert_btn).on_hover_text("Convert and save this image to HAP MOV or other image formats").clicked() {
                            if let Some(ref img_info) = self.still_image_info {
                                let ext = self.still_image_export_format.clone();
                                let default_name = img_info.path.file_stem()
                                    .map(|s| format!("{}_converted.{}", s.to_string_lossy(), ext))
                                    .unwrap_or_else(|| format!("converted.{}", ext));

                                let mut dialog = rfd::FileDialog::new().set_file_name(&default_name);
                                if let Some(parent) = img_info.path.parent() {
                                    dialog = dialog.set_directory(parent);
                                }

                                if let Some(dest) = dialog.pick_file() {
                                    if let Some(ref rgba) = self.raw_frame_cache {
                                        if ext == "mov" {
                                            match export_image_to_hap_mov(rgba, img_info.width, img_info.height, &dest, HapFormat::HapY, true) {
                                                Ok(()) => {
                                                    let fname = dest.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                                                    self.notify(format!("Saved HAP MOV: {}", fname), colors::ACCENT_GREEN);
                                                    self.open_media_file(dest, &ctx);
                                                }
                                                Err(err) => {
                                                    self.notify(format!("HAP export failed: {}", err), colors::ACCENT_RED);
                                                }
                                            }
                                        } else {
                                            match export_image_to_file(rgba, img_info.width, img_info.height, &dest) {
                                                Ok(()) => {
                                                    let fname = dest.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                                                    self.notify(format!("Saved image: {}", fname), colors::ACCENT_GREEN);
                                                }
                                                Err(err) => {
                                                    self.notify(format!("Image export failed: {}", err), colors::ACCENT_RED);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if has_video {
                        // Standard player transport buttons
                        if ui.add_enabled(count > 0, egui::Button::new("|<").min_size(Vec2::new(30.0, 26.0))).on_hover_text("Jump to Start (Home)").clicked() {
                            self.seek_to_frame(0, &ctx);
                        }
                        if ui.add_enabled(count > 0, egui::Button::new("-10").min_size(Vec2::new(36.0, 26.0))).on_hover_text("Step -10 Frames (Shift+Left)").clicked() {
                            self.seek_to_frame(self.current_frame.saturating_sub(10), &ctx);
                        }
                        if ui.add_enabled(count > 0, egui::Button::new("<").min_size(Vec2::new(30.0, 26.0))).on_hover_text("Step -1 Frame (Left)").clicked() {
                            self.seek_to_frame(self.current_frame.saturating_sub(1), &ctx);
                        }

                        let (btn_rect, btn_resp) = ui.allocate_exact_size(Vec2::new(52.0, 26.0), egui::Sense::click());
                        let btn_resp = btn_resp.on_hover_text(if self.is_playing { "Pause (Space)" } else { "Play (Space)" });
                        if btn_resp.clicked() {
                            self.toggle_playback(&ctx);
                        }

                        let bg_color = if self.is_playing {
                            if btn_resp.hovered() {
                                Color32::from_rgb(235, 165, 45)
                            } else {
                                colors::ACCENT_AMBER
                            }
                        } else {
                            if btn_resp.hovered() {
                                Color32::from_rgb(60, 155, 255)
                            } else {
                                colors::ACCENT_BLUE
                            }
                        };
                        ui.painter().rect_filled(btn_rect, CornerRadius::same(5), bg_color);

                        let center = btn_rect.center();
                        if self.is_playing {
                            let bar_w = 3.5;
                            let bar_h = 13.0;
                            let bar_gap = 4.0;
                            let left_bar = Rect::from_center_size(
                                egui::pos2(center.x - (bar_w + bar_gap) * 0.5, center.y),
                                Vec2::new(bar_w, bar_h),
                            );
                            let right_bar = Rect::from_center_size(
                                egui::pos2(center.x + (bar_w + bar_gap) * 0.5, center.y),
                                Vec2::new(bar_w, bar_h),
                            );
                            ui.painter().rect_filled(left_bar, CornerRadius::same(1), Color32::WHITE);
                            ui.painter().rect_filled(right_bar, CornerRadius::same(1), Color32::WHITE);
                        } else {
                            let tri_w = 11.0;
                            let tri_h = 13.0;
                            let ox = center.x + 1.0;
                            let oy = center.y;
                            let p0 = egui::pos2(ox - tri_w * 0.5, oy - tri_h * 0.5);
                            let p1 = egui::pos2(ox + tri_w * 0.5, oy);
                            let p2 = egui::pos2(ox - tri_w * 0.5, oy + tri_h * 0.5);
                            ui.painter().add(egui::Shape::convex_polygon(
                                vec![p0, p1, p2],
                                Color32::WHITE,
                                Stroke::NONE,
                            ));
                        }

                        if ui.add_enabled(count > 0, egui::Button::new(">").min_size(Vec2::new(30.0, 26.0))).on_hover_text("Step +1 Frame (Right)").clicked() {
                            self.seek_to_frame(self.current_frame + 1, &ctx);
                        }
                        if ui.add_enabled(count > 0, egui::Button::new("+10").min_size(Vec2::new(36.0, 26.0))).on_hover_text("Step +10 Frames (Shift+Right)").clicked() {
                            self.seek_to_frame(self.current_frame + 10, &ctx);
                        }
                        if ui.add_enabled(count > 0, egui::Button::new(">|").min_size(Vec2::new(30.0, 26.0))).on_hover_text("Jump to End (End)").clicked() {
                            self.seek_to_frame(count.saturating_sub(1), &ctx);
                        }

                        let (loop_rect, loop_resp) = ui.allocate_exact_size(Vec2::new(32.0, 26.0), egui::Sense::click());
                        if loop_resp.clicked() {
                            self.loop_playback = !self.loop_playback;
                            self.notify(
                                format!("Loop: {}", if self.loop_playback { "On" } else { "Off" }),
                                colors::ACCENT_CYAN,
                            );
                        }

                        let loop_resp = loop_resp.on_hover_text(if self.loop_playback {
                            "Looping: Enabled (L) - click to toggle"
                        } else {
                            "Looping: Disabled (L) - click to toggle"
                        });

                        let (loop_bg, loop_border, loop_icon) = if self.loop_playback {
                            if loop_resp.hovered() {
                                (Color32::from_rgb(55, 145, 255), Stroke::new(1.0, Color32::from_rgb(100, 185, 255)), Color32::WHITE)
                            } else {
                                (colors::ACCENT_BLUE, Stroke::new(1.0, Color32::from_rgb(70, 155, 245)), Color32::WHITE)
                            }
                        } else {
                            if loop_resp.hovered() {
                                (Color32::from_rgb(36, 42, 54), Stroke::new(1.0, Color32::from_rgb(52, 60, 78)), Color32::from_rgb(200, 210, 225))
                            } else {
                                (Color32::from_rgb(22, 26, 35), Stroke::new(1.0, Color32::from_rgb(38, 44, 58)), Color32::from_rgb(120, 130, 150))
                            }
                        };

                        ui.painter().rect_filled(loop_rect, CornerRadius::same(5), loop_bg);
                        ui.painter().rect_stroke(loop_rect, CornerRadius::same(5), loop_border, egui::StrokeKind::Inside);

                        let lc = loop_rect.center();
                        let l_stroke = Stroke::new(1.5, loop_icon);
                        let top_y = lc.y - 4.0;
                        let bot_y = lc.y + 4.0;

                        // Top horizontal segment and right arrowhead
                        ui.painter().line_segment([egui::pos2(lc.x - 3.5, top_y), egui::pos2(lc.x + 3.0, top_y)], l_stroke);
                        let p_tip = egui::pos2(lc.x + 6.0, top_y);
                        let p_top = egui::pos2(lc.x + 2.5, top_y - 3.0);
                        let p_bot = egui::pos2(lc.x + 2.5, top_y + 3.0);
                        ui.painter().add(egui::Shape::convex_polygon(vec![p_tip, p_top, p_bot], loop_icon, Stroke::NONE));

                        // Right bend curving down to bottom
                        ui.painter().line_segment([egui::pos2(lc.x + 2.0, top_y), egui::pos2(lc.x + 4.5, lc.y - 1.5)], l_stroke);
                        ui.painter().line_segment([egui::pos2(lc.x + 4.5, lc.y - 1.5), egui::pos2(lc.x + 4.5, lc.y + 1.5)], l_stroke);
                        ui.painter().line_segment([egui::pos2(lc.x + 4.5, lc.y + 1.5), egui::pos2(lc.x + 2.0, bot_y)], l_stroke);

                        // Bottom horizontal segment and left arrowhead
                        ui.painter().line_segment([egui::pos2(lc.x + 2.0, bot_y), egui::pos2(lc.x - 3.0, bot_y)], l_stroke);
                        let q_tip = egui::pos2(lc.x - 6.0, bot_y);
                        let q_top = egui::pos2(lc.x - 2.5, bot_y - 3.0);
                        let q_bot = egui::pos2(lc.x - 2.5, bot_y + 3.0);
                        ui.painter().add(egui::Shape::convex_polygon(vec![q_tip, q_top, q_bot], loop_icon, Stroke::NONE));

                        // Left bend curving up to top
                        ui.painter().line_segment([egui::pos2(lc.x - 2.0, bot_y), egui::pos2(lc.x - 4.5, lc.y + 1.5)], l_stroke);
                        ui.painter().line_segment([egui::pos2(lc.x - 4.5, lc.y + 1.5), egui::pos2(lc.x - 4.5, lc.y - 1.5)], l_stroke);
                        ui.painter().line_segment([egui::pos2(lc.x - 4.5, lc.y - 1.5), egui::pos2(lc.x - 2.0, top_y)], l_stroke);
                    } else if self.enc_input_path.is_some() {
                        // Background transcode in progress
                        if let Some(ref status) = self.enc_status {
                            match status {
                                WorkerProgress::Started { total } => {
                                    ui.spinner();
                                    ui.label(RichText::new(format!("Starting HAP stream ({} frames)...", total)).color(colors::ACCENT_CYAN));
                                }
                                WorkerProgress::Progress { current, total, fps, percent } => {
                                    ui.label(RichText::new(format!("Preparing HAP stream: {}/{} ({:.0}%)", current, total, percent)).color(colors::ACCENT_CYAN));
                                    ui.add(egui::ProgressBar::new(*percent / 100.0).show_percentage());
                                    ui.label(format!("{:.0} fps", fps));
                                    if let Some(ref cancel) = self.enc_cancel {
                                        if ui.button("Cancel").clicked() {
                                            cancel.store(true, Ordering::Relaxed);
                                        }
                                    }
                                }
                                _ => {
                                    if ui.button("Transcode Settings... (Ctrl+E)").clicked() {
                                        self.show_transcode_window = true;
                                    }
                                }
                            }
                        } else {
                            if ui.button("Transcode Settings... (Ctrl+E)").clicked() {
                                self.show_transcode_window = true;
                            }
                        }
                    } else {
                        let play_btn = egui::Button::new(RichText::new("Open Media...").strong().size(12.5))
                            .fill(colors::ACCENT_BLUE)
                            .corner_radius(CornerRadius::same(5));
                        if ui.add_sized([110.0, 26.0], play_btn).on_hover_text("Open Media to Play (Ctrl+O)").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("All Media", &["mov", "mp4", "mkv", "avi", "webm", "m4v", "mxf", "ts", "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                                .add_filter("QuickTime HAP Videos (*.mov)", &["mov"])
                                .add_filter("Video Files (*.mp4, *.mkv, *.webm, *.mxf, *.avi)", &["mp4", "mkv", "avi", "webm", "m4v", "mxf", "ts"])
                                .add_filter("Still Images (*.png, *.jpg, *.tiff, *.webp, *.bmp)", &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                                .pick_file()
                            {
                                self.open_media_file(path, &ctx);
                            }
                        }
                    }

                    // Right: Channel & Background & Exporter buttons
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if has_reader {
                            let exp_text = if self.show_export_panel { "Hide Exporter" } else { "Export Frames..." };
                            if ui.add(egui::Button::new(exp_text).min_size(Vec2::new(110.0, 26.0))).clicked() {
                                self.show_export_panel = !self.show_export_panel;
                            }
                        }

                        ui.add_space(4.0);

                        // Background quick picker
                        egui::ComboBox::from_id_salt("bottom_bg_combo")
                            .selected_text(match self.bg_mode {
                                BackgroundViewMode::Checkerboard => "Checker",
                                BackgroundViewMode::Dark => "Dark",
                                BackgroundViewMode::Light => "Light",
                            })
                            .width(68.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Checkerboard, "Checkerboard");
                                ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Dark, "Dark");
                                ui.selectable_value(&mut self.bg_mode, BackgroundViewMode::Light, "Light");
                            });

                        // Channel quick picker
                        let old_chan = self.channel_mode;
                        egui::ComboBox::from_id_salt("bottom_chan_combo")
                            .selected_text(match self.channel_mode {
                                ChannelViewMode::Rgba => "RGBA",
                                ChannelViewMode::AlphaMatte => "Alpha",
                                ChannelViewMode::RgbOpaque => "RGB",
                            })
                            .width(68.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.channel_mode, ChannelViewMode::Rgba, "RGBA (1)");
                                ui.selectable_value(&mut self.channel_mode, ChannelViewMode::RgbOpaque, "RGB (2)");
                                ui.selectable_value(&mut self.channel_mode, ChannelViewMode::AlphaMatte, "Alpha (3)");
                            });
                        if old_chan != self.channel_mode {
                            self.refresh_channel_view(&ctx);
                        }
                    });
                });

                // Collapsible Frame Exporter
                if has_reader && self.show_export_panel {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.strong("Frame Exporter:");
                        ui.label("Format:");
                        egui::ComboBox::from_id_salt("export_fmt_box")
                            .selected_text(&self.export_format)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_format, "png".into(), "PNG (.png)");
                                ui.selectable_value(&mut self.export_format, "jpg".into(), "JPEG (.jpg)");
                                ui.selectable_value(&mut self.export_format, "tiff".into(), "TIFF (.tiff)");
                                ui.selectable_value(&mut self.export_format, "webp".into(), "WebP (.webp)");
                                ui.selectable_value(&mut self.export_format, "bmp".into(), "BMP (.bmp)");
                            });

                        if ui.add(egui::Button::new(RichText::new("Choose Destination & Export").strong()).min_size(Vec2::new(190.0, 26.0))).clicked() {
                            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                if let Some(ref path) = self.mov_path {
                                    self.last_export_dir = Some(folder.clone());
                                    let cancel_flag = Arc::new(AtomicBool::new(false));
                                    let (tx, rx) = crossbeam_channel::unbounded();
                                    self.export_rx = Some(rx);
                                    self.export_cancel = Some(cancel_flag.clone());
                                    spawn_export_worker(path.clone(), folder, self.export_format.clone(), cancel_flag, tx);
                                }
                            }
                        }

                        if let Some(ref status) = self.export_status {
                            match status {
                                WorkerProgress::Progress { current, total, percent, .. } => {
                                    ui.label(format!("{}/{} frames", current, total));
                                    ui.add(egui::ProgressBar::new(*percent / 100.0).show_percentage());
                                    if let Some(ref cancel) = self.export_cancel {
                                        if ui.button("Cancel").clicked() {
                                            cancel.store(true, Ordering::Relaxed);
                                        }
                                    }
                                }
                                WorkerProgress::Finished { message } => {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(message).color(colors::ACCENT_GREEN));
                                        if let Some(ref dir) = self.last_export_dir {
                                            if ui.button("Reveal Folder").clicked() {
                                                reveal_in_file_manager(dir);
                                            }
                                        }
                                    });
                                }
                                WorkerProgress::Error(err) => {
                                    ui.label(RichText::new(err).color(colors::ACCENT_RED));
                                }
                                _ => {}
                            }
                        }
                    });
                }

                // Toast bar at bottom of transport
                if let Some((ref msg, time, color)) = self.toast {
                    if time.elapsed().as_secs_f32() < 4.0 {
                        ui.add_space(2.0);
                        ui.label(RichText::new(msg).color(color).strong().size(12.0));
                    } else {
                        self.toast = None;
                    }
                }
            });

        // ===================================================================
        // 3. CENTRAL MAIN CANVAS (PLAYER VIEWPORT)
        // OCCUPIES REMAINING VIEWPORT SPACE BETWEEN TOP AND BOTTOM PANELS
        // ===================================================================
        let canvas_frame = if has_media {
            egui::Frame::new()
                .fill(colors::BG_APP)
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::same(8))
        } else {
            egui::Frame::new()
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::ZERO)
        };

        egui::CentralPanel::default()
            .frame(canvas_frame)
            .show(ui, |ui: &mut egui::Ui| {
                let has_media = self.reader.is_some() || self.generic_player.is_some() || self.preview_texture.is_some() || self.still_image_info.is_some();

                if has_media {
                    let available_size = ui.available_size();
                    let (content_w, content_h) = if let Some(ref r) = self.reader {
                        (r.width() as f32, r.height() as f32)
                    } else if let Some(ref p) = self.generic_player {
                        (p.original_width as f32, p.original_height as f32)
                    } else if let Some(ref img) = self.still_image_info {
                        (img.width as f32, img.height as f32)
                    } else if self.enc_detected_w > 0 && self.enc_detected_h > 0 {
                        (self.enc_detected_w as f32, self.enc_detected_h as f32)
                    } else {
                        (16.0, 9.0)
                    };

                    let aspect = content_w / content_h.max(1.0);
                    let target_w = available_size.x;
                    let target_h = (target_w / aspect).min(available_size.y);
                    let final_w = (target_h * aspect).min(available_size.x);
                    let final_h = target_h;

                    let y_padding = ((available_size.y - final_h) * 0.5).max(0.0);
                    if y_padding > 0.0 {
                        ui.add_space(y_padding);
                    }

                    ui.vertical_centered(|ui| {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(final_w, final_h), egui::Sense::click());
                        if resp.double_clicked() {
                            self.toggle_fullscreen(ui.ctx());
                        }

                        // Paint Canvas Background
                        match self.bg_mode {
                            BackgroundViewMode::Checkerboard => {
                                paint_transparency_checkerboard(ui.painter(), rect);
                            }
                            BackgroundViewMode::Dark => {
                                ui.painter().rect_filled(rect, 0, Color32::from_rgb(12, 14, 18));
                            }
                            BackgroundViewMode::Light => {
                                ui.painter().rect_filled(rect, 0, Color32::from_rgb(180, 185, 195));
                            }
                        }

                        // Paint Media Frame
                        if let Some(ref texture) = self.preview_texture {
                            ui.painter().image(
                                texture.id(),
                                rect,
                                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }

                        // Floating Stream Telemetry HUD (Top-Left of canvas)
                        if self.show_hud_overlay && self.has_video_loaded() {
                            let hud_rect = Rect::from_min_size(
                                rect.min + Vec2::new(12.0, 12.0),
                                Vec2::new(260.0, 130.0),
                            );
                            ui.painter().rect_filled(
                                hud_rect,
                                CornerRadius::same(6),
                                Color32::from_rgba_premultiplied(16, 20, 28, 220),
                            );

                            let mut hud_ui = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(hud_rect.shrink(10.0))
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                            );

                            if let Some(ref r) = self.reader {
                                hud_ui.horizontal(|ui| {
                                    ui.strong(RichText::new(r.format().name()).color(colors::ACCENT_CYAN).size(13.0));
                                    ui.label(RichText::new(format!("{}x{}", r.width(), r.height())).color(colors::TEXT_PRIMARY).size(12.0));
                                });
                                hud_ui.add_space(4.0);
                                let instant_fps = if self.last_decode_ms > 0.0 { 1000.0 / self.last_decode_ms } else { 0.0 };
                                hud_ui.label(RichText::new(format!("Decode: {:.2} ms (~{:.0} FPS)", self.last_decode_ms, instant_fps)).color(colors::ACCENT_GREEN).size(11.5));

                                if let Some(ref s) = self.stream_summary {
                                    hud_ui.label(RichText::new(format!("Bitrate: {:.2} Mbps | Frame: {}", s.avg_bitrate_mbps, format_bytes(self.last_packet_bytes as u64))).color(colors::TEXT_MUTED).size(11.0));
                                    hud_ui.label(RichText::new(format!("Texture: {}", if self.gpu_supports_bc { "Hardware Direct BC Upload" } else { "CPU Software Fallback" })).color(colors::TEXT_FAINT).size(10.5));
                                }
                            } else if let Some(ref p) = self.generic_player {
                                hud_ui.horizontal(|ui| {
                                    ui.strong(RichText::new(p.codec.to_uppercase()).color(colors::ACCENT_CYAN).size(13.0));
                                    ui.label(RichText::new(format!("{}x{}", p.original_width, p.original_height)).color(colors::TEXT_PRIMARY).size(12.0));
                                });
                                hud_ui.add_space(4.0);
                                hud_ui.label(RichText::new(format!("Playback FPS: {:.1} (Target: {:.0})", self.playback_fps, p.fps)).color(colors::ACCENT_GREEN).size(11.5));
                                hud_ui.label(RichText::new("Decoder: Hardware Accelerated (Auto)").color(colors::ACCENT_CYAN).size(11.0));
                                hud_ui.label(RichText::new(format!("Render Target: {}x{}", p.play_width, p.play_height)).color(colors::TEXT_FAINT).size(10.5));
                            }
                        }
                    });
                } else if !self.is_loading_media {
                    // --- CLICK-ANYWHERE PLAYBACK AREA ---
                    let available_size = ui.available_size();
                    let (_canvas_rect, resp) = ui.allocate_exact_size(available_size, egui::Sense::click());

                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }

                    if resp.clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("All Media", &["mov", "mp4", "mkv", "avi", "webm", "m4v", "mxf", "ts", "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                            .add_filter("QuickTime HAP Videos (*.mov)", &["mov"])
                            .add_filter("Video Files (*.mp4, *.mkv, *.webm, *.mxf, *.avi)", &["mp4", "mkv", "avi", "webm", "m4v", "mxf", "ts"])
                            .add_filter("Still Images (*.png, *.jpg, *.tiff, *.webp, *.bmp)", &["png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                            .pick_file()
                        {
                            self.open_media_file(path, &ctx);
                        }
                    }
                }

                if self.is_loading_media {
                    let center = ui.max_rect().center();
                    let loading_rect = Rect::from_center_size(center, Vec2::new(260.0, 52.0));
                    ui.painter().rect_filled(
                        loading_rect,
                        CornerRadius::same(10),
                        Color32::from_rgba_premultiplied(18, 22, 32, 230),
                    );
                    let mut loading_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(loading_rect.shrink(10.0))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    loading_ui.spinner();
                    loading_ui.add_space(8.0);
                    loading_ui.label(
                        RichText::new(format!("Loading {}...", self.loading_filename))
                            .size(13.0)
                            .color(Color32::WHITE)
                            .strong(),
                    );
                }
            });

        // ===================================================================
        // 4. FLOATING STUDIO WINDOWS / DIALOGS
        // ===================================================================
        self.show_transcode_dialog(&ctx);
        self.show_benchmark_dialog(&ctx);
        self.show_audit_dialog(&ctx);
        self.show_diagnostics_dialog(&ctx);
        self.show_shortcuts_dialog(&ctx);
        self.show_about_dialog(&ctx);
    }
}

impl HapLabApp {
    // --- DIALOG: TRANSCODE TO HAP MOV ---
    fn show_transcode_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_transcode_window {
            return;
        }

        let mut open = self.show_transcode_window;
        egui::Window::new("Transcode to HAP MOV")
            .open(&mut open)
            .default_width(650.0)
            .default_height(550.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Source Media Card
                    let input_frame = egui::Frame::new()
                        .fill(colors::BG_CARD)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(egui::Margin::same(14));

                    input_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.strong(RichText::new("Source Media").size(14.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Select Video / File...").clicked() {
                                    if let Some(file) = rfd::FileDialog::new()
                                        .add_filter("All Supported Media", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mxf", "ts", "wmv", "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp"])
                                        .pick_file()
                                    {
                                        self.open_media_file(file, ctx);
                                    }
                                }
                                if ui.button("Choose Folder...").clicked() {
                                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                        self.open_media_file(folder, ctx);
                                    }
                                }
                            });
                        });

                        ui.add_space(6.0);

                        if let Some(ref path) = self.enc_input_path {
                            ui.horizontal(|ui| {
                                if let Some(ref thumb) = self.enc_thumbnail_texture {
                                    let (rect, _) = ui.allocate_exact_size(Vec2::new(72.0, 72.0), egui::Sense::hover());
                                    paint_transparency_checkerboard(ui.painter(), rect);
                                    ui.painter().image(thumb.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                                }
                                ui.vertical(|ui| {
                                    ui.monospace(format!("Path: {}", path.display()));
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        if let Some(ref codec) = self.enc_detected_codec {
                                            render_badge(ui, codec, colors::BG_ELEVATED, colors::ACCENT_CYAN);
                                        }
                                        render_badge(ui, &format!("{} frames", self.enc_detected_frames), colors::BG_ELEVATED, colors::TEXT_PRIMARY);
                                        render_badge(ui, &format!("{}x{}", self.enc_detected_w, self.enc_detected_h), colors::BG_ELEVATED, colors::TEXT_PRIMARY);
                                    });
                                });
                            });
                        } else {
                            ui.label(RichText::new("No input loaded. Select a file or folder above.").color(colors::TEXT_MUTED));
                        }
                    });

                    ui.add_space(10.0);

                    // Presets
                    ui.strong(RichText::new("Presets").size(14.0));
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
                            let bg = if is_sel { colors::ACCENT_BLUE } else { colors::BG_ELEVATED };
                            let btn = egui::Button::new(RichText::new(preset.name()).size(13.0).color(if is_sel { Color32::WHITE } else { colors::TEXT_MUTED }).strong())
                                .min_size(Vec2::new(100.0, 32.0))
                                .fill(bg)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(5));
                            if ui.add(btn).clicked() {
                                self.apply_preset(preset);
                            }
                        }
                    });

                    ui.add_space(4.0);
                    ui.label(RichText::new(self.enc_preset.description()).color(colors::TEXT_MUTED).size(11.5));
                    ui.add_space(10.0);

                    // Codec Settings Grid
                    egui::Grid::new("transcode_params_grid").spacing([24.0, 10.0]).show(ui, |ui| {
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
                            for f in [24.0, 25.0, 30.0, 60.0] {
                                if ui.button(format!("{:.0}", f)).clicked() {
                                    self.enc_fps = f;
                                }
                            }
                        });
                        ui.end_row();

                        ui.label(RichText::new("Chunks & Compression:").color(colors::TEXT_MUTED));
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
                            ui.checkbox(&mut self.enc_snappy, "Snappy Compression");
                        });
                        ui.end_row();

                        ui.label(RichText::new("Color Levels:").color(colors::TEXT_MUTED));
                        egui::ComboBox::from_id_salt("color_range_combo")
                            .selected_text(match self.enc_color_range {
                                ColorRange::Full => "Full Range (0-255 PC / Graphics)",
                                ColorRange::Limited => "Limited Range (16-235 Studio Video -> Expand)",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.enc_color_range, ColorRange::Full, "Full Range (0-255 PC / Graphics / Unreal)");
                                ui.selectable_value(&mut self.enc_color_range, ColorRange::Limited, "Limited Range (16-235 Studio Video -> Expand to 0-255)");
                            });
                        ui.end_row();

                        ui.label(RichText::new("Quality Preset:").color(colors::TEXT_MUTED));
                        egui::ComboBox::from_id_salt("quality_combo")
                            .selected_text(match self.enc_quality {
                                QualityPreset::Production => "Production Master (ClusterFit, Best Quality)",
                                QualityPreset::Draft => "Draft / Rush (RangeFit, ~3x Faster)",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.enc_quality, QualityPreset::Production, "Production Master (ClusterFit, Optimal RMS error)");
                                ui.selectable_value(&mut self.enc_quality, QualityPreset::Draft, "Draft / Rush (RangeFit, Real-time fast ingest)");
                            });
                        ui.end_row();

                        ui.label(RichText::new("Output Destination:").color(colors::TEXT_MUTED));
                        ui.horizontal(|ui| {
                            if let Some(ref out) = self.enc_output_path {
                                ui.monospace(format!("{}", out.display()));
                            } else {
                                ui.label(RichText::new("Not set").color(colors::TEXT_FAINT));
                            }
                            if ui.button("Change...").clicked() {
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

                    ui.add_space(14.0);

                    // Actions & Progress Bar
                    let can_start = self.enc_input_path.is_some()
                        && self.enc_output_path.is_some()
                        && self.enc_detected_frames > 0
                        && self.enc_rx.is_none();

                    ui.horizontal(|ui| {
                        let encode_btn = egui::Button::new(RichText::new("Start Transcoding").size(14.0).strong())
                            .min_size(Vec2::new(160.0, 36.0))
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
                                    video_dimensions: if self.enc_detected_w > 0 && self.enc_detected_h > 0 {
                                        Some((self.enc_detected_w as usize, self.enc_detected_h as usize))
                                    } else {
                                        None
                                    },
                                    total_frames: if self.enc_detected_frames > 0 {
                                        Some(self.enc_detected_frames)
                                    } else {
                                        None
                                    },
                                };

                                self.log(&format!("Started transcode: {:?} -> {:?}", in_p, out_p));
                                spawn_encode_worker(cfg, cancel_flag, tx);
                            }
                        }

                        if self.enc_rx.is_some() {
                            let cancel_btn = egui::Button::new(RichText::new("Cancel").size(14.0).color(colors::ACCENT_RED))
                                .min_size(Vec2::new(90.0, 36.0));
                            if ui.add(cancel_btn).clicked() {
                                if let Some(ref cancel) = self.enc_cancel {
                                    cancel.store(true, Ordering::Relaxed);
                                }
                            }
                        }
                    });

                    // Live Progress Bar
                    if let Some(ref status) = self.enc_status {
                        ui.add_space(8.0);
                        match status {
                            WorkerProgress::Started { total } => {
                                ui.label(format!("Starting transcode: {} frames...", total));
                            }
                            WorkerProgress::Progress { current, total, fps, percent } => {
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(format!("Transcoding: {}/{} frames ({:.1}%)", current, total, percent));
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            ui.monospace(format!("{:.1} FPS", fps));
                                        });
                                    });
                                    ui.add(egui::ProgressBar::new(*percent / 100.0).show_percentage());
                                });
                            }
                            WorkerProgress::Finished { message } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(message).color(colors::ACCENT_GREEN).strong());
                                    if let Some(ref out_mov) = self.enc_last_successful_mov {
                                        if ui.button("Reveal in File Manager").clicked() {
                                            reveal_in_file_manager(out_mov);
                                        }
                                    }
                                });
                            }
                            WorkerProgress::Error(err) => {
                                ui.label(RichText::new(format!("Error: {}", err)).color(colors::ACCENT_RED).strong());
                            }
                        }
                    }
                });
            });
        self.show_transcode_window = open;
    }

    // --- DIALOG: HARDWARE BENCHMARK ---
    fn show_benchmark_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_benchmark_window {
            return;
        }

        let mut open = self.show_benchmark_window;
        egui::Window::new("Hardware Benchmark Suite")
            .open(&mut open)
            .default_width(760.0)
            .default_height(500.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let is_busy = self.bench_is_running;
                        if is_busy {
                            let cancel_btn = egui::Button::new(RichText::new("Cancel Benchmark").color(colors::ACCENT_RED).strong())
                                .min_size(Vec2::new(140.0, 32.0));
                            if ui.add(cancel_btn).clicked() {
                                if let Some(ref cancel) = self.bench_cancel {
                                    cancel.store(true, Ordering::Relaxed);
                                }
                            }
                        } else {
                            let run_btn = egui::Button::new(RichText::new("Run Full Benchmark").strong())
                                .min_size(Vec2::new(160.0, 32.0))
                                .fill(colors::ACCENT_BLUE)
                                .corner_radius(CornerRadius::same(5));
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
                            if ui.button("Clear Results").clicked() {
                                self.bench_scores.clear();
                            }
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            render_badge(ui, &format!("{} Threads", rayon::current_num_threads()), colors::BG_ELEVATED, colors::ACCENT_CYAN);
                            render_badge(ui, &self.gpu_adapter_name, colors::BG_ELEVATED, colors::TEXT_PRIMARY);
                        });
                    });

                    // Live progress meter
                    if self.bench_is_running {
                        ui.add_space(8.0);
                        if let Some(ref test_name) = self.bench_current_test {
                            ui.label(RichText::new(format!("Running: {}", test_name)).color(colors::ACCENT_CYAN).strong());
                            let pct = if self.bench_total_steps > 0 {
                                (self.bench_current_step as f32 / self.bench_total_steps as f32).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            ui.add(egui::ProgressBar::new(pct).text(format!("{:.1} FPS", self.bench_current_fps)));
                        }
                    }

                    ui.add_space(10.0);

                    // Scorecard table
                    if !self.bench_scores.is_empty() {
                        egui::ScrollArea::horizontal().show(ui, |ui| {
                            egui::Grid::new("bench_scores_modal_grid")
                                .striped(true)
                                .spacing([18.0, 8.0])
                                .min_col_width(70.0)
                                .show(ui, |ui| {
                                    ui.label(RichText::new("Test").strong().color(colors::TEXT_PRIMARY));
                                    ui.label(RichText::new("Resolution").strong().color(colors::TEXT_PRIMARY));
                                    ui.label(RichText::new("Throughput").strong().color(colors::TEXT_PRIMARY));
                                    ui.label(RichText::new("Latency").strong().color(colors::TEXT_PRIMARY));
                                    ui.label(RichText::new("Bandwidth").strong().color(colors::TEXT_PRIMARY));
                                    ui.label(RichText::new("Capability Rating").strong().color(colors::TEXT_PRIMARY));
                                    ui.end_row();

                                    for score in &self.bench_scores {
                                        ui.label(RichText::new(&score.test_name).strong());
                                        ui.label(RichText::new(&score.resolution).color(colors::TEXT_MUTED));
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
                        });
                    } else if !self.bench_is_running {
                        ui.vertical_centered(|ui| {
                            ui.add_space(20.0);
                            ui.label(RichText::new("Click 'Run Full Benchmark' to evaluate CPU & GPU throughput.").color(colors::TEXT_MUTED));
                            ui.add_space(20.0);
                        });
                    }

                    ui.add_space(14.0);
                    ui.label(RichText::new("Guidelines: 1080p60 requires frame latency <= 16.6 ms. 4K60 requires latency <= 16.6 ms. Hap Q and Hap R achieve 300-800 FPS on modern multi-core CPUs.").color(colors::TEXT_FAINT).size(11.0));
                });
            });
        self.show_benchmark_window = open;
    }

    // --- DIALOG: STREAM INTEGRITY & FAULT AUDIT ---
    fn show_audit_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_audit_window {
            return;
        }

        let mut open = self.show_audit_window;
        let mut trigger_audit = false;
        egui::Window::new("Stream Health & Integrity Audit")
            .open(&mut open)
            .default_width(700.0)
            .default_height(480.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.reader.is_some() {
                        ui.horizontal(|ui| {
                            let audit_text = if self.is_auditing { "Auditing..." } else { "Run Deep Frame Audit" };
                            if ui.button(RichText::new(audit_text).strong()).clicked() {
                                trigger_audit = true;
                            }

                            if let Some(ref audit) = self.stream_audit {
                                if audit.has_critical_errors() {
                                    render_badge(ui, "CRITICAL FAULTS", Color32::from_rgb(45, 20, 20), colors::ACCENT_RED);
                                } else if audit.has_warnings() {
                                    render_badge(ui, "WARNINGS PRESENT", Color32::from_rgb(45, 38, 15), colors::ACCENT_AMBER);
                                } else {
                                    render_badge(ui, "100% SPEC COMPLIANT", Color32::from_rgb(16, 40, 25), colors::ACCENT_GREEN);
                                }
                            }
                        });

                        ui.add_space(10.0);

                        if let Some(ref audit) = self.stream_audit {
                            ui.label(RichText::new(format!("Audit Report ({} frames sampled)", audit.frames_scanned)).color(colors::TEXT_PRIMARY));
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
                                        ui.label(RichText::new(format!("-> Recommendation: {}", rec)).color(colors::ACCENT_CYAN));
                                    });
                                }
                                ui.add_space(4.0);
                            }
                        } else {
                            ui.label(RichText::new("Click 'Run Deep Frame Audit' to inspect every atom, chunk header, and compression packet.").color(colors::TEXT_MUTED));
                        }
                    } else {
                        ui.label(RichText::new("No HAP video file is currently loaded in the player.").color(colors::TEXT_MUTED));
                    }
                });
            });

        if trigger_audit {
            if let Some(ref mut reader) = self.reader {
                self.is_auditing = true;
                let audit = audit_hap_stream(reader, 120);
                self.stream_audit = Some(audit);
                self.is_auditing = false;
                self.log("Stream health audit completed.");
                self.notify("Stream compliance audit complete.", colors::ACCENT_GREEN);
            }
        }

        self.show_audit_window = open;
    }

    // --- DIALOG: SYSTEM DIAGNOSTICS & GPU ---
    fn show_diagnostics_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_diagnostics_window {
            return;
        }

        let mut open = self.show_diagnostics_window;
        egui::Window::new("System Diagnostics & GPU Telemetry")
            .open(&mut open)
            .default_width(720.0)
            .default_height(480.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.strong(RichText::new("Hardware Graphics Acceleration").size(14.0));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        render_badge(ui, &format!("Adapter: {}", self.gpu_adapter_name), colors::BG_ELEVATED, colors::TEXT_PRIMARY);
                        render_badge(ui, &format!("Backend: {}", self.gpu_backend_name), colors::BG_ELEVATED, colors::ACCENT_CYAN);
                        if self.gpu_supports_bc {
                            render_badge(ui, "Direct VRAM BC Decompression", Color32::from_rgb(16, 40, 25), colors::ACCENT_GREEN);
                        } else {
                            render_badge(ui, "Software CPU Fallback", colors::BG_ELEVATED, colors::TEXT_MUTED);
                        }
                    });

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(10.0);

                    ui.horizontal(|ui| {
                        ui.strong(RichText::new("Activity Logs").size(14.0));
                        ui.add_space(10.0);
                        ui.add(egui::TextEdit::singleline(&mut self.log_search).hint_text("Search logs...").desired_width(200.0));
                        if ui.button("Clear Logs").clicked() {
                            self.system_logs.clear();
                        }
                    });

                    ui.add_space(6.0);

                    let log_frame = egui::Frame::new()
                        .fill(Color32::from_rgb(10, 12, 16))
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(egui::Margin::same(10));

                    log_frame.show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            let filter = self.log_search.to_lowercase();
                            for entry in &self.system_logs {
                                if filter.is_empty() || entry.to_lowercase().contains(&filter) {
                                    ui.monospace(RichText::new(entry).size(11.0).color(colors::TEXT_MUTED));
                                }
                            }
                        });
                    });
                });
            });
        self.show_diagnostics_window = open;
    }

    // --- DIALOG: KEYBOARD SHORTCUTS ---
    fn show_shortcuts_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_shortcuts_window {
            return;
        }

        let mut open = self.show_shortcuts_window;
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut open)
            .default_width(420.0)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("shortcuts_grid").spacing([24.0, 6.0]).show(ui, |ui| {
                    let mut row = |key: &str, desc: &str| {
                        ui.label(RichText::new(key).monospace().strong().color(colors::ACCENT_CYAN));
                        ui.label(desc);
                        ui.end_row();
                    };

                    row("Space", "Play / Pause Video");
                    row("Left / Right", "Step -1 / +1 Frame");
                    row("Shift + Left / Right", "Step -10 / +10 Frames");
                    row("Home / End", "Jump to First / Last Frame");
                    row("L", "Toggle Playback Looping");
                    row("1 / 2 / 3", "RGBA / RGB / Alpha Matte View");
                    row("I", "Toggle Telemetry HUD Overlay");
                    row("Ctrl + O", "Open Media File");
                    row("Ctrl + Shift + O", "Open Image Sequence Folder");
                    row("Ctrl + E", "Toggle Transcode & Ingest Panel");
                    row("Ctrl + B", "Toggle Hardware Benchmark Suite");
                    row("Ctrl + T", "Toggle Stream Fault Audit");
                    row("Ctrl + D", "Toggle System Diagnostics");
                    row("Ctrl + W", "Close Current Media");
                    row("F1", "Show / Hide Shortcuts Reference");
                });
            });
        self.show_shortcuts_window = open;
    }

    // --- DIALOG: ABOUT HAPLAB ---
    fn show_about_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_about_window {
            return;
        }

        let mut open = self.show_about_window;
        egui::Window::new("About HapLab")
            .open(&mut open)
            .default_width(400.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    if let Some(ref icon) = self.app_icon_texture {
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(48.0, 48.0), egui::Sense::hover());
                        ui.painter().image(icon.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        ui.add_space(8.0);
                    }
                    ui.heading(RichText::new("HapLab").size(20.0).strong().color(Color32::WHITE));
                    ui.label(RichText::new("Version 0.1.0").color(colors::TEXT_MUTED));
                    ui.add_space(6.0);
                    ui.label("Pure Rust HAP Video Codec Studio & Benchmarking Suite");
                    ui.add_space(10.0);
                    ui.label(RichText::new("Licensed under PolyForm Noncommercial License 1.0.0").size(11.0).color(colors::TEXT_FAINT));
                    ui.label(RichText::new("Copyright (c) 2026 Lee Brown. All rights reserved.").size(11.0).color(colors::TEXT_FAINT));
                    ui.add_space(12.0);
                });
            });
        self.show_about_window = open;
    }
}
