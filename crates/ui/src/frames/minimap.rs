//! The minimap: the ground you are standing on, small, and following you.
//!
//! **The same split as the world map, and for the same reason.** This crate is
//! handed fractions of its own viewport and never a world coordinate. Turning
//! a position into a place on the minimap belongs beside the terrain grid the
//! art is keyed by ([`adt::minimap`]), because that is where the measurement
//! that settled it lives, and a second copy of it here would agree with the
//! first right up until one of them changed.
//!
//! # The picture is round, and the corners are painted out
//!
//! egui clips to rectangles. A minimap is a disc. So the tiles are drawn as
//! ordinary rectangles clipped to the square, and then a **rim** -- an annulus
//! from the disc's edge out past the corners -- is painted over them. That rim
//! must be opaque: it is the only thing standing between the viewer and four
//! corners of terrain that are outside the map, and a rim that inherited the
//! window's own translucency would show them faintly rather than not at all.
//! [`crate::style::Style::minimap_rim`] therefore carries its own colour
//! rather than reusing the frame background.
//!
//! # What is at the centre is not negotiable
//!
//! The player is drawn at the middle and everything else moves around them.
//! That is the entire difference between this frame and the world map, and it
//! is why the caller recomputes the tile placement every frame rather than
//! caching it -- the art is fixed to the ground and the viewport slides over
//! it.
//!
//! # A marker outside the disc is not drawn
//!
//! Not clamped to the rim, which is what several later clients do: a party
//! member pinned to the edge is a claim that they are *in that direction* at
//! an unknown distance, and this client has spent enough on the difference
//! between "here" and "somewhere over there". Off the map is off the map.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use super::world_map::{MapMarker, MarkerKind};
use crate::style::Style;

/// One piece of terrain art placed on the viewport.
#[derive(Debug, Clone, PartialEq)]
pub struct MinimapTile {
    pub texture: egui::TextureId,
    /// Where it goes, as fractions of the viewport square: `[u0, v0, u1, v1]`.
    ///
    /// **Runs well outside `0..1` and must**: one tile is 533 world units and
    /// the viewport shows a few hundred, so most of every tile hangs off the
    /// edge. The caller states the whole rectangle and the frame clips it,
    /// rather than the caller cropping and the frame trusting -- a cropped
    /// rectangle and cropped texture coordinates are two numbers that can
    /// disagree, which is exactly the bug the world map's patch tiles have.
    pub rect: [f32; 4],
}

/// Everything the frame draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MinimapView {
    /// The area the character is standing in, or a statement about why there
    /// is no name for it.
    pub title: String,
    pub tiles: Vec<MinimapTile>,
    /// The player's arrow, party members and quest objectives, in viewport
    /// fractions with `(0.5, 0.5)` at the middle.
    pub markers: Vec<MapMarker>,
    /// Drawn across the middle when there is no art -- see the world map's
    /// module comment for why "would not load" and "nothing here" must not
    /// look the same.
    pub note: Option<String>,
}

/// A view to draw while the layout is being edited, and before there is a
/// world to draw.
pub fn placeholder() -> MinimapView {
    MinimapView {
        title: "Minimap".into(),
        tiles: Vec::new(),
        markers: vec![MapMarker {
            u: 0.5,
            v: 0.5,
            facing: 0.0,
            kind: MarkerKind::Player,
            label: String::new(),
            outline: Vec::new(),
        }],
        note: Some("no terrain".into()),
    }
}

/// The closest and widest the disc may be zoomed, in world units across it.
///
/// About one map chunk and about two terrain tiles. Past either end the
/// picture is a single texel or a continent, and neither is a minimap. Stated
/// here rather than in [`crate::style`] so the wheel and the style clamp
/// cannot end up with different limits.
pub const MIN_RANGE: f32 = 30.0;
pub const MAX_RANGE: f32 = 1066.0;

fn header_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

/// How much room the window wants: a square viewport with a name over it.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    let pad = style.padding * scale;
    let art = style.minimap_size * scale;
    Vec2::new(
        art + pad * 2.0,
        art + pad * 2.0 + header_height(style, scale),
    )
}

