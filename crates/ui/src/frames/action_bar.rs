//! A row of action slots.
//!
//! Twelve slots to a bar, matching the twelve keys along the number row
//! (`1`-`0`, `-`, `=`). Three bars, reached by no modifier, Shift and Ctrl.
//!
//! Two things here are deliberate and worth stating.
//!
//! **The slot geometry lives in one function.** [`slot_rects`] is used by the
//! drawing *and* by the hit test, rather than each computing its own idea of
//! where slot seven is. Two copies would agree until one of them changed, and
//! the failure -- a click that casts the spell next to the one you pressed --
//! reads as a targeting bug rather than as a layout one. Same rule that keeps
//! the picking ray on the matrix the scene is drawn with.
//!
//! **An icon is optional.** This crate takes an already-uploaded texture id and
//! knows nothing about where it came from. Without a game installation there
//! are no icons at all, and a slot then draws the spell's name instead: the
//! client still works, it is just less pretty. Making the interface *require*
//! game data would be the one place this project's "no bundled assets" rule
//! turned into "does not run without them".

use egui::{Align2, Color32, FontId, Painter, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Slots per bar: the number row, `1` through `=`.
pub const SLOTS: usize = 12;
/// Bars: unmodified, Shift, Ctrl.
pub const BARS: usize = 3;

/// What one slot is showing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlotView {
    /// The key that triggers it, as it should read on screen: `1`, `S-1`.
    pub binding: String,
    pub spell: Option<SlotSpell>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlotSpell {
    pub id: u32,
    pub name: String,
    /// Empty when the spell has none, or when there is no game installation
    /// to read it from -- most spells have no rank at all.
    pub rank: String,
    /// Empty under the same conditions as `rank`.
    pub description: String,
    /// `None` when the icon could not be loaded, or when there is no game
    /// installation to load it from. The slot falls back to text.
    pub icon: Option<egui::TextureId>,
    /// `1.0` the instant a cast starts its cooldown, `0.0` once it is ready
    /// again. Never negative and never above `1.0` -- the caller clamps it,
    /// same as `Element::scale`, so a wrong duration cannot paint a sweep
    /// that overshoots or reverses.
    pub cooldown_fraction: f32,
}

/// How much room a bar wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    let slot = style.slot_size;
    let width = SLOTS as f32 * slot + (SLOTS as f32 - 1.0) * style.slot_gap + style.padding * 2.0;
    let height = slot + style.padding * 2.0;
    Vec2::new(width, height) * scale
}

/// Where each slot sits inside a bar.
///
/// The single source of truth for slot geometry -- see the module comment.
pub fn slot_rects(rect: Rect, style: &Style, scale: f32) -> impl Iterator<Item = Rect> + '_ {
    let slot = style.slot_size * scale;
    let gap = style.slot_gap * scale;
    let origin = rect.min + Vec2::splat(style.padding * scale);
    (0..SLOTS).map(move |index| {
        Rect::from_min_size(
            origin + Vec2::new(index as f32 * (slot + gap), 0.0),
            Vec2::splat(slot),
        )
    })
}

/// Which slot contains a point, if any.
pub fn slot_at(rect: Rect, style: &Style, scale: f32, point: egui::Pos2) -> Option<usize> {
    slot_rects(rect, style, scale).position(|slot| slot.contains(point))
}

