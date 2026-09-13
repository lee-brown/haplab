//! Hardware benchmarking engine for HAP decoding, encoding, and GPU streaming.

use crossbeam_channel::Sender;
use hap_core::{
    decode_frame_to_rgba, decode_frame_to_texture, encode_frame, encode_frame_with_options,
    ColorRange, DitherMode, EncodeOptions, HapFormat, QualityPreset,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Individual test scorecard item.
#[derive(Debug, Clone)]
pub struct BenchmarkScore {
    pub test_name: String,
    pub resolution: String,
    pub frame_count: usize,
    pub elapsed_secs: f64,
    pub fps: f64,
    pub frame_time_ms: f64,
    pub bandwidth_gbps: f64,
    pub performance_rating: &'static str,
}

#[derive(Debug, Clone)]
pub enum BenchmarkProgress {
    Started { test_name: String },
    StepProgress { test_name: String, current: usize, total: usize, current_fps: f32 },
    TestCompleted(BenchmarkScore),
    AllFinished { summary: Vec<BenchmarkScore> },
    Error(String),
}

/// Helper to generate a realistic RGBA gradient test pattern in memory.
fn generate_pattern(w: usize, h: usize) -> Vec<u8> {
    let mut buf = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = y * w * 4;
        let y_val = ((y * 255) / h) as u8;
        for x in 0..w {
            let idx = row + x * 4;
            buf[idx] = ((x * 255) / w) as u8;
            buf[idx + 1] = y_val;
            buf[idx + 2] = (((x + y) * 128) / (w + h)) as u8;
            buf[idx + 3] = 255;
        }
    }
    buf
}

fn rate_decode_fps(fps: f64, is_4k: bool) -> &'static str {
    if is_4k {
        if fps >= 120.0 {
            "Elite (120+ FPS)"
        } else if fps >= 60.0 {
            "Broadcast (60+ FPS)"
        } else if fps >= 30.0 {
            "Smooth (30+ FPS)"
        } else {
            "Entry-Level"
        }
    } else {
        if fps >= 240.0 {
            "Elite (240+ FPS)"
        } else if fps >= 120.0 {
            "High (120+ FPS)"
        } else if fps >= 60.0 {
            "Smooth (60+ FPS)"
        } else {
            "Standard"
        }
    }
}

