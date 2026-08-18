//! `MH2O`: the water, lava and slime lying on a terrain tile.
//!
//! Written from the public format documentation at <https://wowdev.wiki/ADT>,
//! with the three sizes that documentation gets wrong for this build measured
//! against the files themselves -- see [`exists_bitmap_len`] and
//! [`ATTRIBUTES_SIZE`].
//!
//! # Shape
//!
//! One `MH2O` chunk serves the whole tile. It opens with 256 fixed headers, one
//! per map chunk in file order, each naming where that chunk's liquid *layers*
//! live. A chunk with no water has a layer count of zero and nothing else.
//!
//! Every offset in the chunk -- in the headers and in the instances alike -- is
//! measured from the start of the `MH2O` **payload**, not from the file and not
//! from the containing chunk's header. That is the one thing about this format
//! most likely to be got wrong silently: an offset measured from the wrong base
//! still lands inside the chunk and still parses.
//!
//! # Why a layer is a rectangle rather than a chunk
//!
//! A chunk is 8x8 cells and a pond rarely fills one. An instance therefore
//! carries a sub-rectangle -- `x_offset`, `y_offset`, `width`, `height` -- and
//! its vertex grid is `(width + 1) * (height + 1)`, because a rectangle of cells
//! has one more sample than cell along each axis. Reading the grid at `width`
//! rather than `width + 1` shears the surface exactly the way a wrong stride
//! shears the height field next door, and looks like plausible water.
//!
//! # What is *not* here
//!
//! `MCLQ`, the pre-WotLK per-chunk liquid block. It is not a fallback this
//! reader declines to implement: **`Azeroth_32_48` has 256 map chunks and none
//! of them carries one**, while its `MH2O` describes 42 chunks of water. A
//! 3.3.5a tile states its liquid once, in one place.

use chunk::{f32_at, u16_at, u32_at};

/// Cells along one edge of a map chunk, and so the largest rectangle a liquid
/// instance can cover.
pub const CELLS_PER_CHUNK: usize = 8;

/// What [`LiquidInstance::vertex_depth`] answers where the file stores no
/// depth at all.
///
/// Deliberately larger than any depth observed in a file (Northshire's run to
/// 35), so a sheet that says nothing about its depth is drawn at full opacity
/// rather than faded. The open ocean is exactly this case in reverse -- it
/// stores depths and nothing else -- but a WMO pool or an unknown vertex format
/// has none, and a lake that faded to nothing because its depths were missing
/// would look like a rendering bug rather than like missing data.
pub const DEPTH_UNKNOWN: f32 = 255.0;

/// Bytes of the per-chunk header: two offsets and a count.
const HEADER_SIZE: usize = 12;
/// Bytes of one liquid instance.
const INSTANCE_SIZE: usize = 24;

/// Bytes of a chunk's attribute block: two 8x8 bitmasks, one bit per cell,
/// saying which cells are fishable and which are deep.
///
/// **Measured, not transcribed**, and named here even though nothing in this
/// reader consumes it -- the blocks are reached by their own offsets, so the
/// size is never needed to *find* anything. It is recorded because it is what
/// proved the rest of the layout: in `Azeroth_32_48`, chunk 13's sheet covers
/// 3x6 cells with vertex data at 4099, and chunk 14's attributes begin at 4239.
/// `4099 + 4 * (4 * 7) + (4 * 7) == 4239` accounts for that vertex block to the
/// byte, and `4239 + 16 == 4255` is where chunk 14's own vertex data starts.
/// Sixteen, not the eight a single bitmask would need.
pub const ATTRIBUTES_SIZE: usize = 16;

/// What a liquid instance stores per vertex.
///
/// The format decides which arrays follow the instance and in what order, and
/// getting it wrong reads a depth byte as the low byte of a height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VertexFormat {
    /// A height and a depth per vertex.
    HeightDepth,
    /// A height and a texture coordinate per vertex. Used by the ocean shader.
    HeightUv,
    /// Depth only: the surface is flat at `min_height`.
    DepthOnly,
    /// Height, texture coordinate and depth.
    HeightUvDepth,
    /// Anything else this build does not use. Treated as flat, because a
    /// format whose layout is unknown cannot have its arrays located -- and
    /// guessing would read whatever follows as heights.
    Unknown(u16),
}

