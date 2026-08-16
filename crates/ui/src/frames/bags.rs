//! The bag window: everything the character is carrying, in one grid.
//!
//! **Deliberately one window, not one per bag.** The original client gives each
//! bag its own frame, so a character with four bags has five draggable windows
//! that have to be arranged and rearranged whenever one is swapped. This client
//! draws a single grid over the whole of what is carried. It is the one place
//! so far where this interface departs from the original on purpose rather than
//! by omission, and it is a decision rather than a simplification: the layout is
//! customisable anyway (see `crate::layout`), so the thing a player would gain
//! from separate frames -- putting them where they like -- is already provided
//! by moving the one window.
//!
//! The conventions here are the ones the rest of this crate already follows,
//! for the reasons written up in [`super::action_bar`] and [`super::spellbook`]:
//!
//! **The grid geometry lives in one function.** [`slot_rects`] serves the
//! drawing and the hit test both. Two copies agree until one changes, and the
//! failure -- a click that picks up the item beside the one under the cursor --
//! reads as a targeting bug rather than a layout one.
//!
//! **How many rows there are is computed from the slot count**, not stored
//! beside it, so a window sized for sixteen slots cannot disagree with a grid
//! drawn for twenty.
//!
//! **An icon is optional.** This crate takes an uploaded texture id and knows
//! nothing about `Item.dbc` or BLP. With no game installation a slot draws the
//! item's name, and the client still works.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// How many slots to a row.
///
/// Four, matching the backpack's shape in the original client, so the window
/// stays a recognisable size rather than a long strip.
pub const COLUMNS: usize = 4;

/// One slot in the bag window.
///
/// A slot is always drawn, empty or not -- the grid is a property of the
/// character's capacity, not of what happens to be in it. An empty square is
/// `item: None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BagSlot {
    pub item: Option<BagItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BagItem {
    /// The item's entry, so a caller can identify what was clicked.
    pub entry: u32,
    /// What to call it. Item names are not in `Item.dbc` -- they come from the
    /// server -- so until an item query exists this is a placeholder built
    /// from the entry, and it is the icon that carries the meaning.
    pub name: String,
    /// Drawn in the corner when it is more than one. A stack of one shows
    /// nothing rather than a "1", which is what the original does and what
    /// keeps a bag of single items from being covered in digits.
    pub count: u32,
    pub icon: Option<egui::TextureId>,
}

/// How many rows a given number of slots needs.
///
/// The one place the row count is derived. A partial last row still counts, or
/// the slots on it are drawn outside the window and read as items the
/// character does not have.
pub fn rows_for(slots: usize) -> usize {
    slots.div_ceil(COLUMNS)
}

/// The strip along the top holding the title, and the one along the bottom
/// holding the money.
fn header_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

fn footer_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

/// How much room a window of this many slots wants.
///
/// Takes the slot count rather than reading a fixed size from the style,
/// because the number of squares is a fact about the character -- sixteen
/// without bags, more with them -- and a fixed height would either clip the
/// grid or leave a band of empty window under it.
pub fn size(slots: usize, style: &Style, scale: f32) -> Vec2 {
    let slot = style.slot_size;
    let gap = style.slot_gap;
    let width = COLUMNS as f32 * slot + (COLUMNS as f32 - 1.0) * gap + style.padding * 2.0;
    let rows = rows_for(slots) as f32;
    let grid = rows * slot + (rows - 1.0).max(0.0) * gap;
    let height = grid + style.padding * 2.0 + header_height(style, scale) / scale
        + footer_height(style, scale) / scale;
    Vec2::new(width * scale, height * scale)
}

/// Where each slot sits inside the window.
///
/// The single source of truth for grid geometry -- see the module comment.
pub fn slot_rects(
    rect: Rect,
    slots: usize,
    style: &Style,
    scale: f32,
) -> impl Iterator<Item = Rect> + '_ {
    let slot = style.slot_size * scale;
    let gap = style.slot_gap * scale;
    let origin = rect.min
        + Vec2::new(
            style.padding * scale,
            style.padding * scale + header_height(style, scale),
        );
    (0..slots).map(move |index| {
        let (row, column) = (index / COLUMNS, index % COLUMNS);
        Rect::from_min_size(
            origin + Vec2::new(column as f32 * (slot + gap), row as f32 * (slot + gap)),
            Vec2::splat(slot),
        )
    })
}

