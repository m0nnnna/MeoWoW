//! The party: everyone in the group but the reader.
//!
//! A stack of compact member rows rather than a re-use of [`super::unit`],
//! and the reason is not that they should look different. A unit frame's
//! fields are all *known* -- it draws something replicated and in view, so
//! health, power and level are facts. **A party member's fields are not**: a
//! member two zones away is not a replicated object at all, and what is known
//! about them is whatever `SMSG_PARTY_MEMBER_STATS` last said, which may be
//! nothing. `UnitView` carries plain `u32`s and has nowhere to put "not
//! known"; widening it to `Option`s would make every frame that draws a
//! creature answer a question only a party can ask.
//!
//! So the honesty rule this project applies to numbers applies here as
//! geometry: **a bar whose maximum is unknown is drawn empty and unlabelled,
//! never full.** A party frame showing a confident full health bar for a
//! member the client has heard nothing about is exactly the fabricated `47`
//! the tooltip substituter refuses to print -- worse than a blank, because
//! nobody can tell it is wrong.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::{PowerType, Style};

/// One row of the party frame.
///
/// Every live number is an [`Option`] and none of them is defaulted -- see the
/// module comment. `name`, `guid`, `online`, `dead` and `leader` are not,
/// because `SMSG_GROUP_LIST` states all five for every member on every send,
/// so there is no state in which they are unknown.
#[derive(Clone, Debug, PartialEq)]
pub struct PartyMemberView {
    pub name: String,
    /// Who to target when this row is clicked. **A guid rather than the row's
    /// position**, for the same reason a loot row carries a slot: the list is
    /// rebuilt from every group list the server sends, and a position means
    /// nothing by the time the caller reads it back.
    pub guid: u64,
    pub level: Option<u32>,
    pub health: Option<u32>,
    pub max_health: Option<u32>,
    pub power: Option<u32>,
    pub max_power: Option<u32>,
    /// Absent until something says which pool this member spends -- and drawn
    /// as no power bar at all rather than as a mana bar, since defaulting an
    /// index picks a colour instead of admitting ignorance.
    pub power_type: Option<PowerType>,
    /// Whether this member is connected. An offline member **stays in the
    /// list** -- the server keeps them there -- so this has to be drawn
    /// rather than filtered, or a party of three would silently become a
    /// party of two whenever somebody's connection dropped.
    pub online: bool,
    /// A corpse or a ghost. Two different status bits, one meaning here.
    pub dead: bool,
    /// Whether this member leads the group.
    pub leader: bool,
}

impl PartyMemberView {
    /// Stand-ins so the frame can be positioned without finding two other
    /// people first -- the same reason `UnitView::placeholder` exists, and
    /// more pressing here, since a party cannot be formed alone at all.
    ///
    /// The three deliberately differ in *what is known about them*, not just
    /// in their numbers: one full record, one dead with no power pool, and one
    /// offline with nothing but a name. Three identical rows would size the
    /// frame correctly and still hide the fact that a row can be mostly blank.
    pub fn placeholder() -> Vec<Self> {
        vec![
            Self {
                name: "Watcher".into(),
                guid: 3,
                level: Some(12),
                health: Some(410),
                max_health: Some(560),
                power: Some(120),
                max_power: Some(300),
                power_type: Some(PowerType::Mana),
                online: true,
                dead: false,
                leader: true,
            },
            Self {
                name: "Huntertest".into(),
                guid: 4,
                level: Some(9),
                health: Some(0),
                max_health: Some(340),
                power: None,
                max_power: None,
                power_type: None,
                online: true,
                dead: true,
                leader: false,
            },
            Self {
                name: "Testdruid".into(),
                guid: 5,
                level: None,
                health: None,
                max_health: None,
                power: None,
                max_power: None,
                power_type: None,
                online: false,
                dead: false,
                leader: false,
            },
        ]
    }

    /// Whether this member has a power pool worth a second bar.
    ///
    /// All three parts are required. A member out of view has no power fields
    /// at all, and a member whose maximum is zero has a pool this client
    /// cannot draw a fraction of -- both would otherwise get a permanently
    /// empty second bar, which reads as a broken bar rather than as an absent
    /// fact.
    pub fn has_power(&self) -> bool {
        matches!((self.power_type, self.max_power), (Some(_), Some(max)) if max > 0)
    }

    /// The health bar's fill, and **`None` is not zero**: a member nothing is
    /// known about draws an empty bar with no numbers on it, where a member at
    /// zero of a known maximum is dead and says so.
    pub fn health_fraction(&self) -> Option<f32> {
        fraction(self.health, self.max_health)
    }

