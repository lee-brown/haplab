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
    last_cursor_pos: Option<egui::Pos2>,
    last_cursor_activity: Instant,
    cursor_hidden: bool,
    last_fullscreen_toggle: Instant,
    playback_start_instant: Instant,
    playback_start_frame: usize,
    current_frame: usize,
    is_playing: bool,
    loop_playback: bool,
    playback_clock_synced: bool,
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
            last_cursor_pos: None,
            last_cursor_activity: Instant::now(),
            cursor_hidden: false,
            last_fullscreen_toggle: Instant::now(),
            playback_start_instant: Instant::now(),
            playback_start_frame: 0,
            current_frame: 0,
            is_playing: false,
            loop_playback: true,
            playback_clock_synced: false,
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

            gpu_adapter_name: "Hardware Adapter".to_string(),
            gpu_backend_name: "GPU".to_string(),
            gpu_supports_bc: false,
            system_logs: Vec::new(),
            log_search: String::new(),
            app_icon_texture: None,

            ambient_start: Instant::now(),
            show_ambient_glow: true,
            is_drag_hovered: false,
        };

        app.log("HapLab initialized in player-first studio layout.");
        app
    }
}

impl HapLabApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let mut app = Self::default();
        if let Some(ref rs) = cc.wgpu_render_state {
            let info = rs.adapter.get_info();
            app.gpu_adapter_name = info.name;
            app.gpu_backend_name = format!("{:?}", info.backend);
            app.gpu_supports_bc = rs
                .adapter
                .features()
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
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
        }
        app
    }
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
        if self.last_fullscreen_toggle.elapsed().as_millis() < 350 {
            return;
        }
        self.last_fullscreen_toggle = Instant::now();
        self.is_fullscreen = !self.is_fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
        self.last_cursor_activity = Instant::now();
        if !self.is_fullscreen && self.cursor_hidden {
            self.cursor_hidden = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        }
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
                self.playback_clock_synced = false;
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
                self.playback_clock_synced = false;
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
                            .stderr(std::process::Stdio::null());

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
                            cmd.args(&["-vf", &format!("scale={}:{}:flags=fast_bilinear,setsar=1", pw, ph)]);
                        } else {
                            cmd.args(&["-vf", "setsar=1"]);
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
            self.playback_clock_synced = false;
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

fn format_aspect_ratio(w: usize, h: usize) -> &'static str {
    if h == 0 || w == 0 {
        return "";
    }
    let ratio = w as f32 / h as f32;
    if (ratio - 16.0 / 9.0).abs() < 0.05 {
        "16:9"
    } else if (ratio - 4.0 / 3.0).abs() < 0.05 {
        "4:3"
    } else if (ratio - 21.0 / 9.0).abs() < 0.05 || (ratio - 2.39).abs() < 0.05 {
        "2.39:1"
    } else if (ratio - 1.0).abs() < 0.05 {
        "1:1"
    } else if (ratio - 9.0 / 16.0).abs() < 0.05 {
        "9:16"
    } else {
        ""
    }
}

fn handle_window_edge_resize(ctx: &egui::Context, is_maximized: bool, is_fullscreen: bool) {
    if is_maximized || is_fullscreen {
        return;
    }
    let screen_rect = ctx.viewport_rect();
    if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
        let border = 6.0;
        let on_left = pos.x >= screen_rect.min.x && pos.x <= screen_rect.min.x + border;
        let on_right = pos.x <= screen_rect.max.x && pos.x >= screen_rect.max.x - border;
        let on_top = pos.y >= screen_rect.min.y && pos.y <= screen_rect.min.y + border;
        let on_bottom = pos.y <= screen_rect.max.y && pos.y >= screen_rect.max.y - border;

        let resize_dir = match (on_left, on_right, on_top, on_bottom) {
            (true, _, true, _) => Some((egui::viewport::ResizeDirection::NorthWest, egui::CursorIcon::ResizeNorthWest)),
            (_, true, true, _) => Some((egui::viewport::ResizeDirection::NorthEast, egui::CursorIcon::ResizeNorthEast)),
            (true, _, _, true) => Some((egui::viewport::ResizeDirection::SouthWest, egui::CursorIcon::ResizeSouthWest)),
            (_, true, _, true) => Some((egui::viewport::ResizeDirection::SouthEast, egui::CursorIcon::ResizeSouthEast)),
            (true, _, _, _) => Some((egui::viewport::ResizeDirection::West, egui::CursorIcon::ResizeWest)),
            (_, true, _, _) => Some((egui::viewport::ResizeDirection::East, egui::CursorIcon::ResizeEast)),
            (_, _, true, _) => Some((egui::viewport::ResizeDirection::North, egui::CursorIcon::ResizeNorth)),
            (_, _, _, true) => Some((egui::viewport::ResizeDirection::South, egui::CursorIcon::ResizeSouth)),
            _ => None,
        };

        if let Some((dir, cursor)) = resize_dir {
            ctx.set_cursor_icon(cursor);
            if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
            }
        }
    }
}

