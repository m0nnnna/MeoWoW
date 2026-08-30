//! WMO group files: the geometry a root file describes but does not contain.
//!
//! A group is one enclosed or open region of the object -- a room, a floor, an
//! outer wall. Splitting them lets the client draw only what is visible through
//! the portals connecting them.
//!
//! The layout has one quirk worth knowing before reading the code: the whole
//! group is wrapped in a single `MOGP` chunk whose payload is a 68-byte header
//! **followed by more chunks**. Iterating the file at the top level therefore
//! finds only `MVER` and `MOGP`; everything else is nested one level down.

use crate::{f32_at, string_at, u32_at, vec3_at, Chunks, Error};

const BATCH_SIZE: usize = 24;
/// Bytes of `MOGP` payload before the nested chunks begin.
const GROUP_HEADER_SIZE: usize = 68;

/// A run of triangles sharing one material.
#[derive(Clone, Copy, Debug)]
pub struct Batch {
    /// First entry in [`Group::indices`].
    pub start_index: u32,
    pub index_count: u16,
    pub min_vertex: u16,
    pub max_vertex: u16,
    pub flags: u8,
    /// Index into the root's material list.
    pub material_id: u8,
    /// Integer bounding box, used for culling.
    pub bounds: ([i16; 3], [i16; 3]),
}

impl Batch {
    pub fn triangle_count(&self) -> u32 {
        self.index_count as u32 / 3
    }
}

/// Per-triangle material assignment from `MOPY`.
#[derive(Clone, Copy, Debug)]
pub struct TriangleMaterial {
    pub flags: u8,
    /// `0xFF` marks a collision-only triangle with no visible surface.
    pub material_id: u8,
}

impl TriangleMaterial {
    /// Whether the triangle is collision geometry rather than something to
    /// draw. These exist in the vertex and index arrays like any other, so
    /// rendering them produces invisible walls made visible.
    pub fn is_collision_only(&self) -> bool {
        self.material_id == 0xFF
    }

    pub fn no_camera_collide(&self) -> bool {
        self.flags & 0x01 != 0
    }
    pub fn detail(&self) -> bool {
        self.flags & 0x08 != 0
    }
    pub fn render(&self) -> bool {
        self.flags & 0x20 != 0
    }
}

/// One group file.
pub struct Group {
    pub flags: u32,
    pub bounding_box: ([f32; 3], [f32; 3]),
    pub name: String,
    pub descriptive_name: String,
    pub group_id: u32,
    /// Index of the first portal referencing this group.
    pub portal_start: u16,
    pub portal_count: u16,
    pub batch_counts: BatchCounts,

    pub vertices: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u16>,
    pub triangle_materials: Vec<TriangleMaterial>,
    pub batches: Vec<Batch>,
    /// Baked lighting, present only when the group declares it.
    pub vertex_colors: Vec<[u8; 4]>,
    /// Indices into the root's doodad list.
    pub doodad_refs: Vec<u16>,
    /// The group header's `groupLiquid` -- the raw legacy liquid family, used
    /// with `& 3` to pick water / ocean / magma / slime. `0` for a group with
    /// no liquid.
    pub group_liquid: u32,
    /// The `MLIQ` surface, when the group has one.
    pub liquid: Option<Liquid>,
}

/// A group's `MLIQ` chunk: a rectangular grid of liquid surface heights and a
/// per-cell mask saying which cells actually hold liquid.
///
/// This is how a fountain basin, an interior pool, a canal or harbour water
/// inside a building gets its surface -- `MH2O` on the terrain is a separate
/// thing and never covers what sits under a `.wmo`.
///
/// The grid is `verts_x` by `verts_y` vertices (one more each way than the
/// tile counts). Vertex `(i, j)` sits at `corner + (i * TILE, j * TILE, h)` in
/// the group's own space, where `TILE` is `4.1666` -- the same
/// `adt::UNIT_SIZE` the terrain liquid uses -- and `h` is [`Liquid::heights`]
/// at `j * verts_x + i`.
#[derive(Clone, Debug, PartialEq)]
pub struct Liquid {
    pub verts_x: u32,
    pub verts_y: u32,
    pub tiles_x: u32,
    pub tiles_y: u32,
    /// The grid's minimum corner, in group-local space.
    pub corner: [f32; 3],
    /// One surface height per vertex, row-major with `x` fastest.
    pub heights: Vec<f32>,
    /// One byte per tile, row-major with `x` fastest. A tile is dry when the
    /// low nibble is `0xF` -- the "no liquid" sentinel; other bits carry the
    /// liquid type and flags this client does not read.
    pub tiles: Vec<u8>,
}

impl Liquid {
    /// Whether the tile at `(i, j)` holds liquid. Out-of-range is dry.
    pub fn cell_wet(&self, i: u32, j: u32) -> bool {
        if i >= self.tiles_x || j >= self.tiles_y {
            return false;
        }
        self.tiles
            .get((j * self.tiles_x + i) as usize)
            .is_some_and(|flag| flag & 0x0F != 0x0F)
    }

