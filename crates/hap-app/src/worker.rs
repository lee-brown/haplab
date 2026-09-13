//! Background thread workers for responsive GUI progress reporting.

use crossbeam_channel::Sender;
use hap_core::{
    decode_frame_to_rgba, encode_frame_with_options, AlphaMode, ColorRange, DitherMode,
    EncodeOptions, HapFormat, QualityPreset, QtHapReader, QtHapWriter, VideoConfig,
};
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
    pub color_range: ColorRange,
    pub alpha_mode: AlphaMode,
    pub dither_mode: DitherMode,
    pub quality: QualityPreset,
}

pub fn is_video_container(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_lowercase().as_str(),
                "mp4" | "mkv" | "avi" | "mov" | "m4v" | "webm" | "prores" | "mxf" | "ts" | "wmv" | "flv"
            )
        })
        .unwrap_or(false)
}

#[allow(dead_code)]
pub struct VideoProbeInfo {
    pub codec: String,
    pub width: usize,
    pub height: usize,
    pub fps: f32,
    pub frame_count: usize,
    pub duration_secs: f32,
    pub thumbnail_rgba: Option<(u32, u32, Vec<u8>)>,
}

pub fn probe_video_input(path: &std::path::Path) -> Result<VideoProbeInfo, String> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    // 1. Try ffprobe for codec, dimensions, framerate, duration, frame count
    let mut probe_cmd = std::process::Command::new("ffprobe");
    probe_cmd
        .args(&[
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=codec_name,width,height,r_frame_rate,duration,nb_frames",
            "-of", "csv=p=0",
        ])
        .arg(path);
    #[cfg(windows)]
    probe_cmd.creation_flags(CREATE_NO_WINDOW);

    let output = probe_cmd.output().map_err(|e| {
        format!("FFmpeg/FFprobe not found on system PATH ({}). Install FFmpeg to import video files directly, or supply an image sequence (PNG, TIFF, JPEG).", e)
    })?;

    if !output.status.success() {
        return Err("Could not inspect video file using ffprobe.".into());
    }

    let out_str = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = out_str.trim().split(',').collect();
    if parts.len() < 4 {
        return Err("Could not parse video metadata from ffprobe output.".into());
    }

    let codec_raw = parts[0].trim();
    let codec = match codec_raw.to_lowercase().as_str() {
        "h264" | "avc1" => "H.264 / AVC".to_string(),
        "hevc" | "h265" | "hev1" => "H.265 / HEVC".to_string(),
        "av1" | "av01" => "AV1".to_string(),
        "prores" => "Apple ProRes".to_string(),
        "vp9" => "VP9".to_string(),
        "vp8" => "VP8".to_string(),
        "dnxhd" | "dnxhr" => "Avid DNxHD/HR".to_string(),
        other => {
            if other.is_empty() {
                "Video".to_string()
            } else {
                other.to_uppercase()
            }
        }
    };

    let width: usize = parts[1].trim().parse().map_err(|_| "Invalid width")?;
    let height: usize = parts[2].trim().parse().map_err(|_| "Invalid height")?;

    let fps_str = parts[3].trim();
    let fps: f32 = if let Some((num, den)) = fps_str.split_once('/') {
        let n: f32 = num.parse().unwrap_or(30.0);
        let d: f32 = den.parse().unwrap_or(1.0);
        if d > 0.0 { n / d } else { 30.0 }
    } else {
        fps_str.parse().unwrap_or(30.0)
    };

    let duration: f32 = parts.get(4).and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
    let frame_count: usize = parts.get(5)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| {
            if duration > 0.0 && fps > 0.0 {
                (duration * fps).round() as usize
            } else {
                0
            }
        });

    // 2. Extract 1st frame thumbnail via ffmpeg
    let mut thumb_cmd = std::process::Command::new("ffmpeg");
    thumb_cmd
        .args(&["-v", "error", "-ss", "00:00:00", "-i"])
        .arg(path)
        .args(&["-vframes", "1", "-f", "image2pipe", "-vcodec", "png", "-"]);
    #[cfg(windows)]
    thumb_cmd.creation_flags(CREATE_NO_WINDOW);

    let thumbnail_rgba = if let Ok(thumb_out) = thumb_cmd.output() {
        if thumb_out.status.success() && !thumb_out.stdout.is_empty() {
            if let Ok(img) = image::load_from_memory(&thumb_out.stdout) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Some((w, h, rgba.into_raw()))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(VideoProbeInfo {
        codec,
        width,
        height,
        fps,
        frame_count,
        duration_secs: duration,
        thumbnail_rgba,
    })
}

pub fn spawn_encode_worker(
    config: EncodeJobConfig,
    cancel_flag: Arc<AtomicBool>,
    progress_tx: Sender<WorkerProgress>,
) {
    std::thread::spawn(move || {
        if config.input_dir.is_file() && is_video_container(&config.input_dir) {
            spawn_encode_from_video(config, cancel_flag, progress_tx);
            return;
        }

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
            let options = EncodeOptions {
                format: config.format,
                chunk_count: config.chunks,
                use_snappy: config.snappy,
                color_range: config.color_range,
                alpha_mode: config.alpha_mode,
                dither_mode: config.dither_mode,
                quality: config.quality,
            };
            let packet = match encode_frame_with_options(&raw, width as usize, height as usize, &options) {
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

fn spawn_encode_from_video(
    config: EncodeJobConfig,
    cancel_flag: Arc<AtomicBool>,
    progress_tx: Sender<WorkerProgress>,
) {
    let probe = match probe_video_input(&config.input_dir) {
        Ok(p) => p,
        Err(e) => {
            let _ = progress_tx.send(WorkerProgress::Error(e));
            return;
        }
    };

    let width = probe.width;
    let height = probe.height;
    if width % 4 != 0 || height % 4 != 0 {
        let _ = progress_tx.send(WorkerProgress::Error(format!(
            "Video dimensions ({}x{}) must be multiples of 4 for HAP texture encoding.",
            width, height
        )));
        return;
    }

    let total = probe.frame_count.max(1);
    let _ = progress_tx.send(WorkerProgress::Started { total });

    let video_cfg = VideoConfig::new(width as u32, height as u32, config.fps, config.format);
    let mut writer = match QtHapWriter::create(&config.output_file, video_cfg) {
        Ok(w) => w,
        Err(e) => {
            let _ = progress_tx.send(WorkerProgress::Error(format!("Failed to create MOV file: {}", e)));
            return;
        }
    };

    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(&["-v", "error", "-i"])
        .arg(&config.input_dir)
        .args(&["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = match cmd.stdout(std::process::Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = progress_tx.send(WorkerProgress::Error(format!("Failed to spawn ffmpeg: {}", e)));
            return;
        }
    };

    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = progress_tx.send(WorkerProgress::Error("Failed to capture ffmpeg stdout pipe".into()));
            return;
        }
    };

    let frame_bytes = width * height * 4;
    let mut raw = vec![0u8; frame_bytes];
    let mut current_frame = 0usize;
    let start_time = Instant::now();

    use std::io::Read;
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = progress_tx.send(WorkerProgress::Error("Encoding cancelled by user.".into()));
            return;
        }

        match stdout.read_exact(&mut raw) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Video stream completed
                break;
            }
            Err(e) => {
                let _ = child.kill();
                let _ = progress_tx.send(WorkerProgress::Error(format!("Pipe read error: {}", e)));
                return;
            }
        }

        let options = hap_core::EncodeOptions {
            format: config.format,
            chunk_count: config.chunks,
            use_snappy: config.snappy,
            color_range: config.color_range,
            alpha_mode: config.alpha_mode,
            dither_mode: config.dither_mode,
            quality: config.quality,
        };

        let packet = match hap_core::encode_frame_with_options(&raw, width, height, &options) {
            Ok(p) => p,
            Err(e) => {
                let _ = child.kill();
                let _ = progress_tx.send(WorkerProgress::Error(format!("Encode error on frame {}: {}", current_frame, e)));
                return;
            }
        };

        if let Err(e) = writer.write_frame(&packet) {
            let _ = child.kill();
            let _ = progress_tx.send(WorkerProgress::Error(format!("Write error on frame {}: {}", current_frame, e)));
            return;
        }

        current_frame += 1;
        let elapsed = start_time.elapsed().as_secs_f32().max(0.001);
        let fps = current_frame as f32 / elapsed;
        let pct = if total > 0 { (current_frame as f32 / total as f32 * 100.0).min(99.9) } else { 0.0 };

        let _ = progress_tx.send(WorkerProgress::Progress {
            current: current_frame,
            total,
            fps,
            percent: pct,
        });
    }

    let _ = child.wait();

    if let Err(e) = writer.finalize() {
        let _ = progress_tx.send(WorkerProgress::Error(format!("Finalize error: {}", e)));
        return;
    }

    let total_secs = start_time.elapsed().as_secs_f32();
    let _ = progress_tx.send(WorkerProgress::Finished {
        message: format!("Successfully encoded {} frames from video to {:?} ({:.2}s)", current_frame, config.output_file, total_secs),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_container_extensions() {
        assert!(is_video_container(std::path::Path::new("render.mp4")));
        assert!(is_video_container(std::path::Path::new("video.mkv")));
        assert!(is_video_container(std::path::Path::new("clip.mov")));
        assert!(is_video_container(std::path::Path::new("web.webm")));
        assert!(is_video_container(std::path::Path::new("broadcast.mxf")));
        assert!(!is_video_container(std::path::Path::new("frame_0001.png")));
        assert!(!is_video_container(std::path::Path::new("still.jpg")));
    }
}

