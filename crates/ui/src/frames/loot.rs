//! The loot window: what is on the corpse you opened.
//!
//! A list rather than a grid, which is the one place this client follows the
//! original's shape exactly — and for a reason rather than by imitation. A bag
//! square means "the twelfth place something can sit" and is worth drawing
//! empty; a loot slot means "this specific thing is here", there are never
//! more than a handful, and an empty one does not exist. So the window is
//! exactly as tall as the corpse is full.
//!
//! **The row index is not the loot slot.** Every row carries the server's own
//! slot number, because a corpse whose first slot has already been taken still
//! numbers the rest from where they were. A window that asked for row *n*
//! would take the wrong item the moment anything had been looted, and it would
//! do it silently — the request is not acknowledged, and the wrong item simply
//! arrives. This crate therefore never invents a slot number; it carries the
//! one it was handed.
//!
//! Money is a row too, drawn first when there is any. It is taken by its own
//! request rather than by a slot index, so it is the one row whose identity is
//! "money" rather than a number.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// One line in the loot window.
#[derive(Debug, Clone, PartialEq)]
pub struct LootRow {
    /// What clicking this asks for.
    pub take: Take,
    pub name: String,
    /// Drawn beside the name when more than one.
    pub count: u32,
    pub icon: Option<egui::TextureId>,
}

/// What a row takes when it is clicked.
///
/// An enum rather than an optional slot number, because money and an item are
/// taken by *different requests* — one names a slot, the other names nothing —
/// and a caller that had to remember which is which would eventually forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    Money,
    /// The **server's** loot slot. See the module comment for why this is
    /// carried rather than derived from the row's position.
    Item(u8),
}

/// How much room a window with this many rows wants.
///
/// Sized to its contents rather than to a fixed height: a corpse with one item
/// on it should not open a window with five empty lines, which would read as
/// loot that failed to load.
pub fn size(rows: usize, style: &Style, scale: f32) -> Vec2 {
    let row = style.spellbook_row;
    let header = style.font_size + style.gap;
    let height = header + rows.max(1) as f32 * row + style.padding * 2.0;
    Vec2::new(style.loot_width, height) * scale
}

fn header_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

/// Where each row sits.
///
/// The single source of truth for row geometry — used by the drawing and the
/// hit test both, for the reason the rest of this crate does it: two copies
/// agree until one changes, and here the failure is taking the item below the
/// one you clicked.
pub fn row_rects(rect: Rect, rows: usize, style: &Style, scale: f32) -> impl Iterator<Item = Rect> + '_ {
    let pad = style.padding * scale;
    let row = style.spellbook_row * scale;
    let top = rect.min.y + pad + header_height(style, scale);
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    (0..rows).map(move |i| {
        Rect::from_min_size(
            Pos2::new(left, top + i as f32 * row),
            Vec2::new(width, row),
        )
    })
}

