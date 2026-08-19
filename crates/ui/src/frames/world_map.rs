//! The world map: the page for where the player is, and what is on it.
//!
//! **This crate is handed fractions, never world coordinates.** Turning a
//! position into a place on a page is [`dbc::worldmap`]'s job, and it belongs
//! there because that is where the measurement that settled it lives. A second
//! copy of the projection here would agree with the first until one of them
//! changed -- the same reason the picking ray is unprojected from the matrix
//! the scene was drawn with rather than rebuilt from the camera's angles.
//!
//! **The twelve tiles overflow the picture, and that is not a rounding
//! error.** A page is four tiles across and three down, so the grid is
//! 1024x768, but the art stops at 1002x668 -- measured from the tiles' own
//! alpha channel, unanimously across all 91 pages that have one. So the grid
//! is drawn *larger* than the frame and clipped to it, which lands the content
//! exactly on the frame. Drawing the grid to fit the frame instead would look
//! entirely reasonable and put every marker about 2% off horizontally and 15%
//! off vertically, which is the kind of wrong that never fails and never gets
//! noticed.
//!
//! **A page that will not load draws as an empty parchment with its markers
//! still on it.** The alternative -- drawing nothing -- makes a missing
//! texture and a zone with no quests look identical, and only one of those is
//! this client's fault.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Tiles across a page and down it, and how far the art reaches into the grid.
/// Mirrors [`dbc::worldmap`]'s constants, which this crate cannot depend on --
/// `ui` deliberately depends on neither `world` nor the game data.
pub const TILE_COLUMNS: usize = 4;
pub const TILE_ROWS: usize = 3;
pub const TILE_COUNT: usize = TILE_COLUMNS * TILE_ROWS;
/// The drawn picture, in the same texels the tiles are stored in.
pub const CONTENT_WIDTH: f32 = 1002.0;
pub const CONTENT_HEIGHT: f32 = 668.0;
/// Edge of one stored tile.
pub const TILE_TEXELS: f32 = 256.0;
/// The whole grid the twelve tiles make, which is larger than the picture.
pub const GRID_WIDTH: f32 = TILE_COLUMNS as f32 * TILE_TEXELS;
pub const GRID_HEIGHT: f32 = TILE_ROWS as f32 * TILE_TEXELS;

/// What a marker on the page stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    /// The viewer's own character. Drawn as an arrow, because a dot cannot say
    /// which way you are facing and that is half of what a map is for.
    Player,
    /// One point of a quest objective, off `SMSG_QUEST_POI_QUERY_RESPONSE`.
    Objective,
    /// Another party member, drawn only on the page their own zone resolves
    /// to -- see `maps::PartyMemberPin`'s doc comment for why that is a zone
    /// match rather than a position inside the page's rectangle. Its
    /// `label` is always drawn beside the dot, unlike an objective's, which
    /// only shows on hover: knowing *which* member a dot is is the entire
    /// point of a party's dots existing at all.
    PartyMember,
    /// A questgiver this character has walked past that had something over its
    /// head. Drawn as a diamond -- not a circle, because it is not an
    /// objective, and not the literal `!` the world uses, because a glyph the
    /// size of a map pin is unreadable.
    ///
    /// **Both flags exist to stop this pin claiming more than it knows.**
    ///
    /// - `turn_in` separates a `?` from a `!`: a quest to hand in and a quest
    ///   to take send the player to the same place for opposite reasons.
    /// - `live` separates a mark refreshed this session by an NPC actually in
    ///   range from one **remembered** out of a cache. Every pin on a zone the
    ///   player is not standing in is the second kind, and three things may
    ///   have changed since it was recorded -- the quest log, the creature,
    ///   and whether it is even still there. A remembered pin is therefore
    ///   drawn faded, and its label says when it was seen.
    Questgiver { turn_in: bool, live: bool },
}