    pub fn power_fraction(&self) -> Option<f32> {
        fraction(self.power, self.max_power)
    }
}

/// A bar's fill from two values either of which may be unknown.
fn fraction(value: Option<u32>, max: Option<u32>) -> Option<f32> {
    let (value, max) = (value?, max?);
    if max == 0 {
        return None;
    }
    Some((value as f32 / max as f32).clamp(0.0, 1.0))
}

/// How tall one member's row is at `scale`.
fn row_height(style: &Style, scale: f32, has_power: bool) -> f32 {
    let mut height = style.font_size * 1.3 + style.gap + style.party_bar_height;
    if has_power {
        height += style.party_bar_gap + style.party_bar_height;
    }
    height * scale
}

/// How much room the party frame wants.
///
/// **Sized to the members, and to what is known about each of them.** A
/// two-person party is a shorter frame than a four-person one, and a member
/// with no power pool takes a shorter row than one with a bar to draw -- the
/// same rule the loot window and the quest log follow. A fixed height would
/// leave a band of empty frame under a small party, which reads as a window
/// that failed to fill rather than as a group with two people in it.
pub fn size(members: &[PartyMemberView], style: &Style, scale: f32) -> Vec2 {
    if members.is_empty() {
        return Vec2::ZERO;
    }
    let rows: f32 = members
        .iter()
        .map(|member| row_height(style, scale, member.has_power()))
        .sum();
    let gaps = style.gap * scale * members.len().saturating_sub(1) as f32;
    Vec2::new(
        style.party_width * scale,
        style.padding * 2.0 * scale + rows + gaps,
    )
}

/// Which member row a point is in, or `None` between rows and outside the
/// frame.
///
/// Walks the same accumulating heights `draw` does rather than dividing by an
/// average, because the rows are **not** all the same height: a member with a
/// power bar is taller than one without, so a uniform division would put a
/// click on the wrong person for every party where the two are mixed -- which
/// is most of them, since a member out of view has no power fields at all.
pub fn row_at(
    rect: Rect,
    members: &[PartyMemberView],
    style: &Style,
    scale: f32,
    point: Pos2,
) -> Option<usize> {
    if !rect.contains(point) {
        return None;
    }
    let mut top = rect.top() + style.padding * scale;
    for (index, member) in members.iter().enumerate() {
        let height = row_height(style, scale, member.has_power());
        if point.y >= top && point.y < top + height {
            return Some(index);
        }
        top += height + style.gap * scale;
    }
    None
}

/// Paints the party frame into `rect`.
pub fn draw(painter: &Painter, rect: Rect, members: &[PartyMemberView], style: &Style, scale: f32) {
    if members.is_empty() {
        return;
    }
    let corner = corner_radius(style.corner * scale);
    painter.rect_filled(rect, corner, style.background);
    if style.border_width > 0.0 {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(style.border_width * scale, style.border),
            StrokeKind::Inside,
        );
    }

    let inner = rect.shrink(style.padding * scale);
    let painter = painter.with_clip_rect(inner);
    let font = FontId::proportional(style.font_size * scale);

    let mut top = inner.top();
    for member in members {
        // Offline is checked before dead and is not an `else` of it. The two
        // are independent -- a member can lose their connection while dead --
        // and being unreachable is the more useful of the two to see, because
        // nothing about the row will change until they come back.
        let text: Color32 = if !member.online {
            style.party_offline.into()
        } else if member.dead {
            style.text_dead.into()
        } else {
            style.text.into()
        };

        let name = if member.leader {
            // Marked rather than coloured: leadership decides who may invite
            // and kick, and a colour would have to compete with the three
            // this row already uses to say something entirely different.
            format!("{} {}", style.party_leader_mark, member.name)
        } else {
            member.name.clone()
        };
        painter.text(
            Pos2::new(inner.left(), top),
            Align2::LEFT_TOP,
            name,
            font.clone(),
            text,
        );
        // An unknown level prints nothing at all rather than `0` or `??`: the
        // right-hand end of the row is where a level goes, and leaving it
        // empty is the same statement the empty bars below make.
        if let Some(level) = member.level {
            painter.text(
                Pos2::new(inner.right(), top),
                Align2::RIGHT_TOP,
                level.to_string(),
                font.clone(),
                text,
            );
        }

        let bar_height = style.party_bar_height * scale;
        let mut bar_top = top + style.font_size * 1.3 * scale + style.gap * scale;
        let health_fraction = member.health_fraction();
        bar(
            &painter,
            Rect::from_min_size(
                Pos2::new(inner.left(), bar_top),
                Vec2::new(inner.width(), bar_height),
            ),
            health_fraction,
            health_fraction
                .map(|f| style.health_color(f).into())
                .unwrap_or_else(|| Color32::from(style.bar_backdrop)),
            style,
            scale,
            &font,
            member.health,
            member.max_health,
            member.online,
        );

        if member.has_power() {
            bar_top += bar_height + style.party_bar_gap * scale;
            let power_type = member.power_type.unwrap_or_default();
            bar(
                &painter,
                Rect::from_min_size(
                    Pos2::new(inner.left(), bar_top),
                    Vec2::new(inner.width(), bar_height),
                ),
                member.power_fraction(),
                style.power_color(power_type).into(),
                style,
                scale,
                &font,
                member.power,
                member.max_power,
                member.online,
            );
        }

        top += row_height(style, scale, member.has_power()) + style.gap * scale;
    }
}