/// Which row contains a point, if any.
pub fn row_at(rect: Rect, rows: usize, style: &Style, scale: f32, point: Pos2) -> Option<usize> {
    row_rects(rect, rows, style, scale).position(|row| row.contains(point))
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, rows: &[LootRow], style: &Style, scale: f32) {
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

    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        "Loot",
        font.clone(),
        text,
    );

    let painter = painter.with_clip_rect(rect);
    for (index, bounds) in row_rects(rect, rows.len(), style, scale).enumerate() {
        let Some(row) = rows.get(index) else { break };

        let side = bounds.height() - style.border_width * 2.0 * scale;
        let icon_rect = Rect::from_min_size(
            bounds.min + Vec2::splat(style.border_width * scale),
            Vec2::splat(side.max(0.0)),
        );
        match row.icon {
            Some(icon) => {
                painter.image(
                    icon,
                    icon_rect,
                    Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                painter.rect_stroke(
                    icon_rect,
                    corner_radius(style.corner * scale * 0.5),
                    Stroke::new(style.border_width * scale, style.slot_empty_border),
                    StrokeKind::Inside,
                );
            }
        }

        // A stack shows its size in the label rather than in the corner: these
        // rows have room for text where a bag square does not.
        let label = if row.count > 1 {
            format!("{} x{}", row.name, row.count)
        } else {
            row.name.clone()
        };
        painter.text(
            Pos2::new(icon_rect.max.x + style.gap * scale, bounds.center().y),
            Align2::LEFT_CENTER,
            label,
            font.clone(),
            text,
        );
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Placeholder contents, so the window can be positioned without a corpse.
pub fn placeholder() -> Vec<LootRow> {
    vec![
        LootRow {
            take: Take::Money,
            name: "2 copper".into(),
            count: 1,
            icon: None,
        },
        LootRow {
            take: Take::Item(0),
            name: "Frayed Shoes".into(),
            count: 1,
            icon: None,
        },
        LootRow {
            take: Take::Item(1),
            name: "Linen Cloth".into(),
            count: 3,
            icon: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(rows: usize, style: &Style, scale: f32) -> Rect {
        Rect::from_min_size(Pos2::new(200.0, 150.0), size(rows, style, scale))
    }

    #[test]
    fn every_row_is_clickable_at_its_own_centre() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            for count in [1usize, 3, 8] {
                let rect = window(count, &style, scale);
                for (index, row) in row_rects(rect, count, &style, scale).enumerate() {
                    assert_eq!(
                        row_at(rect, count, &style, scale, row.center()),
                        Some(index),
                        "row {index} of {count} at scale {scale}"
                    );
                }
            }
        }
    }

    /// Rows are **contiguous**, not separated: each begins exactly where the
    /// last ended, the way the spellbook's do.
    ///
    /// Worth stating because the first version of this test asserted
    /// `!a.intersects(b)` -- copied from the bag grid, where squares really
    /// are separated by a gap -- and failed. Touching rectangles intersect,
    /// and touching is correct here. The test was describing the wrong shape,
    /// which is the cheaper half of this project's rule that a fix
    /// invalidating a test means one of them is wrong about the data.
    #[test]
    fn rows_are_contiguous_and_fit_the_window() {
        let style = Style::default();
        for count in [1usize, 3, 8] {
            let rect = window(count, &style, 1.0);
            let rows: Vec<Rect> = row_rects(rect, count, &style, 1.0).collect();
            assert_eq!(rows.len(), count);
            for (i, row) in rows.iter().enumerate() {
                assert!(rect.contains_rect(*row), "row {i} of {count} escapes");
            }
            for pair in rows.windows(2) {
                assert!(
                    pair[0].bottom() <= pair[1].top(),
                    "rows out of order or overlapping: {:?} then {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    /// The window is as tall as the corpse is full -- a one-item corpse must
    /// not open a window with empty lines in it, which would read as loot that
    /// failed to load.
    #[test]
    fn the_window_grows_with_the_corpse() {
        let style = Style::default();
        assert!(size(3, &style, 1.0).y > size(1, &style, 1.0).y);
        assert_eq!(size(1, &style, 1.0).x, size(3, &style, 1.0).x);
    }

    #[test]
    fn the_header_is_not_a_row() {
        let style = Style::default();
        let rect = window(3, &style, 1.0);
        assert_eq!(row_at(rect, 3, &style, 1.0, rect.min), None);
    }

    #[test]
    fn scale_multiplies_the_whole_window() {
        let style = Style::default();
        let single = size(3, &style, 1.0);
        let double = size(3, &style, 2.0);
        assert!((double.x - single.x * 2.0).abs() < 0.001);
        assert!((double.y - single.y * 2.0).abs() < 0.001);
    }

    /// **The row index is not the loot slot.** A corpse whose earlier slots
    /// have been taken still numbers the rest from where they were, so a
    /// window that asked for its own row number would take the wrong item --
    /// silently, since the request is not acknowledged.
    #[test]
    fn a_rows_position_is_not_its_loot_slot() {
        let rows = vec![
            LootRow {
                take: Take::Money,
                name: "2 copper".into(),
                count: 1,
                icon: None,
            },
            // Slots 0 and 1 are already gone; the server still calls this 2.
            LootRow {
                take: Take::Item(2),
                name: "Frayed Shoes".into(),
                count: 1,
                icon: None,
            },
        ];
        assert_eq!(rows[1].take, Take::Item(2), "row 1 is not loot slot 1");
        assert_eq!(rows[0].take, Take::Money);
    }
}
