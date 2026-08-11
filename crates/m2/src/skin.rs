//! `.skin` files: the triangles that turn an M2's vertex pool into geometry.
//!
//! Splitting them out lets one model ship several levels of detail over a
//! single vertex pool. A 3.3.5a model normally has four, `Foo00.skin` (highest
//! detail) through `Foo03.skin`.
//!
//! Indices are two levels deep, which is the main thing to get right:
//! [`Skin::triangles`] indexes into [`Skin::vertex_map`], and *that* indexes
//! into the model's vertex array. Skipping the indirection produces geometry
//! that looks plausible but is subtly scrambled.

use crate::Error;

const SUBMESH_SIZE: usize = 48;
const BATCH_SIZE: usize = 24;

/// A contiguous run of geometry, and the unit the client switches on and off
/// -- hair, armour pieces, facial features are all submeshes.
#[derive(Clone, Copy, Debug)]
pub struct Submesh {
    /// Identifies what the submesh represents. The thousands digit groups it
    /// (hair, gloves, boots...) and the remainder selects the variant.
    pub id: u16,
    /// First entry in [`Skin::vertex_map`].
    pub vertex_start: u32,
    pub vertex_count: u32,
    /// First entry in [`Skin::triangles`].
    pub index_start: u32,
    pub index_count: u32,
    pub bone_count: u16,
    pub bone_combo_index: u16,
    /// Maximum bones influencing any one vertex here.
    pub bone_influences: u16,
    pub center: [f32; 3],
    pub sort_center: [f32; 3],
    pub sort_radius: f32,
    /// Set when the stored 16-bit counts overflowed and were reconstructed.
    /// See [`Skin::parse`].
    pub counts_repaired: bool,
}

impl Submesh {
    pub fn triangle_count(&self) -> u32 {
        self.index_count / 3
    }
}

/// Pairs a submesh with the render state to draw it.
#[derive(Clone, Copy, Debug)]
pub struct Batch {
    pub flags: u8,
    pub priority_plane: i8,
    pub shader_id: u16,
    /// Index into [`Skin::submeshes`].
    pub submesh_index: u16,
    pub color_index: u16,
    /// Index into the model's material list.
    pub material_index: u16,
    pub material_layer: u16,
    pub texture_count: u16,
    /// Start index into the model's texture combo table.
    pub texture_combo_index: u16,
    pub texture_coord_combo_index: u16,
    pub texture_weight_combo_index: u16,
    pub texture_transform_combo_index: u16,
}

/// Reconstructs counts that overflowed their 16-bit fields.
///
/// `vertexCount` and `indexCount` are `u16`, and a handful of models exceed
/// that. `Sunwell_Bushes00.skin` has one submesh covering an index array of
/// 93,456 entries but stores 27,920 -- exactly 93,456 minus 65,536. The `Level`
/// field is documented as carrying high bits for the *start* fields, but it is
/// zero here, so the counts simply wrapped.
///
/// Submeshes are laid out contiguously, so the distance to the next one's start
/// gives the true span. Congruence modulo 65536 alone is *not* enough evidence
/// to use it: in `NexusRaid_SkyA00.skin` a submesh correctly declaring 1728
/// indices sits 67,264 before the next start, and 67,264 is congruent to 1728,
/// so a naive rule silently corrupts a valid submesh.
///
/// Repair therefore requires the stored value to be provably wrong first --
/// either not a whole number of triangles, or running past the array -- and the
/// replacement to be provably better. Everything else is left alone and, if it
/// is genuinely broken, surfaces through [`Skin::validate`].
fn repair_wrapped_counts(
    mut submeshes: Vec<Submesh>,
    vertex_total: usize,
    index_total: usize,
) -> Vec<Submesh> {
    const WRAP: u32 = 1 << 16;

    for i in 0..submeshes.len() {
        let next_index = submeshes
            .get(i + 1)
            .map_or(index_total as u32, |s| s.index_start);
        let next_vertex = submeshes
            .get(i + 1)
            .map_or(vertex_total as u32, |s| s.vertex_start);

        let s = &mut submeshes[i];

        let index_broken = s.index_count % 3 != 0
            || (s.index_start + s.index_count) as usize > index_total;
        let span = next_index.saturating_sub(s.index_start);
        if index_broken
            && span > s.index_count
            && span % WRAP == s.index_count % WRAP
            && span % 3 == 0
            && (s.index_start + span) as usize <= index_total
        {
            s.index_count = span;
            s.counts_repaired = true;
        }

        // Vertex counts have no divisibility rule, so the only detectable
        // corruption is running past the end of the map.
        let vertex_broken = (s.vertex_start + s.vertex_count) as usize > vertex_total;
        let span = next_vertex.saturating_sub(s.vertex_start);
        if vertex_broken
            && span > s.vertex_count
            && span % WRAP == s.vertex_count % WRAP
            && (s.vertex_start + span) as usize <= vertex_total
        {
            s.vertex_count = span;
            s.counts_repaired = true;
        }
    }
    submeshes
}

/// A parsed `.skin`.
pub struct Skin {
    vertex_map: Vec<u16>,
    triangles: Vec<u16>,
    submeshes: Vec<Submesh>,
    batches: Vec<Batch>,
}

