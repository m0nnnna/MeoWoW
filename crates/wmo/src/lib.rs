//! Reader for WMO objects: buildings, dungeon interiors, bridges, city blocks.
//!
//! Written from the public format documentation at <https://wowdev.wiki/WMO>.
//!
//! Unlike M2, WMO is a **chunked** format -- a flat sequence of
//! `(magic, size, payload)` records. That makes it forgiving to read: unknown
//! chunks are skipped rather than shifting everything after them.
//!
//! Two traps sit at the very front of it:
//!
//! - **Chunk magics are stored reversed.** `MVER` appears in the file as
//!   `REVM`. Every identifier here is un-reversed on read, so callers see the
//!   documented name.
//! - **A WMO is more than one file.** The root holds materials, doodads and
//!   portals; the geometry lives in numbered group files beside it, and
//!   `Foo.wmo` is accompanied by `Foo_000.wmo` onwards. See [`group`].

pub mod group;

pub use group::{Group, Liquid};

/// The version a 3.3.5a client ships.
pub const VERSION_WOTLK: u32 = 17;

const MATERIAL_SIZE: usize = 64;
const GROUP_INFO_SIZE: usize = 32;
const DOODAD_SET_SIZE: usize = 32;
const DOODAD_DEF_SIZE: usize = 40;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a WMO: expected an MVER chunk, found {0:?}")]
    NotAWmo(String),
    #[error("unsupported WMO version {got} (this reader targets {VERSION_WOTLK})")]
    UnsupportedVersion { got: u32 },
    #[error("missing required chunk {0}")]
    MissingChunk(&'static str),
    #[error("chunk {magic} claims {size} bytes but only {left} remain")]
    TruncatedChunk {
        magic: String,
        size: usize,
        left: usize,
    },
}

// The chunked container itself is shared with ADT; see `crates/chunk`. These
// re-exports keep `wmo::Chunks` working for callers.
pub use chunk::{Chunks, Magic};
pub(crate) use chunk::{f32_at, string_at, u32_at, vec3_at};

/// Confirms a file is a WMO of the expected version.
pub(crate) fn check_version(data: &[u8]) -> Result<(), Error> {
    let Some((magic, payload)) = Chunks::new(data).next() else {
        return Err(Error::NotAWmo("<empty>".into()));
    };
    if magic.0 != *b"MVER" {
        return Err(Error::NotAWmo(magic.to_string()));
    }
    let version = u32_at(payload, 0);
    if version != VERSION_WOTLK {
        return Err(Error::UnsupportedVersion { got: version });
    }
    Ok(())
}

/// Header counts from `MOHD`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Header {
    pub texture_count: u32,
    pub group_count: u32,
    pub portal_count: u32,
    pub light_count: u32,
    pub doodad_name_count: u32,
    pub doodad_def_count: u32,
    pub doodad_set_count: u32,
    /// Ambient light, applied to interior groups that do not carry vertex
    /// colours of their own.
    pub ambient_color: [u8; 4],
    pub wmo_id: u32,
    pub bounding_box: ([f32; 3], [f32; 3]),
    pub flags: u32,
}

/// Surface properties for a batch.
#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub flags: u32,
    pub shader: u32,
    pub blend_mode: u32,
    /// Byte offset into the texture name block.
    pub texture1: u32,
    pub texture2: u32,
    pub diffuse_color: u32,
    /// What this surface is made of, as a `TerrainType` **row id** -- the same
    /// currency `GroundEffectTexture` uses for the ground outside.
    ///
    /// **Measured, and it inverts the convention of the outdoor table.** Out
    /// on the terrain, row 0 (`Dirt`) is what a texture says when it says
    /// nothing, and 22,708 of 24,981 rows carry it. In here the "says nothing"
    /// value is row **10 (`None`)**, which 22,893 of 25,034 materials carry,
    /// while row 0 is a rare and genuine `Dirt` -- 118 materials, whose
    /// textures are called `dirt` sixteen times as often as `None`'s are. Two
    /// tables, one column meaning, opposite conventions for silence.
    ///
    /// Identified the way every column here is: by the **names of the texture
    /// files** the materials carrying it are painted with, scored against row
    /// 10 as the baseline, because rock and wood turn up everywhere and a raw
    /// share proves nothing. Every value with a material word of its own comes
    /// back enriched -- `Sand` x469, `Grass` x305, `Dirt` x15.9, `Snow` x12.1,
    /// `Wood` x7.1, `Metallic` x6.6, `Stone` x2.0 -- over all 1,985 root WMOs.
    /// `Leaves` (2 materials) and `DustyGrass` (6) are too small to vote and
    /// are not claimed. `wow-cli wmo footing` is that measurement.
    ///
    /// `Stone` is the weak one at x2.0, and for a readable reason: a stone
    /// floor is usually filed under `rock` rather than `stone`.
    pub ground_type: u32,
}