    /// The surface height at vertex `(i, j)`.
    pub fn height(&self, i: u32, j: u32) -> f32 {
        self.heights
            .get((j * self.verts_x + i) as usize)
            .copied()
            .unwrap_or(0.0)
    }
}

/// Batches are grouped by how they are lit and blended, and the counts say
/// where each run ends.
#[derive(Clone, Copy, Debug, Default)]
pub struct BatchCounts {
    /// Transition batches, drawn where interior meets exterior.
    pub transition: u16,
    pub interior: u16,
    pub exterior: u16,
}

impl Group {
    /// Parses a group file. `group_names` is the root's `MOGN` block, which is
    /// where the name actually lives.
    pub fn parse(data: &[u8], group_names: &[u8]) -> Result<Self, Error> {
        crate::check_version(data)?;

        let mogp = Chunks::find(data, b"MOGP").ok_or(Error::MissingChunk("MOGP"))?;
        if mogp.len() < GROUP_HEADER_SIZE {
            return Err(Error::TruncatedChunk {
                magic: "MOGP".into(),
                size: GROUP_HEADER_SIZE,
                left: mogp.len(),
            });
        }

        let name_offset = u32_at(mogp, 0) as i32;
        let desc_offset = u32_at(mogp, 4) as i32;
        let name = |offset: i32| {
            if offset >= 0 {
                string_at(group_names, offset as usize).to_string()
            } else {
                String::new()
            }
        };
        let h = |o: usize| u16::from_le_bytes([mogp[o], mogp[o + 1]]);

        // Everything after the fixed header is a nested chunk stream.
        let inner = &mogp[GROUP_HEADER_SIZE..];
        let find = |magic: &[u8; 4]| Chunks::find(inner, magic).unwrap_or(&[]);

        let vertices = find(b"MOVT")
            .chunks_exact(12)
            .map(|v| vec3_at(v, 0))
            .collect();
        let normals = find(b"MONR")
            .chunks_exact(12)
            .map(|v| vec3_at(v, 0))
            .collect();
        let uvs = find(b"MOTV")
            .chunks_exact(8)
            .map(|v| [f32_at(v, 0), f32_at(v, 4)])
            .collect();
        let indices = find(b"MOVI")
            .chunks_exact(2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .collect();
        let triangle_materials = find(b"MOPY")
            .chunks_exact(2)
            .map(|v| TriangleMaterial {
                flags: v[0],
                material_id: v[1],
            })
            .collect();
        let batches = find(b"MOBA")
            .chunks_exact(BATCH_SIZE)
            .map(|b| {
                let i16at = |o: usize| i16::from_le_bytes([b[o], b[o + 1]]);
                Batch {
                    bounds: (
                        [i16at(0), i16at(2), i16at(4)],
                        [i16at(6), i16at(8), i16at(10)],
                    ),
                    start_index: u32_at(b, 12),
                    index_count: u16::from_le_bytes([b[16], b[17]]),
                    min_vertex: u16::from_le_bytes([b[18], b[19]]),
                    max_vertex: u16::from_le_bytes([b[20], b[21]]),
                    flags: b[22],
                    material_id: b[23],
                }
            })
            .collect();
        let vertex_colors = find(b"MOCV")
            .chunks_exact(4)
            // Stored blue, green, red, alpha.
            .map(|c| [c[2], c[1], c[0], c[3]])
            .collect();
        let doodad_refs = find(b"MODR")
            .chunks_exact(2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .collect();

        // `MLIQ`: a 30-byte header, then `verts_x * verts_y` eight-byte vertex
        // records whose trailing `f32` is the surface height, then
        // `tiles_x * tiles_y` mask bytes. A body too short for the counts it
        // announces is dropped rather than read as a grid of zeroes.
        let liquid = {
            let mliq = find(b"MLIQ");
            let header_ok = mliq.len() >= 30;
            let (vx, vy, tx, ty) = if header_ok {
                (u32_at(mliq, 0), u32_at(mliq, 4), u32_at(mliq, 8), u32_at(mliq, 12))
            } else {
                (0, 0, 0, 0)
            };
            let verts = (vx as usize).saturating_mul(vy as usize);
            let tiles_n = (tx as usize).saturating_mul(ty as usize);
            let need = 30usize
                .saturating_add(verts.saturating_mul(8))
                .saturating_add(tiles_n);
            (header_ok && verts > 0 && mliq.len() >= need).then(|| Liquid {
                verts_x: vx,
                verts_y: vy,
                tiles_x: tx,
                tiles_y: ty,
                corner: vec3_at(mliq, 16),
                heights: (0..verts).map(|k| f32_at(mliq, 30 + k * 8 + 4)).collect(),
                tiles: mliq[30 + verts * 8..30 + verts * 8 + tiles_n].to_vec(),
            })
        };

        Ok(Self {
            flags: u32_at(mogp, 8),
            bounding_box: (vec3_at(mogp, 12), vec3_at(mogp, 24)),
            name: name(name_offset),
            descriptive_name: name(desc_offset),
            group_id: u32_at(mogp, 56),
            portal_start: h(36),
            portal_count: h(38),
            batch_counts: BatchCounts {
                transition: h(40),
                interior: h(42),
                exterior: h(44),
            },
            vertices,
            normals,
            uvs,
            indices,
            triangle_materials,
            batches,
            vertex_colors,
            doodad_refs,
            group_liquid: u32_at(mogp, 52),
            liquid,
        })
    }

    pub fn is_interior(&self) -> bool {
        self.flags & 0x2000 != 0
    }
    pub fn is_exterior(&self) -> bool {
        self.flags & 0x08 != 0
    }
    pub fn has_vertex_colors(&self) -> bool {
        self.flags & 0x04 != 0
    }
    /// Whether the group carries a liquid surface.
    ///
    /// Reads the parsed `MLIQ`, not the `0x1000` header flag: across the
    /// Stormwind city object, every group with an `MLIQ` chunk -- the
    /// fountains, the canals, the harbour -- has that flag *clear*, so trusting
    /// it drew no interior water anywhere.
    pub fn has_liquid(&self) -> bool {
        self.liquid.is_some()
    }

    /// The `LiquidType.dbc` row this group's surface should be drawn as, or
    /// `None` when it has no surface or none that renders.
    ///
    /// **`groupLiquid` is not a DBC id in a pre-Cata `.wmo`.** It is a legacy
    /// family index that has to be converted -- `15` means "no liquid", and
    /// any other value is `+1`'d and then mapped by its low two bits to one
    /// of the four "WMO Water/Ocean/Magma/Slime" rows. When the header says
    /// nothing (`0` or `15`), the type comes off the first wet tile's own low
    /// nibble the same way. This is the conversion every reference map
    /// extractor runs; taking `groupLiquid` at face value drew the Northshire
    /// fountain as green lava (raw `15` == `LiquidType.dbc` "Green Lava").
    pub fn liquid_type(&self) -> Option<u16> {
        let surface = self.liquid.as_ref()?;
        let convert = |id: u32| -> u32 {
            if id != 0 && id < 21 {
                match id.wrapping_sub(1) & 3 {
                    // The `0x0008_0000` bit picks "WMO Ocean" over "WMO Water"
                    // for an otherwise-fresh-water group, matching the
                    // extractor exactly.
                    0 => u32::from(self.flags & 0x0008_0000 != 0) + 13,
                    1 => 14,
                    2 => 19,
                    _ => 20,
                }
            } else {
                id
            }
        };
        let mut resolved = match self.group_liquid {
            0 | 15 => 0,
            other => convert(other + 1),
        };
        if resolved == 0 {
            for &tile in &surface.tiles {
                if tile & 0x0F != 0x0F {
                    resolved = convert(u32::from(tile & 0x0F) + 1);
                    break;
                }
            }
        }
        (resolved != 0).then_some(resolved as u16)
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Indices for one batch, ready for an index buffer.
    pub fn batch_indices(&self, batch: &Batch) -> Option<&[u16]> {
        let start = batch.start_index as usize;
        self.indices.get(start..start + batch.index_count as usize)
    }

    /// Checks the arrays agree with each other.
    ///
    /// The per-vertex arrays are parallel and the per-triangle array is a third
    /// of the index count; a mismatch means a chunk was misread rather than the
    /// file being unusual.
    pub fn validate(&self) -> Result<(), String> {
        if self.normals.len() != self.vertices.len() {
            return Err(format!(
                "{} normals for {} vertices",
                self.normals.len(),
                self.vertices.len()
            ));
        }
        if self.uvs.len() != self.vertices.len() {
            return Err(format!(
                "{} uvs for {} vertices",
                self.uvs.len(),
                self.vertices.len()
            ));
        }
        if self.indices.len() % 3 != 0 {
            return Err(format!(
                "{} indices is not a whole number of triangles",
                self.indices.len()
            ));
        }
        if !self.triangle_materials.is_empty()
            && self.triangle_materials.len() != self.indices.len() / 3
        {
            return Err(format!(
                "{} triangle materials for {} triangles",
                self.triangle_materials.len(),
                self.indices.len() / 3
            ));
        }
        if !self.vertex_colors.is_empty() && self.vertex_colors.len() != self.vertices.len() {
            return Err(format!(
                "{} vertex colours for {} vertices",
                self.vertex_colors.len(),
                self.vertices.len()
            ));
        }
        if let Some(&worst) = self.indices.iter().max() {
            if worst as usize >= self.vertices.len() {
                return Err(format!(
                    "index {worst} exceeds the {} vertices present",
                    self.vertices.len()
                ));
            }
        }
        for (i, batch) in self.batches.iter().enumerate() {
            let end = batch.start_index as usize + batch.index_count as usize;
            if end > self.indices.len() {
                return Err(format!(
                    "batch {i} covers indices {}..{end}, past the {} available",
                    batch.start_index,
                    self.indices.len()
                ));
            }
        }
        Ok(())
    }
}
