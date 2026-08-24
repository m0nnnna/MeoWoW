//! GPU device management and asset upload.
//!
//! Kept separate from the windowing layer so the same code can run headless:
//! every capability here works without a surface, which is what makes
//! screenshot rendering and GPU tests possible in CI.

pub mod blit;
pub mod camera;
pub mod capture;
pub mod celestial;
pub mod cull;
pub mod liquid;
pub mod mesh;
pub mod particles;
pub mod precipitation;
pub mod shading;
pub mod shadow;
pub mod sky;
pub mod terrain;
pub mod texture;

pub use blit::Blitter;
pub use camera::{Camera, Fly, Orbit};
pub use celestial::CelestialRenderer;
pub use cull::Frustum;
pub use liquid::{LiquidRenderer, LiquidVertex};
pub use mesh::{GpuMesh, MeshRenderer, MeshVertex};
pub use particles::{ParticleRenderer, RibbonVertex, SpriteInstance};
pub use precipitation::PrecipitationRenderer;
pub use shadow::ShadowMap;
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
    /// Staged writes since this was last read: calls and bytes.
    ///
    /// **Because `queue.submit` is 2.1 ms before it has drawn anything.**
    /// Correlating submission against draw count across a live session gives
    /// r = +0.41 -- a 26% rise in draws moves it 12% -- so most of it is a
    /// fixed cost that has nothing to do with the scene. `write_buffer` does
    /// not copy when it is called; it stages, and the staging belt is flushed
    /// at submit. Every bone palette, every instance buffer and every uniform
    /// this client rewrites per frame therefore shows up *there* rather than
    /// where it was written, which is the one place nobody was looking.
    ///
    /// Calls and bytes both, for the reason every other counter here reports
    /// two numbers: a hundred small writes and one large one cost differently
    /// and want different fixes -- batching versus writing less.
    /// Atomics rather than a `Cell` because `Gpu` is shared across threads --
    /// the GPU tests hold one in a `OnceLock`, which is what stopped eleven
    /// concurrent DX12 device creations from deadlocking. `Relaxed` is right:
    /// nothing orders on these, they are only ever read once a frame by the
    /// thread that wrote them.
    write_calls: std::sync::atomic::AtomicU64,
    write_bytes: std::sync::atomic::AtomicU64,
}

impl Gpu {
    /// Stages a buffer write and counts it. **Use this rather than
    /// `gpu.queue.write_buffer` directly** -- see [`Gpu::writes`]; a write
    /// that is not counted is a cost that reappears inside `submit` with
    /// nothing naming it.
    pub fn write_buffer(&self, buffer: &wgpu::Buffer, offset: u64, data: &[u8]) {
        use std::sync::atomic::Ordering::Relaxed;
        self.write_calls.fetch_add(1, Relaxed);
        self.write_bytes.fetch_add(data.len() as u64, Relaxed);
        self.queue.write_buffer(buffer, offset, data);
    }

    /// Reads the staging counters and zeroes them.
    pub fn take_writes(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.write_calls.swap(0, Relaxed),
            self.write_bytes.swap(0, Relaxed),
        )
    }

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
            write_calls: std::sync::atomic::AtomicU64::new(0),
            write_bytes: std::sync::atomic::AtomicU64::new(0),
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
