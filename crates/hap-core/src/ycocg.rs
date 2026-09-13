//! Scaled YCoCg color space transformations for Hap Q and Hap Q Alpha.
//!
//! Converts RGB <-> Scaled YCoCg-DXT5 per the Real-Time YCoCg-DXT Compression
//! specification (J.M.P. van Waveren & Ignacio Castaño, NVIDIA / id Software).

/// Convert an RGB pixel to unscaled YCoCg (Co and Cg centered at 128).
#[inline]
pub fn rgb_to_ycocg(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let ri = r as i32;
    let gi = g as i32;
    let bi = b as i32;

    let y = (ri + 2 * gi + bi) / 4;
    let co = (ri - bi) / 2 + 128;
    let cg = (-ri + 2 * gi - bi) / 4 + 128;

    (
        y.clamp(0, 255) as u8,
        co.clamp(0, 255) as u8,
        cg.clamp(0, 255) as u8,
    )
}

/// Convert a YCoCg pixel back to RGB.
#[inline]
pub fn ycocg_to_rgb(y: u8, co: u8, cg: u8) -> (u8, u8, u8) {
    let yi = y as i32;
    let coi = co as i32 - 128;
    let cgi = cg as i32 - 128;

    let r = yi + coi - cgi;
    let g = yi + cgi;
    let b = yi - coi - cgi;

    (
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    )
}

use rayon::prelude::*;

/// Transform an entire RGBA8 image buffer (dimensions multiple of 4) into the
/// scaled YCoCg buffer ready for DXT5 compression.
///
/// In the output buffer:
/// - Channel R stores scaled Co
/// - Channel G stores scaled Cg
const BAYER_4X4: [[i32; 4]; 4] = [
    [ 0,  8,  2, 10],
    [12,  4, 14,  6],
    [ 3, 11,  1,  9],
    [15,  7, 13,  5],
];

/// Transform an RGBA8 buffer to the scaled YCoCg layout ready for DXT5 compression,
/// with optional Bayer spatial dithering to prevent color banding on large displays.
pub fn rgba_to_scaled_ycocg_dxt5_input_with_dither(
    rgba: &[u8],
    width: usize,
    height: usize,
    out: &mut [u8],
    enable_dither: bool,
) {
    assert_eq!(rgba.len(), width * height * 4);
    assert_eq!(out.len(), width * height * 4);

    let blocks_x = width / 4;
    let block_row_bytes = width * 4 * 4;

    out.par_chunks_mut(block_row_bytes)
        .enumerate()
        .for_each(|(by, out_row)| {
            let row_rgba = &rgba[by * block_row_bytes..(by + 1) * block_row_bytes];
            for bx in 0..blocks_x {
                let mut y_vals = [0u8; 16];
                let mut co_diffs = [0i32; 16];
                let mut cg_diffs = [0i32; 16];
                let mut max_dev = 0i32;

                for py in 0..4 {
                    let row_offset = (py * width + bx * 4) * 4;
                    let src_slice = &row_rgba[row_offset..row_offset + 16];

                    for px in 0..4 {
                        let i = px * 4;
                        let r = src_slice[i] as i32;
                        let g = src_slice[i + 1] as i32;
                        let b = src_slice[i + 2] as i32;

                        let y = ((r + 2 * g + b) >> 2) as u8;
                        let co_diff = (r - b) >> 1;
                        let cg_diff = (-r + 2 * g - b) >> 2;

                        let pi = py * 4 + px;
                        y_vals[pi] = y;
                        co_diffs[pi] = co_diff;
                        cg_diffs[pi] = cg_diff;

                        max_dev = max_dev.max(co_diff.abs()).max(cg_diff.abs());
                    }
                }

                let (scale_shift, scale_indicator) = if max_dev <= 31 {
                    (2, 24u8) // scale 4: ((4 - 1) * 8) = 24
                } else if max_dev <= 63 {
                    (1, 8u8)  // scale 2: ((2 - 1) * 8) = 8
                } else {
                    (0, 0u8)  // scale 1: ((1 - 1) * 8) = 0
                };

                for py in 0..4 {
                    let dst_offset = (py * width + bx * 4) * 4;
                    let dst_slice = &mut out_row[dst_offset..dst_offset + 16];

                    for px in 0..4 {
                        let pi = py * 4 + px;
                        let dither = if enable_dither {
                            (BAYER_4X4[py][px] * 2 - 15) / 8
                        } else {
                            0
                        };

                        let co_scaled = ((co_diffs[pi] << scale_shift) + 128 + dither).clamp(0, 255) as u8;
                        let cg_scaled = ((cg_diffs[pi] << scale_shift) + 128 + dither).clamp(0, 255) as u8;

                        let di = px * 4;
                        dst_slice[di] = co_scaled;
                        dst_slice[di + 1] = cg_scaled;
                        dst_slice[di + 2] = scale_indicator;
                        dst_slice[di + 3] = y_vals[pi];
                    }
                }
            }
        });
}

