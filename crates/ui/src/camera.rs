//! Camera preferences.
//!
//! Not drawn by this crate and not about frames at all -- these live here
//! because [`crate::layout::Profile`] is the one thing that is written to
//! `ui.toml`, and a setting the player can change is a setting the player
//! expects to still be there tomorrow. The viewer owns the camera; this owns
//! what the player told it.

use serde::{Deserialize, Serialize};

/// How the third-person camera behaves.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
// Container-level default, so a file may set one field and inherit the rest.
#[serde(default, deny_unknown_fields)]
pub struct Camera {
    /// How much of a turn a drag across the full window width is worth, in
    /// **degrees**.
    ///
    /// Degrees rather than radians because this is the number a person edits
    /// by hand in `ui.toml`, and 180 is a quantity anyone can picture where
    /// 3.14159 is not.
    ///
    /// Expressed per *window* rather than per pixel so the feel is a property
    /// of the gesture rather than of the display -- the constant this replaced
    /// was per-pixel, and gave two and a half full turns on a wide monitor
    /// against half a turn on a small one.
    pub turn_per_window: f32,

    /// Whether dragging down tilts the view up.
    pub invert_pitch: bool,

    /// How far back the camera starts, before the wheel moves it.
    pub distance: f32,

    /// How far the world is drawn, in world units.
    ///
    /// **A preference and not a constant, because it is the largest single
    /// lever on the frame and its right value is a judgement.** Measured at
    /// Goldshire: 12,000 units against 397 is 3,713 draws against 2,035 and
    /// 81 frames a second against 110, for under 1% of the pixels at eye
    /// level -- but 13.8% from a rise, where the difference is a horizon.
    /// The reference 3.3.5a client ships `farclip` at **397** and its own
    /// slider stops at 777.
    ///
    /// Here rather than as a bare flag default for the reason the module
    /// exists: a setting the player changes is one they expect to still be
    /// there tomorrow. `--view-distance` still outranks it, because a flag
    /// typed on purpose has to beat a setting typed once -- that is what
    /// keeps a probe reproducible.
    pub view_distance: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            // Half a turn across the window, which is what the viewer's own
            // constant was documented as meaning long before it meant it.
            turn_per_window: 180.0,
            invert_pitch: false,
            distance: 9.0,
            view_distance: DEFAULT_VIEW_DISTANCE,
        }
    }
}

/// The narrowest a full-window drag may be, in degrees.
///
/// Not zero, and not near it: a rate small enough to need several sweeps to
/// turn around reads as a camera that has stopped responding rather than as a
/// precise one.
pub const MIN_TURN_PER_WINDOW: f32 = 45.0;
/// The widest. Two full turns is already past the point where the view is hard
/// to aim, and it is comfortably beyond the old accidental value.
pub const MAX_TURN_PER_WINDOW: f32 = 720.0;

/// What the world is drawn to unless told otherwise: the reference client's
/// own `farclip`, read off the 3.3.5a install with the `MeoBench` addon.
pub const DEFAULT_VIEW_DISTANCE: f32 = 397.0;

/// The range the slider offers.
///
/// The near end is the reference client's own minimum rather than a number
/// chosen here; the far end is past its maximum of 777, because this client
/// keeps nine tiles resident and there is no reason to forbid seeing them.
/// **Neither end is zero**: a view distance of nothing draws nothing, and a
/// value read from a file a person can type into has to be clamped rather
/// than trusted -- the same reason `radians_per_pixel` clamps.
pub const MIN_VIEW_DISTANCE: f32 = 177.0;
pub const MAX_VIEW_DISTANCE: f32 = 1600.0;

/// The closest and furthest the camera may sit from its subject.
///
/// The near end is deliberately not zero: a true first-person view would put
/// the camera inside the character's own head geometry, which this client
/// draws, and the result is a screenful of the inside of a face.
pub const MIN_DISTANCE: f32 = 2.5;
pub const MAX_DISTANCE: f32 = 30.0;

impl Camera {
    /// Radians of turn per pixel of drag, for a window this wide.
    ///
    /// **The single place the preference becomes a rate.** Both mouse axes ask
    /// here, and both pass the *width*: deriving pitch from the height instead
    /// would make a diagonal drag curve on any window that is not square,
    /// because the same hand movement would mean different angles on the two
    /// axes.
    ///
    /// Clamped rather than trusted. This value is read from a file a person can
    /// type into, and a zero would freeze the camera while a negative would
    /// invert it in a way no setting says it should.
    pub fn radians_per_pixel(&self, window_width: f32) -> f32 {
        let degrees = self
            .turn_per_window
            .clamp(MIN_TURN_PER_WINDOW, MAX_TURN_PER_WINDOW);
        degrees.to_radians() / window_width.max(1.0)
    }

