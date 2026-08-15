//! The character panel: the nineteen slots a character wears.
//!
//! Separate from the bag window on purpose. The bags answer "what am I
//! carrying" and this answers "what am I wearing", and unlike the bags -- where
//! combining four frames into one was a deliberate improvement -- there is
//! nothing here to combine. It is one set of slots and it gets one window.
//!
//! **The slots are laid out in two columns down the sides**, which is the shape
//! every game with an equipment screen has converged on, and it is not
//! decoration: a worn slot has a *fixed* identity, so its position is how the
//! player finds it. That is the opposite of the bag grid, where a square means
//! nothing but "the twelfth place something can sit".
//!
//! **A slot with no confirmed name draws no name.** `world::inventory` names
//! eighteen of the nineteen and deliberately leaves the last unnamed, because
//! it could not be measured -- see `InventorySlot::label`. This crate does not
//! know that story and does not need to: it draws whatever label it is handed
//! and leaves the square blank when handed none, so the interface inherits the
//! honesty rather than reimplementing it.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// How many worn slots there are.
pub const SLOTS: usize = 19;

/// How many go down each side.
///
/// Ten on the left and nine on the right, which is `SLOTS` split as evenly as
/// an odd number allows. Derived rather than written down twice, so the two
/// columns cannot disagree about how many they hold.
pub const LEFT_COLUMN: usize = SLOTS.div_ceil(2);

/// One worn slot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EquipSlot {
    /// What this slot is for, when it is known. Empty draws nothing -- see the
    /// module comment.
    pub label: String,
    pub item: Option<super::bags::BagItem>,
}

fn header_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

/// How much room the panel wants.
///
/// Wide enough for two columns of slots with a gap between them for the label
/// text, and tall enough for the longer column.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    let slot = style.slot_size;
    let gap = style.slot_gap;
    // Each column is a square plus room for its name beside it.
    let column = slot + gap + style.character_label_width;
    let width = column * 2.0 + gap + style.padding * 2.0;
    let rows = LEFT_COLUMN as f32;
    let height =
        rows * slot + (rows - 1.0) * gap + style.padding * 2.0 + header_height(style, scale) / scale;
    Vec2::new(width * scale, height * scale)
}

/// Where each slot sits, and which side it is on.
///
/// The single source of truth for the panel's geometry -- the drawing and the
/// hit test both walk this, for the reason the whole crate does it: two copies
/// agree until one changes, and a click that picks up the wrong piece of gear
/// reads as a targeting bug rather than a layout one.
pub fn slot_rects(rect: Rect, style: &Style, scale: f32) -> impl Iterator<Item = Rect> + '_ {
    let slot = style.slot_size * scale;
    let gap = style.slot_gap * scale;
    let column = slot + gap + style.character_label_width * scale;
    let origin = rect.min
        + Vec2::new(
            style.padding * scale,
            style.padding * scale + header_height(style, scale),
        );
    (0..SLOTS).map(move |index| {
        let (row, side) = if index < LEFT_COLUMN {
            (index, 0.0)
        } else {
            (index - LEFT_COLUMN, 1.0)
        };
        Rect::from_min_size(
            origin + Vec2::new(side * (column + gap), row as f32 * (slot + gap)),
            Vec2::splat(slot),
        )
    })
}

/// Which slot contains a point, if any.
pub fn slot_at(rect: Rect, style: &Style, scale: f32, point: Pos2) -> Option<usize> {
    slot_rects(rect, style, scale).position(|slot| slot.contains(point))
}

