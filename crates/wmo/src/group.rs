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
    pub fn has_liquid(&self) -> bool {
        self.flags & 0x1000 != 0
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