impl Material {
    /// Lit from the group's vertex colours rather than the scene light.
    pub fn unlit(&self) -> bool {
        self.flags & 0x01 != 0
    }
    pub fn two_sided(&self) -> bool {
        self.flags & 0x04 != 0
    }
    /// Texture coordinates clamp instead of repeating.
    pub fn clamp_s(&self) -> bool {
        self.flags & 0x08 != 0
    }
    pub fn clamp_t(&self) -> bool {
        self.flags & 0x10 != 0
    }
}

/// Per-group metadata held by the root, before the group file is opened.
#[derive(Clone, Debug)]
pub struct GroupInfo {
    pub flags: u32,
    pub bounding_box: ([f32; 3], [f32; 3]),
    pub name: String,
}

impl GroupInfo {
    /// Whether the group is enclosed. Interiors use the WMO's own lighting
    /// rather than the outdoor sun.
    pub fn is_interior(&self) -> bool {
        self.flags & 0x2000 != 0
    }
    pub fn has_vertex_colors(&self) -> bool {
        self.flags & 0x04 != 0
    }
}

/// A named selection of doodads. Only one set is active at a time, which is
/// how one building ships furnished and empty variants.
#[derive(Clone, Debug)]
pub struct DoodadSet {
    pub name: String,
    pub start: u32,
    pub count: u32,
}

/// One placed doodad: an M2 positioned inside the WMO's local space.
#[derive(Clone, Debug)]
pub struct DoodadDef {
    /// Model path, already resolved through the name block.
    pub path: String,
    pub position: [f32; 3],
    /// Stored x, y, z, w.
    pub rotation: [f32; 4],
    pub scale: f32,
    pub color: [u8; 4],
}

/// The root `.wmo` file.
pub struct Root {
    pub header: Header,
    pub materials: Vec<Material>,
    pub groups: Vec<GroupInfo>,
    pub doodad_sets: Vec<DoodadSet>,
    pub doodads: Vec<DoodadDef>,
    texture_names: Vec<u8>,
}

impl Root {
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        check_version(data)?;

        let mohd = Chunks::find(data, b"MOHD").ok_or(Error::MissingChunk("MOHD"))?;
        let header = Header {
            texture_count: u32_at(mohd, 0),
            group_count: u32_at(mohd, 4),
            portal_count: u32_at(mohd, 8),
            light_count: u32_at(mohd, 12),
            doodad_name_count: u32_at(mohd, 16),
            doodad_def_count: u32_at(mohd, 20),
            doodad_set_count: u32_at(mohd, 24),
            ambient_color: mohd
                .get(28..32)
                .map(|c| [c[0], c[1], c[2], c[3]])
                .unwrap_or_default(),
            wmo_id: u32_at(mohd, 32),
            bounding_box: (vec3_at(mohd, 36), vec3_at(mohd, 48)),
            flags: u32_at(mohd, 60),
        };

        let texture_names = Chunks::find(data, b"MOTX").unwrap_or(&[]).to_vec();

        let materials = Chunks::find(data, b"MOMT")
            .unwrap_or(&[])
            .chunks_exact(MATERIAL_SIZE)
            .map(|m| Material {
                flags: u32_at(m, 0),
                shader: u32_at(m, 4),
                blend_mode: u32_at(m, 8),
                texture1: u32_at(m, 12),
                texture2: u32_at(m, 24),
                diffuse_color: u32_at(m, 28),
                ground_type: u32_at(m, 32),
            })
            .collect();

