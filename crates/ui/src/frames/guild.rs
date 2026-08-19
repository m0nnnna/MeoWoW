//! The guild window: a list of people who are mostly not here.
//!
//! Every other list this interface draws is about things in front of the
//! character -- a corpse's contents, a trainer's spells, a vendor's stock, the
//! members of a party. This one is mostly about characters who logged out
//! days ago, and the frame's whole job is to be honest about which rows are
//! which.
//!
//! **The distinction is the packet's own and is not invented here.** A member
//! record carries a four-byte "days since logout" field *only* when that
//! member is offline, so "online" and "how long ago" are mutually exclusive by
//! construction rather than by a convention this frame chose. An online row
//! shows where they are; an offline row shows how long they have been gone.
//! There is no row that shows both, and no row that shows neither.
//!
//! ## Which rows answer a click
//!
//! Only the online ones. A click sets the chat line to whisper that member,
//! and a whisper to somebody who is not logged in is refused by the server
//! with a line the client would then have to explain -- so the row is dimmed
//! and inert instead, the same decision the trainer window makes about a spell
//! you cannot learn and the mail window makes about an emptied letter.
//!
//! That is deliberately the *interesting* half to test at the window, because
//! it is the half no headless run reaches: a hit test that answered for every
//! row would look identical until somebody clicked a name in grey.
//!
//! ## The officer note column says why it is empty
//!
//! An officer note arrives as an empty string both when there is none and when
//! the reader's rank may not see one, which is the shape this project calls
//! *nothing happened is two findings wearing one sentence*. Here they are
//! separable -- the roster carries the rank table and the reader's own rank --
//! so [`GuildView::officer_notes`] carries the answer and the frame draws a
//! heading rather than a blank.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Whether the officer-note column is worth drawing, and why not when it is
/// not.
///
/// Three states rather than a `bool`, because "the roster has not been read"
/// and "you may not see them" produce the same empty column and want different
/// headings. Mirrors what `world::guild::Roster::officer_notes_visible`
/// returns without borrowing it -- this crate depends on neither `world` nor
/// `render`, which is what makes it testable with no connection and no GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OfficerNotes {
    /// The reader's rank carries the right; an empty note means there is none.
    Visible,
    /// The reader's rank does not; an empty note means nothing at all.
    Hidden,
    /// Not known -- the reader is not on their own roster, which does not
    /// happen and is not asserted away.
    #[default]
    Unknown,
}

/// One line in the guild window.
#[derive(Debug, Clone, PartialEq)]
pub struct GuildRow {
    /// **The member's name**, and what a click whispers. Carried rather than
    /// derived from a position for the same reason a trainer row carries its
    /// spell id: every guild request names a player by *name*, so the name is
    /// the handle and there is no excuse for counting rows.
    pub name: String,
    pub level: u8,
    /// The rank's name where the guild query has answered, and its number
    /// where it has not. Resolved by the caller, because a rank name lives in
    /// a different packet from the rank number.
    pub rank: String,
    /// Where they are, for a member who is logged in. `None` for one who is
    /// not -- and the two are exclusive, because the packet makes them so.
    pub zone: Option<String>,
    /// How long they have been gone, for a member who is not logged in.
    ///
    /// **A duration and not an instant.** The server divides by a day at the
    /// moment it builds the packet, so this ages with the roster rather than
    /// with the clock, and it is drawn as "3 days" rather than as a date.
    pub offline_days: Option<f32>,
    pub public_note: String,
    /// Empty both when there is none and when the reader may not see one --
    /// see [`GuildView::officer_notes`].
    pub officer_note: String,
}

impl GuildRow {
    /// Whether this member is logged in.
    ///
    /// Read off the *absence* of the offline field rather than off a separate
    /// flag, so the frame cannot disagree with the packet: there is exactly
    /// one fact here and one place it is stored.
    pub fn is_online(&self) -> bool {
        self.offline_days.is_none()
    }

    /// Whether clicking this row should do anything.
    ///
    /// Read by the drawing *and* the hit test, from here, so the two cannot
    /// drift into a row that looks clickable and is not.
    pub fn clickable(&self) -> bool {
        self.is_online()
    }
}

/// Everything the guild window draws.
///
/// One value rather than loose slices, because the parts are only meaningful
/// together: a roster drawn under another guild's name is a window that lies
/// about whose it is, and the name and the members genuinely arrive in
/// different packets.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GuildView {
    /// The guild's name where `CMSG_GUILD_QUERY` has answered.
    ///
    /// **Empty is a real state and is drawn as one.** The roster does not
    /// carry the guild's id, let alone its name, so between reading a roster
    /// and the query coming back there is a window with members and no title.
    pub name: String,
    /// The message of the day, as sent.
    pub motd: String,
    pub rows: Vec<GuildRow>,
    pub officer_notes: OfficerNotes,
}

impl GuildView {
    /// How many members are logged in.
    pub fn online(&self) -> usize {
        self.rows.iter().filter(|r| r.is_online()).count()
    }
}

