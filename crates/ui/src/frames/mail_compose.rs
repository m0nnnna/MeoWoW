//! The other half of the mailbox: writing a letter rather than reading one.
//!
//! A separate frame from [`super::mail`] on purpose. The inbox is a painted
//! list with a hit test that has to stay honest about which rows can be
//! clicked; a compose form is four editable fields and two buttons, and
//! folding the two together would tangle each one's geometry with the
//! other's. They open side by side instead -- see [`crate::layout`]'s
//! `MailCompose` default, which sits clear of the inbox and clear of the
//! bags.
//!
//! ## What it carries, and what it does not
//!
//! A recipient name, a subject, a body and an amount of copper to enclose.
//! **No item attachments.** Putting an item in a letter means a drag target
//! that reads the bag window, and the bag window is a list of row positions
//! this crate cannot resolve to a `(bag, slot)` -- the same wall
//! [`super::bags`] describes. An honest four-field form beats a fifth field
//! that half-works, the call [`super::mail`] already makes about its own
//! missing sell window and the auction window makes about its missing sort.
//!
//! ## Text editing lives in the caller
//!
//! Like the chat line and the sign-in screen, this frame draws a string and
//! a caret and reports *which field a click wants focus in*; the caller owns
//! the `String`s and feeds key events into whichever one [`MailComposeView::focus`]
//! names. This crate depends on neither `world` nor `render` and adds no
//! widget system to do it.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Which field of the form the caret is in.
///
/// Mirrors nothing in the caller -- the caller keeps its own copy and this is
/// the value it hands back for drawing, exactly as `SignIn` does with its own
/// focus enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MailComposeField {
    /// Who the letter goes to, by character name. The server takes a name
    /// string; there is no guid to resolve.
    #[default]
    Recipient,
    Subject,
    Body,
    /// Copper to enclose, typed as digits. Parsed by the caller, because
    /// "how many coins is this" is a question about the player's purse and
    /// this crate cannot see it.
    Money,
}

impl MailComposeField {
    /// The four fields, top to bottom, which is also Tab order.
    pub const ORDER: [MailComposeField; 4] = [
        MailComposeField::Recipient,
        MailComposeField::Subject,
        MailComposeField::Money,
        MailComposeField::Body,
    ];

    /// The next field for a Tab press, wrapping.
    pub fn next(self) -> MailComposeField {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }

    /// The previous field for a Shift+Tab press, wrapping.
    pub fn prev(self) -> MailComposeField {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(i + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }
}

/// Everything the compose form draws.
///
/// Owns its strings, rebuilt each frame by the caller from its edit buffers
/// -- the same shape as [`super::MailView`], and cheap for four short
/// strings.
#[derive(Debug, Clone, PartialEq)]
pub struct MailComposeView {
    pub recipient: String,
    pub subject: String,
    pub body: String,
    /// The money field's raw text, digits as typed. Not a number: an empty
    /// field and a zero are different states to a person mid-edit, and the
    /// parse belongs where the purse is.
    pub money: String,
    /// Which field the caret is in.
    pub focus: MailComposeField,
    /// A line under the form: what the last send attempt did, or why Send is
    /// greyed. `None` leaves the row blank but still reserved, the way the
    /// inbox reserves its gesture line.
    pub status: Option<String>,
    /// Whether [`Self::status`] is a complaint, so it takes the error colour
    /// -- the sign-in screen's rule for its own status line.
    pub status_bad: bool,
    /// Whether the Send button is live. False with no recipient, or while a
    /// send is in flight; the caller decides and the frame only draws it.
    pub can_send: bool,
}

impl Default for MailComposeView {
    fn default() -> Self {
        Self {
            recipient: String::new(),
            subject: String::new(),
            body: String::new(),
            money: String::new(),
            focus: MailComposeField::Recipient,
            status: None,
            status_bad: false,
            can_send: false,
        }
    }
}

/// A form with plausible contents, for the layout editor.
pub fn placeholder() -> MailComposeView {
    MailComposeView {
        recipient: "Testwolf".into(),
        subject: "Supplies".into(),
        body: "Take what you need from the bank.".into(),
        money: "1250".into(),
        focus: MailComposeField::Body,
        status: Some("Ready to send.".into()),
        status_bad: false,
        can_send: true,
    }
}

/// What a click on the form asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailComposeClick {
    /// Put the caret in this field.
    Focus(MailComposeField),
    /// Post the letter.
    Send,
    /// Shut the form without sending.
    Close,
}

fn title_height(style: &Style) -> f32 {
    style.font_size + style.gap
}

