//! An experience bar: how far into the current level the character is.
//!
//! Deliberately the plainest frame in this crate. There is no name to draw and
//! no state to switch on -- just a fraction of a bar filled in, the same shape
//! [`super::cast_bar`] uses for its own fill. The `current / next_level`
//! numbers are drawn across it exactly the way [`super::unit`]'s health bar
//! and [`super::party`]'s bars draw theirs, behind the identical
//! `style.show_values` toggle -- one flag turns numbers off every bar in the
//! interface, not this one specially.

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Everything an XP bar draws, a plain snapshot like [`super::CastBarView`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XpBarView {
    pub current: u32,
    /// How much the *next* level takes, not the total ever earned -- the
    /// same "toward the next threshold" number [`Self::fraction`] fills the
    /// bar with. Zero is a real, if rare, wire value (absent field, not a
    /// division guard): see [`Self::fraction`].
    pub next_level: u32,
}

impl XpBarView {
    /// A stand-in so the bar can be positioned before a character is logged
    /// in -- the same reason [`super::CastBarView::placeholder`] exists.
    pub fn placeholder() -> Self {
        Self {
            current: 65,
            next_level: 100,
        }
    }

    /// `0.0` at the start of a level, `1.0` at the end. Clamped, and safe
    /// against `next_level` being zero -- an absent or not-yet-replicated
    /// field reads as no fill rather than a division by zero.
    pub fn fraction(&self) -> f32 {
        if self.next_level == 0 {
            return 0.0;
        }
        (self.current as f32 / self.next_level as f32).clamp(0.0, 1.0)
    }
}

/// How much room an XP bar wants: as wide as an action bar, so the two read
/// as one stack, and thin enough to sit flush beneath it.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    Vec2::new(
        super::action_bar::size(style, scale).x,
        style.xp_bar_height * scale,
    )
}

/// Paints an XP bar into `rect`.
pub fn draw(painter: &Painter, rect: Rect, view: &XpBarView, style: &Style, scale: f32) {
    let corner = corner_radius(style.corner * scale * 0.5);
    painter.rect_filled(rect, corner, style.bar_backdrop);
    if style.border_width > 0.0 {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(style.border_width * scale, style.border),
            StrokeKind::Inside,
        );
    }

    let fraction = view.fraction();
    if fraction > 0.0 {
        let filled = Rect::from_min_size(
            rect.min,
            Vec2::new((rect.width() * fraction).max(1.0), rect.height()),
        );
        painter.rect_filled(filled, corner, style.xp_fill);
    }

    // Clipped to the bar, so a wide number cannot spill into whatever is
    // stacked beneath it -- the same guard `unit::draw` uses for its own
    // health and power numbers.
    let painter = painter.with_clip_rect(rect);
    if style.show_values {
        let font = FontId::proportional(style.font_size * scale);
        let text: Color32 = style.text.into();
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            format!("{} / {}", view.current, view.next_level),
            font,
            text,
        );
    }
}

fn corner_radius(radius: f32) -> CornerRadius {
    CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_multiplies_the_whole_bar() {
        let style = Style::default();
        assert_eq!(size(&style, 2.0), size(&style, 1.0) * 2.0);
    }

    #[test]
    fn the_bar_is_as_wide_as_an_action_bar() {
        let style = Style::default();
        assert_eq!(size(&style, 1.0).x, super::super::action_bar::size(&style, 1.0).x);
    }

    #[test]
    fn halfway_through_a_level_fills_half_the_bar() {
        let view = XpBarView {
            current: 50,
            next_level: 100,
        };
        assert_eq!(view.fraction(), 0.5);
    }

    /// The field this reads is `PRIVATE` and absent before the first login
    /// burst finishes -- see `world::update::fields::PLAYER_NEXT_LEVEL_XP`.
    /// An absent `next_level` must read as an empty bar, not a crash.
    #[test]
    fn a_missing_next_level_does_not_divide_by_zero() {
        let view = XpBarView {
            current: 50,
            next_level: 0,
        };
        assert_eq!(view.fraction(), 0.0);
    }

    #[test]
    fn a_fraction_past_one_is_clamped() {
        let view = XpBarView {
            current: 150,
            next_level: 100,
        };
        assert_eq!(view.fraction(), 1.0);
    }
}
