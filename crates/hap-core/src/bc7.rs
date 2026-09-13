//! Pure Rust BC7 (BPTC UNORM) encoder and decoder for Hap R (Hap 7).
//!
//! Provides CPU-based encoding using PCA Mode 6 and decoding via texture2ddecoder.

use rayon::prelude::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Bc7Error {
    #[error("Invalid dimensions: {width}x{height} (must be multiples of 4)")]
    InvalidDimensions { width: usize, height: usize },
    #[error("Buffer size mismatch: expected {expected}, got {actual}")]
    BufferSizeMismatch { expected: usize, actual: usize },
    #[error("Decompression failed: {0}")]
    DecompressFailed(String),
}

/// 4-bit interpolation weights for BC7 mode 6 (out of 64).
const WEIGHTS4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// Compress an RGBA8 image buffer (dimensions multiple of 4) to BC7 Mode 6.
/// Output is 16 bytes per 4x4 block.
pub fn compress_bc7(rgba: &[u8], width: usize, height: usize) -> Result<Vec<u8>, Bc7Error> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(Bc7Error::InvalidDimensions { width, height });
    }
    let expected_rgba_len = width * height * 4;
    if rgba.len() != expected_rgba_len {
        return Err(Bc7Error::BufferSizeMismatch {
            expected: expected_rgba_len,
            actual: rgba.len(),
        });
    }

    use rayon::prelude::*;

    let blocks_x = width / 4;
    let blocks_y = height / 4;
    let num_blocks = blocks_x * blocks_y;
    let mut out = vec![0u8; num_blocks * 16];
    let block_row_bytes = blocks_x * 16;

    out.par_chunks_mut(block_row_bytes)
        .enumerate()
        .for_each(|(by, out_row)| {
            for bx in 0..blocks_x {
                let mut px = [[0u8; 4]; 16];
                for i in 0..16 {
                    let x = bx * 4 + (i % 4);
                    let y = by * 4 + (i / 4);
                    let idx = (y * width + x) * 4;
                    px[i].copy_from_slice(&rgba[idx..idx + 4]);
                }

                let block = encode_bc7_block_mode6(&px, 2);
                let block_offset = bx * 16;
                out_row[block_offset..block_offset + 16].copy_from_slice(&block);
            }
        });

    Ok(out)
}

/// Decompress BC7 blocks into RGBA8 pixels in parallel across block rows using bcdec_rs.
pub fn decompress_bc7(bc7: &[u8], width: usize, height: usize) -> Result<Vec<u8>, Bc7Error> {
    if width % 4 != 0 || height % 4 != 0 {
        return Err(Bc7Error::InvalidDimensions { width, height });
    }
    let blocks_x = width / 4;
    let blocks_y = height / 4;
    let expected_bc7_len = blocks_x * blocks_y * 16;
    if bc7.len() < expected_bc7_len {
        return Err(Bc7Error::BufferSizeMismatch {
            expected: expected_bc7_len,
            actual: bc7.len(),
        });
    }

    let mut rgba_out = vec![0u8; width * height * 4];
    let row_bytes = width * 4 * 4;

    rgba_out.par_chunks_mut(row_bytes)
        .enumerate()
        .for_each(|(by, out_row)| {
            let row_bc7 = &bc7[by * blocks_x * 16..(by + 1) * blocks_x * 16];
            for bx in 0..blocks_x {
                let block = &row_bc7[bx * 16..(bx + 1) * 16];
                let dst = &mut out_row[bx * 16..];
                bcdec_rs::bc7(block, dst, width * 4);
            }
        });

    Ok(rgba_out)
}