impl VertexFormat {
    pub fn from_raw(raw: u16) -> Self {
        match raw {
            0 => Self::HeightDepth,
            1 => Self::HeightUv,
            2 => Self::DepthOnly,
            3 => Self::HeightUvDepth,
            other => Self::Unknown(other),
        }
    }

    /// Whether a per-vertex height array is present at all.
    ///
    /// `false` does not mean the surface has no height: it means the height is
    /// `min_height` everywhere, which is a statement rather than an absence.
    pub fn has_heights(&self) -> bool {
        matches!(self, Self::HeightDepth | Self::HeightUv | Self::HeightUvDepth)
    }

    /// Bytes one vertex occupies, or `None` for a format whose layout is not
    /// known.
    pub fn stride(&self) -> Option<usize> {
        Some(match self {
            Self::HeightDepth => 4 + 1,
            Self::HeightUv => 4 + 4,
            Self::DepthOnly => 1,
            Self::HeightUvDepth => 4 + 4 + 1,
            Self::Unknown(_) => return None,
        })
    }
}

/// Bytes the render bitmap of a `width` x `height` rectangle occupies.
///
/// **Measured against the files, and it disagrees with the wiki.** The public
/// documentation describes one byte per row -- `height` bytes -- which would
/// make chunk 13's 3x6 rectangle six bytes and chunk 15's 5x8 rectangle eight.
/// In `Azeroth_32_48` the gap between each bitmap and the vertex data that
/// follows it is **three** and **five**: `ceil(width * height / 8)`, packed
/// continuously with no per-row padding. Two rectangles whose two readings
/// disagree in opposite directions, which is what makes it a measurement rather
/// than a coincidence.
pub fn exists_bitmap_len(width: u8, height: u8) -> usize {
    (width as usize * height as usize).div_ceil(8)
}

/// One sheet of liquid covering part of a map chunk.
///
/// A chunk can carry several: a cave pool under a river needs two surfaces at
/// the same place and different heights.
#[derive(Clone, Debug)]
pub struct LiquidInstance {
    /// Row of `LiquidType.dbc`, which is what says whether this is water, lava
    /// or slime -- **and the row's *category* column says it, not its name**.
    /// Row 181 is called `Orange Slime` and is categorised as water.
    pub liquid_type: u16,
    pub vertex_format: VertexFormat,
    /// The flat height to use where no per-vertex height is stored, and the
    /// bounds of the surface where one is.
    pub min_height: f32,
    pub max_height: f32,
    /// Where in the chunk's 8x8 cell grid this sheet starts, and how far it
    /// runs. See [`LiquidInstance::height_at`] for which axis is which.
    pub x_offset: u8,
    pub y_offset: u8,
    pub width: u8,
    pub height: u8,
    /// `(width + 1) * (height + 1)` heights, or empty when the surface is flat.
    ///
    /// Empty is a real answer meaning "flat at `min_height`", which is how the
    /// ocean is stored, and it must not be confused with a failed read.
    pub heights: Vec<f32>,
    /// One flag per *cell*, `width * height` of them, row-major along the same
    /// axes as the rectangle.
    ///
    /// Empty means every cell exists. That is what an `offset_exists_bitmap` of
    /// zero states, and it is the common case: a full 8x8 sheet does not need a
    /// mask to say so.
    pub exists: Vec<bool>,
    /// How deep the liquid is at each vertex, `0` at the waterline rising to
    /// `255`, one per vertex on the same grid as `heights`.
    ///
    /// This is what lets a shore fade rather than end at a hard blue line, and
    /// it is the only per-vertex data the ocean carries at all -- 86,222 of
    /// Azeroth's 92,219 sheets are [`VertexFormat::DepthOnly`], a flat surface
    /// whose entire shape is in this array. Empty for a format that stores
    /// none, which means "no information", not "zero deep".
    pub depths: Vec<u8>,
}

impl LiquidInstance {
    /// Whether the cell at `(i, j)` within this rectangle carries liquid.
    ///
    /// `i` runs along the rectangle's `width` and `j` along its `height`, both
    /// measured from the rectangle's own corner rather than the chunk's.
    pub fn cell_exists(&self, i: usize, j: usize) -> bool {
        if i >= self.width as usize || j >= self.height as usize {
            return false;
        }
        if self.exists.is_empty() {
            return true;
        }
        self.exists
            .get(j * self.width as usize + i)
            .copied()
            .unwrap_or(false)
    }

