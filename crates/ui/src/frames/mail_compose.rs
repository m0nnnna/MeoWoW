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
//! ## What it carries
//!
//! A recipient name, a subject, a body, an amount of copper to enclose, and
//! up to [`MAX_ATTACHMENTS`] items. **Attaching is a modal right-click in the
//! bag window, not a drag** -- the same gesture [`super::trade`] already
//! established for putting an item on a table this crate does not own, and
//! for the same reason: a drag target that reads the bag window needs a
//! `(bag, slot)` this crate cannot resolve, but a click the caller already
//! resolves for its own trade squares resolves identically here. The hint
//! line is drawn only while there are none, exactly as trade's is.
//!
//! ## Text editing lives in the caller
//!
//! Like the chat line and the sign-in screen, this frame draws a string and
//! a caret and reports *which field a click wants focus in*; the caller owns
//! the `String`s and feeds key events into whichever one [`MailComposeView::focus`]
//! names. This crate depends on neither `world` nor `render` and adds no
//! widget system to do it.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use super::mail::MailAttachment;
use crate::style::Style;

/// The server's own limit (AzerothCore's `MAX_MAIL_ITEMS`), refused with
/// `MAIL_ERR_TOO_MANY_ATTACHMENTS` past it. Drawn as a fixed row the same way
/// [`super::trade`]'s seven squares are -- every square shown, empty or not,
/// so the row reads as remaining capacity rather than as a list that might
/// still be growing.
pub const MAX_ATTACHMENTS: usize = 12;

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
    /// Items put on the letter so far, in attach order. Never more than
    /// [`MAX_ATTACHMENTS`] -- the caller enforces the cap before this is
    /// built, the same place [`super::trade::TradeView`] enforces its own
    /// seven-square limit.
    pub attachments: Vec<MailAttachment>,
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
            attachments: Vec::new(),
        }
    }
}

/// A form with plausible contents, for the layout editor.
///
/// One attachment with an icon and one without, so the editor shows both the
/// ordinary square and the name-fallback one -- the same reasoning
/// [`super::trade::placeholder`] gives for mismatched offers.
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
        attachments: vec![
            MailAttachment {
                count: 5,
                icon: None,
                name: "Darnassian Bleu".into(),
            },
            MailAttachment {
                count: 1,
                icon: None,
                name: "Worn Shortsword".into(),
            },
        ],
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
    /// Take an attachment back off the letter. Reversible with no confirm,
    /// unlike deleting a received letter -- nothing has left this character's
    /// bags yet, the server only sees the list at Send.
    RemoveAttachment(usize),
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

/// One attachment square's side, fitted so all [`MAX_ATTACHMENTS`] of them
/// make exactly one row across a box `width` wide -- smaller than
/// `style.slot_size` by construction, the same call [`super::mail`]'s own
/// inbox row already makes for its attachment squares (`row_height(style) *
/// 0.5` there) rather than use the bag grid's full-size square. A fixed
/// column count here would either overrun the form's width or, split into
/// more rows, cost back the vertical room a compose form does not have
/// beside the trainer/taxi/vendor column it defaults into -- see
/// `crate::layout`'s `MailCompose` entry.
///
/// Takes its cap explicitly rather than reading `style.slot_size` itself so
/// one function serves both an unscaled caller ([`attachment_band_height`],
/// which needs a height `size` can add before any scale is known) and a
/// scaled one ([`attachment_rects`]) without either multiplying the other's
/// answer by `scale` a second time.
fn attachment_slot_side(width: f32, pad: f32, gap: f32, cap: f32) -> f32 {
    let usable = (width - pad * 2.0 - gap * (MAX_ATTACHMENTS as f32 - 1.0)).max(1.0);
    (usable / MAX_ATTACHMENTS as f32).min(cap)
}

