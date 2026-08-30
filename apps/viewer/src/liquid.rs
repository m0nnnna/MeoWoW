//! Turning a tile's `MH2O` sheets into drawable geometry, and loading the art
//! that covers them.
//!
//! Two halves that live together because neither is useful alone: the geometry
//! is per tile and streams in and out with it, while the art is per *liquid
//! type* and is shared by every river in the world -- there are three types in
//! the whole of Azeroth and reloading thirty frames of `lake_a` for each of 529
//! wet tiles would be absurd.
//!
//! # Why the frames are bind groups rather than a texture array
//!
//! `LiquidType.dbc` names its art with a `%d`, and water has thirty numbered
//! frames. The original client plays them; it does not blend them. So the
//! animation is "which bind group is bound", chosen from the clock at draw
//! time, and the shader never learns that an animation exists. See
//! [`LiquidTypes::frame_at`].

use std::collections::HashMap;

use glam::Vec3;
use mpq::Chain;
use render::liquid::{LiquidRenderer, LiquidVertex, Look};
use render::{texture::upload_blp, Gpu, UploadedTexture};

/// Frames per second the surface art is played at.
///
/// Chosen by looking, like everything else in [`Look`]. Nothing in the tables
/// states a rate -- `LiquidType` has a `ParticleMovement` and a `ParticleScale`
/// and no clock at all.
const FRAMES_PER_SECOND: f32 = 18.0;

/// Depth, in `MH2O`'s own units, at which liquid reaches its full opacity.
///
/// **Chosen against a measurement rather than picked.** The depth bytes on
/// Northshire's tile run 0 at the waterline to 35 in the channel, so a
/// saturation point of 35 would leave the whole stream translucent and one of 1
/// would put a hard edge back along the bank. A third of the observed maximum
/// gives an opaque middle with a visible margin of shallows -- which is what a
/// bank looks like.
///
/// The first version of this treated the byte as `depth / 255`, which capped
/// the deepest water in Elwynn at 14% opacity and made the river as good as
/// invisible. That is why the number lives here with its evidence attached.
const DEPTH_FOR_FULL_OPACITY: f32 = 12.0;

/// The `LiquidType.material_id` whose art stores its pattern in the alpha
/// channel over a black RGB -- see [`Look::alpha_keyed`], which carries the
/// measurements.
const ALPHA_KEYED_MATERIAL: u32 = 1;

/// Most frames any one liquid's art runs to.
///
/// A ceiling rather than a count: the frames are probed by trying to read them
/// until one is missing, because the table says `%d` and never says how many.
const MAX_FRAMES: u32 = 60;

/// One contiguous run of triangles sharing a liquid type.
pub struct LiquidDraw {
    pub first_index: u32,
    pub index_count: u32,
    /// Row of `LiquidType.dbc`, which selects the art at draw time.
    pub liquid_type: u16,
}

/// One tile's liquid geometry.
pub struct LoadedLiquid {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub draws: Vec<LiquidDraw>,
    pub triangle_count: usize,
}

/// The animation frames and appearance of every liquid type seen so far.
///
/// Loaded on demand rather than up front: a session that never leaves Elwynn
/// needs `Slow Water` and nothing else, and the lava frames are several
/// megabytes that would otherwise be read at login for a player who will never
/// see a volcano.
#[derive(Default)]
pub struct LiquidTypes {
    table: Option<dbc::schema::LiquidType>,
    /// Whether reading the table has been attempted. Without this a failed
    /// read is retried for every sheet on every tile -- the same accounting
    /// `World::npc_looks` keeps, for the same reason.
    tried_table: bool,
    /// Per type id, one bind group per animation frame. An empty vector marks
    /// a type whose art could not be read, so it is not retried.
    frames: HashMap<u16, Vec<wgpu::BindGroup>>,
    /// Kept alive because the bind groups reference their views.
    #[allow(dead_code)]
    textures: Vec<UploadedTexture>,
    looks: HashMap<u16, Look>,
}

impl LiquidTypes {
    /// How a type is drawn, defaulting to water for a type never loaded.
    ///
    /// Water rather than nothing, because the fallback is reached by an
    /// unrecognised liquid, and drawing an unknown liquid as a harmless pond is
    /// the failure that misleads least -- see `LiquidCategory::Unknown`.
    pub fn look(&self, liquid_type: u16) -> Look {
        self.looks.get(&liquid_type).copied().unwrap_or(Look::WATER)
    }