fn label_height(style: &Style) -> f32 {
    style.font_size * 0.85 + style.gap * 0.5
}

fn field_height(style: &Style) -> f32 {
    style.login_row.max(style.font_size * 1.6)
}

/// The body box is a few lines tall; every other field is one. Kept modest
/// so the whole form fits its default pocket in the layout without
/// overlapping a neighbour -- a person who wants a taller message box drags
/// the frame bigger, the same as any other.
fn body_height(style: &Style) -> f32 {
    field_height(style) * 2.0
}

fn status_height(style: &Style) -> f32 {
    style.font_size * 0.85 + style.gap
}

fn button_height(style: &Style) -> f32 {
    style.party_invite_button_height
}

/// One row is a small label over a field box.
fn row_height(style: &Style, field: MailComposeField) -> f32 {
    label_height(style)
        + if field == MailComposeField::Body {
            body_height(style)
        } else {
            field_height(style)
        }
        + style.gap
}

/// How much room the form wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    let rows: f32 = MailComposeField::ORDER
        .iter()
        .map(|f| row_height(style, *f))
        .sum();
    let height = style.padding * 2.0
        + title_height(style)
        + rows
        + status_height(style)
        + button_height(style)
        + style.gap;
    Vec2::new(style.login_width.max(style.loot_width * 1.85), height) * scale
}

/// Every field's box, in [`MailComposeField::ORDER`]. The single source of
/// row geometry, read by the drawing and the hit test both -- the rule every
/// frame in this crate keeps after the trainer window made it load-bearing.
pub fn field_rects(rect: Rect, style: &Style, scale: f32) -> [Rect; 4] {
    let pad = style.padding * scale;
    let gap = style.gap * scale;
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(1.0);
    let mut y = rect.min.y + pad + title_height(style) * scale;
    let mut out = [Rect::NOTHING; 4];
    for (slot, field) in MailComposeField::ORDER.iter().enumerate() {
        let top = y + label_height(style) * scale;
        let h = if *field == MailComposeField::Body {
            body_height(style) * scale
        } else {
            field_height(style) * scale
        };
        out[slot] = Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, h));
        y = top + h + gap;
    }
    out
}

/// The box for one named field.
pub fn field_rect(rect: Rect, style: &Style, scale: f32, field: MailComposeField) -> Rect {
    let slot = MailComposeField::ORDER
        .iter()
        .position(|f| *f == field)
        .unwrap_or(0);
    field_rects(rect, style, scale)[slot]
}

/// The Send and Close buttons, along the bottom edge -- laid out exactly like
/// [`super::destroy_prompt::buttons`], because the answers are opposite and a
/// press between them must answer neither.
pub fn buttons(rect: Rect, style: &Style, scale: f32) -> (Rect, Rect) {
    let pad = style.padding * scale;
    let gap = style.gap * scale;
    let h = button_height(style) * scale;
    let w = ((rect.width() - pad * 2.0 - gap) * 0.5).max(1.0);
    let top = rect.bottom() - pad - h;
    let send = Rect::from_min_size(Pos2::new(rect.left() + pad, top), Vec2::new(w, h));
    let close = Rect::from_min_size(Pos2::new(send.right() + gap, top), Vec2::new(w, h));
    (send, close)
}

