//! The minimap: which terrain tiles are under the viewport, and their art.
//!
//! **The same split as [`crate::maps`], for the same reason.** Turning a world
//! position into a place on the disc is done here, once, and the `ui` crate is
//! handed fractions. The projection is not [`dbc::worldmap`]'s -- a minimap
//! does not sit on a page -- but it uses the identical axis convention (`+x`
//! up the screen, `+y` to the left), and [`crate::maps::screen_facing`] is
//! reused rather than re-derived so the two arrows cannot end up pointing
//! different ways.
//!
//! # The art is one picture per terrain tile, named by its own hash
//!
//! [`adt::minimap`] holds the index and what was measured about it. Two facts
//! shape this module: the pictures are keyed by MD5 of their contents, so
//! **one file can serve many tiles** and the cache is keyed by the resolved
//! path rather than by `(map, x, y)`; and the tile's pixel layout was fitted
//! rather than assumed, so the placement below is the one two independent
//! experiments agreed on (`wow-cli minimap orient` and `minimap seams`).
//!
//! # Nothing here is cached to disk and the memory cache never shrinks
//!
//! A tile is 33KB of DXT1 and stays as BC1 on the GPU, so a session that
//! walked every tile of Azeroth would hold about 22MB. That is small enough
//! not to evict and be wrong about, and it is recorded here rather than
//! discovered later.

use std::collections::HashMap;

use mpq::Chain;
use render::{Gpu, UploadedTexture};

use crate::maps::{screen_facing, Objective, PartyMemberPin, Standing};

/// Tile art, and the index that finds it.
#[derive(Default)]
pub struct Minimap {
    index: adt::minimap::Translate,
    /// Uploaded art by archive path. **By path, not by tile**: the index maps
    /// 18,644 tiles onto 14,420 files, so two tiles that look the same share
    /// one picture, and keying this by `(map, x, y)` would upload the flat
    /// black tile a thousand times.
    art: HashMap<String, Option<egui::TextureId>>,
    /// Kept alive because egui holds only the id.
    uploaded: Vec<UploadedTexture>,
    /// The last area name that resolved.
    ///
    /// **Held rather than recomputed to nothing.** `Streaming::area_at`
    /// answers `None` while the tile under the player is still loading, and a
    /// header that blanked for those frames would read as walking out of the
    /// zone and back in -- the same reason `crate::sound` refuses to treat a
    /// missing area as silence.
    last_title: String,
}

impl Minimap {
    /// Reads `md5translate.trs`.
    ///
    /// Infallible like the rest of the interface: with no installation the
    /// index is empty, every tile resolves to nothing, and the frame says so
    /// rather than refusing to draw.
    pub fn load(chain: &mut Chain) -> Self {
        let started = std::time::Instant::now();
        let index = match chain.read(adt::minimap::TRANSLATE_PATH) {
            Ok(bytes) => adt::minimap::Translate::parse(&String::from_utf8_lossy(&bytes)),
            Err(error) => {
                tracing::warn!(
                    "no minimap index ({}): {error}",
                    adt::minimap::TRANSLATE_PATH
                );
                adt::minimap::Translate::default()
            }
        };
        tracing::info!(
            "minimap index loaded in {:?}: {} tiles named",
            started.elapsed(),
            index.len()
        );
        Self {
            index,
            ..Default::default()
        }
    }

    /// One tile's art, uploading it the first time it is drawn.
    ///
    /// Failures are cached too, on the same reasoning as the world map's
    /// tiles: a file that would not load this frame will not load next frame
    /// either, and retrying at sixty hertz is the mistake `Items::icon`
    /// already refuses.
    fn tile(
        &mut self,
        gpu: &Gpu,
        renderer: &mut egui_wgpu::Renderer,
        chain: &mut Chain,
        map: &str,
        x: usize,
        y: usize,
    ) -> Option<egui::TextureId> {
        let path = self.index.tile_path(map, x, y)?;
        if let Some(cached) = self.art.get(&path) {
            return *cached;
        }
        let loaded = (|| {
            let bytes = chain.read(&path).ok()?;
            let image = blp::Blp::parse(&bytes).ok()?;
            let uploaded = render::texture::upload_blp(gpu, &image, &path);
            let id = renderer.register_native_texture(
                &gpu.device,
                &uploaded.view,
                wgpu::FilterMode::Linear,
            );
            self.uploaded.push(uploaded);
            Some(id)
        })();
        if loaded.is_none() {
            tracing::debug!("minimap tile {map} {x},{y} ({path}) would not load");
        }
        self.art.insert(path, loaded);
        loaded
    }

