//! The world map: which page to draw, and its twelve tiles.
//!
//! The projection itself is not here. It lives in [`dbc::worldmap`], beside
//! the table it was measured against, and this module only chooses a page and
//! uploads its art -- the same split as `items.rs`, which resolves icons but
//! does not decide what an item is.
//!
//! **Twelve textures per page, cached by page.** A page is a megabyte of DXT1
//! and re-uploading it every frame would hitch continuously, so the tiles are
//! uploaded once and kept. Failures cache too: a tile that will not load will
//! not load on the next frame either, and retrying forever is the mistake
//! `Items::icon` already refuses.
//!
//! **A page with no art is still a page.** A missing texture leaves a hole in
//! the grid rather than shifting the others along, and the frame still draws
//! its markers over blank parchment -- because a zone whose art did not load
//! and a zone with nothing on it must not look the same.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dbc::worldmap::{Atlas, Page};
use mpq::Chain;
use render::{Gpu, UploadedTexture};
use ui::frames::world_map::TILE_COUNT;
// Leading `::`, because this binary has a module of its own called `world`.
use ::world::quest::{QuestPoi, QuestPoiSet};

/// Pages, their art, and the names to put on them.
#[derive(Default)]
pub struct Maps {
    atlas: Atlas,
    /// `AreaTable` id to the player-facing zone name. The page's own
    /// `directory` is an internal name -- `SwampOfSorrows`, `Ogrimmar` -- and
    /// putting that on screen would be showing the player a file path.
    zone_names: HashMap<u32, String>,
    /// Uploaded tiles per page id. A `None` is a tile that would not load.
    tiles: HashMap<u32, [Option<egui::TextureId>; TILE_COUNT]>,
    /// Kept alive because egui holds only the id.
    uploaded: Vec<UploadedTexture>,
}

impl Maps {
    /// Reads `WorldMapArea.dbc` and `AreaTable.dbc`.
    ///
    /// Infallible like the rest of the interface: with no game installation
    /// the atlas is empty, every position resolves to no page, and the map
    /// window says so instead of refusing to open.
    pub fn load(chain: &mut Chain) -> Self {
        use dbc::schema::{AreaTable, WorldMapArea};

        let started = std::time::Instant::now();
        let mut maps = Maps::default();
        if let Ok(bytes) = chain.read(WorldMapArea::PATH) {
            if let Ok(table) = WorldMapArea::parse(&bytes) {
                maps.atlas = Atlas::from_table(&table);
            }
        }
        if let Ok(bytes) = chain.read(AreaTable::PATH) {
            if let Ok(table) = AreaTable::parse(&bytes) {
                maps.zone_names = table
                    .iter()
                    .filter(|row| !row.name().is_empty())
                    .map(|row| (row.id(), row.name().to_string()))
                    .collect();
            }
        }
        tracing::info!(
            "world map pages loaded in {:?}: {} pages, {} area names",
            started.elapsed(),
            maps.atlas.pages().len(),
            maps.zone_names.len()
        );
        maps
    }

    /// The page to draw for a position, if any covers it.
    pub fn page_at(&self, map_id: u32, x: f32, y: f32) -> Option<&Page> {
        self.atlas.zone_page(map_id, x, y)
    }

    /// What to call a page.
    ///
    /// Falls back to the internal directory name rather than to nothing: a
    /// title of `SwampOfSorrows` is ugly and checkable, where a blank title
    /// says only that something went wrong somewhere.
    pub fn title(&self, page: &Page) -> String {
        self.zone_names
            .get(&page.area_id)
            .cloned()
            .unwrap_or_else(|| page.directory.clone())
    }

