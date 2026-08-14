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

### Blend mode 4 is additive, and it is 17% of everything

A material's blend value maps to a pipeline blend state. `0` is opaque, `1` is
alpha-tested, `3` and `4` are **both additive**; `2`, `5` and `6` currently all
collapse into straight alpha blending, and `5`/`6` (modulate and modulate-2x,
3.8% of materials between them) are still wrong.

`4` mattered enough to find on its own. A weapon draws its blade twice: once
with the item texture `ItemDisplayInfo` names, and again over the *same
submesh* with a hardcoded reflection map such as `ARMORREFLECT3.BLP`. That
reflection is a DXT1 with no alpha channel, so under alpha blending it is fully
opaque and hides the first pass completely — which rendered every reflective
weapon in the game as a flat dark silhouette. It reads as "the texture failed
to load", and it is not; the texture loads and is then painted over.

The argument for additive is structural, not aesthetic. If blend 4 covered what
was beneath it, `model_texture_left` — a column filled in for 19,702 items —
could never be seen on any weapon carrying a reflect layer. A column exists to
be seen.

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

### The palette must be as long as the longest skeleton

A shared palette — one buffer for a whole scene of different models, as the
world paths use — has to be sized for the largest skeleton in it, not for one
matrix.

This is worth its own heading because of *how it fails*. A vertex whose bone
index runs past the end of the palette does not produce an error or a warning:
the storage read returns zero, the skinned position collapses to the origin, and
the model **silently disappears**. Nothing in the frame says a model was
skipped, because it was not skipped — it was drawn, at zero size.

It cost a debugging session. Server-placed creatures were invisible while the
map's own doodads rendered perfectly, and the two look nothing alike from the
outside: the obvious conclusion was that the entities had never been placed. In
fact a tree indexes bone 0 — in range in a one-element palette — and a character
model indexes sixty. The scene was drawing all of them, and the humanoids were
collapsed to a point.

The rule: when models are drawn in the bind pose, allocate `BIND_POSE_BONES`
matrices and fill *all* of them with identity. Uploading a single matrix leaves
the rest of the buffer zeroed, which is the same failure.

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

The *rotation* has its own trap, and this one shipped wrong: the stored yaw is
offset by 90 degrees, and the sign of that offset was inverted, mirroring every
building in the world. See "Facing needs half a turn" below.

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

## Standing in a live world

`--realm-host` logs in, enters the world as `--character`, and streams the map
the *server* chose around the position it reported. This is where the renderer
and the protocol finally meet:

```console
$ wow-viewer --realm-host <host> --user <account> --character Testwolf --radius 1
Testwolf on Eastern Kingdoms (map 0, Azeroth)
at -8950.0, -132.5, 83.5 facing 0.00 rad
tile 32,48
91 objects reported
```

Three things join up, and only one of them needed new code:

- **The position needs no conversion.** A network position is already in the
  space the renderer streams tiles in. That is *not* true of the coordinates in
  the data files — ADT placements are stored measured inwards from the grid
  corner with the axes permuted, which is why `placement_position` exists — so
  the natural assumption is that the network needs a conversion too. It does
  not, and applying one lands the camera on a tile that does not exist. Both
  halves of that are pinned by tests in `live.rs`.
- **The map comes from `Map.dbc`.** The server sends a numeric map id; the
  renderer needs a directory name. One lookup.
- **Entities are ordinary instanced draws.** A creature is a display id, which
  `CreatureDisplayInfo` turns into a model and its skins — the same path
  `--creature` already used.

Entity models are cached by **display id, not by path**: a display id supplies
skins on top of a model path, so two ids sharing a path are different-looking
creatures, and keying by path gives the second one the first one's hide.

### Driving movement

Once logged in, the viewer keeps the `world::Connection` alive in `App` rather
than dropping it after login, and drives it from the render loop on held
W/S/A/D: W/S send the `MSG_MOVE_*` stream (`MoveStartForward`/`MoveStop` or
`MoveStartBackward`/`MoveStop`, with a heartbeat roughly every 100 ms while
moving), A/D turn the character locally. The camera is recomputed from the
character's position and orientation every frame instead of flying freely, so
it tracks behind rather than needing to be steered separately.

No background thread reads the socket: the connection is pumped once per frame
with a 1 ms `drain`, which is what keeps `SMSG_TIME_SYNC_REQ` answered, plus an
explicit keepalive no faster than `PING_INTERVAL`. Doing this on the render
thread is deliberate, not a shortcut -- RC4 header state cannot be shared or
rewound, so exactly one place may ever read the socket.

