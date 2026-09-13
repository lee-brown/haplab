//! High-level frame decoding pipeline for all HAP flavours.

use crate::bc6h;
use crate::bc7;
use crate::dxt;
use crate::format::HapFormat;
use crate::header::{DecodeInstructions, HeaderError, SectionHeader};
use crate::snappy;
use crate::ycocg;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("Header error: {0}")]
    Header(#[from] HeaderError),
    #[error("Snappy error: {0}")]
    Snappy(#[from] snappy::SnappyError),
    #[error("DXT decompression error: {0}")]
    Dxt(#[from] dxt::DxtError),
    #[error("BC7 decompression error: {0}")]
    Bc7(#[from] bc7::Bc7Error),
    #[error("BC6H decompression error: {0}")]
    Bc6h(#[from] bc6h::Bc6hError),
    #[error("Invalid dimensions: {width}x{height} (must be multiples of 4)")]
    InvalidDimensions { width: usize, height: usize },
    #[error("Corrupt or invalid frame packet: {0}")]
    CorruptFrame(String),
}

/// Raw decompressed texture ready for direct GPU upload.
#[derive(Debug, Clone)]
pub struct RawTextureFrame {
    pub format: HapFormat,
    pub texture_bytes: Vec<u8>,
    pub alpha_bytes: Option<Vec<u8>>,
}

/// Decode a raw HAP packet into raw block-compressed texture data (for direct GPU upload).
pub fn decode_frame_to_texture(packet: &[u8]) -> Result<RawTextureFrame, DecodeError> {
    let hdr = SectionHeader::parse(packet)?;
    let payload = &packet[hdr.header_size..hdr.header_size + hdr.data_size];

    if hdr.section_type == 0x0D {
        // Multi-image container (Hap Q Alpha)
        let color_hdr = SectionHeader::parse(payload)?;
        let color_end = color_hdr.header_size + color_hdr.data_size;
        let color_packet = &payload[..color_end];
        let color_texture = unpack_section_payload(color_hdr.section_type, &color_packet[color_hdr.header_size..color_end])?;

        let alpha_packet = &payload[color_end..];
        let alpha_hdr = SectionHeader::parse(alpha_packet)?;
        let alpha_end = alpha_hdr.header_size + alpha_hdr.data_size;
        let alpha_texture = unpack_section_payload(alpha_hdr.section_type, &alpha_packet[alpha_hdr.header_size..alpha_end])?;

        Ok(RawTextureFrame {
            format: HapFormat::HapM,
            texture_bytes: color_texture,
            alpha_bytes: Some(alpha_texture),
        })
    } else {
        let texture_bytes = unpack_section_payload(hdr.section_type, payload)?;
        let format = match hdr.section_type & 0x0F {
            0x0B => HapFormat::Hap1,
            0x0E => HapFormat::Hap5,
            0x0F => HapFormat::HapY,
            0x01 => HapFormat::HapA,
            0x0C => HapFormat::Hap7,
            0x02 | 0x03 => HapFormat::HapH,
            other => return Err(DecodeError::CorruptFrame(format!("Unknown low nibble format 0x{:02X}", other))),
        };

        Ok(RawTextureFrame {
            format,
            texture_bytes,
            alpha_bytes: None,
        })
    }
}

/// Decode a raw HAP packet into a fully decompressed RGBA8 buffer.
pub fn decode_frame_to_rgba(
    packet: &[u8],
    width: usize,
    height: usize,
) -> Result<Vec<u8>, DecodeError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(DecodeError::InvalidDimensions { width, height });
    }

    let hdr = SectionHeader::parse(packet)?;
    let payload = &packet[hdr.header_size..hdr.header_size + hdr.data_size];

    if hdr.section_type == 0x0D {
        // Multi-image container: Hap Q Alpha
        // Sub-section 1: Color stream (HapY)
        let color_hdr = SectionHeader::parse(payload)?;
        let color_end = color_hdr.header_size + color_hdr.data_size;
        let color_data = &payload[color_hdr.header_size..color_end];
        let color_bc3 = unpack_section_payload(color_hdr.section_type, color_data)?;

        // Sub-section 2: Alpha stream (HapA / BC4)
        let alpha_payload = &payload[color_end..];
        let alpha_hdr = SectionHeader::parse(alpha_payload)?;
        let alpha_end = alpha_hdr.header_size + alpha_hdr.data_size;
        let alpha_data = &alpha_payload[alpha_hdr.header_size..alpha_end];
        let alpha_bc4 = unpack_section_payload(alpha_hdr.section_type, alpha_data)?;

        // Decompress color BC3 and inverse YCoCg directly in single pass
        let mut rgba = vec![0u8; width * height * 4];
        ycocg::decode_scaled_ycocg_bc3_direct(&color_bc3, width, height, &mut rgba)?;

        // Decompress alpha BC4 and write into alpha channel
        let alpha_decompressed = dxt::decompress_bc4(&alpha_bc4, width, height)?;
        for i in 0..(width * height) {
            rgba[i * 4 + 3] = alpha_decompressed[i];
        }

        Ok(rgba)
    } else {
        let texture_bytes = unpack_section_payload(hdr.section_type, payload)?;
        let format_nibble = hdr.section_type & 0x0F;

        match format_nibble {
            0x0B => {
                // Hap 1: DXT1 / BC1
                let rgba = dxt::decompress_bc1(&texture_bytes, width, height)?;
                Ok(rgba)
            }
            0x0E => {
                // Hap 5: DXT5 / BC3
                let rgba = dxt::decompress_bc3(&texture_bytes, width, height)?;
                Ok(rgba)
            }
            0x0F => {
                // Hap Q: Scaled YCoCg in DXT5 / BC3 (fused single-pass decode)
                let mut rgba = vec![0u8; width * height * 4];
                ycocg::decode_scaled_ycocg_bc3_direct(&texture_bytes, width, height, &mut rgba)?;
                Ok(rgba)
            }
            0x01 => {
                // Hap Alpha-Only: BC4
                let alpha_buf = dxt::decompress_bc4(&texture_bytes, width, height)?;
                let mut rgba = vec![0u8; width * height * 4];
                for i in 0..(width * height) {
                    let a = alpha_buf[i];
                    rgba[i * 4] = a;
                    rgba[i * 4 + 1] = a;
                    rgba[i * 4 + 2] = a;
                    rgba[i * 4 + 3] = a;
                }
                Ok(rgba)
            }
            0x0C => {
                // Hap R (BC7)
                let rgba = bc7::decompress_bc7(&texture_bytes, width, height)?;
                Ok(rgba)
            }
            0x02 => {
                // Hap HDR (BC6U unsigned float)
                let rgba = bc6h::decompress_bc6h(&texture_bytes, width, height, false)?;
                Ok(rgba)
            }
            0x03 => {
                // Hap HDR (BC6S signed float)
                let rgba = bc6h::decompress_bc6h(&texture_bytes, width, height, true)?;
                Ok(rgba)
            }
            other => Err(DecodeError::CorruptFrame(format!(
                "Unsupported format low-nibble: 0x{:02X}",
                other
            ))),
        }
    }
}

/// Unpack a section payload into uncompressed BCn texture data according to its high-nibble compressor.
fn unpack_section_payload(section_type: u8, payload: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let comp_nibble = section_type >> 4;
    match comp_nibble {
        0x0A => {
            // Uncompressed raw texture data
            Ok(payload.to_vec())
        }
        0x0B => {
            // Single Snappy compressed buffer
            let decompressed = snappy::decompress_snappy(payload)?;
            Ok(decompressed)
        }
        0x0C => {
            // Decode instructions container (Chunked)
            let instr_hdr = SectionHeader::parse(payload)?;
            if instr_hdr.section_type != 0x01 {
                return Err(DecodeError::CorruptFrame(format!(
                    "Expected 0x01 decode instructions container, got 0x{:02X}",
                    instr_hdr.section_type
                )));
            }

            let instr_end = instr_hdr.header_size + instr_hdr.data_size;
            let instr_payload = &payload[instr_hdr.header_size..instr_end];
            let instructions = DecodeInstructions::parse(instr_payload)?;

            let chunks_payload = &payload[instr_end..];
            let decompressed = snappy::decompress_chunks_parallel(chunks_payload, &instructions.chunks)?;
            Ok(decompressed)
        }
        other => Err(DecodeError::CorruptFrame(format!(
            "Unsupported compressor high-nibble: 0x{:02X}",
            other
        ))),
    }
}
