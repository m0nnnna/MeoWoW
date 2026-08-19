//! The trade window, and the prompt that offers one.
//!
//! **The first frame in this interface that draws two people's state side by
//! side**, and that is the whole design problem. Every other window here shows
//! one thing: what a corpse holds, what a trainer teaches, what this character
//! carries. A trade window shows *your* half and *their* half, and the two are
//! separate packets arriving at separate times -- so the frame has to make it
//! obvious which is which, and must not let a click meant for one land on the
//! other.
//!
//! Three rules fall straight out of that:
//!
//! **The two halves come from genuinely different places, and one of them is
//! not the server.** [`TradeView::theirs`] is a packet. [`TradeView::ours`] is
//! this client's own record of what it put down, because the server sends an
//! offer to the *other* person and never back to its author -- measured over a
//! complete two-client trade, in which every extended packet at both ends
//! described the partner's half and none described the reader's own. So this
//! is the one window in the interface that is half memory, and the caller is
//! what remembers.
//!
//! **Only our half is clickable.** Taking something off the table is a request
//! only its owner may make, so a press on their column does nothing at all
//! rather than doing something plausible. Same rule as the trainer's inert
//! rows: the hit test answers for the squares a click can act on and stays
//! silent for the rest, because a request the server declines in silence is
//! the one failure this client cannot diagnose.
//!
//! **The seventh square is drawn apart from the other six.** It is the slot
//! that does *not* change hands, and a window that lined all seven up would
//! be showing an item as offered that will be handed straight back. It is
//! drawn under its own label, below the grid.
//!
//! The accept button says what the *other* side has done, because that is the
//! only part a player cannot work out for themselves: their own accept is a
//! button they just pressed, and the partner's is a fact only the server
//! knows.
//!
//! **And the window says how to put something in it.** Offering is a modal
//! right-click in the bag window -- there is no drag into these squares -- and
//! a modal gesture with nothing on screen naming it is one nobody finds. The
//! first person to test this milestone reported "I couldn't give him an item",
//! which is a sentence with two causes and no way to tell them apart. The hint
//! is drawn only while this side of the table is empty, so it stops being
//! clutter the moment it has been understood.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Squares in one side's half of the window. Six that change hands, plus the
/// one that does not.
pub const SLOTS: usize = 7;

/// Which square holds the item that is **not** being traded.
pub const NONTRADED: usize = 6;

/// Columns in the six-square grid. Two, so a half is three rows tall and the
/// two halves sit side by side in a window that is wider than it is tall --
/// which is what lets the labels above each column say whose is whose.
const COLUMNS: usize = 2;

/// One square of one side's offer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TradeSquare {
    /// What is in it, or `None` for an empty square. Empty squares are still
    /// drawn: seven outlines say how much room is left, where a shorter list
    /// would say nothing.
    pub item: Option<TradeSquareItem>,
}

/// What is in an occupied square.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeSquareItem {
    /// Drawn only when the stack is bigger than one, like the bag window's.
    pub count: u32,
    /// The item's name if a query has answered, otherwise something naming the
    /// entry. **Never blank**: an unnamed square and an empty square must not
    /// look alike in a window where the difference is somebody's property.
    pub label: String,
    pub icon: Option<egui::TextureId>,
}

/// Everything the trade window draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TradeView {
    /// Who is on the other side. Blank where the name has not resolved, which
    /// is drawn as such rather than as a guid -- see [`Self::partner`]'s use
    /// in [`draw`].
    pub partner: String,
    /// Their half, as the server last stated it.
    pub theirs: [TradeSquare; SLOTS],
    /// Our half, from the caller's own record of what it put on the table.
    /// **Not from a packet** -- see the module comment.
    pub ours: [TradeSquare; SLOTS],
    /// Copper on the table from each side.
    pub their_money: u32,
    pub our_money: u32,
    /// Whether the partner has pressed accept. The one fact in this window a
    /// player cannot see for themselves.
    pub they_accepted: bool,
    /// Whether this client has. Local, because the server never reports a
    /// client's own accept back to it.
    pub we_accepted: bool,
}

