//! Background thread workers for responsive GUI progress reporting.

use crossbeam_channel::{Receiver, Sender};
use hap_core::{
    decode_frame_to_rgba, encode_frame_with_options, extract_stream_summary, AlphaMode, ColorRange,
    DitherMode, EncodeOptions, HapFormat, QualityPreset, QtHapReader, QtHapWriter, StreamSummary,
    VideoConfig,
};
use image::GenericImageView;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum WorkerProgress {
    Started { total: usize },
    Progress { current: usize, total: usize, fps: f32, percent: f32 },
    Finished { message: String },
    Error(String),
}

/// Result of asynchronous media loading off the main GUI thread.
pub enum MediaLoadResult {
    HapVideo {
        path: PathBuf,
        reader: QtHapReader,
        summary: Option<StreamSummary>,
        first_frame_rgba: Option<Vec<u8>>,
        first_frame_decode_ms: f32,
        first_packet_bytes: usize,
    },
    StillImage {
        path: PathBuf,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    GenericVideo {
        path: PathBuf,
        probe: VideoProbeInfo,
    },
    ImageSequence {
        path: PathBuf,
        count: usize,
        width: usize,
        height: usize,
        first_file: Option<PathBuf>,
        last_file: Option<PathBuf>,
        thumbnail_rgba: Option<(usize, usize, Vec<u8>)>,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
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
    pub video_dimensions: Option<(usize, usize)>,
    pub total_frames: Option<usize>,
}

/// Resolve the path to the ffmpeg executable, preferring direct tools binaries over shims.
pub fn find_ffmpeg_binary() -> PathBuf {
    find_tool_binary("ffmpeg")
}

/// Resolve the path to the ffprobe executable, preferring direct tools binaries over shims.
pub fn find_ffprobe_binary() -> PathBuf {
    find_tool_binary("ffprobe")
}

fn find_tool_binary(tool_name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let exe_name = if tool_name.ends_with(".exe") {
            tool_name.to_string()
        } else {
            format!("{}.exe", tool_name)
        };

        // 1. Direct Chocolatey tools directory (bypasses Chocolatey ShimGen shims)
        let choco_tool = PathBuf::from(r"C:\ProgramData\chocolatey\lib\ffmpeg\tools\ffmpeg\bin").join(&exe_name);
        if choco_tool.is_file() {
            return choco_tool;
        }

        // 2. Co-located next to running application binary
        if let Ok(cur_exe) = std::env::current_exe() {
            if let Some(parent) = cur_exe.parent() {
                let local_path = parent.join(&exe_name);
                if local_path.is_file() {
                    return local_path;
                }
            }
        }

        // 3. User Scoop installations
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let scoop_path = PathBuf::from(&userprofile).join(format!(r"scoop\apps\ffmpeg\current\bin\{}", exe_name));
            if scoop_path.is_file() {
                return scoop_path;
            }
        }

        // 4. Windows WinGet package directories
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            let winget_path = PathBuf::from(&localappdata).join(format!(r"Microsoft\WinGet\Packages\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe\ffmpeg\bin\{}", exe_name));
            if winget_path.is_file() {
                return winget_path;
            }
        }

        // 5. System PATH lookup
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join(&exe_name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }

    PathBuf::from(tool_name)
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

#[derive(Clone, Debug)]
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

/// Quickly probe video dimensions and stream metadata without extracting thumbnails.
pub fn probe_video_dimensions(path: &std::path::Path) -> Result<(usize, usize, usize, f32), String> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let ffprobe_bin = find_ffprobe_binary();
    let mut probe_cmd = std::process::Command::new(&ffprobe_bin);
    probe_cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .args(&[
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height,r_frame_rate,duration,nb_frames",
            "-of", "csv=p=0",
        ])
        .arg(path);
    #[cfg(windows)]
    probe_cmd.creation_flags(CREATE_NO_WINDOW);

    let (tx, rx) = crossbeam_channel::bounded(1);
    let bin_display = ffprobe_bin.display().to_string();
    std::thread::spawn(move || {
        let res = probe_cmd.output();
        let _ = tx.send(res);
    });