/// What a click at `point` wants, or `None` for a press that landed on the
/// form's chrome. A miss is not routed to the nearest field: the same call
/// [`super::destroy_prompt::click_at`] makes, because Send here is not
/// reversible from inside the form.
pub fn click_at(
    rect: Rect,
    view: &MailComposeView,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<MailComposeClick> {
    let (send, close) = buttons(rect, style, scale);
    if close.contains(point) {
        return Some(MailComposeClick::Close);
    }
    if send.contains(point) && view.can_send {
        return Some(MailComposeClick::Send);
    }
    let rects = field_rects(rect, style, scale);
    for (slot, field) in MailComposeField::ORDER.iter().enumerate() {
        if rects[slot].contains(point) {
            return Some(MailComposeClick::Focus(*field));
        }
    }
    None
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Paints the form.
pub fn draw(painter: &Painter, rect: Rect, view: &MailComposeView, style: &Style, scale: f32) {
    let corner = corner_radius(style.corner * scale);
    painter.rect_filled(rect, corner, style.spellbook_background);
    if style.border_width > 0.0 {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(style.border_width * scale, style.border),
            StrokeKind::Inside,
        );
    }

    let text: Color32 = style.text.into();
    let dim: Color32 = style.quest_dim.into();
    let accent: Color32 = style.login_accent.into();
    let pad = style.padding * scale;
    let font = FontId::proportional(style.font_size * scale);
    let small = FontId::proportional(style.font_size * 0.85 * scale);

    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        "Send Mail",
        font.clone(),
        text,
    );

    let clip = painter.with_clip_rect(rect);
    let rects = field_rects(rect, style, scale);
    for (slot, field) in MailComposeField::ORDER.iter().enumerate() {
        let box_rect = rects[slot];
        let (label, hint, value): (&str, &str, &str) = match field {
            MailComposeField::Recipient => ("To", "character name", &view.recipient),
            MailComposeField::Subject => ("Subject", "", &view.subject),
            MailComposeField::Money => ("Money (copper)", "0", &view.money),
            MailComposeField::Body => ("Message", "", &view.body),
        };
        clip.text(
            Pos2::new(box_rect.left(), box_rect.top() - style.gap * 0.5 * scale),
            Align2::LEFT_BOTTOM,
            label,
            small.clone(),
            dim,
        );

        let focused = view.focus == *field;
        clip.rect_filled(box_rect, corner_radius(style.corner * scale), style.login_field);
        clip.rect_stroke(
            box_rect,
            corner_radius(style.corner * scale),
            Stroke::new(
                if focused {
                    2.0 * scale
                } else {
                    style.border_width.max(1.0) * scale
                },
                if focused { accent } else { Color32::from(style.border) },
            ),
            StrokeKind::Inside,
        );

        let inner = box_rect.shrink2(Vec2::new(style.padding * scale, style.gap * 0.5 * scale));
        let inner_clip = clip.with_clip_rect(inner);
        let top_left = Pos2::new(inner.left(), inner.top());
        if value.is_empty() && !hint.is_empty() {
            inner_clip.text(top_left, Align2::LEFT_TOP, hint, small.clone(), dim);
        }

        // The body wraps; the one-liners scroll their tail into view the way
        // the sign-in path field does, so what is being typed stays visible.
        let text_rect = if *field == MailComposeField::Body {
            inner_clip.text(top_left, Align2::LEFT_TOP, value, font.clone(), text)
        } else {
            let width = inner_clip
                .layout_no_wrap(value.to_string(), font.clone(), text)
                .size()
                .x;
            if width > inner.width() {
                inner_clip.text(
                    inner.right_center(),
                    Align2::RIGHT_CENTER,
                    value,
                    font.clone(),
                    text,
                )
            } else {
                inner_clip.text(
                    inner.left_center(),
                    Align2::LEFT_CENTER,
                    value,
                    font.clone(),
                    text,
                )
            }
        };
        if focused {
            let (x, y0, y1) = if *field == MailComposeField::Body {
                (
                    text_rect.right().min(inner.right()).max(inner.left()) + 1.0,
                    (text_rect.bottom().min(inner.bottom()) - font.size).max(inner.top()),
                    text_rect.bottom().min(inner.bottom()),
                )
            } else {
                (
                    text_rect.right().min(inner.right()) + 1.0,
                    inner.top() + 2.0 * scale,
                    inner.bottom() - 2.0 * scale,
                )
            };
            inner_clip.line_segment(
                [Pos2::new(x, y0), Pos2::new(x, y1)],
                Stroke::new(1.5 * scale, accent),
            );
        }
    }

    // The status line, always reserved. Same reasoning as the inbox's
    // gesture line: a row that appears only when there is something to say
    // moves the buttons under the cursor at the moment somebody clicks one.
    let (send, close) = buttons(rect, style, scale);
    if let Some(status) = &view.status {
        clip.text(
            Pos2::new(rect.left() + pad, send.top() - style.gap * scale),
            Align2::LEFT_BOTTOM,
            status,
            small.clone(),
            if view.status_bad {
                Color32::from(style.login_error)
            } else {
                dim
            },
        );
    }

    button(
        &clip,
        send,
        "Send",
        view.can_send,
        style.party_invite_accept,
        style,
        scale,
        &font,
    );
    button(
        &clip,
        close,
        "Close",
        true,
        style.party_invite_decline,
        style,
        scale,
        &font,
    );
}

