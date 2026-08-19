# WMO

Implementation notes for `crates/wmo`. Records what the format actually does
and where it bit us, rather than restating
[wowdev.wiki/WMO](https://wowdev.wiki/WMO).

WMO is the format for everything built rather than animated: houses, city
blocks, dungeon interiors, bridges. A stock 3.3.5a install has 1,985 root
objects made of 9,346 groups.

## Shape

Unlike M2, WMO is **chunked**: a flat sequence of `(magic, size, payload)`
records. That makes it forgiving — unknown chunks are skipped rather than
shifting everything after them — and it is why this reader tolerates truncation
by simply stopping.

Two things sit right at the front:

- **Chunk magics are stored reversed.** `MVER` is `REVM` on disk. Every
  identifier is un-reversed on read so callers see the documented name.
- **A WMO is more than one file.** The root holds materials, doodads, portals
  and lights; the geometry is in numbered group files beside it. `Foo.wmo` is
  accompanied by `Foo_000.wmo` onwards, always three digits.

Group files parse as WMOs themselves, so anything walking the listfile must
tell them apart from roots — otherwise every wall loads as its own building.
`is_group_path` does that, and a test checks the rule against the real listfile
rather than against itself: for a sample of roots, the declared `_000` group
must actually exist.

## The group header hides a nested chunk stream

A whole group file is wrapped in one `MOGP` chunk whose payload is a 68-byte
header **followed by more chunks**. Iterating the file at the top level finds
only `MVER` and `MOGP`; vertices, indices and batches are one level down inside
`MOGP`'s payload.

## Collision geometry shares the arrays

`MOPY` assigns a material per triangle, and `0xFF` marks collision-only
surfaces. These are not stored separately — they sit in the same vertex and
index arrays as everything visible, and the only thing keeping them off screen
is that no render batch covers them. The Human Farm has 225 of them.

A test asserts that no collision triangle falls inside any batch range, because
the failure mode is invisible walls made visible.

## Everything is indexed by byte offset

Texture names, group names and doodad names are all string blocks addressed by
byte offset rather than by index — `MOMT.texture1` is an offset into `MOTX`,
not the *n*th texture. A group's own name lives in the **root's** `MOGN` block,
so parsing a group requires passing that block in.

The doodad name offset shares its 32-bit word with a flags byte and needs
masking to 24 bits.

## Winding is the opposite of M2's

**WMO triangles wind counter-clockwise; M2 triangles wind clockwise.**

Using one convention for both culls exactly the surfaces you want to see. The
symptom is not an obviously inverted model: the Human Farm's roof simply
vanished and the interior ceiling showed through, which reads as missing
geometry or a hole in the mesh rather than as a culling bug. Disabling culling
entirely produced a solid, correctly textured roof, which is what identified it.

Winding is therefore part of the pipeline's `RenderState` rather than a
constant.

## Materials

Blend modes are their own enumeration, not M2's: `0` opaque, `1` the
alpha-tested cutout used for railings and foliage, higher values blend. Every
material on the Human Farm is opaque, including its window glass.

Note that a pale render is not necessarily wrong. Stormwind-style human
buildings genuinely use near-white plaster and pale blue slate; both textures
look almost blank when exported, and mistaking them for a missing-texture
placeholder wastes time.

## Doodad sets

Doodads are M2s placed in the WMO's local space, and `MODS` partitions them
into named sets of which only one is active. That is how a single building
ships furnished and empty — the Human Farm has six sets over 132 placements.
Sets are parsed but not yet rendered.

## Verification

`wow-cli wmo info <root>` prints materials, textures, doodad sets and a line
per group. `wow-cli wmo survey` parses everything. Expected result on a stock
install:

```
1985/1985 root objects parsed, 9346 groups
  29384952 vertices, 31649493 triangles, 250300 doodad placements
  largest group: 32761 vertices
no failures
```

`--wmo-group <n>` in the viewer draws a single group, which is how the roof was
isolated from the walls while chasing the winding bug.


## What a surface is made of

`MOMT` carries a `ground_type`, and it is the same currency the terrain outside
uses: a **`TerrainType` row id**. With `MOPY` giving a material per triangle --
and the group's own validation asserting `MOPY` is parallel to `MOVI`'s triples,
checked archive-wide by `wmo survey` -- that is enough to ask what a character
standing on a particular triangle is standing on.

**Identified by the filenames of the textures the materials are painted with,
scored against a baseline.** Rock and wood are everywhere in this game's art, so
a raw share proves nothing. Row 10 (`None`) is 91% of the table and is by
construction the materials that decline to say anything, which makes it the
control:

| row | materials | own word | share | in `None` | enrichment |
|---|---|---|---|---|---|
| 0 `Dirt` | 118 | dirt | 7.6% | 0.5% | x15.9 |
| 1 `Metallic` | 575 | metal | 21.4% | 3.2% | x6.6 |
| 2 `Stone` | 808 | stone | 7.4% | 3.7% | x2.0 |
| 3 `Snow` | 78 | snow | 24.4% | 2.0% | x12.1 |
| 4 `Wood` | 490 | wood | 47.3% | 6.6% | x7.1 |
| 5 `Grass` | 10 | grass | 60.0% | 0.2% | x305 |
| 7 `Sand` | 9 | sand | 77.8% | 0.2% | x469 |

over all 1,985 root WMOs. `Stone` is weakest for a readable reason -- a stone
floor is usually filed under `rock`. `Leaves` (2 materials) and `DustyGrass`
(6) are too small to vote.

**The "nothing" value is row 10, not row 0.** Outside, `GroundEffectTexture`
uses 0 (which is also `Dirt`) for a texture that says nothing, on 22,708 of
24,981 rows. In here it is row 10 (`None`), on 22,893 of 25,034 materials, and
row 0 is a rare genuine `Dirt`. One column meaning, two opposite defaults;
reading this one as 0 would have every wall claiming to be dirt.

**Most buildings say nothing.** Only **622 of 1,985** label any surface at all.
Northshire Abbey is not one of them -- all 130 of `NSabbey.wmo`'s materials are
`None` -- while the Elwynn lake bridge is, with `Wood` planks over `Stone`
piers. A feature built on this is correct and silent in two buildings out of
three, which is a fact about the authoring rather than about the reader.

`wow-cli wmo footing` is that measurement. One thing it also records is a test
that came back **flat**: filing art under a `floor` directory does not separate
a surface from a wall, because `None` is 5% floor art and so is `Metallic`.

## Not parsed yet

Portals (`MOPV`/`MOPT`/`MOPR`), lights (`MOLT`), fog (`MFOG`), liquid (`MLIQ`),
and the BSP tree (`MOBN`/`MOBR`). Portal-based visibility is what makes large
interiors affordable, so it matters before city-scale scenes.