impl eframe::App for HapLabApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_studio_theme(ctx);

        // Track cursor activity and movement for true fullscreen auto-hide
        let current_cursor = ctx.input(|i| i.pointer.latest_pos());
        let cursor_moved = match (current_cursor, self.last_cursor_pos) {
            (Some(p1), Some(p2)) => p1.distance(p2) > 2.0,
            (Some(_), None) => true,
            _ => false,
        };
        let user_input_active = cursor_moved
            || ctx.input(|i| {
                i.pointer.any_down()
                    || i.pointer.any_pressed()
                    || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
            });

        if user_input_active {
            if cursor_moved || ctx.input(|i| i.pointer.any_down() || i.pointer.any_pressed()) {
                self.last_cursor_pos = current_cursor;
            }
            self.last_cursor_activity = Instant::now();
            if self.is_fullscreen && self.cursor_hidden {
                self.cursor_hidden = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
            }
        }

        let fullscreen_ui_visible = !self.is_fullscreen || self.last_cursor_activity.elapsed().as_secs_f32() < 2.5;

        if self.is_fullscreen {
            if !fullscreen_ui_visible {
                if !self.cursor_hidden {
                    self.cursor_hidden = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
                }
            } else {
                let elapsed = self.last_cursor_activity.elapsed().as_secs_f32();
                let remaining = (2.5 - elapsed).max(0.02);
                ctx.request_repaint_after_secs(remaining);
            }
        } else if self.cursor_hidden {
            self.cursor_hidden = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        }

        let is_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        handle_window_edge_resize(ctx, is_maximized, self.is_fullscreen);

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
        // Note: Query egui_wants_keyboard_input outside ctx.input to avoid self-deadlock on Context RwLock
        let egui_wants_keyboard = ctx.egui_wants_keyboard_input();
        let mut open_file = false;
        let mut open_folder = false;
        let mut toggle_transcode = false;
        let mut toggle_bench = false;
        let mut toggle_audit = false;
        let mut toggle_diag = false;
        let mut close_media = false;
        let mut toggle_shortcuts = false;
        let mut toggle_hud = false;
        let mut toggle_fs = false;
        let mut toggle_play = false;
        let mut seek_delta: Option<isize> = None;
        let mut seek_abs: Option<usize> = None;
        let mut toggle_loop = false;
        let mut set_channel: Option<ChannelViewMode> = None;

        ctx.input(|i| {
            // Hotkeys with Ctrl / Command
            if i.modifiers.command {
                if i.key_pressed(egui::Key::O) {
                    if i.modifiers.shift {
                        open_folder = true;
                    } else {
                        open_file = true;
                    }
                }
                if i.key_pressed(egui::Key::E) { toggle_transcode = true; }
                if i.key_pressed(egui::Key::B) { toggle_bench = true; }
                if i.key_pressed(egui::Key::T) { toggle_audit = true; }
                if i.key_pressed(egui::Key::D) { toggle_diag = true; }
                if i.key_pressed(egui::Key::W) { close_media = true; }
            }

            // Function keys
            if i.key_pressed(egui::Key::F1) { toggle_shortcuts = true; }
            if i.key_pressed(egui::Key::I) { toggle_hud = true; }
            if i.key_pressed(egui::Key::F11) { toggle_fs = true; }
            if i.key_pressed(egui::Key::Escape) && self.is_fullscreen { toggle_fs = true; }

            if !egui_wants_keyboard {
                if i.key_pressed(egui::Key::Space) {
                    toggle_play = true;
                }
                if i.key_pressed(egui::Key::ArrowLeft) {
                    seek_delta = Some(if i.modifiers.shift { -10 } else { -1 });
                }
                if i.key_pressed(egui::Key::ArrowRight) {
                    seek_delta = Some(if i.modifiers.shift { 10 } else { 1 });
                }
                if i.key_pressed(egui::Key::Home) {
                    seek_abs = Some(0);
                }
                if i.key_pressed(egui::Key::End) {
                    seek_abs = Some(usize::MAX);
                }
                if i.key_pressed(egui::Key::L) {
                    toggle_loop = true;
                }
                if i.key_pressed(egui::Key::Num1) {
                    set_channel = Some(ChannelViewMode::Rgba);
                }
                if i.key_pressed(egui::Key::Num2) {
                    set_channel = Some(ChannelViewMode::RgbOpaque);
                }
                if i.key_pressed(egui::Key::Num3) {
                    set_channel = Some(ChannelViewMode::AlphaMatte);
                }
            }
        });

        if open_folder {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                self.open_media_file(folder, ctx);
            }
        }
        if open_file {
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
        if toggle_transcode { self.show_transcode_window = !self.show_transcode_window; }
        if toggle_bench { self.show_benchmark_window = !self.show_benchmark_window; }
        if toggle_audit { self.show_audit_window = !self.show_audit_window; }
        if toggle_diag { self.show_diagnostics_window = !self.show_diagnostics_window; }
        if close_media { self.close_media(); }
        if toggle_shortcuts { self.show_shortcuts_window = !self.show_shortcuts_window; }
        if toggle_hud { self.show_hud_overlay = !self.show_hud_overlay; }
        if toggle_fs { self.toggle_fullscreen(ctx); }
        if toggle_play && self.has_video_loaded() { self.toggle_playback(ctx); }
        if let Some(delta) = seek_delta {
            if self.has_video_loaded() {
                let frame_count = self.video_total_frames();
                if frame_count > 0 {
                    let target = if delta < 0 {
                        self.current_frame.saturating_sub((-delta) as usize)
                    } else {
                        (self.current_frame + delta as usize).min(frame_count.saturating_sub(1))
                    };
                    self.seek_to_frame(target, ctx);
                }
            }
        }
        if let Some(abs) = seek_abs {
            if self.has_video_loaded() {
                let frame_count = self.video_total_frames();
                if frame_count > 0 {
                    let target = abs.min(frame_count.saturating_sub(1));
                    self.seek_to_frame(target, ctx);
                }
            }
        }
        if toggle_loop {
            self.loop_playback = !self.loop_playback;
            self.notify(
                format!("Loop: {}", if self.loop_playback { "On" } else { "Off" }),
                colors::ACCENT_CYAN,
            );
        }
        if let Some(mode) = set_channel {
            self.channel_mode = mode;
            self.refresh_channel_view(ctx);
        }

        // 3. Playback Frame Clock Synchronization
        if self.is_playing {
            if let Some(ref reader) = self.reader {
                let fps = reader.fps().max(1.0);
                let count = reader.frame_count();
                let mut target_frame = if self.playback_clock_synced {
                    let elapsed_secs = self.playback_start_instant.elapsed().as_secs_f64();
                    self.playback_start_frame + (elapsed_secs * fps as f64).floor() as usize
                } else {
                    self.playback_start_instant = Instant::now();
                    self.playback_clock_synced = true;
                    self.playback_start_frame
                };

                if count > 0 {
                    if target_frame >= count {
                        if self.loop_playback {
                            self.playback_clock_synced = false;
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
                let mut target_frame = if self.playback_clock_synced {
                    let elapsed_secs = self.playback_start_instant.elapsed().as_secs_f64();
                    self.playback_start_frame + (elapsed_secs * fps as f64).floor() as usize
                } else {
                    self.playback_start_frame
                };

                if total > 0 && target_frame >= total {
                    if self.loop_playback {
                        self.playback_clock_synced = false;
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
                            if !self.playback_clock_synced {
                                // First decoded frame has arrived from the player thread.
                                // Synchronize wall clock right now so startup never fast-forwards.
                                self.playback_start_instant = Instant::now();
                                self.playback_start_frame = frame_idx;
                                self.playback_clock_synced = true;
                                target_frame = frame_idx;
                            }
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
                                self.playback_clock_synced = false;
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
                        self.playback_clock_synced = false;
                        self.playback_start_instant = Instant::now();
                        self.playback_start_frame = 0;
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
        let is_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let is_fullscreen = self.is_fullscreen;
        let fullscreen_ui_visible = !is_fullscreen || self.last_cursor_activity.elapsed().as_secs_f32() < 2.5;

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

        // Subtle 1px studio window border when windowed and not maximized
        if !is_fullscreen && !is_maximized {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("window_border")));
            painter.rect_stroke(
                ctx.viewport_rect(),
                0.0,
                Stroke::new(1.0, Color32::from_rgb(38, 38, 38)),
                egui::StrokeKind::Inside,
            );
        }

        // ===================================================================
        // 1. TOP STUDIO HEADER & BOTTOM TRANSPORT/METADATA BARS
        // REVEALED IN WINDOWED MODE OR WHEN CURSOR MOVES IN FULLSCREEN
        // ===================================================================
        if !is_fullscreen || fullscreen_ui_visible {
            let menu_frame = if has_media {
                egui::Frame::new()
                    .fill(Color32::from_rgba_premultiplied(12, 12, 12, 235))
                    .stroke(Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(14, 6))
            } else {
                egui::Frame::new()
                    .fill(Color32::from_rgba_premultiplied(12, 12, 12, 160))
                    .stroke(Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(14, 7))
            };

            egui::Panel::top("top_menu_panel")
                .show_separator_line(false)
                .frame(menu_frame)
                .show(ui, |ui: &mut egui::Ui| {
                    self.render_top_bar(ui, is_fullscreen, is_maximized);
                });

            // Thin metadata bar docked at the very bottom of the window
            let metadata_frame = egui::Frame::new()
                .fill(Color32::from_rgb(12, 12, 12))
                .stroke(Stroke::new(1.0, Color32::from_rgb(26, 26, 26)))
                .inner_margin(egui::Margin::symmetric(14, 4));

            egui::Panel::bottom("bottom_metadata_panel")
                .resizable(false)
                .show_separator_line(false)
                .frame(metadata_frame)
                .show(ui, |ui| {
                    self.render_bottom_metadata(ui);
                });

            // Transport controls bar docked directly above the metadata row
            let transport_frame = if has_media {
                egui::Frame::new()
                    .fill(Color32::from_rgba_premultiplied(12, 12, 12, 235))
                    .stroke(Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(18, 8))
            } else {
                egui::Frame::new()
                    .fill(Color32::from_rgba_premultiplied(12, 12, 12, 160))
                    .stroke(Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(18, 8))
            };

            egui::Panel::bottom("bottom_transport_panel")
                .resizable(false)
                .show_separator_line(false)
                .frame(transport_frame)
                .show(ui, |ui| {
                    self.render_bottom_transport(ui, &ctx);
                });
        }

        // ===================================================================
        // 3. CENTRAL MAIN CANVAS (PLAYER VIEWPORT)
        // OCCUPIES REMAINING VIEWPORT SPACE BETWEEN TOP AND BOTTOM PANELS
        // ===================================================================
        let canvas_frame = if is_fullscreen {
            egui::Frame::new()
                .fill(Color32::BLACK)
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::ZERO)
        } else if has_media {
            egui::Frame::new()
                .fill(colors::BG_APP)
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::same(6))
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

                    // Derive aspect ratio strictly from active texture if loaded,
                    // guaranteeing 100% distortion-free fit without any stretching.
                    let aspect = if let Some(ref tex) = self.preview_texture {
                        let sz = tex.size_vec2();
                        if sz.y > 0.0 {
                            sz.x / sz.y
                        } else {
                            content_w / content_h.max(1.0)
                        }
                    } else {
                        content_w / content_h.max(1.0)
                    };

                    let aspect = aspect.max(0.01);
                    let (final_w, final_h) = if available_size.x / aspect <= available_size.y {
                        (available_size.x, available_size.x / aspect)
                    } else {
                        (available_size.y * aspect, available_size.y)
                    };

                    let y_padding = ((available_size.y - final_h) * 0.5).max(0.0);
                    let x_padding = ((available_size.x - final_w) * 0.5).max(0.0);

                    // Allocate full available area for interaction (double-click anywhere in video/letterbox toggles fullscreen)
                    let (canvas_rect, canvas_resp) = ui.allocate_exact_size(available_size, egui::Sense::click());
                    if canvas_resp.double_clicked() {
                        self.toggle_fullscreen(&ctx);
                    }

                    let video_rect = Rect::from_min_size(
                        egui::pos2(canvas_rect.min.x + x_padding, canvas_rect.min.y + y_padding),
                        Vec2::new(final_w, final_h),
                    );

                    // Paint Canvas Background
                    if !is_fullscreen {
                        match self.bg_mode {
                            BackgroundViewMode::Checkerboard => {
                                paint_transparency_checkerboard(ui.painter(), video_rect);
                            }
                            BackgroundViewMode::Dark => {
                                ui.painter().rect_filled(video_rect, 0, Color32::from_rgb(12, 12, 12));
                            }
                            BackgroundViewMode::Light => {
                                ui.painter().rect_filled(video_rect, 0, Color32::from_rgb(180, 185, 195));
                            }
                        }
                    }

                    // Paint Media Frame
                    if let Some(ref texture) = self.preview_texture {
                        ui.painter().image(
                            texture.id(),
                            video_rect,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }

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
                    let max_width = (ui.max_rect().width() - 40.0).max(120.0);
                    let box_width = 420.0_f32.min(max_width);
                    let loading_rect = Rect::from_center_size(center, Vec2::new(box_width, 54.0));
                    ui.painter().rect_filled(
                        loading_rect,
                        CornerRadius::same(10),
                        Color32::from_rgba_premultiplied(16, 16, 16, 240),
                    );
                    ui.painter().rect_stroke(
                        loading_rect,
                        CornerRadius::same(10),
                        Stroke::new(1.0, colors::BORDER_SUBTLE),
                        egui::StrokeKind::Inside,
                    );
                    let hover_resp = ui.interact(loading_rect, ui.id().with("loading_overlay"), egui::Sense::hover());
                    if !self.loading_filename.is_empty() {
                        hover_resp.on_hover_text(&self.loading_filename);
                    }

                    let inner_rect = loading_rect.shrink2(Vec2::new(14.0, 10.0));
                    let mut loading_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(inner_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    loading_ui.set_clip_rect(inner_rect);
                    loading_ui.spinner();
                    loading_ui.add_space(10.0);

                    let display_name = if self.loading_filename.is_empty() {
                        "media".to_string()
                    } else {
                        self.loading_filename.clone()
                    };
                    loading_ui.add(
                        egui::Label::new(
                            RichText::new(format!("Loading {}...", display_name))
                                .size(13.0)
                                .color(colors::TEXT_PRIMARY)
                                .strong(),
                        )
                        .truncate(),
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
    fn render_top_bar(&mut self, ui: &mut egui::Ui, is_fullscreen: bool, is_maximized: bool) {
        let ctx = ui.ctx().clone();
        let has_media = self.reader.is_some() || self.enc_input_path.is_some() || self.generic_player.is_some() || self.still_image_info.is_some() || self.preview_texture.is_some();

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
                        ui.add_space(3.0);
                    }
                    let title_resp = ui.add(
                        egui::Label::new(RichText::new("HapLab").strong().size(14.0).color(Color32::WHITE))
                            .sense(egui::Sense::click_and_drag()),
                    );
                    if title_resp.drag_started_by(egui::PointerButton::Primary) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if title_resp.double_clicked() {
                        if is_fullscreen {
                            self.toggle_fullscreen(&ctx);
                        } else {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
                        }
                    }
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

                        ui.separator();
                        let fs_label = if self.is_fullscreen { "Exit Fullscreen (F11 / Esc)" } else { "Enter Fullscreen (F11)" };
                        if ui.button(fs_label).clicked() {
                            self.toggle_fullscreen(&ctx);
                            ui.close();
                        }
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

                    // --- RIGHT-ALIGNED STATUS, QUICK BUTTONS & WINDOW CONTROLS ---
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                        // 1. CUSTOM WINDOW CONTROLS (Far Right of Title Bar)
                        // Close button (turns crimson red on hover)
                        let close_size = Vec2::new(36.0, 24.0);
                        let (close_rect, close_resp) = ui.allocate_exact_size(close_size, egui::Sense::click());
                        if close_resp.hovered() {
                            ui.painter().rect_filled(close_rect, CornerRadius::same(3), colors::ACCENT_RED);
                        }
                        let close_color = if close_resp.hovered() { Color32::WHITE } else { colors::TEXT_MUTED };
                        let c_pos = close_rect.center();
                        ui.painter().line_segment([c_pos + Vec2::new(-4.0, -4.0), c_pos + Vec2::new(4.0, 4.0)], Stroke::new(1.3, close_color));
                        ui.painter().line_segment([c_pos + Vec2::new(-4.0, 4.0), c_pos + Vec2::new(4.0, -4.0)], Stroke::new(1.3, close_color));
                        let close_resp = close_resp.on_hover_text("Close (Alt+F4)");
                        if close_resp.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }

                        // Maximize / Restore / Exit Fullscreen button
                        let max_size = Vec2::new(32.0, 24.0);
                        let (max_rect, max_resp) = ui.allocate_exact_size(max_size, egui::Sense::click());
                        if max_resp.hovered() {
                            ui.painter().rect_filled(max_rect, CornerRadius::same(3), Color32::from_rgba_premultiplied(255, 255, 255, 22));
                        }
                        let max_color = if max_resp.hovered() { Color32::WHITE } else { colors::TEXT_MUTED };
                        let m_pos = max_rect.center();
                        if is_fullscreen || is_maximized {
                            // Restore icon (two overlapping squares)
                            let r1 = Rect::from_center_size(m_pos + Vec2::new(2.0, -2.0), Vec2::new(7.5, 7.5));
                            ui.painter().rect_stroke(r1, 0.0, Stroke::new(1.1, max_color), egui::StrokeKind::Inside);
                            let r2 = Rect::from_center_size(m_pos + Vec2::new(-2.0, 2.0), Vec2::new(7.5, 7.5));
                            ui.painter().rect_filled(r2, 0.0, Color32::from_rgb(16, 16, 16));
                            ui.painter().rect_stroke(r2, 0.0, Stroke::new(1.1, max_color), egui::StrokeKind::Inside);
                        } else {
                            // Maximize icon (single square)
                            let r = Rect::from_center_size(m_pos, Vec2::new(9.0, 9.0));
                            ui.painter().rect_stroke(r, 0.0, Stroke::new(1.2, max_color), egui::StrokeKind::Inside);
                        }
                        let max_tooltip = if is_fullscreen {
                            "Exit Fullscreen (Esc / F11)"
                        } else if is_maximized {
                            "Restore Window"
                        } else {
                            "Maximize Window"
                        };
                        let max_resp = max_resp.on_hover_text(max_tooltip);
                        if max_resp.clicked() {
                            if is_fullscreen {
                                self.toggle_fullscreen(&ctx);
                            } else {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
                            }
                        }

                        // Minimize button
                        let min_size = Vec2::new(32.0, 24.0);
                        let (min_rect, min_resp) = ui.allocate_exact_size(min_size, egui::Sense::click());
                        if min_resp.hovered() {
                            ui.painter().rect_filled(min_rect, CornerRadius::same(3), Color32::from_rgba_premultiplied(255, 255, 255, 22));
                        }
                        let min_color = if min_resp.hovered() { Color32::WHITE } else { colors::TEXT_MUTED };
                        let min_pos = min_rect.center();
                        ui.painter().line_segment([min_pos + Vec2::new(-4.5, 3.5), min_pos + Vec2::new(4.5, 3.5)], Stroke::new(1.3, min_color));
                        let min_resp = min_resp.on_hover_text("Minimize Window");
                        if min_resp.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }

                        ui.add_space(8.0);

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

                        let file_is_selected = has_media || self.enc_input_path.is_some();
                        let trans_btn = if file_is_selected {
                            egui::Button::new(RichText::new("Transcode").strong().size(12.5).color(Color32::WHITE))
                                .min_size(Vec2::new(86.0, 26.0))
                                .fill(colors::ACCENT_BLUE)
                                .corner_radius(CornerRadius::same(5))
                        } else {
                            egui::Button::new(RichText::new("Transcode").strong().size(12.5).color(colors::TEXT_FAINT))
                                .min_size(Vec2::new(86.0, 26.0))
                                .fill(colors::BG_CARD)
                                .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
                                .corner_radius(CornerRadius::same(5))
                        };

                        let trans_resp = ui.add_enabled(file_is_selected, trans_btn);
                        let trans_resp = if file_is_selected {
                            trans_resp.on_hover_text("Open Transcode & Ingest Panel (Ctrl+E)")
                        } else {
                            trans_resp.on_disabled_hover_text("Open or select a media file first to enable transcoding")
                        };
                        if trans_resp.clicked() {
                            self.show_transcode_window = !self.show_transcode_window;
                        }

                                                // Draggable middle spacer for moving window or toggling maximize (Clean, no badges or details)
                        let remaining_space = ui.available_size();
                        if remaining_space.x > 8.0 {
                            let (_drag_rect, drag_resp) = ui.allocate_exact_size(remaining_space, egui::Sense::click_and_drag());
                            if drag_resp.drag_started_by(egui::PointerButton::Primary) {
                                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                            }
                            if drag_resp.double_clicked() {
                                if is_fullscreen {
                                    self.toggle_fullscreen(&ctx);
                                } else {
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
                                }
                            }
                        }
                    });
                });
    }


    /// Renders a thin, detailed metadata strip at the bottom of the window.
    fn render_bottom_metadata(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(7.0, 0.0);

            if let Some((ref msg, time, color)) = self.toast {
                if time.elapsed().as_secs_f32() < 4.0 {
                    ui.label(RichText::new(msg).color(color).strong().size(11.5));
                    ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                }
            }

            if let Some(ref reader) = self.reader {
                // Filename
                if let Some(ref p) = self.mov_path {
                    if let Some(name) = p.file_name() {
                        ui.strong(RichText::new(name.to_string_lossy()).color(colors::TEXT_PRIMARY).size(11.5));
                        ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                    }
                }

                // Codec format
                ui.label(RichText::new(reader.format().name()).color(colors::ACCENT_CYAN).strong().size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));

                // Resolution & Aspect Ratio
                let ar = format_aspect_ratio(reader.width() as usize, reader.height() as usize);
                let res_text = if ar.is_empty() {
                    format!("{}x{}", reader.width(), reader.height())
                } else {
                    format!("{}x{} ({})", reader.width(), reader.height(), ar)
                };
                ui.label(RichText::new(res_text).color(colors::TEXT_PRIMARY).size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));

                // FPS
                ui.label(RichText::new(format!("{:.2} FPS", reader.fps())).color(colors::TEXT_MUTED).size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));

                // Frames & Duration
                let count = reader.frame_count();
                let tc = format_smpte_timecode(count.saturating_sub(1), reader.fps());
                ui.label(RichText::new(format!("{} frames ({})", count, tc)).color(colors::TEXT_MUTED).size(11.5));

                // Chunks
                if let Some(ref s) = self.stream_summary {
                    if s.chunk_count > 1 {
                        ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                        ui.label(RichText::new(format!("{} Chunks", s.chunk_count)).color(colors::TEXT_MUTED).size(11.0));
                    }
                }

                // Bitrate & packet
                if let Some(ref s) = self.stream_summary {
                    ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                    ui.label(RichText::new(format!("{:.1} Mbps | {}", s.avg_bitrate_mbps, format_bytes(self.last_packet_bytes as u64))).color(colors::TEXT_MUTED).size(11.0));
                }

                // Decode time (goes red if slower than required frame budget)
                let frame_budget_ms = if reader.fps() > 0.0 { 1000.0 / reader.fps() } else { 33.33 };
                let decode_color = if self.last_decode_ms > frame_budget_ms {
                    colors::ACCENT_RED
                } else {
                    colors::ACCENT_GREEN
                };
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                ui.label(RichText::new(format!("Decode: {:.2} ms", self.last_decode_ms)).color(decode_color).size(11.5));

                // Right-aligned hardware status
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.gpu_supports_bc {
                        ui.label(RichText::new("Direct VRAM BC Upload").color(colors::ACCENT_GREEN).size(11.0));
                    } else {
                        ui.label(RichText::new("CPU Software Fallback").color(colors::TEXT_FAINT).size(11.0));
                    }
                });
            } else if let Some(ref player) = self.generic_player {
                if let Some(ref p) = self.enc_input_path {
                    if let Some(name) = p.file_name() {
                        ui.strong(RichText::new(name.to_string_lossy()).color(colors::TEXT_PRIMARY).size(11.5));
                        ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                    }
                }

                ui.label(RichText::new(player.codec.to_uppercase()).color(colors::ACCENT_CYAN).strong().size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));

                let ar = format_aspect_ratio(player.original_width, player.original_height);
                let res_text = if ar.is_empty() {
                    format!("{}x{}", player.original_width, player.original_height)
                } else {
                    format!("{}x{} ({})", player.original_width, player.original_height, ar)
                };
                ui.label(RichText::new(res_text).color(colors::TEXT_PRIMARY).size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));

                ui.label(RichText::new(format!("{:.1} FPS", player.fps)).color(colors::TEXT_MUTED).size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));

                let count = player.total_frames;
                let tc = format_smpte_timecode(count.saturating_sub(1), player.fps);
                ui.label(RichText::new(format!("{} frames ({})", count, tc)).color(colors::TEXT_MUTED).size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));

                ui.label(RichText::new(format!("Playback: {:.1} FPS", self.playback_fps)).color(colors::ACCENT_GREEN).size(11.5));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Hardware Accelerated Decoder").color(colors::ACCENT_CYAN).size(11.0));
                });
            } else if let Some(ref img) = self.still_image_info {
                if let Some(name) = img.path.file_name() {
                    ui.strong(RichText::new(name.to_string_lossy()).color(colors::TEXT_PRIMARY).size(11.5));
                    ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                }
                ui.label(RichText::new("Still Image").color(colors::ACCENT_CYAN).strong().size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));

                let ar = format_aspect_ratio(img.width, img.height);
                let res_text = if ar.is_empty() {
                    format!("{}x{}", img.width, img.height)
                } else {
                    format!("{}x{} ({})", img.width, img.height, ar)
                };
                ui.label(RichText::new(res_text).color(colors::TEXT_PRIMARY).size(11.5));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Ready to convert or export").color(colors::TEXT_FAINT).size(11.0));
                });
            } else if let Some(ref p) = self.enc_input_path {
                if let Some(name) = p.file_name() {
                    ui.strong(RichText::new(name.to_string_lossy()).color(colors::TEXT_PRIMARY).size(11.5));
                    ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                }
                if let Some(ref codec) = self.enc_detected_codec {
                    ui.label(RichText::new(codec).color(colors::ACCENT_CYAN).strong().size(11.5));
                    ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                }
                if self.enc_detected_w > 0 {
                    ui.label(RichText::new(format!("{}x{}", self.enc_detected_w, self.enc_detected_h)).color(colors::TEXT_PRIMARY).size(11.5));
                    ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                }
                if self.enc_fps > 0.0 {
                    ui.label(RichText::new(format!("{:.1} FPS", self.enc_fps)).color(colors::TEXT_MUTED).size(11.5));
                }
            } else {
                ui.label(RichText::new("No media loaded").color(colors::TEXT_MUTED).size(11.5));
                ui.label(RichText::new("|").color(colors::TEXT_FAINT).size(11.0));
                ui.label(RichText::new("Drag & drop a video or image, or press Ctrl+O").color(colors::TEXT_FAINT).size(11.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("GPU: {} ({})", self.gpu_adapter_name, self.gpu_backend_name)).color(colors::TEXT_FAINT).size(10.5));
                });
            }
        });
    }

    fn render_bottom_transport(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
                let has_reader = self.reader.is_some();
                let has_video = self.has_video_loaded();
                let is_still_image = self.still_image_info.is_some();
                let count = self.video_total_frames();
                let fps = self.video_fps();

                // Scrubber slider across top of bottom panel with dark track groove
                let avail_w = ui.available_width();
                let scrub_size = Vec2::new(avail_w, 18.0);
                let (scrub_rect, scrub_resp) = ui.allocate_exact_size(
                    scrub_size,
                    if has_video && count > 0 {
                        egui::Sense::click_and_drag()
                    } else {
                        egui::Sense::hover()
                    },
                );

                if has_video && count > 0 {
                    if scrub_resp.hovered() || scrub_resp.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }

                let track_h = 6.0;
                let track_rect = Rect::from_min_max(
                    egui::pos2(scrub_rect.min.x, scrub_rect.center().y - track_h * 0.5),
                    egui::pos2(scrub_rect.max.x, scrub_rect.center().y + track_h * 0.5),
                );

                // 1. Dark Track Groove
                let track_bg = Color32::from_rgb(18, 18, 18);
                let track_stroke = Stroke::new(1.0, Color32::from_rgb(34, 34, 34));
                ui.painter().rect_filled(track_rect, CornerRadius::same(3), track_bg);
                ui.painter().rect_stroke(track_rect, CornerRadius::same(3), track_stroke, egui::StrokeKind::Inside);

                // 2. Trailing progress & Handle position
                let progress = if has_video && count > 1 {
                    (self.current_frame as f32 / (count - 1) as f32).clamp(0.0, 1.0)
                } else {
                    0.0 // When no video is selected, handle sits on the far left
                };

                let fill_w = progress * track_rect.width();
                if fill_w > 0.0 {
                    let fill_rect = Rect::from_min_max(
                        track_rect.min,
                        egui::pos2(track_rect.min.x + fill_w, track_rect.max.y),
                    );
                    ui.painter().rect_filled(fill_rect, CornerRadius::same(3), colors::ACCENT_RED);
                }

                // 3. Seek Handle ("seek thing")
                let knob_x = track_rect.min.x + progress * track_rect.width();
                let knob_center = egui::pos2(knob_x, track_rect.center().y);
                if has_video && count > 0 {
                    let is_active = scrub_resp.hovered() || scrub_resp.dragged();
                    let knob_radius = if is_active { 7.0 } else { 5.5 };
                    let knob_stroke = if is_active {
                        Stroke::new(1.5, colors::ACCENT_RED)
                    } else {
                        Stroke::new(1.0, Color32::from_rgb(200, 200, 200))
                    };
                    ui.painter().circle_filled(knob_center, knob_radius, Color32::WHITE);
                    ui.painter().circle_stroke(knob_center, knob_radius, knob_stroke);
                } else {
                    // Disabled knob positioned on the far left
                    let knob_radius = 4.5;
                    ui.painter().circle_filled(knob_center, knob_radius, Color32::from_rgb(70, 70, 70));
                    ui.painter().circle_stroke(knob_center, knob_radius, Stroke::new(1.0, Color32::from_rgb(45, 45, 45)));
                }

                // 4. Scrubbing interaction
                if has_video && count > 0 {
                    if scrub_resp.drag_started() {
                        if self.is_playing {
                            self.was_playing_before_scrub = true;
                            self.is_playing = false;
                            if let Some(ref mut player) = self.generic_player {
                                player.stop_playback();
                            }
                        }
                    }
                    if scrub_resp.clicked() || scrub_resp.dragged() {
                        if let Some(mouse_pos) = scrub_resp.interact_pointer_pos() {
                            let t = ((mouse_pos.x - track_rect.min.x) / track_rect.width()).clamp(0.0, 1.0);
                            let target_frame = (t * (count - 1) as f32).round() as usize;
                            if target_frame != self.current_frame {
                                self.current_frame = target_frame;
                                self.seek_to_frame(self.current_frame, &ctx);
                            }
                        }
                    }
                    if scrub_resp.drag_stopped() {
                        if self.was_playing_before_scrub {
                            self.was_playing_before_scrub = false;
                            self.is_playing = true;
                            self.playback_clock_synced = false;
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
                    }

                    if scrub_resp.hovered() {
                        if let Some(mouse_pos) = scrub_resp.hover_pos() {
                            let t = ((mouse_pos.x - track_rect.min.x) / track_rect.width()).clamp(0.0, 1.0);
                            let hover_frame = (t * (count - 1) as f32).round() as usize;
                            let hover_tc = format_smpte_timecode(hover_frame, fps);
                            scrub_resp.on_hover_text(format!("{} (Frame {})", hover_tc, hover_frame));
                        }
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
                        let btn_size = Vec2::new(30.0, 26.0);

                        // 1. Jump to Start (|<)
                        let (s_rect, s_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                        let s_resp = s_resp.on_hover_text("Jump to Start (Home)");
                        if s_resp.clicked() && count > 0 {
                            self.seek_to_frame(0, &ctx);
                        }
                        let s_bg = if s_resp.hovered() && count > 0 { colors::BG_CARD_HOVER } else { colors::BG_CARD };
                        let s_icon = if count > 0 { if s_resp.hovered() { Color32::WHITE } else { colors::TEXT_PRIMARY } } else { colors::TEXT_FAINT };
                        ui.painter().rect_filled(s_rect, CornerRadius::same(5), s_bg);
                        ui.painter().rect_stroke(s_rect, CornerRadius::same(5), Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
                        let sc = s_rect.center();
                        ui.painter().rect_filled(Rect::from_center_size(egui::pos2(sc.x - 4.5, sc.y), Vec2::new(2.0, 10.0)), CornerRadius::same(1), s_icon);
                        let sp0 = egui::pos2(sc.x - 3.5, sc.y);
                        let sp1 = egui::pos2(sc.x + 3.5, sc.y - 4.5);
                        let sp2 = egui::pos2(sc.x + 3.5, sc.y + 4.5);
                        ui.painter().add(egui::Shape::convex_polygon(vec![sp0, sp1, sp2], s_icon, Stroke::NONE));

                        // 2. Step -10 Frames (<< standard rewind double triangle)
                        let (r10_rect, r10_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                        let r10_resp = r10_resp.on_hover_text("Step -10 Frames (Shift+Left)");
                        if r10_resp.clicked() && count > 0 {
                            self.seek_to_frame(self.current_frame.saturating_sub(10), &ctx);
                        }
                        let r10_bg = if r10_resp.hovered() && count > 0 { colors::BG_CARD_HOVER } else { colors::BG_CARD };
                        let r10_icon = if count > 0 { if r10_resp.hovered() { Color32::WHITE } else { colors::TEXT_PRIMARY } } else { colors::TEXT_FAINT };
                        ui.painter().rect_filled(r10_rect, CornerRadius::same(5), r10_bg);
                        ui.painter().rect_stroke(r10_rect, CornerRadius::same(5), Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
                        let r10_c = r10_rect.center();
                        let a0 = egui::pos2(r10_c.x - 5.0, r10_c.y);
                        let a1 = egui::pos2(r10_c.x - 0.5, r10_c.y - 4.5);
                        let a2 = egui::pos2(r10_c.x - 0.5, r10_c.y + 4.5);
                        ui.painter().add(egui::Shape::convex_polygon(vec![a0, a1, a2], r10_icon, Stroke::NONE));
                        let b0 = egui::pos2(r10_c.x + 0.5, r10_c.y);
                        let b1 = egui::pos2(r10_c.x + 5.0, r10_c.y - 4.5);
                        let b2 = egui::pos2(r10_c.x + 5.0, r10_c.y + 4.5);
                        ui.painter().add(egui::Shape::convex_polygon(vec![b0, b1, b2], r10_icon, Stroke::NONE));

                        // 3. Step -1 Frame (< single left triangle)
                        let (r1_rect, r1_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                        let r1_resp = r1_resp.on_hover_text("Step -1 Frame (Left)");
                        if r1_resp.clicked() && count > 0 {
                            self.seek_to_frame(self.current_frame.saturating_sub(1), &ctx);
                        }
                        let r1_bg = if r1_resp.hovered() && count > 0 { colors::BG_CARD_HOVER } else { colors::BG_CARD };
                        let r1_icon = if count > 0 { if r1_resp.hovered() { Color32::WHITE } else { colors::TEXT_PRIMARY } } else { colors::TEXT_FAINT };
                        ui.painter().rect_filled(r1_rect, CornerRadius::same(5), r1_bg);
                        ui.painter().rect_stroke(r1_rect, CornerRadius::same(5), Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
                        let r1_c = r1_rect.center();
                        let c0 = egui::pos2(r1_c.x - 3.5, r1_c.y);
                        let c1 = egui::pos2(r1_c.x + 3.0, r1_c.y - 5.0);
                        let c2 = egui::pos2(r1_c.x + 3.0, r1_c.y + 5.0);
                        ui.painter().add(egui::Shape::convex_polygon(vec![c0, c1, c2], r1_icon, Stroke::NONE));

                        // 4. Play / Pause Button
                        let (btn_rect, btn_resp) = ui.allocate_exact_size(Vec2::new(52.0, 26.0), egui::Sense::click());
                        let btn_resp = btn_resp.on_hover_text(if self.is_playing { "Pause (Space)" } else { "Play (Space)" });
                        if btn_resp.clicked() {
                            self.toggle_playback(&ctx);
                        }

                        let bg_color = if btn_resp.hovered() {
                            Color32::from_rgb(245, 75, 95)
                        } else {
                            colors::ACCENT_RED
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

                        // 5. Step +1 Frame (> single right triangle)
                        let (f1_rect, f1_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                        let f1_resp = f1_resp.on_hover_text("Step +1 Frame (Right)");
                        if f1_resp.clicked() && count > 0 {
                            self.seek_to_frame(self.current_frame + 1, &ctx);
                        }
                        let f1_bg = if f1_resp.hovered() && count > 0 { colors::BG_CARD_HOVER } else { colors::BG_CARD };
                        let f1_icon = if count > 0 { if f1_resp.hovered() { Color32::WHITE } else { colors::TEXT_PRIMARY } } else { colors::TEXT_FAINT };
                        ui.painter().rect_filled(f1_rect, CornerRadius::same(5), f1_bg);
                        ui.painter().rect_stroke(f1_rect, CornerRadius::same(5), Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
                        let f1_c = f1_rect.center();
                        let d0 = egui::pos2(f1_c.x + 3.5, f1_c.y);
                        let d1 = egui::pos2(f1_c.x - 3.0, f1_c.y - 5.0);
                        let d2 = egui::pos2(f1_c.x - 3.0, f1_c.y + 5.0);
                        ui.painter().add(egui::Shape::convex_polygon(vec![d0, d1, d2], f1_icon, Stroke::NONE));

                        // 6. Step +10 Frames (>> standard fast-forward double triangle)
                        let (f10_rect, f10_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                        let f10_resp = f10_resp.on_hover_text("Step +10 Frames (Shift+Right)");
                        if f10_resp.clicked() && count > 0 {
                            self.seek_to_frame(self.current_frame + 10, &ctx);
                        }
                        let f10_bg = if f10_resp.hovered() && count > 0 { colors::BG_CARD_HOVER } else { colors::BG_CARD };
                        let f10_icon = if count > 0 { if f10_resp.hovered() { Color32::WHITE } else { colors::TEXT_PRIMARY } } else { colors::TEXT_FAINT };
                        ui.painter().rect_filled(f10_rect, CornerRadius::same(5), f10_bg);
                        ui.painter().rect_stroke(f10_rect, CornerRadius::same(5), Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
                        let f10_c = f10_rect.center();
                        let e0 = egui::pos2(f10_c.x - 0.5, f10_c.y);
                        let e1 = egui::pos2(f10_c.x - 5.0, f10_c.y - 4.5);
                        let e2 = egui::pos2(f10_c.x - 5.0, f10_c.y + 4.5);
                        ui.painter().add(egui::Shape::convex_polygon(vec![e0, e1, e2], f10_icon, Stroke::NONE));
                        let g0 = egui::pos2(f10_c.x + 5.0, f10_c.y);
                        let g1 = egui::pos2(f10_c.x + 0.5, f10_c.y - 4.5);
                        let g2 = egui::pos2(f10_c.x + 0.5, f10_c.y + 4.5);
                        ui.painter().add(egui::Shape::convex_polygon(vec![g0, g1, g2], f10_icon, Stroke::NONE));

                        // 7. Jump to End (>|)
                        let (e_rect, e_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                        let e_resp = e_resp.on_hover_text("Jump to End (End)");
                        if e_resp.clicked() && count > 0 {
                            self.seek_to_frame(count.saturating_sub(1), &ctx);
                        }
                        let e_bg = if e_resp.hovered() && count > 0 { colors::BG_CARD_HOVER } else { colors::BG_CARD };
                        let e_icon = if count > 0 { if e_resp.hovered() { Color32::WHITE } else { colors::TEXT_PRIMARY } } else { colors::TEXT_FAINT };
                        ui.painter().rect_filled(e_rect, CornerRadius::same(5), e_bg);
                        ui.painter().rect_stroke(e_rect, CornerRadius::same(5), Stroke::new(1.0, colors::BORDER_SUBTLE), egui::StrokeKind::Inside);
                        let ec = e_rect.center();
                        let h0 = egui::pos2(ec.x + 3.5, ec.y);
                        let h1 = egui::pos2(ec.x - 3.5, ec.y - 4.5);
                        let h2 = egui::pos2(ec.x - 3.5, ec.y + 4.5);
                        ui.painter().add(egui::Shape::convex_polygon(vec![h0, h1, h2], e_icon, Stroke::NONE));
                        ui.painter().rect_filled(Rect::from_center_size(egui::pos2(ec.x + 4.5, ec.y), Vec2::new(2.0, 10.0)), CornerRadius::same(1), e_icon);

                        // 8. Loop Button with Highlight
                        let (loop_rect, loop_resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
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
                                (Color32::from_rgb(245, 75, 95), Stroke::new(1.0, Color32::from_rgb(255, 110, 130)), Color32::WHITE)
                            } else {
                                (colors::ACCENT_RED, Stroke::new(1.0, colors::ACCENT_RED), Color32::WHITE)
                            }
                        } else {
                            if loop_resp.hovered() {
                                (colors::BG_CARD_HOVER, Stroke::new(1.0, colors::BORDER_SUBTLE), Color32::WHITE)
                            } else {
                                (colors::BG_CARD, Stroke::new(1.0, colors::BORDER_SUBTLE), colors::TEXT_MUTED)
                            }
                        };

                        ui.painter().rect_filled(loop_rect, CornerRadius::same(5), loop_bg);
                        ui.painter().rect_stroke(loop_rect, CornerRadius::same(5), loop_border, egui::StrokeKind::Inside);

                        let lc = loop_rect.center();
                        let l_stroke = Stroke::new(1.6, loop_icon);

                        // Upper branch: vertical up, rounded turn right, horizontal right, arrow pointing right
                        let x_l = lc.x - 5.5;
                        let x_r = lc.x + 5.5;
                        let y_t = lc.y - 4.5;
                        let r = 2.5;

                        ui.painter().line_segment([egui::pos2(x_l, lc.y - 0.5), egui::pos2(x_l, y_t + r)], l_stroke);
                        ui.painter().line_segment([egui::pos2(x_l, y_t + r), egui::pos2(x_l + r, y_t)], l_stroke);
                        ui.painter().line_segment([egui::pos2(x_l + r, y_t), egui::pos2(x_r, y_t)], l_stroke);
                        // Upper arrow (pointing right)
                        ui.painter().line_segment([egui::pos2(x_r - 3.5, y_t - 3.0), egui::pos2(x_r, y_t)], l_stroke);
                        ui.painter().line_segment([egui::pos2(x_r, y_t), egui::pos2(x_r - 3.5, y_t + 3.0)], l_stroke);

                        // Lower branch: vertical down, rounded turn left, horizontal left, arrow pointing left
                        let y_b = lc.y + 4.5;
                        ui.painter().line_segment([egui::pos2(x_r, lc.y + 0.5), egui::pos2(x_r, y_b - r)], l_stroke);
                        ui.painter().line_segment([egui::pos2(x_r, y_b - r), egui::pos2(x_r - r, y_b)], l_stroke);
                        ui.painter().line_segment([egui::pos2(x_r - r, y_b), egui::pos2(x_l, y_b)], l_stroke);
                        // Lower arrow (pointing left)
                        ui.painter().line_segment([egui::pos2(x_l + 3.5, y_b - 3.0), egui::pos2(x_l, y_b)], l_stroke);
                        ui.painter().line_segment([egui::pos2(x_l, y_b), egui::pos2(x_l + 3.5, y_b + 3.0)], l_stroke);
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

    }

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
                // Fixed Bottom Bar: Actions & Progress Bar (Pinned so user never has to scroll)
                egui::Panel::bottom("transcode_bottom_bar")
                    .show_separator_line(true)
                    .frame(
                        egui::Frame::new()
                            .fill(colors::BG_CARD)
                            .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
                            .inner_margin(egui::Margin::symmetric(14, 10))
                    )
                    .show(ui, |ui| {
                        let can_start = self.enc_input_path.is_some()
                            && self.enc_output_path.is_some()
                            && self.enc_detected_frames > 0
                            && self.enc_rx.is_none();

                        ui.horizontal(|ui| {
                            let encode_btn = if can_start {
                                egui::Button::new(RichText::new("Start Transcoding").size(14.0).color(Color32::WHITE).strong())
                                    .min_size(Vec2::new(160.0, 36.0))
                                    .fill(colors::ACCENT_BLUE)
                                    .corner_radius(CornerRadius::same(6))
                            } else {
                                egui::Button::new(RichText::new("Start Transcoding").size(14.0).color(colors::TEXT_FAINT).strong())
                                    .min_size(Vec2::new(160.0, 36.0))
                                    .fill(colors::BG_ELEVATED)
                                    .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
                                    .corner_radius(CornerRadius::same(6))
                            };

                            let encode_resp = ui.add_enabled(can_start, encode_btn);
                            let encode_resp = if can_start {
                                encode_resp.on_hover_text("Start encoding selected media to HAP QuickTime MOV")
                            } else {
                                encode_resp.on_disabled_hover_text("Select a source file and output destination to enable transcoding")
                            };

                            if encode_resp.clicked() {
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

                // Scrollable Central Area for Media Configuration
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::ZERO))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            // Source Media Card
                            let input_frame = egui::Frame::new()
                                .fill(colors::BG_CARD)
                                .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
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
                                            let tsz = thumb.size_vec2();
                                            let t_aspect = if tsz.y > 0.0 { tsz.x / tsz.y } else { 1.0 };
                                            let (tw, th) = if t_aspect >= 1.0 {
                                                (72.0, (72.0 / t_aspect).max(18.0))
                                            } else {
                                                ((72.0 * t_aspect).max(18.0), 72.0)
                                            };
                                            let (rect, _) = ui.allocate_exact_size(Vec2::new(tw, th), egui::Sense::hover());
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
                        });
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
                        .fill(colors::BG_CARD)
                        .stroke(Stroke::new(1.0, colors::BORDER_SUBTLE))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::VideoProbeInfo;

    #[test]
    fn test_pause_playback_and_render() {
        let mut app = HapLabApp::default();
        let ctx = egui::Context::default();

        let probe = VideoProbeInfo {
            codec: "H.264".to_string(),
            width: 1920,
            height: 1080,
            fps: 30.0,
            frame_count: 100,
            duration_secs: 3.33,
            thumbnail_rgba: None,
        };
        app.generic_player = Some(GenericVideoPlayer::new(PathBuf::from("test.mp4"), &probe));
        app.is_playing = true;

        // Verify toggle_playback sets is_playing to false
        app.toggle_playback(&ctx);
        assert!(!app.is_playing);

        // Now run a full egui frame with is_playing = false
        let output = ctx.run_ui(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let ui_ctx = ui.ctx().clone();
                app.render_bottom_transport(ui, &ui_ctx);
                app.render_bottom_metadata(ui);
            });
        });
        let clipped_primitives = ctx.tessellate(output.shapes, 1.0);
        assert!(!clipped_primitives.is_empty());

        // Test with HAP Reader as well
        let temp_dir = std::env::temp_dir().join("haplab_test_hap_pause");
        let _ = std::fs::create_dir_all(&temp_dir);
        let mov_path = temp_dir.join("test_hap.mov");
        let rgba = vec![128u8; 64 * 64 * 4];

        if crate::worker::export_image_to_hap_mov(&rgba, 64, 64, &mov_path, hap_core::HapFormat::HapY, true).is_ok() {
            if let Ok(reader) = hap_core::QtHapReader::open(&mov_path) {
                let mut hap_app = HapLabApp::default();
                hap_app.reader = Some(reader);
                hap_app.is_playing = true;
                hap_app.toggle_playback(&ctx);
                assert!(!hap_app.is_playing);

                let output2 = ctx.run_ui(Default::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let ui_ctx = ui.ctx().clone();
                        hap_app.render_bottom_transport(ui, &ui_ctx);
                        hap_app.render_bottom_metadata(ui);
                    });
                });
                let prim2 = ctx.tessellate(output2.shapes, 1.0);
                assert!(!prim2.is_empty());
            }
        }
        let _ = std::fs::remove_file(mov_path);
        let _ = std::fs::remove_dir(temp_dir);
    }
}
