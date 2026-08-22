//! A unit frame: who something is, and how much of it is left.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::{PowerType, Style};

/// Everything a unit frame draws, flattened out of whatever the caller read it
/// from.
///
/// Deliberately a plain snapshot with no reference back to replicated state.
/// The interface should be drawable from a struct literal -- which is what
/// makes the edit-mode placeholder below possible, and what keeps this crate
/// testable without a live connection.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitView {
    pub name: String,
    pub level: Option<u32>,
    pub health: u32,
    pub max_health: u32,
    pub power: u32,
    pub max_power: u32,
    pub power_type: PowerType,
    /// Whether this player has released and is walking as a ghost.
    ///
    /// Carried separately from [`Self::is_dead`] rather than folded into it,
    /// because a ghost has **one** health, not zero -- see
    /// `world::state::Entity::is_ghost`'s doc comment. `is_dead` alone would
    /// read a ghost as an ordinary living player with a nearly empty bar.
    /// Always `false` for anything that is not a player: nothing else in this
    /// protocol ever carries the ghost flag.
    pub ghost: bool,
    /// A rogue's (or cat-form druid's) combo points stacked against *this*
    /// unit, `0` to `5`.
    ///
    /// **`None` means "not the combo target", not "zero points."** The wire
    /// carries one combo count for the whole character, naming which unit it
    /// is stacked against -- so whether *this* frame gets to show it is a
    /// question the caller has to answer by comparing guids, the same way
    /// `ghost` is a fact about a specific player rather than something this
    /// struct can derive from health and power alone. Left `None` on the
    /// player frame always: points are drawn against a target, never against
    /// oneself, and a player frame that lit up whenever the target frame did
    /// would be showing the same fact twice under two different names.
    pub combo_points: Option<u8>,
}

impl UnitView {
    /// A stand-in so a frame can be positioned before there is anything to put
    /// in it.
    ///
    /// Without this, laying out the target frame would mean finding something
    /// to target first, and then keeping it targeted while dragging -- which
    /// is not possible for the frame that appears only while a target exists.
    pub fn placeholder(name: &str) -> Self {
        Self {
            name: name.to_string(),
            level: Some(60),
            health: 3_500,
            max_health: 5_000,
            power: 2_000,
            max_power: 4_000,
            power_type: PowerType::Mana,
            ghost: false,
            combo_points: None,
        }
    }

    pub fn health_fraction(&self) -> f32 {
        fraction(self.health, self.max_health)
    }

    pub fn power_fraction(&self) -> f32 {
        fraction(self.power, self.max_power)
    }

    /// Whether this unit is dead.
    ///
    /// **A known maximum is required, and that is the whole subtlety.** A
    /// field that has not arrived reads zero here rather than absent -- see
    /// `hud::unit_view` -- so testing `health == 0` alone would mark every
    /// unit as dead for the moment between its creation and its first field
    /// update, which in a fresh login is a hundred creatures at once. A
    /// maximum of zero means "not known yet"; a maximum with no health left
    /// means dead.
    pub fn is_dead(&self) -> bool {
        self.max_health > 0 && self.health == 0
    }

    /// Whether this unit has a resource worth a second bar.
    ///
    /// Most creatures do not, and a permanently empty bar under every wolf in
    /// Elwynn reads as a bug in the bar rather than as a fact about wolves --
    /// so the frame is simply shorter for them.
    pub fn has_power(&self) -> bool {
        self.max_power > 0
    }

    /// Whether this frame reserves a row for combo point pips.
    ///
    /// True whenever [`Self::combo_points`] is `Some`, including `Some(0)`:
    /// the row's presence says "this is the combo target", and a target with
    /// none built up yet is still the combo target -- the same distinction
    /// [`Self::has_power`] draws between a class with no resource and one
    /// sitting at zero of it.
    pub fn has_combo_points(&self) -> bool {
        self.combo_points.is_some()
    }
}

/// Combo points cap at five in 3.3.5a -- no talent in this expansion raises
/// it. A count above that is drawn as a full row rather than trusted, the
/// same restraint [`UnitView::health_fraction`] applies to overhealing.
const MAX_COMBO_POINTS: u8 = 5;

