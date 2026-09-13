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
/// - Channel B stores the scale indicator `(scale - 1) * 8`
/// - Channel A stores Luma Y (which DXT5 encodes in its high-precision 8-bit alpha block)
pub fn rgba_to_scaled_ycocg_dxt5_input(
    rgba: &[u8],
    width: usize,
    height: usize,
    out: &mut [u8],
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
                // Step 1: Collect pixels of the 4x4 block and compute max chroma deviation
                let mut ycocg_pixels = [(0u8, 0u8, 0u8); 16];
                let mut max_dev = 0i32;

                for py in 0..4 {
                    for px in 0..4 {
                        let x = bx * 4 + px;
                        let src_idx = (py * width + x) * 4;
                        let r = row_rgba[src_idx];
                        let g = row_rgba[src_idx + 1];
                        let b = row_rgba[src_idx + 2];

                        let (lum, co, cg) = rgb_to_ycocg(r, g, b);
                        let pi = py * 4 + px;
                        ycocg_pixels[pi] = (lum, co, cg);

                        let dev_co = (co as i32 - 128).abs();
                        let dev_cg = (cg as i32 - 128).abs();
                        max_dev = max_dev.max(dev_co).max(dev_cg);
                    }
                }

                // Step 2: Determine per-block scale (1, 2, or 4)
                let scale = if max_dev <= 31 {
                    4
                } else if max_dev <= 63 {
                    2
                } else {
                    1
                };

                let blue_scale_indicator = ((scale - 1) * 8) as u8;

                // Step 3: Write scaled values to output
                for py in 0..4 {
                    for px in 0..4 {
                        let x = bx * 4 + px;
                        let dst_idx = (py * width + x) * 4;
                        let pi = py * 4 + px;
                        let (lum, co, cg) = ycocg_pixels[pi];

                        let co_scaled = ((co as i32 - 128) * scale + 128).clamp(0, 255) as u8;
                        let cg_scaled = ((cg as i32 - 128) * scale + 128).clamp(0, 255) as u8;

                        out_row[dst_idx] = co_scaled;               // R -> Co
                        out_row[dst_idx + 1] = cg_scaled;           // G -> Cg
                        out_row[dst_idx + 2] = blue_scale_indicator;// B -> Scale
                        out_row[dst_idx + 3] = lum;                 // A -> Y
                    }
                }
            }
        });
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
