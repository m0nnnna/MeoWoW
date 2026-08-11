//! Offscreen rendering and readback.
//!
//! Exists so the viewer can produce a frame without a window. That makes the
//! render path testable in CI, and gives a way to check visual output from a
//! terminal rather than by looking at a screen.

use crate::{Error, Gpu};

/// Buffer row pitch required by texture-to-buffer copies.
const COPY_ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// A render target that can be read back.
pub struct Offscreen {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

impl Offscreen {
    pub fn new(gpu: &Gpu, width: u32, height: u32, format: wgpu::TextureFormat) -> Self {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            texture,
            width,
            height,
            format,
        }
    }

    /// Copies the target back to host memory as tightly packed RGBA8.
    ///
    /// The GPU copy needs each row padded to a 256-byte boundary, so the
    /// padding is stripped on the way out.
    pub fn read_rgba(&self, gpu: &Gpu) -> Result<Vec<u8>, Error> {
        let unpadded = self.width * 4;
        let padded = unpadded.div_ceil(COPY_ALIGN) * COPY_ALIGN;

        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * self.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| Error::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| Error::Readback(e.to_string()))?
            .map_err(|e| Error::Readback(e.to_string()))?;

        let view = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| Error::Readback(e.to_string()))?;
        let mut out = Vec::with_capacity((unpadded * self.height) as usize);
        for row in view.chunks_exact(padded as usize) {
            out.extend_from_slice(&row[..unpadded as usize]);
        }
        drop(view);
        buffer.unmap();
        Ok(out)
    }
}