/// A window's worth of plausible rows, for the layout editor.
///
/// Deliberately carries **both kinds of row**, because the editor is where
/// somebody sizes this frame and a roster of nothing but online members is a
/// narrower window than the real one -- and because a placeholder that could
/// not show the inert case would let somebody style a window whose greyed rows
/// they had never seen.
pub fn placeholder() -> GuildView {
    GuildView {
        name: "Cat Herders".into(),
        motd: "Mice are for sharing.".into(),
        officer_notes: OfficerNotes::Visible,
        rows: vec![
            GuildRow {
                name: "Testwolf".into(),
                level: 5,
                rank: "Guild Master".into(),
                zone: Some("Elwynn Forest".into()),
                offline_days: None,
                public_note: "the founder".into(),
                officer_note: "knows where the mailbox is".into(),
            },
            GuildRow {
                name: "Watcher".into(),
                level: 1,
                rank: "Officer".into(),
                zone: None,
                offline_days: Some(0.4),
                public_note: "watches".into(),
                officer_note: "second account".into(),
            },
            GuildRow {
                name: "Huntertest".into(),
                level: 2,
                rank: "Veteran".into(),
                zone: None,
                offline_days: Some(12.7),
                public_note: "has a gun and a full pouch".into(),
                officer_note: String::new(),
            },
        ],
    }
}

/// How much room a window with this many rows wants.
///
/// Three header lines: a title carrying the guild's name and the online count,
/// the message of the day, and the column headings. The motd gets its own
/// because it is a sentence somebody wrote, the same reason the trainer's
/// greeting does.
pub fn size(rows: usize, style: &Style, scale: f32) -> Vec2 {
    let height =
        header(style) + rows.max(1) as f32 * style.spellbook_row + style.padding * 2.0;
    Vec2::new(style.loot_width * 2.0, height) * scale
}

/// Unscaled height of the three header lines together.
fn header(style: &Style) -> f32 {
    (style.font_size + style.gap) * 3.0
}

/// Where each row sits.
///
/// The single source of truth for row geometry, read by the drawing and the
/// hit test both -- the rule the party frame's variable-height rows made
/// load-bearing.
pub fn row_rects(
    rect: Rect,
    rows: usize,
    style: &Style,
    scale: f32,
) -> impl Iterator<Item = Rect> + '_ {
    let pad = style.padding * scale;
    let row = style.spellbook_row * scale;
    let top = rect.min.y + pad + header(style) * scale;
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    (0..rows).map(move |i| {
        Rect::from_min_size(Pos2::new(left, top + i as f32 * row), Vec2::new(width, row))
    })
}