/// One outlined button, copied from [`super::destroy_prompt::button`] with an
/// enabled state added -- a greyed Send has to read as unavailable rather
/// than as a different colour of live.
#[allow(clippy::too_many_arguments)]
fn button(
    painter: &Painter,
    rect: Rect,
    label: &str,
    enabled: bool,
    colour: crate::style::Color,
    style: &Style,
    scale: f32,
    font: &FontId,
) {
    let corner = corner_radius(style.corner * scale * 0.5);
    let colour: Color32 = if enabled {
        colour.into()
    } else {
        Color32::from(style.quest_dim)
    };
    painter.rect_filled(rect, corner, style.bar_backdrop);
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(style.border_width.max(1.0) * scale, colour),
        StrokeKind::Inside,
    );
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font.clone(), colour);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(style: &Style, scale: f32) -> Rect {
        Rect::from_min_size(Pos2::new(60.0, 40.0), size(style, scale))
    }

    /// Every field box sits inside the form and none overlaps the next --
    /// the drawing and the hit test share `field_rects`, so this is the whole
    /// of their contract.
    #[test]
    fn field_boxes_are_disjoint_and_inside() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            let r = rect(&style, scale);
            let rects = field_rects(r, &style, scale);
            for b in &rects {
                assert!(r.contains_rect(*b), "{b:?} outside {r:?} at {scale}");
            }
            for pair in rects.windows(2) {
                assert!(
                    pair[0].max.y <= pair[1].min.y + 0.01,
                    "field boxes overlap at scale {scale}"
                );
            }
        }
    }

    /// A click in a field's box asks for that field and no other.
    #[test]
    fn a_field_click_focuses_only_that_field() {
        let style = Style::default();
        let view = placeholder();
        let r = rect(&style, 1.0);
        for (slot, field) in MailComposeField::ORDER.iter().enumerate() {
            let point = field_rects(r, &style, 1.0)[slot].center();
            assert_eq!(
                click_at(r, &view, &style, 1.0, point),
                Some(MailComposeClick::Focus(*field)),
            );
        }
    }

    /// The two buttons answer opposite things, so a press has to land on one
    /// and not the other -- asserting only Close would pass a rule that
    /// answered Close for the whole bottom band.
    #[test]
    fn send_and_close_answer_only_themselves() {
        let style = Style::default();
        let view = placeholder();
        let r = rect(&style, 1.0);
        let (send, close) = buttons(r, &style, 1.0);
        assert_eq!(
            click_at(r, &view, &style, 1.0, send.center()),
            Some(MailComposeClick::Send)
        );
        assert_eq!(
            click_at(r, &view, &style, 1.0, close.center()),
            Some(MailComposeClick::Close)
        );
        assert!(!send.intersects(close), "the buttons overlap");
    }

    /// Send is inert while the form says it cannot send -- the frame must not
    /// report a click the caller would have to know to ignore, the trainer
    /// window's rule about inert rows.
    #[test]
    fn a_greyed_send_reports_nothing() {
        let style = Style::default();
        let mut view = placeholder();
        view.can_send = false;
        let r = rect(&style, 1.0);
        let (send, _) = buttons(r, &style, 1.0);
        assert_eq!(click_at(r, &view, &style, 1.0, send.center()), None);
    }

    /// A press on the title or the gap between fields asks for nothing --
    /// Send cannot be taken back from inside the form, so a miss must not be
    /// nudged onto a button.
    #[test]
    fn a_press_on_the_chrome_answers_nothing() {
        let style = Style::default();
        let view = placeholder();
        let r = rect(&style, 1.0);
        let title = Pos2::new(r.center().x, r.top() + 2.0);
        assert_eq!(click_at(r, &view, &style, 1.0, title), None);
    }

    #[test]
    fn tab_order_wraps_both_ways() {
        use MailComposeField::*;
        assert_eq!(Recipient.next(), Subject);
        assert_eq!(Subject.next(), Money);
        assert_eq!(Money.next(), Body);
        assert_eq!(Body.next(), Recipient);
        assert_eq!(Recipient.prev(), Body);
    }

    #[test]
    fn scale_multiplies_the_whole_form() {
        let style = Style::default();
        assert_eq!(size(&style, 2.0), size(&style, 1.0) * 2.0);
    }

    /// The focused field reaches the paint -- otherwise the caret is drawn in
    /// the same place no matter which field a person is typing in.
    #[test]
    fn the_focused_field_changes_what_is_painted() {
        fn painted(view: &MailComposeView) -> String {
            let ctx = egui::Context::default();
            let style = Style::default();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(900.0))),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                let r = Rect::from_min_size(Pos2::ZERO, size(&style, 1.0));
                draw(&painter, r, view, &style, 1.0);
            });
            let rendered = format!("{:?}", output.shapes);
            output.drop_without_applying_deltas();
            rendered
        }
        let mut a = placeholder();
        a.focus = MailComposeField::Recipient;
        let mut b = placeholder();
        b.focus = MailComposeField::Subject;
        assert_ne!(painted(&a), painted(&b), "the caret ignored the focused field");
    }
}
