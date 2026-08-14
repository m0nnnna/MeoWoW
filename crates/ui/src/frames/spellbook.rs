//! The spellbook: what the character can do, and where an action bar's
//! contents come from.
//!
//! Until this existed the bars were filled once at login by
//! `App::seed_action_bars` and could never be changed from inside the client --
//! which meant an ability the seeder's filter rejected was unreachable no
//! matter what the character knew. Auto-attack was exactly that: it sits on
//! `SkillLineAbility`'s generic line 183 with a class mask of zero, the same
//! bucket `Opening` and `Honorless Target` live in, so the filter that keeps a
//! warrior's bar free of junk necessarily rejected it too. A list you can drag
//! from is the general answer to that, where widening the filter is a guess
//! that readmits the junk.
//!
//! Two things here follow the rules the rest of this crate already follows.
//!
//! **The row geometry lives in one function.** [`row_rects`] is used by the
//! drawing *and* by the hit test, for the same reason [`super::action_bar`]'s
//! slots are: two copies agree until one changes, and the failure -- a click
//! that picks up the spell below the one you pressed -- reads as a targeting
//! bug rather than a layout one.
//!
//! **How many rows fit is measured from the rectangle, not from the style.**
//! The panel is scrollable, so the scroll offset has to be clamped against the
//! row count, and a count derived separately from the style would drift from
//! the one the drawing used the moment egui constrained the area. Everything
//! asks [`rows_in`].

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// One spell as the book lists it.
///
/// The same shape as an action slot's [`super::action_bar::SlotSpell`] minus
/// the cooldown, and for the same reason: this crate takes an already-uploaded
/// texture id and knows nothing about `Spell.dbc` or where the artwork came
/// from. Without a game installation there are no icons, and a row draws its
/// name alone.
#[derive(Debug, Clone, PartialEq)]
pub struct SpellbookEntry {
    pub id: u32,
    pub name: String,
    /// Empty for most spells -- most have no rank at all.
    pub rank: String,
    pub icon: Option<egui::TextureId>,
}

/// How much room the book wants.
pub fn size(style: &Style, scale: f32) -> Vec2 {
    Vec2::new(style.spellbook_width, style.spellbook_height) * scale
}

/// The strip along the top holding the title and the position indicator.
fn header_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

/// How many rows fit in a given rectangle.
///
/// The one measurement everything else is derived from -- see the module
/// comment for why this takes a rectangle rather than reading the style's
/// height.
pub fn rows_in(rect: Rect, style: &Style, scale: f32) -> usize {
    let row = style.spellbook_row * scale;
    if row <= 0.0 {
        return 0;
    }
    let available = rect.height() - style.padding * 2.0 * scale - header_height(style, scale);
    (available / row).floor().max(0.0) as usize
}

/// How many rows fit at a scale, for a caller with no rectangle yet.
///
/// Answered by measuring the rectangle [`size`] would produce rather than by a
/// second formula, so the scroll clamp cannot disagree with the drawing.
pub fn page_rows(style: &Style, scale: f32) -> usize {
    rows_in(
        Rect::from_min_size(Pos2::ZERO, size(style, scale)),
        style,
        scale,
    )
}

/// Where each visible row sits inside the panel.
///
/// The single source of truth for row geometry -- see the module comment.
pub fn row_rects(rect: Rect, style: &Style, scale: f32) -> impl Iterator<Item = Rect> + '_ {
    let pad = style.padding * scale;
    let row = style.spellbook_row * scale;
    let top = rect.min.y + pad + header_height(style, scale);
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    (0..rows_in(rect, style, scale))
        .map(move |i| Rect::from_min_size(Pos2::new(left, top + i as f32 * row), Vec2::new(width, row)))
}

