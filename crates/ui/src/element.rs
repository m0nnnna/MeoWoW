//! Where a piece of interface sits.
//!
//! Every element is positioned the same way: pick a point on the screen
//! (the [`Anchor`]), pick the matching point on the element itself, and offset
//! one from the other. That single rule is what makes the layout survive a
//! resize -- an element anchored to [`Anchor::BottomRight`] stays in the corner
//! when the window grows, where a stored absolute position would drift off the
//! edge or leave a gap.
//!
//! The rule runs in both directions, and that is the part worth being careful
//! about. Drawing needs anchor + offset to become a rectangle; dragging needs a
//! rectangle to become an offset. Two separately written conversions would
//! drift, and the symptom -- an element that creeps a little every time it is
//! dragged -- is slow enough to blame on anything. [`Element::rect`] and
//! [`Element::offset_for`] are therefore written as one formula and its
//! inverse, and round-tripped in the tests below.

use egui::{Pos2, Rect, Vec2};
use serde::{Deserialize, Serialize};

/// The point an element is measured from, on both the screen and the element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    #[default]
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// Every anchor, in reading order, for the edit-mode picker.
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];

    /// How far across and down the anchor sits, as fractions of a rectangle.
    ///
    /// This is the whole of the anchoring maths: the same pair is applied to
    /// the screen to find the reference point and to the element to find which
    /// of its own corners meets that point.
    pub fn fractions(self) -> Vec2 {
        let (x, y) = match self {
            Anchor::TopLeft => (0.0, 0.0),
            Anchor::Top => (0.5, 0.0),
            Anchor::TopRight => (1.0, 0.0),
            Anchor::Left => (0.0, 0.5),
            Anchor::Center => (0.5, 0.5),
            Anchor::Right => (1.0, 0.5),
            Anchor::BottomLeft => (0.0, 1.0),
            Anchor::Bottom => (0.5, 1.0),
            Anchor::BottomRight => (1.0, 1.0),
        };
        Vec2::new(x, y)
    }

    pub fn label(self) -> &'static str {
        match self {
            Anchor::TopLeft => "top left",
            Anchor::Top => "top",
            Anchor::TopRight => "top right",
            Anchor::Left => "left",
            Anchor::Center => "center",
            Anchor::Right => "right",
            Anchor::BottomLeft => "bottom left",
            Anchor::Bottom => "bottom",
            Anchor::BottomRight => "bottom right",
        }
    }
}

/// One placed piece of interface.
///
/// Deliberately `Copy` and free of any handle to what it draws: an element
/// says *where* and *how big*, and the frame modules say *what*. That split is
/// what lets the whole layout be read out of a text file the user wrote.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
// Container-level default, so a hand-written entry may set one field and
// inherit the rest. A user editing this file by hand should not have to
// discover the full field list to nudge something ten pixels.
#[serde(default, deny_unknown_fields)]
pub struct Element {
    pub anchor: Anchor,
    /// Screen pixels from the anchor, before the element's own scale.
    pub offset: [f32; 2],
    /// Multiplies every dimension the frame draws with.
    pub scale: f32,
    pub visible: bool,
}

impl Default for Element {
    fn default() -> Self {
        Self {
            anchor: Anchor::TopLeft,
            offset: [0.0, 0.0],
            scale: 1.0,
            visible: true,
        }
    }
}

/// Scales outside this range are refused when a layout file is read: a zero
/// scale draws nothing, and a hand-edited typo (`scale = 100`) would otherwise
/// fill the screen with one health bar and leave no way back except deleting
/// the file, since the controls that would fix it are underneath.
pub const MIN_SCALE: f32 = 0.25;
pub const MAX_SCALE: f32 = 4.0;

impl Element {
    /// Where this element lands on screen, given how big it wants to be.
    pub fn rect(&self, screen: Rect, size: Vec2) -> Rect {
        let fractions = self.anchor.fractions();
        let point = screen.min + screen.size() * fractions;
        let min = point + Vec2::from(self.offset) - size * fractions;
        Rect::from_min_size(min, size)
    }

    /// The inverse of [`Element::rect`]: the offset that would place this
    /// element's top-left corner at `min`.
    ///
    /// Used by dragging, which knows where the pointer put the frame and needs
    /// to store that as an anchor-relative offset.
    pub fn offset_for(&self, screen: Rect, size: Vec2, min: Pos2) -> [f32; 2] {
        let fractions = self.anchor.fractions();
        let point = screen.min + screen.size() * fractions;
        ((min + size * fractions) - point).into()
    }