/// Encodes one 4x4 block of RGBA pixels using BC7 Mode 6.
fn encode_bc7_block_mode6(px: &[[u8; 4]; 16], refine_iters: u32) -> [u8; 16] {
    // 1. Calculate mean and covariance in 4D RGBA space
    let mut mean = [0f32; 4];
    for p in px {
        for c in 0..4 {
            mean[c] += p[c] as f32;
        }
    }
    for c in 0..4 {
        mean[c] /= 16.0;
    }

    let mut cov = [[0f32; 4]; 4];
    for p in px {
        let d = [
            p[0] as f32 - mean[0],
            p[1] as f32 - mean[1],
            p[2] as f32 - mean[2],
            p[3] as f32 - mean[3],
        ];
        for i in 0..4 {
            for j in 0..4 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }

    // 2. Principal axis via power iteration
    let mut range = [0f32; 4];
    for c in 0..4 {
        let mut lo = 255f32;
        let mut hi = 0f32;
        for p in px {
            lo = lo.min(p[c] as f32);
            hi = hi.max(p[c] as f32);
        }
        range[c] = hi - lo;
    }

    let mut axis = range;
    for _ in 0..8 {
        let mut next = [0f32; 4];
        for i in 0..4 {
            for j in 0..4 {
                next[i] += cov[i][j] * axis[j];
            }
        }
        let len = (next[0] * next[0] + next[1] * next[1] + next[2] * next[2] + next[3] * next[3]).sqrt();
        if len > 1e-4 {
            for c in 0..4 {
                axis[c] = next[c] / len;
            }
        } else {
            break;
        }
    }

    // 3. Project pixels onto axis to find initial endpoints
    let mut min_t = f32::MAX;
    let mut max_t = f32::MIN;
    for p in px {
        let t = (p[0] as f32 - mean[0]) * axis[0]
            + (p[1] as f32 - mean[1]) * axis[1]
            + (p[2] as f32 - mean[2]) * axis[2]
            + (p[3] as f32 - mean[3]) * axis[3];
        min_t = min_t.min(t);
        max_t = max_t.max(t);
    }

    let mut ep0 = [0f32; 4];
    let mut ep1 = [0f32; 4];
    for c in 0..4 {
        ep0[c] = (mean[c] + min_t * axis[c]).clamp(0.0, 255.0);
        ep1[c] = (mean[c] + max_t * axis[c]).clamp(0.0, 255.0);
    }

    // Quantize endpoints to 7-bit + p-bit (8-bit representation)
    let (mut ep0_q, mut p0) = quantize_ep(&ep0);
    let (mut ep1_q, mut p1) = quantize_ep(&ep1);

    // 4. Assign indices
    let mut indices = assign_indices(px, &ep0_q, p0, &ep1_q, p1);

    // 5. Optional refinement
    for _ in 0..refine_iters {
        let (new_ep0, new_p0, new_ep1, new_p1) = refit_endpoints(px, &indices);
        let new_indices = assign_indices(px, &new_ep0, new_p0, &new_ep1, new_p1);
        ep0_q = new_ep0;
        p0 = new_p0;
        ep1_q = new_ep1;
        p1 = new_p1;
        indices = new_indices;
    }

    // In BC7 Mode 6, index 0 must have MSB = 0 (i.e. index 0 < 8)
    // If index[0] >= 8, swap endpoints and invert indices: idx = 15 - idx
    if indices[0] >= 8 {
        std::mem::swap(&mut ep0_q, &mut ep1_q);
        std::mem::swap(&mut p0, &mut p1);
        for idx in &mut indices {
            *idx = 15 - *idx;
        }
    }

    // 6. Pack into 128-bit (16-byte) Mode 6 block
    // Mode 6: header 0b01000000 (bit 6 set), followed by:
    // ep0[R,G,B,A] (7 bits each) = 28 bits
    // ep1[R,G,B,A] (7 bits each) = 28 bits
    // p0 (1 bit), p1 (1 bit) = 2 bits
    // indices: pixel 0 has 3 bits, pixels 1..15 have 4 bits = 3 + 15*4 = 63 bits
    // Total bits = 7 + 28 + 28 + 2 + 63 = 128 bits
    pack_mode6_block(&ep0_q, p0, &ep1_q, p1, &indices)
}

fn quantize_ep(ep: &[f32; 4]) -> ([u8; 4], u8) {
    let mut out = [0u8; 4];
    for c in 0..4 {
        let val = (ep[c].round() as i32).clamp(0, 255);
        out[c] = (val >> 1) as u8;
    }
    // Shared p-bit can be estimated from average residual
    let p_bit = if ep.iter().any(|&v| (v.round() as i32) & 1 != 0) { 1 } else { 0 };
    (out, p_bit)
}

fn dequantize_channel(q7: u8, p: u8) -> u8 {
    ((q7 << 1) | p) as u8
}

fn assign_indices(
    px: &[[u8; 4]; 16],
    ep0: &[u8; 4],
    p0: u8,
    ep1: &[u8; 4],
    p1: u8,
) -> [u8; 16] {
    let c0 = [
        dequantize_channel(ep0[0], p0) as i32,
        dequantize_channel(ep0[1], p0) as i32,
        dequantize_channel(ep0[2], p0) as i32,
        dequantize_channel(ep0[3], p0) as i32,
    ];
    let c1 = [
        dequantize_channel(ep1[0], p1) as i32,
        dequantize_channel(ep1[1], p1) as i32,
        dequantize_channel(ep1[2], p1) as i32,
        dequantize_channel(ep1[3], p1) as i32,
    ];

    let mut palette = [[0i32; 4]; 16];
    for i in 0..16 {
        let w = WEIGHTS4[i] as i32;
        for c in 0..4 {
            palette[i][c] = ((64 - w) * c0[c] + w * c1[c] + 32) >> 6;
        }
    }

    let mut indices = [0u8; 16];
    for (pi, p) in px.iter().enumerate() {
        let mut best_dist = i32::MAX;
        let mut best_idx = 0;
        for (i, entry) in palette.iter().enumerate() {
            let dr = p[0] as i32 - entry[0];
            let dg = p[1] as i32 - entry[1];
            let db = p[2] as i32 - entry[2];
            let da = p[3] as i32 - entry[3];
            let dist = dr * dr + dg * dg + db * db + da * da;
            if dist < best_dist {
                best_dist = dist;
                best_idx = i as u8;
            }
        }
        indices[pi] = best_idx;
    }
    indices
}

fn refit_endpoints(
    px: &[[u8; 4]; 16],
    indices: &[u8; 16],
) -> ([u8; 4], u8, [u8; 4], u8) {
    let mut sum_w = 0.0f32;
    let mut sum_w2 = 0.0f32;
    let mut sum_p = [0.0f32; 4];
    let mut sum_pw = [0.0f32; 4];

    for (i, p) in px.iter().enumerate() {
        let w = WEIGHTS4[indices[i] as usize] as f32 / 64.0;
        sum_w += w;
        sum_w2 += w * w;
        for c in 0..4 {
            let pv = p[c] as f32;
            sum_p[c] += pv;
            sum_pw[c] += pv * w;
        }
    }

    let n = 16.0f32;
    let denom = n * sum_w2 - sum_w * sum_w;
    if denom.abs() < 1e-4 {
        let (q, p) = quantize_ep(&[sum_p[0] / n, sum_p[1] / n, sum_p[2] / n, sum_p[3] / n]);
        return (q, p, q, p);
    }

    let mut ep0 = [0.0f32; 4];
    let mut ep1 = [0.0f32; 4];
    for c in 0..4 {
        let slope = (n * sum_pw[c] - sum_w * sum_p[c]) / denom;
        let intercept = (sum_p[c] - slope * sum_w) / n;
        ep0[c] = intercept.clamp(0.0, 255.0);
        ep1[c] = (intercept + slope).clamp(0.0, 255.0);
    }

    let (q0, p0) = quantize_ep(&ep0);
    let (q1, p1) = quantize_ep(&ep1);
    (q0, p0, q1, p1)
}

struct BitWriter {
    buf: [u8; 16],
    bit_pos: usize,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            buf: [0u8; 16],
            bit_pos: 0,
        }
    }

    fn write_bits(&mut self, mut value: u64, mut count: usize) {
        while count > 0 {
            let byte_idx = self.bit_pos / 8;
            let bit_offset = self.bit_pos % 8;
            let bits_avail_in_byte = 8 - bit_offset;
            let bits_to_write = count.min(bits_avail_in_byte);

            let mask = (1u64 << bits_to_write) - 1;
            let chunk = ((value & mask) as u8) << bit_offset;
            self.buf[byte_idx] |= chunk;

            value >>= bits_to_write;
            count -= bits_to_write;
            self.bit_pos += bits_to_write;
        }
    }
}