/// One member's bar, which is where "not known" actually reaches the screen.
///
/// A fill of `None` paints the backdrop and **no numbers**. That is the whole
/// reason this takes `Option`s where [`super::unit`]'s bar takes `u32`s: an
/// empty bar labelled `0 / 0` says the member has no health, and an empty bar
/// labelled nothing says the client has not been told.
#[allow(clippy::too_many_arguments)]
fn bar(
    painter: &Painter,
    rect: Rect,
    fraction: Option<f32>,
    fill: Color32,
    style: &Style,
    scale: f32,
    font: &FontId,
    value: Option<u32>,
    max: Option<u32>,
    online: bool,
) {
    let corner = corner_radius(style.corner * scale * 0.5);
    painter.rect_filled(rect, corner, style.bar_backdrop);
    if let Some(fraction) = fraction {
        if fraction > 0.0 {
            let filled = Rect::from_min_size(
                rect.min,
                Vec2::new((rect.width() * fraction).max(1.0), rect.height()),
            );
            painter.rect_filled(filled, corner, fill);
        }
    }

    let painter = painter.with_clip_rect(rect);
    // Offline text is dimmed along with the name above it, so a whole row
    // reads as stale at a glance rather than one line of it.
    let text: Color32 = if online {
        style.text.into()
    } else {
        style.party_offline.into()
    };
    if style.show_values {
        if let (Some(value), Some(max)) = (value, max) {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!("{value} / {max}"),
                font.clone(),
                text,
            );
        }
    }
    if style.show_percent {
        if let Some(fraction) = fraction {
            let inset = (style.padding * 0.5 * scale).max(2.0);
            painter.text(
                Pos2::new(rect.right() - inset, rect.center().y),
                Align2::RIGHT_CENTER,
                format!("{:.0}%", fraction * 100.0),
                font.clone(),
                text,
            );
        }
    }
}

