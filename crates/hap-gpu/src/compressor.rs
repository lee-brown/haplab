//! GPU-accelerated DXT and BCn texture compressor using wgpu compute shaders.

use bytemuck::{Pod, Zeroable};
use hap_core::HapFormat;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GpuCompressError {
    #[error("Device or adapter error: {0}")]
    Device(String),
    #[error("Buffer mapping failed: {0}")]
    BufferMap(String),
    #[error("Invalid dimensions: {width}x{height} (must be > 0 and multiples of 4)")]
    InvalidDimensions { width: u32, height: u32 },
    #[error("Input buffer size mismatch: expected {expected}, got {actual}")]
    InputSizeMismatch { expected: usize, actual: usize },
    #[error("Unsupported format for GPU compression: {0:?}")]
    UnsupportedFormat(HapFormat),
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct CompressParams {
    width: u32,
    height: u32,
    blocks_x: u32,
    blocks_y: u32,
    refine_iters: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

fn with_refit(source: &str) -> String {
    format!("{}\n{}", include_str!("shaders/_refit.wgsl"), source)
}

/// GPU-accelerated block compressor using wgpu compute shaders.
pub struct GpuCompressor {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,

    pipeline_bc1: wgpu::ComputePipeline,
    pipeline_bc3: wgpu::ComputePipeline,
    pipeline_ycocg: wgpu::ComputePipeline,
    pipeline_bc4: wgpu::ComputePipeline,
    pipeline_bc7: wgpu::ComputePipeline,

    bind_group_layout: wgpu::BindGroupLayout,
    input_buffer: wgpu::Buffer,
    output_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    params_buffer: wgpu::Buffer,

    pub width: u32,
    pub height: u32,
    pub blocks_x: u32,
    pub blocks_y: u32,
}

impl GpuCompressor {
    /// Initialize a GPU compressor for the specified image dimensions.
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        width: u32,
        height: u32,
    ) -> Result<Self, GpuCompressError> {
        if width == 0 || height == 0 || width % 4 != 0 || height % 4 != 0 {
            return Err(GpuCompressError::InvalidDimensions { width, height });
        }

        let blocks_x = width / 4;
        let blocks_y = height / 4;
        let total_blocks = (blocks_x * blocks_y) as u64;

        // Largest format is 16 bytes per block (BC3 / BC7)
        let input_size = (width as u64) * (height as u64) * 4;
        let output_size = total_blocks * 16;

        let input_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("HAP Input Buffer"),
            size: input_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("HAP Output Buffer"),
            size: output_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("HAP Readback Buffer"),
            size: output_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("HAP Params Uniform Buffer"),
            size: std::mem::size_of::<CompressParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("HAP Compress Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("HAP Compress Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        // Helper to build a compute pipeline from shader source
        let make_pipeline = |name: &str, source: &str| -> Result<wgpu::ComputePipeline, GpuCompressError> {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(name),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            Ok(device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            }))
        };

        let pipeline_bc1 = make_pipeline("BC1 Pipeline", &with_refit(include_str!("shaders/bc1_compress.wgsl")))?;
        let pipeline_bc3 = make_pipeline("BC3 Pipeline", &with_refit(include_str!("shaders/bc3_compress.wgsl")))?;
        let pipeline_ycocg = make_pipeline("YCoCg-BC3 Pipeline", &with_refit(include_str!("shaders/ycocg_bc3_compress.wgsl")))?;
        let pipeline_bc4 = make_pipeline("BC4 Pipeline", &with_refit(include_str!("shaders/bc4_compress.wgsl")))?;
        let pipeline_bc7 = make_pipeline("BC7 Pipeline", &with_refit(include_str!("shaders/bc7_compress.wgsl")))?;

        Ok(Self {
            device,
            queue,
            pipeline_bc1,
            pipeline_bc3,
            pipeline_ycocg,
            pipeline_bc4,
            pipeline_bc7,
            bind_group_layout,
            input_buffer,
            output_buffer,
            readback_buffer,
            params_buffer,
            width,
            height,
            blocks_x,
            blocks_y,
        })
    }

    /// Compress raw RGBA8 pixels into block-compressed texture data on the GPU.
    pub fn compress(&mut self, rgba_data: &[u8], format: HapFormat) -> Result<Vec<u8>, GpuCompressError> {
        let expected_len = (self.width as usize) * (self.height as usize) * 4;
        if rgba_data.len() != expected_len {
            return Err(GpuCompressError::InputSizeMismatch {
                expected: expected_len,
                actual: rgba_data.len(),
            });
        }

        let pipeline = match format {
            HapFormat::Hap1 => &self.pipeline_bc1,
            HapFormat::Hap5 => &self.pipeline_bc3,
            HapFormat::HapY => &self.pipeline_ycocg,
            HapFormat::HapA => &self.pipeline_bc4,
            HapFormat::Hap7 => &self.pipeline_bc7,
            other => return Err(GpuCompressError::UnsupportedFormat(other)),
        };

        let bytes_per_block = match format {
            HapFormat::Hap1 | HapFormat::HapA => 8,
            HapFormat::Hap5 | HapFormat::HapY | HapFormat::Hap7 => 16,
            _ => 16,
        };
        let output_bytes_len = (self.blocks_x as usize) * (self.blocks_y as usize) * bytes_per_block;

        // 1. Upload pixels
        self.queue.write_buffer(&self.input_buffer, 0, rgba_data);

        // 2. Upload params uniform
        let params = CompressParams {
            width: self.width,
            height: self.height,
            blocks_x: self.blocks_x,
            blocks_y: self.blocks_y,
            refine_iters: 2,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        self.queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(&params));

        // 3. Create bind group
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("HAP Compress Bind Group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.input_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params_buffer.as_entire_binding(),
                },
            ],
        });

        // 4. Dispatch compute shader
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("HAP Compress Encoder"),
        });

        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("HAP Compress Pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(pipeline);
            cpass.set_bind_group(0, &bind_group, &[]);
            cpass.dispatch_workgroups(self.blocks_x, self.blocks_y, 1);
        }

        // 5. Copy output buffer to readback buffer
        encoder.copy_buffer_to_buffer(
            &self.output_buffer,
            0,
            &self.readback_buffer,
            0,
            output_bytes_len as u64,
        );

        self.queue.submit(Some(encoder.finish()));

        // 6. Map buffer and read results synchronously
        let slice = self.readback_buffer.slice(..output_bytes_len as u64);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = sender.send(res);
        });

        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());

        match receiver.recv() {
            Ok(Ok(())) => {
                let data = slice.get_mapped_range();
                let result = data.to_vec();
                drop(data);
                self.readback_buffer.unmap();
                Ok(result)
            }
            Ok(Err(e)) => Err(GpuCompressError::BufferMap(format!("{:?}", e))),
            Err(e) => Err(GpuCompressError::BufferMap(e.to_string())),
        }
    }
}
