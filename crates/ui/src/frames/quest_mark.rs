//! The exclamation and question marks that float over questgivers.
//!
//! Deliberately **not** an [`crate::Element`], for the same reason
//! [`crate::frames::marker`] is not: this follows a creature around the world
//! and has no anchor or offset to place it by. It is switched off, resized and
//! recoloured in [`crate::Style`] instead.
//!
//! **The mark is the server's answer, never this client's opinion.** Whether a
//! quest can be taken depends on level, race, class, reputation, every
//! prerequisite in its chain and whatever the realm has been scripted to
//! check; a client working that out from a quest table would be reimplementing
//! the server's eligibility rules and would be wrong on any realm with custom
//! content. So `world::quest::QuestgiverMark` comes off the wire and this
//! draws it -- and a value nobody has seen the server produce draws nothing at
//! all, because a wrong exclamation sends a player somewhere for no reason.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect};

/// Which mark to draw. Mirrors the measured half of
/// `world::quest::QuestgiverMark`; this crate depends on neither `world` nor
/// the game data, so the caller translates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestMark {
    /// A quest is on offer: the bright exclamation.
    Available,
    /// On offer, and outlevelled: the grey exclamation.
    AvailableTrivial,
    /// In the log and unfinished: the grey question mark.
    Incomplete,
    /// In the log and finished: the bright question mark.
    Complete,
}

impl QuestMark {
    /// The glyph itself. `!` for something to take, `?` for something to hand
    /// back -- the shapes 3.3.5a uses, because a player already knows them.
    pub fn glyph(self) -> &'static str {
        match self {
            QuestMark::Available | QuestMark::AvailableTrivial => "!",
            QuestMark::Incomplete | QuestMark::Complete => "?",
        }
    }

    /// Whether this is the bright form. Brightness carries "you can act on
    /// this now" and the glyph carries "take" against "hand in", so the two
    /// axes stay independent -- which is what makes four states legible with
    /// two shapes.
    pub fn is_bright(self) -> bool {
        matches!(self, QuestMark::Available | QuestMark::Complete)
    }
}

/// Where a mark sits given the box its owner occupies on screen.
///
/// Above the head and centred, and derived from the same box the selection
/// bracket uses so the two cannot drift apart.
pub fn position(over: Rect, size: f32) -> Pos2 {
    Pos2::new(over.center().x, over.min.y - size * 0.6)
}

/// Draws one mark over a unit.
///
/// Painted five times: four black offsets and then the colour on top. An
/// outline rather than a shadow, because these are read against grass, stone
/// and sky in turn, and a single dark drop shadow disappears against exactly
/// one of them.
pub fn draw(painter: &Painter, over: Rect, mark: QuestMark, bright: Color32, dim: Color32, size: f32) {
    if !over.is_positive() || size <= 0.0 {
        return;
    }
    let at = position(over, size);
    let font = FontId::proportional(size);
    let colour = if mark.is_bright() { bright } else { dim };
    let outline = size * 0.08;
    for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
        painter.text(
            Pos2::new(at.x + dx * outline, at.y + dy * outline),
            Align2::CENTER_BOTTOM,
            mark.glyph(),
            font.clone(),
            Color32::from_black_alpha(220),
        );
    }
    painter.text(at, Align2::CENTER_BOTTOM, mark.glyph(), font, colour);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two shapes and two brightnesses, and the four states each pick one of
    /// each. Asserted as a table rather than one case, because the failure
    /// worth catching is two states sharing a look -- a player cannot tell
    /// "come back when you are done" from "hand it in" if both are a bright
    /// question mark.
    #[test]
    fn every_mark_is_distinguishable_from_the_others() {
        let all = [
            QuestMark::Available,
            QuestMark::AvailableTrivial,
            QuestMark::Incomplete,
            QuestMark::Complete,
        ];
        let looks: Vec<(&str, bool)> = all.iter().map(|m| (m.glyph(), m.is_bright())).collect();
        for (i, a) in looks.iter().enumerate() {
            for b in &looks[i + 1..] {
                assert_ne!(a, b, "two marks look the same: {looks:?}");
            }
        }
        // And the shapes are the ones a player of 3.3.5a already knows.
        assert_eq!(QuestMark::Available.glyph(), "!");
        assert_eq!(QuestMark::Complete.glyph(), "?");
        assert!(QuestMark::Available.is_bright());
        assert!(!QuestMark::AvailableTrivial.is_bright());
        assert!(!QuestMark::Incomplete.is_bright());
        assert!(QuestMark::Complete.is_bright());
    }

    /// The mark goes above the head, not on it: a glyph over a creature's face
    /// hides the thing you are looking at.
    #[test]
    fn the_mark_sits_above_the_box() {
        let over = Rect::from_min_max(Pos2::new(100.0, 200.0), Pos2::new(140.0, 280.0));
        let at = position(over, 20.0);
        assert!(at.y < over.min.y, "{at:?} is not above {over:?}");
        assert!((at.x - over.center().x).abs() < 1e-6);
    }
}
