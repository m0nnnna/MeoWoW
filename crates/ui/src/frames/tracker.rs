//! The objective tracker: what to do, how far along, and how far away.
//!
//! **The frame that makes a quest log into a quest tracker.** A log is a thing
//! you open, read and close; a tracker is a thing that is simply there, and
//! the difference is not cosmetic. The whole reason an addon like Questie
//! exists is that the question *"what am I supposed to be doing"* is asked
//! constantly and answering it should not cost a keypress.
//!
//! ## It owns no facts
//!
//! Every line here is assembled by the caller out of things that were already
//! being drawn somewhere else: the title and objective text come from the
//! quest cache, the counters come from the player's own quest-log fields and
//! bags, and the distance comes from the objective polygons
//! `CMSG_QUEST_POI_QUERY` already answered. **This frame is a second view of
//! the same data, never a second copy of it** -- two copies agree until one of
//! them changes, which is the rule the picking ray and the map projection are
//! both written to.
//!
//! ## It is a window onto a longer list, and it says so
//!
//! A quest log holds twenty-five and a tracker that drew all of them would
//! cover the screen, so it draws the first few. **That is exactly the shape
//! the auction window had to learn to admit**: fifty real rows out of 1,284
//! real rows look precisely like the whole answer, and a line that appears
//! only when there is a surplus is a line nobody has learned to read. So the
//! header states the count in every state, including the uninteresting ones.
//!
//! ## What it will not say
//!
//! **No direction, no arrow, and no distance it has not been given.** A
//! distance comes from a marker the server sent for a quest actually in the
//! log; a quest with no markers gets no number rather than a guess from its
//! objective text. That is the same call `describe_cast_failure` makes about
//! naming a status code and the auction window makes about sorting fifty rows
//! of 1,284: an honest absence beats a plausible wrong answer, because a
//! number nobody can check is worse than a blank.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::Style;

/// How hard a quest is for the character reading it.
///
/// **The grey band is the server's word and the rest are this client's**, and
/// the split is deliberate rather than tidy.
///
/// `SMSG_QUESTGIVER_STATUS` distinguishes a bright exclamation from a grey one
/// -- [`world::quest::QuestgiverMark::AvailableTrivial`] -- so *where a quest
/// stops being worth doing* is a judgement the realm has already made and
/// this client can simply report. Every other boundary is presentation: the
/// original interface colours a quest four more ways by how its level compares
/// to the player's, and those thresholds are not on the wire, not in a DBC
/// this project has read, and not verifiable here.
///
/// So they are named as what they are -- a presentation choice made in one
/// place, stated in [`Difficulty::of`], and easy to find and correct -- rather
/// than transcribed from memory and presented as a fact. The rule this follows
/// is the one about not transcribing a table you have not verified, especially
/// one that only produces text; the concession is that a colour is not a
/// number, so a wrong band is misleading rather than believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Difficulty {
    /// Well above the character. The original interface draws this red.
    VeryHard,
    Hard,
    /// About right.
    Even,
    Easy,
    /// Below the level the *server* calls trivial, when the server has said
    /// so, and below this client's own green band when it has not.
    Trivial,
    /// The quest's level is not known, or scales to the player (`-1` on the
    /// wire). Drawn in the ordinary text colour, which says nothing.
    Unknown,
}

impl Difficulty {
    /// Which band a quest falls in for a character.
    ///
    /// The thresholds are `+5`, `+3` and `-2` relative to the player, and the
    /// grey boundary is the caller's `trivial` -- pass what the server said if
    /// anything did, and `false` otherwise. See the type's own comment for why
    /// those two halves are kept apart.
    pub fn of(quest_level: i32, player_level: u32, trivial: bool) -> Self {
        if trivial {
            return Self::Trivial;
        }
        // `-1` is "scales to the player" and `0` is a quest whose level this
        // client has not been told. Neither is a comparison anybody can make.
        if quest_level <= 0 || player_level == 0 {
            return Self::Unknown;
        }
        let player = player_level as i32;
        if quest_level >= player + 5 {
            Self::VeryHard
        } else if quest_level >= player + 3 {
            Self::Hard
        } else if quest_level >= player - 2 {
            Self::Even
        } else {
            Self::Easy
        }
    }

    fn colour(self, style: &Style) -> Color32 {
        match self {
            Self::VeryHard => style.quest_very_hard.into(),
            Self::Hard => style.quest_hard.into(),
            Self::Even => style.quest_even.into(),
            Self::Easy => style.quest_easy.into(),
            Self::Trivial => style.quest_trivial.into(),
            Self::Unknown => style.text.into(),
        }
    }
}

