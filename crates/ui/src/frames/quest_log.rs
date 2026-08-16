//! The quest log: what the character has taken on, and what each one wants.
//!
//! **Every line here is a fact the server was asked for.** This client ships
//! nobody's quest database, so a title and an objective line are not looked up
//! in a table -- they arrive in answer to `CMSG_QUEST_QUERY` and are cached per
//! realm. That has a consequence this frame has to draw honestly: for a moment
//! after login, or forever if the realm never answers, a quest in the log is a
//! number and nothing else.
//!
//! So an entry knows the difference between *waiting* and *empty*. A quest
//! whose description has not arrived says so; it does not render as a quest
//! with no objectives, which is a real and different thing. Collapsing the two
//! is the same mistake as an absent update field reading as unknown rather
//! than as zero, and it costs the same way -- silently, and in the direction
//! of looking finished.
//!
//! **The rows are the geometry's single source of truth**, used by the drawing
//! and by the hit test, exactly as the loot and spellbook frames do it. Two
//! copies agree until one changes, and here the failure is selecting the quest
//! below the one that was clicked.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// How much the server has managed to say about one quest in the log.
///
/// Three states rather than an `Option<String>`, because the third one --
/// asked and not answered -- has to be visible. A player looking at a blank
/// line deserves to know whether the quest has no objectives or whether the
/// client is still waiting, and only one of those is worth waiting through.
#[derive(Debug, Clone, PartialEq)]
pub enum QuestDetail {
    /// The server described it.
    Known {
        title: String,
        /// The one-line "what to do", which may be empty for a quest that
        /// genuinely has none.
        objective: String,
        level: i32,
    },
    /// Asked, still waiting.
    Waiting,
    /// Asked, and the server said nothing. **Not** the same as a quest with no
    /// content, and drawn differently so nobody has to guess.
    Unanswered,
}

/// One quest in the log.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestLogEntry {
    /// The quest id out of `PLAYER_QUEST_LOG`. Always known -- it is the one
    /// thing the log field carries directly -- which is why it is a field
    /// rather than part of [`QuestDetail`].
    pub id: u32,
    pub detail: QuestDetail,
    /// Whether every objective is done, read out of the quest log's own state
    /// field rather than inferred from the objectives -- the client cannot
    /// count what a creature drop is worth, and the server already knows.
    pub complete: bool,
}

impl QuestLogEntry {
    /// What to draw on the row.
    ///
    /// **The id is shown whatever else is known**, and never replaced by a
    /// placeholder title. A player who can see `[783]` can report it; one
    /// looking at `Unknown quest` cannot.
    fn label(&self) -> String {
        match &self.detail {
            // **The marker goes on whatever is known.** A quest whose
            // description has not arrived can still be complete, and hiding
            // that until the text loads would tell the player there is
            // nothing to hand in.
            QuestDetail::Known { title, level, .. } if *level > 0 => {
                format!("[{level}] {title}{}", self.done_marker())
            }
            QuestDetail::Known { title, .. } => format!("{title}{}", self.done_marker()),
            QuestDetail::Waiting => {
                format!("Quest {} -- asking the server...{}", self.id, self.done_marker())
            }
            QuestDetail::Unanswered => format!(
                "Quest {} -- no answer from the realm{}",
                self.id,
                self.done_marker()
            ),
        }
    }

    fn done_marker(&self) -> &'static str {
        if self.complete {
            " (Complete)"
        } else {
            ""
        }
    }

    /// The second line, or nothing.
    fn objective(&self) -> Option<&str> {
        match &self.detail {
            QuestDetail::Known { objective, .. } if !objective.is_empty() => Some(objective),
            _ => None,
        }
    }

    /// Whether this row is a statement about the client rather than about the
    /// quest, which is drawn dimmer so it does not read as content.
    fn is_placeholder(&self) -> bool {
        !matches!(self.detail, QuestDetail::Known { .. })
    }
}

/// How tall one entry is: a title line, plus an objective line when there is
/// one.
fn entry_height(entry: &QuestLogEntry, style: &Style, scale: f32) -> f32 {
    let row = style.spellbook_row * scale;
    if entry.objective().is_some() {
        row + (style.font_size + style.gap) * scale
    } else {
        row
    }
}