/// A bar's fill, guarding the division that a freshly created unit would
/// otherwise make: a unit whose fields have arrived but whose maximum has not
/// reports `health / 0`.
fn fraction(value: u32, max: u32) -> f32 {
    if max == 0 {
        return 0.0;
    }
    (value as f32 / max as f32).clamp(0.0, 1.0)
}

/// A combo point pip's height, as a fraction of the ordinary bar height --
/// smaller than a resource bar because five of them sit in a row rather than
/// carrying a number of their own.
const COMBO_PIP_HEIGHT_FRACTION: f32 = 0.4;

/// How much room a unit frame wants.
///
/// `scale` multiplies everything, which is only true because the frame is
/// painted rather than laid out by egui -- see [`super`].
pub fn size(style: &Style, scale: f32, has_power: bool, has_combo_points: bool) -> Vec2 {
    let name_height = style.font_size * 1.3;
    let mut height = style.padding * 2.0 + name_height + style.gap + style.bar_height;
    if has_power {
        height += style.gap + style.bar_height;
    }
    if has_combo_points {
        height += style.gap + style.bar_height * COMBO_PIP_HEIGHT_FRACTION;
    }
    Vec2::new(style.frame_width, height) * scale
}

/// Paints a unit frame into `rect`.
pub fn draw(painter: &Painter, rect: Rect, unit: &UnitView, style: &Style, scale: f32) {
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
    // Nothing is allowed outside the frame: a long name should run out of
    // room, not out of the frame and across the world behind it.
    let painter = painter.with_clip_rect(inner);

    let font = FontId::proportional(style.font_size * scale);
    // A dead unit is dimmed rather than relabelled. An empty health bar is
    // already the fact; what it is missing is that an empty bar and a bar
    // whose maximum has not arrived look identical, and the name is the place
    // with room to say which.
    //
    // Ghost is checked first and is not an `else` of `is_dead`: a ghost's
    // health reads as one, not zero, so `is_dead()` is false for a ghost and
    // the two conditions do not partition the space the way they look like
    // they do.
    let text: Color32 = if unit.ghost {
        style.text_ghost.into()
    } else if unit.is_dead() {
        style.text_dead.into()
    } else {
        style.text.into()
    };
    painter.text(
        inner.left_top(),
        Align2::LEFT_TOP,
        &unit.name,
        font.clone(),
        text,
    );
    if let Some(level) = unit.level {
        painter.text(
            inner.right_top(),
            Align2::RIGHT_TOP,
            level.to_string(),
            font.clone(),
            text,
        );
    }

    let bar_height = style.bar_height * scale;
    let mut top = inner.top() + style.font_size * 1.3 * scale + style.gap * scale;

    let health = Rect::from_min_size(
        Pos2::new(inner.left(), top),
        Vec2::new(inner.width(), bar_height),
    );
    bar(
        &painter,
        health,
        unit.health_fraction(),
        style.health_color(unit.health_fraction()).into(),
        style,
        scale,
        &font,
        unit.health,
        unit.max_health,
    );

    if unit.has_power() {
        top += bar_height + style.gap * scale;
        let power = Rect::from_min_size(
            Pos2::new(inner.left(), top),
            Vec2::new(inner.width(), bar_height),
        );
        bar(
            &painter,
            power,
            unit.power_fraction(),
            style.power_color(unit.power_type).into(),
            style,
            scale,
            &font,
            unit.power,
            unit.max_power,
        );
    }

    if let Some(count) = unit.combo_points {
        top += bar_height + style.gap * scale;
        let pip_height = bar_height * COMBO_PIP_HEIGHT_FRACTION;
        let pip_gap = style.gap * scale;
        let pip_width =
            (inner.width() - pip_gap * (MAX_COMBO_POINTS as f32 - 1.0)) / MAX_COMBO_POINTS as f32;
        let filled: Color32 = style.power_color(crate::style::PowerType::Energy).into();
        for i in 0..MAX_COMBO_POINTS {
            let left = inner.left() + i as f32 * (pip_width + pip_gap);
            let pip = Rect::from_min_size(Pos2::new(left, top), Vec2::new(pip_width, pip_height));
            let fill = if i < count.min(MAX_COMBO_POINTS) {
                filled
            } else {
                style.bar_backdrop.into()
            };
            painter.rect_filled(pip, corner_radius(style.corner * scale * 0.25), fill);
        }
    }
}