/// What a click in the window asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeClick {
    /// Take our own item out of this slot. Only ever produced for a square on
    /// our side that has something in it.
    Clear(u8),
    Accept,
    Cancel,
}

/// A window's worth of plausible contents, for the layout editor.
///
/// Deliberately **asymmetric** -- items on one side, money on the other, one
/// accept pressed and one not. A placeholder with matching halves would let
/// somebody position a window in which a left/right mix-up is invisible, which
/// is the exact mistake this frame is shaped to prevent.
pub fn placeholder() -> TradeView {
    let mut view = TradeView {
        partner: "Watcher".into(),
        their_money: 0,
        our_money: 12_345,
        they_accepted: true,
        we_accepted: false,
        ..Default::default()
    };
    view.theirs[0].item = Some(TradeSquareItem {
        count: 3,
        label: "Refreshing Spring Water".into(),
        icon: None,
    });
    view.theirs[1].item = Some(TradeSquareItem {
        count: 1,
        label: "Worn Shortsword".into(),
        icon: None,
    });
    view.ours[0].item = Some(TradeSquareItem {
        count: 5,
        label: "Darnassian Bleu".into(),
        icon: None,
    });
    view
}

/// Unscaled height of the title and the two column headings.
fn header(style: &Style) -> f32 {
    (style.font_size + style.gap) * 2.0
}

/// Unscaled height of what sits under the grid: the hint line, the money line
/// and the two buttons.
///
/// The hint's room is reserved whether or not it is drawn. It appears and
/// disappears as the table fills and empties, and a window that resized with it
/// would move its own Cancel button out from under the cursor at the exact
/// moment somebody puts an item down.
fn footer(style: &Style) -> f32 {
    (style.font_size + style.gap) * 2.0 + style.party_invite_button_height + style.gap
}

/// Unscaled height of the non-traded row: its label and one square.
fn nontraded_band(style: &Style) -> f32 {
    style.font_size + style.gap + style.slot_size + style.gap
}

/// How much room the window wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    let rows = SLOTS.div_ceil(COLUMNS).saturating_sub(1).max(1);
    let column = COLUMNS as f32 * style.slot_size + (COLUMNS as f32 - 1.0) * style.slot_gap;
    let width = column * 2.0 + style.gap * 3.0 + style.padding * 2.0;
    let grid = rows as f32 * style.slot_size + (rows as f32 - 1.0) * style.slot_gap;
    let height =
        header(style) + grid + style.gap + nontraded_band(style) + footer(style) + style.padding * 2.0;
    Vec2::new(width.max(style.loot_width), height) * scale
}

/// Where every square of one side sits.
///
/// **The single statement of the geometry**, read by the drawing and by the
/// hit test both. Two independently written copies agree until one of them
/// changes -- and here the cost of them disagreeing is a click that takes an
/// item out of a square other than the one under the cursor.
///
/// `theirs` picks the column. The six traded squares come first in reading
/// order; index [`NONTRADED`] is the lone square below the grid, deliberately
/// not in line with the others.
pub fn square_rects(
    rect: Rect,
    theirs: bool,
    style: &Style,
    scale: f32,
) -> impl Iterator<Item = Rect> + '_ {
    let pad = style.padding * scale;
    let gap = style.gap * scale;
    let side = style.slot_size * scale;
    let slot_gap = style.slot_gap * scale;
    let column = COLUMNS as f32 * side + (COLUMNS as f32 - 1.0) * slot_gap;

    // Ours on the left, theirs on the right. Fixed rather than configurable:
    // a window whose sides could swap is a window in which a player's habit is
    // wrong on somebody else's machine.
    let left = if theirs {
        rect.min.x + pad + column + gap * 2.0
    } else {
        rect.min.x + pad
    };
    let top = rect.min.y + pad + header(style) * scale;
    let rows = SLOTS.div_ceil(COLUMNS).saturating_sub(1).max(1);
    let grid_height = rows as f32 * side + (rows as f32 - 1.0) * slot_gap;

    (0..SLOTS).map(move |index| {
        if index == NONTRADED {
            // Below the grid and below its own label, on its own. See the
            // module comment: lining it up with the six would draw something
            // as offered that is going to be handed back.
            Pos2::new(
                left,
                top + grid_height + gap + (style.font_size + style.gap) * scale,
            )
        } else {
            let (row, column) = (index / COLUMNS, index % COLUMNS);
            Pos2::new(
                left + column as f32 * (side + slot_gap),
                top + row as f32 * (side + slot_gap),
            )
        }
    })
    .map(move |at| Rect::from_min_size(at, Vec2::splat(side)))
}