/// Which row contains a point, **if that row is one a click can act on**.
///
/// The clickability test lives here rather than in the caller for the reason
/// the trainer window states: a roster is mostly inert rows, so "which row is
/// under the cursor" and "which row would a click whisper" are different
/// questions, and a caller asking the first and acting on the second opens a
/// whisper to somebody who is not logged in.
pub fn row_at(
    rect: Rect,
    rows: &[GuildRow],
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<usize> {
    row_rects(rect, rows.len(), style, scale)
        .position(|row| row.contains(point))
        .filter(|&index| rows[index].clickable())
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// How long ago, in the roughest units that are still true.
///
/// **Never a date.** The packet carries a duration measured when it was built,
/// so converting it to a calendar day would need the moment of the reading and
/// would be a number nobody could check -- exactly what this project refuses
/// to draw. Hours below a day, then days.
pub fn ago(days: f32) -> String {
    if days < 1.0 / 24.0 {
        "just now".to_string()
    } else if days < 1.0 {
        format!("{:.0} hours", days * 24.0)
    } else if days < 2.0 {
        "1 day".to_string()
    } else {
        format!("{:.0} days", days)
    }
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &GuildView, style: &Style, scale: f32) {
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
    let line = (style.font_size + style.gap) * scale;
    let font = FontId::proportional(style.font_size * scale);
    let small = FontId::proportional(style.font_size * 0.85 * scale);

    // The title carries the guild's name where one is known and says so where
    // one is not -- a roster with no title is a real state, because the name
    // arrives in a different packet from the members.
    let title = if view.name.is_empty() {
        "Guild".to_string()
    } else {
        format!("{}  ({} of {} online)", view.name, view.online(), view.rows.len())
    };
    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        title,
        font.clone(),
        text,
    );
    painter.text(
        rect.min + Vec2::new(pad, pad + line),
        Align2::LEFT_TOP,
        &view.motd,
        small.clone(),
        dim(text, 0.75),
    );

    // The column headings, and the officer column's heading is the whole
    // reason it is drawn: an empty column headed "officer" and one headed
    // "officer (hidden)" are the same pixels and different facts.
    let heading = match view.officer_notes {
        OfficerNotes::Visible => "name / level / rank                    note            officer",
        OfficerNotes::Hidden => "name / level / rank                    note      officer (hidden)",
        OfficerNotes::Unknown => "name / level / rank                    note",
    };
    painter.text(
        rect.min + Vec2::new(pad, pad + line * 2.0),
        Align2::LEFT_TOP,
        heading,
        small.clone(),
        dim(text, 0.45),
    );

    let painter = painter.with_clip_rect(rect);
    for (index, bounds) in row_rects(rect, view.rows.len(), style, scale).enumerate() {
        let Some(row) = view.rows.get(index) else {
            break;
        };
        // Dimmed rather than hidden, and *labelled*: a member who has not
        // logged in for a fortnight is information, and a roster that showed
        // only the people currently online would be a party frame.
        let colour = if row.is_online() {
            text
        } else {
            dim(text, 0.45)
        };

        painter.text(
            Pos2::new(bounds.min.x, bounds.center().y),
            Align2::LEFT_CENTER,
            format!("{}  {}", row.name, row.level),
            font.clone(),
            colour,
        );
        painter.text(
            Pos2::new(bounds.min.x + bounds.width() * 0.28, bounds.center().y),
            Align2::LEFT_CENTER,
            &row.rank,
            small.clone(),
            colour,
        );
        // Exactly one of the two, because the packet carries exactly one.
        let whereabouts = match (&row.zone, row.offline_days) {
            (Some(zone), _) => zone.clone(),
            (None, Some(days)) => ago(days),
            (None, None) => String::new(),
        };
        painter.text(
            Pos2::new(bounds.min.x + bounds.width() * 0.48, bounds.center().y),
            Align2::LEFT_CENTER,
            whereabouts,
            small.clone(),
            colour,
        );
        painter.text(
            Pos2::new(bounds.min.x + bounds.width() * 0.68, bounds.center().y),
            Align2::LEFT_CENTER,
            &row.public_note,
            small.clone(),
            colour,
        );
        if view.officer_notes == OfficerNotes::Visible && !row.officer_note.is_empty() {
            painter.text(
                Pos2::new(bounds.max.x, bounds.center().y),
                Align2::RIGHT_CENTER,
                &row.officer_note,
                small.clone(),
                dim(colour, 0.8),
            );
        }
    }
}

/// Dims a colour towards the background without inventing a new one, so a
/// restyled interface greys its inert rows in its own palette rather than in
/// this module's.
fn dim(colour: Color32, factor: f32) -> Color32 {
    let scale = |channel: u8| (f32::from(channel) * factor).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(
        scale(colour.r()),
        scale(colour.g()),
        scale(colour.b()),
        colour.a(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(rows: usize) -> (Rect, Style, f32) {
        let style = Style::default();
        let scale = 1.0;
        let rect = Rect::from_min_size(Pos2::new(60.0, 40.0), size(rows, &style, scale));
        (rect, style, scale)
    }

    #[test]
    fn rows_tile_the_window_without_overlapping() {
        let view = placeholder();
        let (rect, style, scale) = bounds(view.rows.len());
        let placed: Vec<Rect> = row_rects(rect, view.rows.len(), &style, scale).collect();
        assert_eq!(placed.len(), view.rows.len());
        for pair in placed.windows(2) {
            assert!(pair[0].max.y <= pair[1].min.y + 0.001);
        }
        for row in &placed {
            assert!(rect.contains_rect(*row), "{row:?} escapes {rect:?}");
        }
    }

    /// **The test this window exists to pass**, and both halves are asserted
    /// deliberately. A hit test that answered for every row passes the first
    /// half alone, and the failure it produces is a whisper opened to somebody
    /// who is not logged in -- which looks like a bug in chat rather than one
    /// in the roster.
    #[test]
    fn only_online_rows_answer_a_click() {
        let view = placeholder();
        let (rect, style, scale) = bounds(view.rows.len());
        let centres: Vec<Pos2> = row_rects(rect, view.rows.len(), &style, scale)
            .map(|r| r.center())
            .collect();

        assert_eq!(row_at(rect, &view.rows, &style, scale, centres[0]), Some(0));
        assert_eq!(row_at(rect, &view.rows, &style, scale, centres[1]), None);
        assert_eq!(row_at(rect, &view.rows, &style, scale, centres[2]), None);
        assert_eq!(
            row_at(rect, &view.rows, &style, scale, rect.min + Vec2::splat(1.0)),
            None
        );
    }

    /// Online and offline are exclusive because the packet makes them so, and
    /// the frame reads that from one field rather than from two that could
    /// disagree.
    #[test]
    fn a_row_is_online_exactly_when_it_has_no_offline_field() {
        let view = placeholder();
        assert!(view.rows[0].is_online());
        assert!(!view.rows[1].is_online());
        assert_eq!(view.online(), 1);
    }

    /// A duration is drawn as a duration. The boundaries matter more than the
    /// wording: "just now" has to be reachable, or a member who logged out a
    /// minute ago reads as "0 days".
    #[test]
    fn elapsed_time_is_drawn_in_units_that_are_still_true() {
        assert_eq!(ago(0.0), "just now");
        assert_eq!(ago(0.25), "6 hours");
        assert_eq!(ago(1.4), "1 day");
        assert_eq!(ago(12.7), "13 days");
    }
}
