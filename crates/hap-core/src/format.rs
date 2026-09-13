//! HAP video format definitions, FourCC codes, and specification parameters.

use std::fmt;

/// All supported flavours of the HAP video codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HapFormat {
    /// Standard Hap: RGB DXT1/BC1, 8 bytes per 4x4 block, 8:1 compression, no alpha.
    Hap1,
    /// Hap Alpha: RGBA DXT5/BC3, 16 bytes per 4x4 block, 4:1 compression, smooth 8-bit alpha.
    Hap5,
    /// Hap Q: High quality RGB using Scaled YCoCg stored in DXT5/BC3 blocks.
    HapY,
    /// Hap Q Alpha: Multi-image container with YCoCg DXT5 (color) + BC4/RGTC1 (alpha).
    HapM,
    /// Hap Alpha-Only: BC4 / RGTC1 single-channel 8-bit alpha / matte mask.
    HapA,
    /// Hap R (Hap 7): Modern RGBA BPTC / BC7 UNORM with 8 partition modes for high visual fidelity.
    Hap7,
    /// Hap HDR: RGB BPTC / BC6H float for high-dynamic-range video.
    HapH,
}

impl HapFormat {
    /// List of all supported formats.
    pub const ALL: &'static [HapFormat] = &[
        HapFormat::Hap1,
        HapFormat::Hap5,
        HapFormat::HapY,
        HapFormat::HapM,
        HapFormat::HapA,
        HapFormat::Hap7,
        HapFormat::HapH,
    ];

    /// Returns the human-readable display name.
    pub fn name(&self) -> &'static str {
        match self {
            HapFormat::Hap1 => "Hap (Hap 1)",
            HapFormat::Hap5 => "Hap Alpha (Hap 5)",
            HapFormat::HapY => "Hap Q",
            HapFormat::HapM => "Hap Q Alpha",
            HapFormat::HapA => "Hap Alpha-Only",
            HapFormat::Hap7 => "Hap R (Hap 7 / BC7)",
            HapFormat::HapH => "Hap HDR (BC6H)",
        }
    }

    /// Returns the QuickTime 4-character code (FourCC).
    pub fn fourcc(&self) -> [u8; 4] {
        match self {
            HapFormat::Hap1 => *b"Hap1",
            HapFormat::Hap5 => *b"Hap5",
            HapFormat::HapY => *b"HapY",
            HapFormat::HapM => *b"HapM",
            HapFormat::HapA => *b"HapA",
            HapFormat::Hap7 => *b"Hap7",
            HapFormat::HapH => *b"HapH",
        }
    }

    /// Try parsing a format from a 4-character FourCC code.
    pub fn from_fourcc(fourcc: &[u8; 4]) -> Option<Self> {
        match fourcc {
            b"Hap1" => Some(HapFormat::Hap1),
            b"Hap5" => Some(HapFormat::Hap5),
            b"HapY" => Some(HapFormat::HapY),
            b"HapM" => Some(HapFormat::HapM),
            b"HapA" => Some(HapFormat::HapA),
            b"Hap7" | b"HapR" | b"hapr" => Some(HapFormat::Hap7),
            b"HapH" => Some(HapFormat::HapH),
            _ => None,
        }
    }

    /// Top-level section type byte for raw (uncompressed) frames.
    pub fn uncompressed_type_byte(&self) -> u8 {
        match self {
            HapFormat::Hap1 => 0xAB,
            HapFormat::Hap5 => 0xAE,
            HapFormat::HapY => 0xAF,
            HapFormat::HapM => 0x0D, // Multiple images container
            HapFormat::HapA => 0xA1,
            HapFormat::Hap7 => 0xAC,
            HapFormat::HapH => 0xA2, // BC6U float
        }
    }

    /// Top-level section type byte for Snappy compressed frames.
    pub fn snappy_type_byte(&self) -> u8 {
        match self {
            HapFormat::Hap1 => 0xBB,
            HapFormat::Hap5 => 0xBE,
            HapFormat::HapY => 0xBF,
            HapFormat::HapM => 0x0D, // Multiple images container
            HapFormat::HapA => 0xB1,
            HapFormat::Hap7 => 0xBC,
            HapFormat::HapH => 0xB2,
        }
    }

    /// Top-level section type byte for chunked / decode instruction frames.
    pub fn chunked_type_byte(&self) -> u8 {
        match self {
            HapFormat::Hap1 => 0xCB,
            HapFormat::Hap5 => 0xCE,
            HapFormat::HapY => 0xCF,
            HapFormat::HapM => 0x0D,
            HapFormat::HapA => 0xC1,
            HapFormat::Hap7 => 0xCC,
            HapFormat::HapH => 0xC2,
        }
    }

    /// Number of bytes per 4x4 pixel block in the primary texture stream.
    pub fn bytes_per_block(&self) -> usize {
        match self {
            HapFormat::Hap1 => 8,   // DXT1
            HapFormat::Hap5 => 16,  // DXT5
            HapFormat::HapY => 16,  // DXT5 YCoCg
            HapFormat::HapM => 24,  // 16 (DXT5 YCoCg) + 8 (BC4 Alpha)
            HapFormat::HapA => 8,   // BC4
            HapFormat::Hap7 => 16,  // BC7
            HapFormat::HapH => 16,  // BC6H
        }
    }

    /// Returns true if this format supports an alpha / transparency channel.
    pub fn has_alpha(&self) -> bool {
        matches!(
            self,
            HapFormat::Hap5 | HapFormat::HapM | HapFormat::HapA | HapFormat::Hap7
        )
    }

    /// Returns true if this is a high dynamic range format.
    pub fn is_hdr(&self) -> bool {
        matches!(self, HapFormat::HapH)
    }

    /// Typical bit depth (24 for RGB, 32 for RGBA).
    pub fn bits_per_pixel(&self) -> u16 {
        if self.has_alpha() {
            32
        } else {
            24
        }
    }
}

impl fmt::Display for HapFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}
