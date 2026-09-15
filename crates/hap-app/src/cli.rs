//! Command-line interface for headless HAP video encoding, decoding, and inspection.

use clap::{Args, Parser, Subcommand, ValueEnum};
use hap_core::{
    decode_frame_to_rgba, encode_frame_with_options, AlphaMode, ColorRange, DitherMode,
    EncodeOptions, HapFormat, QualityPreset, QtHapReader, QtHapWriter, VideoConfig,
};
use hap_gpu::GpuCompressor;
use image::GenericImageView;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name = "hap")]
#[command(author = "Lee Brown")]
#[command(version = "0.1.0")]
#[command(about = "Pure Rust HAP video encoder, decoder, and visual inspector", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Encode an image sequence to a HAP QuickTime MOV file
    Encode(EncodeArgs),
    /// Decode frames from a HAP QuickTime MOV file to an image sequence
    Decode(DecodeArgs),
    /// Inspect metadata and track structure of a HAP MOV file
    Info(InfoArgs),
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliHapFormat {
    Hap1,
    Hap5,
    HapY,
    HapM,
    HapA,
    Hap7,
    HapR,
}

impl From<CliHapFormat> for HapFormat {
    fn from(c: CliHapFormat) -> Self {
        match c {
            CliHapFormat::Hap1 => HapFormat::Hap1,
            CliHapFormat::Hap5 => HapFormat::Hap5,
            CliHapFormat::HapY => HapFormat::HapY,
            CliHapFormat::HapM => HapFormat::HapM,
            CliHapFormat::HapA => HapFormat::HapA,
            CliHapFormat::Hap7 | CliHapFormat::HapR => HapFormat::Hap7,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliColorRange {
    /// Full PC / Graphics levels 0..=255
    Full,
    /// Limited / Studio broadcast levels 16..=235 (expands to 0..=255)
    Limited,
}

impl From<CliColorRange> for ColorRange {
    fn from(c: CliColorRange) -> Self {
        match c {
            CliColorRange::Full => ColorRange::Full,
            CliColorRange::Limited => ColorRange::Limited,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliAlphaMode {
    /// Straight (unassociated) alpha
    Straight,
    /// Premultiply alpha: RGB = (RGB * Alpha) / 255
    Premultiply,
    /// Demultiply (un-premultiply) alpha: RGB = (RGB * 255) / Alpha
    Demultiply,
    /// Discard alpha channel (force opaque 255)
    Discard,
}

impl From<CliAlphaMode> for AlphaMode {
    fn from(a: CliAlphaMode) -> Self {
        match a {
            CliAlphaMode::Straight => AlphaMode::Straight,
            CliAlphaMode::Premultiply => AlphaMode::Premultiply,
            CliAlphaMode::Demultiply => AlphaMode::Demultiply,
            CliAlphaMode::Discard => AlphaMode::Discard,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliDitherMode {
    /// No dithering
    None,
    /// Bayer 4x4 spatial ordered dithering (reduces banding on LED walls)
    Bayer,
}

impl From<CliDitherMode> for DitherMode {
    fn from(d: CliDitherMode) -> Self {
        match d {
            CliDitherMode::None => DitherMode::None,
            CliDitherMode::Bayer => DitherMode::Bayer4x4,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliQualityPreset {
    /// Draft / Fast (RangeFit - ~3x faster encode)
    Draft,
    /// Production (ClusterFit - optimal endpoint clustering)
    Production,
}

impl From<CliQualityPreset> for QualityPreset {
    fn from(q: CliQualityPreset) -> Self {
        match q {
            CliQualityPreset::Draft => QualityPreset::Draft,
            CliQualityPreset::Production => QualityPreset::Production,
        }
    }
}

#[derive(Args, Debug)]
pub struct EncodeArgs {
    /// Path to directory containing image sequence, or file pattern
    #[arg(short, long)]
    pub input: PathBuf,

    /// Output .mov destination path
    #[arg(short, long)]
    pub output: PathBuf,

    /// HAP codec flavour: hap1, hap5, hapy, hapm, hapa, hap7 / hapr
    #[arg(short, long, value_enum, default_value_t = CliHapFormat::HapY)]
    pub format: CliHapFormat,

    /// Frame rate in frames per second
    #[arg(long, default_value_t = 30.0)]
    pub fps: f32,

    /// Number of parallel chunks per frame for multi-threaded decoding
    #[arg(short, long, default_value_t = 4)]
    pub chunks: usize,

    /// Apply Snappy second-stage compression
    #[arg(long, default_value_t = true)]
    pub snappy: bool,

    /// Input color range: full (0-255), limited (16-235 video levels)
    #[arg(long, value_enum, default_value_t = CliColorRange::Full)]
    pub color_range: CliColorRange,

    /// Alpha channel handling: straight, premultiply, demultiply, discard
    #[arg(long, value_enum, default_value_t = CliAlphaMode::Straight)]
    pub alpha_mode: CliAlphaMode,

    /// Chroma dithering mode to eliminate color banding: none, bayer
    #[arg(long, value_enum, default_value_t = CliDitherMode::None)]
    pub dither: CliDitherMode,

    /// Encoding quality vs speed: draft (ultra-fast RangeFit), production (ClusterFit)
    #[arg(long, value_enum, default_value_t = CliQualityPreset::Production)]
    pub quality: CliQualityPreset,

    /// Enable GPU hardware acceleration via wgpu
    #[arg(long, default_value_t = false)]
    pub gpu: bool,
}

#[derive(Args, Debug)]
pub struct DecodeArgs {
    /// Input HAP .mov file
    #[arg(short, long)]
    pub input: PathBuf,

    /// Output directory for extracted frames
    #[arg(short, long)]
    pub output: PathBuf,

    /// Extracted image format: png, jpg, tiff, bmp
    #[arg(long, default_value = "png")]
    pub format: String,

    /// First frame index (0-based, inclusive)
    #[arg(long)]
    pub start: Option<usize>,

    /// Last frame index (0-based, inclusive)
    #[arg(long)]
    pub end: Option<usize>,
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// Input HAP .mov file to inspect
    #[arg(short, long)]
    pub input: PathBuf,
}

pub fn run_cli(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Encode(args) => run_encode(args),
        Commands::Decode(args) => run_decode(args),
        Commands::Info(args) => run_info(args),
    }
}

fn collect_image_files(input_path: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();

    if input_path.is_dir() {
        for entry in fs::read_dir(input_path)? {
            let entry = entry?;
            let p = entry.path();
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if matches!(ext_lower.as_str(), "png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" | "webp" | "tga") {
                    files.push(p);
                }
            }
        }
        files.sort();
    } else if input_path.is_file() {
        files.push(input_path.to_path_buf());
    } else {
        return Err(format!("Input path not found: {:?}", input_path).into());
    }

    if files.is_empty() {
        return Err(format!("No supported images found in {:?}", input_path).into());
    }

    Ok(files)
}

fn run_encode(args: EncodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    if args.input.is_file() && crate::worker::is_video_container(&args.input) {
        let probe = crate::worker::probe_video_input(&args.input)?;
        println!(
            "Input video detected [{}]: {}x{} @ {:.2} fps, ~{} frames ({:.2}s)",
            probe.codec, probe.width, probe.height, probe.fps, probe.frame_count, probe.duration_secs
        );
        let width = probe.width;
        let height = probe.height;
        if width % 4 != 0 || height % 4 != 0 {
            return Err(format!("Video dimensions ({}x{}) must be multiples of 4 for HAP encoding", width, height).into());
        }

        let fps = if args.fps > 0.0 { args.fps } else { probe.fps };
        let hap_format: HapFormat = args.format.into();
        let video_cfg = VideoConfig::new(width as u32, height as u32, fps, hap_format);
        let mut writer = QtHapWriter::create(&args.output, video_cfg)?;

        #[cfg(windows)]
        use std::os::windows::process::CommandExt;
        #[cfg(windows)]
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let ffmpeg_bin = crate::worker::find_ffmpeg_binary();
        let mut cmd = std::process::Command::new(&ffmpeg_bin);
        cmd.args(&[
            "-nostdin", "-v", "error",
            "-hwaccel", "auto",
            "-threads", "0",
            "-i",
        ])
            .arg(&args.input)
            .args(&["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let mut child = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn ffmpeg ({}): {}", ffmpeg_bin.display(), e))?;
        let mut stdout = child.stdout.take().ok_or("Failed to capture ffmpeg stdout pipe")?;

        let frame_bytes = width * height * 4;
        let mut raw = vec![0u8; frame_bytes];
        let mut current_frame = 0usize;
        let start_time = Instant::now();

        use std::io::Read;
        let encode_opts = EncodeOptions {
            format: hap_format,
            chunk_count: args.chunks,
            use_snappy: args.snappy,
            color_range: args.color_range.into(),
            alpha_mode: args.alpha_mode.into(),
            dither_mode: args.dither.into(),
            quality: args.quality.into(),
        };

        loop {
            match stdout.read_exact(&mut raw) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    let _ = child.kill();
                    return Err(format!("Pipe read error: {}", e).into());
                }
            }

            let packet = hap_core::encode_frame_with_options(&raw, width, height, &encode_opts)?;
            writer.write_frame(&packet)?;
            current_frame += 1;
            if current_frame % 30 == 0 || current_frame == probe.frame_count {
                let elapsed = start_time.elapsed().as_secs_f32().max(0.001);
                let fps_rate = current_frame as f32 / elapsed;
                print!("\rEncoded frame {}/{} ({:.1} FPS)...", current_frame, probe.frame_count, fps_rate);
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        }
        let _ = child.wait();
        writer.finalize()?;
        let total_secs = start_time.elapsed().as_secs_f32();
        println!(
            "\nSuccessfully encoded {} frames to {:?} ({:.2}s, {:.1} FPS avg)",
            current_frame, args.output, total_secs, current_frame as f32 / total_secs.max(0.001)
        );
        return Ok(());
    }

    let image_files = collect_image_files(&args.input)?;
    let total_frames = image_files.len();
    println!("Found {} frames to encode.", total_frames);

    let first_img = image::open(&image_files[0])?;
    let (width, height) = first_img.dimensions();

    if width % 4 != 0 || height % 4 != 0 {
        return Err(format!("Image dimensions ({}x{}) must be multiples of 4", width, height).into());
    }

    let hap_format: HapFormat = args.format.into();
    let encode_opts = EncodeOptions {
        format: hap_format,
        chunk_count: args.chunks,
        use_snappy: args.snappy,
        color_range: args.color_range.into(),
        alpha_mode: args.alpha_mode.into(),
        dither_mode: args.dither.into(),
        quality: args.quality.into(),
    };

    println!(
        "Encoding settings: Format={}, Res={}x{}, FPS={:.2}, Chunks={}, Snappy={}, Range={:?}, Alpha={:?}, Dither={:?}, Quality={:?}, GPU={}",
        hap_format.name(),
        width,
        height,
        args.fps,
        args.chunks,
        args.snappy,
        args.color_range,
        args.alpha_mode,
        args.dither,
        args.quality,
        args.gpu
    );

    let config = VideoConfig::new(width, height, args.fps, hap_format);
    let mut writer = QtHapWriter::create(&args.output, config)?;

    // Setup GPU compressor if requested
    let mut gpu_compressor = if args.gpu {
        let instance = wgpu::Instance::default();
        let adapter_opt: Option<wgpu::Adapter> = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })).ok();

        if let Some(adapter) = adapter_opt {
            let dq: Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError> =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()));
            if let Ok((device, queue)) = dq {
                let dev_arc = Arc::new(device);
                let q_arc = Arc::new(queue);
                GpuCompressor::new(dev_arc, q_arc, width, height).ok()
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    if args.gpu && gpu_compressor.is_none() {
        println!("GPU initialization failed or unsupported, falling back to CPU encoding.");
    }

    let start_time = Instant::now();
    for (i, path) in image_files.iter().enumerate() {
        let img = image::open(path)?.to_rgba8();
        let raw_rgba = img.into_raw();

        let packet = if let Some(ref mut gpu) = gpu_compressor {
            // Compress blocks on GPU then pack
            if let Ok(bc_blocks) = gpu.compress(&raw_rgba, hap_format) {
                // Multi-chunk or single chunk Snappy
                if args.chunks > 1 {
                    let (chunks, infos) = hap_core::snappy::compress_chunks_parallel(&bc_blocks, args.chunks, args.snappy)?;
                    let instr = hap_core::DecodeInstructions::build(&infos);
                    let mut instr_sec = Vec::new();
                    hap_core::SectionHeader::write_header(0x01, instr.len(), &mut instr_sec);
                    instr_sec.extend_from_slice(&instr);
                    let chunks_size: usize = chunks.iter().map(|c| c.len()).sum();
                    let total_size = instr_sec.len() + chunks_size;
                    let mut pkt = Vec::with_capacity(total_size + 8);
                    hap_core::SectionHeader::write_header(hap_format.chunked_type_byte(), total_size, &mut pkt);
                    pkt.extend_from_slice(&instr_sec);
                    for c in chunks {
                        pkt.extend_from_slice(&c);
                    }
                    pkt
                } else if args.snappy {
                    let compressed = hap_core::snappy::compress_snappy(&bc_blocks)?;
                    let mut pkt = Vec::with_capacity(compressed.len() + 8);
                    hap_core::SectionHeader::write_header(hap_format.snappy_type_byte(), compressed.len(), &mut pkt);
                    pkt.extend_from_slice(&compressed);
                    pkt
                } else {
                    let mut pkt = Vec::with_capacity(bc_blocks.len() + 8);
                    hap_core::SectionHeader::write_header(hap_format.uncompressed_type_byte(), bc_blocks.len(), &mut pkt);
                    pkt.extend_from_slice(&bc_blocks);
                    pkt
                }
            } else {
                encode_frame_with_options(&raw_rgba, width as usize, height as usize, &encode_opts)?
            }
        } else {
            encode_frame_with_options(&raw_rgba, width as usize, height as usize, &encode_opts)?
        };

        writer.write_frame(&packet)?;

        if (i + 1) % 10 == 0 || i + 1 == total_frames {
            let elapsed = start_time.elapsed().as_secs_f32();
            let current_fps = (i + 1) as f32 / elapsed.max(0.001);
            print!("\rEncoded [{}/{}] frames ({:.1} fps)", i + 1, total_frames, current_fps);
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    }

    println!("\nFinalizing QuickTime container...");
    writer.finalize()?;
    let total_elapsed = start_time.elapsed();
    println!("Encode complete: {:?} ({:.2}s)", args.output, total_elapsed.as_secs_f32());
    Ok(())
}

fn run_decode(args: DecodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut reader = QtHapReader::open(&args.input)?;
    let total_frames = reader.frame_count();
    let width = reader.width() as usize;
    let height = reader.height() as usize;

    let start = args.start.unwrap_or(0).min(total_frames.saturating_sub(1));
    let end = args.end.unwrap_or(total_frames.saturating_sub(1)).min(total_frames.saturating_sub(1));

    fs::create_dir_all(&args.output)?;
    println!(
        "Decoding {} ({} frames: {}..{}) to {:?}",
        args.input.display(),
        end - start + 1,
        start,
        end,
        args.output
    );

    let start_time = Instant::now();
    for idx in start..=end {
        let packet = reader.read_frame_packet(idx)?;
        let rgba = decode_frame_to_rgba(&packet, width, height)?;

        let filename = format!("frame_{:06}.{}", idx, args.format);
        let out_path = args.output.join(filename);

        if let Some(img) = image::RgbaImage::from_raw(width as u32, height as u32, rgba) {
            img.save(&out_path)?;
        }

        if (idx - start + 1) % 10 == 0 || idx == end {
            let elapsed = start_time.elapsed().as_secs_f32();
            let fps = (idx - start + 1) as f32 / elapsed.max(0.001);
            print!("\rDecoded [{}/{}] frames ({:.1} fps)", idx - start + 1, end - start + 1, fps);
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    }

    println!("\nDecoding finished successfully.");
    Ok(())
}

fn run_info(args: InfoArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut reader = QtHapReader::open(&args.input)?;
    let metadata = fs::metadata(&args.input)?;
    let file_size_mb = metadata.len() as f64 / (1024.0 * 1024.0);

    println!("==================================================");
    println!(" HAP Video Stream Inspection");
    println!("==================================================");
    println!("File:         {}", args.input.display());
    println!("File Size:    {:.2} MB", file_size_mb);
    println!("Resolution:   {} x {}", reader.width(), reader.height());
    println!("Codec FourCC: {}", String::from_utf8_lossy(&reader.format().fourcc()));
    println!("HAP Flavour:  {}", reader.format().name());
    println!("Has Alpha:    {}", if reader.format().has_alpha() { "Yes" } else { "No" });
    println!("Frame Rate:   {:.2} fps", reader.fps());
    println!("Frame Count:  {}", reader.frame_count());
    println!("Duration:     {:.2} seconds", reader.duration());

    if reader.duration() > 0.0 {
        let bitrate_mbps = (metadata.len() as f64 * 8.0) / (reader.duration() * 1_000_000.0);
        println!("Avg Bitrate:  {:.2} Mbps", bitrate_mbps);
    }

    if let Ok(first_pkt) = reader.read_frame_packet(0) {
        if let Ok(hdr) = hap_core::SectionHeader::parse(&first_pkt) {
            println!("Frame 0 Size: {} bytes (Header: {} bytes, Type: 0x{:02X})", first_pkt.len(), hdr.header_size, hdr.section_type);
        }
    }
    println!("==================================================");

    Ok(())
}
