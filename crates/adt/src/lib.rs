//! Reader for terrain: `WDT` map definitions and `ADT` tiles.
//!
//! Written from the public format documentation at <https://wowdev.wiki/ADT>.
//!
//! A continent is a 64x64 grid of tiles. The `WDT` says which of those 4,096
//! tiles actually exist; each tile that does is a separate `ADT` file holding a
//! 16x16 grid of *chunks*, and each chunk carries a 145-value height field,
//! up to four texture layers, and references to the models placed on it.
//!
//! The awkward part is the height field. It is not a square grid: 145 is
//! `9x9 + 8x8`, two interleaved lattices where the inner one sits at the centre
//! of each outer cell. See [`Chunk::vertex_position`].

use chunk::{f32_at, string_at, strings, u16_at, u32_at, vec3_at, Chunks};

/// The version a 3.3.5a client ships.
pub const VERSION_WOTLK: u32 = 18;

/// Tiles along one edge of a map.
pub const TILES_PER_MAP: usize = 64;
/// Chunks along one edge of a tile.
pub const CHUNKS_PER_TILE: usize = 16;
/// Chunks in a tile.
pub const CHUNK_COUNT: usize = CHUNKS_PER_TILE * CHUNKS_PER_TILE;

/// World units across one tile. The map is 64 tiles wide, which puts the
/// origin at `32 * TILE_SIZE`.
pub const TILE_SIZE: f32 = 1600.0 / 3.0;
/// World units across one chunk.
pub const CHUNK_SIZE: f32 = TILE_SIZE / CHUNKS_PER_TILE as f32;
/// World units between adjacent height samples on the outer lattice.
pub const UNIT_SIZE: f32 = CHUNK_SIZE / 8.0;

/// Height samples per chunk: a 9x9 outer lattice plus an 8x8 inner one.
pub const HEIGHTS_PER_CHUNK: usize = 9 * 9 + 8 * 8;
/// Alpha maps are always this square, whatever their storage width.
pub const ALPHA_SIZE: usize = 64;

const MCNK_HEADER_SIZE: usize = 128;
const DOODAD_PLACEMENT_SIZE: usize = 36;
const OBJECT_PLACEMENT_SIZE: usize = 64;
const LAYER_SIZE: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a {expected}: expected an MVER chunk, found {found}")]
    WrongFormat {
        expected: &'static str,
        found: String,
    },
    #[error("unsupported version {got} (this reader targets {VERSION_WOTLK})")]
    UnsupportedVersion { got: u32 },
    #[error("missing required chunk {0}")]
    MissingChunk(&'static str),
    #[error("tile has {got} map chunks, expected {CHUNK_COUNT}")]
    WrongChunkCount { got: usize },
}

fn check_version(data: &[u8], expected: &'static str) -> Result<(), Error> {
    let Some((magic, payload)) = Chunks::new(data).next() else {
        return Err(Error::WrongFormat {
            expected,
            found: "<empty>".into(),
        });
    };
    if magic.0 != *b"MVER" {
        return Err(Error::WrongFormat {
            expected,
            found: magic.to_string(),
        });
    }
    let version = u32_at(payload, 0);
    if version != VERSION_WOTLK {
        return Err(Error::UnsupportedVersion { got: version });
    }
    Ok(())
}

/// A map definition: which tiles exist, and how their alpha maps are stored.
pub struct Wdt {
    pub flags: u32,
    /// `true` where a tile file exists. Indexed `[y][x]`, matching the
    /// `<map>_<x>_<y>.adt` naming.
    present: Vec<bool>,
}

impl Wdt {
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        check_version(data, "WDT")?;
        let mphd = Chunks::find(data, b"MPHD").ok_or(Error::MissingChunk("MPHD"))?;
        let main = Chunks::find(data, b"MAIN").ok_or(Error::MissingChunk("MAIN"))?;

        // MAIN is 64x64 entries of 8 bytes; bit 0 of the first word means the
        // tile has an ADT file.
        let present = (0..TILES_PER_MAP * TILES_PER_MAP)
            .map(|i| u32_at(main, i * 8) & 0x1 != 0)
            .collect();