The keepalive uses `Connection::send_ping`, which fires and returns rather than
waiting for the pong. `Connection::ping` -- fine in a CLI, where the round trip
*is* the point -- blocks for one on a render thread: tens of milliseconds on
the live realm, and up to the full read timeout if the server stalls, every
`PING_INTERVAL`. The next frame's `drain` collects the echo instead.

Z follows the terrain. The keys drive the two horizontal axes; the altitude is
then read back out of the height field the ground is drawn from
(`World::height_at`, which resolves a position to its tile, its chunk and the
surface inside it) rather than kept at whatever the server last reported.

Carrying a stale Z presented as four apparently unrelated faults, which is
worth recording because none of them pointed at altitude: the character sank
into rising ground; the click marker landed off-centre, the picking ray
starting from an eye that is a fixed offset above a wrong altitude; hills could
not be walked up; and *another* client saw this one twitch, as the server
corrected an altitude that had been wrong for a while. One cause, four bug
reports.

The height field is sampled the way it is drawn -- four triangles fanning from
each cell's inner-lattice sample, not a bilinear patch across the outer four --
so the ground a character stands on is the ground it can see. See
`adt::height_in_chunk`, and `wow-cli adt height <map> --x= --y=` to ask about
any position offline.

What confirmed it was not the render: the human starting position the realm
reports at login, `-8950.0, -132.5, 83.5`, against `wow-cli adt height Azeroth
--x=-8950.0 --y=-132.5`, which answers **83.528** from the map files alone. Two
derivations that share nothing -- one a value stored on a server, one an
interpolation over an ADT -- agreeing to three centimetres. The offline version
of the same check is in `crates/adt`'s real-data tests: a tile's doodads were
placed by an artist standing them on this surface, and 706 of Northshire's 759
sit within a unit of it, with a median offset of zero.

Jumping, falling and collision are still absent, and so is standing on anything
that is not terrain -- a bridge or an upper floor is WMO geometry, which
nothing here can be asked about yet. Where the ADT describes no terrain at all
(a hole, punched for a doorway or a cave mouth) the altitude is left alone
rather than guessed.

### Drawing the replicated world

`LiveWorld` carries a `world::WorldState` alongside the connection instead of
the one-shot `Vec<Entity>` the login burst used to produce. Every batch
`pump_live_connection` drains is folded into it with `WorldState::replicate`:
object updates, relayed movement, monster moves and destroys all have to be
handled in the same place, because a caller that folded only object updates
would build a world that looked plausible and was quietly frozen everywhere
else.

That dispatch briefly existed twice -- once here, once in `tools/wow-cli` --
before moving into `WorldState` itself. Two copies of an opcode table over the
same state machine is exactly the failure the previous paragraph describes,
one level up: a new opcode wired into one and not the other freezes whatever
it should have moved, silently. `live::replicate` is now a two-line wrapper
that calls `WorldState::replicate` and keeps only the failure count the viewer
cares about; `wow-cli` calls the same method for the fuller `Replication`
report it prints.

`live::drawable_entities` turns that state into what the renderer needs --
guid, display id, an interpolated position and facing, scale, whether a move
is currently in flight, excluding the character's own body -- read fresh from
`state` each time it is called rather than cached, since the state changes on
every fold.

**Position is interpolated, not snapped.** `SMSG_MONSTER_MOVE` only ever
reports a path's start, its end, and how long the whole thing takes.
`Entity::interpolated_position` lerps between them against wall-clock elapsed
time since the move was received (`move_started`), clamped to the endpoints.

Facing has two regimes, and both are computed inside `interpolated_position`
itself -- nowhere else, so nothing can bypass either one. While `t < 1.0`
(still travelling), it is the direction of travel: `from` and `to` never
carry their own orientation, both decoding fixed at zero, since the wire does
not report a *starting* facing. Once `t >= 1.0` (arrived -- which a
zero-duration move reaches immediately, not through some separate early
return that would skip this logic entirely), `Entity::arrival_facing` takes
over if the wire supplied one for this move, falling back to the same
direction-of-travel computation otherwise. An *arrival* facing is a
different thing the wire genuinely can supply: three of the five move types
carry one, and `FACING_ANGLE`'s is parsed into `MonsterMove::facing` rather
than discarded (`FACING_SPOT` and `FACING_TARGET` also carry one, as a point
or a guid to face rather than a bare angle, and remain unparsed -- the former
is a small further step from here, the latter needs another entity's live
position, a `WorldState` lookup the packet parser has no access to).