/// Which visible row contains a point, if any.
///
/// A row *index into what is on screen*, not into the entry list: the caller
/// adds the scroll offset. Keeping the two separate is what stops a scrolled
/// book from picking up the wrong spell.
pub fn row_at(rect: Rect, style: &Style, scale: f32, point: Pos2) -> Option<usize> {
    row_rects(rect, style, scale).position(|row| row.contains(point))
}

/// The largest scroll offset that still shows a full page.
///
/// Scrolling past the end would leave a panel of blank rows and no way to tell
/// that from a book that failed to load.
pub fn max_scroll(entries: usize, rect: Rect, style: &Style, scale: f32) -> usize {
    entries.saturating_sub(rows_in(rect, style, scale))
}

/// Paints the book.
///
/// `scroll` is the index of the first entry shown and `selected` is the spell
/// currently held for assignment, drawn highlighted so it is obvious that a
/// click on a bar slot is going to do something.
pub fn draw(
    painter: &Painter,
    rect: Rect,
    entries: &[SpellbookEntry],
    scroll: usize,
    selected: Option<u32>,
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

    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        "Spellbook",
        font.clone(),
        text,
    );

    // Where in the list this page sits. A scrollable panel with no indicator
    // gives a reader no way to tell "nothing further down" from "scrolling is
    // broken", and this book is short enough that the whole answer fits in a
    // corner.
    let shown = rows_in(rect, style, scale);
    if entries.len() > shown {
        let last = (scroll + shown).min(entries.len());
        painter.text(
            Pos2::new(rect.max.x - pad, rect.min.y + pad),
            Align2::RIGHT_TOP,
            format!("{}-{} of {}", scroll + 1, last, entries.len()),
            font.clone(),
            style.slot_binding.into(),
        );
    }

    let painter = painter.with_clip_rect(rect);
    for (offset, bounds) in row_rects(rect, style, scale).enumerate() {
        let Some(entry) = entries.get(scroll + offset) else {
            break;
        };

        if selected == Some(entry.id) {
            painter.rect_filled(
                bounds,
                corner_radius(style.corner * scale * 0.5),
                style.spellbook_selected,
            );
        }

        // A square the height of the row, so the icon scales with the row
        // rather than with the action bar's slot size -- the two are
        // independently tunable and a book of bar-sized icons would not fit.
        let side = bounds.height() - style.border_width * 2.0 * scale;
        let icon_rect = Rect::from_min_size(
            bounds.min + Vec2::splat(style.border_width * scale),
            Vec2::splat(side.max(0.0)),
        );
        match entry.icon {
            Some(icon) => painter.image(
                icon,
                icon_rect,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            ),
            // No game data, or an icon that would not load. The square still
            // gets drawn so the names stay in a column.
            None => painter.rect_stroke(
                icon_rect,
                corner_radius(style.corner * scale * 0.5),
                Stroke::new(style.border_width * scale, style.slot_empty_border),
                StrokeKind::Inside,
            ),
        };

        let label = if entry.rank.is_empty() {
            entry.name.clone()
        } else {
            format!("{} ({})", entry.name, entry.rank)
        };
        painter.text(
            Pos2::new(icon_rect.max.x + style.gap * scale, bounds.center().y),
            Align2::LEFT_CENTER,
            label,
            font.clone(),
            text,
        );
    }
}