    /// The bind group for a type at a moment, or `None` if its art is missing.
    ///
    /// `seconds` is a wall clock; which frame it lands on is arithmetic, so
    /// nothing here has any state to advance and a paused frame loop cannot
    /// desynchronise the animation from the world.
    pub fn frame_at(&self, liquid_type: u16, seconds: f32) -> Option<&wgpu::BindGroup> {
        let frames = self.frames.get(&liquid_type)?;
        if frames.is_empty() {
            return None;
        }
        let step = (seconds * FRAMES_PER_SECOND).max(0.0) as usize;
        frames.get(step % frames.len())
    }

    /// The category a type belongs to, which is what decides whether a
    /// character in it merely swims or also burns.
    pub fn category(&self, liquid_type: u16) -> dbc::schema::LiquidCategory {
        let Some(table) = self.table.as_ref() else {
            // Unknown, not water: claiming a liquid is harmless because a
            // table failed to load is exactly the fabrication this project
            // refuses elsewhere. `Unknown` draws as water and warns about
            // nothing, which is the same picture and an honest label.
            return dbc::schema::LiquidCategory::Unknown(liquid_type as u32);
        };
        table
            .iter()
            .find(|row| row.id() == liquid_type as u32)
            .map(|row| row.kind())
            .unwrap_or(dbc::schema::LiquidCategory::Unknown(liquid_type as u32))
    }

    /// Loads a type's art if it has not been loaded already.
    fn ensure(
        &mut self,
        gpu: &Gpu,
        renderer: &LiquidRenderer,
        chain: &mut Chain,
        liquid_type: u16,
    ) {
        if self.frames.contains_key(&liquid_type) {
            return;
        }
        if !self.tried_table {
            self.tried_table = true;
            self.table = chain
                .read(dbc::schema::LiquidType::PATH)
                .ok()
                .and_then(|bytes| dbc::schema::LiquidType::parse(&bytes).ok());
            if self.table.is_none() {
                tracing::warn!("LiquidType.dbc did not load; liquid will draw as plain water");
            }
        }

        let category = self.category(liquid_type);
        let mut look = match category {
            dbc::schema::LiquidCategory::Water => Look::WATER,
            dbc::schema::LiquidCategory::Ocean => Look::OCEAN,
            dbc::schema::LiquidCategory::Magma => Look::MAGMA,
            dbc::schema::LiquidCategory::Slime => Look::SLIME,
            dbc::schema::LiquidCategory::Unknown(_) => Look::WATER,
        };

        let row = self
            .table
            .as_ref()
            .and_then(|t| t.iter().find(|row| row.id() == liquid_type as u32));

        // **The material decides how the art is read, and the category only
        // predicts it.** The two agree across every row this build uses, but
        // they are different columns answering different questions -- what the
        // liquid *is* versus how its texture is *stored* -- and row 100's
        // `Basic Procedural Water` is material 3, a reflection map that is
        // neither. Reading the column that actually states it means an
        // unfamiliar material falls through to the colour rule rather than
        // being assumed alpha-keyed because it is called water.
        if let Some(row) = row {
            look.alpha_keyed = row.material_id() == ALPHA_KEYED_MATERIAL;
        }
        self.looks.insert(liquid_type, look);

        let pattern = row.map(|row| row.texture().to_string()).unwrap_or_default();

        let mut views = Vec::new();
        if !pattern.is_empty() {
            for frame in 1..=MAX_FRAMES {
                let path = pattern.replace("%d", &frame.to_string());
                let Ok(bytes) = chain.read(&path) else { break };
                let Ok(parsed) = blp::Blp::parse(&bytes) else {
                    break;
                };
                let uploaded = upload_blp(gpu, &parsed, &path);
                views.push(renderer.bind_surface(gpu, &uploaded.view));
                self.textures.push(uploaded);
                // A pattern with no `%d` in it names one file, and probing
                // for a second would read the same texture sixty times.
                if !pattern.contains("%d") {
                    break;
                }
            }
        }
        if views.is_empty() {
            tracing::warn!(
                liquid_type,
                pattern = %pattern,
                "no surface art resolved; this liquid will not be drawn"
            );
        } else {
            tracing::debug!(liquid_type, frames = views.len(), "liquid art loaded");
        }
        self.frames.insert(liquid_type, views);
    }
}

