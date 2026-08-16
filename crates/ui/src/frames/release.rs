//! The prompt shown while dead and not yet released.
//!
//! Deliberately the plainest frame here: one line of text and a click target
//! covering the whole rectangle, because there is exactly one thing to do
//! with it. Modelled on [`super::cast_bar`], the other single-purpose bar,
//! rather than on the loot window -- there is no list to lay out, so nothing
//! here needs `loot`'s row geometry.

use egui::{Align2, Color32, FontId, Painter, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Everything the prompt draws, a plain snapshot like [`super::UnitView`].
#[derive(Clone, Debug, PartialEq)]
pub struct ReleasePromptView {
    /// What the frame says. Carried as text rather than as an enum the
    /// drawing code switches on, because the caller already knows the exact
    /// wording it wants ("You have died." versus a delay-specific message)
    /// and this crate has no business duplicating that judgement.
    pub text: String,
}

impl ReleasePromptView {
    /// A stand-in so the prompt can be positioned without dying first -- the
    /// same reason `UnitView::placeholder` and `CastBarView::placeholder`
    /// exist.
    pub fn placeholder() -> Self {
        Self {
            text: "You have died. Click to release your spirit.".into(),
        }
    }
}

/// How much room the prompt wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    Vec2::new(
        style.release_prompt_width,
        style.bar_height + style.padding * 2.0,
    ) * scale
}

/// Paints the prompt into `rect`.
pub fn draw(painter: &Painter, rect: Rect, view: &ReleasePromptView, style: &Style, scale: f32) {
    let corner = corner_radius(style.corner * scale);
    painter.rect_filled(rect, corner, style.background);
    let stroke_color: Color32 = style.release_prompt_text.into();
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(style.border_width.max(1.0) * scale, stroke_color),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(style.padding * scale);
    let painter = painter.with_clip_rect(inner);
    let font = FontId::proportional(style.font_size * scale);
    painter.text(
        inner.center(),
        Align2::CENTER_CENTER,
        &view.text,
        font,
        stroke_color,
    );
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_multiplies_the_whole_prompt() {
        let style = Style::default();
        assert_eq!(size(&style, 2.0), size(&style, 1.0) * 2.0);
    }
}
