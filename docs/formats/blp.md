# BLP

Implementation notes for `crates/blp`. Records what the format actually does
and where it bit us, rather than restating
[wowdev.wiki/BLP](https://wowdev.wiki/BLP).

## Shape

A 0x94-byte header, a 256-entry BGRA palette, then up to 16 mip levels located
by parallel offset and size tables. The palette is **always present and always
full size**, even for DXT textures that never index into it, so pixel data
never starts before offset 0x494.

Pixels are stored one of three ways, selected by the `encoding` byte:

| Encoding | Storage |
|----------|---------|
| 1 | 8-bit palette indices, followed by a separate packed alpha plane |
| 2 | Block-compressed: DXT1, DXT3, or DXT5 |
| 3 | Raw BGRA8888 |

Content type 0 means JPEG. Blizzard stopped using it long before this client,
so it is rejected rather than supported — encountering one means we are
misreading the file.

## What a stock install actually contains

From `wow-cli blp survey` over 107,927 readable textures, all of which parse:

| Encoding | Count |
|----------|-------|
| DXT1, alpha depth 0 | 37,227 |
| palettized, alpha depth 8 | 24,742 |
| DXT5, alpha depth 8 | 13,543 |
| DXT1, alpha depth 1 | 12,258 |
| palettized, alpha depth 0 | 11,126 |
| DXT3, alpha depth 8 | 7,766 |
| palettized, alpha depth 1 | 882 |
| DXT3, alpha depth 4 | 289 |
| palettized, alpha depth 4 | 90 |
| BGRA8888 | 4 |

Every path is exercised by real data, including all four palettized alpha
depths. BGRA is vanishingly rare — four files, one of which
(`textures\SunGlare.blp`) declares a nonsensical `alpha_depth` of 136. That
field is meaningless when alpha is already in the pixels, so it must not be
validated for this encoding.

The widest texture is 1365px, which is **not** a power of two. Do not assume
power-of-two dimensions.

## The mip chain lies

`mip_count` is not the number of usable levels.

Blizzard's tooling stops generating real mips once a dimension falls below one
4-pixel block row, then pads every remaining entry with a **single dummy
block**. `Interface\AchievementFrame\UI-Achievement-MetalBorder-Top.blp` is
512x16 and declares ten levels:

```
 0:   512x16      8192 bytes
 1:   256x8       2048 bytes
 2:   128x4        512 bytes
 3:    64x2         16 bytes   <- would need 256
 4:    32x1         16 bytes   <- would need 128
 ...
 9:     1x1         16 bytes
```

Levels 4 through 6 are byte-for-byte identical filler. The data is perfectly
contiguous and ends exactly at EOF, so nothing is missing or truncated — the
file is exactly as intended.

`Blp::usable_mip_count` returns the leading run whose stored size matches its
dimensions, and **only those levels may be uploaded to the GPU**. Describing a
16-byte buffer as a 64x2 DXT5 surface reads past the end of it. Across a
2,500-texture sample, every padded level is exactly one block, so the tail is a
uniform known shape rather than arbitrary truncation.

## Decoding details that matter

- **565 endpoints must be replicated, not shifted.** `(r << 3)` maps 0x1F to
  0xF8; the low bits have to be filled from the high ones (`(r << 3) | (r >> 2)`)
  or every texture comes out slightly dark.
- **DXT1 punchthrough.** When the two colour endpoints are in ascending order,
  the block trades its fourth interpolated colour for transparency. Ignoring
  the ordering makes cutout textures — foliage, hair — opaque black where they
  should be invisible.
- **4-bit alpha expands by 17, not 16.** DXT3 and palettized `alpha_depth=4`
  store a nibble; `v * 17` maps 0xF to 0xFF, whereas `v << 4` tops out at 0xF0
  and nothing is ever fully opaque.
- **1-bit alpha is packed LSB-first**, so pixel 0 is the low bit of byte 0.
- **Palette entries are BGRA**, and raw BGRA pixels need the same swizzle. Get
  it wrong and brown fur renders blue — which is exactly how it was caught.

## The CPU decoder is not the fast path

`dxt::decode` exists for tooling, tests, and the rare case that genuinely needs
pixels host-side. The renderer will not call it: `wgpu` uploads DXT blocks to
the GPU compressed and the hardware samples them directly, so decoding would
cost both time and four times the memory. `Blp::level` returns bytes exactly as
stored for that reason, and `decode_rgba` is the separate, slower path.

## Verification

`wow-cli blp survey` parses every texture in the install and tallies encodings
with an example path per row. `blp info` shows the mip chain and marks padded
levels; `blp export` writes a level to PNG, which is how each decode path was
checked by eye — a decoder can pass every size assertion and still produce
garbage.