        Ok(Self {
            flags: u32_at(mphd, 0),
            present,
        })
    }

    pub fn has_tile(&self, x: usize, y: usize) -> bool {
        if x >= TILES_PER_MAP || y >= TILES_PER_MAP {
            return false;
        }
        self.present[y * TILES_PER_MAP + x]
    }

    pub fn tile_count(&self) -> usize {
        self.present.iter().filter(|&&p| p).count()
    }

    /// Every existing tile as `(x, y)`.
    pub fn tiles(&self) -> Vec<(usize, usize)> {
        (0..TILES_PER_MAP)
            .flat_map(|y| (0..TILES_PER_MAP).map(move |x| (x, y)))
            .filter(|&(x, y)| self.has_tile(x, y))
            .collect()
    }

    /// Whether alpha maps are stored at 8 bits per texel rather than 4.
    ///
    /// This is a property of the *map*, not of the tile, so an ADT cannot be
    /// decoded correctly without reading its WDT first.
    pub fn big_alpha(&self) -> bool {
        self.flags & 0x4 != 0
    }
}

/// One texture layer of a chunk.
#[derive(Clone, Copy, Debug)]
pub struct Layer {
    /// Index into the tile's texture list.
    pub texture_id: u32,
    pub flags: u32,
    /// Byte offset into the chunk's `MCAL` payload.
    pub alpha_offset: u32,
    pub effect_id: u32,
}

impl Layer {
    /// Whether this layer's alpha map is run-length compressed.
    pub fn compressed_alpha(&self) -> bool {
        self.flags & 0x200 != 0
    }
    pub fn animated(&self) -> bool {
        self.flags & 0x40 != 0
    }
}

/// An M2 placed on the terrain.
#[derive(Clone, Debug)]
pub struct DoodadPlacement {
    pub path: String,
    pub unique_id: u32,
    pub position: [f32; 3],
    /// Euler angles in degrees.
    pub rotation: [f32; 3],
    pub scale: f32,
}

/// A WMO placed on the terrain.
#[derive(Clone, Debug)]
pub struct ObjectPlacement {
    pub path: String,
    pub unique_id: u32,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub bounds: ([f32; 3], [f32; 3]),
    pub doodad_set: u16,
    pub name_set: u16,
}

/// One 16th of a tile edge: a height field plus its texture layers.
pub struct Chunk {
    pub index: (u32, u32),
    /// World position of the chunk's origin corner.
    pub position: [f32; 3],
    pub area_id: u32,
    /// Bitmask of 4x4 sub-squares removed from the terrain, for doorways and
    /// cave mouths.
    pub holes: u16,
    /// Heights relative to `position[2]`, outer and inner lattices interleaved.
    pub heights: Vec<f32>,
    pub normals: Vec<[i8; 3]>,
    pub layers: Vec<Layer>,
    /// One 64x64 map per layer after the first; layer 0 is always opaque.
    pub alpha_maps: Vec<Vec<u8>>,
    pub doodad_refs: Vec<u32>,
    pub object_refs: Vec<u32>,
}

impl Chunk {
    /// Whether a 4x4 sub-square has been punched out.
    pub fn has_hole(&self, x: usize, y: usize) -> bool {
        if x >= 4 || y >= 4 {
            return false;
        }
        self.holes & (1 << (y * 4 + x)) != 0
    }

    /// Position of one height sample, in world space.
    ///
    /// The 145 samples are two interleaved lattices stored as alternating rows:
    /// nine outer samples, then eight inner ones offset half a unit in both
    /// axes, nine times over. Treating them as a flat 9x9 grid -- or forgetting
    /// the half-unit offset -- produces terrain that looks nearly right and has
    /// a rippled surface.
    ///
    /// Both axes run *negative* from the chunk origin, which is how the game's
    /// north-west-origin tile grid maps onto its coordinate system.
    pub fn vertex_position(&self, index: usize) -> [f32; 3] {
        let (row, col, inner) = lattice_coords(index);
        // `row` already counts half-steps: seventeen rows span eight units, so
        // an inner row's half-unit offset is *in the row index*. Adding another
        // half here double-counts it and shears the surface.
        let column_offset = if inner { 0.5 } else { 0.0 };
        [
            self.position[0] - row as f32 * 0.5 * UNIT_SIZE,
            self.position[1] - (col as f32 + column_offset) * UNIT_SIZE,
            self.position[2] + self.heights.get(index).copied().unwrap_or(0.0),
        ]
    }