/// One filled bar with its numbers.
#[allow(clippy::too_many_arguments)]
fn bar(
    painter: &Painter,
    rect: Rect,
    fraction: f32,
    fill: Color32,
    style: &Style,
    scale: f32,
    font: &FontId,
    value: u32,
    max: u32,
) {
    let corner = corner_radius(style.corner * scale * 0.5);
    painter.rect_filled(rect, corner, style.bar_backdrop);
    if fraction > 0.0 {
        let filled = Rect::from_min_size(
            rect.min,
            Vec2::new((rect.width() * fraction).max(1.0), rect.height()),
        );
        painter.rect_filled(filled, corner, fill);
    }

    // Numbers go on last and are clipped to the bar, so a wide value cannot
    // spill into the bar underneath.
    let painter = painter.with_clip_rect(rect);
    let text: Color32 = style.text.into();
    let inset = (style.padding * 0.5 * scale).max(2.0);
    if style.show_values {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            format!("{value} / {max}"),
            font.clone(),
            text,
        );
    }
    if style.show_percent {
        painter.text(
            Pos2::new(rect.right() - inset, rect.center().y),
            Align2::RIGHT_CENTER,
            format!("{:.0}%", fraction * 100.0),
            font.clone(),
            text,
        );
    }
}

#[cfg(test)]
mod dead_tests {
    use super::*;

    fn unit(health: u32, max_health: u32) -> UnitView {
        UnitView {
            name: "Kobold Vermin".into(),
            level: Some(1),
            health,
            max_health,
            power: 0,
            max_power: 0,
            power_type: PowerType::Mana,
            ghost: false,
            combo_points: None,
        }
    }

    /// The distinction the whole thing turns on. A unit whose fields have not
    /// arrived reads `0/0`, and calling that dead would grey out every
    /// creature in range for the moment after login -- a hundred of them at
    /// once, which would look like the feature rather than like the bug.
    #[test]
    fn a_unit_with_no_fields_yet_is_not_dead() {
        assert!(!unit(0, 0).is_dead(), "0/0 means unknown, not dead");
        assert!(unit(0, 42).is_dead(), "0 of a known 42 is dead");
        assert!(!unit(1, 42).is_dead());
        assert!(!unit(42, 42).is_dead());
    }

    /// And it has to reach the screen, not just the predicate: a dead unit's
    /// name is painted in a different colour, so the same frame drawn dead and
    /// alive must not produce identical shapes.
    #[test]
    fn a_dead_unit_is_drawn_differently() {
        fn painted(unit: &UnitView) -> String {
            let ctx = egui::Context::default();
            let style = Style::default();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                let rect = Rect::from_min_size(Pos2::ZERO, size(&style, 1.0, false, false));
                draw(&painter, rect, unit, &style, 1.0);
            });
            let rendered = format!("{:?}", output.shapes);
            output.drop_without_applying_deltas();
            rendered
        }

        let alive = painted(&unit(42, 42));
        let dead = painted(&unit(0, 42));
        assert!(alive.len() > 100, "the frame painted nothing to compare");
        assert_ne!(
            alive, dead,
            "a dead unit drew exactly the same shapes as a living one"
        );

        // A ghost has **one** health, not zero (see `UnitView::ghost`'s doc
        // comment), so `is_dead()` is false for it -- the only thing that can
        // possibly distinguish it on screen is the `ghost` flag actually
        // being read by `draw`. This is the case that would pass silently if
        // the check above were the only one: a ghost with `ghost: false`
        // draws identically to an ordinary living player at 1/42 health.
        let mut ghost = unit(1, 42);
        ghost.ghost = true;
        let ghost = painted(&ghost);
        assert_ne!(
            alive, ghost,
            "a ghost drew exactly the same shapes as a living player"
        );
        assert_ne!(
            dead, ghost,
            "a ghost drew exactly the same shapes as a dead-not-released unit"
        );
    }
}

