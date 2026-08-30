//! The prompt shown while dead and not yet released.
//!
//! Deliberately the plainest frame here: one line of text and a click target
//! covering the whole rectangle, because there is exactly one thing to do
//! with it. Modelled on [`super::cast_bar`], the other single-purpose bar,
//! rather than on the loot window -- there is no list to lay out, so nothing
//! here needs `loot`'s row geometry.

use egui::{Align2, Color32, FontId, Painter, Rect, Stroke, StrokeKind, Vec2};
use egui::emath::Rot2;

use crate::style::Style;

/// Everything the prompt draws, a plain snapshot like [`super::UnitView`].
#[derive(Clone, Debug, PartialEq)]
pub struct ReleasePromptView {
    /// What the frame says. Carried as text rather than as an enum the
    /// drawing code switches on, because the caller already knows the exact
    /// wording it wants ("You have died." versus a delay-specific message)
    /// and this crate has no business duplicating that judgement.
    pub text: String,
    /// Which way the character's body lies, as a screen-space angle in
    /// radians: `0` points right, `+` turns clockwise (egui's y grows
    /// downward). `None` while alive, before the corpse query is answered, or
    /// while not a ghost -- there is nothing to point at.
    ///
    /// Resolved by the caller from the camera and the body's world position,
    /// the same division of labour the world-anchored markers use: this crate
    /// draws the arrow it is handed and knows nothing about where a corpse is.
    pub body_bearing: Option<f32>,
}

impl ReleasePromptView {
    /// A stand-in so the prompt can be positioned without dying first -- the
    /// same reason `UnitView::placeholder` and `CastBarView::placeholder`
    /// exist.
    pub fn placeholder() -> Self {
        Self {
            text: "You have died. Click to release your spirit.".into(),
            body_bearing: Some(-std::f32::consts::FRAC_PI_2),
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

    // The body arrow sits at the left end of the prompt, clear of the
    // centred text. A ghost that has released has one thing left to do --
    // reach the body -- and until now the only hint of which way that was
    // lay on the minimap. Drawn before the text's clip rect is set so it is
    // not clipped away.
    if let Some(bearing) = view.body_bearing {
        let disc = (style.bar_height * scale * 0.75).max(7.0);
        let centre = egui::pos2(inner.left() + disc, rect.center().y);
        painter.circle_filled(centre, disc, style.background);
        painter.circle_stroke(
            centre,
            disc,
            Stroke::new(style.border_width.max(1.0) * scale, stroke_color),
        );
        let rot = Rot2::from_angle(bearing);
        let tip = centre + rot * egui::vec2(disc * 0.72, 0.0);
        let back = disc * 0.5;
        let wing = disc * 0.5;
        let left = centre + rot * egui::vec2(-back, -wing);
        let right = centre + rot * egui::vec2(-back, wing);
        painter.add(egui::Shape::convex_polygon(
            vec![tip, left, right],
            stroke_color,
            Stroke::NONE,
        ));
    }

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
