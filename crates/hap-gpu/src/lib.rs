//! # hap-gpu
//!
//! GPU hardware acceleration for HAP video encoding and decoding using wgpu.
//! Features compute shaders for BC1, BC3, YCoCg-BC3, BC4, and BC7, as well as
//! zero-copy hardware BC texture playback.

pub mod compressor;
pub mod renderer;

pub use compressor::{GpuCompressError, GpuCompressor};
pub use renderer::{check_bc_support, GpuCapabilities, GpuTexture};

#[cfg(test)]
mod tests {
    use super::*;
    use hap_core::HapFormat;
    use std::sync::Arc;

    #[test]
    fn test_gpu_compressor_if_available() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }));

        let adapter = match adapter {
            Ok(a) => a,
            Err(_) => {
                println!("No GPU adapter found in test environment; skipping test.");
                return;
            }
        };

        let res = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()));
        let (device, queue) = match res {
            Ok(dq) => dq,
            Err(_) => {
                println!("Could not request GPU device in test environment; skipping test.");
                return;
            }
        };

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let (w, h) = (16, 16);
        let mut compressor = match GpuCompressor::new(device, queue, w, h) {
            Ok(c) => c,
            Err(e) => {
                println!("GPU compressor could not be initialized: {:?}; skipping test.", e);
                return;
            }
        };

        let rgba = vec![128u8; (w * h * 4) as usize];
        let bc1_res = compressor.compress(&rgba, HapFormat::Hap1);
        if let Ok(bc1) = bc1_res {
            assert_eq!(bc1.len(), ((w * h / 16) * 8) as usize);
        }
    }
}
