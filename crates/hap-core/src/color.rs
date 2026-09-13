//! Color range, alpha mode, and preprocessing operations for professional HAP encoding.

use rayon::prelude::*;
use std::borrow::Cow;
use crate::format::HapFormat;

/// Input color range / levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorRange {
    /// Full PC / Graphics range 0..=255.
    #[default]
    Full,
    /// Limited / Studio video range (16..=235 for RGB/Luma).
    /// Expands video levels to 0..=255 to prevent washed-out milky blacks on GPU displays.
    Limited,
}

/// Alpha channel transparency processing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlphaMode {
    /// Straight (unassociated) alpha: leave RGB colors unmodified.
    #[default]
    Straight,
    /// Premultiply alpha: RGB = (RGB * Alpha + 127) / 255.
    /// Eliminates dark fringing halos in real-time blend shaders (Resolume, Unreal, etc).
    Premultiply,
    /// Demultiply (un-premultiply) alpha: RGB = (RGB * 255 + Alpha/2) / Alpha.
    /// Strips pre-baked black fringe from graphics assets.
    Demultiply,
    /// Discard alpha channel: forces Alpha = 255 (completely opaque).
    Discard,
}

/// Chroma dithering mode to reduce color banding on large LED walls and laser projectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DitherMode {
    /// No dithering (fastest).
    #[default]
    None,
    /// 4x4 Bayer spatial ordered dithering.
    Bayer4x4,
}

/// Compression quality vs. encoding speed preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityPreset {
    /// Fast / Draft (RangeFit): ~3x faster encode speed, ideal for quick rushes.
    Draft,
    /// Balanced / High quality (ClusterFit): optimal endpoint clustering for show delivery.
    #[default]
    Production,
}

/// Comprehensive encoding options for HAP video creation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncodeOptions {
    pub format: HapFormat,
    pub chunk_count: usize,
    pub use_snappy: bool,
    pub color_range: ColorRange,
    pub alpha_mode: AlphaMode,
    pub dither_mode: DitherMode,
    pub quality: QualityPreset,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            format: HapFormat::HapY,
            chunk_count: 4,
            use_snappy: true,
            color_range: ColorRange::Full,
            alpha_mode: AlphaMode::Straight,
            dither_mode: DitherMode::None,
            quality: QualityPreset::Production,
        }
    }
}

/// Compile-time lookup table mapping 16..=235 studio levels to 0..=255 full range.
pub static LIMITED_TO_FULL_LUT: [u8; 256] = {
    let mut lut = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        let val = i as i32;
        // Broadcast video range 16..=235 mapped to 0..=255: (val - 16) * 255 / (235 - 16)
        let expanded = ((val - 16) * 255 + 109) / 219;
        lut[i] = if expanded < 0 {
            0
        } else if expanded > 255 {
            255
        } else {
            expanded as u8
        };
        i += 1;
    }
    lut
};

/// 4x4 Bayer spatial dithering matrix.
pub const BAYER_4X4: [[i32; 4]; 4] = [
    [ 0,  8,  2, 10],
    [12,  4, 14,  6],
    [ 3, 11,  1,  9],
    [15,  7, 13,  5],
];

/// Preprocess an RGBA8 buffer according to color range and alpha mode.
/// If settings are default (`Full` range and `Straight` alpha), zero allocations or copies occur.
pub fn preprocess_rgba<'a>(
    rgba: &'a [u8],
    width: usize,
    height: usize,
    color_range: ColorRange,
    alpha_mode: AlphaMode,
) -> Cow<'a, [u8]> {
    assert_eq!(rgba.len(), width * height * 4);

    if color_range == ColorRange::Full && alpha_mode == AlphaMode::Straight {
        return Cow::Borrowed(rgba);
    }

    let mut out = rgba.to_vec();
    const CHUNK_PIXELS: usize = 1024;
    let chunk_bytes = CHUNK_PIXELS * 4;

    out.par_chunks_mut(chunk_bytes).for_each(|chunk| {
        for pixel in chunk.chunks_exact_mut(4) {
            let mut r = pixel[0];
            let mut g = pixel[1];
            let mut b = pixel[2];
            let mut a = pixel[3];

            // 1. Color Range Expansion (16..235 -> 0..255)
            if color_range == ColorRange::Limited {
                r = LIMITED_TO_FULL_LUT[r as usize];
                g = LIMITED_TO_FULL_LUT[g as usize];
                b = LIMITED_TO_FULL_LUT[b as usize];
            }

            // 2. Alpha Processing
            match alpha_mode {
                AlphaMode::Straight => {}
                AlphaMode::Premultiply => {
                    let a32 = a as u32;
                    r = ((r as u32 * a32 + 127) / 255) as u8;
                    g = ((g as u32 * a32 + 127) / 255) as u8;
                    b = ((b as u32 * a32 + 127) / 255) as u8;
                }
                AlphaMode::Demultiply => {
                    if a > 0 {
                        let a32 = a as u32;
                        r = (((r as u32 * 255) + (a32 / 2)) / a32).min(255) as u8;
                        g = (((g as u32 * 255) + (a32 / 2)) / a32).min(255) as u8;
                        b = (((b as u32 * 255) + (a32 / 2)) / a32).min(255) as u8;
                    } else {
                        r = 0;
                        g = 0;
                        b = 0;
                    }
                }
                AlphaMode::Discard => {
                    a = 255;
                }
            }

            pixel[0] = r;
            pixel[1] = g;
            pixel[2] = b;
            pixel[3] = a;
        }
    });

    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limited_range_expansion() {
        assert_eq!(LIMITED_TO_FULL_LUT[16], 0);
        assert_eq!(LIMITED_TO_FULL_LUT[235], 255);
        assert_eq!(LIMITED_TO_FULL_LUT[0], 0);
        assert_eq!(LIMITED_TO_FULL_LUT[255], 255);
        // Midpoint around 125.5
        assert!((LIMITED_TO_FULL_LUT[125] as i32 - 127).abs() <= 1);
    }

    #[test]
    fn test_alpha_premultiply_and_demultiply() {
        let input = vec![200u8, 100u8, 50u8, 128u8];
        let premul = preprocess_rgba(&input, 1, 1, ColorRange::Full, AlphaMode::Premultiply);
        assert!((premul[0] as i32 - 100).abs() <= 1);
        assert!((premul[1] as i32 - 50).abs() <= 1);
        assert!((premul[2] as i32 - 25).abs() <= 1);
        assert_eq!(premul[3], 128);

        let demul = preprocess_rgba(&premul, 1, 1, ColorRange::Full, AlphaMode::Demultiply);
        assert!((demul[0] as i32 - 200).abs() <= 2);
        assert!((demul[1] as i32 - 100).abs() <= 2);
        assert!((demul[2] as i32 - 50).abs() <= 2);
    }

    #[test]
    fn test_alpha_discard() {
        let input = vec![120u8, 130u8, 140u8, 0u8];
        let result = preprocess_rgba(&input, 1, 1, ColorRange::Full, AlphaMode::Discard);
        assert_eq!(result[0], 120);
        assert_eq!(result[1], 130);
        assert_eq!(result[2], 140);
        assert_eq!(result[3], 255);
    }
}