    /// Height samples along one edge, for checking that neighbouring chunks
    /// agree.
    pub fn edge_heights(&self, edge: Edge) -> Vec<f32> {
        (0..9)
            .map(|i| {
                let index = match edge {
                    Edge::North => i,
                    Edge::South => 8 * 17 + i,
                    Edge::West => i * 17,
                    Edge::East => i * 17 + 8,
                };
                self.heights.get(index).copied().unwrap_or(f32::NAN)
            })
            .collect()
    }
}

/// Which side of a chunk an edge is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    North,
    South,
    West,
    East,
}

/// Splits a height index into its lattice coordinates.
///
/// Rows alternate: row 0 holds nine outer samples at indices 0..9, row 1 holds
/// eight inner samples at 9..17, and so on for seventeen rows.
pub fn lattice_coords(index: usize) -> (usize, usize, bool) {
    let pair = index / 17;
    let within = index % 17;
    if within < 9 {
        (pair * 2, within, false)
    } else {
        (pair * 2 + 1, within - 9, true)
    }
}

/// One terrain tile.
pub struct Adt {
    pub textures: Vec<String>,
    pub doodad_models: Vec<String>,
    pub object_models: Vec<String>,
    pub doodads: Vec<DoodadPlacement>,
    pub objects: Vec<ObjectPlacement>,
    pub chunks: Vec<Chunk>,
}

impl Adt {
    /// Parses a tile. `big_alpha` comes from the map's WDT and decides how
    /// alpha maps are stored.
    pub fn parse(data: &[u8], big_alpha: bool) -> Result<Self, Error> {
        check_version(data, "ADT")?;

        let textures = Chunks::find(data, b"MTEX")
            .map(|b| strings(b).into_iter().map(str::to_string).collect())
            .unwrap_or_default();
        let doodad_models: Vec<String> = Chunks::find(data, b"MMDX")
            .map(|b| strings(b).into_iter().map(str::to_string).collect())
            .unwrap_or_default();
        let object_models: Vec<String> = Chunks::find(data, b"MWMO")
            .map(|b| strings(b).into_iter().map(str::to_string).collect())
            .unwrap_or_default();

        // Placements name their model by byte offset into the name block, via
        // a table of offsets -- not by ordinal.
        let mmdx = Chunks::find(data, b"MMDX").unwrap_or(&[]);
        let mmid = Chunks::find(data, b"MMID").unwrap_or(&[]);
        let mwmo = Chunks::find(data, b"MWMO").unwrap_or(&[]);
        let mwid = Chunks::find(data, b"MWID").unwrap_or(&[]);

        let name_of = |block: &[u8], offsets: &[u8], id: u32| -> String {
            let offset = u32_at(offsets, id as usize * 4) as usize;
            string_at(block, offset).to_string()
        };

        let doodads = Chunks::find(data, b"MDDF")
            .unwrap_or(&[])
            .chunks_exact(DOODAD_PLACEMENT_SIZE)
            .map(|d| DoodadPlacement {
                path: name_of(mmdx, mmid, u32_at(d, 0)),
                unique_id: u32_at(d, 4),
                position: vec3_at(d, 8),
                rotation: vec3_at(d, 20),
                // Stored as a fixed-point value where 1024 means 1.0.
                scale: u16_at(d, 32) as f32 / 1024.0,
            })
            .collect();

        let objects = Chunks::find(data, b"MODF")
            .unwrap_or(&[])
            .chunks_exact(OBJECT_PLACEMENT_SIZE)
            .map(|d| ObjectPlacement {
                path: name_of(mwmo, mwid, u32_at(d, 0)),
                unique_id: u32_at(d, 4),
                position: vec3_at(d, 8),
                rotation: vec3_at(d, 20),
                bounds: (vec3_at(d, 32), vec3_at(d, 44)),
                doodad_set: u16_at(d, 58),
                name_set: u16_at(d, 60),
            })
            .collect();

        let chunks: Vec<Chunk> = Chunks::new(data)
            .filter(|(magic, _)| magic.0 == *b"MCNK")
            .map(|(_, payload)| parse_chunk(payload, big_alpha))
            .collect();

        if chunks.len() != CHUNK_COUNT {
            return Err(Error::WrongChunkCount { got: chunks.len() });
        }

        Ok(Self {
            textures,
            doodad_models,
            object_models,
            doodads,
            objects,
            chunks,
        })
    }

