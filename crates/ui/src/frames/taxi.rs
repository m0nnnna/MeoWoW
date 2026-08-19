//! The flight master's window: where you are, and everywhere you can go.
//!
//! The simplest list in this interface, and deliberately so. Unlike the
//! trainer's, **every row here is clickable** -- the caller has already
//! filtered the destinations against the server's known-node mask, so a row
//! that is drawn is a flight that can be bought. There is no dimmed state to
//! draw and none to hit-test around.
//!
//! That places the filtering squarely with the caller, which is the right
//! place for it: this crate depends on neither `world` nor `dbc` and has no
//! way to know what a character has visited.
//!
//! The header names the node you are standing at, because it is the one piece
//! of information the *server* supplied and the client could not have worked
//! out -- see `world::taxi::TaxiMenu::current_node`.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// One place a flight master will send you.
#[derive(Debug, Clone, PartialEq)]
pub struct TaxiRow {
    /// **The `TaxiNodes` row id**, and what a click asks for -- never the
    /// row's position. The list is filtered per character by the known-node
    /// mask, so position *n* names a different place to two people standing
    /// at the same flight master. The same rule as a loot slot, a gossip
    /// option index and a trainer's spell id.
    pub node: u32,
    pub name: String,
    /// Copper, from `TaxiPath.cost`.
    pub cost: u32,
}

/// Everything the flight window draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaxiView {
    /// What the node you are standing at is called. Empty while the menu is
    /// still being asked for.
    pub here: String,
    pub rows: Vec<TaxiRow>,
}

/// A window's worth of plausible destinations, for the layout editor.
pub fn placeholder() -> TaxiView {
    TaxiView {
        here: "Stormwind, Elwynn".into(),
        rows: vec![
            TaxiRow { node: 4, name: "Sentinel Hill, Westfall".into(), cost: 190 },
            TaxiRow { node: 5, name: "Lakeshire, Redridge".into(), cost: 450 },
            TaxiRow { node: 6, name: "Ironforge, Dun Morogh".into(), cost: 720 },
            TaxiRow { node: 12, name: "Darkshire, Duskwood".into(), cost: 610 },
        ],
    }
}

/// How much room a window with this many rows wants.
pub fn size(rows: usize, style: &Style, scale: f32) -> Vec2 {
    let height =
        header(style) + rows.max(1) as f32 * style.spellbook_row + style.padding * 2.0;
    Vec2::new(style.loot_width * 1.7, height) * scale
}

/// Unscaled height of the title and the "you are here" line together.
fn header(style: &Style) -> f32 {
    (style.font_size + style.gap) * 2.0
}

/// Where each row sits. The single source of truth for the drawing and the
/// hit test both, so the two cannot disagree about which flight a click buys.
pub fn row_rects(
    rect: Rect,
    rows: usize,
    style: &Style,
    scale: f32,
) -> impl Iterator<Item = Rect> + '_ {
    let pad = style.padding * scale;
    let row = style.spellbook_row * scale;
    let top = rect.min.y + pad + header(style) * scale;
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    (0..rows).map(move |i| {
        Rect::from_min_size(Pos2::new(left, top + i as f32 * row), Vec2::new(width, row))
    })
}

/// Which row contains a point, if any.
pub fn row_at(rect: Rect, rows: usize, style: &Style, scale: f32, point: Pos2) -> Option<usize> {
    row_rects(rect, rows, style, scale).position(|row| row.contains(point))
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &TaxiView, style: &Style, scale: f32) {
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
    let pad = style.padding * scale;
    let font = FontId::proportional(style.font_size * scale);
    let small = FontId::proportional(style.font_size * 0.85 * scale);

    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        "Flight Master",
        font.clone(),
        text,
    );
    // **The node the server named**, not one this client worked out from the
    // player's position -- the two genuinely disagree, and the server's is
    // the one a flight departs from.
    let here = if view.here.is_empty() {
        "Asking...".to_string()
    } else if view.rows.is_empty() {
        format!("{} -- nowhere else you have been", view.here)
    } else {
        format!("At {}", view.here)
    };
    painter.text(
        rect.min + Vec2::new(pad, pad + (style.font_size + style.gap) * scale),
        Align2::LEFT_TOP,
        here,
        small.clone(),
        Color32::from_rgba_unmultiplied(
            (f32::from(text.r()) * 0.75) as u8,
            (f32::from(text.g()) * 0.75) as u8,
            (f32::from(text.b()) * 0.75) as u8,
            text.a(),
        ),
    );

    let painter = painter.with_clip_rect(rect);
    for (index, bounds) in row_rects(rect, view.rows.len(), style, scale).enumerate() {
        let Some(row) = view.rows.get(index) else { break };
        painter.text(
            Pos2::new(bounds.min.x, bounds.center().y),
            Align2::LEFT_CENTER,
            &row.name,
            font.clone(),
            text,
        );
        painter.text(
            Pos2::new(bounds.max.x, bounds.center().y),
            Align2::RIGHT_CENTER,
            super::trainer::money(row.cost),
            small.clone(),
            text,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(rows: usize) -> (Rect, Style, f32) {
        let style = Style::default();
        let rect = Rect::from_min_size(Pos2::new(60.0, 40.0), size(rows, &style, 1.0));
        (rect, style, 1.0)
    }

    #[test]
    fn rows_tile_without_overlapping() {
        let view = placeholder();
        let (rect, style, scale) = bounds(view.rows.len());
        let placed: Vec<Rect> = row_rects(rect, view.rows.len(), &style, scale).collect();
        for pair in placed.windows(2) {
            assert!(pair[0].max.y <= pair[1].min.y + 0.001);
        }
        for row in &placed {
            assert!(rect.contains_rect(*row));
        }
    }

    /// **The index names a node, not a position.** The third row here is node
    /// 6, and a window reporting its position would ask to fly to node 2 --
    /// which is a real node somewhere else entirely.
    #[test]
    fn a_click_finds_the_row_whose_node_it_wants() {
        let view = placeholder();
        let (rect, style, scale) = bounds(view.rows.len());
        let centre = row_rects(rect, view.rows.len(), &style, scale)
            .nth(2)
            .unwrap()
            .center();
        let index = row_at(rect, view.rows.len(), &style, scale, centre).unwrap();
        assert_eq!(index, 2);
        assert_eq!(view.rows[index].node, 6);
        assert_eq!(view.rows[index].name, "Ironforge, Dun Morogh");
    }

    #[test]
    fn a_click_outside_hits_nothing() {
        let view = placeholder();
        let (rect, style, scale) = bounds(view.rows.len());
        assert_eq!(
            row_at(rect, view.rows.len(), &style, scale, rect.min - Vec2::splat(4.0)),
            None
        );
    }

    /// A flight master a character can reach nothing from still gets a
    /// window: the master is real and the emptiness is the answer, where a
    /// missing window would read as a click that never registered.
    #[test]
    fn an_empty_list_still_has_a_window() {
        let (rect, style, scale) = bounds(0);
        assert!(rect.height() > 0.0);
        assert_eq!(row_at(rect, 0, &style, scale, rect.center()), None);
    }
}
