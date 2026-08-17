//! GPU device management and asset upload.
//!
//! Kept separate from the windowing layer so the same code can run headless:
//! every capability here works without a surface, which is what makes
//! screenshot rendering and GPU tests possible in CI.

pub mod blit;
pub mod camera;
pub mod capture;
pub mod mesh;
pub mod precipitation;
pub mod sky;
pub mod terrain;
pub mod texture;

pub use blit::Blitter;
pub use camera::{Camera, Fly, Orbit};
pub use mesh::{GpuMesh, MeshRenderer, MeshVertex};
pub use precipitation::PrecipitationRenderer;
pub use sky::SkyRenderer;
pub use terrain::TerrainRenderer;
pub use texture::UploadedTexture;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(String),
    #[error("could not create device: {0}")]
    Device(String),
    #[error("reading back a texture failed: {0}")]
    Readback(String),
}

/// An initialised GPU device.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// Creates a device, optionally constrained to one that can present to
    /// `surface`. Pass `None` for headless work.
    pub async fn new(surface: Option<&wgpu::Surface<'_>>) -> Result<Self, Error> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| Error::NoAdapter(e.to_string()))?;

        // Block compression is the whole point of uploading BLP data
        // untouched, but it is a feature rather than a guarantee, so ask for
        // it and fall back to CPU decoding if the adapter says no.
        let wanted = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let features = wanted & adapter.features();
        if !features.contains(wanted) {
            tracing::warn!("adapter lacks BC texture compression; will decode on the CPU");
        }

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("MeoWoW device"),
                required_features: features,
                required_limits: adapter.limits(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .map_err(|e| Error::Device(e.to_string()))?;

        let info = adapter.get_info();
        tracing::info!(
            adapter = %info.name,
            backend = ?info.backend,
            bc = features.contains(wanted),
            "gpu ready"
        );

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    pub fn block(surface: Option<&wgpu::Surface<'_>>) -> Result<Self, Error> {
        pollster::block_on(Self::new(surface))
    }

    /// Whether DXT/BC blocks can be handed to the GPU as-is.
    pub fn supports_bc(&self) -> bool {
        self.device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    }

    /// Human-readable adapter summary for the debug overlay.
    pub fn describe(&self) -> String {
        let info = self.adapter.get_info();
        format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type)
    }
}