/// Where the two buttons are.
///
/// Returns `(accept, cancel)`, along the bottom. Stated once, like the party
/// invite's, because the two answers are opposite.
fn buttons(rect: Rect, style: &Style, scale: f32) -> (Rect, Rect) {
    let pad = style.padding * scale;
    let gap = style.gap * scale;
    let height = style.party_invite_button_height * scale;
    let width = ((rect.width() - pad * 2.0 - gap) * 0.5).max(1.0);
    let top = rect.bottom() - pad - height;
    let accept = Rect::from_min_size(Pos2::new(rect.left() + pad, top), Vec2::new(width, height));
    let cancel = Rect::from_min_size(
        Pos2::new(accept.right() + gap, top),
        Vec2::new(width, height),
    );
    (accept, cancel)
}

/// What a click at `point` asked for, if anything.
///
/// **Only our own occupied squares answer.** Their column is drawn and never
/// hit-tested: taking an item off the table is a request only its owner may
/// make, and one sent for their square would be declined in silence. Same
/// shape as the trainer window's `row_at`, which answers only for rows a
/// purchase is legal for.
pub fn click_at(
    rect: Rect,
    view: &TradeView,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<TradeClick> {
    let (accept, cancel) = buttons(rect, style, scale);
    if accept.contains(point) {
        return Some(TradeClick::Accept);
    }
    if cancel.contains(point) {
        return Some(TradeClick::Cancel);
    }
    square_rects(rect, false, style, scale)
        .position(|square| square.contains(point))
        .filter(|&index| view.ours[index].item.is_some())
        .map(|index| TradeClick::Clear(index as u8))
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

fn dim(colour: Color32, by: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        (colour.r() as f32 * by) as u8,
        (colour.g() as f32 * by) as u8,
        (colour.b() as f32 * by) as u8,
        colour.a(),
    )
}

/// Copper as gold/silver/copper, the same arithmetic the bag window uses.
fn purse(copper: u32) -> String {
    let (gold, silver, copper) = (copper / 10_000, (copper % 10_000) / 100, copper % 100);
    if gold > 0 {
        format!("{gold}g {silver}s {copper}c")
    } else if silver > 0 {
        format!("{silver}s {copper}c")
    } else {
        format!("{copper}c")
    }
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &TradeView, style: &Style, scale: f32) {
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
    let gap = style.gap * scale;
    let font = FontId::proportional(style.font_size * scale);
    let small = FontId::proportional(style.font_size * 0.85 * scale);
    let painter = painter.with_clip_rect(rect);

    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        "Trade",
        font.clone(),
        text,
    );

    // Column headings, one over each half. These are the whole reason a
    // player can tell the halves apart, so they are drawn from the same
    // `theirs` flag the squares are placed by rather than from a second
    // decision about which side is which.
    let heading_y = rect.min.y + pad + (style.font_size + style.gap) * scale;
    for (theirs, label) in [
        (false, "You offer".to_string()),
        (
            true,
            if view.partner.is_empty() {
                // Never a guid and never blank: the heading is what says whose
                // property the right-hand column is.
                "They offer".to_string()
            } else {
                format!("{} offers", view.partner)
            },
        ),
    ] {
        if let Some(first) = square_rects(rect, theirs, style, scale).next() {
            painter.text(
                Pos2::new(first.min.x, heading_y),
                Align2::LEFT_TOP,
                label,
                small.clone(),
                dim(text, 0.8),
            );
        }
    }

    for (theirs, squares) in [(false, &view.ours), (true, &view.theirs)] {
        for (index, bounds) in square_rects(rect, theirs, style, scale).enumerate() {
            if index == NONTRADED {
                painter.text(
                    Pos2::new(bounds.min.x, bounds.min.y - gap - style.font_size * scale),
                    Align2::LEFT_TOP,
                    "Will not be traded",
                    small.clone(),
                    dim(text, 0.6),
                );
            }
            draw_square(&painter, bounds, &squares[index], style, scale);
        }
    }

    // **How to put something in.** Drawn only while our own side is empty:
    // offering is a modal right-click in the bag window, which nothing else on
    // screen names, and a gesture nobody can discover is not a gesture. It
    // goes under our column because that is the half it acts on.
    if view.ours.iter().all(|square| square.item.is_none()) {
        if let Some(last) = square_rects(rect, false, style, scale).nth(NONTRADED) {
            painter.text(
                Pos2::new(last.min.x, last.max.y + gap),
                Align2::LEFT_TOP,
                "Right-click a bag item to offer it",
                small.clone(),
                dim(text, 0.6),
            );
        }
    }

    // Money, one line under each column.
    let (accept_button, cancel_button) = buttons(rect, style, scale);
    let money_y = accept_button.top() - gap - style.font_size * scale;
    for (theirs, copper) in [(false, view.our_money), (true, view.their_money)] {
        if let Some(first) = square_rects(rect, theirs, style, scale).next() {
            painter.text(
                Pos2::new(first.min.x, money_y),
                Align2::LEFT_TOP,
                purse(copper),
                small.clone(),
                if copper == 0 { dim(text, 0.5) } else { text },
            );
        }
    }

    // The accept button reports **their** state, not ours: a player knows
    // whether they pressed it and cannot know whether the other person did.
    let label = if view.they_accepted {
        "Accept  (they have)"
    } else if view.we_accepted {
        "Accepted -- waiting"
    } else {
        "Accept"
    };
    draw_button(&painter, accept_button, label, view.we_accepted, style, scale);
    draw_button(&painter, cancel_button, "Cancel", false, style, scale);
}