    /// The starting distance, clamped to what the wheel may reach.
    pub fn start_distance(&self) -> f32 {
        self.distance.clamp(MIN_DISTANCE, MAX_DISTANCE)
    }

    /// How far to draw, clamped to what the slider offers.
    ///
    /// Named apart from the field so the unclamped value can still be
    /// serialised: `ui.toml` records what the player chose, and this is what
    /// the camera is given. Same split as `distance` and
    /// [`Self::start_distance`].
    pub fn far_plane(&self) -> f32 {
        self.view_distance
            .clamp(MIN_VIEW_DISTANCE, MAX_VIEW_DISTANCE)
    }

    /// The sign to apply to a vertical drag.
    pub fn pitch_sign(&self) -> f32 {
        if self.invert_pitch {
            -1.0
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drag across the window is the same turn whatever the window is, which
    /// is the whole reason the setting is per-window rather than per-pixel.
    #[test]
    fn a_full_width_drag_is_the_same_turn_at_any_size() {
        let camera = Camera::default();
        for width in [800.0f32, 1280.0, 1920.0, 3840.0] {
            let across = camera.radians_per_pixel(width) * width;
            assert!(
                (across - std::f32::consts::PI).abs() < 1e-4,
                "a full-width drag turned {across} radians at {width}px"
            );
        }
    }

    /// The slider's ends really do differ, and in the direction the label
    /// claims: a bigger number turns further for the same drag.
    #[test]
    fn a_larger_setting_turns_further() {
        let slow = Camera { turn_per_window: MIN_TURN_PER_WINDOW, ..Default::default() };
        let fast = Camera { turn_per_window: MAX_TURN_PER_WINDOW, ..Default::default() };
        assert!(fast.radians_per_pixel(1920.0) > slow.radians_per_pixel(1920.0) * 4.0);
    }

    /// A value typed into the file cannot freeze or invert the camera.
    ///
    /// `ui.toml` is meant to be hand-edited, so every number in it is an input
    /// from outside and gets the same treatment as one off the wire.
    #[test]
    fn a_hand_edited_value_cannot_break_the_camera() {
        for degrees in [0.0, -720.0, f32::MAX, 1e9] {
            let camera = Camera { turn_per_window: degrees, ..Default::default() };
            let rate = camera.radians_per_pixel(1920.0);
            assert!(
                rate.is_finite() && rate > 0.0,
                "{degrees} degrees gave a rate of {rate}"
            );
            assert!(
                rate <= MAX_TURN_PER_WINDOW.to_radians() / 1920.0 + 1e-6,
                "{degrees} degrees escaped the maximum"
            );
        }
        // And a window that reports no width at all must not divide by it.
        assert!(Camera::default().radians_per_pixel(0.0).is_finite());
    }

    /// The distance is clamped to the range the wheel may reach, or the camera
    /// starts somewhere it can never return to.
    #[test]
    fn the_starting_distance_stays_in_reach_of_the_wheel() {
        for distance in [-5.0, 0.0, 1.0, 9.0, 500.0] {
            let camera = Camera { distance, ..Default::default() };
            let start = camera.start_distance();
            assert!((MIN_DISTANCE..=MAX_DISTANCE).contains(&start), "{distance} gave {start}");
        }
    }

    /// **A view distance of zero draws nothing**, and this value is read from
    /// a file a person can type into -- so it is clamped rather than trusted,
    /// exactly like the turn rate. The default is asserted alongside because
    /// it is the number that decides what a double-click looks like.
    #[test]
    fn the_view_distance_is_clamped_and_defaults_to_the_reference() {
        assert_eq!(Camera::default().far_plane(), DEFAULT_VIEW_DISTANCE);
        for view_distance in [-1.0, 0.0, 1.0, 397.0, 900.0, 99_000.0] {
            let far = Camera { view_distance, ..Default::default() }.far_plane();
            assert!(
                (MIN_VIEW_DISTANCE..=MAX_VIEW_DISTANCE).contains(&far),
                "{view_distance} gave {far}"
            );
        }
        // ...and a value inside the range is passed through untouched, or the
        // clamp would be indistinguishable from ignoring the setting.
        assert_eq!(Camera { view_distance: 600.0, ..Default::default() }.far_plane(), 600.0);
    }

    /// Inverting flips the sign and nothing else.
    #[test]
    fn inverting_only_changes_the_sign() {
        let plain = Camera::default();
        let inverted = Camera { invert_pitch: true, ..Default::default() };
        assert_eq!(plain.pitch_sign(), 1.0);
        assert_eq!(inverted.pitch_sign(), -1.0);
        assert_eq!(
            plain.radians_per_pixel(1920.0),
            inverted.radians_per_pixel(1920.0)
        );
    }
}