/// Paints a bar.
pub fn draw(painter: &Painter, rect: Rect, slots: &[SlotView], style: &Style, scale: f32) {
    let corner = corner_radius(style.corner * scale);
    painter.rect_filled(rect, corner, style.background);
    if style.border_width > 0.0 {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(style.border_width * scale, style.border),
            StrokeKind::Inside,
        );
    }

    let slot_corner = corner_radius(style.corner * scale * 0.5);
    let text: Color32 = style.text.into();

    for (index, bounds) in slot_rects(rect, style, scale).enumerate() {
        let view = slots.get(index);
        painter.rect_filled(bounds, slot_corner, style.slot_background);

        match view.and_then(|view| view.spell.as_ref()) {
            Some(spell) => {
                match spell.icon {
                    // The icon fills the slot, inset by the border so the
                    // artwork does not sit on top of it.
                    Some(icon) => {
                        painter.image(
                            icon,
                            bounds.shrink(style.border_width * scale),
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    // No game data, or an icon that would not load. The slot
                    // still says what is in it.
                    None => {
                        let painter = painter.with_clip_rect(bounds);
                        painter.text(
                            bounds.center(),
                            Align2::CENTER_CENTER,
                            abbreviate(&spell.name),
                            FontId::proportional(style.font_size * scale * 0.85),
                            text,
                        );
                    }
                }
                draw_cooldown(painter, bounds, spell.cooldown_fraction);
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

        // The key, bottom-right, over whatever the slot is showing. Drawn last
        // so an icon cannot hide it: a bar you cannot read the bindings off is
        // a bar you have to remember, which defeats having one.
        if let Some(view) = view {
            if !view.binding.is_empty() {
                let corner_pos = bounds.right_bottom() - Vec2::splat(1.0 * scale);
                painter.text(
                    corner_pos,
                    Align2::RIGHT_BOTTOM,
                    &view.binding,
                    FontId::proportional(style.font_size * scale * 0.8),
                    style.slot_binding.into(),
                );
            }
        }
    }
}

/// Darkens a slot by the fraction of its cooldown still remaining: fully
/// covered the instant a cast starts one, bare again once it clears.
///
/// A rectangle wiping away from the bottom rather than WoW's own radial swipe
/// -- it says the same thing (how much is left, at a glance, with no text to
/// read) for a fraction of the geometry, and this project already has a rule
/// against building more than a task needs.
fn draw_cooldown(painter: &Painter, bounds: Rect, fraction: f32) {
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return;
    }
    let covered = Rect::from_min_max(
        bounds.min,
        egui::pos2(bounds.max.x, bounds.min.y + bounds.height() * fraction),
    );
    painter.rect_filled(covered, 0, Color32::from_black_alpha(170));
}

/// Explains what is in a hovered slot: name, rank if it has one, and
/// description.
///
/// Wanted because two slots can look identical -- `Activate Primary Spec`
/// and `Activate Secondary Spec` share `spell_icon_id` 2970 -- and the name
/// is the only thing that tells them apart.
pub fn hover_tooltip(response: &egui::Response, spell: &SlotSpell) {
    egui::Tooltip::for_widget(response).at_pointer().show(|ui| {
        ui.strong(&spell.name);
        if !spell.rank.is_empty() {
            ui.label(&spell.rank);
        }
        if !spell.description.is_empty() {
            ui.label(&spell.description);
        }
    });
}

/// Shortens a spell name to something that fits an icon-sized square.
///
/// Initials rather than a truncation, because `Heroic Strike` and `Heroic
/// Throw` truncate to the same thing at this width, and two slots that look
/// identical are worse than two that look cryptic.
pub(crate) fn abbreviate(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    match words.len() {
        0 => String::new(),
        1 => words[0].chars().take(4).collect(),
        _ => words
            .iter()
            .filter_map(|word| word.chars().next())
            .take(4)
            .collect(),
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// The label for a slot on a given bar, as it reads on screen.
pub fn binding_label(bar: usize, slot: usize) -> String {
    const KEYS: [&str; SLOTS] = [
        "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "=",
    ];
    let key = KEYS.get(slot).copied().unwrap_or("");
    match bar {
        0 => key.to_string(),
        1 => format!("S-{key}"),
        2 => format!("C-{key}"),
        _ => key.to_string(),
    }
}

/// Placeholder contents, so a bar can be positioned before anything is on it.
pub fn placeholder(bar: usize) -> Vec<SlotView> {
    (0..SLOTS)
        .map(|slot| SlotView {
            binding: binding_label(bar, slot),
            spell: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar_rect(style: &Style, scale: f32) -> Rect {
        Rect::from_min_size(egui::pos2(100.0, 500.0), size(style, scale))
    }

    /// The hit test and the drawing must use the same geometry, or a click
    /// casts the spell next to the one you pressed -- which reads as a
    /// targeting bug rather than a layout one.
    #[test]
    fn every_slot_is_clickable_at_its_own_centre() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            let rect = bar_rect(&style, scale);
            for (index, slot) in slot_rects(rect, &style, scale).enumerate() {
                assert_eq!(
                    slot_at(rect, &style, scale, slot.center()),
                    Some(index),
                    "slot {index} at scale {scale}"
                );
            }
        }
    }

    /// Slots must not overlap, or a click near an edge is ambiguous.
    #[test]
    fn slots_do_not_overlap_and_fit_the_bar() {
        let style = Style::default();
        let rect = bar_rect(&style, 1.0);
        let rects: Vec<Rect> = slot_rects(rect, &style, 1.0).collect();
        assert_eq!(rects.len(), SLOTS);
        for pair in rects.windows(2) {
            assert!(
                pair[0].right() <= pair[1].left(),
                "{:?} overlaps {:?}",
                pair[0],
                pair[1]
            );
        }
        for slot in &rects {
            assert!(rect.contains_rect(*slot), "{slot:?} escapes {rect:?}");
        }
    }

    /// A click in the padding between the bar's edge and its first slot is not
    /// a click on a slot.
    #[test]
    fn the_gaps_belong_to_nothing() {
        let style = Style::default();
        let rect = bar_rect(&style, 1.0);
        assert_eq!(slot_at(rect, &style, 1.0, rect.min), None);
        assert_eq!(
            slot_at(rect, &style, 1.0, egui::pos2(rect.min.x + 1.0, rect.min.y + 1.0)),
            None
        );
    }

    #[test]
    fn scale_multiplies_the_whole_bar() {
        let style = Style::default();
        assert_eq!(size(&style, 2.0), size(&style, 1.0) * 2.0);
    }

    /// Two spells that share a prefix must not abbreviate to the same thing.
    #[test]
    fn abbreviations_distinguish_similar_names() {
        assert_ne!(abbreviate("Heroic Strike"), abbreviate("Heroic Throw"));
        assert_eq!(abbreviate("Heroic Strike"), "HS");
        assert_eq!(abbreviate("Charge"), "Char");
    }

    /// The bindings are what the user reads off the bar to know what to press,
    /// so they have to match what the viewer actually binds.
    #[test]
    fn bindings_read_as_the_keys_that_trigger_them() {
        assert_eq!(binding_label(0, 0), "1");
        assert_eq!(binding_label(0, 9), "0");
        assert_eq!(binding_label(0, 10), "-");
        assert_eq!(binding_label(0, 11), "=");
        assert_eq!(binding_label(1, 0), "S-1");
        assert_eq!(binding_label(2, 11), "C-=");
    }
}
