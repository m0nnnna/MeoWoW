//! A cast bar: what is being cast, and how much of it has finished.
//!
//! Fills left to right as the cast completes, the opposite direction of the
//! action bar's cooldown sweep (which darkens as a spell becomes ready
//! again) -- the two are measuring different things and reading the same
//! direction for both would say "ready" and "casting" with the same shape.

use egui::{Align2, Color32, FontId, Painter, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Everything a cast bar draws, a plain snapshot like [`super::UnitView`].
#[derive(Clone, Debug, PartialEq)]
pub struct CastBarView {
    pub spell_name: String,
    /// `0.0` the instant a cast starts, `1.0` once it lands. Clamped by the
    /// drawing code, same as `SlotSpell::cooldown_fraction`.
    pub progress: f32,
    pub cast_time_ms: u32,
}

impl CastBarView {
    /// A stand-in so the bar can be positioned before anything is being cast
    /// -- the same reason `UnitView::placeholder` exists.
    pub fn placeholder() -> Self {
        Self {
            spell_name: "Casting".into(),
            progress: 0.6,
            cast_time_ms: 2500,
        }
    }
}

/// How much room a cast bar wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    Vec2::new(style.cast_bar_width, style.bar_height + style.padding * 2.0) * scale
}

/// Paints a cast bar into `rect`.
pub fn draw(painter: &Painter, rect: Rect, view: &CastBarView, style: &Style, scale: f32) {
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

    let inner = rect.shrink(style.padding * scale);
    let bar_corner = corner_radius(style.corner * scale * 0.5);
    painter.rect_filled(inner, bar_corner, style.bar_backdrop);

    let progress = view.progress.clamp(0.0, 1.0);
    if progress > 0.0 {
        let filled = Rect::from_min_size(
            inner.min,
            Vec2::new((inner.width() * progress).max(1.0), inner.height()),
        );
        painter.rect_filled(filled, bar_corner, style.casting);
    }

    let painter = painter.with_clip_rect(inner);
    let font = FontId::proportional(style.font_size * scale);
    let text: Color32 = style.text.into();
    painter.text(
        inner.left_center(),
        Align2::LEFT_CENTER,
        &view.spell_name,
        font.clone(),
        text,
    );
    if style.show_values {
        let remaining_s = ((1.0 - progress) * view.cast_time_ms as f32 / 1000.0).max(0.0);
        painter.text(
            inner.right_center(),
            Align2::RIGHT_CENTER,
            format!("{remaining_s:.1}"),
            font,
            text,
        );
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_multiplies_the_whole_bar() {
        let style = Style::default();
        assert_eq!(size(&style, 2.0), size(&style, 1.0) * 2.0);
    }
}