impl Skin {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 0x30 {
            return Err(Error::TooShort { got: bytes.len() });
        }
        if &bytes[..4] != b"SKIN" {
            return Err(Error::BadMagic([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }
        let word = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());

        let read = |what: &'static str, at: usize, stride: usize| -> Result<&[u8], Error> {
            let (count, offset) = (word(at), word(at + 4));
            let start = offset as usize;
            let len = count as usize * stride;
            bytes
                .get(start..start + len)
                .ok_or(Error::ArrayOutOfBounds {
                    what,
                    count,
                    offset,
                    stride,
                    len: bytes.len(),
                })
        };

        let u16s = |raw: &[u8]| -> Vec<u16> {
            raw.chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect()
        };

        let vertex_map = u16s(read("skin vertices", 0x04, 2)?);
        let triangles = u16s(read("skin triangles", 0x0C, 2)?);
        let submesh_raw = read("skin submeshes", 0x1C, SUBMESH_SIZE)?;
        let batch_raw = read("skin batches", 0x24, BATCH_SIZE)?;

        let submeshes = submesh_raw
            .chunks_exact(SUBMESH_SIZE)
            .map(|s| {
                let h = |o: usize| u16::from_le_bytes([s[o], s[o + 1]]);
                let f = |o: usize| f32::from_le_bytes(s[o..o + 4].try_into().unwrap());
                // `level` carries the high 16 bits of both start fields, so
                // models with more than 65535 vertices still fit u16 fields.
                let level = (h(2) as u32) << 16;
                Submesh {
                    id: h(0),
                    vertex_start: level | h(4) as u32,
                    vertex_count: h(6) as u32,
                    index_start: level | h(8) as u32,
                    index_count: h(10) as u32,
                    bone_count: h(12),
                    bone_combo_index: h(14),
                    bone_influences: h(16),
                    center: [f(20), f(24), f(28)],
                    sort_center: [f(32), f(36), f(40)],
                    sort_radius: f(44),
                    counts_repaired: false,
                }
            })
            .collect();
        let submeshes = repair_wrapped_counts(submeshes, vertex_map.len(), triangles.len());

        let batches = batch_raw
            .chunks_exact(BATCH_SIZE)
            .map(|b| {
                let h = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
                Batch {
                    flags: b[0],
                    priority_plane: b[1] as i8,
                    shader_id: h(2),
                    submesh_index: h(4),
                    color_index: h(8),
                    material_index: h(10),
                    material_layer: h(12),
                    texture_count: h(14),
                    texture_combo_index: h(16),
                    texture_coord_combo_index: h(18),
                    texture_weight_combo_index: h(20),
                    texture_transform_combo_index: h(22),
                }
            })
            .collect();

        Ok(Self {
            vertex_map,
            triangles,
            submeshes,
            batches,
        })
    }

    /// Local vertex slots, each holding an index into the model's vertex array.
    pub fn vertex_map(&self) -> &[u16] {
        &self.vertex_map
    }

    /// Triangle corners, as indices into [`Skin::vertex_map`].
    pub fn triangles(&self) -> &[u16] {
        &self.triangles
    }

    pub fn submeshes(&self) -> &[Submesh] {
        &self.submeshes
    }

    pub fn batches(&self) -> &[Batch] {
        &self.batches
    }

    /// Resolves one submesh's triangles into model vertex indices, ready for an
    /// index buffer.
    ///
    /// Returns `None` if the submesh's range is out of bounds, which would mean
    /// the skin and model disagree.
    pub fn submesh_indices(&self, submesh: &Submesh) -> Option<Vec<u32>> {
        let start = submesh.index_start as usize;
        let end = start + submesh.index_count as usize;
        let slice = self.triangles.get(start..end)?;

        slice
            .iter()
            .map(|&local| self.vertex_map.get(local as usize).map(|&v| v as u32))
            .collect()
    }

    /// Checks that every triangle resolves and every submesh range fits.
    ///
    /// Cheap enough to run over the whole archive set, and the only way to know
    /// the two-level indexing is being read correctly.
    pub fn validate(&self, model_vertex_count: usize) -> Result<(), String> {
        for (i, &local) in self.triangles.iter().enumerate() {
            let Some(&v) = self.vertex_map.get(local as usize) else {
                return Err(format!(
                    "triangle index {i} refers to local vertex {local}, but the map has {}",
                    self.vertex_map.len()
                ));
            };
            if v as usize >= model_vertex_count {
                return Err(format!(
                    "local vertex {local} maps to model vertex {v}, but the model has \
                     {model_vertex_count}"
                ));
            }
        }
        for (i, s) in self.submeshes.iter().enumerate() {
            let end = s.index_start as usize + s.index_count as usize;
            if end > self.triangles.len() {
                return Err(format!(
                    "submesh {i} covers indices {}..{end}, past the {} available",
                    s.index_start,
                    self.triangles.len()
                ));
            }
            if s.index_count % 3 != 0 {
                return Err(format!(
                    "submesh {i} has {} indices, not a whole number of triangles",
                    s.index_count
                ));
            }
        }
        for (i, b) in self.batches.iter().enumerate() {
            if b.submesh_index as usize >= self.submeshes.len() {
                return Err(format!(
                    "batch {i} points at submesh {}, but there are {}",
                    b.submesh_index,
                    self.submeshes.len()
                ));
            }
        }
        Ok(())
    }
}