    /// Height of one vertex of the sheet, in world units.
    ///
    /// Vertices are `(width + 1) x (height + 1)`; a flat sheet answers
    /// `min_height` for all of them.
    pub fn vertex_height(&self, i: usize, j: usize) -> f32 {
        if self.heights.is_empty() {
            return self.min_height;
        }
        let stride = self.width as usize + 1;
        self.heights
            .get(j * stride + i)
            .copied()
            .unwrap_or(self.min_height)
    }

    /// How deep the liquid is at one vertex, in the file's own units.
    ///
    /// **Not an alpha byte, which is the obvious reading and is wrong.** Over
    /// the whole of Northshire's tile these bytes run 0 to 35 and never
    /// approach 255 -- so treating the byte as `depth / 255` makes the deepest
    /// part of a river 14% opaque, and the river as good as invisible. They are
    /// a depth: 0 exactly at the waterline, rising into the channel. A caller
    /// wanting an opacity has to choose the depth at which the liquid becomes
    /// opaque; see `DEPTH_FOR_FULL_OPACITY` in the viewer.
    ///
    /// Answers [`DEPTH_UNKNOWN`] for a sheet that stores no depths. That is the
    /// honest fallback: a sheet with no depth information has no shore to fade
    /// towards, and fading one in would invent a shallow edge where the file
    /// describes none.
    pub fn vertex_depth(&self, i: usize, j: usize) -> f32 {
        if self.depths.is_empty() {
            return DEPTH_UNKNOWN;
        }
        let stride = self.width as usize + 1;
        self.depths
            .get(j * stride + i)
            .map_or(DEPTH_UNKNOWN, |&d| d as f32)
    }

    /// Surface height at a world position, or `None` where this sheet does not
    /// reach.
    ///
    /// `chunk_position` is the map chunk's origin corner, the same one
    /// [`crate::height_in_chunk`] takes, and **both axes run inwards
    /// (negative) from it** exactly as the terrain's do. `x_offset` indexes the
    /// axis running inwards along world *y*, and `y_offset` the one running
    /// inwards along world *x* -- the same minor/major pairing the height
    /// field uses, where a sample's index is `row * 17 + col` and `col` is the
    /// world-*y* axis.
    ///
    /// That pairing is the one thing here a reader cannot check by looking: a
    /// transposed rectangle draws a perfectly convincing pond a quarter of a
    /// chunk from where the pond is. What settles it is that **water lies in
    /// low ground**, measured by `wow-cli adt liquid` over every sheet in a
    /// map against the terrain beneath it.
    ///
    /// **The measurement took two attempts to ask a question that could
    /// answer, and both failures are the same one this project keeps paying
    /// for.** Over all 5,660,498 liquid cells in Azeroth the two readings come
    /// out 99.2% against 98.4% -- indistinguishable, and it would have been
    /// read as confirmation. The reason is the population: 86,222 of the
    /// 92,219 sheets are open ocean covering a whole chunk, and **transposing a
    /// full 8x8 rectangle produces the identical footprint**, so those sheets
    /// agree with both readings no matter what the seabed does. Restricted to
    /// cells in a rectangle that transposing actually *moves*, over ground that
    /// differs between the two sample points, 90,346 cells vote:
    ///
    /// | reading | liquid at or above the ground |
    /// |---|---|
    /// | as read | 66,299 (73.4%) |
    /// | axes swapped | 33,223 (36.8%) |
    ///
    /// A factor of two, where the whole-population figure was a rounding
    /// error. It is not 100% because a shoreline genuinely has cells where the
    /// bank rises through the water plane -- what matters is the ratio, and
    /// that only one reading puts rivers in valleys.
    pub fn height_at(&self, chunk_position: [f32; 3], x: f32, y: f32) -> Option<f32> {
        let unit = crate::UNIT_SIZE;
        // Inwards from the corner, in cells.
        let major = (chunk_position[0] - x) / unit;
        let minor = (chunk_position[1] - y) / unit;

        // The same tolerance, for the same reason, as [`crate::height_in_chunk`]:
        // a world coordinate eight thousand units out has a float ulp of about
        // a millimetre, so a point *exactly* on a sheet's far edge computes a
        // hair outside it. Absorbing that much matters more here than it does
        // for terrain -- a shoreline is precisely where a character is closest
        // to the edge of a sheet, and a strict test makes the last hand-span of
        // water intermittently vanish.
        const EDGE_TOLERANCE: f32 = 0.01;
        let side = CELLS_PER_CHUNK as f32;
        if !(-EDGE_TOLERANCE..=side + EDGE_TOLERANCE).contains(&major)
            || !(-EDGE_TOLERANCE..=side + EDGE_TOLERANCE).contains(&minor)
        {
            return None;
        }
        let (major, minor) = (major.clamp(0.0, side), minor.clamp(0.0, side));

        // Relative to the rectangle rather than to the chunk.
        let local_major = major - self.y_offset as f32;
        let local_minor = minor - self.x_offset as f32;
        if local_major < -EDGE_TOLERANCE
            || local_minor < -EDGE_TOLERANCE
            || local_major > self.height as f32 + EDGE_TOLERANCE
            || local_minor > self.width as f32 + EDGE_TOLERANCE
        {
            return None;
        }
        let local_major = local_major.clamp(0.0, self.height as f32);
        let local_minor = local_minor.clamp(0.0, self.width as f32);

        // `min` keeps a position exactly on the far edge inside the last cell.
        let cell_j = (local_major.floor() as usize).min(self.height.saturating_sub(1) as usize);
        let cell_i = (local_minor.floor() as usize).min(self.width.saturating_sub(1) as usize);
        if !self.cell_exists(cell_i, cell_j) {
            return None;
        }

        // Bilinear across the cell's four corners. Unlike the terrain there is
        // no centre sample to fan around: a liquid grid is a plain lattice.
        let (s, t) = (local_major - cell_j as f32, local_minor - cell_i as f32);
        let h00 = self.vertex_height(cell_i, cell_j);
        let h10 = self.vertex_height(cell_i, cell_j + 1);
        let h01 = self.vertex_height(cell_i + 1, cell_j);
        let h11 = self.vertex_height(cell_i + 1, cell_j + 1);
        let top = h00 + (h01 - h00) * t;
        let bottom = h10 + (h11 - h10) * t;
        Some(top + (bottom - top) * s)
    }
}