    let output = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Err(format!("Failed to execute ffprobe ({}): {}", bin_display, e));
        }
        Err(_) => {
            return Err(format!("Timed out inspecting video dimensions with ffprobe ({})", bin_display));
        }
    };

    if !output.status.success() {
        let err_text = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Could not inspect video dimensions using ffprobe: {}", err_text.trim()));
    }

    let out_str = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = out_str.trim().split(',').collect();
    if parts.len() < 3 {
        return Err("Could not parse video stream dimensions from ffprobe output.".into());
    }

    let width: usize = parts[0].trim().parse().map_err(|_| "Invalid width")?;
    let height: usize = parts[1].trim().parse().map_err(|_| "Invalid height")?;

    let fps_str = parts[2].trim();
    let fps: f32 = if let Some((num, den)) = fps_str.split_once('/') {
        let n: f32 = num.parse().unwrap_or(30.0);
        let d: f32 = den.parse().unwrap_or(1.0);
        if d > 0.0 { n / d } else { 30.0 }
    } else {
        fps_str.parse().unwrap_or(30.0)
    };

    let duration: f32 = parts.get(3).and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
    let frame_count: usize = parts.get(4)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| {
            if duration > 0.0 && fps > 0.0 {
                (duration * fps).round() as usize
            } else {
                0
            }
        });

    Ok((width, height, frame_count, fps))
}

pub fn probe_video_input(path: &std::path::Path) -> Result<VideoProbeInfo, String> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    // 1. Try ffprobe for codec, dimensions, framerate, duration, frame count
    let ffprobe_bin = find_ffprobe_binary();
    let mut probe_cmd = std::process::Command::new(&ffprobe_bin);
    probe_cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .args(&[
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=codec_name,width,height,r_frame_rate,duration,nb_frames",
            "-of", "csv=p=0",
        ])
        .arg(path);
    #[cfg(windows)]
    probe_cmd.creation_flags(CREATE_NO_WINDOW);

    let (p_tx, p_rx) = crossbeam_channel::bounded(1);
    let probe_bin_display = ffprobe_bin.display().to_string();
    std::thread::spawn(move || {
        let res = probe_cmd.output();
        let _ = p_tx.send(res);
    });

    let output = match p_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Err(format!("FFmpeg/FFprobe not found on system PATH ({}). Install FFmpeg to import video files directly, or supply an image sequence (PNG, TIFF, JPEG).", e));
        }
        Err(_) => {
            return Err(format!("Timed out inspecting video metadata using ffprobe ({})", probe_bin_display));
        }
    };

    if !output.status.success() {
        let err_text = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Could not inspect video file using ffprobe: {}", err_text.trim()));
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
    let ffmpeg_bin = find_ffmpeg_binary();
    let mut thumb_cmd = std::process::Command::new(&ffmpeg_bin);
    thumb_cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .args(&["-nostdin", "-an", "-sn", "-v", "error", "-ss", "00:00:00", "-i"])
        .arg(path)
        .args(&["-vframes", "1", "-vf", "scale=min(640\\,iw):-2", "-f", "image2pipe", "-vcodec", "png", "-"]);
    #[cfg(windows)]
    thumb_cmd.creation_flags(CREATE_NO_WINDOW);

    let (t_tx, t_rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let res = thumb_cmd.output();
        let _ = t_tx.send(res);
    });

    let thumbnail_rgba = match t_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(thumb_out)) if thumb_out.status.success() && !thumb_out.stdout.is_empty() => {
            if let Ok(img) = image::load_from_memory(&thumb_out.stdout) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Some((w, h, rgba.into_raw()))
            } else {
                None
            }
        }
        _ => None,
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

/// Player engine for generic video formats (H.264 MP4, HEVC, MKV, AVI, WebM, ProRes)
/// streaming raw decoded video frames over anonymous OS pipes in real time.
pub struct GenericVideoPlayer {
    pub path: PathBuf,
    pub original_width: usize,
    pub original_height: usize,
    pub play_width: usize,
    pub play_height: usize,
    pub fps: f32,
    pub total_frames: usize,
    #[allow(dead_code)]
    pub duration_secs: f32,
    pub codec: String,

