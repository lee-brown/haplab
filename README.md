# hap-rs / HapLab

[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/License-PolyForm_Noncommercial_1.0.0-crimson.svg)](https://polyformproject.org/licenses/noncommercial/1.0.0)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)

A native Rust implementation of the [HAP Video Codec specification](https://github.com/vidvox/hap).

Built for real-time video playback, media servers, transcode pipelines, and interactive installations. Provides decoder, encoder, hardware texture streaming, and QuickTime container demuxing/muxing in pure, safe Rust.

---

## Features

- **Native Rust Implementation**: Complete decoder and encoder pipeline implemented in pure Rust.
- **Flavour Support**:
  - **Hap 1 (`Hap1`)**: DXT1 / BC1 (RGB).
  - **Hap Alpha (`Hap5`)**: DXT5 / BC3 (RGBA).
  - **Hap Q (`HapY`)**: Scaled YCoCg-DXT5.
  - **Hap Q Alpha (`HapM`)**: Dual-texture container combining Hap Q (YCoCg) with Hap Alpha-Only (BC4 alpha).
  - **Hap Alpha-Only (`HapA`)**: BC4 / RGTC1 uncompressed single-channel alpha.
  - **Hap R (`Hap7` / `HapR`)**: BC7 UNORM texture compression (Mode 6 endpoint covariance and PCA partitioning).
  - **Hap HDR (`HapH`)**: BC6H floating-point HDR decompression.
- **GPU Compute & Upload**:
  - WGSL compute shaders for BC1, BC3, BC4, BC7, and YCoCg compression via `wgpu`.
  - Direct VRAM texture upload (`Bc1RgbaUnorm`, `Bc3RgbaUnorm`, `Bc7RgbaUnorm`) for hardware texture decompression.
- **Multithreading**: Rayon parallel multi-chunk Snappy compression and decompression.
- **Container Support**: Native QuickTime `.mov` demuxer and streaming muxer (`ftyp`, `moov`, `mdat`, `stsd`, `stts`, `stsc`, `stsz`, `stco`, `co64`).
- **CLI & GUI**: Dual-mode binary providing command-line automation and an interactive desktop application (`egui`/`eframe`).

---

## Codec Flavour Matrix

| Flavour | FourCC | Texture Format | Compression | Alpha | Notes |
|---|---|---|---|---|---|
| **Hap 1** | `Hap1` | DXT1 (BC1) | Snappy (optional) | No | Standard RGB, lowest CPU overhead |
| **Hap Alpha** | `Hap5` | DXT5 (BC3) | Snappy (optional) | Yes | RGBA transparent video |
| **Hap Q** | `HapY` | Scaled YCoCg-DXT5 | Snappy (optional) | No | High-fidelity luma/chroma |
| **Hap Q Alpha** | `HapM` | Scaled YCoCg + BC4 | Snappy (optional) | Yes | Independent 8-bit alpha matte |
| **Hap Alpha-Only** | `HapA` | BC4 (RGTC1) | Snappy (optional) | Alpha Only | Single-channel matte sequences |
| **Hap R** | `Hap7` / `HapR` | BC7 UNORM | Snappy (optional) | Yes | High-fidelity graphics and alpha |
| **Hap HDR** | `HapH` | BC6H Float | Snappy (optional) | No | Half-float RGB video |

---

## Workspace Layout

```
hap/
├── crates/
│   ├── hap-core/     # Core codecs, Snappy, YCoCg, BC1-BC7, QuickTime container
│   ├── hap-gpu/      # wgpu compute shaders, GPU encoder, VRAM texture bindings
│   └── hap-app/      # Standalone binary: CLI and desktop GUI
├── Cargo.toml        # Workspace definition
└── LICENSE-MIT       # MIT License
```

---

## CLI Usage

The `hap` binary automatically operates in CLI mode when arguments are passed:

### Inspect a HAP MOV file
```bash
hap info -i presentation.mov
```

### Encode an Image Sequence
```bash
# Encode PNG/JPEG/TIFF sequence to Hap Q with 4 chunks and Snappy
hap encode -i ./renders/ -o output_hapy.mov --format hap-y --fps 60 --chunks 4 --snappy

# Encode to Hap R (BC7)
hap encode -i ./renders/ -o output_hapr.mov --format hapr --fps 30 --chunks 8 --snappy

# Encode transparent video (Hap Q Alpha)
hap encode -i ./transparent_frames/ -o output_hapm.mov --format hap-m --fps 30 --snappy
```

### Decode a HAP MOV file to PNG Sequence
```bash
hap decode -i output_hapy.mov -o ./decoded_frames/
```

---

## Desktop GUI

Running `hap` without arguments launches the graphical interface:

- **Player & Inspector**:
  - Transport controls and keyboard shortcuts: `Space` (Play/Pause), `Left`/`Right` (step 1 frame), `Shift + Left`/`Right` (step 10 frames), `Home`/`End` (jump to start/end), `L` (toggle loop).
  - SMPTE timecode display (`HH:MM:SS:FF`) and frame percentage.
  - Background options (Checkerboard, Dark, Light) and channel modes (RGBA, Alpha Matte, RGB).
  - Technical stream inspector with resolution, duration, bitrate, and container metadata.
  - Integrated PNG/JPEG/TIFF frame sequence exporter.
- **Encoder**:
  - Image sequence auto-detection and first-frame thumbnail preview.
  - Workflow presets: Hap Q, Hap R, Hap Q Alpha, Hap 1, Hap Alpha, and Custom.
  - Auto-suggested output destination paths.
  - Live encoding telemetry (FPS, elapsed time, ETA).
  - Quick open in player upon completion.
- **Diagnostics**:
  - GPU adapter details, driver backend API, and BC texture support status.
  - Thread pool configuration and searchable runtime activity logs.

---

## Rust Library Usage

Add `hap-core` or `hap-gpu` to your `Cargo.toml`:

```toml
[dependencies]
hap-core = "0.1.0"
hap-gpu = "0.1.0" # Optional: for wgpu acceleration
```

### Decompressing a HAP Frame
```rust
use hap_core::{decode_frame_to_rgba, HapFormat};

let compressed_frame: &[u8] = ...;
let width = 1920;
let height = 1080;
let format = HapFormat::HapY;

let rgba_pixels: Vec<u8> = decode_frame_to_rgba(compressed_frame, width, height, format)?;
```

### Reading a HAP QuickTime Video
```rust
use hap_core::QtHapReader;
use std::fs::File;

let file = File::open("input.mov")?;
let mut reader = QtHapReader::open(file)?;

println!("Format: {:?}, Size: {}x{}", reader.format(), reader.width(), reader.height());

for frame_idx in 0..reader.frame_count() {
    let packet = reader.read_frame_packet(frame_idx)?;
    let rgba = reader.decode_frame(frame_idx)?;
}
```

### Writing a HAP QuickTime Video
```rust
use hap_core::{QtHapWriter, VideoConfig, HapFormat, encode_frame};
use std::fs::File;

let config = VideoConfig {
    width: 1920,
    height: 1080,
    fps: 60.0,
    format: HapFormat::HapR,
    chunks: 4,
    snappy: true,
};

let file = File::create("output.mov")?;
let mut writer = QtHapWriter::create(file, config.clone())?;

for frame_rgba in frame_sequence {
    let compressed = encode_frame(&frame_rgba, config.width, config.height, config.format, config.chunks, config.snappy)?;
    writer.write_frame(&compressed)?;
}

writer.finish()?;
```

---

## Building from Source

```bash
# Clone the repository
git clone https://github.com/your-repo/hap.git
cd hap

# Run test suite
cargo test --workspace

# Build release executable
cargo build --release -p hap-app
```

The compiled binary will be located at `target/release/haplab.exe` (Windows) or `target/release/haplab` (Linux/macOS).

---

## License

This project is licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE.md).

- **Non-Commercial & Personal Use**: You are free to view, compile, run, modify, and distribute this software for personal projects, learning, testing, and academic research.
- **Commercial Use & Cloud Services**: Commercial use—including hosting or operating this software as a software-as-a-service (SaaS), cloud service, or API (e.g. on AWS, GCP, Azure), or embedding it into commercial broadcast/media products—is strictly prohibited without a separate commercial license from the author.
- **Commercial Licensing**: For commercial licensing inquiries, enterprise deployment, or cloud API authorization, contact **Lee Brown**.