/// Every liquid sheet on one tile, indexed by map chunk in file order.
///
/// Always [`crate::CHUNK_COUNT`] entries long, most of them empty. A flat
/// vector rather than a map so a caller holding a chunk index can ask without
/// a lookup that can fail for two different reasons.
#[derive(Clone, Debug, Default)]
pub struct TileLiquid {
    chunks: Vec<Vec<LiquidInstance>>,
}

impl TileLiquid {
    /// Reads the `MH2O` payload of a tile. A tile without one has no liquid,
    /// which is an answer rather than an error -- most tiles inland have none.
    pub fn parse(payload: &[u8]) -> Self {
        let mut chunks = Vec::with_capacity(crate::CHUNK_COUNT);
        for index in 0..crate::CHUNK_COUNT {
            let header = index * HEADER_SIZE;
            if payload.len() < header + HEADER_SIZE {
                chunks.push(Vec::new());
                continue;
            }
            let offset_instances = u32_at(payload, header) as usize;
            let layer_count = u32_at(payload, header + 4) as usize;
            // Attributes -- which cells are fishable and which are deep -- are
            // read past deliberately. Nothing this client draws or walks on
            // consults them, and a field parsed but unused is a field that can
            // drift out of agreement with its own doc comment.
            let _offset_attributes = u32_at(payload, header + 8) as usize;

            let mut layers = Vec::with_capacity(layer_count);
            for layer in 0..layer_count {
                let at = offset_instances + layer * INSTANCE_SIZE;
                if payload.len() < at + INSTANCE_SIZE {
                    break;
                }
                if let Some(instance) = parse_instance(payload, at) {
                    layers.push(instance);
                }
            }
            chunks.push(layers);
        }
        Self { chunks }
    }

