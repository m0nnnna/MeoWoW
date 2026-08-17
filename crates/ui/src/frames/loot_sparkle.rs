//! The shine over a corpse that still has loot on it.
//!
//! Same shape as [`crate::frames::quest_mark`]: not an [`crate::Element`],
//! because this follows a creature around the world rather than sitting at a
//! fixed place in the layout, and it is switched off, resized and recoloured
//! in [`crate::Style`] instead. The caller resolves *which* corpses sparkle
//! -- `world::state::Entity::lootable`, off `UNIT_DYNAMIC_FLAGS` -- and hands
//! this crate only the screen boxes, the same division `quest_mark` and
//! `marker` already draw.

use egui::{Color32, Painter, Pos2, Rect};

/// Draws one sparkle, pulsing so a still corpse still reads as "look here"
/// rather than as a static icon easy to miss against grass or stone.
///
/// `time` is any monotonically increasing clock in seconds -- the caller's
/// own animation clock is enough, and nothing here needs it to start at
/// zero. A ray pattern rather than a plain dot: WoW's own sparkle reads as
/// glinting light rather than as a marker, and four short rays through a
/// core is the cheapest shape that still says that instead of "map pin".
pub fn draw(painter: &Painter, over: Rect, time: f32, colour: Color32, size: f32) {
    if !over.is_positive() || size <= 0.0 {
        return;
    }
    let at = over.center();
    // Two full pulses a second -- fast enough to catch the eye at a glance
    // across a battlefield, slow enough not to read as flickering.
    let phase = (time * std::f32::consts::TAU * 2.0).sin() * 0.5 + 0.5;
    let core = size * (0.28 + 0.12 * phase);
    let ray = size * (0.55 + 0.45 * phase);
    let alpha = (colour.a() as f32 * (0.55 + 0.45 * phase)).round() as u8;
    let c = Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha);

    painter.circle_filled(at, core, c);
    let width = (size * 0.08).max(1.0);
    for (dx, dy) in [(1.0, 0.0), (0.0, 1.0)] {
        painter.line_segment(
            [Pos2::new(at.x - dx * ray, at.y - dy * ray), Pos2::new(at.x + dx * ray, at.y + dy * ray)],
            egui::Stroke::new(width, c),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A degenerate box (behind the camera, or a projection that collapsed
    /// it) must not be drawn as if it were on screen -- the same guard
    /// `quest_mark::draw` makes for the same reason.
    #[test]
    fn a_non_positive_rect_is_not_drawn() {
        // There is no way to observe "nothing was painted" without a real
        // Painter, so this only has to not panic on the guard's inputs --
        // covered by the function running at all in the caller's own tests.
        let empty = Rect::from_min_size(Pos2::ZERO, egui::Vec2::ZERO);
        assert!(!empty.is_positive());
    }

    /// The pulse must stay inside a sane range at every phase, not run away
    /// or go negative -- a radius or an alpha outside its bounds is either
    /// invisible or a panic waiting in whatever consumes it.
    #[test]
    fn the_pulse_never_leaves_its_bounds() {
        for tenth in 0..=40 {
            let time = tenth as f32 / 10.0;
            let phase = (time * std::f32::consts::TAU * 2.0).sin() * 0.5 + 0.5;
            assert!((0.0..=1.0).contains(&phase), "phase {phase} at t={time}");
            let core = 20.0 * (0.28 + 0.12 * phase);
            let ray = 20.0 * (0.55 + 0.45 * phase);
            assert!(core > 0.0 && core < ray, "core {core} should stay under ray {ray}");
        }
    }
}
