# hap-rs: Pure Rust HAP Video Codec & Suite

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![Pure Rust](https://img.shields.io/badge/Pure%20Rust-Zero%20FFmpeg-brightgreen.svg)](#features)
[![GPU Accelerated](https://img.shields.io/badge/GPU-wgpu%20compute-blue.svg)](#gpu-acceleration)

A high-performance, 100% native Rust implementation of the complete [HAP Video Codec specification](https://github.com/vidvox/hap).

Designed for ultra-low-latency real-time video playback, live media servers, VJ software, interactive installations, and multi-display stage productions. **`hap-rs` is completely free of FFmpeg, `libav*`, and external C dependencies**, providing an uncompromised, cross-platform, pure-Rust ecosystem under a permissive **MIT License**.

---

## Features

- 🦀 **100% Pure Native Rust**: Zero LGPL/GPL C libraries, zero FFmpeg bindings, zero external video framework baggage.
- ⚡ **Full Flavour Support**: Includes standard HAP formats as well as modern, rare, and ultra-high-fidelity variants:
  - **Hap 1 (`Hap1`)**: DXT1 / BC1 (RGB 24-bit).
  - **Hap Alpha (`Hap5`)**: DXT5 / BC3 (RGBA 32-bit).
  - **Hap Q (`HapY`)**: Scaled YCoCg-DXT5 (high-fidelity luma/chroma).
  - **Hap Q Alpha (`HapM`)**: Dual-texture container combining Hap Q (YCoCg) with Hap Alpha-Only (BC4 alpha).
  - **Hap Alpha-Only (`HapA`)**: BC4 / RGTC1 uncompressed single-channel alpha.
  - **Hap R (`Hap7` / `HapR`)**: State-of-the-art BC7 texture compression with Mode 6 endpoint covariance and PCA partitioning.
  - **Hap HDR (`HapH`)**: BC6H floating-point HDR video decompression.
- 🚀 **GPU Acceleration via `wgpu`**:
  - Native WGSL compute shaders for BC1, BC3, BC4, BC7, and YCoCg transforms directly on the GPU.
  - Zero-copy VRAM direct texture uploads (`Bc1RgbaUnorm`, `Bc3RgbaUnorm`, `Bc7RgbaUnorm`) allowing frame decompression directly inside GPU hardware memory.
- 🧵 **Multi-Threaded Snappy & Chunking**: Full support for parallel multi-chunk frame compression and decompression via Rayon, delivering blazing decode speeds (>1000 FPS).
- 📦 **Pure Rust QuickTime `.mov` Demuxer & Muxer**: Custom container reader and streaming writer (`ftyp`, `moov`, `mdat`, `stsd`, `stts`, `stsc`, `stsz`, `stco`, `co64`).
- 🖥️ **Dual CLI & Desktop GUI**: A single binary that launches an interactive `egui` desktop video player and converter when run graphically, or functions as a fast headless CLI when given arguments.

---

## Codec Flavour Matrix

| Flavour | FourCC | Texture Format | Compression | Alpha Support | Typical Use Case |
|---|---|---|---|---|---|
| **Hap 1** | `Hap1` | DXT1 (BC1) | Snappy (optional) | No (1-bit punchthrough) | High performance standard RGB |
| **Hap Alpha** | `Hap5` | DXT5 (BC3) | Snappy (optional) | Yes (interpolated 8-bit) | Overlays, transparent graphics |
| **Hap Q** | `HapY` | Scaled YCoCg-DXT5 | Snappy (optional) | No | High-fidelity photorealistic video |
| **Hap Q Alpha** | `HapM` | Scaled YCoCg + BC4 | Snappy (optional) | Yes (independent 8-bit channel) | Studio quality video with alpha matte |
| **Hap Alpha-Only** | `HapA` | BC4 (RGTC1) | Snappy (optional) | Alpha Only | Matte sequences, masks |
| **Hap R** | `Hap7` / `HapR` | BC7 UNORM | Snappy (optional) | Yes (integrated) | Modern graphics, ultra-high quality |
| **Hap HDR** | `HapH` | BC6H Float | Snappy (optional) | No (half-float RGB) | High dynamic range, lighting plates |

---

## Workspace Structure

```
hap/
├── crates/
│   ├── hap-core/     # Core codecs, Snappy, YCoCg, BC1-BC7, QuickTime container
│   ├── hap-gpu/      # wgpu compute shaders, GPU encoder, zero-copy VRAM textures
│   └── hap-app/      # Standalone binary: dual CLI & egui desktop GUI
├── Cargo.toml        # Workspace definition & release optimization profiles
└── LICENSE-MIT       # Permissive MIT open-source license
```

---

## Quick Start (CLI)

The compiled binary `hap.exe` automatically detects CLI arguments:

### 1. Inspect a HAP MOV file
```bash
hap info -i presentation.mov
```
*Outputs detailed stream metadata including FourCC, format flavour, resolution, FPS, total frames, bit rate, and chunk headers.*

### 2. Encode an Image Sequence
```bash
# Encode PNG/JPEG/TIFF sequence to Hap Q with 4 parallel chunks and Snappy
hap encode -i ./renders/ -o output_hapy.mov --format hap-y --fps 60 --chunks 4 --snappy

# Encode with modern BC7 Hap R
hap encode -i ./renders/ -o output_hapr.mov --format hapr --fps 30 --chunks 8 --snappy

# Encode studio quality with Alpha (Hap Q Alpha)
hap encode -i ./transparent_frames/ -o output_hapm.mov --format hap-m --fps 30 --snappy
```

### 3. Decode a HAP MOV file to PNG Sequence
```bash
hap decode -i output_hapy.mov -o ./decoded_frames/
```

---

## Desktop GUI (HAP Video Studio)

When invoked without subcommands (or double-clicked in Windows Explorer / macOS Finder), `hap` launches the hardware-accelerated **HAP Video Studio**:

- 🎨 **Modern Dark Studio Theme**: Custom obsidian-slate palette with smooth corner radiuses, high contrast typography, and non-intrusive toast notifications.
- 🎬 **Hero Drop Zone**: Seamless drag-and-drop with active glowing window border feedback. Drag a `.mov` to instantly play & inspect, or drag an image sequence folder to configure encoding.
- 🎛️ **Pro Media Player & Timeline Scrubber**:
  - Full keyboard shortcuts: `Space` (Play/Pause), `←`/`→` (±1 Frame), `Shift + ←`/`→` (±10 Frames), `Home`/`End` (Jump to Start/End), `L` (Toggle loop mode).
  - Exact SMPTE timecode display (`HH:MM:SS:FF`) and frame percentage metrics.
  - **Transparency Checkerboard**: Renders an underlying alpha checkerboard for transparent videos (`Hap Alpha`, `Hap R`, `Hap Q Alpha`).
  - **Channel Inspector**: Switch between `🎨 Full RGBA`, `👁 Isolated Alpha Matte (B&W)`, and `🌈 RGB Only`.
- 📊 **Stream Technical Inspector**:
  - Live inspection card showing FourCC chip, native resolution & aspect ratio, total frames, duration, container type, bitrate, and file size on disk.
  - **1-Click "Copy Media Info"** and native **"Reveal in Explorer"** integration.
  - Integrated PNG/JPEG/TIFF sequence exporter.
- 🚀 **Smart Video Encoder with 1-Click Presets**:
  - Presets: `🌟 Hap Q (Production)`, `💎 Hap R (BC7 Ultra)`, `🎭 Hap Q Alpha (Broadcast)`, `⚡ Hap 1 (Fastest)`, `🪶 Hap Alpha (DXT5)`, and `⚙️ Custom`.
  - **Sequence Auto-Detection & Thumbnail**: Automatically detects image range, dimensions, total frame count, and loads an instant visual thumbnail preview.
  - **Auto-Naming**: Intelligently names output files based on sequence folder and selected preset (e.g. `MyAnim_HapQ.mov`).
  - **Real-Time Telemetry**: Live progress bar with speed (FPS), elapsed time, and ETA calculations.
  - **1-Click Post-Encode Pipeline**: Prominent `▶ Load into Player & Inspect` button to verify encodes in the player immediately.
- 🛠 **Hardware & Diagnostics Dashboard**: Real-time GPU adapter inspection, driver backend, BC texture capability detection, and searchable event log terminal with clipboard export.

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
let format = HapFormat::HapY; // Or detect from container header

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
    let packet = reader.read_frame(frame_idx)?;
    let rgba = reader.decode_frame(frame_idx)?;
    // Process or render RGBA image
}
```

### Streaming Write to a HAP QuickTime Video
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

Ensure you have Rust installed (1.85+ recommended):

```bash
# Clone the repository
git clone https://github.com/your-repo/hap.git
cd hap

# Run all unit and integration tests
cargo test --workspace

# Build the release standalone binary
cargo build --release -p hap-app
```

The compiled binary will be located at:
- Windows: `target/release/hap.exe`
- Linux/macOS: `target/release/hap`

---

## License

This project is licensed under the **MIT License** - see the [LICENSE-MIT](LICENSE-MIT) file for details.
You are free to use, modify, distribute, and integrate this software in proprietary, commercial, or open-source products with zero royalties or copyleft obligations.
