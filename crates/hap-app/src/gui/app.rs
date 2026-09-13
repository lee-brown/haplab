//! Main eframe / egui application interface.

use crate::worker::{spawn_encode_worker, spawn_export_worker, EncodeJobConfig, WorkerProgress};
use crossbeam_channel::Receiver;
use eframe::egui::{self, Color32, RichText, Stroke, TextureOptions, Vec2};
use hap_core::{decode_frame_to_rgba, HapFormat, QtHapReader};
use image::GenericImageView;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    PlayerInspector,
    Encoder,
    Diagnostics,
}

pub struct HapStudioApp {
    active_tab: ActiveTab,

    // --- Player / Inspector State ---
    mov_path: Option<PathBuf>,
    reader: Option<QtHapReader>,
    current_frame: usize,
    is_playing: bool,
    last_frame_time: Instant,
    preview_texture: Option<egui::TextureHandle>,
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
    enc_format: HapFormat,
    enc_fps: f32,
    enc_chunks: usize,
    enc_snappy: bool,
    enc_rx: Option<Receiver<WorkerProgress>>,
    enc_cancel: Option<Arc<AtomicBool>>,
    enc_status: Option<WorkerProgress>,

    // --- System / Hardware State ---
    gpu_adapter_name: String,
    gpu_backend_name: String,
    gpu_supports_bc: bool,
    system_logs: Vec<String>,
}

impl Default for HapStudioApp {
    fn default() -> Self {
        let instance = wgpu::Instance::default();
        let adapter_res: Result<wgpu::Adapter, _> = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }));
        let (adapter_name, backend_name, supports_bc) = match adapter_res {
            Ok(adapter) => {
                let info = adapter.get_info();
                let has_bc = adapter.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
                (info.name, format!("{:?}", info.backend), has_bc)
            }
            Err(_) => ("Software / Fallback".to_string(), "None".to_string(), false),
        };

        let mut app = Self {
            active_tab: ActiveTab::PlayerInspector,

            mov_path: None,
            reader: None,
            current_frame: 0,
            is_playing: false,
            last_frame_time: Instant::now(),
            preview_texture: None,
            export_format: "png".to_string(),
            export_rx: None,
            export_cancel: None,
            export_status: None,

            enc_input_path: None,
            enc_output_path: None,
            enc_detected_frames: 0,
            enc_detected_w: 0,
            enc_detected_h: 0,
            enc_format: HapFormat::HapY,
            enc_fps: 30.0,
            enc_chunks: 4,
            enc_snappy: true,
            enc_rx: None,
            enc_cancel: None,
            enc_status: None,

            gpu_adapter_name: adapter_name,
            gpu_backend_name: backend_name,
            gpu_supports_bc: supports_bc,
            system_logs: Vec::new(),
        };

        app.log("Application initialized.");
        app.log(&format!("GPU Adapter: {} [{}]", app.gpu_adapter_name, app.gpu_backend_name));
        app.log(&format!("Hardware BC Compression: {}", if app.gpu_supports_bc { "Enabled (wgpu)" } else { "CPU Fallback" }));

        app
    }
}

impl HapStudioApp {
    fn log(&mut self, msg: &str) {
        let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();
        self.system_logs.push(format!("[{}] {}", timestamp, msg));
    }

    fn open_mov_file(&mut self, path: PathBuf, ctx: &egui::Context) {
        match QtHapReader::open(&path) {
            Ok(reader) => {
                self.log(&format!(
                    "Opened HAP file: {:?} ({}x{}, {:.2} fps, {} frames)",
                    path.file_name().unwrap_or_default(),
                    reader.width(),
                    reader.height(),
                    reader.fps(),
                    reader.frame_count()
                ));
                self.reader = Some(reader);
                self.mov_path = Some(path);
                self.current_frame = 0;
                self.is_playing = false;
                self.update_preview_frame(ctx);
            }
            Err(e) => {
                self.log(&format!("Error opening MOV: {}", e));
            }
        }
    }

    fn update_preview_frame(&mut self, ctx: &egui::Context) {
        if let Some(ref mut reader) = self.reader {
            let width = reader.width() as usize;
            let height = reader.height() as usize;
            let frame_idx = self.current_frame.min(reader.frame_count().saturating_sub(1));

            if let Ok(packet) = reader.read_frame_packet(frame_idx) {
                if let Ok(rgba) = decode_frame_to_rgba(&packet, width, height) {
                    let color_img = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                    self.preview_texture = Some(ctx.load_texture("video-preview", color_img, TextureOptions::LINEAR));
                }
            }
        }
    }

