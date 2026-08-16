//! The questgiver window: what an NPC is offering, and the button that takes
//! it.
//!
//! **Its text comes from the quest cache, not from the questgiver's own
//! packets.** `CMSG_QUEST_QUERY` answers for any quest with title, objectives
//! and rewards, and the caller already keeps those per realm -- so
//! `SMSG_QUESTGIVER_QUEST_DETAILS` and `SMSG_QUESTGIVER_OFFER_REWARD` are used
//! as *events* ("this NPC is showing you quest 783", "the reward screen is
//! open") rather than as a second, independently-parsed copy of the same
//! strings. Two parses of the same content can drift; one cannot, and every
//! number this window shows has been checked against the realm's own tables.
//!
//! **Every button is drawn from one geometry function**, used by the painting
//! and by the hit test, exactly as the loot and spellbook frames do it. Here
//! the failure mode is worse than picking the wrong row: `Accept` and
//! `Decline` sit side by side, and a hit test that disagreed with the drawing
//! by a few pixels would decline quests the player meant to take.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// One quest in a questgiver's list.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestgiverRow {
    /// The quest id. Carried rather than derived from the row's position, the
    /// same reasoning a loot row carries its slot: the list is rebuilt
    /// whenever the NPC's offering changes.
    pub id: u32,
    pub title: String,
    /// `0` when the level is unknown -- drawn as no prefix rather than as
    /// `[0]`.
    pub level: i32,
    /// Whether this is a quest to hand *in* rather than take.
    pub turn_in: bool,
}

/// What the window's action button will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestgiverAction {
    /// The quest can be taken.
    Accept,
    /// Every objective is done and it can be handed in.
    Complete,
    /// It is in the log and unfinished -- there is nothing to press.
    Unfinished,
    /// The description has not arrived. **A state of its own**, because a
    /// window offering `Accept` under a blank body is asking the player to
    /// agree to something nobody has read to them.
    Waiting,
}

impl QuestgiverAction {
    /// The button's label, or `None` when there is no button.
    pub fn label(self) -> Option<&'static str> {
        match self {
            QuestgiverAction::Accept => Some("Accept"),
            QuestgiverAction::Complete => Some("Complete Quest"),
            QuestgiverAction::Unfinished | QuestgiverAction::Waiting => None,
        }
    }
}

/// What the window is showing.
#[derive(Debug, Clone, PartialEq)]
pub enum QuestgiverView {
    /// Several quests: pick one.
    List {
        npc: String,
        quests: Vec<QuestgiverRow>,
    },
    /// One quest's text and its action.
    Quest {
        id: u32,
        title: String,
        /// The questgiver's spoken text, or the completion text when handing
        /// in. Empty while the description is still being asked for.
        body: String,
        /// One line per objective, already rendered by the caller -- this
        /// crate knows nothing about creature ids or item entries.
        objectives: Vec<String>,
        rewards: Vec<String>,
        action: QuestgiverAction,
    },
}

/// Which button the user pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuestgiverClick {
    /// A row in the list was chosen, by quest id.
    pub picked: Option<u32>,
    /// The action button was pressed for this quest.
    pub acted: Option<u32>,
    /// The window was dismissed.
    pub closed: bool,
}

/// How wide the window is, and how tall a line of body text is.
fn line_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

fn button_height(style: &Style, scale: f32) -> f32 {
    style.spellbook_row * scale
}

