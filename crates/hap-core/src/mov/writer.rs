//! QuickTime MOV writer for HAP video streams.

use crate::format::HapFormat;
use crate::mov::atoms::*;
use byteorder::{BigEndian, WriteBytesExt};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MovWriterError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Writer already finalized")]
    AlreadyFinalized,
    #[error("No frames written to movie")]
    NoFramesWritten,
    #[error("Invalid video configuration: {0}")]
    InvalidConfig(String),
}

/// Configuration settings for encoding a HAP video track.
#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub format: HapFormat,
    pub timescale: u32,
}

impl VideoConfig {
    pub fn new(width: u32, height: u32, fps: f32, format: HapFormat) -> Self {
        let timescale = if (fps - 29.97).abs() < 0.01 {
            30000
        } else if (fps - 59.94).abs() < 0.01 {
            60000
        } else if (fps - 23.976).abs() < 0.01 {
            24000
        } else {
            (fps * 1000.0).round() as u32
        };

        Self {
            width,
            height,
            fps,
            format,
            timescale,
        }
    }

    pub fn sample_duration(&self) -> u32 {
        (self.timescale as f32 / self.fps).round() as u32
    }
}

#[derive(Debug, Clone)]
struct SampleInfo {
    offset: u64,
    size: u32,
}

/// Pure Rust QuickTime MOV writer for HAP streams.
pub struct QtHapWriter {
    file: File,
    config: VideoConfig,
    samples: Vec<SampleInfo>,
    mdat_header_offset: u64,
    total_data_bytes: u64,
    finalized: bool,
}

impl QtHapWriter {
    /// Create and initialize a new QuickTime MOV file.
    pub fn create<P: AsRef<Path>>(path: P, config: VideoConfig) -> Result<Self, MovWriterError> {
        if config.width == 0 || config.height == 0 {
            return Err(MovWriterError::InvalidConfig("Dimensions must be > 0".into()));
        }
        if config.fps <= 0.0 {
            return Err(MovWriterError::InvalidConfig("FPS must be > 0".into()));
        }

        let mut file = File::create(path)?;

        // 1. Write ftyp atom
        let mut ftyp = Vec::with_capacity(20);
        ftyp.extend_from_slice(&BRAND_QT);      // Major brand: qt  
        ftyp.extend_from_slice(&0u32.to_be_bytes()); // Minor version
        ftyp.extend_from_slice(&BRAND_QT);      // Compatible brand
        let ftyp_atom = create_atom(ATOM_FTYP, &ftyp);
        file.write_all(&ftyp_atom)?;

        // 2. Start mdat atom with placeholder 64-bit length to support arbitrarily large files
        let mdat_header_offset = file.stream_position()?;
        // Atom header: [size=1: u32 BE, type: mdat, extended_size: u64 BE]
        file.write_u32::<BigEndian>(1)?;
        file.write_all(&ATOM_MDAT)?;
        file.write_u64::<BigEndian>(0)?; // Placeholder for total mdat size

        Ok(Self {
            file,
            config,
            samples: Vec::new(),
            mdat_header_offset,
            total_data_bytes: 0,
            finalized: false,
        })
    }

    /// Write one HAP frame packet to the movie.
    pub fn write_frame(&mut self, frame_bytes: &[u8]) -> Result<(), MovWriterError> {
        if self.finalized {
            return Err(MovWriterError::AlreadyFinalized);
        }

        let offset = self.file.stream_position()?;
        self.file.write_all(frame_bytes)?;
        let size = frame_bytes.len() as u32;

        self.samples.push(SampleInfo { offset, size });
        self.total_data_bytes += frame_bytes.len() as u64;

        Ok(())
    }

    /// Finalize the movie by updating the mdat header and appending the moov metadata atom.
    pub fn finalize(mut self) -> Result<(), MovWriterError> {
        if self.finalized {
            return Err(MovWriterError::AlreadyFinalized);
        }
        if self.samples.is_empty() {
            return Err(MovWriterError::NoFramesWritten);
        }

        // 1. Update mdat extended size (16 bytes header + total data bytes)
        let total_mdat_size = 16 + self.total_data_bytes;
        self.file.seek(SeekFrom::Start(self.mdat_header_offset + 8))?;
        self.file.write_u64::<BigEndian>(total_mdat_size)?;

        // 2. Seek to the end of the file to append moov atom
        self.file.seek(SeekFrom::End(0))?;

        // 3. Build moov atom
        let moov_bytes = self.build_moov_atom()?;
        self.file.write_all(&moov_bytes)?;
        self.file.flush()?;

        self.finalized = true;
        Ok(())
    }

