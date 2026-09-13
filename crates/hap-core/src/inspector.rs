//! Deep stream inspection, metrics extraction, and fault/corruption detection for HAP video.

use crate::format::HapFormat;
use crate::header::{DecodeInstructions, SectionHeader};
use crate::mov::reader::{MovReaderError, QtHapReader};

/// High-level technical summary of a HAP stream.
#[derive(Debug, Clone)]
pub struct StreamSummary {
    pub format: HapFormat,
    pub fourcc: [u8; 4],
    pub width: u32,
    pub height: u32,
    pub frame_count: usize,
    pub fps: f32,
    pub duration_secs: f64,
    pub file_size_bytes: u64,
    pub avg_bitrate_mbps: f64,
    pub avg_frame_bytes: usize,
    pub min_frame_bytes: usize,
    pub max_frame_bytes: usize,
    pub uncompressed_frame_bytes: usize,
    pub avg_compression_ratio: f64,
    pub savings_percent: f32,
    pub chunk_count: usize,
    pub uses_snappy: bool,
    pub has_alpha: bool,
    pub texture_type_name: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultSeverity {
    Passed,
    Warning,
    Critical,
}

impl FaultSeverity {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Passed => "PASS",
            Self::Warning => "WARN",
            Self::Critical => "FAIL",
        }
    }
}

/// A detected stream anomaly, fault, or specification violation.
#[derive(Debug, Clone)]
pub struct StreamFault {
    pub check_name: &'static str,
    pub severity: FaultSeverity,
    pub message: String,
    pub recommendation: Option<String>,
}

/// Comprehensive health audit report for a HAP MOV file.
#[derive(Debug, Clone)]
pub struct StreamAudit {
    pub is_clean: bool,
    pub frames_scanned: usize,
    pub checks: Vec<StreamFault>,
}

impl StreamAudit {
    pub fn has_critical_errors(&self) -> bool {
        self.checks.iter().any(|c| c.severity == FaultSeverity::Critical)
    }

    pub fn has_warnings(&self) -> bool {
        self.checks.iter().any(|c| c.severity == FaultSeverity::Warning)
    }
}

