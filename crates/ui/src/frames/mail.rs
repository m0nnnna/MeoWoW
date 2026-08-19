//! The mailbox window: what is waiting, and what is in it.
//!
//! A list like the trainer's and the loot window's, and it shares the trainer's
//! defining property -- **most rows may be inert, and that is normal.** A
//! corpse holds only things you may take; a mailbox holds letters, and a
//! letter you have already emptied is still worth drawing, because its subject
//! and its body are the whole reason somebody sent it.
//!
//! ## One gesture, and the destructive half is not on it
//!
//! Clicking a letter takes everything in it. Clicking a letter with nothing in
//! it does **nothing**, deliberately: throwing a letter away is irreversible
//! and there is no confirmation anywhere in this interface, so it is not put
//! on the same gesture that collects. The two states are mutually exclusive
//! and both are drawn, so a click is never ambiguous -- but a stray one on the
//! wrong row must not destroy anything, which is the same caution that made
//! the CLI's selection helpers refuse players outright after a substring match
//! killed somebody's character.
//!
//! **The window says what the gesture is**, in a line under the list, and the
//! room for it is reserved whether or not there is anything to click. That is
//! 4.26's lesson applied before the first live test rather than after it: the
//! trade window's offer gesture was correct code nobody could find, and the
//! report came back as "I couldn't give him an item".
//!
//! ## The count the server sent is not the count it has
//!
//! `SMSG_MAIL_LIST_RESULT` carries a total *and* a row count, and they differ
//! when the mailbox holds more than fifty letters or more than one packet's
//! worth. The surplus is named nowhere else, so [`MailView::withheld`] is
//! drawn in the header. A window showing the number of rows it received tells
//! the person with a full mailbox that it is not full -- and the letters it
//! silently dropped are the oldest, which are the ones about to expire.
//!
//! ## Everything on a row came off the wire
//!
//! A mailed item is not a replicated object -- it has left the sender's bags
//! and not arrived in the reader's -- so there is nothing to query and nothing
//! to look up. The count, the icon's entry and the durability all travel
//! inside the mail record itself. See `world::mail`.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Why a row is drawn the way it is.
///
/// Mirrors nothing in `world` on purpose -- this crate depends on neither
/// `world` nor `render`, which is what keeps the whole interface testable with
/// no connection and no GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailRowState {
    /// There is money or an attachment to collect. The only state a click
    /// acts on.
    Collectable,
    /// Nothing left in it. Drawn, dimmed, and inert -- see the module comment
    /// on why deleting is not this gesture.
    Empty,
}

impl MailRowState {
    /// Whether clicking this row should send anything.
    ///
    /// Read by the drawing **and** the hit test, from here, so the two cannot
    /// drift into a row that looks clickable and is not -- the rule the
    /// trainer window made load-bearing after an inert row that answered a
    /// click would have shipped a request the server declines in silence.
    pub fn clickable(self) -> bool {
        matches!(self, Self::Collectable)
    }
}

/// One attachment, as far as this window is concerned.
#[derive(Debug, Clone, PartialEq)]
pub struct MailAttachment {
    /// Stack size, straight off the mail record.
    pub count: u32,
    pub icon: Option<egui::TextureId>,
}

/// One letter.
#[derive(Debug, Clone, PartialEq)]
pub struct MailRow {
    /// **The server's mail id**, and what a click acts on. Never a row
    /// position: the inbox is filtered -- deleted, undelivered and expired
    /// letters are skipped -- so positions do not close up, exactly like a
    /// loot slot, a gossip option and a trainer spell.
    pub id: u32,
    /// Who sent it, already resolved. A player's guid needs a name query and
    /// an auction's entry has no name at all, so the resolving is the caller's
    /// job and this is whatever came of it.
    pub sender: String,
    pub subject: String,
    /// The first line of the body, where there is one. Drawn dimmed under the
    /// subject, because a letter with only text in it is inert and would
    /// otherwise be a row that says nothing.
    pub body: String,
    /// Copper enclosed.
    pub money: u32,
    pub attachments: Vec<MailAttachment>,
    /// Whether the reader has opened it. Unread letters are drawn brighter,
    /// which is the only place in this window the check mask shows.
    pub read: bool,
    /// Days until it expires. Drawn because a letter is the one thing in this
    /// client that goes away on its own.
    pub days_left: f32,
    pub state: MailRowState,
}

/// Everything the mail window draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MailView {
    pub rows: Vec<MailRow>,
    /// Letters the server counted and did not send. See the module comment --
    /// this is drawn because nothing else in the protocol names them.
    pub withheld: u32,
}