/// Unscaled height of the attachment band: one label line -- doing double
/// duty as the "how to attach" hint while the row is empty, so there is no
/// second line to reserve -- the row of squares, and the gap before whatever
/// follows.
fn attachment_band_height(style: &Style) -> f32 {
    let width = style.login_width.max(style.loot_width * 1.85);
    let side = attachment_slot_side(width, style.padding, style.slot_gap, style.slot_size);
    label_height(style) + side + style.gap
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
        + attachment_band_height(style)
        + status_height(style)
        + button_height(style)
        + style.gap;
    Vec2::new(style.login_width.max(style.loot_width * 1.85), height) * scale
}

/// Every field's box, in [`MailComposeField::ORDER`]. The single source of
/// row geometry, read by the drawing and the hit test both -- the rule every
/// frame in this crate keeps after the trainer window made it load-bearing.
///
/// **The attachment band sits between Money and Body**, so its height is
/// folded into `y` right after Money's row -- the one place in the loop that
/// knows where that gap belongs, rather than a second function that would
/// have to agree with this one about it.
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
        if *field == MailComposeField::Money {
            y += attachment_band_height(style) * scale;
        }
    }
    out
}

/// Where every attachment square sits, [`MAX_ATTACHMENTS`] of them whether or
/// not they hold anything -- the same reasoning [`super::trade::square_rects`]
/// gives for always drawing all seven of its own. Positioned off the Money
/// field's bottom edge, which is the single fact [`field_rects`] already
/// establishes about where this band starts.
pub fn attachment_rects(rect: Rect, style: &Style, scale: f32) -> [Rect; MAX_ATTACHMENTS] {
    let pad = style.padding * scale;
    let slot_gap = style.slot_gap * scale;
    let side = attachment_slot_side(rect.width(), pad, slot_gap, style.slot_size * scale);
    let left = rect.min.x + pad;
    let top = field_rect(rect, style, scale, MailComposeField::Money).bottom()
        + style.gap * scale
        + label_height(style) * scale;
    let mut out = [Rect::NOTHING; MAX_ATTACHMENTS];
    for (index, slot) in out.iter_mut().enumerate() {
        let at = Pos2::new(left + index as f32 * (side + slot_gap), top);
        *slot = Rect::from_min_size(at, Vec2::splat(side));
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
    // Only an occupied square answers -- an empty one is capacity, not a
    // button, the same rule `trade::click_at` applies to its own squares.
    attachment_rects(rect, style, scale)
        .iter()
        .position(|square| square.contains(point))
        .filter(|&index| index < view.attachments.len())
        .map(MailComposeClick::RemoveAttachment)
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

    // The attachment grid: one label line, then MAX_ATTACHMENTS squares,
    // filled or not -- the same "always draw the capacity" rule `bags::draw`
    // and `trade::draw` both follow. **The label line doubles as the "how to
    // attach" hint while the row is empty** rather than reserving a second
    // line for it: a modal gesture with nothing on screen naming it is not a
    // gesture (the call `trade::draw` makes about its own hint), but a line
    // that appeared only once the grid was empty would move the Message
    // field the moment an attachment lands or leaves -- the same trap
    // `trade`'s footer is written up as avoiding.
    if let Some(first) = attachment_rects(rect, style, scale).first() {
        let label = if view.attachments.is_empty() {
            "Right-click a bag item to attach it".to_string()
        } else {
            format!("Attachments ({}/{MAX_ATTACHMENTS})", view.attachments.len())
        };
        clip.text(
            Pos2::new(first.left(), first.top() - style.gap * 0.5 * scale),
            Align2::LEFT_BOTTOM,
            label,
            small.clone(),
            dim,
        );
    }
    let slot_corner = corner_radius(style.corner * scale * 0.5);
    for (index, bounds) in attachment_rects(rect, style, scale).into_iter().enumerate() {
        clip.rect_filled(bounds, slot_corner, style.slot_background);
        match view.attachments.get(index) {
            Some(attachment) => {
                match attachment.icon {
                    Some(icon) => {
                        clip.image(
                            icon,
                            bounds.shrink(style.border_width * scale),
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    // No icon: the name's first letters rather than nothing,
                    // so an unresolved icon and an actually-empty square
                    // never look alike -- the same call `trade::draw_square`
                    // makes about its own unnamed squares.
                    None => {
                        let squared = clip.with_clip_rect(bounds);
                        squared.text(
                            bounds.center(),
                            Align2::CENTER_CENTER,
                            attachment.name.chars().take(3).collect::<String>(),
                            FontId::proportional(style.font_size * 0.8 * scale),
                            text,
                        );
                    }
                }
                clip.rect_stroke(
                    bounds,
                    slot_corner,
                    Stroke::new(style.border_width * scale, style.border),
                    StrokeKind::Inside,
                );
                if attachment.count > 1 {
                    clip.text(
                        bounds.max - Vec2::splat(style.border_width * scale * 2.0),
                        Align2::RIGHT_BOTTOM,
                        attachment.count.to_string(),
                        FontId::proportional(style.font_size * 0.8 * scale),
                        text,
                    );
                }
            }
            None => {
                clip.rect_stroke(
                    bounds,
                    slot_corner,
                    Stroke::new(style.border_width * scale, style.slot_empty_border),
                    StrokeKind::Inside,
                );
            }
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

    /// Every attachment square sits inside the form and none overlaps the
    /// next -- the drawing and the hit test share `attachment_rects`, same
    /// contract `field_boxes_are_disjoint_and_inside` holds the four text
    /// fields to.
    #[test]
    fn attachment_squares_are_disjoint_and_inside() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            let r = rect(&style, scale);
            let rects = attachment_rects(r, &style, scale);
            for b in &rects {
                assert!(r.contains_rect(*b), "{b:?} outside {r:?} at {scale}");
            }
            for (i, a) in rects.iter().enumerate() {
                for (j, b) in rects.iter().enumerate() {
                    if i != j {
                        assert!(!a.intersects(*b), "squares {i} and {j} overlap at {scale}");
                    }
                }
            }
        }
    }

    /// A click on a filled attachment square asks to remove that one and no
    /// other.
    #[test]
    fn a_filled_attachment_click_removes_only_that_one() {
        let style = Style::default();
        let view = placeholder();
        let r = rect(&style, 1.0);
        let rects = attachment_rects(r, &style, 1.0);
        for index in 0..view.attachments.len() {
            assert_eq!(
                click_at(r, &view, &style, 1.0, rects[index].center()),
                Some(MailComposeClick::RemoveAttachment(index)),
            );
        }
    }

    /// An empty attachment square answers nothing -- it is remaining
    /// capacity, not a button, the rule `trade`'s own squares follow.
    #[test]
    fn an_empty_attachment_square_answers_nothing() {
        let style = Style::default();
        let view = placeholder();
        let r = rect(&style, 1.0);
        let rects = attachment_rects(r, &style, 1.0);
        for index in view.attachments.len()..MAX_ATTACHMENTS {
            assert_eq!(click_at(r, &view, &style, 1.0, rects[index].center()), None);
        }
    }

    /// A full grid of twelve draws without panicking and paints something
    /// different from an empty one -- the smoke test for the loop that walks
    /// `view.attachments` against a fixed-size grid, where an off-by-one
    /// against [`MAX_ATTACHMENTS`] would panic on the thirteenth, not draw
    /// wrongly.
    #[test]
    fn a_full_grid_of_attachments_draws_and_differs_from_empty() {
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
        let mut full = placeholder();
        full.attachments = (0..MAX_ATTACHMENTS)
            .map(|i| MailAttachment {
                count: 1,
                icon: None,
                name: format!("Item {i}"),
            })
            .collect();
        let mut empty = placeholder();
        empty.attachments.clear();
        assert_ne!(
            painted(&full),
            painted(&empty),
            "a full grid painted the same as an empty one"
        );
    }
}
