//! QuickTime MOV container demuxer for HAP video streams.

use crate::format::HapFormat;
use crate::mov::atoms::*;
use byteorder::{BigEndian, ByteOrder, ReadBytesExt};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MovReaderError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Invalid MOV file: {0}")]
    InvalidFile(String),
    #[error("No HAP video track found in MOV")]
    NoHapTrackFound,
    #[error("Unsupported codec: {0}")]
    UnsupportedCodec(String),
    #[error("Frame index {index} out of bounds (total frames: {total})")]
    FrameIndexOutOfBounds { index: usize, total: usize },
}

#[derive(Debug, Clone)]
pub struct FrameSample {
    pub offset: u64,
    pub size: usize,
}

/// Pure Rust QuickTime MOV reader for HAP streams.
pub struct QtHapReader {
    file: File,
    width: u32,
    height: u32,
    fps: f32,
    duration: f64,
    format: HapFormat,
    samples: Vec<FrameSample>,
}

impl QtHapReader {
    /// Open and parse a QuickTime MOV file.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, MovReaderError> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();

        let mut width = 0u32;
        let mut height = 0u32;
        let mut timescale = 0u32;
        let mut duration_units = 0u64;
        let mut format = None;
        let mut samples = Vec::new();
        let mut fps = 0.0f32;

        let mut pos = 0u64;
        while pos < file_len {
            file.seek(SeekFrom::Start(pos))?;
            let (atom_type, atom_size, header_len) = match read_atom_header(&mut file, file_len - pos) {
                Ok(res) => res,
                Err(_) => break,
            };

            if atom_type == ATOM_MOOV {
                let mut moov_payload = vec![0u8; (atom_size - header_len) as usize];
                file.read_exact(&mut moov_payload)?;

                parse_moov(
                    &moov_payload,
                    &mut width,
                    &mut height,
                    &mut timescale,
                    &mut duration_units,
                    &mut format,
                    &mut samples,
                    &mut fps,
                )?;
                break;
            }

            if atom_size == 0 {
                break;
            }
            pos += atom_size;
        }

        let hap_format = format.ok_or(MovReaderError::NoHapTrackFound)?;
        if samples.is_empty() {
            return Err(MovReaderError::InvalidFile("No video samples found".into()));
        }

        let duration = if timescale > 0 {
            duration_units as f64 / timescale as f64
        } else if fps > 0.0 {
            samples.len() as f64 / fps as f64
        } else {
            0.0
        };

        if fps <= 0.0 && duration > 0.0 {
            fps = (samples.len() as f64 / duration) as f32;
        }