    pub fn chunk(&self, x: usize, y: usize) -> Option<&Chunk> {
        self.chunks.get(y * CHUNKS_PER_TILE + x)
    }

    /// Checks that neighbouring chunks agree along shared edges.
    ///
    /// Terrain chunks tile exactly, so a seam means the height field is being
    /// read with the wrong stride or ordering -- an error that otherwise looks
    /// like plausible landscape.
    pub fn validate(&self) -> Result<(), String> {
        for y in 0..CHUNKS_PER_TILE {
            for x in 0..CHUNKS_PER_TILE {
                let Some(here) = self.chunk(x, y) else {
                    return Err(format!("chunk {x},{y} missing"));
                };
                if here.heights.len() != HEIGHTS_PER_CHUNK {
                    return Err(format!(
                        "chunk {x},{y} has {} heights, expected {HEIGHTS_PER_CHUNK}",
                        here.heights.len()
                    ));
                }
                if here.heights.iter().any(|h| !h.is_finite()) {
                    return Err(format!("chunk {x},{y} has non-finite heights"));
                }

                // Compare absolute heights, since each chunk stores its own
                // base in `position`.
                if let Some(east) = self.chunk(x + 1, y).filter(|_| x + 1 < CHUNKS_PER_TILE) {
                    let a: Vec<f32> = here
                        .edge_heights(Edge::East)
                        .iter()
                        .map(|h| h + here.position[2])
                        .collect();
                    let b: Vec<f32> = east
                        .edge_heights(Edge::West)
                        .iter()
                        .map(|h| h + east.position[2])
                        .collect();
                    if a.iter().zip(&b).any(|(x, y)| (x - y).abs() > 0.05) {
                        return Err(format!(
                            "chunk {x},{y} east edge does not meet its neighbour: \
                             {a:?} vs {b:?}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

fn parse_chunk(payload: &[u8], big_alpha: bool) -> Chunk {
    let header = &payload[..MCNK_HEADER_SIZE.min(payload.len())];
    let layer_count = u32_at(header, 0x0C) as usize;
    let doodad_ref_count = u32_at(header, 0x10) as usize;
    let object_ref_count = u32_at(header, 0x38) as usize;
    let alpha_size = u32_at(header, 0x28) as usize;

    // Sub-chunk offsets are relative to the start of the MCNK *chunk header*,
    // which is 8 bytes before this payload.
    let at = |offset: u32| -> &[u8] {
        let start = (offset as usize).saturating_sub(8);
        payload.get(start..).unwrap_or(&[])
    };
    let sub = |offset: u32, magic: &[u8; 4]| -> &[u8] {
        if offset == 0 {
            return &[];
        }
        let region = at(offset);
        Chunks::new(region)
            .next()
            .filter(|(m, _)| m.0 == *magic)
            .map(|(_, p)| p)
            .unwrap_or(&[])
    };

    let mcvt = sub(u32_at(header, 0x14), b"MCVT");
    let heights: Vec<f32> = (0..HEIGHTS_PER_CHUNK).map(|i| f32_at(mcvt, i * 4)).collect();

    let mcnr = sub(u32_at(header, 0x18), b"MCNR");
    let normals: Vec<[i8; 3]> = (0..HEIGHTS_PER_CHUNK)
        .map(|i| {
            let o = i * 3;
            [
                mcnr.get(o).map_or(0, |&b| b as i8),
                mcnr.get(o + 1).map_or(0, |&b| b as i8),
                mcnr.get(o + 2).map_or(127, |&b| b as i8),
            ]
        })
        .collect();

    let mcly = sub(u32_at(header, 0x1C), b"MCLY");
    let layers: Vec<Layer> = mcly
        .chunks_exact(LAYER_SIZE)
        .take(layer_count)
        .map(|l| Layer {
            texture_id: u32_at(l, 0),
            flags: u32_at(l, 4),
            alpha_offset: u32_at(l, 8),
            effect_id: u32_at(l, 12),
        })
        .collect();

    let mcal = sub(u32_at(header, 0x24), b"MCAL");
    let mcal = if mcal.len() >= alpha_size {
        &mcal[..alpha_size]
    } else {
        mcal
    };
    let do_not_fix = u32_at(header, 0x00) & 0x8000 != 0;
    let alpha_maps: Vec<Vec<u8>> = layers
        .iter()
        .skip(1)
        .map(|layer| decode_alpha(mcal, layer, big_alpha, do_not_fix))
        .collect();

    let mcrf = sub(u32_at(header, 0x20), b"MCRF");
    let refs: Vec<u32> = mcrf.chunks_exact(4).map(|c| u32_at(c, 0)).collect();
    let (doodad_refs, object_refs) = refs.split_at(doodad_ref_count.min(refs.len()));

    Chunk {
        index: (u32_at(header, 0x04), u32_at(header, 0x08)),
        position: vec3_at(header, 0x68),
        area_id: u32_at(header, 0x34),
        holes: u16_at(header, 0x3C),
        heights,
        normals,
        layers,
        alpha_maps,
        doodad_refs: doodad_refs.to_vec(),
        object_refs: object_refs
            .iter()
            .take(object_ref_count)
            .copied()
            .collect(),
    }
}

/// Expands one layer's alpha map to a full 64x64 byte map.
///
/// Three storage forms exist and the choice is not local to the layer: 8-bit
/// depends on a flag in the *map's* WDT, and 4-bit maps additionally need their
/// last row and column repaired unless the chunk opts out.
pub fn decode_alpha(mcal: &[u8], layer: &Layer, big_alpha: bool, do_not_fix: bool) -> Vec<u8> {
    let start = layer.alpha_offset as usize;
    let data = mcal.get(start..).unwrap_or(&[]);
    let mut out = vec![0u8; ALPHA_SIZE * ALPHA_SIZE];

    if layer.compressed_alpha() {
        // Run-length: a control byte's high bit selects fill or copy, and its
        // low seven bits give the count.
        let mut src = 0usize;
        let mut dst = 0usize;
        while dst < out.len() && src < data.len() {
            let control = data[src];
            src += 1;
            let count = (control & 0x7F) as usize;
            let fill = control & 0x80 != 0;
            if fill {
                let Some(&value) = data.get(src) else { break };
                src += 1;
                let end = (dst + count).min(out.len());
                out[dst..end].fill(value);
                dst = end;
            } else {
                let end = (dst + count).min(out.len());
                let available = data.len().saturating_sub(src);
                let take = (end - dst).min(available);
                out[dst..dst + take].copy_from_slice(&data[src..src + take]);
                src += take;
                dst += take;
            }
        }
    } else if big_alpha {
        let take = out.len().min(data.len());
        out[..take].copy_from_slice(&data[..take]);
    } else {
        // 4 bits per texel, two texels per byte, low nibble first. Scaled by 17
        // so 0xF reaches 255.
        for i in 0..out.len() {
            let byte = data.get(i / 2).copied().unwrap_or(0);
            let nibble = if i % 2 == 0 { byte & 0x0F } else { byte >> 4 };
            out[i] = nibble * 17;
        }
        if !do_not_fix {
            // The 4-bit form really encodes 63x63: the last row and column are
            // absent and must be repeated, or every chunk gets a dark seam
            // along two of its edges.
            for row in 0..ALPHA_SIZE {
                out[row * ALPHA_SIZE + 63] = out[row * ALPHA_SIZE + 62];
            }
            for col in 0..ALPHA_SIZE {
                out[63 * ALPHA_SIZE + col] = out[62 * ALPHA_SIZE + col];
            }
        }
    }
    out
}

/// Path of a tile within a map directory.
pub fn tile_path(map: &str, x: usize, y: usize) -> String {
    format!(r"World\Maps\{map}\{map}_{x}_{y}.adt")
}

/// Path of a map's WDT.
pub fn wdt_path(map: &str) -> String {
    format!(r"World\Maps\{map}\{map}.wdt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_map_paths() {
        assert_eq!(wdt_path("Azeroth"), r"World\Maps\Azeroth\Azeroth.wdt");
        assert_eq!(
            tile_path("Azeroth", 32, 48),
            r"World\Maps\Azeroth\Azeroth_32_48.adt"
        );
    }

    /// The height field is two interleaved lattices, not a square grid.
    #[test]
    fn lattice_coordinates_alternate_rows() {
        assert_eq!(lattice_coords(0), (0, 0, false));
        assert_eq!(lattice_coords(8), (0, 8, false));
        // Index 9 begins the inner row.
        assert_eq!(lattice_coords(9), (1, 0, true));
        assert_eq!(lattice_coords(16), (1, 7, true));
        assert_eq!(lattice_coords(17), (2, 0, false));
        // The last sample is on the ninth outer row.
        assert_eq!(lattice_coords(HEIGHTS_PER_CHUNK - 1), (16, 8, false));
    }

    #[test]
    fn counts_add_up() {
        assert_eq!(HEIGHTS_PER_CHUNK, 145);
        // Seventeen alternating rows of 9 and 8.
        let total: usize = (0..17).map(|r| if r % 2 == 0 { 9 } else { 8 }).sum();
        assert_eq!(total, HEIGHTS_PER_CHUNK);
        assert!((CHUNK_SIZE - 33.333332).abs() < 1e-4);
        assert!((UNIT_SIZE - 4.1666665).abs() < 1e-4);
    }

    #[test]
    fn four_bit_alpha_expands_and_repairs_edges() {
        let layer = Layer {
            texture_id: 0,
            flags: 0,
            alpha_offset: 0,
            effect_id: 0,
        };
        // Every nibble 0xF: fully opaque everywhere.
        let mcal = vec![0xFFu8; 2048];
        let map = decode_alpha(&mcal, &layer, false, false);
        assert_eq!(map.len(), ALPHA_SIZE * ALPHA_SIZE);
        assert!(map.iter().all(|&v| v == 255));
    }

    /// Without the repair the final row and column stay at whatever the 4-bit
    /// packing left there; with it they copy their neighbour.
    #[test]
    fn edge_repair_copies_the_penultimate_row() {
        let layer = Layer {
            texture_id: 0,
            flags: 0,
            alpha_offset: 0,
            effect_id: 0,
        };
        // Alternating nibbles produce a visible pattern to copy.
        let mcal: Vec<u8> = (0..2048).map(|i| if i % 2 == 0 { 0xF0 } else { 0x0F }).collect();
        let fixed = decode_alpha(&mcal, &layer, false, false);
        for row in 0..ALPHA_SIZE {
            assert_eq!(fixed[row * ALPHA_SIZE + 63], fixed[row * ALPHA_SIZE + 62]);
        }
        for col in 0..ALPHA_SIZE {
            assert_eq!(fixed[63 * ALPHA_SIZE + col], fixed[62 * ALPHA_SIZE + col]);
        }
    }

    #[test]
    fn compressed_alpha_expands_fills_and_copies() {
        let layer = Layer {
            texture_id: 0,
            flags: 0x200,
            alpha_offset: 0,
            effect_id: 0,
        };
        let mut mcal = Vec::new();
        // Fill 100 bytes with 0x40.
        mcal.push(0x80 | 100);
        mcal.push(0x40);
        // Copy three literal bytes.
        mcal.push(3);
        mcal.extend_from_slice(&[1, 2, 3]);
        // Fill the remainder.
        let remaining = ALPHA_SIZE * ALPHA_SIZE - 103;
        for _ in 0..remaining.div_ceil(127) {
            mcal.push(0x80 | 127);
            mcal.push(0xFF);
        }

        let map = decode_alpha(&mcal, &layer, false, false);
        assert!(map[..100].iter().all(|&v| v == 0x40));
        assert_eq!(&map[100..103], &[1, 2, 3]);
        assert_eq!(map[200], 0xFF);
    }

    #[test]
    fn holes_are_a_four_by_four_bitmask() {
        let chunk = Chunk {
            index: (0, 0),
            position: [0.0; 3],
            area_id: 0,
            holes: 0b0000_0000_0000_0011,
            heights: vec![0.0; HEIGHTS_PER_CHUNK],
            normals: vec![[0, 0, 127]; HEIGHTS_PER_CHUNK],
            layers: Vec::new(),
            alpha_maps: Vec::new(),
            doodad_refs: Vec::new(),
            object_refs: Vec::new(),
        };
        assert!(chunk.has_hole(0, 0));
        assert!(chunk.has_hole(1, 0));
        assert!(!chunk.has_hole(2, 0));
        assert!(!chunk.has_hole(0, 1));
    }
}
