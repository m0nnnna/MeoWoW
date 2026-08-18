//! Turning a world position into a place on a map page.
//!
//! Lives beside the tables for the same reason [`crate::light`] does: the tool
//! that checks this projection must not compute it a second way, or it stops
//! being evidence about the first.
//!
//! # What was measured, and what it cost to be sure
//!
//! A projection has four plausible readings -- the horizontal axis can run
//! either way and so can the vertical -- and **all four produce a picture**.
//! That is the shape this project has paid for before: the ADT placement
//! rotation shipped at `-90`, was "fixed" to `+90` because a render looked
//! better, and both were ninety degrees wrong, because a building has four
//! sides and every rotation shows a door to somebody. Choosing between
//! candidates by eye finds the nicer one, not the right one.
//!
//! So the reading here was settled against something that could refute it.
//! [`crate::schema::WorldMapOverlay`] states, in page pixels, where a named
//! area sits on its page; the terrain files state, in world coordinates, which
//! area every chunk of ground belongs to. Those come from different files
//! authored by different tools, so agreement is evidence.
//!
//! Two hand checks first, each with a margin no measurement error could
//! cover:
//!
//! - The `Stormwind` page's own world box, projected onto the `Elwynn` page,
//!   lands at pixels `x -54..447, y 16..350`. Elwynn's `STORMWIND` overlay
//!   texture occupies `x 0..485, y 0..405`. Every other orientation puts it
//!   more than five hundred pixels away, on a canvas a thousand wide.
//! - Northshire's human spawn, `(-8950, -132)`, projects to `(481, 292)`.
//!   Elwynn's `NORTHSHIREVALLEY` overlay's clickable box is `x 425..600,
//!   y 190..375`.
//!
//! Then at population scale: `wow-cli map calibrate` fits the pixel position
//! of every overlay against the centroid of the terrain chunks carrying its
//! area id, and reports the fitted slope. It presupposes neither the
//! orientation (a flipped axis fits a *negative* slope) nor the canvas size
//! (which is whatever the slope's magnitude comes out as).
//!
//! # The page is bigger than the picture
//!
//! A page is twelve 256x256 tiles in four columns and three rows, so the
//! image is 1024x768 -- but the art only fills part of it. **The tiles said so
//! themselves**: on `Elwynn`, tiles 4, 8, 9, 10, 11 and 12 carry an alpha
//! channel and tiles 1, 2, 3, 5, 6 and 7 do not, which is exactly the right
//! column and the bottom row. Padding is transparent; content is not.

use crate::schema::{WorldMapArea, WorldMapAreaRow, WorldMapOverlay, WorldMapOverlayRow};

/// Tiles across a page, and down it.
pub const TILE_COLUMNS: usize = 4;
/// Tiles down a page.
pub const TILE_ROWS: usize = 3;
/// Edge of one tile, in texels.
pub const TILE_TEXELS: usize = 256;

/// Page width in the pixel space [`crate::schema::WorldMapOverlay`] uses.
///
/// Not the same as `TILE_COLUMNS * TILE_TEXELS`: the twelve tiles make a
/// 1024x768 image and the art stops short of its right and bottom edges. See
/// the module docs for how the tiles announced that themselves.
pub const PAGE_WIDTH: f32 = 1002.0;
/// Page height in the same space.
pub const PAGE_HEIGHT: f32 = 668.0;

/// One drawable map page and the world rectangle it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Row id in `WorldMapArea.dbc`, which is what
    /// `SMSG_QUEST_POI_QUERY_RESPONSE` names its markers by.
    pub id: u32,
    /// `Map.dbc` row.
    pub map_id: u32,
    /// `AreaTable.dbc` zone, or 0 for a whole-continent page.
    pub area_id: u32,
    /// Folder under `Interface\WorldMap`.
    pub directory: String,
    pub x_max: f32,
    pub x_min: f32,
    pub y_max: f32,
    pub y_min: f32,
}