    fn scan_encoder_input(&mut self) {
        if let Some(ref path) = self.enc_input_path {
            let mut count = 0;
            let mut first_file = None;

            if path.is_dir() {
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                            let ext_l = ext.to_lowercase();
                            if matches!(ext_l.as_str(), "png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" | "webp") {
                                if count == 0 {
                                    first_file = Some(p);
                                }
                                count += 1;
                            }
                        }
                    }
                }
            } else if path.is_file() {
                count = 1;
                first_file = Some(path.clone());
            }

            self.enc_detected_frames = count;

            if let Some(first) = first_file {
                if let Ok(img) = image::open(&first) {
                    let (w, h) = img.dimensions();
                    self.enc_detected_w = w;
                    self.enc_detected_h = h;
                }
            }

            if self.enc_output_path.is_none() {
                let default_out = if path.is_dir() {
                    path.join("output_hap.mov")
                } else {
                    path.with_extension("mov")
                };
                self.enc_output_path = Some(default_out);
            }
        }
    }
}

impl eframe::App for HapStudioApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Drag & Drop
        ctx.input(|i| {
            if let Some(dropped) = i.raw.dropped_files.first() {
                if let Some(ref path) = dropped.path {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        let ext_lower = ext.to_lowercase();
                        if ext_lower == "mov" || ext_lower == "mp4" {
                            self.open_mov_file(path.clone(), ctx);
                            self.active_tab = ActiveTab::PlayerInspector;
                        } else if matches!(ext_lower.as_str(), "png" | "jpg" | "jpeg" | "tiff" | "bmp" | "webp") {
                            self.enc_input_path = Some(path.clone());
                            self.scan_encoder_input();
                            self.active_tab = ActiveTab::Encoder;
                        }
                    } else if path.is_dir() {
                        self.enc_input_path = Some(path.clone());
                        self.scan_encoder_input();
                        self.active_tab = ActiveTab::Encoder;
                    }
                }
            }
        });

        // 2. Playback Timer loop
        if self.is_playing {
            if let Some(ref reader) = self.reader {
                let fps = reader.fps().max(1.0);
                let frame_interval = 1.0 / fps;
                if self.last_frame_time.elapsed().as_secs_f32() >= frame_interval {
                    self.current_frame = (self.current_frame + 1) % reader.frame_count();
                    self.last_frame_time = Instant::now();
                    self.update_preview_frame(ctx);
                }
                ctx.request_repaint();
            }
        }

        // 3. Background Workers polling
        if let Some(ref rx) = self.enc_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerProgress::Finished { ref message } => {
                        self.log(message);
                        self.enc_status = Some(msg);
                        self.enc_rx = None;
                        break;
                    }
                    WorkerProgress::Error(ref err) => {
                        self.log(&format!("Encode failed: {}", err));
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
                        self.export_status = Some(msg);
                        self.export_rx = None;
                        break;
                    }
                    WorkerProgress::Error(ref err) => {
                        self.log(&format!("Export failed: {}", err));
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
        ui.vertical(|ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("⚡ HAP Video Studio").color(Color32::from_rgb(0, 200, 255)).strong());
                ui.label(RichText::new("v0.1.0").weak());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.gpu_supports_bc {
                        ui.label(RichText::new("⚡ GPU BC Accelerated").color(Color32::from_rgb(0, 255, 150)));
                    } else {
                        ui.label(RichText::new("🖥 CPU Fallback Mode").color(Color32::LIGHT_YELLOW));
                    }
                });
            });

            ui.add_space(4.0);
            ui.separator();

            ui.horizontal(|ui| {
                if ui.selectable_label(self.active_tab == ActiveTab::PlayerInspector, "🎬 Player & Inspector (Decode)").clicked() {
                    self.active_tab = ActiveTab::PlayerInspector;
                }
                if ui.selectable_label(self.active_tab == ActiveTab::Encoder, "🚀 Video Encoder").clicked() {
                    self.active_tab = ActiveTab::Encoder;
                }
                if ui.selectable_label(self.active_tab == ActiveTab::Diagnostics, "🛠 Hardware & Diagnostics").clicked() {
                    self.active_tab = ActiveTab::Diagnostics;
                }
            });

            ui.separator();
            ui.add_space(4.0);

            // Active Tab Content
            match self.active_tab {
                ActiveTab::PlayerInspector => self.show_player_tab(ui),
                ActiveTab::Encoder => self.show_encoder_tab(ui),
                ActiveTab::Diagnostics => self.show_diagnostics_tab(ui),
            }

            ui.add_space(6.0);
            ui.separator();

            // Status Bar
            ui.horizontal(|ui| {
                if let Some(ref path) = self.mov_path {
                    ui.label(format!("Active: {:?}", path.file_name().unwrap_or_default()));
                } else {
                    ui.label("Ready. Drag & Drop a .mov or images to begin.");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("GPU: {}", self.gpu_adapter_name));
                });
            });
        });
    }
}

