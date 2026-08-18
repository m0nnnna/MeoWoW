//! "Testwolf invites you to a group." -- and the two buttons that answer it.
//!
//! The only frame in the interface with **two** things to press, which is why
//! it is not modelled on [`super::release`] despite being the same size and
//! shape. A release prompt can treat its whole rectangle as one click target
//! because there is exactly one thing to do with it; here the two answers are
//! opposite, and a mis-tested pixel accepts an invite the player declined.
//!
//! The geometry is therefore stated once, in [`buttons`], and both `draw` and
//! [`click_at`] read it. Two separately written copies of the same rectangles
//! agree until one of them changes -- the same rule that makes the picking ray
//! unproject the matrix the scene is drawn with rather than rebuild it from
//! the camera's angles.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// The pending invite, as a plain snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct PartyInviteView {
    /// Who is asking. **The only handle the packet carries** -- there is no
    /// guid in `SMSG_GROUP_INVITE`, because an invite is the thing you send
    /// to someone you have only read off a chat line.
    pub from: String,
}

impl PartyInviteView {
    /// A stand-in so the prompt can be positioned without another player
    /// logging in and inviting you -- the hardest precondition of any frame
    /// here to arrange on demand.
    pub fn placeholder() -> Self {
        Self {
            from: "Testwolf".into(),
        }
    }
}

/// Which button was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteAnswer {
    Accept,
    Decline,
}

/// How much room the prompt wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    let height = style.padding * 3.0 + style.font_size * 1.3 + style.party_invite_button_height;
    Vec2::new(style.party_invite_width, height) * scale
}

/// Where the two buttons are, as the single statement both drawing and hit
/// testing read.
///
/// Returns `(accept, decline)`, side by side along the bottom with the gap
/// between them. Accept is on the left because it is the answer the prompt
/// exists for; a player who wants neither can also simply ignore it, which no
/// pixel here can get wrong.
fn buttons(rect: Rect, style: &Style, scale: f32) -> (Rect, Rect) {
    let padding = style.padding * scale;
    let gap = style.gap * scale;
    let height = style.party_invite_button_height * scale;
    let width = ((rect.width() - padding * 2.0 - gap) * 0.5).max(1.0);
    let top = rect.bottom() - padding - height;
    let accept = Rect::from_min_size(
        Pos2::new(rect.left() + padding, top),
        Vec2::new(width, height),
    );
    let decline = Rect::from_min_size(
        Pos2::new(accept.right() + gap, top),
        Vec2::new(width, height),
    );
    (accept, decline)
}

/// Which answer a click at `point` is, or `None` for a press on the frame that
/// missed both buttons.
///
/// **`None` rather than a nearest-button guess.** The two answers are opposite
/// and irreversible-ish -- an accidental accept puts the character in a
/// stranger's group and has to be undone by leaving it -- so a press on the
/// text between them does nothing at all.
pub fn click_at(
    rect: Rect,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<InviteAnswer> {
    let (accept, decline) = buttons(rect, style, scale);
    if accept.contains(point) {
        Some(InviteAnswer::Accept)
    } else if decline.contains(point) {
        Some(InviteAnswer::Decline)
    } else {
        None
    }
}

/// Paints the prompt into `rect`.
pub fn draw(painter: &Painter, rect: Rect, view: &PartyInviteView, style: &Style, scale: f32) {
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
        format!("{} invites you to a group.", view.from),
        font.clone(),
        Color32::from(style.text),
    );

    let (accept, decline) = buttons(rect, style, scale);
    button(&painter, accept, "Accept", style.party_invite_accept, style, scale, &font);
    button(
        &painter,
        decline,
        "Decline",
        style.party_invite_decline,
        style,
        scale,
        &font,
    );
}

/// One labelled button, outlined in its own colour rather than filled with it:
/// the frame sits over the world, and two solid blocks of colour would read as
/// part of the scene behind them at low opacity.
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

    /// The whole reason this frame is not a release prompt: the two answers
    /// are opposite, so a press has to land on one of them and not the other.
    /// Asserting only that Accept works would pass under a rule that returned
    /// Accept for every pixel in the frame.
    #[test]
    fn each_button_answers_only_for_itself() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            let rect = rect(&style, scale);
            let (accept, decline) = buttons(rect, &style, scale);
            assert_eq!(
                click_at(rect, &style, scale, accept.center()),
                Some(InviteAnswer::Accept),
                "at scale {scale}"
            );
            assert_eq!(
                click_at(rect, &style, scale, decline.center()),
                Some(InviteAnswer::Decline),
                "at scale {scale}"
            );
            assert!(
                !accept.contains(decline.center()) && !decline.contains(accept.center()),
                "the two buttons overlap at scale {scale}"
            );
        }
    }

    /// A press on the text does nothing, rather than picking whichever button
    /// is nearer. An accidental accept has to be undone by leaving the group;
    /// an ignored press costs nothing.
    #[test]
    fn a_press_that_misses_both_answers_nothing() {
        let style = Style::default();
        let rect = rect(&style, 1.0);
        let text = Pos2::new(rect.center().x, rect.top() + style.padding + 2.0);
        assert_eq!(click_at(rect, &style, 1.0, text), None);
        assert_eq!(
            click_at(rect, &style, 1.0, Pos2::new(rect.left() - 20.0, rect.center().y)),
            None,
            "a point outside the frame answered"
        );
    }

    /// Both buttons stay inside the frame at every scale -- a button drawn
    /// past the edge is clipped away and becomes unreachable, which is the
    /// same failure as being left out of the `Sense::click()` list.
    #[test]
    fn the_buttons_stay_inside_the_frame() {
        let style = Style::default();
        for scale in [0.25, 1.0, 4.0] {
            let rect = rect(&style, scale);
            let (accept, decline) = buttons(rect, &style, scale);
            assert!(rect.contains_rect(accept), "accept escaped at scale {scale}");
            assert!(rect.contains_rect(decline), "decline escaped at scale {scale}");
        }
    }

    #[test]
    fn scale_multiplies_the_whole_prompt() {
        let style = Style::default();
        assert_eq!(size(&style, 2.0), size(&style, 1.0) * 2.0);
    }

    /// It has to paint, and the inviter's name has to reach the paint -- a
    /// prompt that drew the same shapes whoever was asking would be a prompt
    /// with the name hard-coded out of the drawing path.
    #[test]
    fn the_inviters_name_is_drawn() {
        fn painted(from: &str) -> String {
            let ctx = egui::Context::default();
            let style = Style::default();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(500.0))),
                ..Default::default()
            };
            let view = PartyInviteView { from: from.into() };
            let output = ctx.run_ui(input, |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                let rect = Rect::from_min_size(Pos2::ZERO, size(&style, 1.0));
                draw(&painter, rect, &view, &style, 1.0);
            });
            let rendered = format!("{:?}", output.shapes);
            output.drop_without_applying_deltas();
            rendered
        }

        let one = painted("Testwolf");
        assert!(one.len() > 100, "the prompt painted nothing to compare");
        assert_ne!(one, painted("Huntertest"), "the name never reached the paint");
    }
}