impl Page {
    /// Whether the page states a rectangle at all.
    ///
    /// Three of the 108 rows in this build carry all-zero bounds --
    /// `Dalaran`, `TheNexus` and `UtgardeKeep` -- and a zero-extent page
    /// cannot be projected onto. They are not parse failures; the table simply
    /// does not say where those pages sit, so nothing here pretends to know.
    pub fn has_bounds(&self) -> bool {
        self.x_max > self.x_min && self.y_max > self.y_min
    }

    /// World units the page spans north to south, times what it spans east to
    /// west. Used to pick the *smallest* page containing a point, so a city
    /// wins over the zone around it.
    pub fn world_area(&self) -> f32 {
        (self.x_max - self.x_min) * (self.y_max - self.y_min)
    }

    /// Whether a world position falls inside this page's rectangle.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.has_bounds()
            && (self.x_min..=self.x_max).contains(&x)
            && (self.y_min..=self.y_max).contains(&y)
    }

    /// Where a world position falls on the page, as fractions with `(0, 0)` at
    /// the **top left** of the picture.
    ///
    /// Outside the page's rectangle the fractions run past `0..1` rather than
    /// clamping, because a caller drawing a pin needs to know it is off the
    /// edge and in which direction. Callers that want it on the page should
    /// ask [`Page::contains`] first.
    pub fn project(&self, x: f32, y: f32) -> (f32, f32) {
        // West is +y and north is +x, so the left edge is the largest y and
        // the top edge is the largest x. That is the whole of the decision
        // this module exists to make; see the module docs for how it was
        // settled rather than assumed.
        let u = (self.y_max - y) / (self.y_max - self.y_min);
        let v = (self.x_max - x) / (self.x_max - self.x_min);
        (u, v)
    }

    /// The same, in the pixel space overlays are stated in.
    pub fn project_pixels(&self, x: f32, y: f32) -> (f32, f32) {
        let (u, v) = self.project(x, y);
        (u * PAGE_WIDTH, v * PAGE_HEIGHT)
    }

    /// Archive path of one of the twelve tiles, `1..=12` in reading order.
    ///
    /// Tile 1 is the top-left corner and tile 12 the bottom-right, which is
    /// what the alpha channels on the right column and bottom row establish.
    pub fn tile_path(&self, tile: usize) -> String {
        format!(
            r"Interface\WorldMap\{dir}\{dir}{tile}.blp",
            dir = self.directory
        )
    }

    /// Column and row of a tile, `1..=12`, zero-based.
    pub fn tile_grid(tile: usize) -> (usize, usize) {
        let index = tile.saturating_sub(1);
        (index % TILE_COLUMNS, index / TILE_COLUMNS)
    }
}

/// A patch of a page revealed by exploring the area it covers.
///
/// **The twelve base tiles are the *unexplored* picture.** A zone page with no
/// overlays on it draws as blank parchment with a coastline: the roads, the
/// buildings and the names all live in these patches, one per named sub-area,
/// blitted on at a stated pixel offset. A client that draws only the base
/// tiles has a map that never fills in, which is exactly how this was found --
/// the map worked, and the abbey the character had walked through was not on
/// it.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    pub id: u32,
    /// The [`Page`] this patch is drawn on.
    pub page_id: u32,
    /// The `AreaTable` areas it reveals -- up to four, zero where absent. Any
    /// one of them being explored shows the whole patch, because one texture
    /// is all the table offers.
    pub areas: [u32; 4],
    /// Base name of the texture, e.g. `NORTHSHIREVALLEY`. Upper case in the
    /// table and mixed case on disk, which does not matter: an MPQ hashes the
    /// upper-cased path, so either resolves.
    pub texture: String,
    pub width: u32,
    pub height: u32,
    /// Pixels from the page's left and top edges, in the same 1002x668 space
    /// [`Page::project_pixels`] returns.
    pub offset_x: u32,
    pub offset_y: u32,
}

impl Overlay {
    /// Whether the patch states a rectangle and a texture to fill it.
    pub fn is_drawable(&self) -> bool {
        self.width > 0 && self.height > 0 && !self.texture.is_empty()
    }

