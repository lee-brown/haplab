//! CPU-based texture compression and decompression using texpresso.
//!
//! Provides DXT1 (BC1), DXT5 (BC3), and BC4 (RGTC1) encoding and decoding.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DxtError {
    #[error("Invalid dimensions: {width}x{height} (must be multiples of 4)")]
    InvalidDimensions { width: usize, height: usize },
    #[error("Input buffer size mismatch: expected {expected}, got {actual}")]
    BufferSizeMismatch { expected: usize, actual: usize },
}

/// Compress an RGBA8 buffer to DXT1 / BC1.
pub fn compress_bc1(rgba: &[u8], width: usize, height: usize) -> Result<Vec<u8>, DxtError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(DxtError::InvalidDimensions { width, height });
    }
    if rgba.len() != width * height * 4 {
        return Err(DxtError::BufferSizeMismatch {
            expected: width * height * 4,
            actual: rgba.len(),
        });
    }

    let fmt = texpresso::Format::Bc1;
    let compressed_len = fmt.compressed_size(width, height);
    let mut out = vec![0u8; compressed_len];
    fmt.compress(rgba, width, height, texpresso::Params::default(), &mut out);
    Ok(out)
}

/// Decompress DXT1 / BC1 to RGBA8.
pub fn decompress_bc1(bc1: &[u8], width: usize, height: usize) -> Result<Vec<u8>, DxtError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(DxtError::InvalidDimensions { width, height });
    }
    let fmt = texpresso::Format::Bc1;
    let expected_len = fmt.compressed_size(width, height);
    if bc1.len() < expected_len {
        return Err(DxtError::BufferSizeMismatch {
            expected: expected_len,
            actual: bc1.len(),
        });
    }

    let mut out = vec![0u8; width * height * 4];
    fmt.decompress(&bc1[..expected_len], width, height, &mut out);
    Ok(out)
}

/// Compress an RGBA8 buffer to DXT5 / BC3.
pub fn compress_bc3(rgba: &[u8], width: usize, height: usize) -> Result<Vec<u8>, DxtError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(DxtError::InvalidDimensions { width, height });
    }
    if rgba.len() != width * height * 4 {
        return Err(DxtError::BufferSizeMismatch {
            expected: width * height * 4,
            actual: rgba.len(),
        });
    }

    let fmt = texpresso::Format::Bc3;
    let compressed_len = fmt.compressed_size(width, height);
    let mut out = vec![0u8; compressed_len];
    fmt.compress(rgba, width, height, texpresso::Params::default(), &mut out);
    Ok(out)
}

/// Decompress DXT5 / BC3 to RGBA8.
pub fn decompress_bc3(bc3: &[u8], width: usize, height: usize) -> Result<Vec<u8>, DxtError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(DxtError::InvalidDimensions { width, height });
    }
    let fmt = texpresso::Format::Bc3;
    let expected_len = fmt.compressed_size(width, height);
    if bc3.len() < expected_len {
        return Err(DxtError::BufferSizeMismatch {
            expected: expected_len,
            actual: bc3.len(),
        });
    }

    let mut out = vec![0u8; width * height * 4];
    fmt.decompress(&bc3[..expected_len], width, height, &mut out);
    Ok(out)
}

/// Compress an 8-bit alpha channel buffer to BC4 / RGTC1.
pub fn compress_bc4(alpha: &[u8], width: usize, height: usize) -> Result<Vec<u8>, DxtError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(DxtError::InvalidDimensions { width, height });
    }
    if alpha.len() != width * height {
        return Err(DxtError::BufferSizeMismatch {
            expected: width * height,
            actual: alpha.len(),
        });
    }

    let fmt = texpresso::Format::Bc4;
    let compressed_len = fmt.compressed_size(width, height);
    let mut out = vec![0u8; compressed_len];

    // texpresso requires 4-channel (RGBA) input for all formats
    let mut rgba_input = vec![0u8; width * height * 4];
    for i in 0..(width * height) {
        let a = alpha[i];
        rgba_input[i * 4] = a;
        rgba_input[i * 4 + 1] = a;
        rgba_input[i * 4 + 2] = a;
        rgba_input[i * 4 + 3] = a;
    }

    fmt.compress(&rgba_input, width, height, texpresso::Params::default(), &mut out);
    Ok(out)
}

/// Decompress BC4 / RGTC1 to 8-bit alpha (grayscale).
pub fn decompress_bc4(bc4: &[u8], width: usize, height: usize) -> Result<Vec<u8>, DxtError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(DxtError::InvalidDimensions { width, height });
    }
    let fmt = texpresso::Format::Bc4;
    let expected_len = fmt.compressed_size(width, height);
    if bc4.len() < expected_len {
        return Err(DxtError::BufferSizeMismatch {
            expected: expected_len,
            actual: bc4.len(),
        });
    }

    let mut rgba_buf = vec![0u8; width * height * 4];
    fmt.decompress(&bc4[..expected_len], width, height, &mut rgba_buf);
    let mut out = vec![0u8; width * height];
    for i in 0..(width * height) {
        out[i] = rgba_buf[i * 4];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bc1_roundtrip() {
        let width = 8;
        let height = 8;
        let mut rgba = vec![0u8; width * height * 4];
        for i in 0..(width * height) {
            rgba[i * 4] = (i * 4) as u8;
            rgba[i * 4 + 1] = 200;
            rgba[i * 4 + 2] = 50;
            rgba[i * 4 + 3] = 255;
        }

        let compressed = compress_bc1(&rgba, width, height).unwrap();
        assert_eq!(compressed.len(), (width * height / 16) * 8);

        let decompressed = decompress_bc1(&compressed, width, height).unwrap();
        assert_eq!(decompressed.len(), rgba.len());
    }

    #[test]
    fn test_bc3_roundtrip() {
        let width = 8;
        let height = 8;
        let mut rgba = vec![0u8; width * height * 4];
        for i in 0..(width * height) {
            rgba[i * 4] = 100;
            rgba[i * 4 + 1] = 150;
            rgba[i * 4 + 2] = 200;
            rgba[i * 4 + 3] = (i * 3) as u8; // Alpha gradient
        }

        let compressed = compress_bc3(&rgba, width, height).unwrap();
        assert_eq!(compressed.len(), (width * height / 16) * 16);

        let decompressed = decompress_bc3(&compressed, width, height).unwrap();
        assert_eq!(decompressed.len(), rgba.len());
    }
}
