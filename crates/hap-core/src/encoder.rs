//! High-level frame encoding pipeline for all HAP flavours.

use crate::bc7;
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

/// Encode a raw RGBA8 image buffer into a complete HAP frame packet.
pub fn encode_frame(
    rgba: &[u8],
    width: usize,
    height: usize,
    format: HapFormat,
    chunk_count: usize,
    use_snappy: bool,
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

    match format {
        HapFormat::Hap1 => {
            let bc1_data = dxt::compress_bc1(rgba, width, height)?;
            pack_single_texture_frame(&bc1_data, format, chunk_count, use_snappy)
        }
        HapFormat::Hap5 => {
            let bc3_data = dxt::compress_bc3(rgba, width, height)?;
            pack_single_texture_frame(&bc3_data, format, chunk_count, use_snappy)
        }
        HapFormat::HapY => {
            let mut ycocg_buf = vec![0u8; width * height * 4];
            ycocg::rgba_to_scaled_ycocg_dxt5_input(rgba, width, height, &mut ycocg_buf);
            let bc3_data = dxt::compress_bc3(&ycocg_buf, width, height)?;
            pack_single_texture_frame(&bc3_data, format, chunk_count, use_snappy)
        }
        HapFormat::HapA => {
            let mut alpha_buf = vec![0u8; width * height];
            for i in 0..(width * height) {
                alpha_buf[i] = rgba[i * 4 + 3];
            }
            let bc4_data = dxt::compress_bc4(&alpha_buf, width, height)?;
            pack_single_texture_frame(&bc4_data, format, chunk_count, use_snappy)
        }
        HapFormat::Hap7 => {
            let bc7_data = bc7::compress_bc7(rgba, width, height)?;
            pack_single_texture_frame(&bc7_data, format, chunk_count, use_snappy)
        }
        HapFormat::HapM => {
            // Hap Q Alpha: Multi-image container (0x0D) with two sections:
            // 1. Color section (HapY / scaled YCoCg DXT5)
            // 2. Alpha section (HapA / BC4)
            let mut ycocg_buf = vec![0u8; width * height * 4];
            ycocg::rgba_to_scaled_ycocg_dxt5_input(rgba, width, height, &mut ycocg_buf);
            let bc3_data = dxt::compress_bc3(&ycocg_buf, width, height)?;
            let color_frame = pack_single_texture_frame(&bc3_data, HapFormat::HapY, chunk_count, use_snappy)?;

            let mut alpha_buf = vec![0u8; width * height];
            for i in 0..(width * height) {
                alpha_buf[i] = rgba[i * 4 + 3];
            }
            let bc4_data = dxt::compress_bc4(&alpha_buf, width, height)?;
            let alpha_frame = pack_single_texture_frame(&bc4_data, HapFormat::HapA, chunk_count, use_snappy)?;

            let total_inner_size = color_frame.len() + alpha_frame.len();
            let mut out = Vec::with_capacity(total_inner_size + 8);
            // Multi-image container type 0x0D
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