/// Something drawn on top of the page.
#[derive(Debug, Clone, PartialEq)]
pub struct MapMarker {
    /// Across the page, `0.0` at the left edge of the art.
    pub u: f32,
    /// Down the page, `0.0` at the top edge of the art.
    pub v: f32,
    /// Screen-space heading in radians, `0` pointing up. Only the player's
    /// arrow uses it.
    pub facing: f32,
    pub kind: MarkerKind,
    /// Shown on hover; empty for markers with nothing to say.
    pub label: String,
    /// The region this marker outlines, in the same fractions, or empty for a
    /// marker that is genuinely one spot.
    ///
    /// **A third of the realm's markers are regions and drawing them as a
    /// single pin would be a claim the server never made.** Of the 18,768
    /// markers in this realm's `quest_poi_points`, 12,794 carry one point and
    /// 5,974 carry between three and dozens -- a valley to search rather than
    /// a door to walk to. The pin still sits at the middle of the ring so
    /// there is something to hover, but the ring is what the server said.
    pub outline: Vec<(f32, f32)>,
}

/// A patch of explored art drawn over the base tiles.
///
/// **The base tiles are the unexplored picture**: a zone page with no patches
/// on it is a coastline and nothing else, and the roads, buildings and names
/// arrive one explored area at a time. The caller decides which patches those
/// are; this crate only puts them where it is told.
#[derive(Debug, Clone, PartialEq)]
pub struct MapPatch {
    pub texture: egui::TextureId,
    /// Where it goes, as fractions of the drawn page: `[u0, v0, u1, v1]`.
    pub rect: [f32; 4],
    /// Which part of the texture to take, as fractions. Not always the whole
    /// of it: a patch tile is stored at a power-of-two size that can be larger
    /// than the part of the patch it holds, and drawing the padding would let
    /// a patch bleed past the rectangle the table gave it.
    pub uv: [f32; 4],
}

/// Everything the frame draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapView {
    /// The zone's player-facing name, or a statement about why there is none.
    pub title: String,
    /// The twelve tiles in reading order. A `None` is a tile that would not
    /// load, and leaves a hole rather than shifting the others.
    pub tiles: [Option<egui::TextureId>; TILE_COUNT],
    /// Explored patches, drawn over the tiles and under the markers.
    pub patches: Vec<MapPatch>,
    pub markers: Vec<MapMarker>,
    /// Drawn across the middle when the page itself is missing -- see the
    /// module comment for why the markers stay.
    pub note: Option<String>,
}

/// A view to draw while the layout is being edited, and when the client has
/// nothing yet.
pub fn placeholder() -> MapView {
    MapView {
        title: "World Map".into(),
        tiles: Default::default(),
        patches: Vec::new(),
        markers: vec![MapMarker {
            u: 0.5,
            v: 0.5,
            facing: 0.0,
            kind: MarkerKind::Player,
            label: String::new(),
            outline: Vec::new(),
        }],
        note: Some("no page loaded".into()),
    }
}

fn header_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

/// How much room the window wants.
///
/// The height follows from the width and the art's aspect ratio rather than
/// being configurable on its own: a page drawn to a shape that is not
/// 1002:668 either stretches the world or crops it, and both move every
/// marker.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    let pad = style.padding * scale;
    let width = style.world_map_width * scale;
    let art = (width - pad * 2.0).max(1.0);
    Vec2::new(
        width,
        header_height(style, scale) + art * (CONTENT_HEIGHT / CONTENT_WIDTH) + pad * 2.0,
    )
}

/// The part of the window the page is drawn in.
///
/// The single source of truth for where the art goes, so the tiles, the
/// markers and any future hit test cannot disagree about it.
pub fn art_rect(rect: Rect, style: &Style, scale: f32) -> Rect {
    let pad = style.padding * scale;
    let width = (rect.width() - pad * 2.0).max(0.0);
    Rect::from_min_size(
        Pos2::new(rect.min.x + pad, rect.min.y + pad + header_height(style, scale)),
        Vec2::new(width, width * (CONTENT_HEIGHT / CONTENT_WIDTH)),
    )
}