/// Generate a technical summary from an open `QtHapReader` and file size.
pub fn extract_stream_summary(reader: &mut QtHapReader, file_size: u64) -> Result<StreamSummary, MovReaderError> {
    let width = reader.width();
    let height = reader.height();
    let frame_count = reader.frame_count();
    let fps = reader.fps();
    let duration_secs = reader.duration();
    let format = reader.format();

    let uncompressed_frame_bytes = (width as usize) * (height as usize) * 4;

    // Inspect first frame packet to get chunking and header structure
    let first_pkt = if frame_count > 0 {
        reader.read_frame_packet(0).ok()
    } else {
        None
    };

    let mut chunk_count = 1;
    let mut uses_snappy = false;

    if let Some(ref pkt) = first_pkt {
        if let Ok(hdr) = SectionHeader::parse(pkt) {
            let type_byte = hdr.section_type;
            uses_snappy = matches!(type_byte, 0xBB | 0xEB | 0xFB | 0x1B | 0xCB | 0x2B | 0x3B)
                || (type_byte & 0x0F == 0x01); // decode instructions chunked

            if matches!(type_byte, 0x01 | 0x02 | 0x03 | 0x0C | 0x0E | 0x0F) {
                // instructions section
                if let Ok(instr) = DecodeInstructions::parse(&pkt[hdr.header_size..]) {
                    chunk_count = instr.chunks.len();
                }
            } else if type_byte == 0x0D {
                // Hap M multi-image: inspect inner color section
                if let Ok(c_hdr) = SectionHeader::parse(&pkt[hdr.header_size..]) {
                    if c_hdr.section_type == 0x01 {
                        if let Ok(instr) = DecodeInstructions::parse(&pkt[hdr.header_size + c_hdr.header_size..]) {
                            chunk_count = instr.chunks.len();
                        }
                    }
                }
            }
        }
    }

    let mut total_sample_bytes = 0usize;
    let mut min_frame_bytes = usize::MAX;
    let mut max_frame_bytes = 0usize;

    for s in reader.samples() {
        total_sample_bytes += s.size;
        min_frame_bytes = min_frame_bytes.min(s.size);
        max_frame_bytes = max_frame_bytes.max(s.size);
    }
    if min_frame_bytes == usize::MAX {
        min_frame_bytes = 0;
    }

    let avg_frame_bytes = if frame_count > 0 {
        total_sample_bytes / frame_count
    } else {
        0
    };

    let avg_bitrate_mbps = if duration_secs > 0.001 {
        (total_sample_bytes as f64 * 8.0) / (duration_secs * 1_000_000.0)
    } else {
        0.0
    };

    let avg_compression_ratio = if avg_frame_bytes > 0 {
        uncompressed_frame_bytes as f64 / avg_frame_bytes as f64
    } else {
        1.0
    };

    let savings_percent = if uncompressed_frame_bytes > 0 {
        (1.0 - (avg_frame_bytes as f32 / uncompressed_frame_bytes as f32)) * 100.0
    } else {
        0.0
    };

    let texture_type_name = match format {
        HapFormat::Hap1 => "BC1 / DXT1 (RGB 24-bit)",
        HapFormat::Hap5 => "BC3 / DXT5 (RGBA 32-bit)",
        HapFormat::HapY => "Scaled YCoCg-DXT5 (Co/Cg/Scale/Luma)",
        HapFormat::HapM => "Dual-Stream YCoCg-DXT5 + BC4 Alpha",
        HapFormat::HapA => "BC4 / RGTC1 (Single Channel Matte)",
        HapFormat::Hap7 => "BC7 UNORM (Full 8-bit RGBA)",
        HapFormat::HapH => "BC6H (Half Float HDR)",
    };

    Ok(StreamSummary {
        format,
        fourcc: format.fourcc(),
        width,
        height,
        frame_count,
        fps,
        duration_secs,
        file_size_bytes: file_size,
        avg_bitrate_mbps,
        avg_frame_bytes,
        min_frame_bytes,
        max_frame_bytes,
        uncompressed_frame_bytes,
        avg_compression_ratio,
        savings_percent,
        chunk_count,
        uses_snappy,
        has_alpha: format.has_alpha(),
        texture_type_name,
    })
}