/// The body text, wrapped to the window's width.
///
/// **Wrapped here rather than by egui**, because the height this returns has
/// to agree exactly with what the painter draws -- and a window sized from one
/// wrap and painted with another clips its own last paragraph. Crude on
/// purpose: it breaks on spaces at a character count derived from the font
/// size, which is enough for a proportional font at any scale and has no
/// second opinion about where the lines fall.
fn wrap(text: &str, style: &Style, scale: f32) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    // The average glyph in a proportional face is a little over half its
    // point size wide. Measured against the widest quest text in the starting
    // zone rather than derived: this is a layout constant, not a claim.
    let usable = (style.questgiver_width - style.padding * 2.0).max(1.0);
    let per_line = ((usable / (style.font_size * 0.52)).floor() as usize).max(8);
    let mut lines = Vec::new();
    // The server's own paragraph break, which appears verbatim in quest text.
    for paragraph in text.split("$B") {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > per_line {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    let _ = scale;
    lines
}

/// Every line the window will draw, in order, so the size and the painter
/// cannot disagree about how many there are.
fn body_lines(view: &QuestgiverView, style: &Style, scale: f32) -> Vec<String> {
    match view {
        QuestgiverView::List { quests, .. } => quests
            .iter()
            .map(|row| {
                let prefix = if row.turn_in { "? " } else { "! " };
                if row.level > 0 {
                    format!("{prefix}[{}] {}", row.level, row.title)
                } else {
                    format!("{prefix}{}", row.title)
                }
            })
            .collect(),
        QuestgiverView::Quest {
            body,
            objectives,
            rewards,
            action,
            ..
        } => {
            let mut lines = wrap(body, style, scale);
            if !objectives.is_empty() {
                lines.push(String::new());
                lines.push("Objectives:".into());
                for objective in objectives {
                    lines.extend(wrap(&format!("- {objective}"), style, scale));
                }
            }
            if !rewards.is_empty() {
                lines.push(String::new());
                lines.push("You will receive:".into());
                for reward in rewards {
                    lines.extend(wrap(&format!("- {reward}"), style, scale));
                }
            }
            // **Said out loud rather than left as a blank window.** A player
            // looking at an empty box cannot tell a quest with no text from a
            // client that is still asking.
            if matches!(action, QuestgiverAction::Waiting) && lines.is_empty() {
                lines.push("Asking the server what this quest is...".into());
            }
            if matches!(action, QuestgiverAction::Unfinished) {
                lines.push(String::new());
                lines.push("You are not finished yet.".into());
            }
            lines
        }
    }
}

/// How much room the window wants.
pub fn size(view: &QuestgiverView, style: &Style, scale: f32) -> Vec2 {
    let lines = body_lines(view, style, scale).len().max(1);
    let mut height = line_height(style, scale) // the title
        + lines as f32 * line_height(style, scale)
        + style.padding * 2.0 * scale;
    if has_buttons(view) {
        height += style.gap * scale + button_height(style, scale);
    }
    Vec2::new(style.questgiver_width * scale, height)
}

fn has_buttons(view: &QuestgiverView) -> bool {
    match view {
        // A list closes by picking something or by walking away; a row is not
        // a button, so the strip is not drawn.
        QuestgiverView::List { .. } => false,
        QuestgiverView::Quest { action, .. } => action.label().is_some(),
    }
}

/// Where the list's rows sit, when it is showing one.
///
/// The single source of truth for row geometry -- see the module comment.
pub fn row_rects(rect: Rect, rows: usize, style: &Style, scale: f32) -> Vec<Rect> {
    let pad = style.padding * scale;
    let line = line_height(style, scale);
    let top = rect.min.y + pad + line;
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    (0..rows)
        .map(|i| {
            Rect::from_min_size(
                Pos2::new(left, top + i as f32 * line),
                Vec2::new(width, line),
            )
        })
        .collect()
}

/// Where the action and dismiss buttons sit.
///
/// Returns `(action, dismiss)`. Both are produced together and by the same
/// arithmetic, because they are adjacent: a hit test that disagreed with the
/// drawing by a few pixels would decline quests the player meant to take.
pub fn button_rects(rect: Rect, style: &Style, scale: f32) -> (Rect, Rect) {
    let pad = style.padding * scale;
    let gap = style.gap * scale;
    let height = button_height(style, scale);
    let bottom = rect.max.y - pad;
    let width = ((rect.width() - pad * 2.0 - gap) * 0.5).max(0.0);
    let action = Rect::from_min_size(
        Pos2::new(rect.min.x + pad, bottom - height),
        Vec2::new(width, height),
    );
    let dismiss = Rect::from_min_size(
        Pos2::new(action.max.x + gap, bottom - height),
        Vec2::new(width, height),
    );
    (action, dismiss)
}

/// What a click at `point` means, if anything.
pub fn click_at(
    rect: Rect,
    view: &QuestgiverView,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> QuestgiverClick {
    let mut click = QuestgiverClick::default();
    match view {
        QuestgiverView::List { quests, .. } => {
            if let Some(index) =
                row_rects(rect, quests.len(), style, scale)
                    .into_iter()
                    .position(|row| row.contains(point))
            {
                click.picked = quests.get(index).map(|row| row.id);
            }
        }
        QuestgiverView::Quest { id, action, .. } => {
            if action.label().is_none() {
                return click;
            }
            let (accept, dismiss) = button_rects(rect, style, scale);
            if accept.contains(point) {
                click.acted = Some(*id);
            } else if dismiss.contains(point) {
                click.closed = true;
            }
        }
    }
    click
}

/// Paints the window.
pub fn draw(painter: &Painter, rect: Rect, view: &QuestgiverView, style: &Style, scale: f32) {
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

    let heading = match view {
        QuestgiverView::List { npc, .. } => npc.clone(),
        QuestgiverView::Quest { title, .. } => title.clone(),
    };
    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        heading,
        font.clone(),
        text,
    );

    let painter = painter.with_clip_rect(rect);
    let line = line_height(style, scale);
    let mut y = rect.min.y + pad + line;
    for body in body_lines(view, style, scale) {
        painter.text(
            Pos2::new(rect.min.x + pad, y + line * 0.5),
            Align2::LEFT_CENTER,
            body,
            font.clone(),
            dim,
        );
        y += line;
    }

    if let QuestgiverView::Quest { action, .. } = view {
        if let Some(label) = action.label() {
            let (accept, dismiss) = button_rects(rect, style, scale);
            for (bounds, label) in [(accept, label), (dismiss, "Close")] {
                painter.rect_filled(bounds, corner_radius(style.corner * scale * 0.5), style.spellbook_selected);
                painter.rect_stroke(
                    bounds,
                    corner_radius(style.corner * scale * 0.5),
                    Stroke::new(style.border_width * scale, style.border),
                    StrokeKind::Inside,
                );
                painter.text(bounds.center(), Align2::CENTER_CENTER, label, font.clone(), text);
            }
        }
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// Placeholder contents, so the window can be positioned with no NPC in front
/// of you.
pub fn placeholder() -> QuestgiverView {
    QuestgiverView::Quest {
        id: 783,
        title: "A Threat Within".into(),
        body: "I hope you strapped your belt on tight, young warrior, because \
               there is work to do here in Northshire."
            .into(),
        objectives: vec!["Speak with Marshal McBride.".into()],
        rewards: Vec::new(),
        action: QuestgiverAction::Accept,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quest(action: QuestgiverAction) -> QuestgiverView {
        QuestgiverView::Quest {
            id: 783,
            title: "A Threat Within".into(),
            body: "Some text.".into(),
            objectives: vec!["Speak with Marshal McBride.".into()],
            rewards: Vec::new(),
            action,
        }
    }

    /// **Accept and Close sit side by side**, so the hit test and the drawing
    /// have to agree exactly -- a few pixels of disagreement declines quests
    /// the player meant to take.
    #[test]
    fn the_action_button_and_the_close_button_do_not_overlap() {
        let style = Style::default();
        let view = quest(QuestgiverAction::Accept);
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        let (action, dismiss) = button_rects(rect, &style, 1.0);
        assert!(action.max.x <= dismiss.min.x, "{action:?} {dismiss:?}");
        assert_eq!(
            click_at(rect, &view, &style, 1.0, action.center()).acted,
            Some(783)
        );
        assert!(click_at(rect, &view, &style, 1.0, dismiss.center()).closed);
        // And the accept half must not also report a close.
        assert!(!click_at(rect, &view, &style, 1.0, action.center()).closed);
    }

    /// A quest already in the log and unfinished has nothing to press, and
    /// must not offer a button that would send a request the server refuses.
    #[test]
    fn an_unfinished_quest_has_no_button() {
        let style = Style::default();
        let view = quest(QuestgiverAction::Unfinished);
        assert_eq!(QuestgiverAction::Unfinished.label(), None);
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        // Nothing anywhere in the window reports an action.
        assert_eq!(click_at(rect, &view, &style, 1.0, rect.center()).acted, None);
    }

    /// **A window that has not been told what the quest is must not offer
    /// Accept.** Otherwise the player is asked to agree to something nobody
    /// has read to them.
    #[test]
    fn a_waiting_quest_says_so_and_offers_nothing() {
        let style = Style::default();
        let view = QuestgiverView::Quest {
            id: 783,
            title: "Quest 783".into(),
            body: String::new(),
            objectives: Vec::new(),
            rewards: Vec::new(),
            action: QuestgiverAction::Waiting,
        };
        assert_eq!(QuestgiverAction::Waiting.label(), None);
        let lines = body_lines(&view, &style, 1.0).join(" ");
        assert!(lines.contains("Asking the server"), "{lines}");
    }

    /// A list reports the quest **id** a row names, not its position -- the
    /// second row here, so a bug that always returned the first would fail.
    #[test]
    fn picking_a_row_reports_its_quest_id() {
        let style = Style::default();
        let view = QuestgiverView::List {
            npc: "Marshal McBride".into(),
            quests: vec![
                QuestgiverRow {
                    id: 7,
                    title: "Kobold Camp Cleanup".into(),
                    level: 1,
                    turn_in: false,
                },
                QuestgiverRow {
                    id: 783,
                    title: "A Threat Within".into(),
                    level: 1,
                    turn_in: true,
                },
            ],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        let rows = row_rects(rect, 2, &style, 1.0);
        assert_eq!(
            click_at(rect, &view, &style, 1.0, rows[1].center()).picked,
            Some(783)
        );
        assert_eq!(
            click_at(rect, &view, &style, 1.0, rows[0].center()).picked,
            Some(7)
        );
    }

    /// The window grows with its text, so a long quest is not clipped -- and
    /// the wrap the size was measured from is the wrap the painter uses,
    /// because there is only one.
    #[test]
    fn a_longer_quest_needs_a_taller_window() {
        let style = Style::default();
        let short = quest(QuestgiverAction::Accept);
        let mut long = quest(QuestgiverAction::Accept);
        if let QuestgiverView::Quest { body, .. } = &mut long {
            *body = "word ".repeat(200);
        }
        assert!(size(&long, &style, 1.0).y > size(&short, &style, 1.0).y);
    }

    /// The server's own paragraph marker is a break, not literal text -- quest
    /// bodies are full of `$B` and showing it would be nonsense on screen.
    #[test]
    fn the_paragraph_marker_becomes_a_break() {
        let style = Style::default();
        let lines = wrap("First.$B$BSecond.", &style, 1.0);
        assert!(
            !lines.iter().any(|line| line.contains("$B")),
            "{lines:?}"
        );
        assert!(lines.iter().any(|line| line.contains("First.")));
        assert!(lines.iter().any(|line| line.contains("Second.")));
    }
}