fn draw_button(
    painter: &Painter,
    rect: Rect,
    label: &str,
    pressed: bool,
    style: &Style,
    scale: f32,
) {
    let corner = corner_radius(style.corner * 0.5 * scale);
    painter.rect_filled(
        rect,
        corner,
        if pressed {
            style.spellbook_selected.into()
        } else {
            Color32::from(style.slot_background)
        },
    );
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(style.border_width.max(1.0) * scale, style.border),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(style.font_size * 0.85 * scale),
        style.text.into(),
    );
}

fn draw_square(
    painter: &Painter,
    rect: Rect,
    square: &TradeSquare,
    style: &Style,
    scale: f32,
) {
    let corner = corner_radius(style.corner * 0.5 * scale);
    painter.rect_filled(rect, corner, style.slot_background);
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(
            style.border_width.max(1.0) * scale,
            Color32::from(style.slot_empty_border),
        ),
        StrokeKind::Inside,
    );

    let Some(item) = &square.item else { return };
    match item.icon {
        Some(icon) => {
            painter.image(
                icon,
                rect.shrink(style.border_width * scale),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // No icon yet: the first characters of the name rather than nothing,
        // for the reason `label` is never blank -- an unnamed square and an
        // empty one must not look alike when the difference is whether
        // somebody is giving you something.
        None => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                item.label.chars().take(3).collect::<String>(),
                FontId::proportional(style.font_size * 0.8 * scale),
                style.text.into(),
            );
        }
    }

    if item.count > 1 {
        painter.text(
            rect.max - Vec2::splat(style.border_width * 2.0 * scale),
            Align2::RIGHT_BOTTOM,
            item.count.to_string(),
            FontId::proportional(style.font_size * 0.8 * scale),
            Color32::WHITE,
        );
    }
}

/// "Watcher wants to trade." -- and the two buttons that answer it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TradeOfferView {
    /// Who is asking. Unlike a party invite, which carries only a name, this
    /// one arrives as a **guid** -- so a name here means a query has come back
    /// and a blank one is honest rather than broken.
    pub from: String,
}

