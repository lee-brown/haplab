//! HAP section headers, chunk tables, and container structures per the HAP specification.

use byteorder::{ByteOrder, LittleEndian};
use thiserror::Error;

/// Error parsing or constructing HAP frame headers.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error("Buffer too small: expected at least {expected} bytes, got {actual}")]
    BufferTooSmall { expected: usize, actual: usize },
    #[error("Section extends past buffer: need {need} bytes, have {have}")]
    Truncated { need: usize, have: usize },
    #[error("Unknown or invalid section type: 0x{0:02X}")]
    UnknownSectionType(u8),
    #[error("Malformed decode instructions: {0}")]
    InvalidDecodeInstructions(String),
    #[error("Malformed multi-image section: {0}")]
    InvalidMultiImage(String),
}

/// A parsed section header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionHeader {
    /// Total bytes in the header itself (4 or 8).
    pub header_size: usize,
    /// Size of the section data (excluding header).
    pub data_size: usize,
    /// Type identifier byte.
    pub section_type: u8,
}

impl SectionHeader {
    /// Parse a section header from the beginning of a buffer.
    pub fn parse(buf: &[u8]) -> Result<Self, HeaderError> {
        if buf.len() < 4 {
            return Err(HeaderError::BufferTooSmall {
                expected: 4,
                actual: buf.len(),
            });
        }

        if buf[0] == 0 && buf[1] == 0 && buf[2] == 0 {
            // 8-byte header: [0, 0, 0, type, len: u32 LE]
            if buf.len() < 8 {
                return Err(HeaderError::BufferTooSmall {
                    expected: 8,
                    actual: buf.len(),
                });
            }
            let section_type = buf[3];
            let data_size = LittleEndian::read_u32(&buf[4..8]) as usize;
            Ok(Self {
                header_size: 8,
                data_size,
                section_type,
            })
        } else {
            // 4-byte header: [len: 3 bytes LE, type]
            let data_size = (buf[0] as usize) | ((buf[1] as usize) << 8) | ((buf[2] as usize) << 16);
            let section_type = buf[3];
            Ok(Self {
                header_size: 4,
                data_size,
                section_type,
            })
        }
    }

    /// Write a section header into a byte buffer.
    pub fn write_header(section_type: u8, data_size: usize, out: &mut Vec<u8>) {
        if data_size <= 0x00FF_FFFF && data_size > 0 {
            // 4-byte header
            out.push((data_size & 0xFF) as u8);
            out.push(((data_size >> 8) & 0xFF) as u8);
            out.push(((data_size >> 16) & 0xFF) as u8);
            out.push(section_type);
        } else {
            // 8-byte header: [0, 0, 0, type, len: u32 LE]
            out.extend_from_slice(&[0, 0, 0, section_type]);
            let mut len_bytes = [0u8; 4];
            LittleEndian::write_u32(&mut len_bytes, data_size as u32);
            out.extend_from_slice(&len_bytes);
        }
    }
}

/// Information about a chunk in a chunked HAP frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkInfo {
    /// Byte offset of the chunk relative to the start of the frame data payload.
    pub offset: usize,
    /// Byte size of this chunk.
    pub size: usize,
    /// Compressor: 0x0A (uncompressed) or 0x0B (Snappy).
    pub compressor: u8,
}

/// Parsed chunk instructions table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeInstructions {
    pub chunks: Vec<ChunkInfo>,
}

