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

pub use super::bags::BagItem;

/// One plain speech line in a gossip menu -- "I'd like to browse your
/// goods.", or a custom NPC's own scripted choices. Not a quest: choosing one
/// answers `CMSG_GOSSIP_SELECT_OPTION` and, on a scripted NPC, very often
/// gets back a *new* menu rather than closing anything -- which is what
/// lets a multi-step gossip tree be clicked through one line at a time.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestgiverOption {
    /// **The server's own option id**, not a position in the list -- the
    /// same rule [`QuestgiverRow::id`] follows, and for the identical
    /// reason: a filtered menu leaves holes in the numbering.
    pub index: u32,
    pub message: String,
}

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
    /// A menu: speech lines to click through, quests to pick, or both --
    /// options drawn first, in the order this NPC sent them, quests after.
    List {
        npc: String,
        options: Vec<QuestgiverOption>,
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
        /// Rewards the quest gives unconditionally. A [`BagItem`] each, the
        /// same payload a bag square carries, so the hover tooltip is the one
        /// [`super::bags::hover_tooltip`] already draws -- name coloured by
        /// quality, stats, flavour text. The name is `Item {entry}` until the
        /// caller's `CMSG_ITEM_QUERY_SINGLE` is answered, exactly as a bag
        /// square is.
        rewards: Vec<BagItem>,
        /// Money the quest pays, in copper. `0` draws nothing.
        reward_money: u32,
        /// Optional rewards, of which exactly one is taken -- empty for a
        /// quest offering none. Choosing among these is what
        /// `foss-wow#141`'s predecessor ticket asked for: this window used
        /// to hand over `reward_choices[0]` unconditionally, silently
        /// wrong for any quest with more than one.
        reward_choices: Vec<BagItem>,
        /// Which of `reward_choices` is currently picked. Meaningless when
        /// `reward_choices` is empty; the caller is responsible for keeping
        /// it a valid index otherwise (see [`QuestgiverClick::chosen_reward`]).
        selected_reward: usize,
        action: QuestgiverAction,
    },
}

/// Which button the user pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuestgiverClick {
    /// A quest row in the list was chosen, by quest id.
    pub picked: Option<u32>,
    /// A speech option was chosen, by the **server's own option id** --
    /// never a row position. See [`QuestgiverOption::index`].
    pub chosen_option: Option<u32>,
    /// A reward-choice row was clicked, by index into
    /// [`QuestgiverView::Quest::reward_choices`] -- a row position rather
    /// than a server id, because unlike a gossip option or a quest row this
    /// list is never filtered: the caller sent every choice the quest has.
    pub chosen_reward: Option<usize>,
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

/// Where the hoverable reward rows sit inside [`body_lines`] -- a
/// `(first line index, count)` pair per section, or `None` when that section
/// is absent. The money line is deliberately not covered: it is not an item
/// and has no tooltip.
#[derive(Default, Clone, Copy)]
struct RewardRows {
    /// The unconditional "You will receive:" item rows.
    rewards: Option<(usize, usize)>,
    /// The "Choose one:" pick-one rows.
    choices: Option<(usize, usize)>,
}

/// One reward's text: `Name`, or `Name x3` for a stack.
fn reward_name(item: &BagItem) -> String {
    if item.count > 1 {
        format!("{} x{}", item.name, item.count)
    } else {
        item.name.clone()
    }
}

/// Every line the window will draw, in order, so the size and the painter
/// cannot disagree about how many there are.
fn body_lines(view: &QuestgiverView, style: &Style, scale: f32) -> Vec<String> {
    body_lines_and_choice_range(view, style, scale).0
}