    /// Tiles across the patch, and down it.
    ///
    /// A patch wider or taller than one tile is split the same way a page is,
    /// and **the file count is what confirms the rule**: `STORMWIND` is
    /// 485x405 and ships four files, `FORESTSEDGE` is 256x341 and ships two,
    /// `RIDGEPOINTTOWER` is 306x233 and ships two. Nothing here was
    /// transcribed.
    pub fn tile_grid(&self) -> (usize, usize) {
        let across = (self.width as usize).div_ceil(TILE_TEXELS);
        let down = (self.height as usize).div_ceil(TILE_TEXELS);
        (across.max(1), down.max(1))
    }

    /// How many files the patch is stored in.
    ///
    /// **76 patches have a file past this count, and it is right to ignore
    /// them.** `wow-cli map overlays --verify` resolves every predicted tile
    /// across the whole game -- 1,524 of 1,524 -- and then asks for the one
    /// past the end, which turns up for 76 of the 886 patches. The count
    /// cannot be too low: `ceil(w/256) * ceil(h/256)` covers the stated
    /// rectangle exactly, so a further file has nowhere to go. Exporting the
    /// pair settles what they are -- `MarshlightLake1` is the whole labelled
    /// picture and `MarshlightLake2` is nearly blank, an offcut of some
    /// earlier, taller version of the patch left in the archive when the table
    /// row shrank.
    pub fn tile_count(&self) -> usize {
        let (across, down) = self.tile_grid();
        across * down
    }

    /// Archive path of one tile, `1..=tile_count()`, in reading order.
    pub fn tile_path(&self, page_directory: &str, tile: usize) -> String {
        format!(
            r"Interface\WorldMap\{page_directory}\{texture}{tile}.blp",
            texture = self.texture
        )
    }

    /// Where one tile's top-left corner sits on the page, in page pixels.
    ///
    /// **A tile is stored at a power-of-two size that is usually larger than
    /// the part of the patch it holds** -- `FORESTSEDGE` is 341 tall and its
    /// second tile is stored 128 tall to carry the remaining 85 rows. So the
    /// caller has to crop against [`Overlay::width`] and [`Overlay::height`]
    /// rather than drawing the file at its own size, or the patch bleeds past
    /// the rectangle the table gave it.
    pub fn tile_origin(&self, tile: usize) -> (u32, u32) {
        let (across, _) = self.tile_grid();
        let index = tile.saturating_sub(1);
        let (col, row) = (index % across, index / across);
        (
            self.offset_x + (col * TILE_TEXELS) as u32,
            self.offset_y + (row * TILE_TEXELS) as u32,
        )
    }
}

/// Every page in the table, ready to be searched by position.
#[derive(Debug, Clone, Default)]
pub struct Atlas {
    pages: Vec<Page>,
    overlays: Vec<Overlay>,
}

impl Atlas {
    /// Reads every row, keeping the ones that state a rectangle.
    ///
    /// Overlays arrive separately through [`Atlas::with_overlays`], because a
    /// caller with no `WorldMapOverlay.dbc` should still get a usable atlas
    /// rather than none: a map with no patches on it is an unexplored map,
    /// which is a real state, where a map with no pages is nothing at all.
    pub fn from_table(table: &WorldMapArea) -> Self {
        let pages = table.iter().map(page_from_row).filter(Page::has_bounds).collect();
        Self {
            pages,
            overlays: Vec::new(),
        }
    }

    /// Builds an atlas directly from already-constructed pages, with no
    /// overlays. For a caller (or a test in another crate, where `Atlas`'s
    /// fields are not visible) that has pages from somewhere other than a
    /// real `WorldMapArea.dbc` table.
    pub fn from_pages(pages: Vec<Page>) -> Self {
        Self {
            pages,
            overlays: Vec::new(),
        }
    }

