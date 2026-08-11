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

## Drawing models

`MeshRenderer` caches one pipeline per distinct render state. M2 materials vary
along three axes a pipeline cannot switch at draw time — blending, face
culling, and whether alpha is tested — so the combination is the cache key,
and pipelines are built on first use. A stock creature needs one or two; the
full set is eight.

Pipelines must be built *before* recording, because building takes `&mut self`
while a render pass holds the encoder. `prepare()` pre-warms them so the pass
can look them up immutably.

Batches are sorted opaque-first so the depth buffer is populated before
anything blends against it. Within a group the authored batch order is kept:
M2 orders its batches deliberately.

**Alpha-keyed materials are not transparent for sorting purposes.** They
`discard` rather than blend, so they still write depth and belong in the opaque
pass. Treating them as transparent produces sorting artefacts on hair and
foliage for no reason.

### Coordinate system

WoW is **Z-up, right-handed**, with `+X` forward — the direction a model faces.
The orbit camera uses `Vec3::Z` as up; the usual `+Y`-up assumption lays every
model on its side, which is easy to mistake for a broken vertex layout.

Projection must match wgpu's NDC: **Z in 0..1 with Y up**, the DirectX/Metal
convention. In glam 0.33 that is `camera::rh::proj::directx::perspective`. The
`vulkan` module is also Z 0..1 but Y *down* and would flip the image; `opengl`
uses Z -1..1 and would clip wrongly.

### Framing

`Orbit::frame` fits a bounding sphere rather than the box, because the camera
orbits and the box's silhouette changes as it does.

The bounds come from the vertices actually drawn, **not** from the M2 header's
bounding box. The header box is a culling volume that also covers animation
extents: for `GnollMelee` it is nearly twice the diagonal of the static pose,
which leaves the model small and off-centre in frame.

### Textures a model does not name

Most M2 texture slots have `kind != 0`, meaning the client supplies them.
Creature skins are types 11 to 13, filled from
`CreatureDisplayInfo.texture_variation_*`. The variation is a bare name and the
directory comes from the **model's** path, never from the DBC:
`ShadowHideGnollFighterSkin` in `Creature\GnollMelee\GnollMelee.m2` resolves to
`Creature\GnollMelee\ShadowHideGnollFighterSkin.blp`.

Unresolved slots fall back to a 1x1 white texture so the geometry still renders
as shaded shape rather than failing to draw, and the overlay lists what was
missing.

## Skinning

Bone matrices go to the GPU in a **storage** buffer rather than a uniform: bone
counts are per-model and reach 315, which a fixed-size uniform array would have
to over-allocate for. The palette is allocated per model and rewritten each
frame from a freshly evaluated pose.

Vertices carry `Uint8x4` bone indices and `Unorm8x4` weights, so 255 arrives in
the shader as `1.0` with no divide. Indices address the model's bone list
directly.

Two details the shader has to handle:

- **Weights do not always sum to exactly 1.** They are quantised to 255ths, so
  the blended position is divided by the actual total.
- **Unweighted vertices exist.** Rigid props parented to a single bone leave the
  weights blank; those pass through untransformed rather than collapsing to the
  origin.

The bind pose is not a special case — it is all-identity matrices, which
reproduces exactly what the unskinned renderer drew, so the two paths cannot
drift apart.

## Per-object transforms

Placements are **instance attributes**, not a uniform: a `mat4` occupying vertex
locations 5 to 8 with `step_mode: Instance`. One buffer holds every transform in
a scene and each draw selects its range, so nothing is rebound between objects
and identical models collapse into a single instanced draw — a tile with 785
doodads loads 83 meshes.

Normals are transformed by the same matrix without an inverse-transpose, which
is correct only because placements are rigid with uniform scale. If non-uniform
scaling ever appears, that shortcut has to go.

A vertex buffer cannot be empty, so `InstanceBuffer::upload` substitutes a
single identity entry rather than allowing a zero-length buffer, and
single-asset scenes bind an identity instance so they take the same path.

## Placement coordinates

ADT placements are stored with the axes permuted relative to terrain vertices:
the stored middle component is height, and the other two run *inwards* from the
grid corner, so converting is `32 * TILE_SIZE - v`. Getting it wrong puts every
object somewhere plausible but entirely elsewhere.