Storing the arrival hint anywhere *read* only during motion in progress does
not work -- it would simply never be seen, since a path in flight is always
interpolated by direction of travel regardless. It has to live as a value
consulted specifically once `t` crosses 1.0, which is what `arrival_facing`
is for. Without either kind of hint, a stop falls back to whatever the
entity was already facing a moment before -- not to the parser's placeholder
zero, which briefly turned every stop into "snap to face east" regardless of
which way the creature had been walking. `Entity::is_moving` answers a
related but distinct question, whether that move's *duration* has actually
elapsed: `destination` alone stays set to the last move's endpoint long after
it arrives, so checking only that reported a creature "moving" forever after
its first move ever, with no idle state.

**Both are re-evaluated every frame, not on a timer.** An earlier version
throttled the whole rebuild -- repositioning *and* animating -- to a few times
a second, reasoning that rebuilding every instance buffer every frame was too
costly to justify. That cost was never actually measured against this
project's entity counts (tens to a couple hundred), and what the throttle
produced, watched live, was a stutter: legs mid-stride from a walk cycle
sampled every frame, over a body that only advanced ten times a second. If a
much larger population ever makes `set_entities` measurably expensive, the fix
is updating existing instances' transforms in place rather than reallocating
every buffer -- not reintroducing the same timer.

**Animation is per-(display id, motion) bucket, not per display id.** Several
instances routinely share one display id -- a zone's wolves, say -- and in a
populated zone at least one of a given species is almost always moving.
Animating or not animating a whole display id together is wrong either way:
gate on "any instance moving" and every standing wolf plays the walk cycle
forever; gate on "all instances moving" and a genuinely moving one stands
rigid. `set_entities` splits each display id into up to three groups -- one per
`Motion` -- each with its own instance buffer and its own bone buffer, keyed by
`(display id, motion)`. The split is nearly free: the model itself is still
cached by display id alone in `entity_cache`, so drawing a second bucket for
one species is a clone of the same `Rc`, not a second load. Sequence ids (0 for
stand, 4 for walk, 5 for run) come from `AnimationData.dbc`'s public row
layout, the same convention for every 3.3.5a model.

**Three buckets rather than two, because "moving" does not say which cycle.**
A wolf padding along a patrol route and a wolf charging you are both simply
*moving*, and one flag has to pick a single cycle for both: walk, and the
charge is dragged along by its own legs; run, and the patrol skates ahead of
them. What separates them is a speed, and a speed is not on the wire either --
`SMSG_MONSTER_MOVE` carries two endpoints and a duration, and the speed fields
in a unit's update block say what it *can* do, not what it is doing.
`Entity::move_speed` divides the one by the other, which is the only statement
about the move actually in flight, and `Motion::from_speed` splits at 4.75
units per second: the midpoint of 3.3.5a's 2.5 walk and 7.0 run, which also
leaves the 4.5 backing-up speed on the walking side. A model with no run
sequence falls back to its walk one rather than to the bind pose; nothing falls
back as far as standing, which would be the "creature walking on the spot" bug
inverted. `sequence_for` resolves that fallback for both `set_entities` (which
creates the bone buffer) and `update_animations` (which writes the pose into
it), so the two cannot disagree about which sequence a bucket is playing --
a disagreement that would not error anywhere, just quietly pose one cycle into
a buffer drawn as another.

The player's own body takes the same path, from the keys rather than the wire:
the server never relays our movement back to us. `LIVE_RUN_SPEED` is 7.0, which
is the *run* speed -- 3.3.5a walks at 2.5, and walking is a toggle nothing here
sends -- so holding W now runs rather than walking at run speed.

**A deferral the bucket split does not fix: every instance in one bucket
shares one pose.** One bone buffer per `(display id, moving)` key means one
`update_animations` call writes one pose for every instance drawn from it, so
five wolves walking together animate in exact lockstep -- identical frame,
identical phase, all at once. The bucket split fixed the worse version of
this (a standing wolf playing the walk cycle at all); it did not fix the
residual, and unlike the other tradeoffs on this page, it was not written
down anywhere until now. Giving each instance its own phase would mean either
a bone buffer per instance -- most of the population of a zone, each with its
own storage buffer and bind group, for a handful of bones' worth of
difference -- or sampling `pose_bones` per instance on the CPU and packing the
result into a shared buffer addressed by instance index, which is real
architecture work, not a quick follow-up. Deferring it is still the right
call; this paragraph exists so that call is on record rather than assumed.

**Facing: M2 needs no offset, WMO needs a half turn -- and the reason this
took three attempts is that a *culling* bug was wearing a rotation bug's
clothes.**