    fn build_moov_atom(&self) -> Result<Vec<u8>, MovWriterError> {
        let sample_duration = self.config.sample_duration();
        let total_duration = (self.samples.len() as u64) * (sample_duration as u64);

        // --- mvhd (Movie Header) ---
        let mut mvhd_payload = Vec::new();
        mvhd_payload.push(0); // version 0
        mvhd_payload.extend_from_slice(&[0, 0, 0]); // flags
        mvhd_payload.write_u32::<BigEndian>(0)?; // creation time
        mvhd_payload.write_u32::<BigEndian>(0)?; // modification time
        mvhd_payload.write_u32::<BigEndian>(self.config.timescale)?;
        mvhd_payload.write_u32::<BigEndian>(total_duration as u32)?;
        mvhd_payload.write_u32::<BigEndian>(0x00010000)?; // normal rate 1.0
        mvhd_payload.write_u16::<BigEndian>(0x0100)?; // volume 1.0
        mvhd_payload.extend_from_slice(&[0u8; 10]); // reserved
        // 36-byte unity display matrix
        mvhd_payload.extend_from_slice(&Self::unity_matrix());
        mvhd_payload.extend_from_slice(&[0u8; 24]); // pre-defined
        mvhd_payload.write_u32::<BigEndian>(2)?; // next track ID
        let mvhd_atom = create_atom(ATOM_MVHD, &mvhd_payload);

        // --- trak -> tkhd (Track Header) ---
        let mut tkhd_payload = Vec::new();
        tkhd_payload.push(0); // version
        tkhd_payload.extend_from_slice(&[0, 0, 0x0F]); // flags: track enabled, in movie, in preview
        tkhd_payload.write_u32::<BigEndian>(0)?; // creation
        tkhd_payload.write_u32::<BigEndian>(0)?; // mod
        tkhd_payload.write_u32::<BigEndian>(1)?; // track id = 1
        tkhd_payload.write_u32::<BigEndian>(0)?; // reserved
        tkhd_payload.write_u32::<BigEndian>(total_duration as u32)?;
        tkhd_payload.extend_from_slice(&[0u8; 8]); // reserved
        tkhd_payload.write_u16::<BigEndian>(0)?; // layer
        tkhd_payload.write_u16::<BigEndian>(0)?; // alternate group
        tkhd_payload.write_u16::<BigEndian>(0)?; // volume
        tkhd_payload.write_u16::<BigEndian>(0)?; // reserved
        tkhd_payload.extend_from_slice(&Self::unity_matrix());
        tkhd_payload.write_u32::<BigEndian>(self.config.width << 16)?; // 16.16 fixed point width
        tkhd_payload.write_u32::<BigEndian>(self.config.height << 16)?; // 16.16 fixed point height
        let tkhd_atom = create_atom(ATOM_TKHD, &tkhd_payload);

        // --- mdia -> mdhd (Media Header) ---
        let mut mdhd_payload = Vec::new();
        mdhd_payload.push(0);
        mdhd_payload.extend_from_slice(&[0, 0, 0]);
        mdhd_payload.write_u32::<BigEndian>(0)?;
        mdhd_payload.write_u32::<BigEndian>(0)?;
        mdhd_payload.write_u32::<BigEndian>(self.config.timescale)?;
        mdhd_payload.write_u32::<BigEndian>(total_duration as u32)?;
        mdhd_payload.write_u16::<BigEndian>(0x55C4)?; // Undefined language
        mdhd_payload.write_u16::<BigEndian>(0)?; // quality
        let mdhd_atom = create_atom(ATOM_MDHD, &mdhd_payload);

        // --- mdia -> hdlr (Handler Reference) ---
        let mut hdlr_payload = Vec::new();
        hdlr_payload.push(0);
        hdlr_payload.extend_from_slice(&[0, 0, 0]);
        hdlr_payload.write_u32::<BigEndian>(0)?; // pre-defined
        hdlr_payload.extend_from_slice(&HANDLER_VIDE); // type: vide
        hdlr_payload.extend_from_slice(&[0u8; 12]); // reserved
        hdlr_payload.extend_from_slice(b"HAP Video Handler\0");
        let hdlr_atom = create_atom(ATOM_HDLR, &hdlr_payload);

        // --- minf -> vmhd (Video Media Header) ---
        let mut vmhd_payload = Vec::new();
        vmhd_payload.push(0);
        vmhd_payload.extend_from_slice(&[0, 0, 1]); // flags: 1
        vmhd_payload.write_u16::<BigEndian>(0)?; // graphics mode
        vmhd_payload.extend_from_slice(&[0u8; 6]); // opcolor
        let vmhd_atom = create_atom(ATOM_VMHD, &vmhd_payload);

        // --- minf -> dinf -> dref (Data Reference) ---
        let mut dref_payload = Vec::new();
        dref_payload.push(0);
        dref_payload.extend_from_slice(&[0, 0, 0]);
        dref_payload.write_u32::<BigEndian>(1)?; // entry count
        // alis / url entry with self-contained flag
        let mut url_entry = Vec::new();
        url_entry.push(0);
        url_entry.extend_from_slice(&[0, 0, 1]); // flag 1 = same file
        let url_atom = create_atom(*b"alis", &url_entry);
        dref_payload.extend_from_slice(&url_atom);
        let dref_atom = create_atom(ATOM_DREF, &dref_payload);
        let dinf_atom = create_atom(ATOM_DINF, &dref_atom);

        // --- minf -> stbl (Sample Table) ---
        let stbl_atom = self.build_stbl_atom(sample_duration)?;

        // Assemble minf
        let mut minf_payload = Vec::new();
        minf_payload.extend_from_slice(&vmhd_atom);
        minf_payload.extend_from_slice(&dinf_atom);
        minf_payload.extend_from_slice(&stbl_atom);
        let minf_atom = create_atom(ATOM_MINF, &minf_payload);

        // Assemble mdia
        let mut mdia_payload = Vec::new();
        mdia_payload.extend_from_slice(&mdhd_atom);
        mdia_payload.extend_from_slice(&hdlr_atom);
        mdia_payload.extend_from_slice(&minf_atom);
        let mdia_atom = create_atom(ATOM_MDIA, &mdia_payload);

        // Assemble trak
        let mut trak_payload = Vec::new();
        trak_payload.extend_from_slice(&tkhd_atom);
        trak_payload.extend_from_slice(&mdia_atom);
        let trak_atom = create_atom(ATOM_TRAK, &trak_payload);

        // Assemble moov
        let mut moov_payload = Vec::new();
        moov_payload.extend_from_slice(&mvhd_atom);
        moov_payload.extend_from_slice(&trak_atom);
        let moov_atom = create_atom(ATOM_MOOV, &moov_payload);

        Ok(moov_atom)
    }