    /// Assembles what the frame draws.
    ///
    /// `range` is how many world units the disc spans; `title` is the area the
    /// character is standing in, which the caller reads off the terrain rather
    /// than off a table -- the ground knows which sub-zone it is, and that is
    /// finer than any zone id the server replicates.
    #[allow(clippy::too_many_arguments)]
    pub fn build_view(
        &mut self,
        gpu: &Gpu,
        renderer: &mut egui_wgpu::Renderer,
        chain: &mut Chain,
        standing: Option<Standing>,
        map_directory: &str,
        title: Option<&str>,
        range: f32,
        objectives: &[Objective<'_>],
        party: &[PartyMemberPin],
    ) -> ui::MinimapView {
        if let Some(title) = title.filter(|t| !t.is_empty()) {
            self.last_title = title.to_string();
        }
        let name = if self.last_title.is_empty() {
            "Minimap".to_string()
        } else {
            self.last_title.clone()
        };

        let Some(at) = standing else {
            return ui::MinimapView {
                title: name,
                note: Some("not in the world yet".into()),
                ..Default::default()
            };
        };
        // **One viewport, asked twice.** The tiles and the blips are placed
        // by the same object rather than by two calls that happen to agree --
        // the rule the picking ray is written to, and the reason this lives in
        // `adt` where `wow-cli minimap stitch` can draw a picture from it.
        let placed = adt::minimap::Viewport::new(at.x, at.y, range);
        let project = |x: f32, y: f32| placed.project(x, y);
        let mut tiles = Vec::new();
        for (x, y) in placed.tiles_touching() {
            let Some(texture) = self.tile(gpu, renderer, chain, map_directory, x, y) else {
                continue;
            };
            tiles.push(ui::MinimapTile {
                texture,
                rect: placed.tile_rect(x, y),
            });
        }

        // Objectives, then party members, then the player's arrow last so it
        // is painted over both -- the same order the world map uses, and for
        // the same reason: where you are is never the thing to obscure.
        let mut markers: Vec<ui::MapMarker> = Vec::new();
        for objective in objectives {
            for poi in objective.markers {
                // **A marker names its own map and this does not guess.** The
                // world map asks the same question as a page equality; there
                // is no page here, so it is the `Map.dbc` id -- and it has to
                // be asked, because two continents share a coordinate range
                // and a Kalimdor objective at the same numbers would land on
                // a disc over Elwynn looking entirely reasonable.
                if poi.points.is_empty() || poi.map_id != at.map_id {
                    continue;
                }
                let projected: Vec<(f32, f32)> = poi
                    .points
                    .iter()
                    .map(|(x, y)| project(*x as f32, *y as f32))
                    .collect();
                let count = projected.len() as f32;
                let (u, v) = projected
                    .iter()
                    .fold((0.0, 0.0), |(su, sv), (u, v)| (su + u, sv + v));
                markers.push(ui::MapMarker {
                    u: u / count,
                    v: v / count,
                    facing: 0.0,
                    kind: ui::MarkerKind::Objective,
                    label: objective.label.clone(),
                    // A ring around a single point would be a dot drawn twice
                    // -- and on a disc two hundred units across, a region is
                    // usually bigger than the whole picture, so the ring is
                    // the only honest thing to draw for one. See
                    // `crate::maps::marker`.
                    outline: if projected.len() > 1 {
                        projected
                    } else {
                        Vec::new()
                    },
                });
            }
        }
        for pin in party {
            let (u, v) = project(pin.x, pin.y);
            markers.push(ui::MapMarker {
                u,
                v,
                facing: 0.0,
                kind: ui::MarkerKind::PartyMember,
                label: pin.name.clone(),
                outline: Vec::new(),
            });
        }
        markers.push(ui::MapMarker {
            u: 0.5,
            v: 0.5,
            facing: screen_facing(at.orientation),
            kind: ui::MarkerKind::Player,
            label: String::new(),
            outline: Vec::new(),
        });

        ui::MinimapView {
            title: name,
            // **"No art here" and "nothing here" are different statements.**
            // A tile with no picture leaves the parchment showing, and saying
            // nothing about it would make an unmapped instance look like a
            // client that had stopped drawing.
            note: tiles
                .is_empty()
                .then(|| format!("no minimap art for {map_directory}")),
            tiles,
            markers,
        }
    }
}

#[cfg(test)]
mod tests {