/// Transform an RGBA8 buffer to the scaled YCoCg layout ready for DXT5 compression.
/// - Channel R stores scaled Co
/// - Channel G stores scaled Cg
/// - Channel B stores the scale indicator `(scale - 1) * 8`
/// - Channel A stores Luma Y (which DXT5 encodes in its high-precision 8-bit alpha block)
pub fn rgba_to_scaled_ycocg_dxt5_input(
    rgba: &[u8],
    width: usize,
    height: usize,
    out: &mut [u8],
) {
    rgba_to_scaled_ycocg_dxt5_input_with_dither(rgba, width, height, out, false);
}

/// Transform a decompressed DXT5 buffer (containing scaled YCoCg in RGBA) back to standard RGBA8.
pub fn scaled_ycocg_dxt5_output_to_rgba(
    ycocg_dxt5: &[u8],
    width: usize,
    height: usize,
    out: &mut [u8],
) {
    assert_eq!(ycocg_dxt5.len(), width * height * 4);
    assert_eq!(out.len(), width * height * 4);

    const CHUNK_SIZE: usize = 4096;
    out.par_chunks_mut(CHUNK_SIZE)
        .zip(ycocg_dxt5.par_chunks(CHUNK_SIZE))
        .for_each(|(out_chunk, in_chunk)| {
            for (dst, src) in out_chunk.chunks_exact_mut(4).zip(in_chunk.chunks_exact(4)) {
                let co_scaled = src[0] as f32;
                let cg_scaled = src[1] as f32;
                let blue = src[2];
                let lum = src[3] as f32;

                let scale = ((blue / 8) + 1).max(1) as f32;

                let co = (co_scaled - 128.0) / scale;
                let cg = (cg_scaled - 128.0) / scale;

                let r = (lum + co - cg).clamp(0.0, 255.0) as u8;
                let g = (lum + cg).clamp(0.0, 255.0) as u8;
                let b = (lum - co - cg).clamp(0.0, 255.0) as u8;

                dst[0] = r;
                dst[1] = g;
                dst[2] = b;
                dst[3] = 255;
            }
        });
}

const INV_SCALE_LUT: [f32; 256] = {
    let mut lut = [0.0f32; 256];
    let mut i = 0;
    while i < 256 {
        let s = (i / 8) + 1;
        lut[i] = 1.0 / (s as f32);
        i += 1;
    }
    lut
};