    frame_rx: Option<Receiver<(usize, Vec<u8>)>>,
    cancel_flag: Option<Arc<AtomicBool>>,
    child_handle: Option<Arc<Mutex<Option<std::process::Child>>>>,
}

impl GenericVideoPlayer {
    pub fn new(path: PathBuf, probe: &VideoProbeInfo) -> Self {
        let original_width = probe.width;
        let original_height = probe.height;

        // For preview texture, if dimensions exceed 1920x1080, scale down to 1080p
        // to maintain 60+ FPS decode throughput and low GPU texture upload overhead.
        let (play_width, play_height) = if original_width > 1920 || original_height > 1080 {
            let aspect = original_width as f32 / original_height.max(1) as f32;
            let w = 1920.min(original_width);
            let h = ((w as f32 / aspect).round() as usize) & !1;
            (w, h.max(2))
        } else {
            (original_width, original_height)
        };

        Self {
            path,
            original_width,
            original_height,
            play_width,
            play_height,
            fps: probe.fps.max(1.0),
            total_frames: probe.frame_count,
            duration_secs: probe.duration_secs,
            codec: probe.codec.clone(),
            frame_rx: None,
            cancel_flag: None,
            child_handle: None,
        }
    }

    pub fn start_playback(&mut self, start_frame: usize) {
        self.stop_playback();

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Some(cancel.clone());

        let child_arc = Arc::new(Mutex::new(None));
        self.child_handle = Some(child_arc.clone());

        let (tx, rx) = crossbeam_channel::bounded(8);
        self.frame_rx = Some(rx);

        let path = self.path.clone();
        let pw = self.play_width;
        let ph = self.play_height;
        let orig_w = self.original_width;
        let orig_h = self.original_height;
        let fps = self.fps;
        let total = self.total_frames;

        std::thread::Builder::new()
            .name("generic-video-player".to_string())
            .spawn(move || {
                #[cfg(windows)]
                use std::os::windows::process::CommandExt;
                #[cfg(windows)]
                const CREATE_NO_WINDOW: u32 = 0x08000000;

                let start_sec = (start_frame as f64) / (fps as f64);
                let ffmpeg_bin = find_ffmpeg_binary();
                let mut cmd = std::process::Command::new(&ffmpeg_bin);
                cmd.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());

                cmd.args(&[
                    "-nostdin", "-an", "-sn", "-v", "error",
                    "-hwaccel", "auto",
                    "-threads", "0",
                    "-sws_flags", "fast_bilinear",
                ]);
                if start_sec > 0.04 {
                    cmd.args(&["-ss", &format!("{:.3}", start_sec)]);
                }
                cmd.arg("-i").arg(&path);

                if pw != orig_w || ph != orig_h {
                    cmd.args(&["-vf", &format!("scale={}:{}", pw, ph)]);
                }

                cmd.args(&["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);

                #[cfg(windows)]
                cmd.creation_flags(CREATE_NO_WINDOW);

                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(_) => return,
                };

                let stdout = match child.stdout.take() {
                    Some(s) => s,
                    None => {
                        let _ = child.kill();
                        return;
                    }
                };

                if let Ok(mut lock) = child_arc.lock() {
                    *lock = Some(child);
                }

                use std::io::{BufReader, Read};
                let mut reader = BufReader::with_capacity(2 * 1024 * 1024, stdout);
                let frame_size = pw * ph * 4;
                let mut current_idx = start_frame;

                loop {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }

                    let mut buf = vec![0u8; frame_size];
                    match reader.read_exact(&mut buf) {
                        Ok(()) => {
                            if tx.send((current_idx, buf)).is_err() {
                                break;
                            }
                            current_idx += 1;
                            if total > 0 && current_idx >= total {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }

                if let Ok(mut lock) = child_arc.lock() {
                    if let Some(mut c) = lock.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                }
            })
            .ok();
    }

    pub fn stop_playback(&mut self) {
        if let Some(ref cancel) = self.cancel_flag {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(ref child_arc) = self.child_handle {
            if let Ok(mut lock) = child_arc.lock() {
                if let Some(mut c) = lock.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }
        self.frame_rx = None;
        self.cancel_flag = None;
        self.child_handle = None;
    }

    pub fn try_recv_frame(&self) -> Result<(usize, Vec<u8>), crossbeam_channel::TryRecvError> {
        if let Some(ref rx) = self.frame_rx {
            rx.try_recv()
        } else {
            Err(crossbeam_channel::TryRecvError::Disconnected)
        }
    }

    #[allow(dead_code)]
    pub fn is_streaming(&self) -> bool {
        self.frame_rx.is_some()
    }

    #[allow(dead_code)]
    pub fn fetch_single_frame(&self, frame_idx: usize) -> Option<Vec<u8>> {
        let sec = (frame_idx as f64) / (self.fps as f64);
        let ffmpeg_bin = find_ffmpeg_binary();
        let mut cmd = std::process::Command::new(&ffmpeg_bin);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        cmd.args(&[
            "-nostdin", "-an", "-sn", "-v", "error",
            "-hwaccel", "auto",
            "-threads", "0",
            "-ss", &format!("{:.3}", sec),
            "-i",
        ])
        .arg(&self.path);

        if self.play_width != self.original_width || self.play_height != self.original_height {
            cmd.args(&["-vf", &format!("scale={}:{}", self.play_width, self.play_height)]);
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

        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let res = cmd.output();
            let _ = tx.send(res);
        });

        let expected_len = self.play_width * self.play_height * 4;
        match rx.recv_timeout(Duration::from_millis(1200)) {
            Ok(Ok(out)) if out.stdout.len() >= expected_len => {
                let mut data = out.stdout;
                data.truncate(expected_len);
                Some(data)
            }
            _ => None,
        }
    }
}

impl Drop for GenericVideoPlayer {
    fn drop(&mut self) {
        self.stop_playback();
    }
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
                let save_res = if format == "jpg" || format == "jpeg" {
                    image::DynamicImage::ImageRgba8(img).into_rgb8().save(&out_file)
                } else {
                    img.save(&out_file)
                };
                if let Err(e) = save_res {
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
    let (width, height, total) = if let Some((w, h)) = config.video_dimensions {
        (w, h, config.total_frames.unwrap_or(1))
    } else {
        match probe_video_dimensions(&config.input_dir) {
            Ok((w, h, frames, _fps)) => (w, h, frames.max(1)),
            Err(e) => {
                let _ = progress_tx.send(WorkerProgress::Error(e));
                return;
            }
        }
    };

    if width % 4 != 0 || height % 4 != 0 {
        let _ = progress_tx.send(WorkerProgress::Error(format!(
            "Video dimensions ({}x{}) must be multiples of 4 for HAP texture encoding.",
            width, height
        )));
        return;
    }

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

    let ffmpeg_bin = find_ffmpeg_binary();
    let mut cmd = std::process::Command::new(&ffmpeg_bin);
    cmd.args(&[
        "-nostdin", "-v", "error",
        "-hwaccel", "auto",
        "-threads", "0",
        "-i",
    ])
        .arg(&config.input_dir)
        .args(&["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = match cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = progress_tx.send(WorkerProgress::Error(format!(
                "Failed to spawn ffmpeg ({}): {}",
                ffmpeg_bin.display(),
                e
            )));
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

    let status = child.wait();
    if current_frame == 0 {
        let mut err_detail = String::new();
        if let Some(mut stderr_pipe) = child.stderr.take() {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf);
            err_detail = String::from_utf8_lossy(&buf).trim().to_string();
        }
        let reason = if !err_detail.is_empty() {
            err_detail
        } else {
            format!("FFmpeg exited with {:?} but delivered 0 video frames.", status)
        };
        let _ = progress_tx.send(WorkerProgress::Error(format!("Video transcode failed: {}", reason)));
        return;
    }

    if let Err(e) = writer.finalize() {
        let _ = progress_tx.send(WorkerProgress::Error(format!("Finalize error: {}", e)));
        return;
    }

    let total_secs = start_time.elapsed().as_secs_f32();
    let _ = progress_tx.send(WorkerProgress::Finished {
        message: format!("Successfully encoded {} frames from video to {:?} ({:.2}s)", current_frame, config.output_file, total_secs),
    });
}

/// Check if a path corresponds to a supported still image format.
pub fn is_image_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" | "webp" | "tga"
            )
        })
        .unwrap_or(false)
}