/// [`body_lines`], plus where the reward rows sit within it -- see
/// [`RewardRows`].
///
/// **One function for both**, rather than two that have to agree on where
/// wrapped body text and a variable-length objective list push the reward
/// sections down. `click_at` and `reward_at` read the ranges; `body_lines`
/// (and so `size` and `draw`, which both call it) reads the lines -- the same
/// reasoning [`row_rects`] and [`button_rects`] already apply to fixed
/// geometry, extended to sections whose offset depends on how much text
/// precedes them.
fn body_lines_and_choice_range(
    view: &QuestgiverView,
    style: &Style,
    scale: f32,
) -> (Vec<String>, RewardRows) {
    match view {
        QuestgiverView::List { options, quests, .. } => (
            options
                .iter()
                .map(|option| option.message.clone())
                .chain(quests.iter().map(|row| {
                    let prefix = if row.turn_in { "? " } else { "! " };
                    if row.level > 0 {
                        format!("{prefix}[{}] {}", row.level, row.title)
                    } else {
                        format!("{prefix}{}", row.title)
                    }
                }))
                .collect(),
            RewardRows::default(),
        ),
        QuestgiverView::Quest {
            body,
            objectives,
            rewards,
            reward_money,
            reward_choices,
            selected_reward,
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
            // **One line per reward, never wrapped.** An item name never
            // approaches the wrap width, and a hoverable row has to stay
            // exactly one line so its rectangle -- from `row_rects`, the same
            // function `List` uses -- lands on the row it names rather than on
            // half of it.
            let reward_range = if rewards.is_empty() && *reward_money == 0 {
                None
            } else {
                lines.push(String::new());
                lines.push("You will receive:".into());
                let start = lines.len();
                for reward in rewards {
                    lines.push(format!("- {}", reward_name(reward)));
                }
                let range = (!rewards.is_empty()).then_some((start, rewards.len()));
                if *reward_money > 0 {
                    lines.push(format!("- {}", super::trainer::money(*reward_money)));
                }
                range
            };
            let choice_range = if reward_choices.is_empty() {
                None
            } else {
                lines.push(String::new());
                lines.push("Choose one:".into());
                let start = lines.len();
                for (index, choice) in reward_choices.iter().enumerate() {
                    let marker = if index == *selected_reward { ">" } else { " " };
                    lines.push(format!("{marker} {}", reward_name(choice)));
                }
                Some((start, reward_choices.len()))
            };
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
            (
                lines,
                RewardRows {
                    rewards: reward_range,
                    choices: choice_range,
                },
            )
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

/// **Every state gets a button strip, unconditionally.** This used to be
/// `false` for a `List` and for a `Quest` with nothing to press
/// (`Unfinished`, `Waiting`) -- on the theory that a list closes by picking
/// something or by walking away. In play that is exactly "stuck open with no
/// way to close it": a vendor or a gossip-only NPC opens a `List` with no
/// rows worth picking, and a quest still loading opens a `Waiting` view that
/// used to have no button at all. Reported back from live play, twice.
fn has_buttons(_view: &QuestgiverView) -> bool {
    true
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

/// Where the lone Close button sits when there is no action beside it to
/// share the row with -- a `List`, or a `Quest` whose action has no label
/// (`Unfinished`, `Waiting`). Full width rather than half of one, because
/// nothing else is drawn on this row.
pub fn close_only_rect(rect: Rect, style: &Style, scale: f32) -> Rect {
    let pad = style.padding * scale;
    let height = button_height(style, scale);
    let bottom = rect.max.y - pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    Rect::from_min_size(Pos2::new(rect.min.x + pad, bottom - height), Vec2::new(width, height))
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
        QuestgiverView::List { options, quests, .. } => {
            let total = options.len() + quests.len();
            if let Some(index) = row_rects(rect, total, style, scale)
                .into_iter()
                .position(|row| row.contains(point))
            {
                // Options are drawn first, so an index inside their range is
                // one of them; anything past it is a quest, offset back down
                // to a position in `quests`.
                if index < options.len() {
                    click.chosen_option = options.get(index).map(|option| option.index);
                } else {
                    click.picked = quests.get(index - options.len()).map(|row| row.id);
                }
            } else if close_only_rect(rect, style, scale).contains(point) {
                click.closed = true;
            }
        }
        QuestgiverView::Quest { id, action, .. } => {
            // Checked before the buttons: the choice rows sit in the body,
            // the buttons at the bottom, and the two never overlap, but a
            // reward still has to be pickable before the window has decided
            // whether `Complete` even has a label yet.
            if let (_, RewardRows { choices: Some((start, count)), .. }) =
                body_lines_and_choice_range(view, style, scale)
            {
                if let Some(row) = row_rects(rect, start + count, style, scale)
                    .get(start..)
                    .and_then(|rows| rows.iter().position(|row| row.contains(point)))
                {
                    click.chosen_reward = Some(row);
                    return click;
                }
            }
            if action.label().is_none() {
                if close_only_rect(rect, style, scale).contains(point) {
                    click.closed = true;
                }
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

/// Which reward item the pointer is over, if any -- an unconditional reward
/// or a pick-one choice. The caller feeds the result to
/// [`super::bags::hover_tooltip`], which is why this hands back a [`BagItem`]
/// rather than an index: the tooltip is the one bag squares already use.
pub fn reward_at<'v>(
    rect: Rect,
    view: &'v QuestgiverView,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<&'v BagItem> {
    let QuestgiverView::Quest {
        rewards,
        reward_choices,
        ..
    } = view
    else {
        return None;
    };
    let (lines, ranges) = body_lines_and_choice_range(view, style, scale);
    let rects = row_rects(rect, lines.len(), style, scale);
    let hit = |range: Option<(usize, usize)>, items: &'v [BagItem]| {
        let (start, count) = range?;
        (0..count)
            .find(|&i| rects.get(start + i).is_some_and(|r| r.contains(point)))
            .and_then(|i| items.get(i))
    };
    hit(ranges.rewards, rewards).or_else(|| hit(ranges.choices, reward_choices))
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
    let (lines, ranges) = body_lines_and_choice_range(view, style, scale);
    // A reward row is drawn in its item's quality colour, so a rare drop
    // reads as one at a glance; everything else stays in the dim body shade.
    let quality_at = |index: usize| -> Option<Color32> {
        let QuestgiverView::Quest { rewards, reward_choices, .. } = view else {
            return None;
        };
        let pick = |range: Option<(usize, usize)>, items: &[BagItem]| {
            let (start, count) = range?;
            (index >= start && index < start + count)
                .then(|| items.get(index - start))
                .flatten()
                .map(|item| super::bags::quality_color(item.quality))
        };
        pick(ranges.rewards, rewards).or_else(|| pick(ranges.choices, reward_choices))
    };
    for (index, body) in lines.iter().enumerate() {
        painter.text(
            Pos2::new(rect.min.x + pad, y + line * 0.5),
            Align2::LEFT_CENTER,
            body,
            font.clone(),
            quality_at(index).unwrap_or(dim),
        );
        y += line;
    }

    let buttons: Vec<(Rect, &str)> = match view {
        QuestgiverView::Quest { action, .. } if action.label().is_some() => {
            let (accept, dismiss) = button_rects(rect, style, scale);
            // `action.label()` was just checked `Some`, so this cannot panic.
            vec![(accept, action.label().unwrap()), (dismiss, "Close")]
        }
        // A `List`, or a `Quest` with nothing to press: one full-width Close
        // button, so there is always a way out of the window.
        _ => vec![(close_only_rect(rect, style, scale), "Close")],
    };
    for (bounds, label) in buttons {
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
        reward_money: 0,
        reward_choices: Vec::new(),
        selected_reward: 0,
        action: QuestgiverAction::Accept,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bag_item(name: &str, count: u32) -> BagItem {
        BagItem {
            name: name.into(),
            count,
            ..Default::default()
        }
    }

    fn quest(action: QuestgiverAction) -> QuestgiverView {
        QuestgiverView::Quest {
            id: 783,
            title: "A Threat Within".into(),
            body: "Some text.".into(),
            objectives: vec!["Speak with Marshal McBride.".into()],
            rewards: Vec::new(),
            reward_money: 0,
            reward_choices: Vec::new(),
            selected_reward: 0,
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
    /// must not offer a button that would send a request the server refuses
    /// -- but it must still be closable, or the window is stuck open until
    /// the quest is finished.
    #[test]
    fn an_unfinished_quest_has_no_action_but_can_be_closed() {
        let style = Style::default();
        let view = quest(QuestgiverAction::Unfinished);
        assert_eq!(QuestgiverAction::Unfinished.label(), None);
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        // Nothing anywhere in the window reports an action.
        assert_eq!(click_at(rect, &view, &style, 1.0, rect.center()).acted, None);
        let close = close_only_rect(rect, &style, 1.0);
        assert!(click_at(rect, &view, &style, 1.0, close.center()).closed);
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
            reward_money: 0,
            reward_choices: Vec::new(),
            selected_reward: 0,
            action: QuestgiverAction::Waiting,
        };
        assert_eq!(QuestgiverAction::Waiting.label(), None);
        let lines = body_lines(&view, &style, 1.0).join(" ");
        assert!(lines.contains("Asking the server"), "{lines}");
        // **Reported from live play: a window stuck on "Asking the
        // server..." had no way to close it.** Whatever is holding the
        // reply up -- a lost packet, a realm that will not answer -- the
        // player must still be able to walk away from the conversation.
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        let close = close_only_rect(rect, &style, 1.0);
        assert!(click_at(rect, &view, &style, 1.0, close.center()).closed);
    }

    /// A list reports the quest **id** a row names, not its position -- the
    /// second row here, so a bug that always returned the first would fail.
    #[test]
    fn picking_a_row_reports_its_quest_id() {
        let style = Style::default();
        let view = QuestgiverView::List {
            npc: "Marshal McBride".into(),
            options: Vec::new(),
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

    /// **A list with nothing pickable must still close.** A pure vendor or a
    /// gossip-only NPC opens a `List` with zero quests and no rows worth
    /// picking -- reported from live play as a window that opened and then
    /// could not be gotten rid of.
    #[test]
    fn an_empty_list_can_still_be_closed() {
        let style = Style::default();
        let view = QuestgiverView::List {
            npc: "A Vendor".into(),
            options: Vec::new(),
            quests: Vec::new(),
        };
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        let close = close_only_rect(rect, &style, 1.0);
        assert!(click_at(rect, &view, &style, 1.0, close.center()).closed);
        // And a list that *does* have rows is closable too, without the
        // close button being confused for one of them.
        let view = QuestgiverView::List {
            npc: "Marshal McBride".into(),
            options: Vec::new(),
            quests: vec![QuestgiverRow {
                id: 7,
                title: "Kobold Camp Cleanup".into(),
                level: 1,
                turn_in: false,
            }],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        let close = close_only_rect(rect, &style, 1.0);
        let click = click_at(rect, &view, &style, 1.0, close.center());
        assert!(click.closed);
        assert_eq!(click.picked, None);
    }

    /// **Reported from live play**: a gossip menu's speech lines -- "I'd
    /// like to browse your goods.", or a custom scripted NPC's own choices
    /// -- were parsed and simply never drawn. Options are drawn first, so
    /// this is the test that the row-index math correctly offsets into
    /// `quests` once it walks past them, using the server's own ids for
    /// both rather than positions in either list.
    #[test]
    fn options_and_quests_share_one_list_and_report_their_own_ids() {
        let style = Style::default();
        let view = QuestgiverView::List {
            npc: "Farley".into(),
            options: vec![
                QuestgiverOption {
                    index: 1,
                    message: "I'd like to browse your goods.".into(),
                },
                QuestgiverOption {
                    index: 2,
                    message: "Can I rent a room?".into(),
                },
            ],
            quests: vec![QuestgiverRow {
                id: 333,
                title: "Harlan Needs a Resupply".into(),
                level: 1,
                turn_in: false,
            }],
        };
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        let rows = row_rects(rect, 3, &style, 1.0);

        let first = click_at(rect, &view, &style, 1.0, rows[0].center());
        assert_eq!(first.chosen_option, Some(1));
        assert_eq!(first.picked, None);

        let second = click_at(rect, &view, &style, 1.0, rows[1].center());
        assert_eq!(second.chosen_option, Some(2));

        let third = click_at(rect, &view, &style, 1.0, rows[2].center());
        assert_eq!(third.picked, Some(333));
        assert_eq!(third.chosen_option, None);
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

    fn quest_with_choices(choices: &[&str], selected_reward: usize) -> QuestgiverView {
        let QuestgiverView::Quest {
            id,
            title,
            body,
            objectives,
            rewards,
            reward_money,
            action,
            ..
        } = quest(QuestgiverAction::Complete)
        else {
            unreachable!()
        };
        QuestgiverView::Quest {
            id,
            title,
            body,
            objectives,
            rewards,
            reward_money,
            reward_choices: choices.iter().map(|name| bag_item(name, 1)).collect(),
            selected_reward,
            action,
        }
    }

    /// **`foss-wow#141`'s predecessor ticket: this window had no way to say
    /// which reward the player wanted at all**, so `Complete` always sent
    /// index `0`. A row reports its **position** in `reward_choices`, not a
    /// server id -- unlike a gossip option or a quest row, this list is
    /// never filtered, so a position is exactly what the caller needs to
    /// send back.
    #[test]
    fn picking_a_reward_choice_reports_its_row_position() {
        let style = Style::default();
        let view = quest_with_choices(&["Elwynn Longsword", "Ironforge Breastplate"], 0);
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        let RewardRows { choices: Some((start, count)), .. } =
            body_lines_and_choice_range(&view, &style, 1.0).1
        else {
            panic!("expected a choice range");
        };
        assert_eq!(count, 2);
        let rows = row_rects(rect, start + count, &style, 1.0);
        assert_eq!(
            click_at(rect, &view, &style, 1.0, rows[start].center()).chosen_reward,
            Some(0)
        );
        assert_eq!(
            click_at(rect, &view, &style, 1.0, rows[start + 1].center()).chosen_reward,
            Some(1)
        );
        // A click on the action button is still the action button, not a
        // stray third reward row -- the two regions must not bleed together.
        let (accept, _) = button_rects(rect, &style, 1.0);
        let accept_click = click_at(rect, &view, &style, 1.0, accept.center());
        assert_eq!(accept_click.chosen_reward, None);
        assert_eq!(accept_click.acted, Some(783));
    }

    /// A quest with nothing to choose between draws no picker at all --
    /// `reward_choices` empty is a fact about the quest, not a choice with
    /// one option forced.
    #[test]
    fn no_choices_means_no_picker() {
        let style = Style::default();
        let view = quest_with_choices(&[], 0);
        assert!(body_lines_and_choice_range(&view, &style, 1.0).1.choices.is_none());
        let lines = body_lines(&view, &style, 1.0).join(" ");
        assert!(!lines.contains("Choose one"), "{lines}");
    }

    /// The selected row is marked in the text itself -- this window has no
    /// other way to show state -- and only the selected one carries it.
    #[test]
    fn only_the_selected_reward_is_marked() {
        let style = Style::default();
        let view = quest_with_choices(&["Elwynn Longsword", "Ironforge Breastplate"], 1);
        let lines = body_lines(&view, &style, 1.0);
        let marked: Vec<_> = lines.iter().filter(|line| line.starts_with('>')).collect();
        assert_eq!(marked.len(), 1, "{lines:?}");
        assert!(marked[0].contains("Ironforge Breastplate"), "{marked:?}");
    }

    /// **The bug this milestone is about**: a reward drew as `item 2224` with
    /// nothing on hover. The line now carries the resolved name, and
    /// `reward_at` maps the row back to its `BagItem` so the caller can raise
    /// the same tooltip a bag square gets -- for an unconditional reward and
    /// for a pick-one choice alike.
    #[test]
    fn a_reward_row_shows_its_name_and_is_hoverable() {
        let style = Style::default();
        let QuestgiverView::Quest {
            id, title, body, objectives, action, ..
        } = quest(QuestgiverAction::Complete) else {
            unreachable!()
        };
        let view = QuestgiverView::Quest {
            id,
            title,
            body,
            objectives,
            rewards: vec![bag_item("Worn Shortsword", 1), bag_item("Minor Healing Potion", 5)],
            reward_money: 12_345,
            reward_choices: vec![bag_item("Recruit's Shirt", 1)],
            selected_reward: 0,
            action,
        };
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));

        let joined = body_lines(&view, &style, 1.0).join("\n");
        assert!(joined.contains("- Worn Shortsword"), "{joined}");
        assert!(joined.contains("- Minor Healing Potion x5"), "{joined}");
        // Money reads in coins, not a raw copper count.
        assert!(joined.contains("1g 23s 45c"), "{joined}");

        let (_, ranges) = body_lines_and_choice_range(&view, &style, 1.0);
        let (rstart, rcount) = ranges.rewards.expect("reward rows");
        assert_eq!(rcount, 2, "the money line is not a hoverable reward row");
        let rows = row_rects(rect, body_lines(&view, &style, 1.0).len(), &style, 1.0);
        assert_eq!(
            reward_at(rect, &view, &style, 1.0, rows[rstart].center()).map(|i| i.name.as_str()),
            Some("Worn Shortsword")
        );
        assert_eq!(
            reward_at(rect, &view, &style, 1.0, rows[rstart + 1].center()).map(|i| i.name.as_str()),
            Some("Minor Healing Potion")
        );
        // The money line sits right after the two item rows and is not one.
        assert_eq!(reward_at(rect, &view, &style, 1.0, rows[rstart + 2].center()), None);

        let (cstart, _) = ranges.choices.expect("choice rows");
        assert_eq!(
            reward_at(rect, &view, &style, 1.0, rows[cstart].center()).map(|i| i.name.as_str()),
            Some("Recruit's Shirt")
        );
    }
}