/// Builds one tile's liquid geometry, or `None` when the tile is dry.
///
/// Two triangles per existing cell, wound to match the terrain's clockwise
/// front face -- a liquid sheet is drawn with culling off, so the winding does
/// not decide visibility here, but keeping it consistent means a future pass
/// that does cull sees the same faces everywhere.
pub fn build(
    gpu: &Gpu,
    renderer: &LiquidRenderer,
    chain: &mut Chain,
    types: &mut LiquidTypes,
    tile: &adt::Adt,
) -> Option<LoadedLiquid> {
    if tile.liquid.is_empty() {
        return None;
    }

    // Grouped by type so the draw list is one entry per type per tile rather
    // than one per sheet: a tile of coastline is 256 chunks of the same ocean,
    // and 256 bind-group swaps a frame for one texture would be silly.
    let mut by_type: HashMap<u16, (Vec<LiquidVertex>, Vec<u32>)> = HashMap::new();

    for (index, sheet) in tile.liquid.instances() {
        let Some(chunk) = tile.chunks.get(index) else {
            continue;
        };
        types.ensure(gpu, renderer, chain, sheet.liquid_type);
        let look = types.look(sheet.liquid_type);
        let entry = by_type.entry(sheet.liquid_type).or_default();

        for j in 0..sheet.height as usize {
            for i in 0..sheet.width as usize {
                if !sheet.cell_exists(i, j) {
                    continue;
                }
                let base = entry.0.len() as u32;
                // The cell's four corners, in the sheet's own vertex grid.
                for (di, dj) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (vi, vj) = (i + di, j + dj);
                    // Both axes run inwards from the chunk's origin corner --
                    // `y_offset` along world x and `x_offset` along world y,
                    // the pairing `LiquidInstance::height_at` documents and
                    // `wow-cli adt liquid` measured.
                    let major = sheet.y_offset as f32 + vj as f32;
                    let minor = sheet.x_offset as f32 + vi as f32;
                    let position = Vec3::new(
                        chunk.position[0] - major * adt::UNIT_SIZE,
                        chunk.position[1] - minor * adt::UNIT_SIZE,
                        sheet.vertex_height(vi, vj),
                    );
                    // One repeat of the art per cell, offset by where the cell
                    // sits in the chunk so neighbouring chunks tile
                    // continuously instead of each restarting the pattern.
                    let uv = [minor * 0.25, major * 0.25];
                    // Deep water is opaque and the shallows fade out.
                    let depth = sheet.vertex_depth(vi, vj);
                    let alpha = look.alpha * (depth / DEPTH_FOR_FULL_OPACITY).min(1.0);
                    entry.0.push(LiquidVertex {
                        position: position.into(),
                        uv_motion: [uv[0], uv[1], look.scroll, 0.0],
                        tint: [look.tint[0], look.tint[1], look.tint[2], alpha],
                        mode: [
                            look.emissive,
                            if look.alpha_keyed { 1.0 } else { 0.0 },
                        ],
                    });
                }
                // Corners were pushed as (0,0), (1,0), (0,1), (1,1).
                entry.1.extend_from_slice(&[
                    base,
                    base + 1,
                    base + 3,
                    base,
                    base + 3,
                    base + 2,
                ]);
            }
        }
    }

    finish(gpu, by_type)
}