/// Fused, single-pass Hap Q (Scaled YCoCg DXT5) decoder.
/// Decodes 16-byte BC3 blocks directly into final RGBA8 scanlines across all CPU cores,
/// completely bypassing intermediate buffer allocations.
pub fn decode_scaled_ycocg_bc3_direct(
    bc3_data: &[u8],
    width: usize,
    height: usize,
    out: &mut [u8],
) -> Result<(), crate::dxt::DxtError> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(crate::dxt::DxtError::InvalidDimensions { width, height });
    }
    let blocks_x = width / 4;
    let blocks_y = height / 4;
    let expected_len = blocks_x * blocks_y * 16;
    if bc3_data.len() < expected_len {
        return Err(crate::dxt::DxtError::BufferSizeMismatch {
            expected: expected_len,
            actual: bc3_data.len(),
        });
    }

    let dst_row_bytes = width * 4 * 4;

    out.par_chunks_mut(dst_row_bytes)
        .enumerate()
        .for_each(|(by, dst_block_row)| {
            let src_block_row_offset = by * blocks_x * 16;
            let mut block_pixels = [0u8; 64];

            for bx in 0..blocks_x {
                let block_start = src_block_row_offset + bx * 16;
                let block = &bc3_data[block_start..block_start + 16];

                // Decompress 4x4 block: R=Co, G=Cg, B=Scale, A=Y with pitch = 16 bytes
                bcdec_rs::bc3(block, &mut block_pixels, 16);

                // Reconstruct RGBA into final scanlines
                for py in 0..4 {
                    let dst_y_offset = py * width * 4 + bx * 16;
                    let dst_slice = &mut dst_block_row[dst_y_offset..dst_y_offset + 16];
                    let py4 = py * 4;

                    for px in 0..4 {
                        let src_i = (py4 + px) * 4;
                        let co_scaled = block_pixels[src_i] as f32;
                        let cg_scaled = block_pixels[src_i + 1] as f32;
                        let blue = block_pixels[src_i + 2];
                        let lum = block_pixels[src_i + 3] as f32;

                        let inv_scale = INV_SCALE_LUT[blue as usize];
                        let co = (co_scaled - 128.0) * inv_scale;
                        let cg = (cg_scaled - 128.0) * inv_scale;

                        let r = (lum + co - cg).clamp(0.0, 255.0) as u8;
                        let g = (lum + cg).clamp(0.0, 255.0) as u8;
                        let b = (lum - co - cg).clamp(0.0, 255.0) as u8;

                        let dst_i = px * 4;
                        dst_slice[dst_i] = r;
                        dst_slice[dst_i + 1] = g;
                        dst_slice[dst_i + 2] = b;
                        dst_slice[dst_i + 3] = 255;
                    }
                }
            }
        });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ycocg_lossless_roundtrip_approx() {
        // Test primary colors and gray levels
        let test_colors = [
            (255, 255, 255), // White
            (0, 0, 0),       // Black
            (128, 128, 128), // Gray
            (255, 0, 0),     // Red
            (0, 255, 0),     // Green
            (0, 0, 255),     // Blue
            (255, 255, 0),   // Yellow
            (0, 255, 255),   // Cyan
            (255, 0, 255),   // Magenta
        ];

        for &(r, g, b) in &test_colors {
            let (y, co, cg) = rgb_to_ycocg(r, g, b);
            let (r2, g2, b2) = ycocg_to_rgb(y, co, cg);

            // Due to integer division, roundtrip error is at most 1 unit
            let dr = (r as i32 - r2 as i32).abs();
            let dg = (g as i32 - g2 as i32).abs();
            let db = (b as i32 - b2 as i32).abs();

            assert!(dr <= 2, "R difference too large: {} vs {}", r, r2);
            assert!(dg <= 2, "G difference too large: {} vs {}", g, g2);
            assert!(db <= 2, "B difference too large: {} vs {}", b, b2);
        }
    }

    #[test]
    fn test_scaled_ycocg_4x4_block_roundtrip() {
        // Create 4x4 RGBA image
        let mut rgba = vec![0u8; 16 * 4];
        for i in 0..16 {
            rgba[i * 4] = (i * 15) as u8;
            rgba[i * 4 + 1] = 120;
            rgba[i * 4 + 2] = 200;
            rgba[i * 4 + 3] = 255;
        }

        let mut ycocg_buf = vec![0u8; 16 * 4];
        rgba_to_scaled_ycocg_dxt5_input(&rgba, 4, 4, &mut ycocg_buf);

        let mut restored = vec![0u8; 16 * 4];
        scaled_ycocg_dxt5_output_to_rgba(&ycocg_buf, 4, 4, &mut restored);

        for i in 0..16 {
            let dr = (rgba[i * 4] as i32 - restored[i * 4] as i32).abs();
            let dg = (rgba[i * 4 + 1] as i32 - restored[i * 4 + 1] as i32).abs();
            let db = (rgba[i * 4 + 2] as i32 - restored[i * 4 + 2] as i32).abs();
            assert!(dr <= 2, "Pixel {} dr: {}", i, dr);
            assert!(dg <= 2, "Pixel {} dg: {}", i, dg);
            assert!(db <= 2, "Pixel {} db: {}", i, db);
        }
    }
}
