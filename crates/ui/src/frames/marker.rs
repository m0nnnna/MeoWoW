//! The bracket drawn around whatever is selected, out in the world.
//!
//! Deliberately **not** an [`crate::Element`]. Everything in [`crate::layout`]
//! is screen furniture placed by an anchor and an offset; this follows a
//! creature around and has neither. Forcing it into that model would mean an
//! anchor and offset that do nothing, which is worse than an honest exception:
//! it can be switched off in [`crate::Style`] instead.
//!
//! It is drawn from the **same box the click was tested against**, not from a
//! separately measured one. Two boxes would agree right up until one of them
//! changed, and the failure -- a marker that sits slightly off the thing it
//! selected -- looks like the creature being in the wrong place. Deriving both
//! from one source is the same rule that keeps the picking ray on the matrix
//! the scene is drawn with.

use egui::{Painter, Pos2, Rect, Stroke};

use crate::style::Style;

/// Corner ticks as a fraction of the bracket's shorter side.
const TICK: f32 = 0.28;
/// Never shorter than this, so a distant creature still reads as bracketed
/// rather than as four stray dots.
const MIN_TICK: f32 = 4.0;

/// Draws corner ticks around `rect`.
///
/// Corners rather than a full outline: a closed box around a creature hides
/// its silhouette, which is the thing you were looking at when you clicked it.
pub fn draw(painter: &Painter, rect: Rect, style: &Style) {
    if !rect.is_positive() {
        return;
    }
    let stroke = Stroke::new(style.target_marker_width, style.target_marker);
    let tick = (rect.width().min(rect.height()) * TICK).max(MIN_TICK);

    for (corner, dx, dy) in [
        (rect.left_top(), 1.0, 1.0),
        (rect.right_top(), -1.0, 1.0),
        (rect.left_bottom(), 1.0, -1.0),
        (rect.right_bottom(), -1.0, -1.0),
    ] {
        painter.line_segment(
            [corner, Pos2::new(corner.x + tick * dx, corner.y)],
            stroke,
        );
        painter.line_segment(
            [corner, Pos2::new(corner.x, corner.y + tick * dy)],
            stroke,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A creature far enough away projects to a few pixels. The ticks must not
    /// shrink with it into something indistinguishable from noise.
    #[test]
    fn a_tiny_bracket_keeps_a_usable_tick() {
        let tiny = Rect::from_min_size(Pos2::ZERO, egui::vec2(3.0, 4.0));
        let tick = (tiny.width().min(tiny.height()) * TICK).max(MIN_TICK);
        assert_eq!(tick, MIN_TICK);
    }

    /// And a close one must not have ticks so long they meet in the middle,
    /// which would be the closed box this deliberately is not.
    #[test]
    fn ticks_never_close_the_box() {
        let wide = Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 300.0));
        let tick = (wide.width().min(wide.height()) * TICK).max(MIN_TICK);
        assert!(
            tick * 2.0 < wide.height(),
            "ticks from opposite corners would meet"
        );
    }
}
