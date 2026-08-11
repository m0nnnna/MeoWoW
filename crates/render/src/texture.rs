//! Uploading BLP textures to the GPU.
//!
//! Block-compressed data is handed over untouched wherever the hardware allows
//! it: the GPU samples DXT natively, so decoding first would cost time and four
//! times the memory. Everything else is expanded to RGBA8.

use blp::{Blp, DxtFormat, Encoding, Level};

use crate::Gpu;

/// A texture living on the GPU, with enough provenance for the debug overlay
/// to explain what happened to it.
pub struct UploadedTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    pub mip_levels: u32,
    /// True when DXT blocks went across as-is.
    pub compressed: bool,
    /// Why the compressed path was not taken, when it was not.
    pub fallback_reason: Option<&'static str>,
    pub bytes_uploaded: usize,
}

fn bc_format(format: DxtFormat) -> wgpu::TextureFormat {
    // Game art is authored in sRGB, so sampling must linearise it.
    match format {
        DxtFormat::Dxt1 => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
        DxtFormat::Dxt3 => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
        DxtFormat::Dxt5 => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
    }
}

/// Decides whether a texture can take the compressed path.
///
/// WebGPU requires a block-compressed texture's base dimensions to be a
/// multiple of the 4x4 block. Most game art is power-of-two, but not all of it
/// -- the widest texture in a stock install is 1365px -- so this has to be
/// checked rather than assumed.
fn compressed_plan(gpu: &Gpu, tex: &Blp) -> Result<DxtFormat, &'static str> {
    let Encoding::Dxt(format) = tex.encoding() else {
        return Err("not block-compressed on disk");
    };
    if !gpu.supports_bc() {
        return Err("adapter lacks TEXTURE_COMPRESSION_BC");
    }
    if tex.width() % 4 != 0 || tex.height() % 4 != 0 {
        return Err("dimensions are not a multiple of the 4x4 block");
    }
    Ok(format)
}

/// Uploads a texture, choosing the compressed path when it is available.
pub fn upload_blp(gpu: &Gpu, tex: &Blp, label: &str) -> UploadedTexture {
    // Only the levels backed by real data; the tail of a BLP mip chain is
    // padding, and describing it to the GPU would over-read.
    let levels = tex.usable_mip_count().max(1) as u32;

    match compressed_plan(gpu, tex) {
        Ok(format) => upload_compressed(gpu, tex, label, format, levels),
        Err(reason) => {
            tracing::debug!(texture = label, reason, "decoding on the CPU");
            upload_decoded(gpu, tex, label, levels, Some(reason))
        }
    }
}

fn create(
    gpu: &Gpu,
    label: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    levels: u32,
) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn upload_compressed(
    gpu: &Gpu,
    tex: &Blp,
    label: &str,
    format: DxtFormat,
    levels: u32,
) -> UploadedTexture {
    let wgpu_format = bc_format(format);
    let texture = create(gpu, label, tex.width(), tex.height(), wgpu_format, levels);
    let mut bytes_uploaded = 0;

    for level in 0..levels as usize {
        let Some(Level::Dxt { blocks, .. }) = tex.level(level) else {
            continue;
        };
        let (w, h) = tex.level_size(level);
        let blocks_per_row = w.div_ceil(4);
        let block_rows = h.div_ceil(4);

        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            blocks,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(blocks_per_row * format.block_bytes() as u32),
                rows_per_image: Some(block_rows),
            },
            // Copy extents for a block format must be whole blocks. The last
            // mips of a chain are logically 2x2 and 1x1, but each still
            // occupies one physical 4x4 block, so the copy uses the padded
            // size rather than the logical one.
            wgpu::Extent3d {
                width: blocks_per_row * 4,
                height: block_rows * 4,
                depth_or_array_layers: 1,
            },
        );
        bytes_uploaded += blocks.len();
    }

    UploadedTexture {
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        texture,
        width: tex.width(),
        height: tex.height(),
        format: wgpu_format,
        mip_levels: levels,
        compressed: true,
        fallback_reason: None,
        bytes_uploaded,
    }
}

fn upload_decoded(
    gpu: &Gpu,
    tex: &Blp,
    label: &str,
    levels: u32,
    fallback_reason: Option<&'static str>,
) -> UploadedTexture {
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let texture = create(gpu, label, tex.width(), tex.height(), format, levels);
    let mut bytes_uploaded = 0;

    for level in 0..levels as usize {
        let Some(rgba) = tex.decode_rgba(level) else {
            continue;
        };
        let (w, h) = tex.level_size(level);
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        bytes_uploaded += rgba.len();
    }

    UploadedTexture {
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        texture,
        width: tex.width(),
        height: tex.height(),
        format,
        mip_levels: levels,
        compressed: false,
        fallback_reason,
        bytes_uploaded,
    }
}

/// A trilinear sampler with repeat addressing, which is what almost all game
/// art expects.
pub fn default_sampler(gpu: &Gpu) -> wgpu::Sampler {
    gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("default sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    })
}