/// Which slot contains a point, if any.
pub fn slot_at(
    rect: Rect,
    slots: usize,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<usize> {
    slot_rects(rect, slots, style, scale).position(|slot| slot.contains(point))
}

/// Paints the window.
///
/// `copper` is the character's money, split for display by the caller's
/// counterpart in `world::inventory::purse` -- taken here as one number so
/// this crate has no opinion about the denominations.
pub fn draw(
    painter: &Painter,
    rect: Rect,
    slots: &[BagSlot],
    copper: u32,
    style: &Style,
    scale: f32,
) {
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
        "Bags",
        font.clone(),
        text,
    );

    // How full it is, in the corner. A player deciding whether to go back to
    // town wants the number, and it costs a line that is already drawn.
    let used = slots.iter().filter(|slot| slot.item.is_some()).count();
    painter.text(
        Pos2::new(rect.max.x - pad, rect.min.y + pad),
        Align2::RIGHT_TOP,
        format!("{used}/{}", slots.len()),
        font.clone(),
        style.slot_binding.into(),
    );

    let slot_corner = corner_radius(style.corner * scale * 0.5);
    let painter = painter.with_clip_rect(rect);

    for (index, bounds) in slot_rects(rect, slots.len(), style, scale).enumerate() {
        painter.rect_filled(bounds, slot_corner, style.slot_background);

        let Some(item) = slots.get(index).and_then(|slot| slot.item.as_ref()) else {
            // An empty slot is an outline, so the grid reads as capacity
            // rather than as a window that failed to draw.
            painter.rect_stroke(
                bounds,
                slot_corner,
                Stroke::new(style.border_width * scale, style.slot_empty_border),
                StrokeKind::Inside,
            );
            continue;
        };

        match item.icon {
            Some(icon) => {
                painter.image(
                    icon,
                    bounds.shrink(style.border_width * scale),
                    Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                let clipped = painter.with_clip_rect(bounds);
                clipped.text(
                    bounds.center(),
                    Align2::CENTER_CENTER,
                    super::action_bar::abbreviate(&item.name),
                    FontId::proportional(style.font_size * scale * 0.85),
                    text,
                );
            }
        }

        painter.rect_stroke(
            bounds,
            slot_corner,
            Stroke::new(style.border_width * scale, style.border),
            StrokeKind::Inside,
        );

        // The stack count, bottom right, and only when it means something.
        // See `BagItem::count`.
        if item.count > 1 {
            painter.text(
                bounds.max - Vec2::splat(style.border_width * scale * 2.0),
                Align2::RIGHT_BOTTOM,
                item.count.to_string(),
                FontId::proportional(style.font_size * scale * 0.8),
                text,
            );
        }
    }

    // Money along the bottom. Drawn from the same rectangle the grid was laid
    // out in rather than from the last slot's position, so an empty bag still
    // puts it in the same place.
    let (gold, silver, copper_only) = (copper / 10_000, (copper / 100) % 100, copper % 100);
    painter.text(
        Pos2::new(rect.max.x - pad, rect.max.y - pad),
        Align2::RIGHT_BOTTOM,
        format!("{gold}g {silver}s {copper_only}c"),
        font,
        text,
    );
}

/// Draws the held item against the cursor.
///
/// The same gesture as [`super::spellbook::draw_held`], and deliberately not
/// a second design: picking a thing up and putting it down is one motion
/// whether the thing is a spell or an item, and an indicator anywhere but the
/// cursor would make the second half feel like it might not have registered.
pub fn draw_held(painter: &Painter, at: Pos2, item: &BagItem, style: &Style, scale: f32) {
    let side = style.slot_size * scale * 0.75;
    let bounds = Rect::from_min_size(at + Vec2::splat(4.0 * scale), Vec2::splat(side));
    let corner = corner_radius(style.corner * scale * 0.5);
    painter.rect_filled(bounds, corner, style.slot_background);
    match item.icon {
        Some(icon) => {
            painter.image(
                icon,
                bounds,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            let painter = painter.with_clip_rect(bounds);
            painter.text(
                bounds.center(),
                Align2::CENTER_CENTER,
                super::action_bar::abbreviate(&item.name),
                FontId::proportional(style.font_size * scale * 0.85),
                style.text.into(),
            );
        }
    }
    painter.rect_stroke(
        bounds,
        corner,
        Stroke::new(style.border_width * scale, style.spellbook_selected),
        StrokeKind::Outside,
    );
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Placeholder contents, so the window can be positioned before a character
/// has logged in and picked anything up.
///
/// Sixteen slots -- a backpack -- because that is what every character has
/// before owning a bag, so the placeholder is the real minimum rather than an
/// invented size.
pub fn placeholder() -> Vec<BagSlot> {
    let mut slots = vec![BagSlot::default(); 16];
    for (index, (name, count)) in [("Linen Cloth", 3u32), ("Wool Cloth", 5), ("Bread", 1)]
        .into_iter()
        .enumerate()
    {
        slots[index] = BagSlot {
            item: Some(BagItem {
                entry: index as u32 + 1,
                name: name.to_string(),
                count,
                icon: None,
            }),
        };
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(slots: usize, style: &Style, scale: f32) -> Rect {
        Rect::from_min_size(Pos2::new(100.0, 100.0), size(slots, style, scale))
    }

    /// The hit test and the drawing must use the same geometry, or a click
    /// picks up the item beside the one you pressed.
    #[test]
    fn every_slot_is_clickable_at_its_own_centre() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            for count in [1usize, 4, 16, 20] {
                let rect = window(count, &style, scale);
                for (index, slot) in slot_rects(rect, count, &style, scale).enumerate() {
                    assert_eq!(
                        slot_at(rect, count, &style, scale, slot.center()),
                        Some(index),
                        "slot {index} of {count} at scale {scale}"
                    );
                }
            }
        }
    }

    /// Slots must not overlap, and none may escape the window -- a slot drawn
    /// past the edge is clipped away and reads as an item the character does
    /// not have.
    #[test]
    fn slots_do_not_overlap_and_fit_the_window() {
        let style = Style::default();
        for count in [1usize, 3, 16, 17, 20] {
            let rect = window(count, &style, 1.0);
            let slots: Vec<Rect> = slot_rects(rect, count, &style, 1.0).collect();
            assert_eq!(slots.len(), count);
            for (i, a) in slots.iter().enumerate() {
                assert!(rect.contains_rect(*a), "slot {i} of {count} escapes {rect:?}");
                for b in &slots[i + 1..] {
                    assert!(
                        !a.intersects(*b),
                        "slots overlap in a window of {count}: {a:?} and {b:?}"
                    );
                }
            }
        }
    }

    /// A partial last row still gets a row. Sixteen slots is four rows and
    /// seventeen is five -- the boundary where a `/` instead of a ceiling
    /// division would draw a slot outside the window.
    #[test]
    fn a_partial_row_is_still_a_row() {
        assert_eq!(rows_for(0), 0);
        assert_eq!(rows_for(1), 1);
        assert_eq!(rows_for(4), 1);
        assert_eq!(rows_for(5), 2);
        assert_eq!(rows_for(16), 4);
        assert_eq!(rows_for(17), 5);
    }

    /// The window grows with the number of slots, and only downwards -- the
    /// grid is a fixed four columns wide.
    #[test]
    fn the_window_grows_by_rows_not_columns() {
        let style = Style::default();
        let four = size(4, &style, 1.0);
        let eight = size(8, &style, 1.0);
        assert_eq!(four.x, eight.x, "the window must stay four columns wide");
        assert!(eight.y > four.y, "a second row must make it taller");
    }

    #[test]
    fn scale_multiplies_the_whole_window() {
        let style = Style::default();
        let single = size(16, &style, 1.0);
        let double = size(16, &style, 2.0);
        assert!((double.x - single.x * 2.0).abs() < 0.001);
        assert!((double.y - single.y * 2.0).abs() < 0.001);
    }

    /// The header belongs to nothing: a click on the title must not pick up
    /// the first item.
    #[test]
    fn the_header_is_not_a_slot() {
        let style = Style::default();
        let rect = window(16, &style, 1.0);
        assert_eq!(slot_at(rect, 16, &style, 1.0, rect.min), None);
        assert_eq!(
            slot_at(rect, 16, &style, 1.0, Pos2::new(rect.center().x, rect.min.y + 1.0)),
            None
        );
    }

    /// And neither does the money line at the bottom.
    #[test]
    fn the_footer_is_not_a_slot() {
        let style = Style::default();
        let rect = window(16, &style, 1.0);
        assert_eq!(
            slot_at(
                rect,
                16,
                &style,
                1.0,
                Pos2::new(rect.center().x, rect.max.y - 1.0)
            ),
            None
        );
    }

    /// A window with no slots at all must still be a window rather than
    /// something with a negative height -- an inventory can legitimately be
    /// reported as empty before the login burst finishes.
    #[test]
    fn an_empty_inventory_still_has_a_window() {
        let style = Style::default();
        let empty = size(0, &style, 1.0);
        assert!(empty.x > 0.0 && empty.y > 0.0, "{empty:?}");
        assert_eq!(slot_rects(window(0, &style, 1.0), 0, &style, 1.0).count(), 0);
    }
}