/// Draws the held spell against the cursor.
///
/// Deliberately follows the pointer rather than sitting in a corner: picking a
/// spell up and putting it down are one gesture, and an indicator somewhere
/// else on screen makes the second half feel like a separate command that
/// might not have registered.
pub fn draw_held(painter: &Painter, at: Pos2, entry: &SpellbookEntry, style: &Style, scale: f32) {
    let side = style.slot_size * scale * 0.75;
    let bounds = Rect::from_min_size(at + Vec2::splat(4.0 * scale), Vec2::splat(side));
    let corner = corner_radius(style.corner * scale * 0.5);
    painter.rect_filled(bounds, corner, style.slot_background);
    match entry.icon {
        Some(icon) => {
            painter.image(
                icon,
                bounds,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            let painter = painter.with_clip_rect(bounds);
            painter.text(
                bounds.center(),
                Align2::CENTER_CENTER,
                super::action_bar::abbreviate(&entry.name),
                FontId::proportional(style.font_size * scale * 0.85),
                style.text.into(),
            );
        }
    }
    painter.rect_stroke(
        bounds,
        corner,
        Stroke::new(style.border_width * scale, style.spellbook_selected),
        StrokeKind::Outside,
    );
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Placeholder contents, so the book can be positioned before a character has
/// logged in and learned anything.
pub fn placeholder() -> Vec<SpellbookEntry> {
    ["Auto Attack", "Heroic Strike", "Charge", "Battle Shout"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| SpellbookEntry {
            id: i as u32 + 1,
            name: name.to_string(),
            rank: String::new(),
            icon: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book_rect(style: &Style, scale: f32) -> Rect {
        Rect::from_min_size(Pos2::new(100.0, 100.0), size(style, scale))
    }

    /// The hit test and the drawing must use the same geometry, or a click
    /// picks up the spell below the one you pressed.
    #[test]
    fn every_row_is_clickable_at_its_own_centre() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            let rect = book_rect(&style, scale);
            for (index, row) in row_rects(rect, &style, scale).enumerate() {
                assert_eq!(
                    row_at(rect, &style, scale, row.center()),
                    Some(index),
                    "row {index} at scale {scale}"
                );
            }
        }
    }

    /// Rows must not overlap, and none may escape the panel -- a row drawn
    /// past the bottom edge is clipped away and looks like a spell the
    /// character does not know.
    #[test]
    fn rows_do_not_overlap_and_fit_the_panel() {
        let style = Style::default();
        let rect = book_rect(&style, 1.0);
        let rows: Vec<Rect> = row_rects(rect, &style, 1.0).collect();
        assert!(rows.len() >= 4, "the default book shows {} rows", rows.len());
        for pair in rows.windows(2) {
            assert!(pair[0].bottom() <= pair[1].top(), "{:?} overlaps {:?}", pair[0], pair[1]);
        }
        for row in &rows {
            assert!(rect.contains_rect(*row), "{row:?} escapes {rect:?}");
        }
    }

    /// The header belongs to nothing: a click on the title must not pick up
    /// the first spell.
    #[test]
    fn the_header_is_not_a_row() {
        let style = Style::default();
        let rect = book_rect(&style, 1.0);
        assert_eq!(row_at(rect, &style, 1.0, rect.min), None);
        assert_eq!(
            row_at(rect, &style, 1.0, Pos2::new(rect.center().x, rect.min.y + 1.0)),
            None
        );
    }

    #[test]
    fn scale_multiplies_the_whole_panel() {
        let style = Style::default();
        assert_eq!(size(&style, 2.0), size(&style, 1.0) * 2.0);
    }

    /// A page's worth of rows measured from the style has to be the number
    /// actually drawn, or the scroll clamp lets the list run off the end.
    #[test]
    fn a_page_is_as_many_rows_as_are_drawn() {
        let style = Style::default();
        for scale in [0.5, 1.0, 2.0] {
            assert_eq!(
                page_rows(&style, scale),
                row_rects(book_rect(&style, scale), &style, scale).count()
            );
        }
    }

    /// Scrolling stops where the last entry reaches the bottom of the panel,
    /// and a book that fits entirely does not scroll at all.
    #[test]
    fn scrolling_stops_at_the_last_full_page() {
        let style = Style::default();
        let rect = book_rect(&style, 1.0);
        let page = rows_in(rect, &style, 1.0);
        assert_eq!(max_scroll(page, rect, &style, 1.0), 0);
        assert_eq!(max_scroll(page - 1, rect, &style, 1.0), 0);
        assert_eq!(max_scroll(page + 3, rect, &style, 1.0), 3);
    }
}