/// Ensure RGBA dimensions are multiples of 4 for DXT texture compression.
/// If already aligned, returns a copy without padding overhead.
pub fn pad_rgba_to_multiple_of_4(src: &[u8], width: usize, height: usize) -> (usize, usize, Vec<u8>) {
    let pad_w = (width + 3) & !3;
    let pad_h = (height + 3) & !3;
    if pad_w == width && pad_h == height {
        return (width, height, src.to_vec());
    }
    let mut padded = vec![0u8; pad_w * pad_h * 4];
    for y in 0..height {
        let src_row = &src[y * width * 4..(y + 1) * width * 4];
        let dst_row = &mut padded[y * pad_w * 4..y * pad_w * 4 + width * 4];
        dst_row.copy_from_slice(src_row);
    }
    (pad_w, pad_h, padded)
}

/// Export a still image RGBA buffer to standard image file formats (PNG, JPEG, TIFF, WebP, BMP).
pub fn export_image_to_file(
    rgba: &[u8],
    width: usize,
    height: usize,
    dest: &std::path::Path,
) -> Result<(), String> {
    let img = image::RgbaImage::from_raw(width as u32, height as u32, rgba.to_vec())
        .ok_or_else(|| "Failed to construct RGBA image buffer".to_string())?;

    let ext = dest
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    if matches!(ext.as_str(), "jpg" | "jpeg") {
        let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
        rgb.save(dest)
            .map_err(|e| format!("Failed to save JPEG: {}", e))
    } else {
        img.save(dest)
            .map_err(|e| format!("Failed to save image: {}", e))
    }
}

