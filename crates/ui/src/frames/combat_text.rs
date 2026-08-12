//! Damage numbers that rise and fade above whoever they hit.
//!
//! Drawn the same way [`crate::frames::marker`] is: straight onto a layer
//! rather than through an [`crate::element::Element`]. A floating number
//! follows a *swing*, not a place on screen, so it has no anchor or offset to
//! live at -- the caller projects a world point fresh every frame (see
//! `apps/viewer/src/hud.rs`'s `combat_text_anchor`, `marker_rect`'s same
//! trick applied to a point instead of a box) and this module only draws
//! wherever that projection lands.
//!
//! This module never reads a clock. The caller owns the animation's timeline
//! and hands in `age`; that keeps the rise-and-fade math testable without a
//! real `Instant` anywhere near it, the same split [`crate::frames::cast_bar`]
//! makes between progress and the wall clock that produces it.

use egui::{Align2, FontId, Painter, Pos2};

use crate::style::{Color, Style};

/// What kind of number this is, for colour and size -- this crate's own
/// categories, not the protocol's, the same split [`crate::frames::chat::ChatKind`]
/// makes for a line of scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatTextKind {
    Damage,
    /// Drawn larger as well as in its own colour -- see [`draw`].
    Critical,
    Miss,
}

/// One number in flight.
///
/// `age` is `0.0` at spawn and `1.0` once fully faded, as a fraction of
/// [`Style::combat_text_lifetime_ms`]. The caller computes it fresh each
/// frame from a spawn timestamp; nothing here holds one.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatingText {
    /// Where the text starts, before this frame's rise is applied.
    pub pos: Pos2,
    /// What actually shows -- a damage figure or `"Miss"`. Decided by the
    /// caller, not here: this module draws whatever string it is given and
    /// invents no wording of its own.
    pub text: String,
    pub kind: CombatTextKind,
    pub age: f32,
}

/// The colour a kind of number is drawn in.
pub fn colour(kind: CombatTextKind, style: &Style) -> Color {
    match kind {
        CombatTextKind::Damage => style.combat_text_damage,
        CombatTextKind::Critical => style.combat_text_critical,
        CombatTextKind::Miss => style.combat_text_miss,
    }
}

/// Paints every entry, each risen and faded by its own age.
pub fn draw(painter: &Painter, entries: &[FloatingText], style: &Style) {
    for entry in entries {
        let age = entry.age.clamp(0.0, 1.0);
        let pos = entry.pos - egui::vec2(0.0, style.combat_text_rise * age);

        let mut colour = colour(entry.kind, style);
        colour.0[3] = (colour.0[3] as f32 * (1.0 - age)).round() as u8;
        if colour.0[3] == 0 {
            // Fully transparent text still costs a galley layout and a shape
            // for nothing anyone can see; the caller should have pruned this
            // entry, but skipping it here costs nothing and draws nothing
            // wrong either way.
            continue;
        }

        // A critical hit reads larger, the same way the combat log marks one
        // with "(critical)" rather than leaving the number to speak for
        // itself -- see `world::combat::describe_swing`.
        let size = if entry.kind == CombatTextKind::Critical {
            style.combat_text_size * 1.3
        } else {
            style.combat_text_size
        };

        painter.text(
            pos,
            Align2::CENTER_BOTTOM,
            &entry.text,
            FontId::proportional(size),
            colour.into(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(age: f32, kind: CombatTextKind) -> FloatingText {
        FloatingText {
            pos: Pos2::new(100.0, 100.0),
            text: "6".into(),
            kind,
            age,
        }
    }

    /// Every kind must have a colour of its own, or a miss and a normal hit
    /// look identical -- the same requirement `chat::colour` has for
    /// `ChatKind`.
    #[test]
    fn every_kind_has_its_own_colour() {
        let style = Style::default();
        let kinds = [
            CombatTextKind::Damage,
            CombatTextKind::Critical,
            CombatTextKind::Miss,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(
                    colour(*a, &style),
                    colour(*b, &style),
                    "{a:?} and {b:?} are the same colour"
                );
            }
        }
    }

    /// What `draw` actually painted, as text, so a colour can be compared.
    ///
    /// The two tests below used to recompute `draw`'s own arithmetic and
    /// assert on the result, which is a thing checked against itself: had
    /// `draw` stopped rising or fading altogether they would both still have
    /// passed. `Hud`-level tests in `crate::lib` cover the rise reaching the
    /// screen; the fade was covered by nothing at all.
    fn painted(entries: &[FloatingText]) -> String {
        let ctx = egui::Context::default();
        let style = Style::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ctx| {
            draw(
                &ctx.layer_painter(egui::LayerId::background()),
                entries,
                &style,
            );
        });
        let rendered = format!("{:?}", output.shapes);
        output.drop_without_applying_deltas();
        rendered
    }

    /// Ageing has to change what reaches the screen, through `draw` itself.
    #[test]
    fn ageing_changes_what_draw_paints() {
        let fresh = painted(&[entry(0.0, CombatTextKind::Damage)]);
        let older = painted(&[entry(0.6, CombatTextKind::Damage)]);
        assert!(fresh.len() > 50, "a fresh number painted nothing at all");
        assert_ne!(
            fresh, older,
            "ageing a number changed neither its position nor its colour"
        );
    }

    /// A fully aged number is gone, not merely invisible.
    ///
    /// This is the one assertion that distinguishes a `draw` which fades from
    /// one which ignores `age` entirely, and it is observable rather than
    /// recomputed: nothing is painted at all.
    #[test]
    fn a_fully_aged_number_paints_nothing() {
        let gone = painted(&[entry(1.0, CombatTextKind::Damage)]);
        let empty = painted(&[]);
        assert_eq!(gone, empty, "a fully faded number was still painted");
    }
}