/// egui measures corner radii in whole pixels stored as `u8`.
fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> PartyMemberView {
        PartyMemberView {
            name: "Watcher".into(),
            guid: 3,
            level: Some(12),
            health: Some(410),
            max_health: Some(560),
            power: Some(120),
            max_power: Some(300),
            power_type: Some(PowerType::Mana),
            online: true,
            dead: false,
            leader: false,
        }
    }

    /// A member out of visibility range who has never sent a stats packet.
    /// The common case the whole `Option` layer exists for -- not a corrupt
    /// record, just a name and a guid.
    fn unknown() -> PartyMemberView {
        PartyMemberView {
            name: "Testdruid".into(),
            guid: 5,
            level: None,
            health: None,
            max_health: None,
            power: None,
            max_power: None,
            power_type: None,
            online: true,
            dead: false,
            leader: false,
        }
    }

    /// The distinction the whole frame turns on, and both halves are asserted
    /// because a test of either alone passes under the wrong rule. A member
    /// nothing is known about must not read as a member at zero health --
    /// which is what a `unwrap_or(0)` anywhere in the pipeline would produce,
    /// and it would look like a whole party permanently dead.
    #[test]
    fn an_unknown_bar_is_not_an_empty_one() {
        assert_eq!(unknown().health_fraction(), None, "unknown is not zero");

        let dead = PartyMemberView {
            health: Some(0),
            max_health: Some(340),
            ..known()
        };
        assert_eq!(dead.health_fraction(), Some(0.0), "zero of a known max is zero");
        assert_eq!(known().health_fraction(), Some(410.0 / 560.0));
    }

    /// A maximum that has arrived as zero is a pool this client cannot draw a
    /// fraction of, and dividing by it would be the bug the unit frame's own
    /// guard exists for.
    #[test]
    fn a_zero_maximum_is_unknown_rather_than_a_division() {
        let zeroed = PartyMemberView {
            max_health: Some(0),
            max_power: Some(0),
            ..known()
        };
        assert_eq!(zeroed.health_fraction(), None);
        assert_eq!(zeroed.power_fraction(), None);
        assert!(!zeroed.has_power(), "a zero pool gets no bar at all");
    }

    /// A power type with no numbers, and numbers with no power type, are both
    /// states the wire produces -- the mask names fields independently. Either
    /// one alone must not draw a bar, or the frame invents a colour or a
    /// length.
    #[test]
    fn half_a_power_pool_is_not_a_bar() {
        assert!(known().has_power());
        assert!(!PartyMemberView {
            power_type: None,
            ..known()
        }
        .has_power());
        assert!(!PartyMemberView {
            max_power: None,
            ..known()
        }
        .has_power());
    }

    /// The rows are not all the same height, so the hit test cannot divide by
    /// an average. This is the check that would fail if it did: a party whose
    /// first member has a power bar and whose second does not puts the second
    /// row's centre somewhere a uniform division would call the first row.
    #[test]
    fn a_click_lands_on_the_row_it_is_over() {
        let style = Style::default();
        let members = vec![known(), unknown(), known()];
        let rect = Rect::from_min_size(Pos2::ZERO, size(&members, &style, 1.0));

        let mut top = rect.top() + style.padding;
        for (index, member) in members.iter().enumerate() {
            let height = row_height(&style, 1.0, member.has_power());
            let centre = Pos2::new(rect.center().x, top + height * 0.5);
            assert_eq!(
                row_at(rect, &members, &style, 1.0, centre),
                Some(index),
                "row {index} did not answer for its own centre"
            );
            top += height + style.gap;
        }

        assert_eq!(
            row_at(rect, &members, &style, 1.0, Pos2::new(-10.0, -10.0)),
            None,
            "a point outside the frame named a row"
        );
    }

    /// Sized to the party, like the loot window and the quest log: three
    /// people take more room than two, and a member with a power bar takes
    /// more than one without.
    #[test]
    fn the_frame_grows_with_the_party() {
        let style = Style::default();
        let two = size(&[known(), known()], &style, 1.0);
        let three = size(&[known(), known(), known()], &style, 1.0);
        assert!(three.y > two.y);
        assert_eq!(three.x, two.x, "only the height depends on the party");

        let with_power = size(&[known()], &style, 1.0);
        let without = size(&[unknown()], &style, 1.0);
        assert!(
            without.y < with_power.y,
            "a member with nothing known took as much room as one with a power bar"
        );

        assert_eq!(
            size(&[], &style, 1.0),
            Vec2::ZERO,
            "an empty party asked for room"
        );
    }

    /// `scale` multiplies the whole frame, the property that makes it worth
    /// exposing at all.
    #[test]
    fn scale_multiplies_every_dimension() {
        let style = Style::default();
        let members = vec![known(), unknown()];
        let single = size(&members, &style, 1.0);
        let double = size(&members, &style, 2.0);
        assert!((double.x - single.x * 2.0).abs() < 0.001);
        assert!((double.y - single.y * 2.0).abs() < 0.001);
    }

    /// And it has to reach the screen, not just the predicate. Three states
    /// that differ only in what is *known* must not paint the same shapes --
    /// which is the failure a `unwrap_or(0)` produces, and it is invisible in
    /// every test above.
    #[test]
    fn what_is_known_changes_what_is_drawn() {
        fn painted(members: &[PartyMemberView]) -> String {
            let ctx = egui::Context::default();
            let style = Style::default();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(500.0))),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                let rect = Rect::from_min_size(Pos2::ZERO, size(members, &style, 1.0));
                draw(&painter, rect, members, &style, 1.0);
            });
            let rendered = format!("{:?}", output.shapes);
            output.drop_without_applying_deltas();
            rendered
        }

        let alive = painted(&[known()]);
        assert!(alive.len() > 100, "the frame painted nothing to compare");

        let blank = painted(&[unknown()]);
        assert_ne!(
            alive, blank,
            "a member nothing is known about drew the same as one at full health"
        );

        let dead = painted(&[PartyMemberView {
            health: Some(0),
            dead: true,
            ..known()
        }]);
        assert_ne!(dead, blank, "a dead member drew the same as an unknown one");

        let offline = painted(&[PartyMemberView {
            online: false,
            ..known()
        }]);
        assert_ne!(
            alive, offline,
            "an offline member drew the same as a connected one"
        );

        let leader = painted(&[PartyMemberView {
            leader: true,
            ..known()
        }]);
        assert_ne!(alive, leader, "the leader drew the same as an ordinary member");
    }
}