        Ok(Self {
            file,
            width,
            height,
            fps,
            duration,
            format: hap_format,
            samples,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn fps(&self) -> f32 {
        self.fps
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn format(&self) -> HapFormat {
        self.format
    }

    pub fn frame_count(&self) -> usize {
        self.samples.len()
    }

    pub fn samples(&self) -> &[FrameSample] {
        &self.samples
    }

    /// Read the raw HAP frame packet for a given 0-indexed frame.
    pub fn read_frame_packet(&mut self, frame_idx: usize) -> Result<Vec<u8>, MovReaderError> {
        if frame_idx >= self.samples.len() {
            return Err(MovReaderError::FrameIndexOutOfBounds {
                index: frame_idx,
                total: self.samples.len(),
            });
        }

        let sample = &self.samples[frame_idx];
        self.file.seek(SeekFrom::Start(sample.offset))?;
        let mut buf = vec![0u8; sample.size];
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }
}

fn read_atom_header(r: &mut File, remaining: u64) -> Result<([u8; 4], u64, u64), MovReaderError> {
    if remaining < 8 {
        return Err(MovReaderError::InvalidFile("Truncated atom header".into()));
    }
    let size32 = r.read_u32::<BigEndian>()? as u64;
    let mut atom_type = [0u8; 4];
    r.read_exact(&mut atom_type)?;

    if size32 == 1 {
        if remaining < 16 {
            return Err(MovReaderError::InvalidFile("Truncated 64-bit atom header".into()));
        }
        let size64 = r.read_u64::<BigEndian>()?;
        Ok((atom_type, size64, 16))
    } else if size32 == 0 {
        Ok((atom_type, remaining, 8))
    } else {
        Ok((atom_type, size32, 8))
    }
}

fn parse_moov(
    buf: &[u8],
    width: &mut u32,
    height: &mut u32,
    timescale: &mut u32,
    duration: &mut u64,
    format: &mut Option<HapFormat>,
    samples: &mut Vec<FrameSample>,
    fps: &mut f32,
) -> Result<(), MovReaderError> {
    let mut offset = 0;
    while offset + 8 <= buf.len() {
        let size = BigEndian::read_u32(&buf[offset..offset + 4]) as usize;
        let atom_type = &buf[offset + 4..offset + 8];
        let actual_size = if size == 0 { buf.len() - offset } else { size };
        if offset + actual_size > buf.len() {
            break;
        }

        let payload = &buf[offset + 8..offset + actual_size];

        if atom_type == &ATOM_MVHD && payload.len() >= 20 {
            let version = payload[0];
            if version == 0 && payload.len() >= 20 {
                *timescale = BigEndian::read_u32(&payload[12..16]);
                *duration = BigEndian::read_u32(&payload[16..20]) as u64;
            } else if version == 1 && payload.len() >= 28 {
                *timescale = BigEndian::read_u32(&payload[20..24]);
                *duration = BigEndian::read_u64(&payload[24..32]);
            }
        } else if atom_type == &ATOM_TRAK {
            let mut track_fmt = None;
            let mut track_w = 0u32;
            let mut track_h = 0u32;
            let mut track_samples = Vec::new();
            let mut track_fps = 0.0f32;

            if parse_trak(payload, &mut track_w, &mut track_h, &mut track_fmt, &mut track_samples, &mut track_fps).is_ok() {
                if let Some(fmt) = track_fmt {
                    *width = track_w;
                    *height = track_h;
                    *format = Some(fmt);
                    *samples = track_samples;
                    *fps = track_fps;
                    return Ok(());
                }
            }
        }

        offset += actual_size;
    }

    Ok(())
}

fn parse_trak(
    buf: &[u8],
    width: &mut u32,
    height: &mut u32,
    format: &mut Option<HapFormat>,
    samples: &mut Vec<FrameSample>,
    fps: &mut f32,
) -> Result<(), MovReaderError> {
    let mut offset = 0;
    while offset + 8 <= buf.len() {
        let size = BigEndian::read_u32(&buf[offset..offset + 4]) as usize;
        let atom_type = &buf[offset + 4..offset + 8];
        let actual_size = if size == 0 { buf.len() - offset } else { size };
        if offset + actual_size > buf.len() {
            break;
        }
        let payload = &buf[offset + 8..offset + actual_size];

        if atom_type == &ATOM_TKHD && payload.len() >= 80 {
            // Track dimensions in 16.16 fixed point at offset 76..84
            let w_fixed = BigEndian::read_u32(&payload[payload.len() - 8..payload.len() - 4]);
            let h_fixed = BigEndian::read_u32(&payload[payload.len() - 4..payload.len()]);
            *width = w_fixed >> 16;
            *height = h_fixed >> 16;
        } else if atom_type == &ATOM_MDIA {
            parse_mdia(payload, format, samples, fps)?;
        }

        offset += actual_size;
    }

    Ok(())
}

fn parse_mdia(
    buf: &[u8],
    format: &mut Option<HapFormat>,
    samples: &mut Vec<FrameSample>,
    fps: &mut f32,
) -> Result<(), MovReaderError> {
    let mut offset = 0;
    let mut timescale = 0u32;

    while offset + 8 <= buf.len() {
        let size = BigEndian::read_u32(&buf[offset..offset + 4]) as usize;
        let atom_type = &buf[offset + 4..offset + 8];
        let actual_size = if size == 0 { buf.len() - offset } else { size };
        if offset + actual_size > buf.len() {
            break;
        }
        let payload = &buf[offset + 8..offset + actual_size];

        if atom_type == &ATOM_MDHD && payload.len() >= 20 {
            let version = payload[0];
            if version == 0 {
                timescale = BigEndian::read_u32(&payload[12..16]);
            } else if payload.len() >= 28 {
                timescale = BigEndian::read_u32(&payload[20..24]);
            }
        } else if atom_type == &ATOM_MINF {
            parse_minf(payload, timescale, format, samples, fps)?;
        }

        offset += actual_size;
    }

    Ok(())
}

fn parse_minf(
    buf: &[u8],
    timescale: u32,
    format: &mut Option<HapFormat>,
    samples: &mut Vec<FrameSample>,
    fps: &mut f32,
) -> Result<(), MovReaderError> {
    let mut offset = 0;
    while offset + 8 <= buf.len() {
        let size = BigEndian::read_u32(&buf[offset..offset + 4]) as usize;
        let atom_type = &buf[offset + 4..offset + 8];
        let actual_size = if size == 0 { buf.len() - offset } else { size };
        if offset + actual_size > buf.len() {
            break;
        }
        let payload = &buf[offset + 8..offset + actual_size];

        if atom_type == &ATOM_STBL {
            parse_stbl(payload, timescale, format, samples, fps)?;
        }

        offset += actual_size;
    }

    Ok(())
}

fn parse_stbl(
    buf: &[u8],
    timescale: u32,
    format: &mut Option<HapFormat>,
    samples: &mut Vec<FrameSample>,
    fps: &mut f32,
) -> Result<(), MovReaderError> {
    let mut offset = 0;
    let mut sample_sizes = Vec::new();
    let mut chunk_offsets = Vec::new();
    let mut stsc_entries = Vec::new();

    while offset + 8 <= buf.len() {
        let size = BigEndian::read_u32(&buf[offset..offset + 4]) as usize;
        let atom_type = &buf[offset + 4..offset + 8];
        let actual_size = if size == 0 { buf.len() - offset } else { size };
        if offset + actual_size > buf.len() {
            break;
        }
        let payload = &buf[offset + 8..offset + actual_size];

        if atom_type == &ATOM_STSD && payload.len() >= 16 {
            let entry_count = BigEndian::read_u32(&payload[4..8]);
            if entry_count >= 1 && payload.len() >= 16 {
                let fourcc = [payload[12], payload[13], payload[14], payload[15]];
                if let Some(f) = HapFormat::from_fourcc(&fourcc) {
                    *format = Some(f);
                }
            }
        } else if atom_type == &ATOM_STTS && payload.len() >= 16 {
            let entry_count = BigEndian::read_u32(&payload[4..8]);
            if entry_count >= 1 {
                let sample_duration = BigEndian::read_u32(&payload[12..16]);
                if sample_duration > 0 && timescale > 0 {
                    *fps = timescale as f32 / sample_duration as f32;
                }
            }
        } else if atom_type == &ATOM_STSC && payload.len() >= 8 {
            let entry_count = BigEndian::read_u32(&payload[4..8]) as usize;
            let mut p = 8;
            for _ in 0..entry_count {
                if p + 12 <= payload.len() {
                    let first_chunk = BigEndian::read_u32(&payload[p..p + 4]);
                    let samples_per_chunk = BigEndian::read_u32(&payload[p + 4..p + 8]);
                    let desc_idx = BigEndian::read_u32(&payload[p + 8..p + 12]);
                    stsc_entries.push((first_chunk, samples_per_chunk, desc_idx));
                    p += 12;
                }
            }
        } else if atom_type == &ATOM_STSZ && payload.len() >= 12 {
            let uniform_size = BigEndian::read_u32(&payload[4..8]) as usize;
            let count = BigEndian::read_u32(&payload[8..12]) as usize;
            if uniform_size != 0 {
                sample_sizes = vec![uniform_size; count];
            } else {
                let mut p = 12;
                for _ in 0..count {
                    if p + 4 <= payload.len() {
                        sample_sizes.push(BigEndian::read_u32(&payload[p..p + 4]) as usize);
                        p += 4;
                    }
                }
            }
        } else if atom_type == &ATOM_STCO && payload.len() >= 8 {
            let count = BigEndian::read_u32(&payload[4..8]) as usize;
            let mut p = 8;
            for _ in 0..count {
                if p + 4 <= payload.len() {
                    chunk_offsets.push(BigEndian::read_u32(&payload[p..p + 4]) as u64);
                    p += 4;
                }
            }
        } else if atom_type == &ATOM_CO64 && payload.len() >= 8 {
            let count = BigEndian::read_u32(&payload[4..8]) as usize;
            let mut p = 8;
            for _ in 0..count {
                if p + 8 <= payload.len() {
                    chunk_offsets.push(BigEndian::read_u64(&payload[p..p + 8]));
                    p += 8;
                }
            }
        }

        offset += actual_size;
    }

    // Reconstruct sample positions from chunk offsets and sample sizes
    if !sample_sizes.is_empty() && !chunk_offsets.is_empty() {
        if stsc_entries.is_empty() || (stsc_entries.len() == 1 && stsc_entries[0].1 == 1) {
            // 1 sample per chunk
            for (i, &size) in sample_sizes.iter().enumerate() {
                if i < chunk_offsets.len() {
                    samples.push(FrameSample {
                        offset: chunk_offsets[i],
                        size,
                    });
                }
            }
        } else {
            // Multiple samples per chunk: expand stsc
            let mut sample_idx = 0;
            for chunk_idx in 0..chunk_offsets.len() {
                let chunk_num = (chunk_idx + 1) as u32;
                // Find matching stsc entry
                let mut samples_in_chunk = 1;
                for entry in &stsc_entries {
                    if chunk_num >= entry.0 {
                        samples_in_chunk = entry.1 as usize;
                    }
                }

                let mut current_offset = chunk_offsets[chunk_idx];
                for _ in 0..samples_in_chunk {
                    if sample_idx < sample_sizes.len() {
                        let size = sample_sizes[sample_idx];
                        samples.push(FrameSample {
                            offset: current_offset,
                            size,
                        });
                        current_offset += size as u64;
                        sample_idx += 1;
                    }
                }
            }
        }
    }

    Ok(())
}