/// The square the disc is inscribed in.
///
/// The single source of truth for where the art goes, so the tiles, the
/// markers and the hit test cannot disagree -- the same rule as
/// [`super::world_map::art_rect`].
pub fn art_rect(rect: Rect, style: &Style, scale: f32) -> Rect {
    let pad = style.padding * scale;
    let edge = (rect.width() - pad * 2.0).max(0.0);
    Rect::from_min_size(
        Pos2::new(
            rect.min.x + pad,
            rect.min.y + pad + header_height(style, scale),
        ),
        Vec2::splat(edge),
    )
}

/// Where a fraction of the viewport lands on screen.
pub fn marker_pos(art: Rect, u: f32, v: f32) -> Pos2 {
    Pos2::new(art.min.x + art.width() * u, art.min.y + art.height() * v)
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &MinimapView, style: &Style, scale: f32) {
    let corner = egui::CornerRadius::same((style.corner * scale).round().clamp(0.0, 255.0) as u8);
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
    let centre = art.center();
    let radius = art.width().min(art.height()) / 2.0;
    let clipped = painter.with_clip_rect(art);

    // The ground under a tile that would not load. Drawn before the art so a
    // hole in the mosaic is a patch of parchment rather than whatever the
    // window happened to be sitting on.
    clipped.circle_filled(centre, radius, style.minimap_backing);

    for tile in &view.tiles {
        let at = Rect::from_min_max(
            marker_pos(art, tile.rect[0], tile.rect[1]),
            marker_pos(art, tile.rect[2], tile.rect[3]),
        );
        // Nothing that cannot touch the disc is worth submitting -- a viewport
        // at close zoom overlaps four tiles and the caller may hand over more.
        if !at.intersects(art) {
            continue;
        }
        clipped.image(
            tile.texture,
            at,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    if let Some(note) = &view.note {
        clipped.text(
            centre,
            Align2::CENTER_CENTER,
            note,
            font.clone(),
            style.quest_dim.into(),
        );
    }

    // Objective regions, drawn **under** the rim rather than with the blips.
    //
    // That is what circle-clips them: a ring is map content, it can be larger
    // than the whole disc, and a ring drawn after the rim would trail across
    // the bezel and out of the frame. The blips go on top because a blip is
    // never bigger than the disc and must not be painted over.
    for marker in &view.markers {
        if marker.outline.len() < 2 {
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

    // The rim, over the art and under the blips: this is what makes the map
    // round. See the module comment for why it is opaque and why it is not the
    // window's own background colour.
    clipped.add(rim(centre, radius, radius * 1.6, style.minimap_rim));
    if style.border_width > 0.0 {
        clipped.circle_stroke(
            centre,
            radius,
            Stroke::new(style.border_width * scale, style.border),
        );
    }

    let pin = style.minimap_pin * scale;
    for marker in &view.markers {
        let at = marker_pos(art, marker.u, marker.v);
        // Off the disc is off the map -- never dragged to the rim. See the
        // module comment.
        if at.distance(centre) > radius - pin * 0.5 {
            continue;
        }
        match marker.kind {
            MarkerKind::Objective => {
                // **A region gets its ring and no pin.** On a page the pin is
                // somewhere to hang the label; here there is no hover text and
                // the region is routinely larger than the whole disc, so a pin
                // at its centroid would be claiming a spot the server never
                // named -- and would sit in the middle of the picture whether
                // or not the objective is anywhere near.
                if marker.outline.len() < 2 {
                    clipped.circle(
                        at,
                        pin * 0.5,
                        style.world_map_objective,
                        Stroke::new(scale.max(1.0), style.world_map_outline),
                    );
                }
            }
            MarkerKind::PartyMember => {
                clipped.circle(
                    at,
                    pin * 0.5,
                    style.world_map_party,
                    Stroke::new(scale.max(1.0), style.world_map_outline),
                );
            }
            MarkerKind::Questgiver { turn_in, live } => {
                // The same diamond the page draws, so the two frames cannot
                // disagree about what a questgiver looks like -- and the same
                // fade, which matters more here: a minimap covers a few
                // hundred units, so a pin on it is usually a creature the
                // player is standing near, and a *remembered* one that looked
                // identical would read as an NPC actually in view.
                let colour: Color32 = style.world_map_questgiver.into();
                let colour = if live {
                    colour
                } else {
                    colour.gamma_multiply(style.world_map_remembered)
                };
                let half = pin * 0.6;
                clipped.add(egui::Shape::convex_polygon(
                    vec![
                        at + Vec2::new(0.0, -half),
                        at + Vec2::new(half, 0.0),
                        at + Vec2::new(0.0, half),
                        at + Vec2::new(-half, 0.0),
                    ],
                    colour,
                    Stroke::new(scale.max(1.0), style.world_map_outline),
                ));
                if turn_in {
                    clipped.circle_filled(at, half * 0.35, style.world_map_outline);
                }
            }
            MarkerKind::Player => {
                // The same arrow the world map draws, and for the same reason:
                // a dot cannot say which way you are facing, and on a map that
                // is always centred on you that is the *only* thing the marker
                // has left to say.
                let (sin, cos) = marker.facing.sin_cos();
                let rotate =
                    |x: f32, y: f32| Pos2::new(at.x + x * cos - y * sin, at.y + x * sin + y * cos);
                clipped.add(egui::Shape::convex_polygon(
                    vec![
                        rotate(0.0, -pin),
                        rotate(pin * 0.7, pin * 0.8),
                        rotate(0.0, pin * 0.35),
                        rotate(-pin * 0.7, pin * 0.8),
                    ],
                    style.world_map_player,
                    Stroke::new(scale.max(1.0), style.world_map_outline),
                ));
            }
        }
    }
}

/// A filled annulus, used to paint out everything outside the disc.
///
/// `outer` is deliberately past the square's corners (which sit at
/// `radius * sqrt(2)`), and the painter's clip rectangle cuts it back -- which
/// is cheaper and more exact than trying to draw a square-with-a-hole and
/// getting the corners right.
fn rim(centre: Pos2, inner: f32, outer: f32, colour: impl Into<Color32>) -> egui::Shape {
    const SEGMENTS: usize = 96;
    let colour = colour.into();
    let mut mesh = egui::Mesh::default();
    mesh.reserve_vertices((SEGMENTS + 1) * 2);
    mesh.reserve_triangles(SEGMENTS * 2);
    for step in 0..=SEGMENTS {
        let angle = step as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        mesh.colored_vertex(
            Pos2::new(centre.x + cos * inner, centre.y + sin * inner),
            colour,
        );
        mesh.colored_vertex(
            Pos2::new(centre.x + cos * outer, centre.y + sin * outer),
            colour,
        );
    }
    for step in 0..SEGMENTS {
        let base = step as u32 * 2;
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base + 1, base + 3, base + 2);
    }
    egui::Shape::mesh(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> Style {
        Style::default()
    }

    /// The viewport is square whatever the window's shape, because the art is
    /// a disc and a disc drawn into a rectangle is an ellipse.
    #[test]
    fn the_viewport_is_square() {
        let style = style();
        let want = size(&style, 1.0);
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), want);
        let art = art_rect(rect, &style, 1.0);
        assert!((art.width() - art.height()).abs() < 0.01, "{art:?}");
        assert!((art.width() - style.minimap_size).abs() < 0.01, "{art:?}");
        // And it sits under the header rather than over it.
        assert!(art.min.y > rect.min.y + style.font_size);
    }

    /// The rim has to reach the corners or the map is a disc with four
    /// triangles of stray terrain around it.
    #[test]
    fn the_rim_reaches_past_the_corners() {
        let radius = 100.0;
        let outer = radius * 1.6;
        assert!(
            outer > radius * std::f32::consts::SQRT_2,
            "a rim stopping at {outer} leaves the corners at {} uncovered",
            radius * std::f32::consts::SQRT_2
        );
    }

    /// Scaling the whole frame scales the viewport with it, or the disc and
    /// the window it sits in come apart.
    #[test]
    fn scale_multiplies_every_dimension() {
        let style = style();
        let one = size(&style, 1.0);
        let two = size(&style, 2.0);
        assert!((two.x - one.x * 2.0).abs() < 0.01, "{one:?} {two:?}");
        assert!((two.y - one.y * 2.0).abs() < 0.01, "{one:?} {two:?}");
    }
}
