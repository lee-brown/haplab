//! # hap-core
//!
//! A 100% pure Rust, zero-dependency implementation of the HAP video codec specification.
//! Supports all flavours of HAP:
//! - **Hap 1 (`Hap1`)**: Standard RGB DXT1/BC1
//! - **Hap Alpha (`Hap5`)**: RGBA DXT5/BC3
//! - **Hap Q (`HapY`)**: Scaled YCoCg in DXT5/BC3 for high color fidelity
//! - **Hap Q Alpha (`HapM`)**: Dual-stream container with YCoCg + BC4 alpha
//! - **Hap Alpha-Only (`HapA`)**: BC4 single-channel matte
//! - **Hap R / Hap 7 (`Hap7`, `HapR`)**: High quality RGBA BC7 UNORM
//! - **Hap HDR (`HapH`)**: High Dynamic Range BC6H float
//!
//! Includes pure Rust QuickTime MOV container demuxing and muxing, Snappy compression,
//! and parallel multi-chunk processing via Rayon.

pub mod bc6h;
pub mod bc7;
pub mod color;
pub mod decoder;
pub mod dxt;
pub mod encoder;
pub mod format;
pub mod header;
pub mod inspector;
pub mod mov;
pub mod snappy;
pub mod ycocg;

pub use color::{preprocess_rgba, AlphaMode, ColorRange, DitherMode, EncodeOptions, QualityPreset};
pub use decoder::{decode_frame_to_rgba, decode_frame_to_texture, DecodeError, RawTextureFrame};
pub use encoder::{encode_frame, encode_frame_with_options, EncodeError};
pub use format::HapFormat;
pub use header::{ChunkInfo, DecodeInstructions, SectionHeader};
pub use inspector::{audit_hap_stream, extract_stream_summary, FaultSeverity, StreamAudit, StreamFault, StreamSummary};
pub use mov::{FrameSample, MovReaderError, MovWriterError, QtHapReader, QtHapWriter, VideoConfig};

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_test_pattern(width: usize, height: usize, with_alpha: bool) -> Vec<u8> {
        let mut buf = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) * 4;
                buf[idx] = ((x * 255) / width) as u8;          // Red gradient
                buf[idx + 1] = ((y * 255) / height) as u8;      // Green gradient
                buf[idx + 2] = (((x + y) * 128) / (width + height)) as u8; // Blue
                buf[idx + 3] = if with_alpha { ((x * 255) / width) as u8 } else { 255 };
            }
        }
        buf
    }

    #[test]
    fn test_hap1_roundtrip() {
        let (w, h) = (16, 16);
        let rgba = generate_test_pattern(w, h, false);
        let packet = encode_frame(&rgba, w, h, HapFormat::Hap1, 1, true).unwrap();
        let decoded = decode_frame_to_rgba(&packet, w, h).unwrap();
        assert_eq!(decoded.len(), rgba.len());
    }

    #[test]
    fn test_hap5_roundtrip() {
        let (w, h) = (16, 16);
        let rgba = generate_test_pattern(w, h, true);
        let packet = encode_frame(&rgba, w, h, HapFormat::Hap5, 1, true).unwrap();
        let decoded = decode_frame_to_rgba(&packet, w, h).unwrap();
        assert_eq!(decoded.len(), rgba.len());
    }

    #[test]
    fn test_hapy_roundtrip() {
        let (w, h) = (16, 16);
        let rgba = generate_test_pattern(w, h, false);
        let packet = encode_frame(&rgba, w, h, HapFormat::HapY, 1, true).unwrap();
        let decoded = decode_frame_to_rgba(&packet, w, h).unwrap();
        assert_eq!(decoded.len(), rgba.len());
    }

    #[test]
    fn test_hapr_bc7_roundtrip() {
        let (w, h) = (16, 16);
        let rgba = generate_test_pattern(w, h, true);
        let packet = encode_frame(&rgba, w, h, HapFormat::Hap7, 1, true).unwrap();
        let decoded = decode_frame_to_rgba(&packet, w, h).unwrap();
        assert_eq!(decoded.len(), rgba.len());
    }

    #[test]
    fn test_hapm_q_alpha_roundtrip() {
        let (w, h) = (16, 16);
        let rgba = generate_test_pattern(w, h, true);
        let packet = encode_frame(&rgba, w, h, HapFormat::HapM, 1, true).unwrap();
        let decoded = decode_frame_to_rgba(&packet, w, h).unwrap();
        assert_eq!(decoded.len(), rgba.len());
    }

    #[test]
    fn test_chunked_multi_thread_roundtrip() {
        let (w, h) = (32, 32);
        let rgba = generate_test_pattern(w, h, true);
        // Encode with 4 chunks
        let packet = encode_frame(&rgba, w, h, HapFormat::Hap5, 4, true).unwrap();
        let decoded = decode_frame_to_rgba(&packet, w, h).unwrap();
        assert_eq!(decoded.len(), rgba.len());
    }

    #[test]
    fn test_mov_full_container_roundtrip() {
        let (w, h) = (16, 16);
        let rgba = generate_test_pattern(w, h, false);

        let temp_dir = tempfile::tempdir().unwrap();
        let mov_path = temp_dir.path().join("test_hap.mov");

        let config = VideoConfig::new(w as u32, h as u32, 30.0, HapFormat::HapY);
        let mut writer = QtHapWriter::create(&mov_path, config).unwrap();

        // Write 3 frames
        for _ in 0..3 {
            let packet = encode_frame(&rgba, w, h, HapFormat::HapY, 1, true).unwrap();
            writer.write_frame(&packet).unwrap();
        }
        writer.finalize().unwrap();

        // Now read back with QtHapReader
        let mut reader = QtHapReader::open(&mov_path).unwrap();
        assert_eq!(reader.width(), w as u32);
        assert_eq!(reader.height(), h as u32);
        assert_eq!(reader.format(), HapFormat::HapY);
        assert_eq!(reader.frame_count(), 3);
        assert!((reader.fps() - 30.0).abs() < 0.1);

        for i in 0..3 {
            let packet = reader.read_frame_packet(i).unwrap();
            let decoded = decode_frame_to_rgba(&packet, w, h).unwrap();
            assert_eq!(decoded.len(), rgba.len());
        }
    }

    #[test]
    fn test_encode_frame_with_professional_options() {
        let (w, h) = (16, 16);
        let rgba = generate_test_pattern(w, h, true);

        // Test with Studio Limited range + Premultiplied Alpha + Bayer Dithering + Draft Quality
        let opts = EncodeOptions {
            format: HapFormat::HapY,
            chunk_count: 2,
            use_snappy: true,
            color_range: ColorRange::Limited,
            alpha_mode: AlphaMode::Premultiply,
            dither_mode: DitherMode::Bayer4x4,
            quality: QualityPreset::Draft,
        };
        let packet = encode_frame_with_options(&rgba, w, h, &opts).unwrap();
        let decoded = decode_frame_to_rgba(&packet, w, h).unwrap();
        assert_eq!(decoded.len(), rgba.len());

        // Test with Hap 7 (BC7) + Production Quality + Discard Alpha
        let opts_bc7 = EncodeOptions {
            format: HapFormat::Hap7,
            chunk_count: 1,
            use_snappy: true,
            color_range: ColorRange::Full,
            alpha_mode: AlphaMode::Discard,
            dither_mode: DitherMode::None,
            quality: QualityPreset::Production,
        };
        let packet_bc7 = encode_frame_with_options(&rgba, w, h, &opts_bc7).unwrap();
        let decoded_bc7 = decode_frame_to_rgba(&packet_bc7, w, h).unwrap();
        assert_eq!(decoded_bc7.len(), rgba.len());
        // Verify alpha was forced to 255
        for p in decoded_bc7.chunks_exact(4) {
            assert_eq!(p[3], 255);
        }
    }

    #[test]
    fn test_benchmark_1080p_decode() {
        let mov_path = std::path::Path::new(r"C:\Users\Lee Brown\.gemini\antigravity-cli\brain\0ca19e38-643b-49ec-b073-cf1600c37ab0\scratch\perf_test\perf_1080p_hapy.mov");
        if !mov_path.exists() {
            return;
        }
        let mut reader = QtHapReader::open(mov_path).unwrap();
        let count = reader.frame_count();
        let w = reader.width() as usize;
        let h = reader.height() as usize;

        let start = std::time::Instant::now();
        for i in 0..count {
            let pkt = reader.read_frame_packet(i).unwrap();
            let rgba = decode_frame_to_rgba(&pkt, w, h).unwrap();
            assert_eq!(rgba.len(), w * h * 4);
        }
        let elapsed = start.elapsed();
        let fps = count as f64 / elapsed.as_secs_f64();
        println!("\n>>> HAP-CORE 1080p IN-MEMORY DECODE: {} frames in {:.4}s = {:.1} FPS ({:.2} ms/frame) <<<\n", count, elapsed.as_secs_f64(), fps, (elapsed.as_secs_f64() * 1000.0) / count as f64);
    }

    #[test]
    fn test_8k_resolution_support() {
        let (w, h) = (7680, 4320); // 8K UHD
        assert_eq!(w % 4, 0);
        assert_eq!(h % 4, 0);

        // Allocate 8K frame buffer (132 MB)
        let mut rgba = vec![128u8; w * h * 4];
        rgba[0] = 255;
        rgba[1] = 0;
        rgba[2] = 128;
        rgba[3] = 255;

        let opts = EncodeOptions {
            format: HapFormat::Hap1,
            chunk_count: 8,
            use_snappy: true,
            color_range: ColorRange::Full,
            alpha_mode: AlphaMode::Straight,
            dither_mode: DitherMode::None,
            quality: QualityPreset::Draft,
        };

        let start_enc = std::time::Instant::now();
        let packet = encode_frame_with_options(&rgba, w, h, &opts).expect("8K encode failed");
        let enc_time = start_enc.elapsed();

        assert!(!packet.is_empty());
        println!("\n>>> 8K UHD (7680x4320) ENCODE: {:.2} ms (packet size: {} bytes, savings: {:.1}%) <<<",
            enc_time.as_secs_f64() * 1000.0,
            packet.len(),
            (1.0 - (packet.len() as f64 / (w * h * 4) as f64)) * 100.0
        );

        let start_dec = std::time::Instant::now();
        let decoded = decode_frame_to_rgba(&packet, w, h).expect("8K decode failed");
        let dec_time = start_dec.elapsed();

        assert_eq!(decoded.len(), rgba.len());
        println!(">>> 8K UHD (7680x4320) DECODE: {:.2} ms ({:.1} FPS) <<<\n",
            dec_time.as_secs_f64() * 1000.0,
            1.0 / dec_time.as_secs_f64()
        );
    }
}