    fn build_stbl_atom(&self, sample_duration: u32) -> Result<Vec<u8>, MovWriterError> {
        // 1. stsd (Sample Description)
        let mut stsd_payload = Vec::new();
        stsd_payload.push(0);
        stsd_payload.extend_from_slice(&[0, 0, 0]);
        stsd_payload.write_u32::<BigEndian>(1)?; // entry count = 1

        // VisualSampleEntry
        let mut entry = Vec::new();
        entry.extend_from_slice(&[0u8; 6]); // reserved
        entry.write_u16::<BigEndian>(1)?; // data reference index = 1
        entry.write_u16::<BigEndian>(0)?; // version
        entry.write_u16::<BigEndian>(0)?; // revision
        entry.write_u32::<BigEndian>(0)?; // vendor
        entry.write_u32::<BigEndian>(0)?; // temporal quality
        entry.write_u32::<BigEndian>(0)?; // spatial quality
        entry.write_u16::<BigEndian>(self.config.width as u16)?;
        entry.write_u16::<BigEndian>(self.config.height as u16)?;
        entry.write_u32::<BigEndian>(0x00480000)?; // 72 dpi horizontal
        entry.write_u32::<BigEndian>(0x00480000)?; // 72 dpi vertical
        entry.write_u32::<BigEndian>(0)?; // data size
        entry.write_u16::<BigEndian>(1)?; // frame count
        // Compressor name: 32 bytes (Pascal string: [len, bytes...])
        let name_bytes = self.config.format.name().as_bytes();
        let name_len = name_bytes.len().min(31);
        let mut comp_name = [0u8; 32];
        comp_name[0] = name_len as u8;
        comp_name[1..1 + name_len].copy_from_slice(&name_bytes[..name_len]);
        entry.extend_from_slice(&comp_name);
        entry.write_u16::<BigEndian>(self.config.format.bits_per_pixel())?; // depth
        entry.write_i16::<BigEndian>(-1)?; // color table id

        let entry_atom = create_atom(self.config.format.fourcc(), &entry);
        stsd_payload.extend_from_slice(&entry_atom);
        let stsd_atom = create_atom(ATOM_STSD, &stsd_payload);

        // 2. stts (Time-to-sample)
        let mut stts_payload = Vec::new();
        stts_payload.push(0);
        stts_payload.extend_from_slice(&[0, 0, 0]);
        stts_payload.write_u32::<BigEndian>(1)?; // 1 entry
        stts_payload.write_u32::<BigEndian>(self.samples.len() as u32)?; // sample count
        stts_payload.write_u32::<BigEndian>(sample_duration)?; // sample duration
        let stts_atom = create_atom(ATOM_STTS, &stts_payload);

        // 3. stsc (Sample-to-chunk)
        let mut stsc_payload = Vec::new();
        stsc_payload.push(0);
        stsc_payload.extend_from_slice(&[0, 0, 0]);
        stsc_payload.write_u32::<BigEndian>(1)?; // 1 entry
        stsc_payload.write_u32::<BigEndian>(1)?; // first chunk = 1
        stsc_payload.write_u32::<BigEndian>(1)?; // samples per chunk = 1
        stsc_payload.write_u32::<BigEndian>(1)?; // sample description index = 1
        let stsc_atom = create_atom(ATOM_STSC, &stsc_payload);

        // 4. stsz (Sample sizes)
        let mut stsz_payload = Vec::new();
        stsz_payload.push(0);
        stsz_payload.extend_from_slice(&[0, 0, 0]);
        stsz_payload.write_u32::<BigEndian>(0)?; // sample size = 0 (variable)
        stsz_payload.write_u32::<BigEndian>(self.samples.len() as u32)?;
        for s in &self.samples {
            stsz_payload.write_u32::<BigEndian>(s.size)?;
        }
        let stsz_atom = create_atom(ATOM_STSZ, &stsz_payload);

        // 5. stco or co64 (Chunk offsets)
        let max_offset = self.samples.last().map(|s| s.offset).unwrap_or(0);
        let offset_atom = if max_offset <= 0xFFFF_FFFF {
            let mut stco_payload = Vec::new();
            stco_payload.push(0);
            stco_payload.extend_from_slice(&[0, 0, 0]);
            stco_payload.write_u32::<BigEndian>(self.samples.len() as u32)?;
            for s in &self.samples {
                stco_payload.write_u32::<BigEndian>(s.offset as u32)?;
            }
            create_atom(ATOM_STCO, &stco_payload)
        } else {
            let mut co64_payload = Vec::new();
            co64_payload.push(0);
            co64_payload.extend_from_slice(&[0, 0, 0]);
            co64_payload.write_u32::<BigEndian>(self.samples.len() as u32)?;
            for s in &self.samples {
                co64_payload.write_u64::<BigEndian>(s.offset)?;
            }
            create_atom(ATOM_CO64, &co64_payload)
        };

        // Assemble stbl
        let mut stbl_payload = Vec::new();
        stbl_payload.extend_from_slice(&stsd_atom);
        stbl_payload.extend_from_slice(&stts_atom);
        stbl_payload.extend_from_slice(&stsc_atom);
        stbl_payload.extend_from_slice(&stsz_atom);
        stbl_payload.extend_from_slice(&offset_atom);
        Ok(create_atom(ATOM_STBL, &stbl_payload))
    }

    fn unity_matrix() -> [u8; 36] {
        let mut m = [0u8; 36];
        // 0x00010000 at index 0, 16, and 0x40000000 at index 32
        m[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
        m[16..20].copy_from_slice(&0x00010000u32.to_be_bytes());
        m[32..36].copy_from_slice(&0x40000000u32.to_be_bytes());
        m
    }
}
