//! Snappy compression and parallel multi-chunk processing for HAP video frames.

use crate::header::ChunkInfo;
use rayon::prelude::*;
use thiserror::Error;

/// Errors that can occur during Snappy compression or decompression.
#[derive(Debug, Error)]
pub enum SnappyError {
    #[error("Snappy decompression error: {0}")]
    Decompress(#[from] snap::Error),
    #[error("Chunk extends past buffer boundaries")]
    OutOfBounds,
    #[error("Destination buffer size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: usize, actual: usize },
}

/// Decompress a single Snappy buffer.
pub fn decompress_snappy(compressed: &[u8]) -> Result<Vec<u8>, SnappyError> {
    let mut decoder = snap::raw::Decoder::new();
    let decompressed = decoder.decompress_vec(compressed)?;
    Ok(decompressed)
}

/// Compress a single buffer with Snappy.
pub fn compress_snappy(raw: &[u8]) -> Result<Vec<u8>, SnappyError> {
    let mut encoder = snap::raw::Encoder::new();
    let compressed = encoder.compress_vec(raw)?;
    Ok(compressed)
}

/// Decompress multiple chunks concurrently using Rayon threads into a single contiguous output buffer.
pub fn decompress_chunks_parallel(
    frame_payload: &[u8],
    chunks: &[ChunkInfo],
) -> Result<Vec<u8>, SnappyError> {
    // Process chunks in parallel
    let decompressed_chunks: Result<Vec<Vec<u8>>, SnappyError> = chunks
        .par_iter()
        .map(|chunk| {
            let start = chunk.offset;
            let end = start + chunk.size;
            if end > frame_payload.len() {
                return Err(SnappyError::OutOfBounds);
            }
            let chunk_data = &frame_payload[start..end];

            match chunk.compressor {
                0x0A => {
                    // Uncompressed raw data
                    Ok(chunk_data.to_vec())
                }
                0x0B => {
                    // Snappy compressed
                    let mut decoder = snap::raw::Decoder::new();
                    decoder.decompress_vec(chunk_data).map_err(SnappyError::from)
                }
                other => {
                    // Fallback to raw copy if unknown compressor
                    log::warn!("Unknown chunk compressor 0x{:02X}, treating as uncompressed", other);
                    Ok(chunk_data.to_vec())
                }
            }
        })
        .collect();

    let decompressed_chunks = decompressed_chunks?;

    // Calculate total size and concatenate
    let total_size: usize = decompressed_chunks.iter().map(|c| c.len()).sum();
    let mut out = Vec::with_capacity(total_size);
    for chunk in decompressed_chunks {
        out.extend_from_slice(&chunk);
    }

    Ok(out)
}

/// Compress raw data by dividing it into N chunks and compressing each with Snappy concurrently.
pub fn compress_chunks_parallel(
    raw_data: &[u8],
    chunk_count: usize,
    use_snappy: bool,
) -> Result<(Vec<Vec<u8>>, Vec<ChunkInfo>), SnappyError> {
    let num_chunks = chunk_count.max(1).min(raw_data.len().max(1));
    let chunk_raw_size = raw_data.len().div_ceil(num_chunks);

    // Split raw data into slices aligned to chunk_raw_size
    let raw_slices: Vec<&[u8]> = raw_data.chunks(chunk_raw_size).collect();

    let compressed_chunks: Result<Vec<Vec<u8>>, SnappyError> = raw_slices
        .par_iter()
        .map(|slice| {
            if use_snappy {
                compress_snappy(slice)
            } else {
                Ok(slice.to_vec())
            }
        })
        .collect();

    let compressed_chunks = compressed_chunks?;

    let mut chunk_infos = Vec::with_capacity(compressed_chunks.len());
    let mut current_offset = 0;
    let compressor_byte = if use_snappy { 0x0B } else { 0x0A };

    for chunk in &compressed_chunks {
        chunk_infos.push(ChunkInfo {
            offset: current_offset,
            size: chunk.len(),
            compressor: compressor_byte,
        });
        current_offset += chunk.len();
    }

    Ok((compressed_chunks, chunk_infos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snappy_single_roundtrip() {
        let original = b"The quick brown fox jumps over the lazy dog. 1234567890! Repeating pattern! Repeating pattern!";
        let compressed = compress_snappy(original).unwrap();
        assert!(!compressed.is_empty());
        let decompressed = decompress_snappy(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_snappy_chunked_parallel_roundtrip() {
        // Create a buffer of 64KB with patterned data
        let mut original = Vec::with_capacity(65536);
        for i in 0..65536 {
            original.push((i % 256) as u8);
        }

        let (chunks, infos) = compress_chunks_parallel(&original, 4, true).unwrap();
        assert_eq!(chunks.len(), 4);
        assert_eq!(infos.len(), 4);

        // Concatenate chunks to simulate contiguous frame payload
        let mut frame_payload = Vec::new();
        for chunk in &chunks {
            frame_payload.extend_from_slice(chunk);
        }

        let decompressed = decompress_chunks_parallel(&frame_payload, &infos).unwrap();
        assert_eq!(decompressed.len(), original.len());
        assert_eq!(decompressed, original);
    }
}