impl DecodeInstructions {
    /// Parse decode instructions from the payload of a `0x01` section.
    pub fn parse(payload: &[u8]) -> Result<Self, HeaderError> {
        let mut offset = 0;
        let mut compressors = Vec::new();
        let mut sizes = Vec::new();
        let mut offsets = Vec::new();

        while offset < payload.len() {
            let hdr = SectionHeader::parse(&payload[offset..])?;
            let sec_start = offset + hdr.header_size;
            let sec_end = sec_start + hdr.data_size;
            if sec_end > payload.len() {
                return Err(HeaderError::Truncated {
                    need: sec_end,
                    have: payload.len(),
                });
            }
            let data = &payload[sec_start..sec_end];

            match hdr.section_type {
                0x02 => {
                    // Compressor table (1 byte per chunk)
                    compressors.extend_from_slice(data);
                }
                0x03 => {
                    // Chunk size table (4 bytes u32 LE per chunk)
                    if data.len() % 4 != 0 {
                        return Err(HeaderError::InvalidDecodeInstructions(
                            "Chunk size table length is not a multiple of 4".into(),
                        ));
                    }
                    for chunk in data.chunks_exact(4) {
                        sizes.push(LittleEndian::read_u32(chunk) as usize);
                    }
                }
                0x04 => {
                    // Chunk offset table (4 bytes u32 LE per chunk)
                    if data.len() % 4 != 0 {
                        return Err(HeaderError::InvalidDecodeInstructions(
                            "Chunk offset table length is not a multiple of 4".into(),
                        ));
                    }
                    for chunk in data.chunks_exact(4) {
                        offsets.push(LittleEndian::read_u32(chunk) as usize);
                    }
                }
                _ => {
                    // Decoders must ignore unknown sub-sections
                }
            }

            offset = sec_end;
        }

        if compressors.len() != sizes.len() {
            return Err(HeaderError::InvalidDecodeInstructions(format!(
                "Compressor count ({}) does not match chunk count ({})",
                compressors.len(),
                sizes.len()
            )));
        }

        let mut chunks = Vec::with_capacity(sizes.len());
        let mut running_offset = 0;

        for (i, (&size, &comp)) in sizes.iter().zip(compressors.iter()).enumerate() {
            let chunk_offset = if i < offsets.len() {
                offsets[i]
            } else {
                running_offset
            };
            chunks.push(ChunkInfo {
                offset: chunk_offset,
                size,
                compressor: comp,
            });
            running_offset += size;
        }

        Ok(Self { chunks })
    }

    /// Build a decode instruction container section payload.
    pub fn build(chunks: &[ChunkInfo]) -> Vec<u8> {
        let mut out = Vec::new();

        // 1. Chunk compressor table (0x02)
        let comp_bytes: Vec<u8> = chunks.iter().map(|c| c.compressor).collect();
        SectionHeader::write_header(0x02, comp_bytes.len(), &mut out);
        out.extend_from_slice(&comp_bytes);

        // 2. Chunk size table (0x03)
        let mut size_bytes = Vec::with_capacity(chunks.len() * 4);
        for c in chunks {
            let mut b = [0u8; 4];
            LittleEndian::write_u32(&mut b, c.size as u32);
            size_bytes.extend_from_slice(&b);
        }
        SectionHeader::write_header(0x03, size_bytes.len(), &mut out);
        out.extend_from_slice(&size_bytes);

        // 3. Chunk offset table (0x04)
        let mut offset_bytes = Vec::with_capacity(chunks.len() * 4);
        for c in chunks {
            let mut b = [0u8; 4];
            LittleEndian::write_u32(&mut b, c.offset as u32);
            offset_bytes.extend_from_slice(&b);
        }
        SectionHeader::write_header(0x04, offset_bytes.len(), &mut out);
        out.extend_from_slice(&offset_bytes);

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_4byte_header_roundtrip() {
        let mut buf = Vec::new();
        SectionHeader::write_header(0xBB, 12345, &mut buf);
        assert_eq!(buf.len(), 4);
        let parsed = SectionHeader::parse(&buf).unwrap();
        assert_eq!(parsed.header_size, 4);
        assert_eq!(parsed.section_type, 0xBB);
        assert_eq!(parsed.data_size, 12345);
    }

    #[test]
    fn test_8byte_header_roundtrip() {
        let mut buf = Vec::new();
        // Size requiring more than 24 bits: > 0x00FF_FFFF (e.g. 20,000,000 bytes)
        let size = 20_000_000;
        SectionHeader::write_header(0xBC, size, &mut buf);
        assert_eq!(buf.len(), 8);
        assert_eq!(buf[0..3], [0, 0, 0]);
        let parsed = SectionHeader::parse(&buf).unwrap();
        assert_eq!(parsed.header_size, 8);
        assert_eq!(parsed.section_type, 0xBC);
        assert_eq!(parsed.data_size, size);
    }

    #[test]
    fn test_decode_instructions_roundtrip() {
        let chunks = vec![
            ChunkInfo {
                offset: 0,
                size: 100,
                compressor: 0x0B,
            },
            ChunkInfo {
                offset: 100,
                size: 150,
                compressor: 0x0B,
            },
            ChunkInfo {
                offset: 250,
                size: 200,
                compressor: 0x0A,
            },
        ];

        let instructions_payload = DecodeInstructions::build(&chunks);
        let parsed = DecodeInstructions::parse(&instructions_payload).unwrap();
        assert_eq!(parsed.chunks, chunks);
    }
}