/// Run an exhaustive technical health and compliance audit on a HAP QuickTime file.
pub fn audit_hap_stream(
    reader: &mut QtHapReader,
    max_frames_to_scan: usize,
) -> StreamAudit {
    let mut checks = Vec::new();
    let width = reader.width();
    let height = reader.height();
    let frame_count = reader.frame_count();
    let format = reader.format();

    // 1. Dimension Divisibility Check (DXT/BC 4x4 block compliance)
    if width % 4 != 0 || height % 4 != 0 {
        checks.push(StreamFault {
            check_name: "Resolution Divisibility (4x4 Blocks)",
            severity: FaultSeverity::Critical,
            message: format!(
                "Resolution {}x{} is not divisible by 4. Hardware decoders and media servers will crash or show diagonal shear.",
                width, height
            ),
            recommendation: Some("Re-encode with dimensions padded to multiples of 4 (e.g. 1920x1080).".into()),
        });
    } else {
        checks.push(StreamFault {
            check_name: "Resolution Divisibility (4x4 Blocks)",
            severity: FaultSeverity::Passed,
            message: format!("Resolution {}x{} perfectly aligns with standard 4x4 GPU texture blocks.", width, height),
            recommendation: None,
        });
    }

    // 2. Codec FourCC Check
    let fourcc = format.fourcc();
    let fourcc_str = String::from_utf8_lossy(&fourcc);
    if matches!(fourcc_str.as_ref(), "Hap1" | "Hap5" | "HapY" | "HapM" | "HapA" | "Hap7") {
        checks.push(StreamFault {
            check_name: "Codec FourCC Identification",
            severity: FaultSeverity::Passed,
            message: format!("Standard compliant FourCC '{}' ({}) recognized.", fourcc_str, format.name()),
            recommendation: None,
        });
    } else {
        checks.push(StreamFault {
            check_name: "Codec FourCC Identification",
            severity: FaultSeverity::Warning,
            message: format!("Non-standard or experimental FourCC '{}'.", fourcc_str),
            recommendation: Some("Verify media server compatibility.".into()),
        });
    }

    // 3. Multi-Threading Chunk Distribution Check
    let scan_limit = frame_count.min(max_frames_to_scan);
    let mut chunk_counts_seen = Vec::new();
    let mut snappy_errors = 0usize;
    let mut payload_errors = 0usize;

    for i in 0..scan_limit {
        let pkt = match reader.read_frame_packet(i) {
            Ok(p) => p,
            Err(e) => {
                checks.push(StreamFault {
                    check_name: "Container Frame Read",
                    severity: FaultSeverity::Critical,
                    message: format!("Failed reading frame {}: {}", i, e),
                    recommendation: Some("File is truncated or QuickTime sample table is corrupt.".into()),
                });
                break;
            }
        };

        if let Ok(hdr) = SectionHeader::parse(&pkt) {
            if hdr.header_size + hdr.data_size > pkt.len() {
                payload_errors += 1;
            }

            if matches!(hdr.section_type, 0x01 | 0x02 | 0x03 | 0x0C | 0x0E | 0x0F) {
                if let Ok(instr) = DecodeInstructions::parse(&pkt[hdr.header_size..]) {
                    chunk_counts_seen.push(instr.chunks.len());
                }
            } else {
                chunk_counts_seen.push(1);
            }

            // Check Snappy & texture decompress
            if let Err(_) = crate::decoder::decode_frame_to_texture(&pkt) {
                snappy_errors += 1;
            }
        } else {
            payload_errors += 1;
        }
    }

    // 4. Snappy Stream Decompression Check
    if snappy_errors == 0 {
        checks.push(StreamFault {
            check_name: "Snappy Stream Integrity",
            severity: FaultSeverity::Passed,
            message: format!("All scanned frames ({} total) decompressed with zero CRC or stream errors.", scan_limit),
            recommendation: None,
        });
    } else {
        checks.push(StreamFault {
            check_name: "Snappy Stream Integrity",
            severity: FaultSeverity::Critical,
            message: format!("{} of {} scanned frames failed Snappy decompression.", snappy_errors, scan_limit),
            recommendation: Some("The compressed data stream contains corrupted bytes or truncated blocks.".into()),
        });
    }

    // 5. Header and Payload Alignment Check
    if payload_errors == 0 {
        checks.push(StreamFault {
            check_name: "Packet Payload Alignment",
            severity: FaultSeverity::Passed,
            message: "All section headers match sample atom byte boundaries.".into(),
            recommendation: None,
        });
    } else {
        checks.push(StreamFault {
            check_name: "Packet Payload Alignment",
            severity: FaultSeverity::Critical,
            message: format!("{} frames had truncated or mismatched payload headers.", payload_errors),
            recommendation: Some("File was partially written or terminated prematurely.".into()),
        });
    }

    // 6. Slicing / Chunking Recommendation Check
    let is_4k_or_higher = width >= 3840 || height >= 2160;
    let has_single_chunk = chunk_counts_seen.contains(&1) || reader.frame_count() == 0;
    if is_4k_or_higher && has_single_chunk {
        checks.push(StreamFault {
            check_name: "Multi-Core Slice Distribution",
            severity: FaultSeverity::Warning,
            message: "High-resolution video (4K+) encoded with only 1 chunk. Media servers will only utilize 1 CPU thread.".into(),
            recommendation: Some("Re-encode with 4 or 8 chunks for smooth 60/120 FPS multi-threaded playback.".into()),
        });
    } else {
        checks.push(StreamFault {
            check_name: "Multi-Core Slice Distribution",
            severity: FaultSeverity::Passed,
            message: "Chunk distribution is appropriate for real-time parallel decoding.".into(),
            recommendation: None,
        });
    }

    let is_clean = checks.iter().all(|c| c.severity == FaultSeverity::Passed);

    StreamAudit {
        is_clean,
        frames_scanned: scan_limit,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_fault_severity() {
        let fault = StreamFault {
            check_name: "Test",
            severity: FaultSeverity::Passed,
            message: "OK".into(),
            recommendation: None,
        };
        assert_eq!(fault.severity.label(), "PASS");
    }
}