M2 geometry was being drawn with clockwise winding, which culls every
front-facing triangle. That does not produce a missing model. It produces one
you can see *into*: what survives is the interior of the far surface, which
has a silhouette, a texture and a size, and reads at a glance as a model
facing away from you. So a correctly-oriented character looked reversed, a
static render at a server-confirmed heading appeared to show its back where
its face belonged, and a half turn was added to entities to "fix" it. The
rotation was the innocent one.

Separating them took a live A/B: with the winding corrected to
counter-clockwise, a key that toggles a half turn on entity facing settles it
in seconds -- everything walks forwards at zero offset and backwards at half
a turn. `OWC_NO_CULL=1` is kept as the diagnostic that found the winding, by
making the see-through fronts disappear.

The settled values, and what each rests on:

| what | offset | evidence |
|---|---|---|
| entities (players, creatures) | none | live A/B with the winding fixed |
| doodads (M2) | none | fence runs measured from positions; same format as entities |
| world objects (WMO) | +180 | a real client screenshot: the abbey's portal faces the path, between lamp pillars whose positions no rotation can move |

**This section previously said the opposite of all of it** -- that M2 winds
clockwise, and that facing needed no offset because an M2 faces +X and the
doodad quarter-turn was a mistake to carry over. The +X part was right by
accident and for the wrong reason. Everything else was written from reasoning
about the formats rather than from looking at a render, which is exactly the
failure the rest of this document warns about.

Superseded, kept for the shape of the mistake:

This section used to claim entity facing needed no offset, on the grounds that
an M2's forward is +X. It is not: **an M2's local forward is -X**, and until
that was measured, every creature in the world was turned exactly backwards
and every doodad and building was mirrored with it.

Two separate paths carry the same fact, and both are now corrected:

- **Entities** (`world::set_entities`) rotate by `orientation + PI`.
- **ADT placements** (`scene::placement_rotation`) offset yaw by `+90`, not
  `-90`. That is the same 180 degrees, arriving through the doodad formula's
  own quarter-turn.

Neither could be checked for as long as it went unchecked, and the reason is
worth stating because it is not carelessness. Creature headings come from the
server with nothing to compare them against, and a wolf, a tree or a fence
gives no clue which end is the front. The two references that finally settled
it are both asymmetric and both external to the renderer:

- The **player's own character**, once it was drawn: turn it to a heading the
  server confirms (`wow-cli world --face 0`), put the camera at the matching
  yaw so it stands directly behind, and whether you see a face or a back is
  not a matter of opinion.
- **Northshire Abbey**, once the camera was pointed at it: at `-90` it renders
  a blank wall of stained glass with no way in, and at `+90` it renders the
  entrance, its steps and its portico. A building with a door is a compass.

The general form, which is in `CLAUDE.md` as well: a value with nothing to
compare it against is not verified by looking at it, however carefully.

Verified with the same two-client rig that closed 3.4: one client walked while
the other -- running the real `wow-viewer` binary rather than a test harness --
drew the replicated character sliding, turning to face its direction of
travel, playing its walk cycle while moving and its stand cycle at rest, all
at full frame rate. None of the four bugs above showed up in a headless run or
in `cargo test` -- only in watching the window while a second client moved.

### What still looks wrong

~~Humanoid NPCs render white.~~ Fixed; see "Everyone has a skin now" below.
The diagnosis recorded here was also wrong in an instructive way, so it is kept:
it said a runtime compositor was needed, when in fact an artist had already
composed every one of those textures and shipped them in the archives.

Facing uses the same quarter-turn offset as the doodad path, on the reasoning
that the offset is a property of where an M2's forward axis points rather than
of where the angle came from. Position is what these screenshots really assert;
facing is inferred from that consistency and has not been checked against a
reference client.

### Everyone has a skin now

Two different populations rendered as white ghosts, for two different reasons,
and only one of them was the reason previously written down here.