/// How much room the window wants.
///
/// Sized to its contents like the loot window rather than to a fixed height: a
/// character on one quest should not open a panel with twenty-four blank
/// lines, which reads as a log that failed to load.
pub fn size(entries: &[QuestLogEntry], style: &Style, scale: f32) -> Vec2 {
    let content: f32 = entries
        .iter()
        .map(|entry| entry_height(entry, style, scale))
        .sum();
    // A minimum of one row, so an empty log is still a window that can be
    // seen, moved and closed rather than a sliver.
    let content = content.max(style.spellbook_row * scale);
    let height = header_height(style, scale) + content + style.padding * 2.0 * scale;
    Vec2::new(style.quest_log_width * scale, height)
}

fn header_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

/// Where each entry sits.
///
/// The single source of truth for row geometry -- see the module comment.
/// Entries are not all the same height, so this accumulates rather than
/// multiplying by an index, and anything that needs a row's position has to
/// come through here.
pub fn entry_rects(
    rect: Rect,
    entries: &[QuestLogEntry],
    style: &Style,
    scale: f32,
) -> Vec<Rect> {
    let pad = style.padding * scale;
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    let mut top = rect.min.y + pad + header_height(style, scale);
    entries
        .iter()
        .map(|entry| {
            let height = entry_height(entry, style, scale);
            let bounds = Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, height));
            top += height;
            bounds
        })
        .collect()
}