fn pack_mode6_block(
    ep0: &[u8; 4],
    p0: u8,
    ep1: &[u8; 4],
    p1: u8,
    indices: &[u8; 16],
) -> [u8; 16] {
    let mut w = BitWriter::new();
    // Mode 6 prefix: 0b01000000 (bit 6 is 1, trailing zeroes = 6 bits, so mode bits = 0b1000000)
    w.write_bits(0b1000000, 7);

    // Endpoints: R0, R1, G0, G1, B0, B1, A0, A1 (7 bits each)
    for c in 0..4 {
        w.write_bits(ep0[c] as u64, 7);
        w.write_bits(ep1[c] as u64, 7);
    }

    // P-bits: p0, p1 (1 bit each)
    w.write_bits(p0 as u64, 1);
    w.write_bits(p1 as u64, 1);

    // Indices: pixel 0 has 3 bits (MSB fixed to 0)
    w.write_bits(indices[0] as u64, 3);
    // Pixels 1..15 have 4 bits each
    for i in 1..16 {
        w.write_bits(indices[i] as u64, 4);
    }

    w.buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bc7_mode6_roundtrip() {
        let width = 8;
        let height = 8;
        let mut rgba = vec![0u8; width * height * 4];
        for i in 0..(width * height) {
            rgba[i * 4] = 200;
            rgba[i * 4 + 1] = 100;
            rgba[i * 4 + 2] = 50;
            rgba[i * 4 + 3] = 255;
        }

        let compressed = compress_bc7(&rgba, width, height).unwrap();
        assert_eq!(compressed.len(), (width * height / 16) * 16);

        let decompressed = decompress_bc7(&compressed, width, height).unwrap();
        assert_eq!(decompressed.len(), rgba.len());

        for i in 0..(width * height) {
            let dr = (rgba[i * 4] as i32 - decompressed[i * 4] as i32).abs();
            let dg = (rgba[i * 4 + 1] as i32 - decompressed[i * 4 + 1] as i32).abs();
            let db = (rgba[i * 4 + 2] as i32 - decompressed[i * 4 + 2] as i32).abs();
            assert!(dr <= 10, "dr: {}", dr);
            assert!(dg <= 10, "dg: {}", dg);
            assert!(db <= 10, "db: {}", db);
        }
    }
}