/// Export a still image RGBA buffer to a QuickTime HAP MOV file using pure Rust encoders.
pub fn export_image_to_hap_mov(
    rgba: &[u8],
    width: usize,
    height: usize,
    dest: &std::path::Path,
    format: HapFormat,
    snappy: bool,
) -> Result<(), String> {
    let (pad_w, pad_h, padded) = pad_rgba_to_multiple_of_4(rgba, width, height);
    let opts = EncodeOptions {
        format,
        quality: QualityPreset::Production,
        chunk_count: 4,
        use_snappy: snappy,
        color_range: ColorRange::Full,
        alpha_mode: AlphaMode::Straight,
        dither_mode: DitherMode::None,
    };
    let packet = encode_frame_with_options(&padded, pad_w, pad_h, &opts)
        .map_err(|e| format!("HAP encoding failed: {}", e))?;

    let video_cfg = VideoConfig::new(pad_w as u32, pad_h as u32, 30.0, format);
    let mut writer = QtHapWriter::create(dest, video_cfg)
        .map_err(|e| format!("Failed to create MOV file: {}", e))?;
    writer.write_frame(&packet)
        .map_err(|e| format!("Failed to write frame: {}", e))?;
    writer.finalize()
        .map_err(|e| format!("Failed to finalize MOV file: {}", e))?;
    Ok(())
}

