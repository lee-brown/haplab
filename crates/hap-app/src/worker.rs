//! Background thread workers for responsive GUI progress reporting.

use crossbeam_channel::Sender;
use hap_core::{decode_frame_to_rgba, encode_frame, HapFormat, QtHapReader, QtHapWriter, VideoConfig};
use image::GenericImageView;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum WorkerProgress {
    Started { total: usize },
    Progress { current: usize, total: usize, fps: f32, percent: f32 },
    Finished { message: String },
    Error(String),
}

pub struct EncodeJobConfig {
    pub input_dir: PathBuf,
    pub output_file: PathBuf,
    pub format: HapFormat,
    pub fps: f32,
    pub chunks: usize,
    pub snappy: bool,
}

pub fn spawn_encode_worker(
    config: EncodeJobConfig,
    cancel_flag: Arc<AtomicBool>,
    progress_tx: Sender<WorkerProgress>,
) {
    std::thread::spawn(move || {
        let mut image_files = Vec::new();
        if config.input_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&config.input_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                        let ext_l = ext.to_lowercase();
                        if matches!(ext_l.as_str(), "png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" | "webp") {
                            image_files.push(p);
                        }
                    }
                }
            }
            image_files.sort();
        } else if config.input_dir.is_file() {
            image_files.push(config.input_dir.clone());
        }

        if image_files.is_empty() {
            let _ = progress_tx.send(WorkerProgress::Error("No valid image files found in input path.".into()));
            return;
        }

        let total = image_files.len();
        let _ = progress_tx.send(WorkerProgress::Started { total });

        let first_img = match image::open(&image_files[0]) {
            Ok(img) => img,
            Err(e) => {
                let _ = progress_tx.send(WorkerProgress::Error(format!("Failed to open first image: {}", e)));
                return;
            }
        };

        let (width, height) = first_img.dimensions();
        if width % 4 != 0 || height % 4 != 0 {
            let _ = progress_tx.send(WorkerProgress::Error(format!(
                "Image dimensions ({}x{}) must be multiples of 4.",
                width, height
            )));
            return;
        }

        let video_cfg = VideoConfig::new(width, height, config.fps, config.format);
        let mut writer = match QtHapWriter::create(&config.output_file, video_cfg) {
            Ok(w) => w,
            Err(e) => {
                let _ = progress_tx.send(WorkerProgress::Error(format!("Failed to create MOV file: {}", e)));
                return;
            }
        };

        let start_time = Instant::now();
        for (i, path) in image_files.iter().enumerate() {
            if cancel_flag.load(Ordering::Relaxed) {
                let _ = progress_tx.send(WorkerProgress::Error("Encoding cancelled by user.".into()));
                return;
            }

            let img = match image::open(path) {
                Ok(img) => img.to_rgba8(),
                Err(e) => {
                    let _ = progress_tx.send(WorkerProgress::Error(format!("Failed reading frame {}: {}", i, e)));
                    return;
                }
            };

            let raw = img.into_raw();
            let packet = match encode_frame(&raw, width as usize, height as usize, config.format, config.chunks, config.snappy) {
                Ok(pkt) => pkt,
                Err(e) => {
                    let _ = progress_tx.send(WorkerProgress::Error(format!("Encode error on frame {}: {}", i, e)));
                    return;
                }
            };

            if let Err(e) = writer.write_frame(&packet) {
                let _ = progress_tx.send(WorkerProgress::Error(format!("Write error on frame {}: {}", i, e)));
                return;
            }

            let current = i + 1;
            let elapsed = start_time.elapsed().as_secs_f32().max(0.001);
            let fps = current as f32 / elapsed;
            let percent = (current as f32 / total as f32) * 100.0;

            let _ = progress_tx.send(WorkerProgress::Progress {
                current,
                total,
                fps,
                percent,
            });
        }

        if let Err(e) = writer.finalize() {
            let _ = progress_tx.send(WorkerProgress::Error(format!("Failed finalizing MOV: {}", e)));
            return;
        }

        let total_secs = start_time.elapsed().as_secs_f32();
        let _ = progress_tx.send(WorkerProgress::Finished {
            message: format!(
                "Successfully encoded {} frames to {:?} ({:.2}s)",
                total, config.output_file, total_secs
            ),
        });
    });
}

pub fn spawn_export_worker(
    mov_path: PathBuf,
    export_dir: PathBuf,
    format: String,
    cancel_flag: Arc<AtomicBool>,
    progress_tx: Sender<WorkerProgress>,
) {
    std::thread::spawn(move || {
        let mut reader = match QtHapReader::open(&mov_path) {
            Ok(r) => r,
            Err(e) => {
                let _ = progress_tx.send(WorkerProgress::Error(format!("Failed to open MOV: {}", e)));
                return;
            }
        };

        if let Err(e) = fs::create_dir_all(&export_dir) {
            let _ = progress_tx.send(WorkerProgress::Error(format!("Failed creating export directory: {}", e)));
            return;
        }

        let total = reader.frame_count();
        let width = reader.width() as usize;
        let height = reader.height() as usize;
        let _ = progress_tx.send(WorkerProgress::Started { total });

        let start_time = Instant::now();
        for i in 0..total {
            if cancel_flag.load(Ordering::Relaxed) {
                let _ = progress_tx.send(WorkerProgress::Error("Export cancelled by user.".into()));
                return;
            }

            let packet = match reader.read_frame_packet(i) {
                Ok(p) => p,
                Err(e) => {
                    let _ = progress_tx.send(WorkerProgress::Error(format!("Read frame {} error: {}", i, e)));
                    return;
                }
            };

            let rgba = match decode_frame_to_rgba(&packet, width, height) {
                Ok(b) => b,
                Err(e) => {
                    let _ = progress_tx.send(WorkerProgress::Error(format!("Decode frame {} error: {}", i, e)));
                    return;
                }
            };

            let filename = format!("frame_{:06}.{}", i, format);
            let out_file = export_dir.join(filename);

            if let Some(img) = image::RgbaImage::from_raw(width as u32, height as u32, rgba) {
                if let Err(e) = img.save(&out_file) {
                    let _ = progress_tx.send(WorkerProgress::Error(format!("Failed saving frame {}: {}", i, e)));
                    return;
                }
            }

            let current = i + 1;
            let elapsed = start_time.elapsed().as_secs_f32().max(0.001);
            let fps = current as f32 / elapsed;
            let percent = (current as f32 / total as f32) * 100.0;

            let _ = progress_tx.send(WorkerProgress::Progress {
                current,
                total,
                fps,
                percent,
            });
        }

        let total_secs = start_time.elapsed().as_secs_f32();
        let _ = progress_tx.send(WorkerProgress::Finished {
            message: format!("Successfully exported {} frames to {:?} ({:.2}s)", total, export_dir, total_secs),
        });
    });
}