        // Group names are indexed by byte offset, as everywhere else here.
        let group_names = Chunks::find(data, b"MOGN").unwrap_or(&[]);
        let groups = Chunks::find(data, b"MOGI")
            .unwrap_or(&[])
            .chunks_exact(GROUP_INFO_SIZE)
            .map(|g| {
                let offset = u32_at(g, 28) as i32;
                GroupInfo {
                    flags: u32_at(g, 0),
                    bounding_box: (vec3_at(g, 4), vec3_at(g, 16)),
                    // -1 means the group is unnamed.
                    name: if offset >= 0 {
                        string_at(group_names, offset as usize).to_string()
                    } else {
                        String::new()
                    },
                }
            })
            .collect();

        let doodad_sets = Chunks::find(data, b"MODS")
            .unwrap_or(&[])
            .chunks_exact(DOODAD_SET_SIZE)
            .map(|d| DoodadSet {
                // Fixed 20-byte field, NUL-padded rather than terminated.
                name: string_at(&d[..20], 0).to_string(),
                start: u32_at(d, 20),
                count: u32_at(d, 24),
            })
            .collect();

        let doodad_names = Chunks::find(data, b"MODN").unwrap_or(&[]);
        let doodads = Chunks::find(data, b"MODD")
            .unwrap_or(&[])
            .chunks_exact(DOODAD_DEF_SIZE)
            .map(|d| {
                // The name offset shares its word with a flags byte.
                let name_offset = u32_at(d, 0) & 0x00FF_FFFF;
                DoodadDef {
                    path: string_at(doodad_names, name_offset as usize).to_string(),
                    position: vec3_at(d, 4),
                    rotation: [
                        f32_at(d, 16),
                        f32_at(d, 20),
                        f32_at(d, 24),
                        f32_at(d, 28),
                    ],
                    scale: f32_at(d, 32),
                    color: [d[36], d[37], d[38], d[39]],
                }
            })
            .collect();

        Ok(Self {
            header,
            materials,
            groups,
            doodad_sets,
            doodads,
            texture_names,
        })
    }

    /// Resolves a material's texture slot to a path.
    pub fn texture(&self, offset: u32) -> &str {
        string_at(&self.texture_names, offset as usize)
    }

    /// Every distinct texture the object references.
    pub fn textures(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .materials
            .iter()
            .flat_map(|m| [self.texture(m.texture1), self.texture(m.texture2)])
            .filter(|s| !s.is_empty())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Doodads belonging to one set.
    pub fn doodads_in_set(&self, set: &DoodadSet) -> &[DoodadDef] {
        let start = (set.start as usize).min(self.doodads.len());
        let end = (start + set.count as usize).min(self.doodads.len());
        &self.doodads[start..end]
    }
}

/// Path of a WMO's numbered group file.
///
/// `Foo.wmo` is accompanied by `Foo_000.wmo`, `Foo_001.wmo`, and so on --
/// always three digits.
pub fn group_path(root_path: &str, index: usize) -> String {
    let stem = root_path
        .strip_suffix(".wmo")
        .or_else(|| root_path.strip_suffix(".WMO"))
        .unwrap_or(root_path);
    format!("{stem}_{index:03}.wmo")
}