/// Asynchronously load media (HAP MOV, Still Image, Generic Video, or Image Sequence)
/// off the GUI thread to eliminate player freezes.
pub fn spawn_media_loader(path: PathBuf, tx: Sender<MediaLoadResult>) {
    std::thread::Builder::new()
        .name("media-loader".to_string())
        .spawn(move || {
            // 1. Directory -> image sequence
            if path.is_dir() {
                if let Ok(entries) = fs::read_dir(&path) {
                    let mut files: Vec<PathBuf> = entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| is_image_file(p))
                        .collect();
                    files.sort();
                    let count = files.len();
                    let first_file = files.first().cloned();
                    let last_file = files.last().cloned();

                    let mut thumb_res = None;
                    let mut w = 0;
                    let mut h = 0;
                    if let Some(ref first) = first_file {
                        if let Ok(img) = image::open(first) {
                            w = img.width() as usize;
                            h = img.height() as usize;
                            let thumb = img.thumbnail(320, 320).to_rgba8();
                            thumb_res = Some((thumb.width() as usize, thumb.height() as usize, thumb.into_raw()));
                        }
                    }

                    let _ = tx.send(MediaLoadResult::ImageSequence {
                        path,
                        count,
                        width: w,
                        height: h,
                        first_file,
                        last_file,
                        thumbnail_rgba: thumb_res,
                    });
                    return;
                }
            }

            // 2. Still image check
            if is_image_file(&path) {
                match image::open(&path) {
                    Ok(img) => {
                        let w = img.width() as usize;
                        let h = img.height() as usize;
                        let rgba = img.to_rgba8().into_raw();
                        let _ = tx.send(MediaLoadResult::StillImage {
                            path,
                            width: w,
                            height: h,
                            rgba,
                        });
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(MediaLoadResult::Failed {
                            path,
                            error: format!("Failed to decode image: {}", e),
                        });
                        return;
                    }
                }
            }

            // 3. QuickTime HAP MOV check (only attempt MOV parser on .mov files)
            let is_mov = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("mov"))
                .unwrap_or(false);

            if is_mov {
                if let Ok(mut reader) = QtHapReader::open(&path) {
                    let file_size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let summary = extract_stream_summary(&mut reader, file_size).ok();
                    let (first_rgba, decode_ms, pkt_bytes) = if let Ok(pkt) = reader.read_frame_packet(0) {
                        let p_len = pkt.len();
                        let t0 = Instant::now();
                        let rgba = decode_frame_to_rgba(&pkt, reader.width() as usize, reader.height() as usize).ok();
                        let ms = t0.elapsed().as_secs_f32() * 1000.0;
                        (rgba, ms, p_len)
                    } else {
                        (None, 0.0, 0)
                    };

                    let _ = tx.send(MediaLoadResult::HapVideo {
                        path,
                        reader,
                        summary,
                        first_frame_rgba: first_rgba,
                        first_frame_decode_ms: decode_ms,
                        first_packet_bytes: pkt_bytes,
                    });
                    return;
                }
            }

            // 4. Video container check (MP4, MKV, AVI, WebM, non-HAP MOV, etc.)
            if is_video_container(&path) {
                match probe_video_input(&path) {
                    Ok(probe) => {
                        let _ = tx.send(MediaLoadResult::GenericVideo {
                            path,
                            probe,
                        });
                        return;
                    }
                    Err(probe_err) => {
                        let _ = tx.send(MediaLoadResult::Failed {
                            path,
                            error: format!("Unable to parse video stream: {}", probe_err),
                        });
                        return;
                    }
                }
            }

            // 5. Fallback: attempt image decode in case extension was missing
            if let Ok(img) = image::open(&path) {
                let w = img.width() as usize;
                let h = img.height() as usize;
                let rgba = img.to_rgba8().into_raw();
                let _ = tx.send(MediaLoadResult::StillImage {
                    path,
                    width: w,
                    height: h,
                    rgba,
                });
                return;
            }

            let _ = tx.send(MediaLoadResult::Failed {
                path,
                error: "Unrecognized or unsupported media format".to_string(),
            });
        })
        .ok();
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

    #[test]
    fn test_image_extensions() {
        assert!(is_image_file(std::path::Path::new("test.png")));
        assert!(is_image_file(std::path::Path::new("photo.jpg")));
        assert!(is_image_file(std::path::Path::new("picture.jpeg")));
        assert!(is_image_file(std::path::Path::new("scan.tiff")));
        assert!(is_image_file(std::path::Path::new("graphic.webp")));
        assert!(is_image_file(std::path::Path::new("icon.bmp")));
        assert!(!is_image_file(std::path::Path::new("movie.mov")));
        assert!(!is_image_file(std::path::Path::new("video.mp4")));
    }

    #[test]
    fn test_pad_rgba_to_multiple_of_4() {
        let raw = vec![255u8; 6 * 5 * 4];
        let (pw, ph, padded) = pad_rgba_to_multiple_of_4(&raw, 6, 5);
        assert_eq!(pw, 8);
        assert_eq!(ph, 8);
        assert_eq!(padded.len(), 8 * 8 * 4);

        let aligned = vec![128u8; 8 * 8 * 4];
        let (aw, ah, aligned_res) = pad_rgba_to_multiple_of_4(&aligned, 8, 8);
        assert_eq!(aw, 8);
        assert_eq!(ah, 8);
        assert_eq!(aligned_res.len(), 8 * 8 * 4);
    }

    #[test]
    fn test_export_still_image_and_hap_mov() {
        let temp_dir = std::env::temp_dir().join("haplab_test_img_export");
        let _ = fs::create_dir_all(&temp_dir);

        let w = 8;
        let h = 8;
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                rgba.push((x * 30) as u8);
                rgba.push((y * 30) as u8);
                rgba.push(180);
                rgba.push(255);
            }
        }

        // Export to PNG
        let png_path = temp_dir.join("test_out.png");
        assert!(export_image_to_file(&rgba, w, h, &png_path).is_ok());
        assert!(png_path.exists());

        // Export to JPEG
        let jpg_path = temp_dir.join("test_out.jpg");
        assert!(export_image_to_file(&rgba, w, h, &jpg_path).is_ok());
        assert!(jpg_path.exists());

        // Export to HAP MOV
        let mov_path = temp_dir.join("test_out.mov");
        assert!(export_image_to_hap_mov(&rgba, w, h, &mov_path, HapFormat::HapY, true).is_ok());
        assert!(mov_path.exists());

        // Verify QtHapReader can open the generated single-frame HAP MOV
        let mut reader = QtHapReader::open(&mov_path).expect("Should open generated MOV");
        assert_eq!(reader.frame_count(), 1);
        assert_eq!(reader.width(), 8);
        assert_eq!(reader.height(), 8);
        let pkt = reader.read_frame_packet(0).expect("Should read packet 0");
        let decoded = decode_frame_to_rgba(&pkt, 8, 8).expect("Should decode packet");
        assert_eq!(decoded.len(), 8 * 8 * 4);

        let _ = fs::remove_file(png_path);
        let _ = fs::remove_file(jpg_path);
        let _ = fs::remove_file(mov_path);
        let _ = fs::remove_dir(temp_dir);
    }

    #[test]
    fn test_find_and_spawn_ffmpeg() {
        let ffmpeg_bin = find_ffmpeg_binary();
        let ffprobe_bin = find_ffprobe_binary();
        assert!(!ffmpeg_bin.as_os_str().is_empty());
        assert!(!ffprobe_bin.as_os_str().is_empty());

        #[cfg(windows)]
        use std::os::windows::process::CommandExt;
        #[cfg(windows)]
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let mut cmd = std::process::Command::new(&ffmpeg_bin);
        cmd.args(&["-nostdin", "-v", "error", "-version"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let res = cmd.spawn();
        assert!(res.is_ok(), "Spawning ffmpeg should succeed without error: {:?}", res);
    }

    #[test]
    fn test_transcode_mp4_to_hap_mov() {
        let ffmpeg_bin = find_ffmpeg_binary();
        let temp_dir = std::env::temp_dir().join("haplab_test_mp4_transcode");
        let _ = fs::create_dir_all(&temp_dir);
        let mp4_path = temp_dir.join("input.mp4");
        let mov_path = temp_dir.join("output.mov");

        #[cfg(windows)]
        use std::os::windows::process::CommandExt;
        #[cfg(windows)]
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // 1. Generate test MP4
        let mut gen_cmd = std::process::Command::new(&ffmpeg_bin);
        gen_cmd.args(&[
            "-nostdin",
            "-y",
            "-f", "lavfi",
            "-i", "testsrc=size=64x64:rate=30",
            "-vframes", "4",
        ])
        .arg(&mp4_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        gen_cmd.creation_flags(CREATE_NO_WINDOW);

        if let Ok(status) = gen_cmd.status() {
            if !status.success() {
                // If ffmpeg cannot encode H.264 testsrc in this environment, skip gracefully
                return;
            }
        } else {
            return;
        }

        assert!(mp4_path.exists());

        // 2. Test probe_video_dimensions
        let (w, h, frames, fps) = probe_video_dimensions(&mp4_path).expect("probe_video_dimensions should succeed");
        assert_eq!(w, 64);
        assert_eq!(h, 64);
        assert_eq!(frames, 4);
        assert!((fps - 30.0).abs() < 0.1);

        // 3. Test spawn_encode_from_video
        let config = EncodeJobConfig {
            input_dir: mp4_path.clone(),
            output_file: mov_path.clone(),
            format: HapFormat::HapY,
            fps,
            chunks: 2,
            snappy: true,
            color_range: ColorRange::Full,
            alpha_mode: AlphaMode::Straight,
            dither_mode: DitherMode::None,
            quality: QualityPreset::Production,
            video_dimensions: Some((w, h)),
            total_frames: Some(frames),
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::unbounded();
        spawn_encode_from_video(config, cancel, tx);

        let mut finished = false;
        while let Ok(msg) = rx.recv() {
            match msg {
                WorkerProgress::Finished { .. } => {
                    finished = true;
                    break;
                }
                WorkerProgress::Error(e) => {
                    panic!("Encode failed with error: {}", e);
                }
                _ => {}
            }
        }

        assert!(finished, "Transcode should complete successfully");
        assert!(mov_path.exists(), "Output MOV should exist");

        // 4. Verify the generated HAP MOV with QtHapReader
        let mut reader = QtHapReader::open(&mov_path).expect("Should open transcode MOV");
        assert_eq!(reader.frame_count(), 4);
        assert_eq!(reader.width(), 64);
        assert_eq!(reader.height(), 64);
        let pkt = reader.read_frame_packet(0).expect("Should read frame 0");
        let rgba = decode_frame_to_rgba(&pkt, 64, 64).expect("Should decode frame 0 to RGBA");
        assert_eq!(rgba.len(), 64 * 64 * 4);

        let _ = fs::remove_file(mp4_path);
        let _ = fs::remove_file(mov_path);
        let _ = fs::remove_dir(temp_dir);
    }

    #[test]
    fn test_spawn_media_loader_mp4() {
        let ffmpeg_bin = find_ffmpeg_binary();
        let temp_dir = std::env::temp_dir().join("haplab_test_loader_mp4");
        let _ = fs::create_dir_all(&temp_dir);
        let mp4_path = temp_dir.join("loader_test.mp4");

        #[cfg(windows)]
        use std::os::windows::process::CommandExt;
        #[cfg(windows)]
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let mut gen_cmd = std::process::Command::new(&ffmpeg_bin);
        gen_cmd.args(&[
            "-nostdin",
            "-y",
            "-f", "lavfi",
            "-i", "testsrc=size=64x64:rate=30",
            "-vframes", "2",
        ])
        .arg(&mp4_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        gen_cmd.creation_flags(CREATE_NO_WINDOW);

        if let Ok(status) = gen_cmd.status() {
            if !status.success() {
                return;
            }
        } else {
            return;
        }

        let (tx, rx) = crossbeam_channel::unbounded();
        spawn_media_loader(mp4_path.clone(), tx);

        let result = rx.recv_timeout(Duration::from_secs(6)).expect("Loader should respond within timeout");
        match result {
            MediaLoadResult::GenericVideo { path, probe } => {
                assert_eq!(path, mp4_path);
                assert_eq!(probe.width, 64);
                assert_eq!(probe.height, 64);
                assert_eq!(probe.frame_count, 2);
            }
            other => panic!("Expected GenericVideo result, got {:?}", std::mem::discriminant(&other)),
        }

        let _ = fs::remove_file(mp4_path);
        let _ = fs::remove_dir(temp_dir);
    }
}