Rotations are Euler degrees in the game's internal Y-up space, and yaw is offset
by 90 degrees because the stored angle is measured from a different axis than
the model's forward.

## Framing a tile

Frame on the **tile**, not on everything it references. A tile at the edge of a
city lists that city's WMO in full: Northshire pulls in all of Stormwind, whose
bounding box is over a thousand units across, and framing that shrinks the tile
to a speck beside a distant cluster of buildings. That looked like a placement
bug for a while and was simply correct geography.

## Cameras

Two, sharing conventions so a bearing means the same thing in either: `Orbit`
circles a target, `Fly` moves freely. Worlds fly by default — orbiting a
nine-tile block means circling something two kilometres wide, which is useless
for looking at anything inside it.

`Fly::from_orbit` converts between them, and the viewer applies `--yaw`/`--pitch`
to an orbit camera *before* converting. Overriding a fly camera's angles after
positioning it leaves the camera in place staring at empty sky, which is exactly
the screenshot it produced the first time.

Strafing uses a right vector re-levelled against world up, so looking down and
strafing never rolls the view, and vertical movement follows world up rather
than the view direction.

## Loading a block of tiles

`--radius n` loads the `(2n+1)²` tiles around one, skipping any the WDT does not
declare — coastlines are ragged and a block near one is mostly ocean.

**Placements must be deduplicated by `unique_id`.** An object straddling a tile
border is listed by *every* tile it touches, so a nine-tile block without
deduplication draws the same building several times over itself. Northshire's
3x3 block yields 27 buildings and 4,933 doodads from 192 unique models.

## Terrain has its own pipeline

Every other surface samples one texture. A terrain chunk samples **four** and
mixes them with a per-chunk alpha map, which cannot be expressed through the
single-texture material binding the mesh pipeline uses — so terrain gets its own
pipeline, sharing the camera bind group so both see the same view.

Layers 1 to 3 pack into the red, green and blue channels of one RGBA texture per
chunk. Layer 0 is the base and needs no coverage of its own; chunks with fewer
layers leave the unused channels at zero, so those layers contribute nothing and
the padding texture bound to their slots never shows.

Two details the shader depends on:

- **Tileset art repeats eight times across a chunk.** UVs are stored in chunk
  space (0 to 1) and multiplied by eight for the tileset; sampling the tileset
  at chunk coordinates instead stretches a single copy over the whole chunk,
  which is what the first terrain renders did.
- **The blend map must clamp, not repeat.** It spans exactly one chunk, so
  wrapping stitches a seam along two edges of every one of them.

The blend texture is `Rgba8Unorm`, not sRGB: it is coverage, not colour, and
putting a gamma curve on it would bias every blend.

Landscape draws before the objects standing on it. It is opaque and fills most
of the frame, so drawing it first rejects the most fragments.

## Streaming

`--stream --radius n` keeps the `(2n+1)²` tiles around the camera resident,
loading and evicting as it moves. Three things make that work at world scale.

**Models are cached across tiles.** Elwynn's trees stand on every tile; loading
them per tile multiplies memory by the number resident. Failures are cached too,
so a broken path is not retried by every tile that places it.

**Each placement is owned by exactly one tile** — the tile its *position* falls
on. An object straddling a border is listed by every tile it touches, so without
a single owner it is drawn once per listing and only partly removed when one
neighbour is evicted.

**Loading is budgeted.** Reading and uploading a tile is slow enough to show as
a stall, so only two are admitted per frame and the rest queue nearest-first, so
the view fills outwards.

Eviction keeps a one-tile margin beyond the load radius. Without it a camera
sitting on a boundary reloads the same tiles every time it drifts a metre.

Stormwind at radius 2 is 25 tiles, 374 cached models, 11,490 instances and
13,183 draw calls.

### Tile coordinates

`tile_at` inverts the grid, and the axes are **swapped as well as inverted**: a
tile's origin is `(32 - tile_y) * TILE_SIZE` on x and `(32 - tile_x) * TILE_SIZE`
on y. A tile owns everything *below* its origin on both axes, so a point exactly
on the origin belongs to the neighbour — which is what a boundary test has to
account for.

### Empty buffers

`GpuMesh::upload` substitutes a one-element buffer for empty input, because a
zero-sized buffer cannot be sliced and slicing is unconditional at draw time.
Loaders still reject degenerate geometry outright; the padding exists so the
failure is a missing model rather than a panic inside a render pass.
