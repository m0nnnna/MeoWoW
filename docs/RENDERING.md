# Rendering

Notes on `crates/render` and `apps/viewer`.

## Layering

`crates/render` owns the GPU and knows nothing about windows. Every capability
in it works headless, which is what makes `--screenshot` and the GPU tests
possible. `apps/viewer` adds winit and egui on top.

That split is worth preserving: a render path that can only be exercised by
opening a window cannot be tested, and this project will accumulate a lot of
render code.

## Textures go to the GPU compressed

`Blp::level` returns DXT blocks exactly as stored, and `upload_blp` hands them
straight to `wgpu` as `Bc1/Bc2/Bc3RgbaUnormSrgb`. The hardware samples block
compression natively, so decoding first would cost both time and four times the
memory.

The CPU decoder is the fallback, taken when:

- the texture is palettized or BGRA (no GPU equivalent),
- the adapter lacks `TEXTURE_COMPRESSION_BC`, or
- the base dimensions are not a multiple of 4.

That last one is not hypothetical. WebGPU requires a block-compressed texture's
base size to be a whole number of 4x4 blocks, and a stock install contains
textures as wide as 1365px. `UploadedTexture::fallback_reason` records which
case applied, and the viewer's overlay shows it.

### Copy extents must be whole blocks

Uploading the tail of a mip chain fails with `Copy width is not a multiple of
block width` if you pass the *logical* mip size. A 2x2 mip of a BC texture still
occupies one physical 4x4 block, so `write_texture` takes the padded extent
(`blocks_per_row * 4`) even though the mip is logically smaller. Only the last
two levels of any chain are affected, which is exactly the kind of thing that
survives casual testing on one texture.

## Colour

The surface is configured to an sRGB format, and textures are uploaded with
`...UnormSrgb`. Sampling therefore linearises, and writing re-encodes — which is
what future lighting maths will need.

egui logs a warning about this: it prefers a non-sRGB target. It is not a
defect. `egui-wgpu` selects a dedicated `fs_main_linear_framebuffer` shader
entry point when the target `is_srgb()`, so the UI is composited correctly
either way. Choosing a non-sRGB surface to silence the warning would mean
hand-encoding sRGB in our own shaders instead, which is the worse trade.

## Dependency pinning

**`wgpu`, `egui`, `egui-wgpu`, and `egui-winit` move together.** `egui-wgpu`
pins a `wgpu` major version, and mixing them produces two `wgpu` crates in the
graph whose `Device` types are unrelated — the failure is a wall of type errors
in code you did not write. The working combination is recorded in the workspace
`[workspace.dependencies]` table:

| Crate | Version |
|-------|---------|
| wgpu | 30 |
| egui, egui-wgpu, egui-winit | 0.36 |
| winit | 0.30 |

### The `windows` crate pin

`Cargo.lock` pins `windows` to a version that both `wgpu-hal` and
`gpu-allocator` can share. This matters more than it sounds.

`wgpu-hal 30` requires `windows ^0.62`. `gpu-allocator 0.28` requires
`windows >=0.58, <=0.62`. Left alone, cargo resolved `gpu-allocator` onto
`windows 0.58` while `wgpu-hal` used `0.62`, and the DX12 backend failed to
compile with dozens of errors about `ID3D12Device` not matching `ID3D12Device`
— the same type from two different crate versions.

If a future `cargo update` reintroduces this, the fix is to force both onto one
version:

```console
cargo update -p windows@<old> --precise 0.62.0
```

This is an upstream packaging problem, not something wrong with the project.