/// One quest on the tracker.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackedQuest {
    pub id: u32,
    /// The quest's own name, or a statement about why there is none.
    ///
    /// **Never a made-up title.** A quest whose description has not arrived
    /// draws as its id, which a player can report; a placeholder cannot be.
    pub title: String,
    /// One line per counted objective, already formatted -- `Kobold Vermin:
    /// 4/8`. Formatted by the caller for the same reason the quest log's are:
    /// how far along an objective is comes from the player's own fields and
    /// bags, and this crate can see neither.
    pub progress: Vec<String>,
    /// Whether every objective is done, off the log's own state field.
    pub complete: bool,
    pub difficulty: Difficulty,
    /// The quest's level, drawn in brackets when it is known. `None` for a
    /// quest that has not been described, or one that scales.
    pub level: Option<i32>,
    /// Yards to the nearest objective marker the realm gave for this quest, or
    /// `None` when it gave none. See the module comment: no marker, no number.
    pub distance: Option<f32>,
}

impl TrackedQuest {
    /// The title line.
    fn label(&self) -> String {
        let level = match self.level {
            Some(level) if level > 0 => format!("[{level}] "),
            _ => String::new(),
        };
        let done = if self.complete { " (Complete)" } else { "" };
        match self.distance {
            // Rounded to a yard. A tenth of a yard on a moving character is a
            // number that never stops changing and says nothing more.
            Some(yards) => format!("{level}{}{done} - {:.0} yd", self.title, yards),
            None => format!("{level}{}{done}", self.title),
        }
    }
}

/// Everything the frame draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackerView {
    /// The quests actually drawn, already capped by the caller.
    pub quests: Vec<TrackedQuest>,
    /// How many the character is carrying in total.
    ///
    /// **Separate from `quests.len()` on purpose, and stated in every state**
    /// -- see the module comment. A tracker showing five of five and one
    /// showing five of eleven must not look the same.
    pub total: usize,
}

/// A view to draw while the layout is being edited, and before there is a
/// world.
///
/// **Carries a quest**, because a frame that draws as an empty box cannot be
/// positioned: the whole point of the edit mode is seeing where a thing will
/// sit, and an empty tracker is a sliver the size of its header.
pub fn placeholder() -> TrackerView {
    TrackerView {
        quests: vec![TrackedQuest {
            id: 0,
            title: "A Threat Within".into(),
            progress: vec!["Kobold Vermin slain: 4/8".into()],
            complete: false,
            difficulty: Difficulty::Even,
            level: Some(2),
            distance: Some(146.0),
        }],
        total: 1,
    }
}

fn header_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

fn line_height(style: &Style, scale: f32) -> f32 {
    (style.font_size + style.gap) * scale
}

/// How tall one quest is: its title, and a line per counted objective.
fn quest_height(quest: &TrackedQuest, style: &Style, scale: f32) -> f32 {
    line_height(style, scale) * (1 + quest.progress.len()) as f32 + style.gap * scale
}

/// How much room the frame wants.
///
/// Sized to its contents, like the loot window and the quest log: a character
/// on one quest should not reserve a panel's worth of screen for blank lines,
/// and this frame is always on screen so that cost would be permanent.
pub fn size(view: &TrackerView, style: &Style, scale: f32) -> Vec2 {
    let content: f32 = view
        .quests
        .iter()
        .map(|quest| quest_height(quest, style, scale))
        .sum();
    // A minimum of one line, so a character with nothing to do still has a
    // frame that can be seen, moved and put somewhere.
    let content = content.max(line_height(style, scale));
    Vec2::new(
        style.tracker_width * scale,
        header_height(style, scale) + content + style.padding * 2.0 * scale,
    )
}

/// Where each quest's title sits.
///
/// **The single source of truth for the geometry**, read by the drawing and by
/// the hit test. The rows are not all the same height -- a quest with three
/// objectives is three lines taller than one with none -- so this accumulates
/// rather than dividing by an average, which is the mistake that silently
/// selects the row below the one that was clicked.
pub fn quest_rects(rect: Rect, view: &TrackerView, style: &Style, scale: f32) -> Vec<Rect> {
    let pad = style.padding * scale;
    let left = rect.min.x + pad;
    let width = (rect.width() - pad * 2.0).max(0.0);
    let mut top = rect.min.y + pad + header_height(style, scale);
    view.quests
        .iter()
        .map(|quest| {
            let height = quest_height(quest, style, scale);
            let bounds = Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, height));
            top += height;
            bounds
        })
        .collect()
}

