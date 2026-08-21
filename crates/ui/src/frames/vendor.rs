//! The vendor window: what an NPC will sell, and what it costs.
//!
//! **Structured like the trainer window, deliberately**, and for the same
//! reason: both are a fixed list the server built for this character, priced
//! by the server, with one action per row and most of the row's content
//! being the reason a click would do nothing. A trainer row is inert because
//! the spell is already known or out of reach; a vendor row is inert only
//! when the stock has run out -- everything else here is always buyable, at
//! a price this client did not compute.
//!
//! **No Close button of its own.** A vendor's stock list only ever opens
//! alongside the questgiver window -- `greet` always opens one -- and closing
//! that closes this with it, the same arrangement the trainer window already
//! relies on. See `apps/viewer/src/main.rs`'s questgiver-close handler.
//!
//! **The price is drawn as sent**, not recomputed from `Item.dbc`'s
//! `BuyPrice`: the server applies the buyer's reputation discount before
//! sending, and a client that showed the table's number would be wrong for
//! every player not at neutral standing. See `world::vendor`.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// One thing a vendor is selling.
#[derive(Debug, Clone, PartialEq)]
pub struct VendorRow {
    /// **The server's own vendor slot**, and what a click asks for -- never a
    /// row position. See [`crate::frames::trainer::TrainerRow`] for the
    /// identical reasoning: the server is free to leave holes.
    pub slot: u32,
    /// Row in `Item.dbc`, sent alongside the slot so the server can check the
    /// two still agree.
    pub entry: u32,
    pub name: String,
    /// Copper, exactly as the server quoted it -- already discounted for this
    /// character's standing.
    pub price: u32,
    /// How many the buyer gets for one purchase at [`Self::price`].
    pub buy_count: u32,
    /// How many are left, or `None` for an endless supply.
    pub remaining: Option<u32>,
    pub icon: Option<egui::TextureId>,
}

impl VendorRow {
    /// Whether clicking this row should send anything. The only inert state
    /// a vendor row has, unlike a trainer's three.
    pub fn clickable(&self) -> bool {
        self.remaining != Some(0)
    }
}

/// Everything the vendor window draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VendorView {
    /// The vendor's own name, resolved by the caller at greeting time.
    pub name: String,
    pub rows: Vec<VendorRow>,
}

/// A window's worth of plausible rows, for the layout editor.
///
/// Carries a sold-out row alongside two ordinary ones, for the same reason
/// the trainer's placeholder carries all three of its states: the editor is
/// where somebody sizes this frame, and a placeholder missing a state sizes
/// a window that fits a shorter list than the real one.
pub fn placeholder() -> VendorView {
    VendorView {
        name: "Innkeeper Farley".into(),
        rows: vec![
            VendorRow {
                slot: 1,
                entry: 159,
                name: "Refreshing Spring Water".into(),
                price: 23,
                buy_count: 5,
                remaining: None,
                icon: None,
            },
            VendorRow {
                slot: 2,
                entry: 414,
                name: "Dalaran Sharp".into(),
                price: 118,
                buy_count: 5,
                remaining: Some(3),
                icon: None,
            },
            VendorRow {
                slot: 3,
                entry: 422,
                name: "Dwarven Mild".into(),
                price: 475,
                buy_count: 5,
                remaining: Some(0),
                icon: None,
            },
        ],
    }
}

/// How much room a window with this many rows wants.
///
/// One header line rather than the trainer's two: a vendor sends no greeting
/// text, only a stock list, so there is nothing to give a second line to.
pub fn size(rows: usize, style: &Style, scale: f32) -> Vec2 {
    let row = style.spellbook_row;
    let height = header(style) + rows.max(1) as f32 * row + style.padding * 2.0;
    Vec2::new(style.loot_width * 1.6, height) * scale
}

/// Unscaled height of the title line.
fn header(style: &Style) -> f32 {
    style.font_size + style.gap
}

/// Where each row sits. The single source of truth for row geometry, used by
/// the drawing and the hit test both -- see [`crate::frames::trainer`] for
/// why that matters here as much as it does there.
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