    /// The Northshire human spawn -- the position this project has logged in
    /// at more often than any other, and the one the world map's own hand
    /// check uses.
    const SPAWN: (f32, f32) = (-8950.0, -132.0);

    fn placed(x: f32, y: f32, range: f32) -> adt::minimap::Viewport {
        adt::minimap::Viewport::new(x, y, range)
    }

    /// The player is at the middle, and the axes run the way a map does.
    #[test]
    fn the_player_is_the_centre() {
        let placed = placed(SPAWN.0, SPAWN.1, 200.0);
        let (u, v) = placed.project(SPAWN.0, SPAWN.1);
        assert!((u - 0.5).abs() < 1e-4 && (v - 0.5).abs() < 1e-4, "{u} {v}");
        // North is up: a larger world x is nearer the top.
        let (_, north) = placed.project(SPAWN.0 + 50.0, SPAWN.1);
        assert!(north < 0.5, "north must be up, got {north}");
        // West is left: a larger world y is nearer the left edge.
        let (west, _) = placed.project(SPAWN.0, SPAWN.1 + 50.0);
        assert!(west < 0.5, "west must be left, got {west}");
        // And the scale is the range: half a range away is the edge.
        let (edge, _) = placed.project(SPAWN.0, SPAWN.1 - 100.0);
        assert!((edge - 1.0).abs() < 1e-4, "{edge}");
    }

    /// The spawn is inside `Azeroth_32_48`, which is the tile the world map's
    /// own checks and this project's login logs have both named for years.
    #[test]
    fn the_spawn_tile_is_32_48() {
        let placed = placed(SPAWN.0, SPAWN.1, 200.0);
        assert!(
            placed.tiles_touching().contains(&(32, 48)),
            "{:?}",
            placed.tiles_touching()
        );
    }

    /// A viewport smaller than a tile still overlaps four of them when it sits
    /// on a corner, and that is the case the placement has to get right --
    /// three tiles drawn correctly and a fourth in the wrong place looks like
    /// a seam rather than like a bug.
    #[test]
    fn a_viewport_on_a_corner_touches_four_tiles() {
        // The corner where 32,48 meets 33,49: the largest x and y of the
        // next tile along in both.
        let corner_x = (32.0 - 49.0) * adt::TILE_SIZE;
        let corner_y = (32.0 - 33.0) * adt::TILE_SIZE;
        let placed = placed(corner_x, corner_y, 200.0);
        let touching = placed.tiles_touching();
        for tile in [(32, 48), (33, 48), (32, 49), (33, 49)] {
            assert!(touching.contains(&tile), "{tile:?} missing from {touching:?}");
        }
    }

    /// A tile's rectangle is one range wide per range of world, and its top
    /// left is the corner with the largest world coordinates -- the reading
    /// the two experiments settled on.
    #[test]
    fn a_tile_is_placed_by_its_largest_corner() {
        let range = 200.0;
        let placed = placed(SPAWN.0, SPAWN.1, range);
        let [u0, v0, u1, v1] = placed.tile_rect(32, 48);
        assert!(u1 > u0 && v1 > v0, "{u0} {v0} {u1} {v1}");
        let want = adt::TILE_SIZE / range;
        assert!((u1 - u0 - want).abs() < 1e-3, "{}", u1 - u0);
        assert!((v1 - v0 - want).abs() < 1e-3, "{}", v1 - v0);
        // The player is inside their own tile's rectangle.
        assert!(u0 < 0.5 && u1 > 0.5 && v0 < 0.5 && v1 > 0.5);
    }

    /// Two tiles side by side meet exactly, with no gap and no overlap. A half
    /// tile of error here draws a plausible map with a seam nobody would
    /// call a bug.
    #[test]
    fn neighbouring_tiles_meet() {
        let placed = placed(SPAWN.0, SPAWN.1, 200.0);
        let left = placed.tile_rect(32, 48);
        let right = placed.tile_rect(33, 48);
        assert!((left[2] - right[0]).abs() < 1e-3, "{left:?} {right:?}");
        let below = placed.tile_rect(32, 49);
        assert!((left[3] - below[1]).abs() < 1e-3, "{left:?} {below:?}");
    }
}
