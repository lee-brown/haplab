//! High-level frame encoding pipeline for all HAP flavours.

use crate::bc7;
use crate::color::{preprocess_rgba, AlphaMode, ColorRange, DitherMode, EncodeOptions, QualityPreset};
use crate::dxt;
use crate::format::HapFormat;
use crate::header::{DecodeInstructions, SectionHeader};
use crate::snappy;
use crate::ycocg;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("DXT encoding error: {0}")]
    Dxt(#[from] dxt::DxtError),
    #[error("BC7 encoding error: {0}")]
    Bc7(#[from] bc7::Bc7Error),
    #[error("Snappy error: {0}")]
    Snappy(#[from] snappy::SnappyError),
    #[error("Invalid dimensions: {width}x{height} (must be multiples of 4)")]
    InvalidDimensions { width: usize, height: usize },
    #[error("Buffer size mismatch: expected {expected}, got {actual}")]
    BufferSizeMismatch { expected: usize, actual: usize },
    #[error("Encoding for format {0:?} not supported")]
    UnsupportedFormat(HapFormat),
}

/// Encode a raw RGBA8 image buffer into a complete HAP frame packet using comprehensive options.
pub fn encode_frame_with_options(
    rgba: &[u8],
    width: usize,
    height: usize,
    options: &EncodeOptions,
) -> Result<Vec<u8>, EncodeError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(EncodeError::InvalidDimensions { width, height });
    }
    if rgba.len() != width * height * 4 {
        return Err(EncodeError::BufferSizeMismatch {
            expected: width * height * 4,
            actual: rgba.len(),
        });
    }

    // 1. Color and Alpha preprocessing (zero-copy Cow if default Full range & Straight alpha)
    let processed_rgba = preprocess_rgba(rgba, width, height, options.color_range, options.alpha_mode);
    let src = processed_rgba.as_ref();

    let format = options.format;
    let chunk_count = options.chunk_count;
    let use_snappy = options.use_snappy;

    match format {
        HapFormat::Hap1 => {
            let params = match options.quality {
                QualityPreset::Draft => texpresso::Params {
                    algorithm: texpresso::Algorithm::RangeFit,
                    weights: [0.2126, 0.7152, 0.0722],
                    weigh_colour_by_alpha: false,
                },
                QualityPreset::Production => texpresso::Params {
                    algorithm: texpresso::Algorithm::ClusterFit,
                    weights: [0.2126, 0.7152, 0.0722],
                    weigh_colour_by_alpha: false,
                },
            };
            let bc1_data = dxt::compress_bc1_with_params(src, width, height, params)?;
            pack_single_texture_frame(&bc1_data, format, chunk_count, use_snappy)
        }
        HapFormat::Hap5 => {
            let params = match options.quality {
                QualityPreset::Draft => texpresso::Params {
                    algorithm: texpresso::Algorithm::RangeFit,
                    weights: [0.2126, 0.7152, 0.0722],
                    weigh_colour_by_alpha: false,
                },
                QualityPreset::Production => texpresso::Params {
                    algorithm: texpresso::Algorithm::ClusterFit,
                    weights: [0.2126, 0.7152, 0.0722],
                    weigh_colour_by_alpha: false,
                },
            };
            let bc3_data = dxt::compress_bc3_with_params(src, width, height, params)?;
            pack_single_texture_frame(&bc3_data, format, chunk_count, use_snappy)
        }
        HapFormat::HapY => {
            let enable_dither = options.dither_mode == DitherMode::Bayer4x4;
            let mut ycocg_buf = vec![0u8; width * height * 4];
            ycocg::rgba_to_scaled_ycocg_dxt5_input_with_dither(src, width, height, &mut ycocg_buf, enable_dither);

            let params = match options.quality {
                QualityPreset::Draft => texpresso::Params {
                    algorithm: texpresso::Algorithm::RangeFit,
                    weights: [1.0, 1.0, 1.0], // Uniform weighting for Co, Cg, Scale indicator!
                    weigh_colour_by_alpha: false,
                },
                QualityPreset::Production => texpresso::Params {
                    algorithm: texpresso::Algorithm::ClusterFit,
                    weights: [1.0, 1.0, 1.0], // Uniform weighting for Co, Cg, Scale indicator!
                    weigh_colour_by_alpha: false,
                },
            };
            let bc3_data = dxt::compress_bc3_with_params(&ycocg_buf, width, height, params)?;
            pack_single_texture_frame(&bc3_data, format, chunk_count, use_snappy)
        }
        HapFormat::HapA => {
            let mut alpha_buf = vec![0u8; width * height];
            for i in 0..(width * height) {
                alpha_buf[i] = src[i * 4 + 3];
            }
            let bc4_data = dxt::compress_bc4(&alpha_buf, width, height)?;
            pack_single_texture_frame(&bc4_data, format, chunk_count, use_snappy)
        }
        HapFormat::Hap7 => {
            let refine_iters = match options.quality {
                QualityPreset::Draft => 1,
                QualityPreset::Production => 2,
            };
            let bc7_data = bc7::compress_bc7_with_refinement(src, width, height, refine_iters)?;
            pack_single_texture_frame(&bc7_data, format, chunk_count, use_snappy)
        }
        HapFormat::HapM => {
            // Hap Q Alpha: Multi-image container (0x0D) with two sections:
            // 1. Color section (HapY / scaled YCoCg DXT5)
            // 2. Alpha section (HapA / BC4)
            let enable_dither = options.dither_mode == DitherMode::Bayer4x4;
            let mut ycocg_buf = vec![0u8; width * height * 4];
            ycocg::rgba_to_scaled_ycocg_dxt5_input_with_dither(src, width, height, &mut ycocg_buf, enable_dither);

            let params = match options.quality {
                QualityPreset::Draft => texpresso::Params {
                    algorithm: texpresso::Algorithm::RangeFit,
                    weights: [1.0, 1.0, 1.0],
                    weigh_colour_by_alpha: false,
                },
                QualityPreset::Production => texpresso::Params {
                    algorithm: texpresso::Algorithm::ClusterFit,
                    weights: [1.0, 1.0, 1.0],
                    weigh_colour_by_alpha: false,
                },
            };
            let bc3_data = dxt::compress_bc3_with_params(&ycocg_buf, width, height, params)?;
            let color_frame = pack_single_texture_frame(&bc3_data, HapFormat::HapY, chunk_count, use_snappy)?;

            let mut alpha_buf = vec![0u8; width * height];
            for i in 0..(width * height) {
                alpha_buf[i] = src[i * 4 + 3];
            }
            let bc4_data = dxt::compress_bc4(&alpha_buf, width, height)?;
            let alpha_frame = pack_single_texture_frame(&bc4_data, HapFormat::HapA, chunk_count, use_snappy)?;

            let total_inner_size = color_frame.len() + alpha_frame.len();
            let mut out = Vec::with_capacity(total_inner_size + 8);
            SectionHeader::write_header(0x0D, total_inner_size, &mut out);
            out.extend_from_slice(&color_frame);
            out.extend_from_slice(&alpha_frame);
            Ok(out)
        }
        HapFormat::HapH => {
            Err(EncodeError::UnsupportedFormat(HapFormat::HapH))
        }
    }
}

