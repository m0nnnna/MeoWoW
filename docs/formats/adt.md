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

## Not implemented yet

Alpha-blended layer compositing — the renderer currently draws each chunk with
its base layer only, so terrain shows the dominant texture without the blends
between them. Liquid (`MH2O`), shadow maps (`MCSH`), vertex colours (`MCCV`) and
flight bounds are parsed past but not interpreted.
