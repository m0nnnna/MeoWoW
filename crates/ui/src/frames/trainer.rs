//! The trainer window: what this NPC will teach, and what it costs.
//!
//! A list, sized to its contents, built the same way the loot window is --
//! but with one difference that shapes the whole frame: **most rows are not
//! clickable, and that is normal rather than exceptional.** A corpse only
//! holds things you may take. A trainer's list is mostly things you may not:
//! ranks above your level, spells you already know. Drawing those is the
//! point -- "come back at 20" is information -- so the window has to say
//! *why* a row is inert without making it look broken.
//!
//! Three states, three colours, and the greyed ones are still drawn with
//! their level and price. A row that cannot be bought is dimmed rather than
//! hidden, because a list that silently omitted them would read as a trainer
//! with nothing to teach.
//!
//! **The row carries its spell id and the caller sends that**, never a row
//! position. The server filters the list per character, so position `n` means
//! different things to two people standing at the same NPC -- the same reason
//! a loot slot and a gossip option index are carried rather than counted, and
//! here the id is available so there is no excuse.
//!
//! **The price is drawn as sent.** It is the discounted figure the server will
//! actually charge, not the table's, and a window that recomputed it would be
//! wrong for everyone not at neutral standing. See [`world::trainer`].

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// Why a row is drawn the way it is.
///
/// Mirrors `world::trainer::TrainerSpellState` rather than borrowing it,
/// because this crate deliberately depends on neither `world` nor `render` --
/// which is what makes the whole interface testable without a connection or a
/// GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainerRowState {
    /// Learnable now. The only state a click does anything in.
    Available,
    /// Level, skill or a prerequisite is missing.
    Unavailable,
    /// Already known.
    Known,
}