    /// Switches anchors without moving the element on screen.
    ///
    /// Changing an anchor is the one edit that would otherwise teleport a
    /// frame across the window, which reads as a bug rather than as a choice.
    /// Re-anchoring keeps the pixels where they are and only changes what the
    /// element is measured from -- so the difference shows up on the *next*
    /// resize, which is exactly when the user meant it to.
    pub fn rebase(&mut self, anchor: Anchor, screen: Rect, size: Vec2) {
        let held = self.rect(screen, size).min;
        self.anchor = anchor;
        self.offset = self.offset_for(screen, size, held);
    }

    /// Clamps a hand-edited element into a range that can still be seen and
    /// undone. Returns whether anything had to change, so the caller can say
    /// so rather than silently disagreeing with the file on disk.
    pub fn sanitise(&mut self) -> bool {
        let scale = self.scale.clamp(MIN_SCALE, MAX_SCALE);
        let offset = [
            if self.offset[0].is_finite() {
                self.offset[0]
            } else {
                0.0
            },
            if self.offset[1].is_finite() {
                self.offset[1]
            } else {
                0.0
            },
        ];
        let changed = scale != self.scale || offset != self.offset;
        self.scale = scale;
        self.offset = offset;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0))
    }

    /// The anchor's whole purpose: a corner-anchored element keeps its
    /// distance from that corner when the window changes size, where a stored
    /// absolute position would not.
    #[test]
    fn corner_anchors_survive_a_resize() {
        let element = Element {
            anchor: Anchor::BottomRight,
            offset: [-20.0, -20.0],
            ..Default::default()
        };
        let size = Vec2::new(200.0, 60.0);

        let small = element.rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)), size);
        let large = element.rect(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(2560.0, 1440.0)),
            size,
        );

        assert_eq!(small.max, Pos2::new(780.0, 580.0));
        assert_eq!(large.max, Pos2::new(2540.0, 1420.0));
    }

    /// `rect` and `offset_for` are one formula and its inverse. If they ever
    /// stop agreeing, a dragged frame creeps by the difference every frame it
    /// is held -- a drift slow enough to be blamed on the pointer.
    #[test]
    fn placing_and_measuring_are_inverses() {
        let size = Vec2::new(220.0, 74.0);
        for anchor in Anchor::ALL {
            let element = Element {
                anchor,
                offset: [37.0, -19.0],
                ..Default::default()
            };
            let rect = element.rect(screen(), size);
            let recovered = element.offset_for(screen(), size, rect.min);
            assert_eq!(recovered, element.offset, "{}", anchor.label());
        }
    }

    /// Dragging is `offset_for` applied to a moved rectangle, so a drag of a
    /// known distance must move the frame exactly that far -- under every
    /// anchor, including the ones whose fractions are negative-going.
    #[test]
    fn a_drag_moves_the_frame_by_the_drag_distance() {
        let size = Vec2::new(220.0, 74.0);
        let delta = Vec2::new(13.0, -47.0);
        for anchor in Anchor::ALL {
            let mut element = Element {
                anchor,
                ..Default::default()
            };
            let before = element.rect(screen(), size);
            element.offset = element.offset_for(screen(), size, before.min + delta);
            let after = element.rect(screen(), size);
            assert_eq!(after.min, before.min + delta, "{}", anchor.label());
        }
    }

    /// Re-anchoring is a change of reference, not a move.
    #[test]
    fn rebasing_holds_the_frame_still() {
        let size = Vec2::new(220.0, 74.0);
        let start = Element {
            anchor: Anchor::TopLeft,
            offset: [400.0, 300.0],
            ..Default::default()
        };
        let held = start.rect(screen(), size);
        for anchor in Anchor::ALL {
            let mut element = start;
            element.rebase(anchor, screen(), size);
            assert_eq!(element.rect(screen(), size), held, "{}", anchor.label());
        }
    }

    /// A partial entry inherits the rest, so hand-editing one field is safe.
    #[test]
    fn a_partial_entry_inherits_its_defaults() {
        let element: Element = toml::from_str("offset = [12.0, 34.0]").unwrap();
        assert_eq!(element.offset, [12.0, 34.0]);
        assert_eq!(element.scale, 1.0, "an omitted scale must not mean zero");
        assert!(element.visible);
        assert_eq!(element.anchor, Anchor::TopLeft);
    }

    /// A typo in the file must not produce a layout with no way back to the
    /// controls that would fix it.
    #[test]
    fn an_unusable_scale_is_clamped() {
        let mut element = Element {
            scale: 100.0,
            ..Default::default()
        };
        assert!(element.sanitise());
        assert_eq!(element.scale, MAX_SCALE);

        let mut zero = Element {
            scale: 0.0,
            ..Default::default()
        };
        assert!(zero.sanitise());
        assert_eq!(zero.scale, MIN_SCALE);
    }
}