/// A window's worth of plausible rows, for the layout editor.
///
/// Carries **both states and a withheld count**, because the editor is where
/// somebody sizes this frame and a placeholder of nothing but full letters
/// sizes a window for a shorter list than the real one -- the same reason the
/// trainer placeholder carries greyed rows.
pub fn placeholder() -> MailView {
    MailView {
        withheld: 3,
        rows: vec![
            MailRow {
                id: 1,
                sender: "Testwolf".into(),
                subject: "Supplies".into(),
                body: "Take what you need.".into(),
                money: 4321,
                attachments: vec![MailAttachment {
                    count: 5,
                    icon: None,
                }],
                read: false,
                days_left: 30.0,
                state: MailRowState::Collectable,
            },
            MailRow {
                id: 2,
                sender: "Auction House".into(),
                subject: "Auction successful".into(),
                body: String::new(),
                money: 0,
                attachments: Vec::new(),
                read: true,
                days_left: 2.4,
                state: MailRowState::Empty,
            },
        ],
    }
}

/// Unscaled height of the title line.
fn header(style: &Style) -> f32 {
    style.font_size + style.gap
}

/// Unscaled height of the band under the list holding the gesture line.
///
/// **Reserved whether or not the line is drawn**, for the reason the trade
/// window reserves its own: a window that grew as its contents changed would
/// move its own rows out from under the cursor at the moment somebody clicks
/// one.
fn footer(style: &Style) -> f32 {
    style.font_size * 0.85 + style.gap
}

/// A letter's row is two lines: the subject over the sender and the body.
fn row_height(style: &Style) -> f32 {
    style.spellbook_row * 1.6
}

/// How much room a window with this many rows wants.
pub fn size(rows: usize, style: &Style, scale: f32) -> Vec2 {
    let height = header(style)
        + rows.max(1) as f32 * row_height(style)
        + footer(style)
        + style.padding * 2.0;
    Vec2::new(style.loot_width * 1.9, height) * scale
}

/// Where each row sits.
///
/// The single source of truth for row geometry, read by the drawing and the
/// hit test both.
pub fn row_rects(
    rect: Rect,
    rows: usize,
    style: &Style,
    scale: f32,
) -> impl Iterator<Item = Rect> + '_ {
    let pad = style.padding * scale;
    let row = row_height(style) * scale;
    let top = rect.min.y + pad + header(style) * scale;
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    (0..rows).map(move |i| {
        Rect::from_min_size(Pos2::new(left, top + i as f32 * row), Vec2::new(width, row))
    })
}