/// Encode a raw RGBA8 image buffer into a complete HAP frame packet using default options.
pub fn encode_frame(
    rgba: &[u8],
    width: usize,
    height: usize,
    format: HapFormat,
    chunk_count: usize,
    use_snappy: bool,
) -> Result<Vec<u8>, EncodeError> {
    let options = EncodeOptions {
        format,
        chunk_count,
        use_snappy,
        color_range: ColorRange::Full,
        alpha_mode: AlphaMode::Straight,
        dither_mode: DitherMode::None,
        quality: QualityPreset::Production,
    };
    encode_frame_with_options(rgba, width, height, &options)
}

/// Pack compressed texture blocks into a standard HAP frame (with optional chunking and Snappy).
fn pack_single_texture_frame(
    texture_bytes: &[u8],
    format: HapFormat,
    chunk_count: usize,
    use_snappy: bool,
) -> Result<Vec<u8>, EncodeError> {
    if chunk_count > 1 {
        // Multi-chunk layout
        let (chunks, chunk_infos) = snappy::compress_chunks_parallel(texture_bytes, chunk_count, use_snappy)?;
        let instructions_payload = DecodeInstructions::build(&chunk_infos);

        // Instructions section: type 0x01
        let mut instr_sec = Vec::new();
        SectionHeader::write_header(0x01, instructions_payload.len(), &mut instr_sec);
        instr_sec.extend_from_slice(&instructions_payload);

        let chunks_size: usize = chunks.iter().map(|c| c.len()).sum();
        let total_payload_size = instr_sec.len() + chunks_size;

        let mut out = Vec::with_capacity(total_payload_size + 8);
        SectionHeader::write_header(format.chunked_type_byte(), total_payload_size, &mut out);
        out.extend_from_slice(&instr_sec);
        for c in chunks {
            out.extend_from_slice(&c);
        }

        Ok(out)
    } else if use_snappy {
        // Single Snappy compressed frame
        let compressed = snappy::compress_snappy(texture_bytes)?;
        let mut out = Vec::with_capacity(compressed.len() + 8);
        SectionHeader::write_header(format.snappy_type_byte(), compressed.len(), &mut out);
        out.extend_from_slice(&compressed);
        Ok(out)
    } else {
        // Uncompressed raw frame
        let mut out = Vec::with_capacity(texture_bytes.len() + 8);
        SectionHeader::write_header(format.uncompressed_type_byte(), texture_bytes.len(), &mut out);
        out.extend_from_slice(texture_bytes);
        Ok(out)
    }
}
