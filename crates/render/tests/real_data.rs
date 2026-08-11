//! GPU tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` is set *and* an adapter is available, so the suite
//! still passes on a machine with neither.

use render::{capture::Offscreen, texture::upload_blp, Blitter, Gpu};

fn setup() -> Option<(Gpu, mpq::Chain)> {
    let data = std::env::var_os("WOW_DATA")?;
    let gpu = match Gpu::block(None) {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("skipping: no GPU adapter ({e})");
            return None;
        }
    };
    Some((gpu, mpq::Chain::open_wow_data(data, "enUS").ok()?))
}

macro_rules! require_gpu {
    () => {
        match setup() {
            Some(pair) => pair,
            None => {
                eprintln!("skipping: WOW_DATA unset or no adapter");
                return;
            }
        }
    };
}

fn load(chain: &mut mpq::Chain, path: &str) -> blp::Blp {
    blp::Blp::parse(&chain.read(path).expect(path)).expect(path)
}

/// DXT data must reach the GPU without a CPU decode -- that is the entire
/// reason `Blp::level` exposes raw blocks.
#[test]
fn dxt_textures_upload_compressed() {
    let (gpu, mut chain) = require_gpu!();
    if !gpu.supports_bc() {
        eprintln!("skipping: adapter lacks BC");
        return;
    }

    for (path, want) in [
        (
            r"CHARACTER\Vrykul\Male\TEXTURES\VYKRULMALEGREY.BLP",
            wgpu::TextureFormat::Bc1RgbaUnormSrgb,
        ),
        (
            r"Character\Goblin\Female\GoblinFemale01.blp",
            wgpu::TextureFormat::Bc2RgbaUnormSrgb,
        ),
        (
            r"Interface\Icons\Achievement_Boss_PathaleonTheCalculator.blp",
            wgpu::TextureFormat::Bc3RgbaUnormSrgb,
        ),
    ] {
        let tex = load(&mut chain, path);
        let uploaded = upload_blp(&gpu, &tex, path);
        assert!(uploaded.compressed, "{path} fell back: {:?}", uploaded.fallback_reason);
        assert_eq!(uploaded.format, want, "{path}");
        // Only levels with real data; the padded tail must never be uploaded.
        assert_eq!(uploaded.mip_levels as usize, tex.usable_mip_count());
    }
}

/// Formats with no GPU equivalent take the decode path, and say why.
#[test]
fn non_dxt_textures_fall_back_to_rgba() {
    let (gpu, mut chain) = require_gpu!();
    let path = r"CHARACTER\Taunka\Male\TAUNKAMALEFACELOWER00_00.BLP";
    let uploaded = upload_blp(&gpu, &load(&mut chain, path), path);

    assert!(!uploaded.compressed);
    assert_eq!(uploaded.format, wgpu::TextureFormat::Rgba8UnormSrgb);
    assert_eq!(
        uploaded.fallback_reason,
        Some("not block-compressed on disk")
    );
}

/// End-to-end check that the GPU path agrees with the CPU decoder.
///
/// Rendering a square texture into an equally square target makes the mapping
/// 1:1, so each output pixel samples one texel at its centre and the result
/// should match the CPU decode within sRGB rounding. This is what catches a
/// wrong block layout, a swapped channel, or an off-by-one row pitch -- none of
/// which a size assertion would notice.
#[test]
fn gpu_output_matches_the_cpu_decoder() {
    let (gpu, mut chain) = require_gpu!();
    if !gpu.supports_bc() {
        eprintln!("skipping: adapter lacks BC");
        return;
    }

    let path = r"CHARACTER\Vrykul\Male\TEXTURES\VYKRULMALEGREY.BLP";
    let tex = load(&mut chain, path);
    let (w, h) = (tex.width(), tex.height());
    assert_eq!(w, h, "test assumes a square texture for a 1:1 mapping");

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = Offscreen::new(&gpu, w, h, format);
    let blitter = Blitter::new(&gpu, format);
    let uploaded = upload_blp(&gpu, &tex, path);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    blitter.draw(
        &gpu,
        &mut encoder,
        &target.view,
        (w, h),
        &uploaded.view,
        (w, h),
    );
    gpu.queue.submit([encoder.finish()]);

    let rendered = target.read_rgba(&gpu).expect("readback");
    let expected = tex.decode_rgba(0).expect("cpu decode");
    assert_eq!(rendered.len(), expected.len());

    // Hardware BC decoding is allowed to differ slightly from ours in how it
    // rounds interpolated endpoints, so compare with a small tolerance and
    // require the vast majority of pixels to agree closely.
    let mut worst = 0i32;
    let mut close = 0usize;
    let total = (w * h) as usize;
    for (got, want) in rendered.chunks_exact(4).zip(expected.chunks_exact(4)) {
        let delta = (0..3)
            .map(|c| (got[c] as i32 - want[c] as i32).abs())
            .max()
            .unwrap_or(0);
        worst = worst.max(delta);
        if delta <= 4 {
            close += 1;
        }
    }
    let ratio = close as f32 / total as f32;
    assert!(
        ratio > 0.99,
        "only {:.2}% of pixels matched within tolerance (worst channel delta {worst})",
        ratio * 100.0
    );
}

/// Headless rendering has to work without a surface, or the screenshot path and
/// these tests would both be impossible.
#[test]
fn renders_offscreen_without_a_window() {
    let (gpu, mut chain) = require_gpu!();
    let path = r"Interface\Icons\Spell_Fire_Fireball02.blp";
    let uploaded = upload_blp(&gpu, &load(&mut chain, path), path);

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = Offscreen::new(&gpu, 320, 200, format);
    let blitter = Blitter::new(&gpu, format);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    blitter.draw(
        &gpu,
        &mut encoder,
        &target.view,
        (320, 200),
        &uploaded.view,
        (uploaded.width, uploaded.height),
    );
    gpu.queue.submit([encoder.finish()]);

    let pixels = target.read_rgba(&gpu).expect("readback");
    assert_eq!(pixels.len(), 320 * 200 * 4);

    // A blank frame would still be the right size, so require real variation.
    let first = &pixels[..4];
    assert!(pixels.chunks_exact(4).any(|px| px != first));
    // The target is opaque.
    assert!(pixels.chunks_exact(4).all(|px| px[3] == 255));
}