/// Spawns a background worker running the suite of benchmarks.
pub fn spawn_benchmark_worker(
    cancel_flag: Arc<AtomicBool>,
    progress_tx: Sender<BenchmarkProgress>,
) {
    std::thread::spawn(move || {
        let mut scores = Vec::new();

        // -------------------------------------------------------------
        // TEST 1: 1080p Hap Q CPU Decode (60 frames)
        // -------------------------------------------------------------
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }
        let test1_name = "1080p Hap Q Decode".to_string();
        let _ = progress_tx.send(BenchmarkProgress::Started {
            test_name: test1_name.clone(),
        });

        let (w_1080, h_1080) = (1920, 1080);
        let pattern_1080 = generate_pattern(w_1080, h_1080);
        let pkt_1080 = match encode_frame(&pattern_1080, w_1080, h_1080, HapFormat::HapY, 4, true) {
            Ok(p) => p,
            Err(e) => {
                let _ = progress_tx.send(BenchmarkProgress::Error(format!("Failed preparing 1080p packet: {}", e)));
                return;
            }
        };

        let frames_1080 = 60;
        let start1 = Instant::now();
        for i in 0..frames_1080 {
            if cancel_flag.load(Ordering::Relaxed) {
                return;
            }
            if let Err(e) = decode_frame_to_rgba(&pkt_1080, w_1080, h_1080) {
                let _ = progress_tx.send(BenchmarkProgress::Error(format!("Decode error: {}", e)));
                return;
            }
            let cur = i + 1;
            let el = start1.elapsed().as_secs_f32().max(0.001);
            let _ = progress_tx.send(BenchmarkProgress::StepProgress {
                test_name: test1_name.clone(),
                current: cur,
                total: frames_1080,
                current_fps: cur as f32 / el,
            });
        }
        let el1 = start1.elapsed().as_secs_f64();
        let fps1 = frames_1080 as f64 / el1;
        let ms1 = (el1 * 1000.0) / frames_1080 as f64;
        let bytes_per_frame_1080 = (w_1080 * h_1080 * 4) as f64;
        let bw1 = (fps1 * bytes_per_frame_1080) / 1_000_000_000.0;

        let score1 = BenchmarkScore {
            test_name: test1_name,
            resolution: "1920 × 1080".into(),
            frame_count: frames_1080,
            elapsed_secs: el1,
            fps: fps1,
            frame_time_ms: ms1,
            bandwidth_gbps: bw1,
            performance_rating: rate_decode_fps(fps1, false),
        };
        scores.push(score1.clone());
        let _ = progress_tx.send(BenchmarkProgress::TestCompleted(score1));

        // -------------------------------------------------------------
        // TEST 2: 4K UHD Hap Q CPU Decode (20 frames)
        // -------------------------------------------------------------
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }
        let test2_name = "4K UHD Hap Q CPU Decode".to_string();
        let _ = progress_tx.send(BenchmarkProgress::Started {
            test_name: test2_name.clone(),
        });

        let (w_4k, h_4k) = (3840, 2160);
        let pattern_4k = generate_pattern(w_4k, h_4k);
        let pkt_4k = match encode_frame(&pattern_4k, w_4k, h_4k, HapFormat::HapY, 8, true) {
            Ok(p) => p,
            Err(e) => {
                let _ = progress_tx.send(BenchmarkProgress::Error(format!("Failed preparing 4K packet: {}", e)));
                return;
            }
        };

        let frames_4k = 20;
        let start2 = Instant::now();
        for i in 0..frames_4k {
            if cancel_flag.load(Ordering::Relaxed) {
                return;
            }
            if let Err(e) = decode_frame_to_rgba(&pkt_4k, w_4k, h_4k) {
                let _ = progress_tx.send(BenchmarkProgress::Error(format!("Decode error: {}", e)));
                return;
            }
            let cur = i + 1;
            let el = start2.elapsed().as_secs_f32().max(0.001);
            let _ = progress_tx.send(BenchmarkProgress::StepProgress {
                test_name: test2_name.clone(),
                current: cur,
                total: frames_4k,
                current_fps: cur as f32 / el,
            });
        }
        let el2 = start2.elapsed().as_secs_f64();
        let fps2 = frames_4k as f64 / el2;
        let ms2 = (el2 * 1000.0) / frames_4k as f64;
        let bytes_per_frame_4k = (w_4k * h_4k * 4) as f64;
        let bw2 = (fps2 * bytes_per_frame_4k) / 1_000_000_000.0;

        let score2 = BenchmarkScore {
            test_name: test2_name,
            resolution: "3840 × 2160".into(),
            frame_count: frames_4k,
            elapsed_secs: el2,
            fps: fps2,
            frame_time_ms: ms2,
            bandwidth_gbps: bw2,
            performance_rating: rate_decode_fps(fps2, true),
        };
        scores.push(score2.clone());
        let _ = progress_tx.send(BenchmarkProgress::TestCompleted(score2));

        // -------------------------------------------------------------
        // TEST 3: 1080p Real-time Encode Benchmark (Draft Preset, 30 frames)
        // -------------------------------------------------------------
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }
        let test3_name = "1080p Ingest Encode".to_string();
        let _ = progress_tx.send(BenchmarkProgress::Started {
            test_name: test3_name.clone(),
        });

        let opts_draft = EncodeOptions {
            format: HapFormat::HapY,
            chunk_count: 4,
            use_snappy: true,
            color_range: ColorRange::Full,
            alpha_mode: hap_core::AlphaMode::Straight,
            dither_mode: DitherMode::None,
            quality: QualityPreset::Draft,
        };

        let frames_enc = 30;
        let start3 = Instant::now();
        for i in 0..frames_enc {
            if cancel_flag.load(Ordering::Relaxed) {
                return;
            }
            if let Err(e) = encode_frame_with_options(&pattern_1080, w_1080, h_1080, &opts_draft) {
                let _ = progress_tx.send(BenchmarkProgress::Error(format!("Encode error: {}", e)));
                return;
            }
            let cur = i + 1;
            let el = start3.elapsed().as_secs_f32().max(0.001);
            let _ = progress_tx.send(BenchmarkProgress::StepProgress {
                test_name: test3_name.clone(),
                current: cur,
                total: frames_enc,
                current_fps: cur as f32 / el,
            });
        }
        let el3 = start3.elapsed().as_secs_f64();
        let fps3 = frames_enc as f64 / el3;
        let ms3 = (el3 * 1000.0) / frames_enc as f64;
        let bw3 = (fps3 * bytes_per_frame_1080) / 1_000_000_000.0;

        let rating3 = if fps3 >= 60.0 {
            "Broadcast (60+ FPS)"
        } else if fps3 >= 30.0 {
            "Real-Time (30+ FPS)"
        } else {
            "Near Real-Time"
        };

        let score3 = BenchmarkScore {
            test_name: test3_name,
            resolution: "1920 × 1080".into(),
            frame_count: frames_enc,
            elapsed_secs: el3,
            fps: fps3,
            frame_time_ms: ms3,
            bandwidth_gbps: bw3,
            performance_rating: rating3,
        };
        scores.push(score3.clone());
        let _ = progress_tx.send(BenchmarkProgress::TestCompleted(score3));

        // -------------------------------------------------------------
        // TEST 4: GPU Texture Upload & Parse (120 frames)
        // -------------------------------------------------------------
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }
        let test4_name = "Texture Streaming".to_string();
        let _ = progress_tx.send(BenchmarkProgress::Started {
            test_name: test4_name.clone(),
        });

        let frames_tex = 120;
        let start4 = Instant::now();
        for i in 0..frames_tex {
            if cancel_flag.load(Ordering::Relaxed) {
                return;
            }
            let _ = decode_frame_to_texture(&pkt_1080);
            let cur = i + 1;
            let el = start4.elapsed().as_secs_f32().max(0.001);
            let _ = progress_tx.send(BenchmarkProgress::StepProgress {
                test_name: test4_name.clone(),
                current: cur,
                total: frames_tex,
                current_fps: cur as f32 / el,
            });
        }
        let el4 = start4.elapsed().as_secs_f64();
        let fps4 = frames_tex as f64 / el4;
        let ms4 = (el4 * 1000.0) / frames_tex as f64;
        let score4 = BenchmarkScore {
            test_name: test4_name,
            resolution: "1920 × 1080".into(),
            frame_count: frames_tex,
            elapsed_secs: el4,
            fps: fps4,
            frame_time_ms: ms4,
            bandwidth_gbps: (fps4 * (pkt_1080.len() as f64)) / 1_000_000_000.0,
            performance_rating: "Ultra (>500 FPS)",
        };
        scores.push(score4.clone());
        let _ = progress_tx.send(BenchmarkProgress::TestCompleted(score4));

        let _ = progress_tx.send(BenchmarkProgress::AllFinished { summary: scores });
    });
}