/// Paints the panel.
pub fn draw(painter: &Painter, rect: Rect, slots: &[EquipSlot], style: &Style, scale: f32) {
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
        "Character",
        font.clone(),
        text,
    );

    let slot_corner = corner_radius(style.corner * scale * 0.5);
    let label_font = FontId::proportional(style.font_size * scale * 0.8);
    let painter = painter.with_clip_rect(rect);

    for (index, bounds) in slot_rects(rect, style, scale).enumerate() {
        painter.rect_filled(bounds, slot_corner, style.slot_background);

        let slot = slots.get(index);
        match slot.and_then(|slot| slot.item.as_ref()) {
            Some(item) => {
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
            }
            None => {
                painter.rect_stroke(
                    bounds,
                    slot_corner,
                    Stroke::new(style.border_width * scale, style.slot_empty_border),
                    StrokeKind::Inside,
                );
            }
        }

        // The name of the slot, beside the square. Left column labels sit to
        // the right of their square and vice versa, so both read outward from
        // the middle and neither runs into the other.
        if let Some(label) = slot.map(|slot| slot.label.as_str()).filter(|l| !l.is_empty()) {
            let (at, align) = if index < LEFT_COLUMN {
                (
                    Pos2::new(bounds.max.x + style.gap * scale, bounds.center().y),
                    Align2::LEFT_CENTER,
                )
            } else {
                (
                    Pos2::new(bounds.min.x - style.gap * scale, bounds.center().y),
                    Align2::RIGHT_CENTER,
                )
            };
            painter.text(at, align, label, label_font.clone(), style.slot_binding.into());
        }
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Placeholder contents, so the panel can be positioned before a character has
/// logged in.
///
/// Nineteen empty slots with the labels this client has actually confirmed --
/// including the unnamed one, which draws blank exactly as it will in play.
pub fn placeholder() -> Vec<EquipSlot> {
    const LABELS: [&str; SLOTS] = [
        "Head", "Neck", "Shoulders", "Shirt", "Chest", "Waist", "Legs", "Feet", "Wrists", "Hands",
        "Finger 1", "Finger 2", "Trinket 1", "Trinket 2", "Back", "Main Hand", "Off Hand", "",
        "Tabard",
    ];
    LABELS
        .into_iter()
        .map(|label| EquipSlot {
            label: label.to_string(),
            item: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(style: &Style, scale: f32) -> Rect {
        Rect::from_min_size(Pos2::new(80.0, 60.0), size(style, scale))
    }

    #[test]
    fn every_slot_is_clickable_at_its_own_centre() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            let rect = panel(&style, scale);
            for (index, slot) in slot_rects(rect, &style, scale).enumerate() {
                assert_eq!(
                    slot_at(rect, &style, scale, slot.center()),
                    Some(index),
                    "slot {index} at scale {scale}"
                );
            }
        }
    }

    /// Slots must not overlap and must stay inside the panel -- including
    /// across the two columns, which is the boundary a single-column formula
    /// would get wrong.
    #[test]
    fn slots_do_not_overlap_and_fit_the_panel() {
        let style = Style::default();
        let rect = panel(&style, 1.0);
        let slots: Vec<Rect> = slot_rects(rect, &style, 1.0).collect();
        assert_eq!(slots.len(), SLOTS);
        for (i, a) in slots.iter().enumerate() {
            assert!(rect.contains_rect(*a), "slot {i} escapes {rect:?}");
            for b in &slots[i + 1..] {
                assert!(!a.intersects(*b), "slot {i} overlaps: {a:?} {b:?}");
            }
        }
    }

    /// The two columns really are two columns: the second half sits to the
    /// right of the first, and each column's rows descend.
    #[test]
    fn the_panel_is_two_columns() {
        let style = Style::default();
        let slots: Vec<Rect> = slot_rects(panel(&style, 1.0), &style, 1.0).collect();
        assert_eq!(LEFT_COLUMN, 10, "ten on the left, nine on the right");

        for pair in slots[..LEFT_COLUMN].windows(2) {
            assert!(pair[0].top() < pair[1].top(), "the left column must descend");
            assert_eq!(pair[0].left(), pair[1].left());
        }
        for pair in slots[LEFT_COLUMN..].windows(2) {
            assert!(pair[0].top() < pair[1].top(), "the right column must descend");
        }
        assert!(
            slots[LEFT_COLUMN].left() > slots[0].right(),
            "the second column must clear the first"
        );
    }

    #[test]
    fn scale_multiplies_the_whole_panel() {
        let style = Style::default();
        let single = size(&style, 1.0);
        let double = size(&style, 2.0);
        assert!((double.x - single.x * 2.0).abs() < 0.001);
        assert!((double.y - single.y * 2.0).abs() < 0.001);
    }

    #[test]
    fn the_header_is_not_a_slot() {
        let style = Style::default();
        let rect = panel(&style, 1.0);
        assert_eq!(slot_at(rect, &style, 1.0, rect.min), None);
    }

    /// The placeholder must carry exactly the labels the protocol crate
    /// confirmed -- eighteen names and one deliberate blank. If a later change
    /// names slot 17, this fails and points at the measurement that did not
    /// happen.
    #[test]
    fn the_placeholder_leaves_the_unmeasured_slot_blank() {
        let slots = placeholder();
        assert_eq!(slots.len(), SLOTS);
        assert_eq!(slots[17].label, "", "slot 17 was never measured");
        assert_eq!(
            slots.iter().filter(|s| s.label.is_empty()).count(),
            1,
            "exactly one slot should be unnamed"
        );
    }
}