    /// Adds the patch table, keeping the rows that state a texture and a size.
    pub fn with_overlays(mut self, table: &WorldMapOverlay) -> Self {
        self.overlays = table
            .iter()
            .map(overlay_from_row)
            .filter(Overlay::is_drawable)
            .collect();
        self
    }

    /// All pages, in table order.
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// Every patch drawn on one page, in table order.
    pub fn overlays(&self, page_id: u32) -> impl Iterator<Item = &Overlay> {
        self.overlays.iter().filter(move |o| o.page_id == page_id)
    }

    /// The page with this `WorldMapArea.dbc` id.
    pub fn page(&self, id: u32) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// The best page to draw a position on: the smallest zone page whose
    /// rectangle contains it.
    ///
    /// Continent pages (`area_id == 0`) are excluded, because every position
    /// on a continent is inside one and it would win nothing useful -- a
    /// player standing in Elwynn wants the Elwynn page. Smallest-first is what
    /// makes a city beat the zone around it, which is the behaviour the
    /// original client has.
    pub fn zone_page(&self, map_id: u32, x: f32, y: f32) -> Option<&Page> {
        self.pages
            .iter()
            .filter(|p| p.map_id == map_id && p.area_id != 0 && p.contains(x, y))
            .min_by(|a, b| {
                a.world_area()
                    .partial_cmp(&b.world_area())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// The whole-continent page for a map, if it has one.
    pub fn continent_page(&self, map_id: u32) -> Option<&Page> {
        self.pages.iter().find(|p| p.map_id == map_id && p.area_id == 0)
    }

    /// The zone page whose `area_id` equals this `AreaTable` id -- an
    /// equality test, the same shape as [`Self::page`] for a
    /// `WorldMapArea` id rather than a containment test against a
    /// rectangle. **For a caller with no world position to test against**,
    /// such as a party member's coarse `(zone, position)` pair rather than a
    /// live entity: `area_id` is not the fine-grained zone a member may
    /// actually be standing in (see `Page::area_id`'s own doc comment), so a
    /// caller resolving a real `AreaTable` id normally needs to walk its
    /// `parent_area_id` chain until one of the ancestors matches here --
    /// that walk needs `AreaTable` itself, which this crate does not load,
    /// so it lives on the caller. Continent pages (`area_id == 0`) never
    /// match, for the same reason [`Self::zone_page`] excludes them: every
    /// zone belongs to some continent, and matching an unset field would be
    /// a coincidence, not an answer.
    pub fn page_by_area(&self, area_id: u32) -> Option<&Page> {
        if area_id == 0 {
            return None;
        }
        self.pages.iter().find(|p| p.area_id == area_id)
    }
}

fn overlay_from_row(row: WorldMapOverlayRow<'_>) -> Overlay {
    Overlay {
        id: row.id(),
        page_id: row.world_map_area_id(),
        areas: [
            row.area_id_0(),
            row.area_id_1(),
            row.area_id_2(),
            row.area_id_3(),
        ],
        texture: row.texture().to_string(),
        width: row.width(),
        height: row.height(),
        offset_x: row.offset_x(),
        offset_y: row.offset_y(),
    }
}

fn page_from_row(row: WorldMapAreaRow<'_>) -> Page {
    Page {
        id: row.id(),
        map_id: row.map_id(),
        area_id: row.area_id(),
        directory: row.directory().to_string(),
        x_max: row.x_max(),
        x_min: row.x_min(),
        y_max: row.y_max(),
        y_min: row.y_min(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Elwynn` row, transcribed from `wow-cli dbc dump WorldMapArea`.
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

    /// The `Stormwind` row, which is a page in its own right *and* an overlay
    /// on Elwynn -- which is what makes it able to check the projection.
    fn stormwind() -> Page {
        Page {
            id: 301,
            map_id: 0,
            area_id: 1519,
            directory: "Stormwind".into(),
            x_max: -7995.833,
            x_min: -9154.166,
            y_max: 1722.917,
            y_min: -14.583,
        }
    }

    /// Northshire's human-warrior spawn, which this project has walked to and
    /// logged in at more times than any other position in the game.
    ///
    /// Elwynn's `NORTHSHIREVALLEY` overlay states a clickable box of
    /// `x 425..600, y 190..375`, and the spawn is inside it.
    #[test]
    fn the_northshire_spawn_lands_on_northshires_overlay() {
        let (x, y) = elwynn().project_pixels(-8950.0, -132.0);
        assert!(
            (425.0..=600.0).contains(&x) && (190.0..=375.0).contains(&y),
            "spawn projected to ({x:.0}, {y:.0}), outside NORTHSHIREVALLEY's box"
        );
    }

    /// The decisive one: three of the four possible orientations put Stormwind
    /// hundreds of pixels from the overlay that draws it, so this asserts the
    /// chosen reading **and** that the wrong ones are excluded -- the same
    /// shape as testing an exception beside the thing it is indistinguishable
    /// from.
    #[test]
    fn stormwinds_page_projects_onto_stormwinds_overlay_and_no_flip_does() {
        let elwynn = elwynn();
        let sw = stormwind();
        // Elwynn's STORMWIND overlay texture occupies x 0..485, y 0..405.
        let (left, top) = elwynn.project_pixels(sw.x_max, sw.y_max);
        let (right, bottom) = elwynn.project_pixels(sw.x_min, sw.y_min);
        assert!(
            (-60.0..60.0).contains(&left) && (400.0..500.0).contains(&right),
            "horizontal span {left:.0}..{right:.0} does not match the overlay's 0..485"
        );
        assert!(
            (-20.0..60.0).contains(&top) && (300.0..420.0).contains(&bottom),
            "vertical span {top:.0}..{bottom:.0} does not match the overlay's 0..405"
        );

        // Each flip is what a different reading of the bounds would give, and
        // each misses by more than the whole overlay is wide.
        let flipped_left = PAGE_WIDTH - left;
        let flipped_top = PAGE_HEIGHT - top;
        assert!(
            flipped_left > 485.0,
            "a flipped horizontal axis would still land on the overlay"
        );
        assert!(
            flipped_top > 405.0,
            "a flipped vertical axis would still land on the overlay"
        );
    }

    #[test]
    fn a_page_with_no_bounds_is_not_projected_onto() {
        let dalaran = Page {
            id: 504,
            map_id: 571,
            area_id: 4395,
            directory: "Dalaran".into(),
            x_max: 0.0,
            x_min: 0.0,
            y_max: 0.0,
            y_min: 0.0,
        };
        assert!(!dalaran.has_bounds());
        assert!(!dalaran.contains(0.0, 0.0));
    }

    /// A city sits inside a zone, so both pages contain the point and the
    /// smaller one has to win -- otherwise standing in Stormwind opens the
    /// Elwynn map.
    #[test]
    fn the_smallest_containing_page_wins() {
        let atlas = Atlas {
            pages: vec![elwynn(), stormwind()],
            overlays: Vec::new(),
        };
        // The Trade District, comfortably inside both rectangles.
        let page = atlas.zone_page(0, -8800.0, 600.0).expect("a page");
        assert_eq!(page.id, 301, "expected Stormwind, got {}", page.directory);
        // And a point in Elwynn proper is only in the one page.
        let page = atlas.zone_page(0, -9450.0, -1100.0).expect("a page");
        assert_eq!(page.id, 30, "expected Elwynn, got {}", page.directory);
    }

    /// A party member's zone names a page by equality, not by containment --
    /// the same shape a quest POI already uses, and the one that answers
    /// `page_at`/`zone_page` cannot: there is no world position to test a
    /// rectangle against, only the `AreaTable` id a member's own stats
    /// packet carries. `area_id` values are the real ones from `elwynn()`
    /// and `stormwind()` above -- 12 and 1519, matching what `Watcher` and a
    /// character standing in Stormwind actually reported live.
    #[test]
    fn a_zone_finds_its_page_by_equality() {
        let atlas = Atlas {
            pages: vec![elwynn(), stormwind()],
            overlays: Vec::new(),
        };
        assert_eq!(atlas.page_by_area(12).map(|p| p.id), Some(30));
        assert_eq!(atlas.page_by_area(1519).map(|p| p.id), Some(301));
        assert_eq!(
            atlas.page_by_area(9999),
            None,
            "a zone with no page of its own must not resolve to one anyway"
        );
        assert_eq!(
            atlas.page_by_area(0),
            None,
            "a continent's own zero area_id must never match a real zone's lookup"
        );
    }

    fn overlay(texture: &str, width: u32, height: u32, offset: (u32, u32)) -> Overlay {
        Overlay {
            id: 0,
            page_id: 30,
            areas: [9, 0, 0, 0],
            texture: texture.into(),
            width,
            height,
            offset_x: offset.0,
            offset_y: offset.1,
        }
    }

    /// **The file counts on disk are what settle this**, so the three rows
    /// picked are the ones whose counts differ: a patch inside one tile, one
    /// split vertically, one split horizontally, and one split both ways.
    /// `Interface\WorldMap\Elwynn` holds exactly `NorthshireValley1`,
    /// `ForestsEdge1..2`, `RidgepointTower1..2` and `Stormwind1..4`.
    #[test]
    fn a_patch_is_split_into_tiles_the_way_its_files_are() {
        let northshire = overlay("NORTHSHIREVALLEY", 256, 256, (381, 147));
        assert_eq!(northshire.tile_grid(), (1, 1));
        assert_eq!(northshire.tile_count(), 1);

        // 256x341: one column, two rows.
        assert_eq!(overlay("FORESTSEDGE", 256, 341, (124, 327)).tile_count(), 2);
        // 306x233: two columns, one row.
        assert_eq!(
            overlay("RIDGEPOINTTOWER", 306, 233, (696, 435)).tile_count(),
            2
        );
        // 485x405: two by two.
        let stormwind = overlay("STORMWIND", 485, 405, (0, 0));
        assert_eq!(stormwind.tile_grid(), (2, 2));
        assert_eq!(stormwind.tile_count(), 4);

        assert_eq!(
            northshire.tile_path("Elwynn", 1),
            r"Interface\WorldMap\Elwynn\NORTHSHIREVALLEY1.blp"
        );
    }

    /// Reading order, and it matters: read down-then-across instead, a
    /// two-by-two patch puts its top-right corner at the bottom left and the
    /// picture is still a picture.
    #[test]
    fn a_patchs_tiles_run_across_before_down() {
        let stormwind = overlay("STORMWIND", 485, 405, (100, 200));
        assert_eq!(stormwind.tile_origin(1), (100, 200));
        assert_eq!(stormwind.tile_origin(2), (100 + 256, 200));
        assert_eq!(stormwind.tile_origin(3), (100, 200 + 256));
        assert_eq!(stormwind.tile_origin(4), (100 + 256, 200 + 256));
    }

    /// A row with no texture or no size states no patch. 94 of the table's 988
    /// rows are like that and drawing them would be drawing nothing at a
    /// position, which reads as a hole in the art.
    #[test]
    fn a_patch_with_no_texture_or_no_size_is_not_drawable() {
        assert!(!overlay("", 256, 256, (0, 0)).is_drawable());
        assert!(!overlay("SOMETHING", 0, 256, (0, 0)).is_drawable());
        assert!(overlay("SOMETHING", 256, 256, (0, 0)).is_drawable());
    }

    #[test]
    fn tiles_are_four_across_and_three_down_in_reading_order() {
        assert_eq!(Page::tile_grid(1), (0, 0));
        assert_eq!(Page::tile_grid(4), (3, 0));
        assert_eq!(Page::tile_grid(5), (0, 1));
        assert_eq!(Page::tile_grid(12), (3, 2));
        assert_eq!(
            elwynn().tile_path(7),
            r"Interface\WorldMap\Elwynn\Elwynn7.blp"
        );
    }
}