/// Which entry contains a point, if any.
pub fn entry_at(
    rect: Rect,
    entries: &[QuestLogEntry],
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<usize> {
    entry_rects(rect, entries, style, scale)
        .into_iter()
        .position(|bounds| bounds.contains(point))
}

/// Paints the window.
pub fn draw(
    painter: &Painter,
    rect: Rect,
    entries: &[QuestLogEntry],
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
    let dim: Color32 = style.quest_dim.into();
    let pad = style.padding * scale;
    let font = FontId::proportional(style.font_size * scale);
    let small = FontId::proportional(style.font_size * 0.85 * scale);

    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        // The count is part of the title because "how many am I carrying" is
        // the first question anyone opens this to answer, and the log has a
        // hard limit of twenty-five.
        format!("Quest Log ({})", entries.len()),
        font.clone(),
        text,
    );

    let painter = painter.with_clip_rect(rect);
    let bounds = entry_rects(rect, entries, style, scale);
    for (entry, bounds) in entries.iter().zip(bounds) {
        if selected == Some(entry.id) {
            painter.rect_filled(bounds, corner_radius(style.corner * scale * 0.5), style.spellbook_selected);
        }
        let colour = if entry.is_placeholder() { dim } else { text };
        painter.text(
            Pos2::new(bounds.min.x, bounds.min.y + style.spellbook_row * scale * 0.5),
            Align2::LEFT_CENTER,
            entry.label(),
            font.clone(),
            colour,
        );
        if let Some(objective) = entry.objective() {
            painter.text(
                Pos2::new(
                    bounds.min.x + style.gap * scale * 2.0,
                    bounds.min.y + style.spellbook_row * scale + (style.font_size * 0.5) * scale,
                ),
                Align2::LEFT_CENTER,
                objective,
                small.clone(),
                dim,
            );
        }
    }

    if entries.is_empty() {
        painter.text(
            Pos2::new(
                rect.min.x + pad,
                rect.min.y + pad + header_height(style, scale) + style.spellbook_row * scale * 0.5,
            ),
            Align2::LEFT_CENTER,
            "No quests.",
            font,
            dim,
        );
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Placeholder contents, so the window can be positioned with an empty log.
///
/// **Shows all three detail states**, because the point of positioning a
/// window in edit mode is seeing how big it gets, and a log of three tidy
/// known quests is not the worst case.
pub fn placeholder() -> Vec<QuestLogEntry> {
    vec![
        QuestLogEntry {
            id: 783,
            detail: QuestDetail::Known {
                title: "A Threat Within".into(),
                objective: "Speak with Marshal McBride.".into(),
                level: 1,
            },
            complete: true,
        },
        QuestLogEntry {
            id: 16,
            detail: QuestDetail::Waiting,
            complete: false,
        },
        QuestLogEntry {
            id: 106,
            detail: QuestDetail::Unanswered,
            complete: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(id: u32, objective: &str) -> QuestLogEntry {
        QuestLogEntry {
            id,
            detail: QuestDetail::Known {
                title: "Title".into(),
                objective: objective.into(),
                level: 5,
            },
            complete: false,
        }
    }

    /// **The distinction this frame exists to draw.** A quest still being
    /// asked about must not produce the same row as one with no objectives:
    /// the second is a fact about the quest and the first is a fact about the
    /// client, and only one of them means "nothing more to do here".
    #[test]
    fn waiting_and_empty_do_not_render_the_same() {
        let waiting = QuestLogEntry {
            id: 783,
            detail: QuestDetail::Waiting,
            complete: false,
        };
        let empty = known(783, "");
        assert_ne!(waiting.label(), empty.label());
        assert!(waiting.is_placeholder());
        assert!(!empty.is_placeholder());
    }

    /// And an unanswered quest is a third thing again -- the realm was asked
    /// and would not say, which is worth telling the player because it will
    /// not resolve by waiting.
    #[test]
    fn unanswered_is_not_the_same_as_waiting() {
        let waiting = QuestLogEntry {
            id: 783,
            detail: QuestDetail::Waiting,
            complete: false,
        };
        let unanswered = QuestLogEntry {
            id: 783,
            detail: QuestDetail::Unanswered,
            complete: false,
        };
        assert_ne!(waiting.label(), unanswered.label());
    }

    /// The id survives every state, so a player can always report which quest
    /// a broken row is about.
    #[test]
    fn the_id_is_always_visible_when_nothing_else_is() {
        for detail in [QuestDetail::Waiting, QuestDetail::Unanswered] {
            let entry = QuestLogEntry {
                id: 4242,
                detail,
                complete: false,
            };
            assert!(entry.label().contains("4242"));
        }
    }

    /// A finished quest says so, and an unfinished one does not -- asserting
    /// only the first would pass if every row claimed to be complete.
    #[test]
    fn only_a_finished_quest_is_marked_complete() {
        let mut done = known(1, "objective");
        done.complete = true;
        let not_done = known(1, "objective");
        assert!(done.label().contains("Complete"));
        assert!(!not_done.label().contains("Complete"));
    }

    /// **A quest can be complete before its description arrives**, and the
    /// marker has to survive that -- otherwise a player with a finished quest
    /// and a slow realm is told there is nothing to hand in.
    #[test]
    fn a_waiting_row_can_still_be_marked_complete() {
        let entry = QuestLogEntry {
            id: 783,
            detail: QuestDetail::Waiting,
            complete: true,
        };
        assert!(entry.label().contains("Complete"));
    }

    /// Rows of different heights must not overlap, and the hit test must agree
    /// with the drawing about where each one is -- the failure otherwise is
    /// selecting the quest below the one that was clicked.
    #[test]
    fn entries_tile_without_overlapping_and_hit_test_agrees() {
        let style = Style::default();
        let entries = vec![
            known(1, "has an objective line"),
            known(2, ""),
            known(3, "and another"),
        ];
        let rect = Rect::from_min_size(Pos2::ZERO, size(&entries, &style, 1.0));
        let rects = entry_rects(rect, &entries, &style, 1.0);
        assert_eq!(rects.len(), 3);
        for pair in rects.windows(2) {
            assert!(
                (pair[0].max.y - pair[1].min.y).abs() < 0.01,
                "rows must meet exactly, not overlap or leave a gap"
            );
        }
        // Every row's own centre must hit that row and no other.
        for (index, bounds) in rects.iter().enumerate() {
            assert_eq!(
                entry_at(rect, &entries, &style, 1.0, bounds.center()),
                Some(index)
            );
        }
    }

    /// The window grows with its contents, and an entry with a second line is
    /// taller than one without -- otherwise the objective would be drawn
    /// outside the row the hit test believes in.
    #[test]
    fn an_objective_line_makes_the_entry_taller() {
        let style = Style::default();
        let with = vec![known(1, "objective")];
        let without = vec![known(1, "")];
        assert!(size(&with, &style, 1.0).y > size(&without, &style, 1.0).y);
    }

    /// An empty log is still a window big enough to see and move.
    #[test]
    fn an_empty_log_still_has_a_window() {
        let style = Style::default();
        let empty = size(&[], &style, 1.0);
        assert!(empty.y > style.padding * 2.0);
        assert!(empty.x > 0.0);
    }
}