/// Which quest a point is on, if any.
///
/// **Answers only for the quests actually drawn.** A click below the last one
/// is a click on the frame's background, and the caller is told nothing rather
/// than being handed the nearest row -- the same rule the trainer, guild and
/// auction rows follow, where a hit test that answers for everything is
/// indistinguishable from a correct one until somebody clicks the wrong thing.
pub fn quest_at(
    rect: Rect,
    view: &TrackerView,
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<u32> {
    quest_rects(rect, view, style, scale)
        .into_iter()
        .zip(&view.quests)
        .find(|(bounds, _)| bounds.contains(point))
        .map(|(_, quest)| quest.id)
}

/// Paints the frame.
pub fn draw(painter: &Painter, rect: Rect, view: &TrackerView, style: &Style, scale: f32) {
    let corner = corner_radius(style.corner * scale);
    painter.rect_filled(rect, corner, style.tracker_background);
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
    let done: Color32 = style.quest_complete.into();
    let pad = style.padding * scale;
    let font = FontId::proportional(style.font_size * scale);
    let small = FontId::proportional(style.font_size * 0.85 * scale);

    painter.text(
        rect.min + Vec2::splat(pad),
        Align2::LEFT_TOP,
        header(view),
        font.clone(),
        text,
    );

    let painter = painter.with_clip_rect(rect);
    for (quest, bounds) in view.quests.iter().zip(quest_rects(rect, view, style, scale)) {
        // A finished quest is coloured by *being finished*, not by how hard it
        // was: "go and hand this in" is the only thing worth saying about it,
        // and a grey trivial quest that is ready to turn in would otherwise be
        // the least visible line on a frame whose whole job is saying what to
        // do next.
        let colour = if quest.complete {
            done
        } else {
            quest.difficulty.colour(style)
        };
        painter.text(
            Pos2::new(bounds.min.x, bounds.min.y + line_height(style, scale) * 0.5),
            Align2::LEFT_CENTER,
            quest.label(),
            font.clone(),
            colour,
        );
        let mut line_top = bounds.min.y + line_height(style, scale);
        for line in &quest.progress {
            painter.text(
                Pos2::new(
                    bounds.min.x + style.gap * scale * 2.0,
                    line_top + line_height(style, scale) * 0.5,
                ),
                Align2::LEFT_CENTER,
                line,
                small.clone(),
                dim,
            );
            line_top += line_height(style, scale);
        }
    }

    if view.quests.is_empty() {
        painter.text(
            Pos2::new(
                rect.min.x + pad,
                rect.min.y + pad + header_height(style, scale) + line_height(style, scale) * 0.5,
            ),
            Align2::LEFT_CENTER,
            "Nothing tracked.",
            font,
            dim,
        );
    }
}

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

/// The header, which states the count in **every** state.
///
/// See the module comment: a line that appears only when there is a surplus is
/// a line nobody has learned to read, so "3 of 3" is drawn as readily as
/// "5 of 11". The word is "of" and not a bare number for the same reason the
/// auction window draws its window as one sentence.
fn header(view: &TrackerView) -> String {
    if view.total == 0 {
        "Objectives".into()
    } else if view.quests.len() == view.total {
        format!("Objectives ({})", view.total)
    } else {
        format!("Objectives ({} of {})", view.quests.len(), view.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quest(id: u32, progress: usize) -> TrackedQuest {
        TrackedQuest {
            id,
            title: format!("Quest {id}"),
            progress: (0..progress).map(|n| format!("thing {n}: 0/1")).collect(),
            complete: false,
            difficulty: Difficulty::Even,
            level: Some(5),
            distance: None,
        }
    }

    fn view(quests: Vec<TrackedQuest>, total: usize) -> TrackerView {
        TrackerView { quests, total }
    }

    /// **The header says it is a window in every state**, including the ones
    /// where it is uninteresting. A line that only appears when there is a
    /// surplus is a line nobody has learned to read -- the rule 4.30 paid for.
    #[test]
    fn the_header_states_the_count_whether_or_not_it_is_a_window() {
        assert_eq!(header(&view(vec![], 0)), "Objectives");
        assert_eq!(header(&view(vec![quest(1, 0)], 1)), "Objectives (1)");
        assert_eq!(
            header(&view(vec![quest(1, 0), quest(2, 0)], 11)),
            "Objectives (2 of 11)"
        );
    }

    /// **The rows are not all the same height**, so the hit test must walk the
    /// same accumulating heights the drawing does. An averaged division targets
    /// the wrong quest, silently.
    #[test]
    fn the_hit_test_finds_the_quest_that_was_clicked_and_not_its_neighbour() {
        let style = Style::default();
        // Deliberately uneven: one objective, then four, then none.
        let view = view(vec![quest(1, 1), quest(2, 4), quest(3, 0)], 3);
        let rect = Rect::from_min_size(Pos2::ZERO, size(&view, &style, 1.0));
        let rects = quest_rects(rect, &view, &style, 1.0);
        assert_eq!(rects.len(), 3);
        assert!(
            rects[1].height() > rects[0].height() && rects[0].height() > rects[2].height(),
            "the middle quest is the tallest and the last the shortest"
        );
        for (bounds, quest) in rects.iter().zip(&view.quests) {
            assert_eq!(
                quest_at(rect, &view, &style, 1.0, bounds.center()),
                Some(quest.id)
            );
        }
        // And the rows do not overlap, which is what would let a click land on
        // two of them.
        assert!(rects[0].max.y <= rects[1].min.y);
        assert!(rects[1].max.y <= rects[2].min.y);
    }

    /// A click on the frame's own background is not a click on the last quest.
    #[test]
    fn a_click_past_the_last_quest_answers_nothing() {
        let style = Style::default();
        let view = view(vec![quest(1, 0)], 1);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 400.0));
        assert_eq!(
            quest_at(rect, &view, &style, 1.0, Pos2::new(10.0, 380.0)),
            None
        );
        // The header is not a quest either.
        assert_eq!(quest_at(rect, &view, &style, 1.0, Pos2::new(10.0, 2.0)), None);
    }

    /// **The grey band is the server's and the rest are this client's.** The
    /// realm's own trivial verdict wins outright, whatever the arithmetic
    /// would have said.
    #[test]
    fn the_servers_trivial_verdict_beats_the_clients_arithmetic() {
        // A level-60 quest for a level-1 character is as hard as it gets --
        // and if the realm says it is trivial, the realm is what is drawn.
        assert_eq!(Difficulty::of(60, 1, false), Difficulty::VeryHard);
        assert_eq!(Difficulty::of(60, 1, true), Difficulty::Trivial);
    }

    /// The four client-side bands, at their boundaries rather than in their
    /// middles -- the only place an off-by-one shows.
    #[test]
    fn the_client_bands_sit_where_they_say_they_do() {
        let player = 20;
        assert_eq!(Difficulty::of(25, player, false), Difficulty::VeryHard);
        assert_eq!(Difficulty::of(24, player, false), Difficulty::Hard);
        assert_eq!(Difficulty::of(23, player, false), Difficulty::Hard);
        assert_eq!(Difficulty::of(22, player, false), Difficulty::Even);
        assert_eq!(Difficulty::of(18, player, false), Difficulty::Even);
        assert_eq!(Difficulty::of(17, player, false), Difficulty::Easy);
    }

    /// **A level nobody can compare is not a difficulty.** `-1` scales to the
    /// player and `0` is a quest this client has not been told about; both
    /// draw in the ordinary colour rather than being forced into a band.
    #[test]
    fn an_uncomparable_level_has_no_band() {
        assert_eq!(Difficulty::of(-1, 20, false), Difficulty::Unknown);
        assert_eq!(Difficulty::of(0, 20, false), Difficulty::Unknown);
        // And a player whose level has not replicated yet.
        assert_eq!(Difficulty::of(10, 0, false), Difficulty::Unknown);
    }

    /// **No marker, no number.** A quest the realm gave no markers for draws
    /// no distance rather than a guess, and one that did draws it rounded to
    /// a yard.
    #[test]
    fn a_distance_is_drawn_only_when_the_realm_supplied_one() {
        let mut quest = quest(783, 0);
        quest.title = "A Threat Within".into();
        assert_eq!(quest.label(), "[5] A Threat Within");
        quest.distance = Some(146.4);
        assert_eq!(quest.label(), "[5] A Threat Within - 146 yd");
        quest.complete = true;
        assert_eq!(quest.label(), "[5] A Threat Within (Complete) - 146 yd");
        // A quest whose level never arrived says nothing about its level
        // rather than claiming zero.
        quest.level = None;
        assert_eq!(quest.label(), "A Threat Within (Complete) - 146 yd");
    }

    /// An empty tracker is still a frame somebody can find and move, not a
    /// sliver.
    #[test]
    fn an_empty_tracker_is_still_a_frame() {
        let style = Style::default();
        let empty = size(&view(vec![], 0), &style, 1.0);
        assert!(empty.x > 0.0 && empty.y > 0.0);
        assert!(size(&view(vec![quest(1, 3)], 1), &style, 1.0).y > empty.y);
    }
}