    /// The twelve tiles of a page, uploading them the first time it is drawn.
    pub fn tiles(
        &mut self,
        gpu: &Gpu,
        renderer: &mut egui_wgpu::Renderer,
        chain: &mut Chain,
        page: &Page,
    ) -> [Option<egui::TextureId>; TILE_COUNT] {
        if let Some(cached) = self.tiles.get(&page.id) {
            return *cached;
        }
        let mut ids: [Option<egui::TextureId>; TILE_COUNT] = Default::default();
        let mut missing = 0usize;
        for (index, slot) in ids.iter_mut().enumerate() {
            let path = page.tile_path(index + 1);
            *slot = (|| {
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
            if slot.is_none() {
                missing += 1;
            }
        }
        if missing > 0 {
            tracing::debug!(
                "world map page {} ({}): {missing} of {TILE_COUNT} tiles would not load",
                page.id,
                page.directory
            );
        }
        self.tiles.insert(page.id, ids);
        ids
    }
}

/// Where the realm says each quest's objectives are.
///
/// **Memory only, and deliberately unlike `world::QuestCache`.** A quest
/// description is a fact about the quest and keeps; a POI answer is not. The
/// server answers `CMSG_QUEST_POI_QUERY` **only for quests in the player's own
/// log**, and it answers a quest it has no markers for and a quest that is not
/// in the log with the same empty list -- so an empty answer is a statement
/// this client is not entitled to write down. Caching one would turn "you did
/// not have it then" into "it has no markers", permanently, on disk.
///
/// Entries are dropped when a quest leaves the log for the same reason: the
/// next time it is taken the markers have to be asked for again, because what
/// is held may have been the empty answer given while it was absent.
#[derive(Default)]
pub struct Objectives {
    /// Answered quests, **including the ones answered with nothing** -- the
    /// server sends a set per requested id, so an empty vector here is a real
    /// answer and is what stops the id being asked about again.
    markers: HashMap<u32, Vec<QuestPoi>>,
    /// Ids a request has gone out for and when, so a log of five quests does
    /// not send five requests a frame -- and so a reply that never arrives is
    /// eventually asked for again rather than leaving that quest silently
    /// unmarked for the session.
    asked: HashMap<u32, Instant>,
}

/// How long an unanswered request is left before it is sent again.
///
/// The quest cache gives up after ten seconds and draws "the realm would not
/// say"; there is nothing to draw here, so this retries instead. Long enough
/// that a slow realm is not asked twice, short enough that a lost reply costs
/// a pause rather than the session.
const POI_RETRY: Duration = Duration::from_secs(15);

impl Objectives {
    /// Up to `limit` log quests with no answer and no request outstanding,
    /// marked as asked. The caller must send them or call
    /// [`Objectives::give_up`].
    ///
    /// The same shape as `world::QuestCache::take_unknown`, and for the same
    /// reason: a store that hands out ids and marks them in flight is what
    /// stops a per-frame loop from asking the same thing sixty times a second.
    pub fn take_unknown(&mut self, log: &[u32], limit: usize, now: Instant) -> Vec<u32> {
        let mut out = Vec::new();
        for quest in log {
            if out.len() >= limit {
                break;
            }
            if *quest == 0 || self.markers.contains_key(quest) {
                continue;
            }
            let overdue = self
                .asked
                .get(quest)
                .is_none_or(|sent| now.duration_since(*sent) >= POI_RETRY);
            if overdue {
                self.asked.insert(*quest, now);
                out.push(*quest);
            }
        }
        out
    }

    /// Puts an id back after a send failed, so it is asked about again on the
    /// next frame rather than after the retry window.
    pub fn give_up(&mut self, quest: u32) {
        self.asked.remove(&quest);
    }

    /// Records what one `SMSG_QUEST_POI_QUERY_RESPONSE` said.
    pub fn insert(&mut self, sets: &[QuestPoiSet]) {
        for set in sets {
            self.asked.remove(&set.quest_id);
            self.markers.insert(set.quest_id, set.markers.clone());
        }
    }

    /// What the server said about one quest, empty when it has said nothing.
    pub fn markers(&self, quest: u32) -> &[QuestPoi] {
        self.markers.get(&quest).map_or(&[], Vec::as_slice)
    }

    /// Forgets every quest not in the log. See the type's own doc comment for
    /// why this is not just housekeeping.
    pub fn retain_log(&mut self, log: &[u32]) {
        self.markers.retain(|quest, _| log.contains(quest));
        self.asked.retain(|quest, _| log.contains(quest));
    }
}

/// One quest's markers and the name to put on them, for [`build_view`].
pub struct Objective<'a> {
    /// What a hovered marker says. The quest's title where the cache has one,
    /// and its id where it does not -- never an invented name.
    pub label: String,
    pub markers: &'a [QuestPoi],
}

/// Where the character is, for [`build_view`].
#[derive(Debug, Clone, Copy)]
pub struct Standing {
    pub map_id: u32,
    pub x: f32,
    pub y: f32,
    /// World heading, the one `MSG_MOVE_*` carries.
    pub orientation: f32,
}

/// Assembles what the map frame draws.
///
/// A free function rather than a method on the viewer because the caller holds
/// the renderer mutably already, and this needs the archive chain and the page
/// cache alongside it -- three disjoint fields, which is exactly the shape the
/// icon loaders use.
///
/// **Every failure has its own sentence.** Not logged in, logged in somewhere
/// no page covers, and a page whose art would not load are three different
/// situations, and a client that drew the same blank rectangle for all of them
/// would be saying nothing at all -- the same reason a quest log row
/// distinguishes "asking" from "no answer" from "no objectives".
pub fn build_view(
    maps: &mut Maps,
    gpu: &Gpu,
    renderer: &mut egui_wgpu::Renderer,
    chain: &mut Chain,
    standing: Option<Standing>,
    objectives: &[Objective<'_>],
) -> ui::MapView {
    let Some(at) = standing else {
        return ui::MapView {
            title: "World Map".into(),
            note: Some("not in the world yet".into()),
            ..Default::default()
        };
    };
    let Some(page) = maps.page_at(at.map_id, at.x, at.y).cloned() else {
        return ui::MapView {
            title: "World Map".into(),
            note: Some(format!(
                "no page covers map {} at {:.0}, {:.0}",
                at.map_id, at.x, at.y
            )),
            ..Default::default()
        };
    };

    let (u, v) = page.project(at.x, at.y);
    let tiles = maps.tiles(gpu, renderer, chain, &page);
    // Objectives first so the player's arrow is painted over them rather than
    // under: where you are is never the thing to obscure.
    let mut markers: Vec<ui::MapMarker> = objectives
        .iter()
        .flat_map(|objective| {
            objective
                .markers
                .iter()
                .filter_map(|poi| marker(&page, poi, &objective.label))
        })
        .collect();
    markers.push(ui::MapMarker {
        u,
        v,
        facing: screen_facing(at.orientation),
        kind: ui::MarkerKind::Player,
        label: String::new(),
        outline: Vec::new(),
    });
    ui::MapView {
        title: maps.title(&page),
        note: tiles
            .iter()
            .all(Option::is_none)
            .then(|| format!("no art for {}", page.directory)),
        tiles,
        markers,
    }
}

/// One server marker, projected onto a page, or `None` when it belongs to a
/// different page.
///
/// **The marker names its own page and this does not guess at one.**
/// `quest_poi.WorldMapAreaId` is a row in `WorldMapArea.dbc` -- the same id
/// space the pages are keyed by -- so "is this marker on the page being drawn"
/// is an equality rather than a containment test. Testing containment instead
/// would draw a Westfall objective on the Elwynn page wherever the two
/// rectangles overlap, and it would look entirely reasonable.
fn marker(page: &Page, poi: &QuestPoi, label: &str) -> Option<ui::MapMarker> {
    if poi.world_map_area_id != page.id || poi.points.is_empty() {
        return None;
    }
    let projected: Vec<(f32, f32)> = poi
        .points
        .iter()
        .map(|(x, y)| page.project(*x as f32, *y as f32))
        .collect();
    // The middle of the ring, which is where the pin and its hover text go.
    // For a single-point marker that is the point itself; for a region it is
    // an anchor for the label, and the ring beside it is what states the
    // extent -- a centroid drawn *instead* of the ring would be inventing a
    // precision the server did not send.
    let count = projected.len() as f32;
    let (u, v) = projected
        .iter()
        .fold((0.0, 0.0), |(su, sv), (u, v)| (su + u, sv + v));
    Some(ui::MapMarker {
        u: u / count,
        v: v / count,
        facing: 0.0,
        kind: ui::MarkerKind::Objective,
        label: label.to_string(),
        // A ring around a single point would be a dot drawn twice.
        outline: if projected.len() > 1 {
            projected
        } else {
            Vec::new()
        },
    })
}

/// Where the player's arrow points on a page, in screen radians with `0` up.
///
/// **It is the negative of the world heading, and that sign is derived rather
/// than tried.** The arrow is drawn pointing up and rotated, and this project
/// has already paid for a rotation chosen because a render looked right: the
/// ADT placement offset shipped at `-90`, was "corrected" to `+90` on the
/// strength of a screenshot, and both were ninety degrees wrong. An arrow has
/// the same problem in a worse form -- it looks like an arrow at every angle.
///
/// So: `world::motion::Direction::direction` moves the character by
/// `(cos o, sin o)` in `(x, y)`, which is the client's own walking code and
/// confirmed against a live realm. The page runs `+x` up and `+y` left, so
/// that heading is the screen direction `(-sin o, -cos o)`. egui rotates by
/// `(x cos f - y sin f, x sin f + y cos f)` with `y` pointing *down*, which
/// sends the shape's tip `(0, -1)` to `(sin f, -cos f)`. Equating the two
/// gives `sin f = -sin o` and `cos f = cos o`, so `f = -o`.
pub fn screen_facing(world_orientation: f32) -> f32 {
    -world_orientation
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Elwynn` page, id 30 -- the page every captured POI names.
    fn elwynn() -> Page {
        Page {
            id: 30,
            map_id: 0,
            area_id: 12,
            directory: "Elwynn".into(),
            x_max: -7939.583,
            x_min: -10254.166,
            y_max: 1535.417,
            y_min: -1935.417,
        }
    }

    fn poi(area: u32, points: &[(i32, i32)]) -> QuestPoi {
        QuestPoi {
            id: 0,
            objective_index: None,
            map_id: 0,
            world_map_area_id: area,
            floor_id: 0,
            points: points.to_vec(),
            unknown: (0, 1),
        }
    }

    /// Quest 783's real marker, from the capture in `world::quest`, against
    /// the place this project has logged in at more than any other: the
    /// objective is a few paces from the spawn, so its pin must be too.
    #[test]
    fn a_captured_objective_lands_where_the_spawn_does() {
        let page = elwynn();
        let marker = marker(&page, &poi(30, &[(-8903, -163)]), "A Threat Within").unwrap();
        let (spawn_u, spawn_v) = page.project(-8950.0, -132.0);
        assert!(
            (marker.u - spawn_u).abs() < 0.03 && (marker.v - spawn_v).abs() < 0.03,
            "the objective landed at ({:.3}, {:.3}), the spawn at ({spawn_u:.3}, {spawn_v:.3})",
            marker.u,
            marker.v
        );
        assert_eq!(marker.kind, ui::MarkerKind::Objective);
        assert_eq!(marker.label, "A Threat Within");
        assert!(
            marker.outline.is_empty(),
            "one point is a pin, not a ring drawn on top of itself"
        );
    }

    /// A marker states which page it belongs to, and that is not the page the
    /// player happens to be standing on. Quest 106's marker names area 39;
    /// drawn on Elwynn it would be a plausible-looking pin in the wrong zone.
    #[test]
    fn a_marker_for_another_page_is_not_drawn_on_this_one() {
        assert!(marker(&elwynn(), &poi(39, &[(-9930, 500)]), "elsewhere").is_none());
        // And an empty polygon is nothing to draw rather than a pin at the
        // origin, which is a real corner of every page.
        assert!(marker(&elwynn(), &poi(30, &[]), "nowhere").is_none());
    }

    /// A region keeps its ring, and the pin sits inside it.
    #[test]
    fn a_polygon_keeps_its_outline_and_pins_the_middle() {
        let page = elwynn();
        let square = [
            (-9000, -100),
            (-9000, -200),
            (-8900, -200),
            (-8900, -100),
        ];
        let marker = marker(&page, &poi(30, &square), "somewhere").unwrap();
        assert_eq!(marker.outline.len(), 4);
        let (mid_u, mid_v) = page.project(-8950.0, -150.0);
        assert!((marker.u - mid_u).abs() < 1e-3 && (marker.v - mid_v).abs() < 1e-3);
    }

    /// The store must not remember an empty answer given while a quest was out
    /// of the log, which is the one way it could lie permanently. See
    /// [`Objectives`].
    #[test]
    fn a_quest_that_leaves_the_log_is_forgotten_and_asked_about_again() {
        let now = Instant::now();
        let mut objectives = Objectives::default();
        assert_eq!(objectives.take_unknown(&[783, 333], 4, now), vec![783, 333]);
        // Asked once, so not asked again while the answer is still coming.
        assert!(objectives.take_unknown(&[783, 333], 4, now).is_empty());
        objectives.insert(&[QuestPoiSet {
            quest_id: 783,
            markers: vec![poi(30, &[(-8903, -163)])],
        }]);
        assert_eq!(objectives.markers(783).len(), 1);
        // And an answer is final: it is not re-asked when the window passes.
        assert!(objectives
            .take_unknown(&[783], 4, now + POI_RETRY * 2)
            .is_empty());

        // Handed in: the markers go, and so does the memory of having asked.
        objectives.retain_log(&[333]);
        assert!(objectives.markers(783).is_empty());
        assert_eq!(objectives.take_unknown(&[333, 783], 4, now), vec![783]);
    }

    /// **An empty answer is an answer.** The server sends a set per requested
    /// id, so a quest with no markers comes back as an empty list -- and if
    /// that did not count, every quest without map markers would be asked
    /// about again every fifteen seconds forever.
    #[test]
    fn a_quest_the_realm_has_no_markers_for_is_not_asked_about_twice() {
        let now = Instant::now();
        let mut objectives = Objectives::default();
        assert_eq!(objectives.take_unknown(&[38], 4, now), vec![38]);
        objectives.insert(&[QuestPoiSet {
            quest_id: 38,
            markers: Vec::new(),
        }]);
        assert!(objectives
            .take_unknown(&[38], 4, now + POI_RETRY * 2)
            .is_empty());
    }

    /// A reply that never arrives has to be asked for again, or that quest is
    /// silently unmarked for the whole session -- the failure a store that
    /// only remembers "asked" cannot recover from.
    #[test]
    fn an_unanswered_request_is_sent_again_once_the_window_passes() {
        let now = Instant::now();
        let mut objectives = Objectives::default();
        assert_eq!(objectives.take_unknown(&[783], 4, now), vec![783]);
        assert!(objectives
            .take_unknown(&[783], 4, now + POI_RETRY / 2)
            .is_empty());
        assert_eq!(
            objectives.take_unknown(&[783], 4, now + POI_RETRY),
            vec![783]
        );
    }

    /// A send that failed has to leave the id askable immediately, rather than
    /// waiting out a window it never actually spent on the wire.
    #[test]
    fn giving_up_puts_an_id_back() {
        let now = Instant::now();
        let mut objectives = Objectives::default();
        assert_eq!(objectives.take_unknown(&[783], 4, now), vec![783]);
        objectives.give_up(783);
        assert_eq!(objectives.take_unknown(&[783], 4, now), vec![783]);
    }

    /// The four cardinal headings, each asserted as a screen *direction*.
    ///
    /// Checking the angle against a formula would only restate the formula;
    /// what has to be true is that facing north draws an arrow pointing up the
    /// page, and there is exactly one sign for which all four hold.
    #[test]
    fn the_arrow_points_where_the_character_faces() {
        use std::f32::consts::{FRAC_PI_2, PI};

        // Where the tip of an up-pointing shape lands once egui has rotated
        // it -- see `world_map::draw`, which builds the same point.
        let tip = |orientation: f32| {
            let (sin, cos) = screen_facing(orientation).sin_cos();
            (sin, -cos)
        };
        let close = |(dx, dy): (f32, f32), want: (f32, f32)| {
            (dx - want.0).abs() < 1e-5 && (dy - want.1).abs() < 1e-5
        };

        // North is +x and up the page is -y on screen.
        assert!(close(tip(0.0), (0.0, -1.0)), "north: {:?}", tip(0.0));
        // West is +y and left is -x.
        assert!(
            close(tip(FRAC_PI_2), (-1.0, 0.0)),
            "west: {:?}",
            tip(FRAC_PI_2)
        );
        assert!(close(tip(PI), (0.0, 1.0)), "south: {:?}", tip(PI));
        assert!(
            close(tip(3.0 * FRAC_PI_2), (1.0, 0.0)),
            "east: {:?}",
            tip(3.0 * FRAC_PI_2)
        );
    }
}