    /// Sheets on one map chunk, by its index in file order.
    pub fn chunk(&self, index: usize) -> &[LiquidInstance] {
        self.chunks.get(index).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Map chunks carrying at least one sheet.
    pub fn chunks_with_liquid(&self) -> usize {
        self.chunks.iter().filter(|c| !c.is_empty()).count()
    }

    /// Every sheet on the tile, paired with the chunk index it belongs to.
    pub fn instances(&self) -> impl Iterator<Item = (usize, &LiquidInstance)> {
        self.chunks
            .iter()
            .enumerate()
            .flat_map(|(index, layers)| layers.iter().map(move |layer| (index, layer)))
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.iter().all(Vec::is_empty)
    }
}

fn parse_instance(payload: &[u8], at: usize) -> Option<LiquidInstance> {
    let liquid_type = u16_at(payload, at);
    let vertex_format = VertexFormat::from_raw(u16_at(payload, at + 2));
    let min_height = f32_at(payload, at + 4);
    let max_height = f32_at(payload, at + 8);
    let x_offset = *payload.get(at + 12)?;
    let y_offset = *payload.get(at + 13)?;
    let width = *payload.get(at + 14)?;
    let height = *payload.get(at + 15)?;
    let offset_exists = u32_at(payload, at + 16) as usize;
    let offset_vertices = u32_at(payload, at + 20) as usize;

    // A rectangle larger than the chunk is a misread offset rather than a very
    // large pond, and the vertex read that follows would be enormous.
    if width as usize > CELLS_PER_CHUNK
        || height as usize > CELLS_PER_CHUNK
        || x_offset as usize + width as usize > CELLS_PER_CHUNK
        || y_offset as usize + height as usize > CELLS_PER_CHUNK
    {
        return None;
    }

    let cells = width as usize * height as usize;
    let exists = if offset_exists == 0 || cells == 0 {
        // Zero is "every cell", not "no cells". A full sheet stores no mask.
        Vec::new()
    } else {
        let bytes = payload.get(offset_exists..offset_exists + exists_bitmap_len(width, height))?;
        (0..cells)
            .map(|cell| bytes[cell / 8] & (1 << (cell % 8)) != 0)
            .collect()
    };

    let vertices = (width as usize + 1) * (height as usize + 1);
    // The arrays follow one another whole -- all the heights, then all the
    // texture coordinates, then all the depths -- rather than interleaving per
    // vertex. Reading them interleaved would put a depth byte in the low byte
    // of the second height, which parses and produces a surface with a fine
    // tremble in it.
    let heights = if offset_vertices == 0 || !vertex_format.has_heights() {
        // Flat at `min_height`. The ocean is stored this way and so is every
        // still lake, so this is the common case rather than a degenerate one.
        Vec::new()
    } else {
        let end = offset_vertices + vertices * 4;
        let block = payload.get(offset_vertices..end)?;
        (0..vertices).map(|v| f32_at(block, v * 4)).collect()
    };
    let depths = match depth_offset(vertex_format, offset_vertices, vertices) {
        Some(at) => payload.get(at..at + vertices).map(<[u8]>::to_vec).unwrap_or_default(),
        None => Vec::new(),
    };

    Some(LiquidInstance {
        liquid_type,
        vertex_format,
        min_height,
        max_height,
        x_offset,
        y_offset,
        width,
        height,
        heights,
        exists,
        depths,
    })
}

/// Where a sheet's depth array starts, or `None` for a format that stores
/// none.
///
/// The depth bytes always come *last*, after whichever of the height and
/// texture-coordinate arrays the format carries -- so the offset is a function
/// of the format rather than a stored field, and getting the format wrong here
/// reads texture coordinates as depths.
fn depth_offset(format: VertexFormat, base: usize, vertices: usize) -> Option<usize> {
    if base == 0 {
        return None;
    }
    let before = match format {
        VertexFormat::HeightDepth => 4,
        VertexFormat::DepthOnly => 0,
        VertexFormat::HeightUvDepth => 4 + 4,
        // No depth array at all: the ocean shader's format stores a texture
        // coordinate where the others store a depth.
        VertexFormat::HeightUv | VertexFormat::Unknown(_) => return None,
    };
    Some(base + before * vertices)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bitmap length that the files measured, not the one the wiki states.
    ///
    /// Both readings are asserted here because they differ in opposite
    /// directions for these two rectangles: per-row padding would make the
    /// first six bytes and the second eight, and the packed reading makes them
    /// three and five. A test that only pinned one rectangle would pass under
    /// either.
    #[test]
    fn the_exists_bitmap_is_packed_rather_than_padded_per_row() {
        assert_eq!(exists_bitmap_len(3, 6), 3, "3x6 measured three bytes");
        assert_eq!(exists_bitmap_len(5, 8), 5, "5x8 measured five bytes");
        // A full sheet is the one case where both readings agree, which is
        // exactly why it cannot be the sample that settles it.
        assert_eq!(exists_bitmap_len(8, 8), 8);
    }

    /// The arithmetic that identified the block sizes, kept as a test so the
    /// numbers cannot drift away from the doc comments that cite them.
    ///
    /// These are real offsets out of `Azeroth_32_48`: chunk 13's sheet is 3x6
    /// with vertex data at 4099, and chunk 14's attributes and vertex data are
    /// at 4239 and 4255. Nothing here reads the file -- the point is that one
    /// consistent set of sizes explains all three positions, and no other set
    /// does.
    #[test]
    fn the_measured_offsets_account_for_every_byte() {
        let (width, height) = (3usize, 6usize);
        let vertices = (width + 1) * (height + 1);
        // Format 0 is a f32 height plus a u8 depth per vertex.
        assert_eq!(4099 + vertices * 4 + vertices, 4239, "chunk 13's sheet");
        assert_eq!(4239 + ATTRIBUTES_SIZE, 4255, "chunk 14's attributes");
        // And chunk 14's own 8x8 sheet ends where chunk 15's attributes begin.
        let full = (8 + 1) * (8 + 1);
        assert_eq!(4255 + full * 4 + full, 4660, "chunk 14's sheet");
        // Chunk 15's exists bitmap follows its attributes, and its 5x8
        // rectangle needs five bytes before the vertex data at 4681.
        assert_eq!(4660 + ATTRIBUTES_SIZE, 4676, "chunk 15's attributes");
        assert_eq!(4676 + exists_bitmap_len(5, 8), 4681, "chunk 15's bitmap");
    }

    #[test]
    fn vertex_strides_match_their_arrays() {
        assert_eq!(VertexFormat::from_raw(0).stride(), Some(5));
        assert_eq!(VertexFormat::from_raw(1).stride(), Some(8));
        assert_eq!(VertexFormat::from_raw(2).stride(), Some(1));
        assert_eq!(VertexFormat::from_raw(3).stride(), Some(9));
        assert_eq!(VertexFormat::from_raw(9).stride(), None);
        // Depth-only carries no height array, so its surface is flat.
        assert!(!VertexFormat::from_raw(2).has_heights());
        assert!(VertexFormat::from_raw(0).has_heights());
    }

    /// A hand-built `MH2O` payload, so the offsets are known rather than
    /// inferred: one chunk with one full-cover flat sheet.
    fn flat_tile(liquid_type: u16, level: f32, chunk_index: usize) -> Vec<u8> {
        let mut payload = vec![0u8; crate::CHUNK_COUNT * HEADER_SIZE];
        let instances_at = payload.len();
        // Header for the one chunk that has water.
        let header = chunk_index * HEADER_SIZE;
        payload[header..header + 4].copy_from_slice(&(instances_at as u32).to_le_bytes());
        payload[header + 4..header + 8].copy_from_slice(&1u32.to_le_bytes());

        let mut instance = Vec::new();
        instance.extend_from_slice(&liquid_type.to_le_bytes());
        instance.extend_from_slice(&0u16.to_le_bytes()); // height + depth
        instance.extend_from_slice(&level.to_le_bytes());
        instance.extend_from_slice(&level.to_le_bytes());
        instance.extend_from_slice(&[0, 0, 8, 8]); // full 8x8
        instance.extend_from_slice(&0u32.to_le_bytes()); // no exists bitmap
        instance.extend_from_slice(&0u32.to_le_bytes()); // no vertex data
        assert_eq!(instance.len(), INSTANCE_SIZE);
        payload.extend_from_slice(&instance);
        payload
    }

    #[test]
    fn a_flat_sheet_covers_its_whole_chunk() {
        let payload = flat_tile(5, 143.99, 14);
        let liquid = TileLiquid::parse(&payload);
        assert_eq!(liquid.chunks_with_liquid(), 1);
        let sheet = &liquid.chunk(14)[0];
        assert_eq!(sheet.liquid_type, 5);
        assert_eq!(sheet.vertex_format, VertexFormat::HeightDepth);
        // No vertex data: flat is a statement, not a missing read.
        assert!(sheet.heights.is_empty());
        // No bitmap: every cell, rather than none.
        assert!(sheet.exists.is_empty());
        assert!(sheet.cell_exists(0, 0) && sheet.cell_exists(7, 7));
        assert!(!sheet.cell_exists(8, 0), "outside the rectangle");

        let origin = [-8533.0, -1600.0, 100.0];
        // Both corners of the chunk, and a point in the middle.
        for (u, v) in [(0.0, 0.0), (4.0, 4.0), (8.0, 8.0)] {
            let at = sheet.height_at(
                origin,
                origin[0] - u * crate::UNIT_SIZE,
                origin[1] - v * crate::UNIT_SIZE,
            );
            assert_eq!(at, Some(143.99), "at {u},{v}");
        }
        // Off the chunk entirely: the axes run inwards, so the far side is out.
        assert!(sheet.height_at(origin, origin[0] + 1.0, origin[1]).is_none());
    }

    /// An empty layer count is water-free, and reading it must not invent a
    /// sheet from whatever the offsets happen to point at.
    #[test]
    fn chunks_without_water_stay_empty() {
        let payload = flat_tile(5, 10.0, 14);
        let liquid = TileLiquid::parse(&payload);
        assert!(liquid.chunk(0).is_empty());
        assert!(liquid.chunk(255).is_empty());
        assert!(!liquid.is_empty(), "the tile as a whole does have water");
        assert!(TileLiquid::parse(&[]).is_empty());
    }

    /// A sheet stored with real per-vertex heights interpolates between them,
    /// and reads its grid at `width + 1`.
    ///
    /// The stride is the point. A 2x2 rectangle has a 3x3 vertex grid, and
    /// reading it at stride 2 puts the second row's first sample where the
    /// first row's third belongs -- which still produces a smooth-looking
    /// surface, tilted the wrong way.
    #[test]
    fn per_vertex_heights_use_a_grid_one_larger_than_the_rectangle() {
        let mut payload = vec![0u8; crate::CHUNK_COUNT * HEADER_SIZE];
        let instances_at = payload.len();
        payload[0..4].copy_from_slice(&(instances_at as u32).to_le_bytes());
        payload[4..8].copy_from_slice(&1u32.to_le_bytes());

        let vertices_at = instances_at + INSTANCE_SIZE;
        let mut instance = Vec::new();
        instance.extend_from_slice(&5u16.to_le_bytes());
        instance.extend_from_slice(&0u16.to_le_bytes());
        instance.extend_from_slice(&0f32.to_le_bytes());
        instance.extend_from_slice(&8f32.to_le_bytes());
        instance.extend_from_slice(&[0, 0, 2, 2]);
        instance.extend_from_slice(&0u32.to_le_bytes());
        instance.extend_from_slice(&(vertices_at as u32).to_le_bytes());
        payload.extend_from_slice(&instance);

        // A 3x3 grid sloping along the major axis only: 0, 4, 8 per row.
        for j in 0..3 {
            for _ in 0..3 {
                payload.extend_from_slice(&((j as f32) * 4.0).to_le_bytes());
            }
        }

        let liquid = TileLiquid::parse(&payload);
        let sheet = &liquid.chunk(0)[0];
        assert_eq!(sheet.heights.len(), 9);
        assert_eq!(sheet.vertex_height(0, 0), 0.0);
        assert_eq!(sheet.vertex_height(2, 1), 4.0);
        assert_eq!(sheet.vertex_height(0, 2), 8.0);

        let origin = [0.0, 0.0, 0.0];
        // Half a cell in along the major axis is half way to the next row.
        let at = sheet
            .height_at(origin, -0.5 * crate::UNIT_SIZE, -1.0 * crate::UNIT_SIZE)
            .expect("inside the sheet");
        assert!((at - 2.0).abs() < 1e-3, "got {at}");
        // The minor axis is flat, so moving along it changes nothing.
        let across = sheet
            .height_at(origin, -0.5 * crate::UNIT_SIZE, -1.7 * crate::UNIT_SIZE)
            .expect("inside the sheet");
        assert!((across - at).abs() < 1e-3, "the flat axis moved: {across}");
    }

    /// A rectangle that does not start at the chunk's corner answers only over
    /// its own footprint -- and it is the offsets, not the size, that decide
    /// where that is.
    #[test]
    fn an_offset_rectangle_covers_only_its_own_cells() {
        let mut payload = vec![0u8; crate::CHUNK_COUNT * HEADER_SIZE];
        let instances_at = payload.len();
        payload[0..4].copy_from_slice(&(instances_at as u32).to_le_bytes());
        payload[4..8].copy_from_slice(&1u32.to_le_bytes());

        let mut instance = Vec::new();
        instance.extend_from_slice(&5u16.to_le_bytes());
        instance.extend_from_slice(&0u16.to_le_bytes());
        instance.extend_from_slice(&12f32.to_le_bytes());
        instance.extend_from_slice(&12f32.to_le_bytes());
        // Starts three cells along the minor axis and five along the major.
        instance.extend_from_slice(&[3, 5, 2, 2]);
        instance.extend_from_slice(&0u32.to_le_bytes());
        instance.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&instance);

        let liquid = TileLiquid::parse(&payload);
        let sheet = &liquid.chunk(0)[0];
        let origin = [0.0, 0.0, 0.0];
        let at = |major: f32, minor: f32| {
            sheet.height_at(
                origin,
                -major * crate::UNIT_SIZE,
                -minor * crate::UNIT_SIZE,
            )
        };
        // Inside: major 5..7, minor 3..5.
        assert_eq!(at(5.5, 3.5), Some(12.0));
        assert_eq!(at(6.5, 4.5), Some(12.0));
        // Outside on each axis separately, which is what catches a transposed
        // rectangle: swapping the two offsets would answer at (3.5, 5.5).
        assert_eq!(at(3.5, 5.5), None, "the offsets are not interchangeable");
        assert_eq!(at(1.0, 4.0), None);
        assert_eq!(at(6.0, 1.0), None);
    }

