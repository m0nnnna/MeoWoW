//! "Destroy Refreshing Spring Water?" -- the confirmation that stands between
//! a bag drag that missed every window and an item actually being gone.
//!
//! **Two buttons, like [`super::party_invite`] and for the identical
//! reason:** the answers are opposite and one of them is irreversible, so a
//! press between them must answer neither. Modelled closely on that frame's
//! geometry rather than [`super::release`]'s single click-anywhere rectangle.
//!
//! **Not a customisable element.** Every other window in this interface can
//! be dragged to a new spot because a player might want it somewhere else
//! for the rest of the session; this one exists for a few seconds at a time
//! and is gone before repositioning it would matter, so it is drawn at a
//! fixed screen position instead of through [`crate::layout`].

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// What is about to be destroyed.
#[derive(Clone, Debug, PartialEq)]
pub struct DestroyPromptView {
    pub name: String,
    pub icon: Option<egui::TextureId>,
}

/// Which button was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestroyAnswer {
    Confirm,
    Cancel,
}

/// How much room the prompt wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    let height = style.padding * 3.0 + style.font_size * 1.3 + style.party_invite_button_height;
    Vec2::new(style.party_invite_width, height) * scale
}

/// Where the two buttons are. Stated once, the way [`super::party_invite`]'s
/// are, so the drawing and the hit test cannot disagree about which half of
/// the prompt answers which way.
pub(crate) fn buttons(rect: Rect, style: &Style, scale: f32) -> (Rect, Rect) {
    let padding = style.padding * scale;
    let gap = style.gap * scale;
    let height = style.party_invite_button_height * scale;
    let width = ((rect.width() - padding * 2.0 - gap) * 0.5).max(1.0);
    let top = rect.bottom() - padding - height;
    let confirm = Rect::from_min_size(
        Pos2::new(rect.left() + padding, top),
        Vec2::new(width, height),
    );
    let cancel = Rect::from_min_size(
        Pos2::new(confirm.right() + gap, top),
        Vec2::new(width, height),
    );
    (confirm, cancel)
}

/// Which answer a click at `point` is, or `None` for a press that missed
/// both buttons -- deliberately not a nearest-button guess, the same call
/// [`super::party_invite::click_at`] makes and for the same reason: this
/// answer cannot be undone by leaving a group, it destroys the item outright.
pub fn click_at(rect: Rect, style: &Style, scale: f32, point: Pos2) -> Option<DestroyAnswer> {
    let (confirm, cancel) = buttons(rect, style, scale);
    if confirm.contains(point) {
        Some(DestroyAnswer::Confirm)
    } else if cancel.contains(point) {
        Some(DestroyAnswer::Cancel)
    } else {
        None
    }
}

/// Paints the prompt into `rect`.
pub fn draw(painter: &Painter, rect: Rect, view: &DestroyPromptView, style: &Style, scale: f32) {
    let corner = corner_radius(style.corner * scale);
    painter.rect_filled(rect, corner, style.background);
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(
            style.border_width.max(1.0) * scale,
            Color32::from(style.party_invite_border),
        ),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(style.padding * scale);
    let painter = painter.with_clip_rect(inner);
    let font = FontId::proportional(style.font_size * scale);

    painter.text(
        Pos2::new(inner.center().x, inner.top()),
        Align2::CENTER_TOP,
        format!("Destroy {}?", view.name),
        font.clone(),
        Color32::from(style.text),
    );

    let (confirm, cancel) = buttons(rect, style, scale);
    button(&painter, confirm, "Destroy", style.party_invite_decline, style, scale, &font);
    button(&painter, cancel, "Cancel", style.party_invite_accept, style, scale, &font);
}

/// One labelled button, outlined rather than filled -- see
/// [`super::party_invite::button`], which this copies exactly.
fn button(
    painter: &Painter,
    rect: Rect,
    label: &str,
    colour: crate::style::Color,
    style: &Style,
    scale: f32,
    font: &FontId,
) {
    let corner = corner_radius(style.corner * scale * 0.5);
    let colour: Color32 = colour.into();
    painter.rect_filled(rect, corner, style.bar_backdrop);
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(style.border_width.max(1.0) * scale, colour),
        StrokeKind::Inside,
    );
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font.clone(), colour);
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(style: &Style, scale: f32) -> Rect {
        Rect::from_min_size(Pos2::new(200.0, 120.0), size(style, scale))
    }

    fn view() -> DestroyPromptView {
        DestroyPromptView {
            name: "Refreshing Spring Water".into(),
            icon: None,
        }
    }

    /// The two answers are opposite, so a press has to land on one and not
    /// the other -- asserting only Confirm would pass a rule that answered
    /// Confirm for the whole frame.
    #[test]
    fn each_button_answers_only_for_itself() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            let rect = rect(&style, scale);
            let (confirm, cancel) = buttons(rect, &style, scale);
            assert_eq!(
                click_at(rect, &style, scale, confirm.center()),
                Some(DestroyAnswer::Confirm),
                "at scale {scale}"
            );
            assert_eq!(
                click_at(rect, &style, scale, cancel.center()),
                Some(DestroyAnswer::Cancel),
                "at scale {scale}"
            );
            assert!(
                !confirm.contains(cancel.center()) && !cancel.contains(confirm.center()),
                "the two buttons overlap at scale {scale}"
            );
        }
    }

    /// A press on the text destroys nothing -- an accidental confirm cannot
    /// be undone, so a miss must answer neither button.
    #[test]
    fn a_press_that_misses_both_answers_nothing() {
        let style = Style::default();
        let rect = rect(&style, 1.0);
        let text = Pos2::new(rect.center().x, rect.top() + style.padding + 2.0);
        assert_eq!(click_at(rect, &style, 1.0, text), None);
    }

    #[test]
    fn scale_multiplies_the_whole_prompt() {
        let style = Style::default();
        assert_eq!(size(&style, 2.0), size(&style, 1.0) * 2.0);
    }

    /// The item's own name has to reach the paint, or every prompt would
    /// read the same regardless of what is actually about to be destroyed.
    #[test]
    fn the_items_name_is_drawn() {
        fn painted(view: &DestroyPromptView) -> String {
            let ctx = egui::Context::default();
            let style = Style::default();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(500.0))),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                let rect = Rect::from_min_size(Pos2::ZERO, size(&style, 1.0));
                draw(&painter, rect, view, &style, 1.0);
            });
            let rendered = format!("{:?}", output.shapes);
            output.drop_without_applying_deltas();
            rendered
        }

        let one = painted(&view());
        assert!(one.len() > 100, "the prompt painted nothing to compare");
        let mut other = view();
        other.name = "Worn Shortsword".into();
        assert_ne!(one, painted(&other), "the name never reached the paint");
    }
}
