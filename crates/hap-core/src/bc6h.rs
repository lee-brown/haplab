//! Pure Rust BC6H (BPTC Float) decoder for Hap HDR (HapH).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Bc6hError {
    #[error("Invalid dimensions: {width}x{height} (must be multiples of 4)")]
    InvalidDimensions { width: usize, height: usize },
    #[error("Buffer size mismatch: expected {expected}, got {actual}")]
    BufferSizeMismatch { expected: usize, actual: usize },
    #[error("Decompression failed: {0}")]
    DecompressFailed(String),
}

/// Decompress BC6H blocks to RGBA8 (tone-mapped/clamped preview) pixels.
pub fn decompress_bc6h(
    bc6h: &[u8],
    width: usize,
    height: usize,
    is_signed: bool,
) -> Result<Vec<u8>, Bc6hError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(Bc6hError::InvalidDimensions { width, height });
    }
    let num_blocks = (width / 4) * (height / 4);
    let expected_bc6h_len = num_blocks * 16;
    if bc6h.len() < expected_bc6h_len {
        return Err(Bc6hError::BufferSizeMismatch {
            expected: expected_bc6h_len,
            actual: bc6h.len(),
        });
    }

    let mut u32_buf = vec![0u32; width * height];
    if is_signed {
        texture2ddecoder::decode_bc6_signed(&bc6h[..expected_bc6h_len], width, height, &mut u32_buf)
            .map_err(|e| Bc6hError::DecompressFailed(e.to_string()))?;
    } else {
        texture2ddecoder::decode_bc6_unsigned(&bc6h[..expected_bc6h_len], width, height, &mut u32_buf)
            .map_err(|e| Bc6hError::DecompressFailed(e.to_string()))?;
    }

    let mut rgba_out = vec![0u8; width * height * 4];
    for (i, &p) in u32_buf.iter().enumerate() {
        let bytes = p.to_le_bytes();
        rgba_out[i * 4] = bytes[2];
        rgba_out[i * 4 + 1] = bytes[1];
        rgba_out[i * 4 + 2] = bytes[0];
        rgba_out[i * 4 + 3] = 255;
    }

    Ok(rgba_out)
}
