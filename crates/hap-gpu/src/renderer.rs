//! Direct GPU texture upload and rendering for HAP video playback.

use hap_core::HapFormat;
use std::sync::Arc;

/// Capabilities detected on the active GPU adapter.
#[derive(Debug, Clone, Copy)]
pub struct GpuCapabilities {
    pub supports_bc: bool,
    pub adapter_name: &'static str,
}

/// Helper to check if a wgpu Device/Adapter supports hardware BC texture compression.
pub fn check_bc_support(device: &wgpu::Device) -> bool {
    device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
}

/// Direct GPU texture manager for smooth playback preview.
pub struct GpuTexture {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

impl GpuTexture {
    /// Create a texture for holding decompressed or hardware BC textures.
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        width: u32,
        height: u32,
        texture_format: wgpu::TextureFormat,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("HAP Video Frame Texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            device,
            queue,
            texture,
            view,
            width,
            height,
            format: texture_format,
        }
    }

    /// Upload raw RGBA8 pixels to this GPU texture.
    pub fn upload_rgba(&self, rgba_data: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 4),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Upload raw BCn compressed blocks directly to VRAM without CPU pixel decoding!
    pub fn upload_bc(&self, bc_data: &[u8], hap_format: HapFormat) {
        let bytes_per_block = hap_format.bytes_per_block() as u32;
        let blocks_x = self.width / 4;
        let bytes_per_row = blocks_x * bytes_per_block;

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bc_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(self.height / 4),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }
}