/// Builds the liquid surfaces a placed `.wmo` declares -- fountain basins,
/// interior pools, canal and harbour water. `MH2O` on the terrain never
/// reaches under a `.wmo`, so without this every one of them is a dry hole.
///
/// `surfaces` are `(LiquidType.dbc row, wet-cell quads)` with the placement
/// transform already applied, so this only has to skin them the same way
/// [`build`] skins a terrain sheet. Each quad is `[bottom-left, bottom-right,
/// top-left, top-right]`.
pub fn build_wmo(
    gpu: &Gpu,
    renderer: &LiquidRenderer,
    chain: &mut Chain,
    types: &mut LiquidTypes,
    surfaces: &[(u16, Vec<[Vec3; 4]>)],
) -> Option<LoadedLiquid> {
    let mut by_type: HashMap<u16, (Vec<LiquidVertex>, Vec<u32>)> = HashMap::new();
    for (liquid_type, cells) in surfaces {
        if cells.is_empty() {
            continue;
        }
        types.ensure(gpu, renderer, chain, *liquid_type);
        let look = types.look(*liquid_type);
        let entry = by_type.entry(*liquid_type).or_default();
        for quad in cells {
            let base = entry.0.len() as u32;
            for corner in quad {
                // Keyed off world position so neighbouring cells tile
                // continuously rather than each restarting the pattern -- the
                // same reason `build` offsets its UVs by the cell's place in
                // the chunk. `MLIQ` carries no depth, so a pool is drawn at
                // full opacity throughout.
                let uv = [corner.x * 0.06, corner.y * 0.06];
                entry.0.push(LiquidVertex {
                    position: (*corner).into(),
                    uv_motion: [uv[0], uv[1], look.scroll, 0.0],
                    tint: [look.tint[0], look.tint[1], look.tint[2], look.alpha],
                    mode: [look.emissive, if look.alpha_keyed { 1.0 } else { 0.0 }],
                });
            }
            entry.1.extend_from_slice(&[base, base + 1, base + 3, base, base + 3, base + 2]);
        }
    }
    finish(gpu, by_type)
}

/// Packs one or more typed vertex/index runs into a single buffer pair, one
/// [`LiquidDraw`] per type. Shared by [`build`] and [`build_wmo`].
fn finish(
    gpu: &Gpu,
    by_type: HashMap<u16, (Vec<LiquidVertex>, Vec<u32>)>,
) -> Option<LoadedLiquid> {
    use wgpu::util::DeviceExt;

    let mut vertices: Vec<LiquidVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut draws = Vec::new();
    let mut ordered: Vec<(u16, (Vec<LiquidVertex>, Vec<u32>))> = by_type.into_iter().collect();
    // Sorted so draw order does not depend on hash iteration, which would make
    // two runs of `--screenshot` differ where sheets overlap.
    ordered.sort_by_key(|(id, _)| *id);
    for (liquid_type, (part_vertices, part_indices)) in ordered {
        if part_indices.is_empty() {
            continue;
        }
        let base = vertices.len() as u32;
        let first_index = indices.len() as u32;
        vertices.extend(part_vertices);
        indices.extend(part_indices.iter().map(|i| i + base));
        draws.push(LiquidDraw {
            first_index,
            index_count: indices.len() as u32 - first_index,
            liquid_type,
        });
    }
    if indices.is_empty() {
        return None;
    }

    Some(LoadedLiquid {
        triangle_count: indices.len() / 3,
        vertices: gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("liquid vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
        indices: gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("liquid indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
        draws,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame chosen is a function of the clock and nothing else, so it
    /// cannot drift, and it wraps rather than running off the end.
    ///
    /// Built by hand rather than from a GPU, because what is being asserted is
    /// arithmetic: the real `frame_at` needs bind groups, which need a device.
    #[test]
    fn the_frame_is_arithmetic_on_the_clock() {
        let frame_of = |seconds: f32, count: usize| {
            let step = (seconds * FRAMES_PER_SECOND).max(0.0) as usize;
            step % count
        };
        assert_eq!(frame_of(0.0, 30), 0);
        // Eighteen frames a second, so half a second is nine along.
        assert_eq!(frame_of(0.5, 30), 9);
        // And it wraps rather than ending: a river does not stop.
        assert_eq!(frame_of(30.0 / FRAMES_PER_SECOND, 30), 0);
        assert_eq!(frame_of(31.0 / FRAMES_PER_SECOND, 30), 1);
        // A clock that somehow goes backwards must not index negatively.
        assert_eq!(frame_of(-5.0, 30), 0);
    }

    /// A type never loaded draws as water rather than not at all.
    #[test]
    fn an_unknown_type_falls_back_to_water() {
        let types = LiquidTypes::default();
        assert_eq!(types.look(999), Look::WATER);
        assert!(types.frame_at(999, 0.0).is_none());
        // But it is *labelled* unknown, not labelled water: the drawing may
        // guess and the categorisation may not.
        assert_eq!(
            types.category(999),
            dbc::schema::LiquidCategory::Unknown(999)
        );
    }
}