/// Which row contains a point, **if that row is still in stock**. A sold-out
/// row is drawn and is not a target, for the reason a known trainer spell is
/// not: a click the server would decline in silence is the one failure this
/// client cannot diagnose.
pub fn row_at(
    rect: Rect,
    rows: &[VendorRow],
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<usize> {
    row_rects(rect, rows.len(), style, scale)
        .position(|row| row.contains(point))
        .filter(|&index| rows[index].clickable())
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &VendorView, style: &Style, scale: f32) {
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
        &view.name,
        font.clone(),
        text,
    );

    let painter = painter.with_clip_rect(rect);
    for (index, bounds) in row_rects(rect, view.rows.len(), style, scale).enumerate() {
        let Some(row) = view.rows.get(index) else { break };

        let dimmed = !row.clickable();
        let label = if dimmed {
            crate::frames::trainer::dim(text, 0.45)
        } else {
            text
        };

        let side = bounds.height() - style.border_width * 2.0 * scale;
        if let Some(icon) = row.icon {
            let square = Rect::from_min_size(
                Pos2::new(bounds.min.x, bounds.min.y + style.border_width * scale),
                Vec2::splat(side),
            );
            let tint = if dimmed { Color32::from_gray(110) } else { Color32::WHITE };
            painter.image(
                icon,
                square,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                tint,
            );
        }

        let left = bounds.min.x + side + style.gap * scale;
        painter.text(
            Pos2::new(left, bounds.center().y),
            Align2::LEFT_CENTER,
            &row.name,
            font.clone(),
            label,
        );

        // Right-aligned: the price, and the stock count beside it when it is
        // finite. Sold out replaces the price outright -- a number nobody
        // can spend is not information, and drawing it beside "sold out"
        // would read as a price still on offer.
        let right = Pos2::new(bounds.max.x, bounds.center().y);
        let price = crate::frames::trainer::money(row.price);
        let note = match row.remaining {
            Some(0) => "sold out".to_string(),
            Some(n) => format!("{price}  x{n}"),
            None => price,
        };
        painter.text(right, Align2::RIGHT_CENTER, note, small.clone(), label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<VendorRow> {
        vec![
            VendorRow {
                slot: 1,
                entry: 159,
                name: "Refreshing Spring Water".into(),
                price: 23,
                buy_count: 5,
                remaining: None,
                icon: None,
            },
            VendorRow {
                slot: 2,
                entry: 414,
                name: "Dalaran Sharp".into(),
                price: 118,
                buy_count: 5,
                remaining: Some(3),
                icon: None,
            },
            VendorRow {
                slot: 3,
                entry: 422,
                name: "Dwarven Mild".into(),
                price: 475,
                buy_count: 5,
                remaining: Some(0),
                icon: None,
            },
        ]
    }

    fn bounds(rows: usize) -> (Rect, Style, f32) {
        let style = Style::default();
        let scale = 1.0;
        let rect = Rect::from_min_size(Pos2::new(100.0, 80.0), size(rows, &style, scale));
        (rect, style, scale)
    }

    /// Rows do not overlap and each sits inside the window -- stated once and
    /// read by the painter and the hit test both.
    #[test]
    fn rows_tile_the_window_without_overlapping() {
        let rows = rows();
        let (rect, style, scale) = bounds(rows.len());
        let placed: Vec<Rect> = row_rects(rect, rows.len(), &style, scale).collect();
        assert_eq!(placed.len(), rows.len());
        for pair in placed.windows(2) {
            assert!(pair[0].max.y <= pair[1].min.y + 0.001);
        }
        for row in &placed {
            assert!(rect.contains_rect(*row), "{row:?} escapes {rect:?}");
        }
    }

    /// **The test this window exists to pass.** A click on an in-stock row
    /// answers with its index; a click on the sold-out row answers nothing,
    /// because the server declines that purchase in total silence.
    #[test]
    fn only_in_stock_rows_answer_a_click() {
        let rows = rows();
        let (rect, style, scale) = bounds(rows.len());
        let centres: Vec<Pos2> = row_rects(rect, rows.len(), &style, scale)
            .map(|r| r.center())
            .collect();

        assert_eq!(
            row_at(rect, &rows, &style, scale, centres[0]),
            Some(0),
            "unlimited stock"
        );
        assert_eq!(
            row_at(rect, &rows, &style, scale, centres[1]),
            Some(1),
            "finite stock"
        );
        assert_eq!(
            row_at(rect, &rows, &style, scale, centres[2]),
            None,
            "sold out"
        );
    }

    /// The index names the row the caller reads a slot and an entry off of,
    /// never a position it recounts itself.
    #[test]
    fn the_index_names_the_right_slot() {
        let rows = rows();
        let (rect, style, scale) = bounds(rows.len());
        let centre = row_rects(rect, rows.len(), &style, scale)
            .nth(1)
            .unwrap()
            .center();
        let index = row_at(rect, &rows, &style, scale, centre).unwrap();
        assert_eq!(rows[index].slot, 2);
        assert_eq!(rows[index].entry, 414);
    }

    /// A point outside the window is nobody's row.
    #[test]
    fn a_click_outside_hits_nothing() {
        let rows = rows();
        let (rect, style, scale) = bounds(rows.len());
        assert_eq!(
            row_at(rect, &rows, &style, scale, rect.min - Vec2::splat(5.0)),
            None
        );
    }

    /// An empty stock list still gets a window: a vendor with nothing left
    /// still answers `CMSG_LIST_INVENTORY`, and a zero-height frame would
    /// read as a request that failed.
    #[test]
    fn an_empty_stock_list_still_has_a_window() {
        let (rect, style, scale) = bounds(0);
        assert!(rect.height() > 0.0);
        assert_eq!(row_at(rect, &[], &style, scale, rect.center()), None);
    }
}