/// Where a fraction of the page lands on screen.
pub fn marker_pos(art: Rect, u: f32, v: f32) -> Pos2 {
    Pos2::new(art.min.x + art.width() * u, art.min.y + art.height() * v)
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &MapView, style: &Style, scale: f32) {
    let corner = corner_radius(style.corner * scale);
    painter.rect_filled(rect, corner, style.spellbook_background);
    if style.border_width > 0.0 {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(style.border_width * scale, style.border),
            StrokeKind::Inside,
        );
    }

    let text: Color32 = style.text.into();
    let font = FontId::proportional(style.font_size * scale);
    painter.text(
        rect.min + Vec2::splat(style.padding * scale),
        Align2::LEFT_TOP,
        &view.title,
        font.clone(),
        text,
    );

    let art = art_rect(rect, style, scale);
    // The parchment the tiles sit on, so a page that will not load is still a
    // map-shaped thing with markers on it rather than a hole in the window.
    painter.rect_filled(art, corner, style.world_map_backing);

    // The grid is bigger than the art: the content fills only the top-left
    // 1002x668 of a 1024x768 layout. Scaling by that ratio and clipping puts
    // the picture exactly on `art`. See the module comment.
    let tile = Vec2::new(
        art.width() * (TILE_TEXELS / CONTENT_WIDTH),
        art.height() * (TILE_TEXELS / CONTENT_HEIGHT),
    );
    let clipped = painter.with_clip_rect(art);
    for (index, id) in view.tiles.iter().enumerate() {
        let Some(id) = id else { continue };
        let (col, row) = (index % TILE_COLUMNS, index / TILE_COLUMNS);
        let at = Rect::from_min_size(
            art.min + Vec2::new(col as f32 * tile.x, row as f32 * tile.y),
            tile,
        );
        clipped.image(
            *id,
            at,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // The explored patches, over the base art and under everything else.
    for patch in &view.patches {
        let at = Rect::from_min_max(
            marker_pos(art, patch.rect[0], patch.rect[1]),
            marker_pos(art, patch.rect[2], patch.rect[3]),
        );
        clipped.image(
            patch.texture,
            at,
            Rect::from_min_max(
                Pos2::new(patch.uv[0], patch.uv[1]),
                Pos2::new(patch.uv[2], patch.uv[3]),
            ),
            Color32::WHITE,
        );
    }

    if let Some(note) = &view.note {
        clipped.text(
            art.center(),
            Align2::CENTER_CENTER,
            note,
            font.clone(),
            style.quest_dim.into(),
        );
    }

    let pin = style.world_map_pin * scale;
    // Every ring first, then every pin, so a pin is never buried under the
    // ring of the marker drawn after it -- two quests in one valley is the
    // ordinary case, not the awkward one. Outlined rather than filled: a quest
    // region can be concave -- a road, a river bank -- and egui's cheap fill
    // wants a convex path, so a fill would either be wrong or would have to
    // triangulate a shape the server promised nothing about.
    for marker in &view.markers {
        if marker.outline.is_empty() {
            continue;
        }
        let ring: Vec<Pos2> = marker
            .outline
            .iter()
            .map(|(u, v)| marker_pos(art, *u, *v))
            .collect();
        clipped.add(egui::Shape::closed_line(
            ring,
            Stroke::new((scale * 1.5).max(1.0), style.world_map_objective),
        ));
    }
    for marker in &view.markers {
        let at = marker_pos(art, marker.u, marker.v);
        match marker.kind {
            MarkerKind::Objective => {
                clipped.circle(
                    at,
                    pin * 0.5,
                    style.world_map_objective,
                    Stroke::new(scale.max(1.0), style.world_map_outline),
                );
            }
            MarkerKind::PartyMember => {
                clipped.circle(
                    at,
                    pin * 0.5,
                    style.world_map_party,
                    Stroke::new(scale.max(1.0), style.world_map_outline),
                );
                // Always on, unlike an objective's label -- see
                // `MarkerKind::PartyMember`'s doc comment for why.
                if !marker.label.is_empty() {
                    clipped.text(
                        at + Vec2::new(pin * 0.8, 0.0),
                        Align2::LEFT_CENTER,
                        &marker.label,
                        font.clone(),
                        style.world_map_party.into(),
                    );
                }
            }
            MarkerKind::Questgiver { turn_in, live } => {
                let colour: Color32 = style.world_map_questgiver.into();
                // Faded rather than a different hue: a memory is the *same*
                // claim held less firmly, and a second colour would read as a
                // different kind of thing.
                let colour = if live {
                    colour
                } else {
                    colour.gamma_multiply(style.world_map_remembered)
                };
                // A diamond for "take this" and a smaller one inside it for
                // "hand this in", which is the one distinction that has to
                // survive being four pixels across.
                let half = pin * 0.6;
                let diamond = vec![
                    at + Vec2::new(0.0, -half),
                    at + Vec2::new(half, 0.0),
                    at + Vec2::new(0.0, half),
                    at + Vec2::new(-half, 0.0),
                ];
                clipped.add(egui::Shape::convex_polygon(
                    diamond,
                    colour,
                    Stroke::new(scale.max(1.0), style.world_map_outline),
                ));
                if turn_in {
                    clipped.circle_filled(at, half * 0.35, style.world_map_outline);
                }
            }
            MarkerKind::Player => {
                // An arrow rather than a dot: the heading is the half of "you
                // are here" a dot cannot say.
                let (sin, cos) = marker.facing.sin_cos();
                let rotate = |x: f32, y: f32| {
                    Pos2::new(at.x + x * cos - y * sin, at.y + x * sin + y * cos)
                };
                let points = vec![
                    rotate(0.0, -pin),
                    rotate(pin * 0.7, pin * 0.8),
                    rotate(0.0, pin * 0.35),
                    rotate(-pin * 0.7, pin * 0.8),
                ];
                clipped.add(egui::Shape::convex_polygon(
                    points,
                    style.world_map_player,
                    Stroke::new(scale.max(1.0), style.world_map_outline),
                ));
            }
        }
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Names the quest a pin belongs to.
///
/// One line and no detail: what a pin can say honestly is which quest put it
/// there. The objective's own text is in the log window, which is where a
/// reader who wants it already looks.
pub fn hover_tooltip(response: &egui::Response, label: &str) {
    if label.is_empty() {
        return;
    }
    egui::Tooltip::for_widget(response)
        .at_pointer()
        .show(|ui| {
            ui.strong(label);
        });
}

/// The marker nearest a point, within a pin's radius. For hover text.
pub fn marker_at(
    rect: Rect,
    view: &MapView,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<usize> {
    let art = art_rect(rect, style, scale);
    let reach = style.world_map_pin * scale;
    view.markers
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.label.is_empty())
        .map(|(i, m)| (i, marker_pos(art, m.u, m.v).distance(point)))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the tile scaling: the twelve tiles have to reach
    /// *past* the frame, because the art does not fill them.
    #[test]
    fn the_tile_grid_overflows_the_art_by_the_padding_in_the_files() {
        let style = Style::default();
        let rect = Rect::from_min_size(Pos2::ZERO, size(&style, 1.0));
        let art = art_rect(rect, &style, 1.0);
        let grid = Vec2::new(
            art.width() * (GRID_WIDTH / CONTENT_WIDTH),
            art.height() * (GRID_HEIGHT / CONTENT_HEIGHT),
        );
        assert!(
            grid.x > art.width() && grid.y > art.height(),
            "the grid {grid:?} must be bigger than the art {:?}",
            art.size()
        );
        // 1024/1002 and 768/668 -- and the vertical overflow is much the
        // larger, which is exactly the error a fit-to-frame layout would make.
        assert!((grid.x / art.width() - 1.0219).abs() < 1e-3);
        assert!((grid.y / art.height() - 1.1497).abs() < 1e-3);
    }

    #[test]
    fn the_art_keeps_the_pages_aspect_ratio() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            let rect = Rect::from_min_size(Pos2::ZERO, size(&style, scale));
            let art = art_rect(rect, &style, scale);
            let ratio = art.width() / art.height();
            assert!(
                (ratio - CONTENT_WIDTH / CONTENT_HEIGHT).abs() < 1e-3,
                "aspect {ratio} at scale {scale}"
            );
        }
    }

    /// **A remembered pin must not look like a live one.**
    ///
    /// This is the whole honesty claim of the questgiver cache: everything
    /// drawn from it is a fact about the past, and three things may have
    /// changed since. The frame's answer is to fade it -- so the test asserts
    /// the two are actually painted in *different* colours, which is the one
    /// thing a person cannot check by reading the code that computes them.
    #[test]
    fn a_remembered_questgiver_is_drawn_fainter_than_a_live_one() {
        fn pin_colour(live: bool) -> Color32 {
            let style = Style::default();
            let rect = Rect::from_min_size(Pos2::ZERO, size(&style, 1.0));
            let view = MapView {
                markers: vec![MapMarker {
                    u: 0.5,
                    v: 0.5,
                    facing: 0.0,
                    kind: MarkerKind::Questgiver {
                        turn_in: false,
                        live,
                    },
                    label: "Deputy Willem".into(),
                    outline: Vec::new(),
                }],
                ..Default::default()
            };
            let ctx = egui::Context::default();
            let mut found = None;
            let output = ctx.run_ui(egui::RawInput::default(), |ctx| {
                let layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("t"));
                let painter = egui::Painter::new(ctx.clone(), layer, rect);
                draw(&painter, rect, &view, &style, 1.0);
            });
            for clipped in &output.shapes {
                // The diamond: a four-point path, where the player's arrow
                // has four *and* is only drawn when there is a player marker,
                // which this view has none of.
                if let egui::Shape::Path(path) = &clipped.shape {
                    if path.points.len() == 4 {
                        found = Some(path.fill);
                    }
                }
            }
            output.drop_without_applying_deltas();
            found.expect("a questgiver pin must be painted")
        }

        let live = pin_colour(true);
        let remembered = pin_colour(false);
        assert_ne!(
            live, remembered,
            "a memory drawn identically to a fact is the one thing this pin must not do"
        );
        // Fainter, not merely different: a second hue would read as a
        // different kind of thing rather than as the same claim held less
        // firmly.
        assert!(
            remembered.a() < live.a() || remembered.r() < live.r(),
            "remembered {remembered:?} is not fainter than live {live:?}"
        );
    }

    #[test]
    fn a_fraction_lands_on_the_matching_corner() {
        let style = Style::default();
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), size(&style, 1.0));
        let art = art_rect(rect, &style, 1.0);
        assert_eq!(marker_pos(art, 0.0, 0.0), art.min);
        assert_eq!(marker_pos(art, 1.0, 1.0), art.max);
        assert_eq!(marker_pos(art, 0.5, 0.5), art.center());
    }

    /// Hover has to pick the marker under the cursor, not the first one whose
    /// label is set -- two quests in one zone is the ordinary case.
    #[test]
    fn hover_finds_the_nearest_labelled_marker() {
        let style = Style::default();
        let rect = Rect::from_min_size(Pos2::ZERO, size(&style, 1.0));
        let art = art_rect(rect, &style, 1.0);
        let view = MapView {
            markers: vec![
                MapMarker {
                    u: 0.2,
                    v: 0.2,
                    facing: 0.0,
                    kind: MarkerKind::Objective,
                    label: "near".into(),
                    outline: Vec::new(),
                },
                MapMarker {
                    u: 0.8,
                    v: 0.8,
                    facing: 0.0,
                    kind: MarkerKind::Objective,
                    label: "far".into(),
                    outline: Vec::new(),
                },
                // The player carries no label, so it is never the answer.
                MapMarker {
                    u: 0.8,
                    v: 0.8,
                    facing: 0.0,
                    kind: MarkerKind::Player,
                    label: String::new(),
                    outline: Vec::new(),
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            marker_at(rect, &view, &style, 1.0, marker_pos(art, 0.8, 0.8)),
            Some(1)
        );
        assert_eq!(
            marker_at(rect, &view, &style, 1.0, marker_pos(art, 0.2, 0.2)),
            Some(0)
        );
        assert_eq!(
            marker_at(rect, &view, &style, 1.0, marker_pos(art, 0.5, 0.5)),
            None
        );
    }
}