impl TrainerRowState {
    /// Whether clicking this row should send anything.
    ///
    /// Read by the drawing *and* the hit test, from here, so the two cannot
    /// drift into a row that looks clickable and is not.
    pub fn clickable(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// One line in the trainer window.
#[derive(Debug, Clone, PartialEq)]
pub struct TrainerRow {
    /// **The server's spell id**, and what a click asks for. See the module
    /// comment for why this is carried rather than derived from position.
    pub spell: u32,
    pub name: String,
    /// Copper, exactly as the server quoted it.
    pub cost: u32,
    /// Drawn beside the name when the row is out of reach, because "not yet"
    /// is only useful with a number attached.
    pub required_level: u8,
    pub state: TrainerRowState,
    pub icon: Option<egui::TextureId>,
}

/// Everything the trainer window draws.
///
/// One value rather than a greeting beside a slice, because the two are only
/// ever meaningful together: a greeting with somebody else's spell list is a
/// window that lies about which NPC it belongs to, and this milestone can have
/// two trainers stacked at the same spot.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrainerView {
    /// What the trainer said, as sent. Drawn even when [`Self::rows`] is
    /// empty -- a trainer with nothing for this character still greets, and a
    /// blank window would read as a request that failed.
    pub greeting: String,
    pub rows: Vec<TrainerRow>,
}

/// A window's worth of plausible rows, for the layout editor.
///
/// Deliberately carries **all three states**, because the editor is where
/// somebody sizes and positions this frame and the greyed rows are what make
/// it as tall as it really gets. A placeholder of nothing but learnable rows
/// would size a window that fits a shorter list than the real one.
pub fn placeholder() -> TrainerView {
    TrainerView {
        greeting: "Hello, warrior!  Ready for some training?".into(),
        rows: vec![
            TrainerRow {
                spell: 6673,
                name: "Battle Shout".into(),
                cost: 9,
                required_level: 1,
                state: TrainerRowState::Known,
                icon: None,
            },
            TrainerRow {
                spell: 100,
                name: "Charge".into(),
                cost: 95,
                required_level: 4,
                state: TrainerRowState::Available,
                icon: None,
            },
            TrainerRow {
                spell: 772,
                name: "Rend".into(),
                cost: 95,
                required_level: 4,
                state: TrainerRowState::Available,
                icon: None,
            },
            TrainerRow {
                spell: 3127,
                name: "Parry".into(),
                cost: 95,
                required_level: 6,
                state: TrainerRowState::Unavailable,
                icon: None,
            },
        ],
    }
}

/// How much room a window with this many rows wants.
///
/// Two header lines rather than one: the trainer's greeting is a sentence and
/// deserves its own, and a window that squeezed it beside a title would
/// truncate the one piece of text the server bothered to send.
pub fn size(rows: usize, style: &Style, scale: f32) -> Vec2 {
    let row = style.spellbook_row;
    let height = header(style) + rows.max(1) as f32 * row + style.padding * 2.0;
    Vec2::new(style.loot_width * 1.6, height) * scale
}

/// Unscaled height of the title and greeting lines together.
fn header(style: &Style) -> f32 {
    (style.font_size + style.gap) * 2.0
}

/// Where each row sits.
///
/// The single source of truth for row geometry, used by the drawing and the
/// hit test both -- the rule the party frame's variable-height rows made
/// load-bearing, and it matters here for the same reason it did there: a row
/// targeted by an averaged division is the wrong row, silently.
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
/// The clickability test lives here rather than in the caller deliberately. A
/// trainer list is mostly inert rows, so "which row is under the cursor" and
/// "which row would a click buy" are genuinely different questions, and a
/// caller that asked the first and acted on the second would send a request
/// the server declines in silence -- the one failure this client cannot
/// diagnose.
pub fn row_at(
    rect: Rect,
    rows: &[TrainerRow],
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

/// Paints the window.
pub fn draw(
    painter: &Painter,
    rect: Rect,
    greeting: &str,
    rows: &[TrainerRow],
    style: &Style,
    scale: f32,
) {
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

    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        "Trainer",
        font.clone(),
        text,
    );
    // The greeting is the server's own words and the only part of this window
    // that is not a number, so it is drawn even when there is nothing to
    // teach -- a trainer who has nothing for you still says something, and a
    // blank window would read as a failed request.
    painter.text(
        rect.min + Vec2::new(pad, pad + (style.font_size + style.gap) * scale),
        Align2::LEFT_TOP,
        greeting,
        small.clone(),
        dim(text, 0.75),
    );

    let painter = painter.with_clip_rect(rect);
    for (index, bounds) in row_rects(rect, rows.len(), style, scale).enumerate() {
        let Some(row) = rows.get(index) else { break };

        let (label, note) = match row.state {
            TrainerRowState::Available => (text, None),
            // Dimmed rather than hidden, and *labelled*: the level is the
            // whole content of the message.
            TrainerRowState::Unavailable => (
                dim(text, 0.45),
                Some(format!("level {}", row.required_level)),
            ),
            TrainerRowState::Known => (dim(text, 0.45), Some("known".to_string())),
        };

        let side = bounds.height() - style.border_width * 2.0 * scale;
        if let Some(icon) = row.icon {
            let square = Rect::from_min_size(
                Pos2::new(bounds.min.x, bounds.min.y + style.border_width * scale),
                Vec2::splat(side),
            );
            let tint = match row.state {
                TrainerRowState::Available => Color32::WHITE,
                _ => Color32::from_gray(110),
            };
            painter.image(
                icon,
                square,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                tint,
            );
        }

        let left = bounds.min.x + side + style.gap * scale;
        painter.text(
            Pos2::new(left, bounds.center().y),
            Align2::LEFT_CENTER,
            &row.name,
            font.clone(),
            label,
        );

        // Right-aligned, and only one of the two is ever shown: a row you
        // cannot buy has no useful price, and a row you can needs no excuse.
        let right = Pos2::new(bounds.max.x, bounds.center().y);
        match note {
            Some(note) => {
                painter.text(right, Align2::RIGHT_CENTER, note, small.clone(), label);
            }
            None => {
                painter.text(
                    right,
                    Align2::RIGHT_CENTER,
                    money(row.cost),
                    small.clone(),
                    label,
                );
            }
        }
    }
}

/// Dims a colour towards the background without inventing a new one, so a
/// restyled interface greys its inert rows in its own palette rather than in
/// this module's.
pub(crate) fn dim(colour: Color32, factor: f32) -> Color32 {
    let scale = |channel: u8| (f32::from(channel) * factor).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(
        scale(colour.r()),
        scale(colour.g()),
        scale(colour.b()),
        colour.a(),
    )
}

/// Copper into the three coin units.
///
/// **Free of the table**, like everything else here: the number is what the
/// server quoted, and this only changes its base. A trainer spell at 9 copper
/// and one at 1,200 gold go through the same arithmetic.
pub fn money(copper: u32) -> String {
    let (gold, rest) = (copper / 10_000, copper % 10_000);
    let (silver, copper) = (rest / 100, rest % 100);
    match (gold, silver) {
        (0, 0) => format!("{copper}c"),
        (0, _) => format!("{silver}s {copper}c"),
        _ => format!("{gold}g {silver}s {copper}c"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<TrainerRow> {
        vec![
            TrainerRow {
                spell: 6673,
                name: "Battle Shout".into(),
                cost: 9,
                required_level: 1,
                state: TrainerRowState::Known,
                icon: None,
            },
            TrainerRow {
                spell: 100,
                name: "Charge".into(),
                cost: 95,
                required_level: 4,
                state: TrainerRowState::Available,
                icon: None,
            },
            TrainerRow {
                spell: 3127,
                name: "Parry".into(),
                cost: 95,
                required_level: 6,
                state: TrainerRowState::Unavailable,
                icon: None,
            },
        ]
    }

    fn bounds(rows: usize) -> (Rect, Style, f32) {
        let style = Style::default();
        let scale = 1.0;
        let rect = Rect::from_min_size(Pos2::new(100.0, 80.0), size(rows, &style, scale));
        (rect, style, scale)
    }

    /// Rows do not overlap and each sits inside the window. The geometry is
    /// stated once and read by both the painter and the hit test, so this is
    /// a check on both.
    #[test]
    fn rows_tile_the_window_without_overlapping() {
        let rows = rows();
        let (rect, style, scale) = bounds(rows.len());
        let placed: Vec<Rect> = row_rects(rect, rows.len(), &style, scale).collect();
        assert_eq!(placed.len(), rows.len());
        for pair in placed.windows(2) {
            assert!(pair[0].max.y <= pair[1].min.y + 0.001);
        }
        for row in &placed {
            assert!(rect.contains_rect(*row), "{row:?} escapes {rect:?}");
        }
    }

    /// **The test this window exists to pass.** A click lands on the row it
    /// is over, and only when that row can actually be bought -- so the
    /// centre of the learnable row answers and the centres of the known and
    /// out-of-reach rows answer nothing.
    ///
    /// Both halves are asserted deliberately. A hit test that returned every
    /// row would pass the first half alone, and the failure it produces is
    /// the worst kind available here: a request the server declines in total
    /// silence, which is indistinguishable from a protocol bug.
    #[test]
    fn only_learnable_rows_answer_a_click() {
        let rows = rows();
        let (rect, style, scale) = bounds(rows.len());
        let centres: Vec<Pos2> = row_rects(rect, rows.len(), &style, scale)
            .map(|r| r.center())
            .collect();

        assert_eq!(row_at(rect, &rows, &style, scale, centres[0]), None, "known");
        assert_eq!(
            row_at(rect, &rows, &style, scale, centres[1]),
            Some(1),
            "available"
        );
        assert_eq!(
            row_at(rect, &rows, &style, scale, centres[2]),
            None,
            "out of reach"
        );
    }

    /// The index the hit test returns indexes the slice it was given, so the
    /// caller reads a spell id off it rather than counting. Guards against a
    /// future version that filters the inert rows out of the geometry and
    /// leaves the indices meaning something else.
    #[test]
    fn the_index_names_the_right_spell() {
        let rows = rows();
        let (rect, style, scale) = bounds(rows.len());
        let centre = row_rects(rect, rows.len(), &style, scale)
            .nth(1)
            .unwrap()
            .center();
        let index = row_at(rect, &rows, &style, scale, centre).unwrap();
        assert_eq!(rows[index].spell, 100);
    }

    /// A point outside the window is nobody's row.
    #[test]
    fn a_click_outside_hits_nothing() {
        let rows = rows();
        let (rect, style, scale) = bounds(rows.len());
        assert_eq!(
            row_at(rect, &rows, &style, scale, rect.min - Vec2::splat(5.0)),
            None
        );
    }

    /// An empty list still gets a window, because a trainer with nothing for
    /// this character still greets -- and a zero-height frame would read as a
    /// request that failed.
    #[test]
    fn an_empty_list_still_has_a_window() {
        let (rect, style, scale) = bounds(0);
        assert!(rect.height() > 0.0);
        assert_eq!(row_at(rect, &[], &style, scale, rect.center()), None);
    }

    #[test]
    fn money_reads_in_coins() {
        assert_eq!(money(9), "9c");
        assert_eq!(money(95), "95c");
        assert_eq!(money(1_234), "12s 34c");
        assert_eq!(money(1_020_304), "102g 3s 4c");
    }
}