/// egui measures corner radii in whole pixels stored as `u8`, so a scaled
/// radius has to be rounded and capped rather than passed through.
fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> UnitView {
        UnitView {
            name: "Testwolf".into(),
            level: Some(4),
            health: 60,
            max_health: 120,
            power: 30,
            max_power: 100,
            power_type: PowerType::Mana,
            ghost: false,
            combo_points: None,
        }
    }

    /// A unit whose maximum has not arrived yet must not divide by it. Update
    /// fields arrive in whatever order the server packed them, so a unit with
    /// health and no maximum is a real state and not a corrupt one.
    #[test]
    fn a_missing_maximum_is_an_empty_bar_not_a_division() {
        let partial = UnitView {
            health: 40,
            max_health: 0,
            max_power: 0,
            ..unit()
        };
        assert_eq!(partial.health_fraction(), 0.0);
        assert_eq!(partial.power_fraction(), 0.0);
        assert!(!partial.has_power());
    }

    /// Health above the reported maximum is normal for a moment after a buff
    /// drops, and a bar wider than its own frame looks like a layout bug.
    #[test]
    fn overhealth_is_clamped_to_a_full_bar() {
        let over = UnitView {
            health: 500,
            max_health: 120,
            ..unit()
        };
        assert_eq!(over.health_fraction(), 1.0);
    }

    /// `scale` has to multiply the whole frame, not just its width -- that is
    /// the property that makes it worth exposing at all.
    #[test]
    fn scale_multiplies_every_dimension() {
        let style = Style::default();
        let single = size(&style, 1.0, true, false);
        let double = size(&style, 2.0, true, false);
        assert_eq!(double, single * 2.0);
    }

    /// A creature with no resource gets a shorter frame rather than an empty
    /// second bar.
    #[test]
    fn a_unit_without_power_gets_a_shorter_frame() {
        let style = Style::default();
        let with = size(&style, 1.0, true, false);
        let without = size(&style, 1.0, false, false);
        assert!(without.y < with.y);
        assert_eq!(without.x, with.x);
        assert_eq!(with.y - without.y, style.gap + style.bar_height);
    }

    /// A target carrying combo points gets a taller frame still, for the pip
    /// row -- the same shape as the power-bar test above, one field over.
    #[test]
    fn a_combo_target_gets_a_taller_frame() {
        let style = Style::default();
        let without = size(&style, 1.0, true, false);
        let with = size(&style, 1.0, true, true);
        assert!(with.y > without.y);
        assert_eq!(with.x, without.x);
    }

    /// The pip row itself has to reach the screen, not just the size
    /// reservation -- the same distinction `a_dead_unit_is_drawn_differently`
    /// draws for the health bar.
    #[test]
    fn combo_points_change_what_is_painted() {
        fn painted(view: &UnitView) -> String {
            let ctx = egui::Context::default();
            let style = Style::default();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                let rect = Rect::from_min_size(
                    Pos2::ZERO,
                    size(&style, 1.0, true, view.has_combo_points()),
                );
                draw(&painter, rect, view, &style, 1.0);
            });
            let rendered = format!("{:?}", output.shapes);
            output.drop_without_applying_deltas();
            rendered
        }

        let none = UnitView {
            combo_points: None,
            ..unit()
        };
        let zero = UnitView {
            combo_points: Some(0),
            ..unit()
        };
        let three = UnitView {
            combo_points: Some(3),
            ..unit()
        };
        let none = painted(&none);
        let zero = painted(&zero);
        let three = painted(&three);
        assert_ne!(none, zero, "a combo target with none built up still draws its row");
        assert_ne!(zero, three, "3 of 5 pips must look different from none filled");
    }
}