**Humanoid NPCs: the texture was already composed, by an artist.** The note
above assumed a runtime compositor was needed, because that is what a *player*
needs. It is not what an NPC needs. `CreatureDisplayInfo.extended_display_info_id`
points at `CreatureDisplayInfoExtra`, whose `bake_name` column names a finished
texture of the whole character — armour, tabard, face and all — under
`Textures\BakedNpcTextures\`. Nothing to blend; one texture load.

The scale of it is worth stating: **15,446 of build 12340's 24,262 display ids
have an extended row and no texture variation of their own.** 64% of every
creature appearance in the game had no skin from anywhere this client looked.
Guards, innkeepers, quest givers — every humanoid that is not a beast.

Two measurements made this safe to write rather than guess:

- The columns were confirmed by *consistency*, not transcription. Group every
  extra row by its race and gender columns and ask which model the displays
  pointing at it actually use: 33 groups, each dominated by exactly the
  matching character model — race 1 male by `HumanMale.mdx` 2,133 times, race
  20 male by `NorthrendSkeletonMale.mdx` 30 times, through all 21 races. The
  tail of one to five rows per group is the data reusing an extra row across
  displays, not a column meaning something else.
- **The bakes are in the archives but not in the listfile.** `wow-cli ls
  BakedNpcTextures` shows 50 of them, and a coverage check built on that
  listing concluded that 0.1% of bake names ship — which would have killed the
  whole approach. MPQ resolves by hash, not by listing: 40 of 40 randomly
  sampled names read back fine. *Listing a directory and reading a path are
  different questions*, and the cheap one answered the wrong one.

**Other players: the appearance is on the wire, at an index worth measuring.**
A player's display id is 49 for every human male alive and its
`CreatureDisplayInfo` row is empty, so a stranger's five character-creation
numbers have to come from their update fields — `PLAYER_BYTES` and
`PLAYER_BYTES_2`. Transcribing those indices from memory produces a client that
parses perfectly and gives every stranger the wrong face.

So they were searched for instead. The same five numbers arrive twice by
unrelated routes — `SMSG_CHAR_ENUM`, confirmed against a live realm since 3.2,
and the update fields — so `wow-cli world --enter <name> --appearance` packs the
character list's answer and asks which field holds it.

The first two runs returned *two* candidates and settled nothing, because every
character this project had ever created was made with an all-zero appearance,
and a search for zero matches every zero field in the object. That is the
"a property test is only as good as the population you run it against" rule
arriving for the third time. `--create` therefore grew appearance flags, and a
character made with five *different* non-zero values matched exactly one field,
holding `0x02070503` — which pins the byte order as well as the index, since any
other packing would have matched nothing.

**Then the same tooling found the bug in the first version of the fix.** A
stranger standing in Northshire was still white, and `--appearance` said why:
field `0x99` *unset*, field `0x9a` present. **An absent update field is a zero,
not an unknown** — a create block carries only non-zero values, so a character
with the default appearance has no `PLAYER_BYTES` at all, while
`PLAYER_BYTES_2` still arrives because its upper bytes hold rest state.
Refusing on absence left exactly the plainest-looking players white, which is
the bug the field was added to fix. Both directions were then observed: the
field appears when non-zero and is omitted when zero.

Verified end to end through the two-client rig, which is the strongest shape
available here: a character was *created* on one account with skin 3, face 5,
hair 7, colour 2, facial hair 4, and a viewer logged in on the *other* account
read those exact five numbers back out of the update fields and resolved them to
`HumanMaleSkin00_03.blp` and `Hair02_02.blp`. The appearance went out through
character creation, through the server, and back in through a different client's
update stream — the write and the read confirmed against each other via a third
party that had to understand both.

The measurable end state, on one Northshire scene of 17 drawn entities: **no
entity has an unfilled body texture.** Before, exactly one did, and that one
number is a better regression test than any screenshot — which is why
`World::entity_model` now warns, with the display id and the slot, whenever a
model draws against a placeholder. `load_dressed` had always collected that list
and every caller had always dropped it, so the entire white-humanoid problem was
invisible in the logs. Same shape as the packet body this project once refused
and threw away.

What is still unfilled in that scene is slot type 2 — the object/item skin,
which is equipment — and slot 6 on models whose hair geoset is bald. Equipment
geometry and its textures are the next piece; `CreatureDisplayInfoExtra` already
carries eleven item display ids per NPC, read and named but unused.

Player hair was reported missing and is not: human-male hairstyle 0 is *bald*.
`CharHairGeosets` maps only variation 0 to geoset 0, and all thirteen colours of
`CharSections` type 3 for that variation have an empty texture. The character
being looked at simply had no hair. A character created with hairstyle 7 draws
its hair correctly.

### Armour, painted on

A character's clothes are not geometry in the first instance -- they are eight
texture patches blended onto the same composed skin the face and underwear go
onto. `ItemDisplayInfo` names them per item, and this client now paints them for
the player's own body.

**The columns named themselves, so nothing had to be transcribed.** Every stored
component name carries its own suffix: `Leather_A_05Yellow_Chest_TU` is a
torso-upper, `..._Pant_LL` a leg-lower. Across all 57,986 rows each of the eight
columns is 98.9%-100% dominated by exactly its own suffix, in order. The
stragglers are Blizzard's own typos -- a few names ending `_A`, a few truncated
to a bare trailing underscore -- which is also why the check is stated as a
percentage rather than an assertion.

**The regions were confirmed twice over.** The five new ones (arms, hand, torso
upper and lower, leg lower, foot) join the three the face and underwear had
already pinned. Component textures ship at two resolutions, 128 wide and 256
wide, so their sizes prove nothing alone -- but their aspect ratios do: hand,
torso-lower and foot measure 4:1 and the other five 2:1, exactly as the layout
predicts, and swapping any two regions breaks that. The stronger check is that
the ten regions **tile the 512x512 skin exactly**, left column 128+128+64+64+128
and right column 128+64+128+128+64, both reaching 512 with nothing overlapping
and nothing left over. A layout guessed a region at a time would not close, and
`the_skin_regions_tile_it_exactly` now fails if anyone moves one.

**No slot enum had to be transcribed either.** Items are painted in the order
`SMSG_CHAR_ENUM` sends them, and that order is already inner-to-outer wherever
two items share a component: shirt before chest, bracer before glove, trouser
before boot. So the layering falls out of the wire order, and the one thing that
could be wrong -- something painted over what should cover it -- is exactly what
a render shows.

**`--skin-out` writes the composed atlas to a PNG**, and it earned its place
immediately. Seen at walking distance the character looked bare-chested, and the
obvious reading was that the torso regions were wrong. The atlas showed all ten
regions painted correctly and the torso wearing a white shirt with brown braces
-- which at that size reads as skin. *The character was correct and the look at
it was not.* Ten regions assembled from a dozen files are far too small on
screen to judge individually; the atlas answers in one glance which is the same
reason every format here has a dump command.

Still texture-only. Sleeves, boot tops and glove cuffs that stand off the body
are geometry, switched on by `ItemDisplayInfo.geoset_group_*` against the item's
inventory type -- a mapping that lives in client logic rather than in a column,
so it wants rendering rather than transcribing. Weapons, shields and shoulders
are separate M2s on attachment points. And other players are still undressed:
their visible-item fields carry item *entry* ids, which need `Item.dbc` to reach
a display id, and those field indices want the same search treatment
`PLAYER_BYTES` got.

### Lighting, part one: the clock and the curves

Lighting and a day/night cycle are **one piece of work, not two**, and the data
says so before any code does. Every colour in `LightIntBand` and every distance
in `LightFloatBand` is a *curve over time of day* -- sixteen keys in half-minutes
since midnight. There is no static ambient colour to add a cycle to later; a
client that hardcoded an hour would be rewriting this the moment the sun moved.

So the clock came first.

**The clock.** `SMSG_LOGIN_SETTIMESPEED` was named in `opcode.rs` and parsed
nowhere. It is twelve bytes: a packed date-and-time word, a float, and a word
that is zero in the only capture on hand and is therefore left unnamed. The
float is `0x3C88888A`, which is 1/60 -- game minutes per real second, so this
realm runs game time at wall-clock rate. A client that assumed that rate would
drift on a realm that chose otherwise, so it is read rather than assumed.

The packed word was **confirmed against the wall clock rather than
transcribed**. One capture decoded to minute 2, hour 4, weekday 4, day 12, month
7, year 26 -- and it was taken at 04:02 UTC on Thursday 13 August 2026. All six
fields agree, including the two that are zero-based and the one that counts from
2000. That is six independent checks against a calendar this project did not
write, and a wrong layout could not match the date as well as the time. A second
capture two game-minutes later differed in exactly one byte, the minute.

**The curves.** Four tables, and the awkward part is that the curves are not
addressed by a column:

    LightIntBand.id   = (LightParams.id - 1) * 18 + n + 1
    LightFloatBand.id = (LightParams.id - 1) * 6  + n + 1

That was measured. The ids are *sparse* in all three tables -- `LightParams`
runs to 917 across 850 rows -- so a rule that only worked for dense ids would
have looked fine on the first row and failed later. It reproduces both maxima
exactly: 916x18+18 = 16,506 and 916x6+6 = 5,502, the largest id in each band
table, on top of exact 18:1 and 6:1 row-count ratios.

**Lights are positional, not per-zone.** A `Light` row sits at a point on a map
with an inner and outer radius. Northshire is covered by none of Azeroth's 82
lights -- the nearest is 124,000 units away -- and falls back to light 1, whose
position is the origin and whose radius is zero: the map default. `wow-cli
light` prints the runners-up for exactly this reason, because "the wrong light
was chosen" and "the right light looks like this" are indistinguishable from a
single row.

**Blue is the low byte, and the data settled it.** Byte order in a packed colour
is the kind of thing that yields a blue sky or an orange one with equal
confidence. Sampling Azeroth's sky bands at three hours decides it:

| | midnight | dawn | noon |
|---|---|---|---|
| blue first | (0, 12, 32) near-black blue | (255, 171, 64) orange | (58, 162, 207) sky blue |
| red first | (32, 12, 0) dark brown | (64, 171, 255) blue | (207, 162, 58) ochre |

One of those is a sky and the other is not, three bands agree, and the
disagreement is widest exactly where it is most obvious -- a sunrise.

**What the eighteen bands mean is deliberately still blank.** They are ambient,
diffuse, sky gradient, fog and so on, and none of that has been confirmed here.
Naming them from memory is the `describe_cast_failure` mistake: a wrong offset
fails loudly, a wrong *name* just misexplains for ever. `wow-cli light` prints
all eighteen with their brightness at four hours of the day, which is the
evidence needed to identify them by what they do -- band 0 goes pure orange at
dawn and bands 3 to 5 track the sky. Settling it wants a render, not a table.

Next: choosing the light for the camera's position each frame, sampling the
bands at the running clock, and replacing the camera-following headlight in
`mesh.rs` and `terrain.rs` -- which is where those band meanings get confirmed
or corrected.

### Lighting, part two: on screen, and the band a render rejected

The headlight is gone. `sun.w` in the camera uniform carries "there is light
data"; where there is, both shaders use the world's own ambient and diffuse and
a real sun direction, and where there is not -- a model view, a texture view,
any scene with no place and no hour -- they fall back to the fixed
ambient-plus-headlight they always had, so an offline render still reads as
shape rather than going black.

**Terrain and models share the lighting functions verbatim.** Terrain lit one
way and the buildings standing on it lit another is the seam a player notices
first.

**The sun's direction is chosen, not measured, and that is stated wherever it is
used.** No table carries one: `Light.dbc` and its bands describe colours over
time and say nothing about direction. `sun_direction` rises at 06:00, is
overhead at noon, sets at 18:00 and spends the night below the horizon -- where
the dot product clamps to zero and only ambient remains, which is what makes
night look like night with no second code path.

**Band 0 was the first guess at the direct light, and a render refused it.**
It looked plausible from the numbers: bright at every hour, shifting hue rather
than intensity. Rendered, midday Elwynn had olive grass and an orange road,
because band 0 reads (255, 136, 0) at noon. Band 6 is neutral grey
(180, 180, 180) at noon and dim blue (49, 86, 123) at midnight -- a sun that
becomes a moon -- and with band 1's blue-grey fill the pair lands close to the
0.38/0.62 placeholder it replaced. That closeness is the sanity check: real data
arriving at roughly where a hand-tuned constant already was.

This is the fourth time on this project that a table has been identified by
rendering it rather than by reading it, and the first three are why `bands` is
a named module of constants rather than numbers scattered through the shader
feed. One wrong index there does not fail -- it just makes the world the wrong
colour.

**The sky is cleared to the world's own colour too.** No skybox is drawn yet,
but without this a midnight scene is lit for night and framed by a daytime
horizon, which reads as a bug in the lighting rather than as a missing feature.
Dawn now comes up peach, noon pale blue, midnight dark blue.

**Fog is wired and currently invisible**, which is worth saying rather than
leaving to be discovered: the distance band reads 18,000 units on Azeroth and
fog starts at a quarter of that, so nothing within kilometres of the camera is
touched. The colour band it uses is unconfirmed. Both are left connected rather
than disabled, so the day the distance turns out to mean something else, it
shows up as fog appearing rather than as nothing happening.

`--hour` overrides the realm's clock, because the curves are functions of the
hour and waiting six real hours for dusk is not a debugging loop. `wow-cli
light` prints exactly what the renderer will use, resolved **through the same
code** -- a verification tool that computed its own numbers would stop being
evidence about the renderer the moment either drifted.

Still to do: a skybox proper (`LightSkybox` names the models), weather, the
several sky bands that layer into a gradient rather than one flat colour, and
the interior lights that matter once a building has an inside.

### Game objects

Doors, chests, mailboxes, signposts, benches, and the ships and zeppelins the
server sends every client regardless of where it is. Thirty-two of them arrive
in Northshire's login burst; until now every one was created in world state and
then dropped, because `Entity::display_id` read the *unit* display field and
nothing else.

**The field was found by search, and the search caught a false positive worth
recording.** Resolving every set field of all thirty-two objects against
`GameObjectDisplayInfo` gives *two* fields that hit 100%: `0x08` and `0x02`. The
table has 3,790 rows spread over ids up to 9,624, so at 39% density "is this a
valid id" is nearly free -- the same trap `Spell.dbc`'s duration column set.

What separates them is not validity but **variation**:

| field | distinct values | resolves to |
|---|---|---|
| `0x02` | 1 (always 33) | `PowderKeg01.mdx`, thirty-two times |
| `0x08` | 7 | inn benches x16, elevators, a zeppelin, ships |

Thirty-two identical powder kegs is not Northshire. Sixteen inn benches at the
abbey the player is standing in, is. `0x02` is the object's type mask and its
validity was an accident of density.

**A display id means different things to different objects.** 603 is a wolf in
`CreatureDisplayInfo` and an inn bench in `GameObjectDisplayInfo`, so
`Entity::display_id` now branches on `object_type` -- the caller already knows
which kind it holds, and a client that reads the wrong field gets a plausible id
and the wrong model. For the same reason the renderer's entity cache xors a
constant into the key for game objects: without it, whichever of the two loaded
first would answer for both.

**Game objects reuse the tile loader's path-keyed cache rather than the
creature one.** `GameObjectDisplayInfo.model` names an `.mdx` *or* a `.wmo` -- a
mailbox is a model and a ship is a building -- and that cache already handles
both and already deduplicates: Northshire's sixteen benches load one model
between them.

Their rotation is the orientation from the position block, which is right for
anything standing on level ground. Game objects also carry a packed quaternion
for genuinely tilted things, which is read past and not applied; a leaning
signpost is the visible symptom, and there is not one in Northshire to look at.

### Equipment geometry

Gloves, boot tops, sleeves and cloaks are geometry, not paint: geosets inside the
character model, switched on by `ItemDisplayInfo.geoset_group_*`. Which *group*
an item switches depends on the slot it is worn in, and that mapping is client
logic rather than a column -- the shape `CLAUDE.md` warns can only be found by
looking.

**So it was found by looking, twice over.** First at the data, then at a render.

The model says which groups exist. `wow-cli m2 info --limit 200` on `HumanMale`
lists `401`-`404`, `501`-`505`, `802`-`803`, `902`-`903`, `1002`, `1102`, `1104`,
`1202`, `1301`-`1302`, `1501`-`1506`, `1703`, `1802`. (The `--limit` exists
because the list was truncated at 24 with "37 more", and a dump that cannot show
the thing being asked about is not a dump.)

The items say which values each slot uses. Every inventory type was identified
by **what its items paint** rather than from a remembered enum: type 8's items
set a foot texture, type 10's a hand texture, type 16's almost nothing but a
thousand cape geosets. Then the two are matched:

| slot | type | values in the data | model has | group |
|---|---|---|---|---|
| hands | 10 | 1, 2, 3 | 401-404 | 4 |
| feet | 8 | 1, 2, 3 | 501-505 | 5 |
| back | 16 | 1, 2, 5 | 1501-1506 | 15 |
| shirt, chest | 4, 5 | 1, 2 | 802, 803 | 8 |
| robe | 20 | 1, 2 | 1301, 1302 | 13 |

**Belt, legs and tabard are deliberately absent.** Their values do not line up:
a tabard uses value 1 where the only tabard geoset is `1202`, and legs drive two
of the three columns at once. A mapping for them would be a guess dressed as a
table, and this project has already paid four attempts for geoset rules read
rather than rendered. A missing belt buckle costs nothing; a robe's skirt on
somebody's arm costs an afternoon.

The same measurement identified `CreatureDisplayInfoExtra`'s eleven item
columns, which have no inventory type of their own -- their *position* is the
slot. Column 6's 23,102 items set a foot texture; column 7's set only a lower
arm; column 10's set almost no texture and a thousand capes.

**Variant zero means "adds no geometry", not "this group is empty".** The first
version treated every equipped item as a decision about its group, which hides
the bare body part underneath -- and starting gear routinely carries a geoset
group of zero, so Testwolf's ordinary boots would have left him with no feet.
`Look::decided_groups` now records only the groups something actually switched
on, and everything else falls back to the bare-body rule that was already there.

Verified by rendering. A Stormwind guard, whose gear is all variant zero, keeps
his bare hands and plain boots; an NPC wearing real gear comes back with
flared gauntlet cuffs, armoured boot tops and a cloak.