/// Whether a path names a group file rather than a root.
///
/// Group files sit beside their root in the archive and parse as WMOs, so
/// anything walking the listfile has to tell them apart or it will try to load
/// each building's parts as separate buildings.
pub fn is_group_path(path: &str) -> bool {
    let stem = path
        .strip_suffix(".wmo")
        .or_else(|| path.strip_suffix(".WMO"))
        .unwrap_or(path);
    match stem.len().checked_sub(4) {
        Some(cut) => {
            let tail = &stem[cut..];
            tail.starts_with('_') && tail[1..].chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a chunked file, writing magics reversed as the format does.
    fn chunked(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (magic, payload) in chunks {
            let m = *magic;
            out.extend_from_slice(&[m[3], m[2], m[1], m[0]]);
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(payload);
        }
        out
    }

    #[test]
    fn reads_reversed_magics() {
        let data = chunked(&[(b"MVER", 17u32.to_le_bytes().to_vec())]);
        // The bytes on disk really are backwards.
        assert_eq!(&data[..4], b"REVM");
        let (magic, payload) = Chunks::new(&data).next().unwrap();
        assert_eq!(magic.as_str(), "MVER");
        assert_eq!(u32_at(payload, 0), 17);
    }

    #[test]
    fn skips_unknown_chunks() {
        let data = chunked(&[
            (b"MVER", 17u32.to_le_bytes().to_vec()),
            (b"ZZZZ", vec![1, 2, 3, 4, 5, 6, 7, 8]),
            (b"MOHD", vec![0u8; 64]),
        ]);
        assert!(Chunks::find(&data, b"MOHD").is_some());
        assert_eq!(Chunks::new(&data).count(), 3);
    }

    /// A truncated tail must end iteration rather than panic.
    #[test]
    fn stops_at_a_truncated_chunk() {
        let mut data = chunked(&[
            (b"MVER", 17u32.to_le_bytes().to_vec()),
            (b"MOHD", vec![0u8; 64]),
        ]);
        data.truncate(data.len() - 20);
        assert_eq!(Chunks::new(&data).count(), 1);
    }

    #[test]
    fn rejects_other_versions() {
        let data = chunked(&[(b"MVER", 14u32.to_le_bytes().to_vec())]);
        assert!(matches!(
            Root::parse(&data),
            Err(Error::UnsupportedVersion { got: 14 })
        ));
    }

    #[test]
    fn rejects_files_that_do_not_start_with_mver() {
        let data = chunked(&[(b"MOHD", vec![0u8; 64])]);
        assert!(matches!(Root::parse(&data), Err(Error::NotAWmo(_))));
    }

    #[test]
    fn derives_group_paths() {
        assert_eq!(
            group_path(r"World\wmo\Azeroth\Buildings\Foo.wmo", 0),
            r"World\wmo\Azeroth\Buildings\Foo_000.wmo"
        );
        assert_eq!(group_path("Foo.wmo", 42), "Foo_042.wmo");
    }

    #[test]
    fn distinguishes_group_files_from_roots() {
        assert!(is_group_path("Foo_000.wmo"));
        assert!(is_group_path(r"a\b\Cathedral_017.WMO"));
        assert!(!is_group_path("Foo.wmo"));
        // A name that merely ends in digits is not a group.
        assert!(!is_group_path("Building01.wmo"));
        assert!(!is_group_path("Foo_00.wmo"));
    }

    #[test]
    fn reads_strings_by_byte_offset() {
        let block = b"first\0second\0";
        assert_eq!(string_at(block, 0), "first");
        assert_eq!(string_at(block, 6), "second");
        assert_eq!(string_at(block, 999), "");
    }

    /// A group's `MLIQ` surface, off a known-good synthetic body shaped like
    /// the Stormwind fountain's: a small grid, all cells wet, one flat height.
    #[test]
    fn a_group_liquid_grid_parses() {
        let mut header = vec![0u8; 68];
        header[0..4].copy_from_slice(&(-1i32).to_le_bytes()); // no name
        header[4..8].copy_from_slice(&(-1i32).to_le_bytes());
        header[52..56].copy_from_slice(&0u32.to_le_bytes()); // groupLiquid: water

        // MOVT so the group has geometry and `validate` has something to check.
        let mut movt = Vec::new();
        for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in v {
                movt.extend_from_slice(&c.to_le_bytes());
            }
        }

        // MLIQ: 3x3 verts, 2x2 tiles, corner (10,20,5), every height 5.0,
        // every tile 0x40 (wet).
        let mut mliq = Vec::new();
        mliq.extend_from_slice(&3u32.to_le_bytes());
        mliq.extend_from_slice(&3u32.to_le_bytes());
        mliq.extend_from_slice(&2u32.to_le_bytes());
        mliq.extend_from_slice(&2u32.to_le_bytes());
        for c in [10.0f32, 20.0, 5.0] {
            mliq.extend_from_slice(&c.to_le_bytes());
        }
        mliq.extend_from_slice(&0u16.to_le_bytes()); // material
        for _ in 0..9 {
            mliq.extend_from_slice(&[0, 0, 0, 0]); // flow/light, unread
            mliq.extend_from_slice(&5.0f32.to_le_bytes()); // height
        }
        mliq.extend_from_slice(&[0x40, 0x40, 0x40, 0x0F]); // last tile dry

        let mut mogp = header;
        for (magic, payload) in [(b"MOVT", movt), (b"MLIQ", mliq)] {
            mogp.extend_from_slice(&[magic[3], magic[2], magic[1], magic[0]]);
            mogp.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            mogp.extend_from_slice(&payload);
        }

        let file = chunked(&[
            (b"MVER", 17u32.to_le_bytes().to_vec()),
            (b"MOGP", mogp),
        ]);
        let group = group::Group::parse(&file, &[]).unwrap();

        assert_eq!(group.group_liquid, 0);
        assert!(group.has_liquid());
        let liquid = group.liquid.as_ref().unwrap();
        assert_eq!((liquid.verts_x, liquid.verts_y), (3, 3));
        assert_eq!((liquid.tiles_x, liquid.tiles_y), (2, 2));
        assert_eq!(liquid.corner, [10.0, 20.0, 5.0]);
        assert_eq!(liquid.height(2, 2), 5.0);
        assert!(liquid.cell_wet(0, 0));
        assert!(liquid.cell_wet(1, 0));
        assert!(!liquid.cell_wet(1, 1), "0x0F is the dry sentinel");
        assert!(!liquid.cell_wet(9, 9), "out of range is dry, not a panic");

        // `groupLiquid` 0 -> take the type off the first wet tile's low
        // nibble (`0x40 & 0xF == 0`), `+1`, convert -> `LiquidType.dbc` 13,
        // "WMO Water".
        assert_eq!(group.liquid_type(), Some(13));

        // A body one byte short of its announced grid is dropped, not read as
        // zeroes.
        let short = &file[..file.len() - 1];
        assert!(group::Group::parse(short, &[])
            .map(|g| g.liquid.is_none())
            .unwrap_or(true));
    }

    /// The legacy `groupLiquid` conversion, the same one every reference map
    /// extractor runs. Not a DBC id: `15` is "no liquid", everything else is
    /// `+1`'d and mapped by its low bits.
    #[test]
    fn a_legacy_group_liquid_index_converts_to_a_dbc_row() {
        // Header only, no chunks -- `liquid_type` reads `MLIQ` from `liquid`,
        // so build one directly.
        let with = |group_liquid: u32, flags: u32, tiles: Vec<u8>| group::Group {
            flags,
            bounding_box: ([0.0; 3], [0.0; 3]),
            name: String::new(),
            descriptive_name: String::new(),
            group_id: 0,
            portal_start: 0,
            portal_count: 0,
            batch_counts: Default::default(),
            vertices: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            triangle_materials: Vec::new(),
            batches: Vec::new(),
            vertex_colors: Vec::new(),
            doodad_refs: Vec::new(),
            group_liquid,
            liquid: Some(group::Liquid {
                verts_x: 2,
                verts_y: 2,
                tiles_x: 1,
                tiles_y: 1,
                corner: [0.0; 3],
                heights: vec![0.0; 4],
                tiles,
            }),
        };

        // Stormwind's canals: `5` -> `+1` -> `6` -> `(6-1)&3 == 1` -> row 14,
        // "WMO Ocean".
        assert_eq!(with(5, 0, vec![0x40]).liquid_type(), Some(14));
        // Northshire's fountain: header says `15` (no liquid), so the type
        // comes off the tile -- low nibble `4`, `+1` -> `5`, `(5-1)&3 == 0`
        // -> row 13, "WMO Water".
        assert_eq!(with(15, 0, vec![0x44]).liquid_type(), Some(13));
        // The `0x80000` group flag turns that fresh water into "WMO Ocean".
        assert_eq!(with(15, 0x0008_0000, vec![0x44]).liquid_type(), Some(14));
        // `15` with every tile dry is genuinely no liquid.
        assert_eq!(with(15, 0, vec![0x0F]).liquid_type(), None);
    }
}