    /// A bitmap with holes in it reports them, and a hole is `None` rather
    /// than a height -- the same distinction the terrain draws between a hole
    /// and ground at some altitude.
    #[test]
    fn a_cell_the_bitmap_clears_carries_no_liquid() {
        let mut payload = vec![0u8; crate::CHUNK_COUNT * HEADER_SIZE];
        let instances_at = payload.len();
        payload[0..4].copy_from_slice(&(instances_at as u32).to_le_bytes());
        payload[4..8].copy_from_slice(&1u32.to_le_bytes());

        let bitmap_at = instances_at + INSTANCE_SIZE;
        let mut instance = Vec::new();
        instance.extend_from_slice(&5u16.to_le_bytes());
        instance.extend_from_slice(&0u16.to_le_bytes());
        instance.extend_from_slice(&3f32.to_le_bytes());
        instance.extend_from_slice(&3f32.to_le_bytes());
        instance.extend_from_slice(&[0, 0, 2, 2]);
        instance.extend_from_slice(&(bitmap_at as u32).to_le_bytes());
        instance.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&instance);
        // Four cells; clear the second.
        payload.push(0b1101);

        let liquid = TileLiquid::parse(&payload);
        let sheet = &liquid.chunk(0)[0];
        assert_eq!(sheet.exists.len(), 4);
        assert!(sheet.cell_exists(0, 0));
        assert!(!sheet.cell_exists(1, 0), "the cleared cell");
        assert!(sheet.cell_exists(0, 1) && sheet.cell_exists(1, 1));