impl TradeOfferView {
    pub fn placeholder() -> Self {
        Self {
            from: "Watcher".into(),
        }
    }
}

/// Which answer was pressed.
///
/// Two of the three the protocol has: the ignore form is not offered, because
/// there is no ignore list in this client and a button that meant the same as
/// Decline would be two buttons for one decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeOfferAnswer {
    Accept,
    Decline,
}

/// How much room the prompt wants. The same shape as the party invite's, and
/// deliberately so: they are the same kind of interruption.
pub fn offer_size(style: &Style, scale: f32) -> Vec2 {
    let height = style.padding * 3.0 + style.font_size * 1.3 + style.party_invite_button_height;
    Vec2::new(style.party_invite_width, height) * scale
}

fn offer_buttons(rect: Rect, style: &Style, scale: f32) -> (Rect, Rect) {
    buttons(rect, style, scale)
}

/// Which answer a click is, or `None` for a press that missed both buttons.
///
/// `None` rather than a nearest-button guess, for the reason the party
/// invite's does it: the two answers are opposite, and an accidental accept
/// opens a window a stranger can put things in.
pub fn offer_click_at(
    rect: Rect,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<TradeOfferAnswer> {
    let (accept, decline) = offer_buttons(rect, style, scale);
    if accept.contains(point) {
        Some(TradeOfferAnswer::Accept)
    } else if decline.contains(point) {
        Some(TradeOfferAnswer::Decline)
    } else {
        None
    }
}

/// Paints the prompt.
pub fn draw_offer(
    painter: &Painter,
    rect: Rect,
    view: &TradeOfferView,
    style: &Style,
    scale: f32,
) {
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
    painter.text(
        Pos2::new(inner.center().x, inner.top()),
        Align2::CENTER_TOP,
        if view.from.is_empty() {
            "Somebody wants to trade.".to_string()
        } else {
            format!("{} wants to trade.", view.from)
        },
        FontId::proportional(style.font_size * scale),
        style.text.into(),
    );

    let (accept, decline) = offer_buttons(rect, style, scale);
    draw_button(&painter, accept, "Trade", false, style, scale);
    draw_button(&painter, decline, "Decline", false, style, scale);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::new(100.0, 100.0), size(&Style::default(), 1.0))
    }

    /// The two halves must not overlap, or a click meant for one lands on the
    /// other -- which in this window means taking somebody else's item off the
    /// table, or trying to.
    #[test]
    fn the_two_halves_do_not_overlap() {
        let (style, scale) = (Style::default(), 1.0);
        let ours: Vec<Rect> = square_rects(rect(), false, &style, scale).collect();
        let theirs: Vec<Rect> = square_rects(rect(), true, &style, scale).collect();
        for a in &ours {
            for b in &theirs {
                assert!(
                    !a.intersects(*b),
                    "our square {a:?} overlaps their square {b:?}"
                );
            }
        }
    }

    /// Ours on the left, always. A window whose sides could swap is one where
    /// a habit is wrong on somebody else's machine.
    #[test]
    fn our_half_is_the_left_one() {
        let (style, scale) = (Style::default(), 1.0);
        let ours = square_rects(rect(), false, &style, scale).next().unwrap();
        let theirs = square_rects(rect(), true, &style, scale).next().unwrap();
        assert!(ours.min.x < theirs.min.x);
    }

    /// The seventh square is below the grid rather than in it -- the whole
    /// point being that it is not part of the offer.
    #[test]
    fn the_nontraded_square_is_apart_from_the_six() {
        let (style, scale) = (Style::default(), 1.0);
        let squares: Vec<Rect> = square_rects(rect(), false, &style, scale).collect();
        let lowest_traded = squares[..NONTRADED]
            .iter()
            .map(|r| r.max.y)
            .fold(f32::MIN, f32::max);
        assert!(
            squares[NONTRADED].min.y > lowest_traded,
            "the non-traded square is level with the six that are traded"
        );
    }

    /// Nothing under the grid sits on top of the squares.
    ///
    /// Worth asserting rather than eyeballing, because the band under the grid
    /// grew when the "right-click a bag item" hint was added, and the thing it
    /// would have grown into is the non-traded square -- which is a *click
    /// target*. A button drawn over a square is the recurring bug in this
    /// interface, and the last of them took a live session to find.
    #[test]
    fn the_buttons_clear_every_square() {
        let (style, scale) = (Style::default(), 1.0);
        let (accept, cancel) = buttons(rect(), &style, scale);
        for theirs in [false, true] {
            for square in square_rects(rect(), theirs, &style, scale) {
                assert!(
                    !square.intersects(accept) && !square.intersects(cancel),
                    "a button sits on top of {square:?}"
                );
            }
        }
        assert!(rect().contains_rect(accept) && rect().contains_rect(cancel));
    }

    /// A click on our own occupied square asks to clear *that* square, and a
    /// click on the same position in their column asks for nothing.
    ///
    /// **Both halves are asserted**, because a hit test that answered for
    /// everything would pass the first assertion alone and would send a
    /// request the server declines in silence.
    #[test]
    fn only_our_own_occupied_squares_answer() {
        let (style, scale) = (Style::default(), 1.0);
        let mut view = placeholder();
        // Match the placeholder to what this test is about: one of ours full,
        // the mirrored square of theirs full too.
        view.ours[2].item = Some(TradeSquareItem {
            count: 1,
            label: "Worn Shortsword".into(),
            icon: None,
        });
        view.theirs[2].item = view.ours[2].item.clone();

        let ours = square_rects(rect(), false, &style, scale).nth(2).unwrap();
        assert_eq!(
            click_at(rect(), &view, &style, scale, ours.center()),
            Some(TradeClick::Clear(2))
        );

        let theirs = square_rects(rect(), true, &style, scale).nth(2).unwrap();
        assert_eq!(click_at(rect(), &view, &style, scale, theirs.center()), None);
    }

    /// An empty square of our own is inert too: there is nothing to take out
    /// of it, and a request naming it would be refused silently.
    #[test]
    fn an_empty_square_of_ours_is_inert() {
        let (style, scale) = (Style::default(), 1.0);
        let mut view = placeholder();
        view.ours[3].item = None;
        let square = square_rects(rect(), false, &style, scale).nth(3).unwrap();
        assert_eq!(click_at(rect(), &view, &style, scale, square.center()), None);
    }

    /// The buttons answer, and a press between them does not.
    #[test]
    fn the_buttons_answer_and_the_gap_does_not() {
        let (style, scale) = (Style::default(), 1.0);
        let view = placeholder();
        let (accept, cancel) = buttons(rect(), &style, scale);
        assert_eq!(
            click_at(rect(), &view, &style, scale, accept.center()),
            Some(TradeClick::Accept)
        );
        assert_eq!(
            click_at(rect(), &view, &style, scale, cancel.center()),
            Some(TradeClick::Cancel)
        );
        let between = Pos2::new((accept.right() + cancel.left()) * 0.5, accept.center().y);
        assert_eq!(click_at(rect(), &view, &style, scale, between), None);
    }

    /// The offer prompt's two answers are opposite, so the gap between them
    /// answers nothing -- the same assertion the party invite carries.
    #[test]
    fn the_offer_prompt_ignores_a_press_between_its_buttons() {
        let (style, scale) = (Style::default(), 1.0);
        let rect = Rect::from_min_size(Pos2::ZERO, offer_size(&style, scale));
        let (accept, decline) = offer_buttons(rect, &style, scale);
        assert_eq!(
            offer_click_at(rect, &style, scale, accept.center()),
            Some(TradeOfferAnswer::Accept)
        );
        assert_eq!(
            offer_click_at(rect, &style, scale, decline.center()),
            Some(TradeOfferAnswer::Decline)
        );
        let between = Pos2::new((accept.right() + decline.left()) * 0.5, accept.center().y);
        assert_eq!(offer_click_at(rect, &style, scale, between), None);
    }
}