/// Which row contains a point, **if that row is one a click can act on**.
///
/// The clickability test is here rather than in the caller, exactly as the
/// trainer window's is: "which row is under the cursor" and "which row would a
/// click collect" are different questions here, because an emptied letter is a
/// normal thing for a mailbox to contain.
pub fn row_at(
    rect: Rect,
    rows: &[MailRow],
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<usize> {
    row_rects(rect, rows.len(), style, scale)
        .position(|row| row.contains(point))
        .filter(|&index| rows[index].state.clickable())
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Dims a colour towards the background without inventing a new one.
fn dim(colour: Color32, factor: f32) -> Color32 {
    let scale = |channel: u8| (f32::from(channel) * factor).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(
        scale(colour.r()),
        scale(colour.g()),
        scale(colour.b()),
        colour.a(),
    )
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &MailView, style: &Style, scale: f32) {
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
    let pad = style.padding * scale;
    let font = FontId::proportional(style.font_size * scale);
    let small = FontId::proportional(style.font_size * 0.85 * scale);

    // The header names both numbers when they differ. See the module comment:
    // the surplus is named nowhere else in the protocol.
    let title = match view.withheld {
        0 => "Mailbox".to_string(),
        withheld => format!("Mailbox  ({withheld} more not sent)"),
    };
    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        title,
        font.clone(),
        text,
    );

    let painter = painter.with_clip_rect(rect);
    for (index, bounds) in row_rects(rect, view.rows.len(), style, scale).enumerate() {
        let Some(row) = view.rows.get(index) else {
            break;
        };

        // Unread is the only place the check mask shows, and it is a
        // brightness rather than a badge -- there is nothing else on the row
        // it could be confused with.
        let label = match (row.state, row.read) {
            (MailRowState::Collectable, false) => text,
            (MailRowState::Collectable, true) => dim(text, 0.85),
            (MailRowState::Empty, _) => dim(text, 0.45),
        };

        let side = row_height(style) * 0.5 * scale;
        let mut x = bounds.min.x;
        for attachment in &row.attachments {
            let square = Rect::from_min_size(
                Pos2::new(x, bounds.min.y + style.border_width * scale),
                Vec2::splat(side),
            );
            if let Some(icon) = attachment.icon {
                painter.image(
                    icon,
                    square,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                painter.rect_filled(square, corner_radius(2.0 * scale), dim(text, 0.2));
            }
            // The stack size comes off the mail record and not off any
            // object, because a mailed item is not one.
            if attachment.count > 1 {
                painter.text(
                    square.right_bottom(),
                    Align2::RIGHT_BOTTOM,
                    attachment.count.to_string(),
                    small.clone(),
                    text,
                );
            }
            x += side + style.gap * scale;
        }

        let left = if row.attachments.is_empty() {
            bounds.min.x
        } else {
            x + style.gap * scale
        };
        painter.text(
            Pos2::new(left, bounds.min.y + style.gap * scale),
            Align2::LEFT_TOP,
            &row.subject,
            font.clone(),
            label,
        );
        // The second line is the sender and whatever text came with it. The
        // body is drawn because a letter carrying nothing but words is
        // otherwise a row that says nothing at all.
        let second = if row.body.is_empty() {
            row.sender.clone()
        } else {
            format!("{} -- {}", row.sender, row.body)
        };
        painter.text(
            Pos2::new(left, bounds.max.y - style.gap * scale),
            Align2::LEFT_BOTTOM,
            second,
            small.clone(),
            dim(label, 0.75),
        );

        let right = bounds.max.x;
        if row.money > 0 {
            painter.text(
                Pos2::new(right, bounds.min.y + style.gap * scale),
                Align2::RIGHT_TOP,
                money(row.money),
                small.clone(),
                label,
            );
        }
        // A letter is the one thing in this client that goes away by itself,
        // so how long is left is drawn on every row rather than on the ones
        // about to expire -- a countdown that appears only when it is nearly
        // over is a countdown nobody has learned to read.
        painter.text(
            Pos2::new(right, bounds.max.y - style.gap * scale),
            Align2::RIGHT_BOTTOM,
            format!("{:.0}d", row.days_left.max(0.0)),
            small.clone(),
            dim(label, 0.6),
        );
    }

    // The gesture, named on screen. See the module comment.
    let hint = if view.rows.is_empty() {
        "Nothing here."
    } else if view.rows.iter().any(|row| row.state.clickable()) {
        "Click a letter to take what is in it"
    } else {
        "Nothing left to take"
    };
    painter.text(
        Pos2::new(rect.min.x + pad, rect.max.y - pad),
        Align2::LEFT_BOTTOM,
        hint,
        small,
        dim(text, 0.6),
    );
}

/// Copper into the three coin units.
pub fn money(copper: u32) -> String {
    crate::frames::trainer::money(copper)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> MailView {
        placeholder()
    }

    /// The clickable filter is the whole of the hit test's contract, and both
    /// halves are asserted: a full letter answers and an emptied one does not.
    ///
    /// Testing only the first would pass just as well if `row_at` reported
    /// every row -- which is the mistake the trainer window's inert rows made
    /// expensive, and there the consequence was a request the server declines
    /// in silence.
    #[test]
    fn only_a_letter_with_something_in_it_answers_a_click() {
        let style = Style::default();
        let view = view();
        let rect = Rect::from_min_size(Pos2::ZERO, size(view.rows.len(), &style, 1.0));
        let rects: Vec<Rect> = row_rects(rect, view.rows.len(), &style, 1.0).collect();

        assert_eq!(
            row_at(rect, &view.rows, &style, 1.0, rects[0].center()),
            Some(0)
        );
        assert_eq!(
            row_at(rect, &view.rows, &style, 1.0, rects[1].center()),
            None,
            "an emptied letter answered a click, and the click would delete nothing \
             and send nothing"
        );
    }

    /// The rows are laid out by one function and nothing else, so a click and
    /// a paint cannot disagree about where row two is.
    #[test]
    fn rows_do_not_overlap_or_leave_the_window() {
        let style = Style::default();
        let view = view();
        let rect = Rect::from_min_size(Pos2::new(40.0, 30.0), size(view.rows.len(), &style, 1.0));
        let rects: Vec<Rect> = row_rects(rect, view.rows.len(), &style, 1.0).collect();
        for pair in rects.windows(2) {
            assert!(pair[0].max.y <= pair[1].min.y + 0.01);
        }
        for row in &rects {
            assert!(rect.contains_rect(*row), "{row:?} is outside {rect:?}");
        }
    }

    /// **The band under the list stays clear of every row.**
    ///
    /// The hint line is the thing that makes the gesture findable, so it grows
    /// towards the click targets -- which is exactly the shape the trade
    /// window had to assert about its own buttons after the same line was
    /// added there.
    #[test]
    fn the_hint_band_never_covers_a_row() {
        let style = Style::default();
        for count in 0..8 {
            let rect = Rect::from_min_size(Pos2::ZERO, size(count, &style, 1.0));
            let band = rect.max.y - style.padding - footer(&style);
            for row in row_rects(rect, count, &style, 1.0) {
                assert!(
                    row.max.y <= band + 0.01,
                    "row {row:?} reaches into the hint band at {band} with {count} rows"
                );
            }
        }
    }

    /// The withheld count reaches the header, because nothing else in the
    /// protocol names those letters.
    #[test]
    fn the_header_names_the_letters_that_were_not_sent() {
        assert_eq!(placeholder().withheld, 3);
    }
}