        let origin = [0.0, 0.0, 0.0];
        let at = |major: f32, minor: f32| {
            sheet.height_at(origin, -major * crate::UNIT_SIZE, -minor * crate::UNIT_SIZE)
        };
        assert_eq!(at(0.5, 0.5), Some(3.0));
        assert_eq!(at(0.5, 1.5), None, "over the cleared cell");
        assert_eq!(at(1.5, 1.5), Some(3.0));
    }

    /// A rectangle that would run off the chunk is a misread offset, and is
    /// refused rather than used to size an enormous read.
    #[test]
    fn an_impossible_rectangle_is_refused() {
        let mut payload = vec![0u8; crate::CHUNK_COUNT * HEADER_SIZE];
        let instances_at = payload.len();
        payload[0..4].copy_from_slice(&(instances_at as u32).to_le_bytes());
        payload[4..8].copy_from_slice(&1u32.to_le_bytes());

        let mut instance = Vec::new();
        instance.extend_from_slice(&5u16.to_le_bytes());
        instance.extend_from_slice(&0u16.to_le_bytes());
        instance.extend_from_slice(&0f32.to_le_bytes());
        instance.extend_from_slice(&0f32.to_le_bytes());
        instance.extend_from_slice(&[6, 0, 8, 8]); // 6 + 8 overruns the chunk
        instance.extend_from_slice(&0u32.to_le_bytes());
        instance.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&instance);

        assert!(TileLiquid::parse(&payload).chunk(0).is_empty());
    }
}