impl HapStudioApp {
    fn show_player_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        ui.horizontal(|ui| {
            if ui.button("📂 Open HAP .mov File...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("QuickTime HAP Video", &["mov", "mp4"])
                    .pick_file()
                {
                    self.open_mov_file(path, &ctx);
                }
            }

            if self.reader.is_some() && ui.button("📤 Export Frames...").clicked() {
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

            ui.label("Export Format:");
            egui::ComboBox::from_id_salt("export_fmt")
                .selected_text(&self.export_format)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.export_format, "png".into(), "PNG (.png)");
                    ui.selectable_value(&mut self.export_format, "jpg".into(), "JPEG (.jpg)");
                    ui.selectable_value(&mut self.export_format, "tiff".into(), "TIFF (.tiff)");
                });
        });

        if let Some(ref status) = self.export_status {
            match status {
                WorkerProgress::Started { total } => {
                    ui.label(format!("Starting export of {} frames...", total));
                }
                WorkerProgress::Progress { current, total, fps, percent } => {
                    ui.horizontal(|ui| {
                        ui.label(format!("Exporting frames: [{}/{}] ({:.1} fps)", current, total, fps));
                        ui.add(egui::ProgressBar::new(*percent / 100.0).show_percentage());
                        if let Some(ref cancel) = self.export_cancel {
                            if ui.button("Cancel").clicked() {
                                cancel.store(true, Ordering::Relaxed);
                            }
                        }
                    });
                }
                WorkerProgress::Finished { message } => {
                    ui.label(RichText::new(message).color(Color32::from_rgb(0, 255, 120)));
                }
                WorkerProgress::Error(err) => {
                    ui.label(RichText::new(format!("Error: {}", err)).color(Color32::from_rgb(255, 80, 80)));
                }
            }
        }

        ui.separator();

        if let Some(metadata) = self.reader.as_ref().map(|r| {
            (r.format(), r.width(), r.height(), r.fps(), r.frame_count(), r.duration())
        }) {
            let (format, width, height, fps, count, duration) = metadata;

            egui::Frame::canvas(ui.style()).stroke(Stroke::new(1.0, Color32::DARK_GRAY)).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Codec:");
                    ui.colored_label(Color32::from_rgb(0, 200, 255), format!("{} [{}]", format.name(), String::from_utf8_lossy(&format.fourcc())));
                    ui.separator();
                    ui.strong("Resolution:");
                    ui.label(format!("{} × {}", width, height));
                    ui.separator();
                    ui.strong("Frame Rate:");
                    ui.label(format!("{:.2} fps", fps));
                    ui.separator();
                    ui.strong("Total Frames:");
                    ui.label(format!("{}", count));
                    ui.separator();
                    ui.strong("Duration:");
                    ui.label(format!("{:.2}s", duration));
                });
            });

            ui.add_space(6.0);

            if let Some(ref texture) = self.preview_texture {
                let avail_size = ui.available_size_before_wrap();
                let aspect = texture.aspect_ratio();
                let display_height = (avail_size.y - 80.0).max(120.0);
                let display_width = display_height * aspect;

                ui.vertical_centered(|ui| {
                    ui.image((texture.id(), Vec2::new(display_width, display_height)));
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Loading frame...");
                });
            }

            ui.add_space(6.0);

            // Transport Controls
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if ui.button(if self.is_playing { "⏸ Pause" } else { "▶ Play" }).clicked() {
                        self.is_playing = !self.is_playing;
                        self.last_frame_time = Instant::now();
                    }

                    if ui.button("⏮ First").clicked() {
                        self.current_frame = 0;
                        self.update_preview_frame(&ctx);
                    }
                    if ui.button("◀ Step").clicked() {
                        if self.current_frame > 0 {
                            self.current_frame -= 1;
                            self.update_preview_frame(&ctx);
                        }
                    }
                    if ui.button("Step ▶").clicked() {
                        if self.current_frame + 1 < count {
                            self.current_frame += 1;
                            self.update_preview_frame(&ctx);
                        }
                    }
                    if ui.button("Last ⏭").clicked() {
                        self.current_frame = count.saturating_sub(1);
                        self.update_preview_frame(&ctx);
                    }

                    ui.separator();

                    let old_frame = self.current_frame;
                    let max_frame = count.saturating_sub(1);
                    ui.add(egui::Slider::new(&mut self.current_frame, 0..=max_frame).text("Frame"));

                    if old_frame != self.current_frame {
                        self.update_preview_frame(&ctx);
                    }

                    let fps_calc = fps.max(1.0);
                    let sec = (self.current_frame as f32 / fps_calc) as u32;
                    let ff = (self.current_frame as f32 % fps_calc) as u32;
                    let mm = (sec / 60) % 60;
                    let hh = sec / 3600;
                    let ss = sec % 60;
                    ui.monospace(format!("{:02}:{:02}:{:02}:{:02}", hh, mm, ss, ff));
                });
            });
        } else {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("🎬 Drop a HAP .mov file here").size(22.0).color(Color32::GRAY));
                    ui.add_space(8.0);
                    ui.label("or click 'Open HAP .mov File...' to inspect and playback video.");
                });
            });
        }
    }

    fn show_encoder_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("🚀 HAP Video Encoder");
        ui.label("Encode image sequences directly into HAP QuickTime MOV files.");
        ui.add_space(8.0);

        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Input Directory / Sequence:");
                if ui.button("📁 Browse Folder...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.enc_input_path = Some(folder);
                        self.scan_encoder_input();
                    }
                }
                if ui.button("📄 Select File...").clicked() {
                    if let Some(file) = rfd::FileDialog::new().pick_file() {
                        self.enc_input_path = Some(file);
                        self.scan_encoder_input();
                    }
                }
            });

            if let Some(ref path) = self.enc_input_path {
                ui.monospace(format!("Path: {:?}", path));
                ui.label(format!(
                    "Found: {} frames | Resolution: {} × {}",
                    self.enc_detected_frames, self.enc_detected_w, self.enc_detected_h
                ));
            } else {
                ui.label(RichText::new("No input selected (Drag & drop a folder/image or click Browse)").weak());
            }
        });

        ui.add_space(8.0);

        ui.group(|ui| {
            ui.strong("Encoder Settings");
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label("HAP Flavour:");
                egui::ComboBox::from_id_salt("hap_flavour")
                    .selected_text(self.enc_format.name())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap1, "Hap (Hap 1) - RGB DXT1 (Smallest)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap5, "Hap Alpha (Hap 5) - RGBA DXT5");
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapY, "Hap Q - Scaled YCoCg DXT5 (High Quality)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::Hap7, "Hap R (Hap 7) - BC7 UNORM (Ultra Quality)");
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapM, "Hap Q Alpha - Dual Stream Color + Alpha");
                        ui.selectable_value(&mut self.enc_format, HapFormat::HapA, "Hap Alpha-Only - BC4 Matte");
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Frame Rate (FPS):");
                ui.add(egui::DragValue::new(&mut self.enc_fps).speed(0.1).range(1.0..=120.0));
                if ui.button("24").clicked() { self.enc_fps = 24.0; }
                if ui.button("25").clicked() { self.enc_fps = 25.0; }
                if ui.button("29.97").clicked() { self.enc_fps = 29.97; }
                if ui.button("30").clicked() { self.enc_fps = 30.0; }
                if ui.button("60").clicked() { self.enc_fps = 60.0; }
            });

            ui.horizontal(|ui| {
                ui.label("Threading Chunks:");
                egui::ComboBox::from_id_salt("chunks_picker")
                    .selected_text(format!("{} Chunks", self.enc_chunks))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.enc_chunks, 1, "1 Chunk (Single)");
                        ui.selectable_value(&mut self.enc_chunks, 2, "2 Chunks");
                        ui.selectable_value(&mut self.enc_chunks, 4, "4 Chunks (Recommended)");
                        ui.selectable_value(&mut self.enc_chunks, 8, "8 Chunks (Recommended for 4K)");
                        ui.selectable_value(&mut self.enc_chunks, 16, "16 Chunks");
                    });

                ui.separator();
                ui.checkbox(&mut self.enc_snappy, "Snappy Compression");
            });
        });

        ui.add_space(8.0);

        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Output Destination (.mov):");
                if ui.button("💾 Browse Destination...").clicked() {
                    if let Some(dest) = rfd::FileDialog::new()
                        .add_filter("QuickTime Movie", &["mov"])
                        .save_file()
                    {
                        self.enc_output_path = Some(dest);
                    }
                }
            });

            if let Some(ref out) = self.enc_output_path {
                ui.monospace(format!("{:?}", out));
            } else {
                ui.label(RichText::new("No destination set").weak());
            }
        });

        ui.add_space(12.0);

        let can_encode = self.enc_input_path.is_some() && self.enc_output_path.is_some() && self.enc_detected_frames > 0 && self.enc_rx.is_none();

        ui.horizontal(|ui| {
            let btn = ui.add_enabled(can_encode, egui::Button::new(RichText::new("🚀 Start Encoding").size(18.0).strong()));
            if btn.clicked() {
                if let (Some(ref in_p), Some(ref out_p)) = (&self.enc_input_path, &self.enc_output_path) {
                    let cancel_flag = Arc::new(AtomicBool::new(false));
                    let (tx, rx) = crossbeam_channel::unbounded();
                    self.enc_rx = Some(rx);
                    self.enc_cancel = Some(cancel_flag.clone());

                    let cfg = EncodeJobConfig {
                        input_dir: in_p.clone(),
                        output_file: out_p.clone(),
                        format: self.enc_format,
                        fps: self.enc_fps,
                        chunks: self.enc_chunks,
                        snappy: self.enc_snappy,
                    };

                    self.log(&format!("Starting encode: {:?} -> {:?}", in_p, out_p));
                    spawn_encode_worker(cfg, cancel_flag, tx);
                }
            }

            if self.enc_rx.is_some() {
                if ui.button(RichText::new("🛑 Cancel").color(Color32::RED)).clicked() {
                    if let Some(ref cancel) = self.enc_cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            }
        });

        if let Some(ref status) = self.enc_status {
            ui.add_space(8.0);
            match status {
                WorkerProgress::Started { total } => {
                    ui.label(format!("Starting encode of {} frames...", total));
                }
                WorkerProgress::Progress { current, total, fps, percent } => {
                    ui.horizontal(|ui| {
                        ui.label(format!("Encoding: [{}/{}] ({:.1} fps)", current, total, fps));
                        ui.add(egui::ProgressBar::new(*percent / 100.0).show_percentage());
                    });
                }
                WorkerProgress::Finished { message } => {
                    ui.label(RichText::new(message).color(Color32::from_rgb(0, 255, 120)).strong());
                }
                WorkerProgress::Error(err) => {
                    ui.label(RichText::new(format!("Error: {}", err)).color(Color32::from_rgb(255, 80, 80)).strong());
                }
            }
        }
    }

    fn show_diagnostics_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("🛠 Hardware & System Diagnostics");
        ui.add_space(6.0);

        egui::Grid::new("diag_grid").striped(true).show(ui, |ui| {
            ui.strong("GPU Adapter:");
            ui.label(&self.gpu_adapter_name);
            ui.end_row();

            ui.strong("Graphics Backend:");
            ui.label(&self.gpu_backend_name);
            ui.end_row();

            ui.strong("Hardware BC Compression:");
            ui.label(if self.gpu_supports_bc {
                RichText::new("Enabled (wgpu)").color(Color32::GREEN)
            } else {
                RichText::new("Unsupported / Fallback to pure Rust CPU").color(Color32::LIGHT_YELLOW)
            });
            ui.end_row();

            ui.strong("CPU Threads (Rayon):");
            ui.label(format!("{} logical threads", rayon::current_num_threads()));
            ui.end_row();
        });

        ui.add_space(10.0);
        ui.separator();
        ui.strong("Event & Activity Log:");
        ui.add_space(4.0);

        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            for log in &self.system_logs {
                ui.monospace(log);
            }
        });
    }
}
