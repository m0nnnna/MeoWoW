# ADT and WDT

Implementation notes for `crates/adt`. Records what the format actually does
and where it bit us, rather than restating
[wowdev.wiki/ADT](https://wowdev.wiki/ADT).

## Shape

A continent is a 64x64 grid of tiles. The map's `WDT` says which of those 4,096
exist; each one that does is a separate `ADT` holding a 16x16 grid of *chunks*.
A chunk carries a height field, up to four texture layers, and references to the
models standing on it.

Sizes are exact thirds and worth writing down: a tile is `1600/3` units across,
a chunk `1/16` of that, and a height sample `1/8` of a chunk.

Both files use the same chunked container as WMO, which is now shared in
`crates/chunk` — including the detail that magics are stored reversed.

## The height field is two interleaved lattices

145 is `9x9 + 8x8`. Samples are stored as seventeen alternating rows: nine outer
samples, then eight inner ones sitting at the centres of the cells the outer row
just defined, and so on.

`lattice_coords` unpacks an index into `(row, col, inner)` where `row` counts
**half-steps** — so the inner rows' half-unit offset is already in the row
index. Adding another half-unit for inner samples double-counts it and shears
the whole surface. That was the first bug here, and it produces scattered
fragments rather than anything recognisable.

Each cell is drawn as four triangles fanning from its inner sample. Two
triangles per cell would ignore the inner lattice entirely and throw away the
detail it carries.

**Chunks tile exactly, so adjacent chunks must agree along shared edges.**
`Adt::validate` checks that, and it is the test that catches a wrong stride:
a mis-read height field still looks like plausible landscape, but the seams give
it away.

## Winding is clockwise, like M2 and unlike WMO

Terrain and WMO are both "world" geometry, but they do not share a winding
convention. Assuming terrain matched WMO culled almost every triangle and left a
handful of slivers on screen — which reads as broken geometry rather than as a
culling bug, and cost a detour into the height field looking for a fault that
was not there.

The general lesson: when geometry is missing rather than wrong, test culling
before suspecting the data.

## Alpha maps have three storage forms

Layers beyond the first carry a 64x64 alpha map, stored one of three ways:

- **Run-length compressed** when the layer's flag `0x200` is set: a control
  byte's high bit selects fill or copy, its low seven bits give the count.
- **8 bits per texel**, when the *map's* WDT sets flag `0x4`. This is a property
  of the map, not the tile, so an ADT cannot be decoded correctly without
  reading its WDT first.
- **4 bits per texel** otherwise — and this form really encodes 63x63. The last
  row and column are absent and must be repeated from their neighbours unless
  the chunk sets `do_not_fix_alpha_map` (`0x8000`), or every chunk gets a dark
  seam along two edges.

## Everything is named indirectly

`MDDF` and `MODF` placements identify their model by an index into `MMID`/`MWID`,
which are tables of *byte offsets* into the `MMDX`/`MWMO` name blocks. Two hops,
and neither is an ordinal. Doodad scale is fixed-point with 1024 meaning 1.0.

Placement paths still carry the historical `.mdx` extension, so they need the
same rewrite `CreatureModelData` does.

## Holes

`holes` is a 4x4 bitmask punching out sub-squares for doorways and cave mouths.
Each bit removes a 2x2 block of the 8x8 render cells, not a single cell.

## Verification

`wow-cli adt map <map>` summarises a map; `adt tile <map> <x> <y>` shows one
tile; `adt survey` parses every tile of every map and checks the seams.
Expected result on a stock install:

```
106 maps, 5744 tiles parsed, 0 declared but absent
  1023338 doodad placements, 11182 world object placements
no failures
```

## What the ground is made of

A texture layer carries an `effect_id`, and that is the only thing in the whole
format that says a particular square yard is road rather than grass. The area id
says which *zone* it is and nothing about the surface; the liquid sheets say
where water is and nothing about the bank.

The chain runs `MCLY.effect_id` -> `GroundEffectTexture.dbc` -> a `TerrainType`
row -> that row's `sound_id`, which is what `FootstepTerrainLookup` keys on.
Over the whole of Azeroth, **387,009 of 390,011 texture layers name a ground
effect and all 387,009 of those resolve**; the 3,002 that name none are a real
answer rather than a gap.

**The terrain column identifies itself by the filenames of the textures that
reach it**, which is the check that made the chain trustworthy -- a bare small
integer pointing into a twelve-row table is nearly free to get right by
accident, and a filename is not:

| terrain | layers | textures they are called |
|---|---|---|
| 0 `Dirt` | 161,780 | dirt x122,678 |
| 2 `Stone` | 80,747 | rock x66,490 |
| 3 `Snow` | 14,972 | snow x14,484 |
| 5 `Grass` | 63,917 | grass x44,840 |
| 7 `Sand` | 18,715 | sand x6,093 |
| 8 `Soggy` | 35,335 | mud x11,427 |
| 9 `DustyGrass` | 8,404 | grass x3,622 |

That table is also what says terrain **0 really is `Dirt`** rather than "unset",
which was a real question: 22,708 of `GroundEffectTexture`'s 24,981 rows carry
it. `wow-cli sound footsteps` prints it.

### Which layer is underfoot

Up to four textures are blended by three alpha maps, so "which one" is a
question about weights rather than a lookup. `adt::footing` computes exactly the
weights the terrain shader draws with -- layer 3 contributes `a3`, layer 2
`a2 * (1 - a3)`, layer 1 `a1 * (1 - a2) * (1 - a3)`, layer 0 the remainder --
and takes the largest. Deriving it from anything else ("the last layer over
half", say) would be a second answer to a question the picture already answers,
and the two would agree until somebody changed the shader.

**The axes come from the renderer for the same reason.** The terrain mesh gives
each vertex `uv = (col / 8, row / 8)` and the shader samples the blend map with
that `uv`, so the alpha map's *column* runs along the axis `height_in_chunk`
calls `col` and its *row* along the one it calls `row`. Rebuilding that from the
format documentation would be a second derivation of something that has to agree
exactly with what is drawn, and a footing rotated a quarter turn against the
picture is a road that sounds like grass a few yards to one side.

The result is reduced to a 16x16 grid per chunk -- a little over two yards a
cell -- because the alpha maps are uploaded to the GPU and dropped, and the GPU
cannot be asked what a character is standing on. That is 256 bytes a chunk
instead of 4KB.

## Not implemented yet

Liquid (`MH2O`), shadow maps (`MCSH`), vertex colours (`MCCV`) and flight bounds
are parsed past but not interpreted.
