//! What a held movement key means, and what has to be sent when it changes.
//!
//! In the protocol crate rather than in the viewer, even though it is driven
//! by a keyboard, because it is a mapping *onto the wire*: a movement state
//! becomes a set of flags and a change of state becomes a particular opcode.
//! Two callers need that mapping -- the viewer, from held keys, and `wow-cli`,
//! from command-line arguments -- and two copies of it would agree only until
//! one of them changed. Same rule that keeps [`crate::movement::MovementInfo`]
//! a single definition read and written by one piece of code.
//!
//! It is also the part that can be tested without a window or a realm, which
//! matters because **a malformed movement packet is not refused.** The server
//! reads it as some other valid movement and the first sign is the character
//! standing somewhere unexpected.
//!
//! Three ideas do most of the work here.
//!
//! **Movement is two independent axes, not one heading.** Before strafing
//! existed the client had a single `Forward | Backward` state, which cannot
//! express running forward *and* sidestepping -- a thing a player does
//! constantly. So [`Motion`] holds both axes, and [`Motion::transitions`]
//! reports what changed between two of them.
//!
//! **The opcode names the axis that changed; the flags carry the whole
//! state.** Beginning to strafe while already running forward sends
//! `MoveStartStrafeLeft` with *both* the forward and strafe-left bits set. A
//! client that sent only the bit matching the opcode would tell the server it
//! had stopped running the instant it started strafing.
//!
//! **Opposite keys cancel rather than fight.** Holding W and S is not "forward
//! then backward depending on which key repeats last"; it is standing still,
//! which is what the game this is modelled on does and what a player pressing
//! both expects.

use crate::opcode::ClientOpcode;
use crate::update::movement_flags;

/// Which way along an axis, if either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Positive,
    Negative,
}

/// The movement keys, as a state rather than as events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Motion {
    pub forward: bool,
    pub backward: bool,
    pub strafe_left: bool,
    pub strafe_right: bool,
}

impl Motion {
    /// Forward or backward, with both-held cancelling to neither.
    pub fn longitudinal(self) -> Option<Axis> {
        match (self.forward, self.backward) {
            (true, false) => Some(Axis::Positive),
            (false, true) => Some(Axis::Negative),
            _ => None,
        }
    }

    /// Strafing left or right, with both-held cancelling to neither.
    pub fn lateral(self) -> Option<Axis> {
        match (self.strafe_left, self.strafe_right) {
            (true, false) => Some(Axis::Positive),
            (false, true) => Some(Axis::Negative),
            _ => None,
        }
    }

    pub fn is_moving(self) -> bool {
        self.longitudinal().is_some() || self.lateral().is_some()
    }

    /// The movement flags this state sets.
    ///
    /// Derived from the *cancelled* axes rather than from the raw keys, so a
    /// player holding W and S together reports standing still rather than
    /// claiming both directions at once -- which is a combination no real
    /// client sends and no server has to make sense of.
    pub fn flags(self) -> u32 {
        let mut flags = 0;
        match self.longitudinal() {
            Some(Axis::Positive) => flags |= movement_flags::FORWARD,
            Some(Axis::Negative) => flags |= movement_flags::BACKWARD,
            None => {}
        }
        match self.lateral() {
            Some(Axis::Positive) => flags |= movement_flags::STRAFE_LEFT,
            Some(Axis::Negative) => flags |= movement_flags::STRAFE_RIGHT,
            None => {}
        }
        flags
    }

    /// Which direction the character travels, in world axes, for a facing.
    ///
    /// Normalised, so moving diagonally is not faster than moving straight --
    /// the bug every implementation of this gets once, and the one a server
    /// with movement checks would notice before the player did. A stationary
    /// motion gives `(0, 0)` rather than a NaN: normalising a zero vector is
    /// the classic way to put a NaN into a position, and a NaN position is one
    /// the server cannot argue with.
    ///
    /// Forward is `(cos, sin)` of the orientation, matching the terrain's own
    /// axes. Left is that turned a quarter turn anticlockwise, which is what
    /// makes `strafe_left` the positive lateral direction above.
    ///
    /// A plain pair rather than a vector type: this crate is the protocol and
    /// deliberately carries no maths library, so the caller with a `Vec2`
    /// makes one.
    pub fn direction(self, orientation: f32) -> (f32, f32) {
        let (sin, cos) = orientation.sin_cos();
        let mut x = 0.0;
        let mut y = 0.0;
        match self.longitudinal() {
            Some(Axis::Positive) => {
                x += cos;
                y += sin;
            }
            Some(Axis::Negative) => {
                x -= cos;
                y -= sin;
            }
            None => {}
        }
        match self.lateral() {
            Some(Axis::Positive) => {
                x += -sin;
                y += cos;
            }
            Some(Axis::Negative) => {
                x -= -sin;
                y -= cos;
            }
            None => {}
        }
        let length = (x * x + y * y).sqrt();
        if length > f32::EPSILON {
            (x / length, y / length)
        } else {
            (0.0, 0.0)
        }
    }

    /// The opcodes to send when moving from one state to another.
    ///
    /// One per axis that changed, in a fixed order so a given key change always
    /// produces the same stream. An axis that did not change sends nothing:
    /// re-announcing a state the server already holds is what heartbeats are
    /// for, and it is the *transitions* that have to be exact.
    pub fn transitions(before: Motion, after: Motion) -> Vec<ClientOpcode> {
        let mut out = Vec::new();
        if before.longitudinal() != after.longitudinal() {
            out.push(match after.longitudinal() {
                Some(Axis::Positive) => ClientOpcode::MoveStartForward,
                Some(Axis::Negative) => ClientOpcode::MoveStartBackward,
                // Not "stop everything": `MoveStop` ends the forward/backward
                // axis, and a character that is still strafing keeps strafing.
                // The flags sent alongside say which is which.
                None => ClientOpcode::MoveStop,
            });
        }
        if before.lateral() != after.lateral() {
            out.push(match after.lateral() {
                Some(Axis::Positive) => ClientOpcode::MoveStartStrafeLeft,
                Some(Axis::Negative) => ClientOpcode::MoveStartStrafeRight,
                None => ClientOpcode::MoveStopStrafe,
            });
        }
        out
    }
}

/// Gravity, in world units per second squared.
///
/// `19.29110527038574`, which is not a rounded-off constant of convenience but
/// the exact value the server simulates falls with. Getting it wrong does not
/// merely look odd: the server computes fall time and fall damage from the
/// distance dropped, so a client whose arc disagrees reports a `fall_time`
/// that does not match the height it fell, and the two drift further the
/// longer the fall.
pub const GRAVITY: f32 = 19.291_105;

/// How fast a character leaves the ground.
///
/// **Chosen, not measured** -- the same honesty the viewer's `sun_direction`
/// gets, and stated for the same reason: a value that came from judgement
/// rather than from the data has to say so, or the next reader treats it as
/// established.
///
/// The jump impulse is decided by the *client* and sent to the server in the
/// falling block, so no server-side table carries it: `MovementUtil.cpp` has
/// `gravity` and `terminalVelocity` and nothing about jumping. This value is
/// the one commonly cited for 3.3.5a and produces a jump of a believable
/// height against the gravity above, but this project has not confirmed it.
///
/// It can be measured exactly, and here is how, so the next person does not
/// have to work it out: capture a real 3.3.5a client with
/// `wow-cli world --capture`, find a `MSG_MOVE_JUMP` (`0x00BB`), and read the
/// first float of the falling block -- `zspeed`, which
/// `world::movement::Falling` calls `velocity`. One jump settles it.
///
/// Being wrong here fails *visibly* -- the character jumps too high or too
/// low -- which is why it is acceptable to ship a chosen value while a wrong
/// field offset would not be.
pub const JUMP_VELOCITY: f32 = 7.955_8;

/// A jump in progress.
///
/// Tracked locally because the server does not simulate the arc for us: it is
/// told the take-off velocity and the landing, and believes the client in
/// between. That is also why the landing has to be *sent* -- see
/// [`ClientOpcode::MoveFallLand`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Jump {
    /// Height above the ground the jump started from.
    pub height: f32,
    /// Current vertical velocity, negative once falling.
    pub velocity: f32,
    /// Milliseconds airborne, which is what the server reads for fall damage.
    pub elapsed_ms: u32,
    /// The horizontal direction at take-off, decomposed the way the wire wants
    /// it. Held rather than recomputed so every packet in one jump agrees, and
    /// so a jump keeps the heading it began with even if the player turns in
    /// mid-air -- which is what the flags mean by a *jump* direction.
    pub sin_angle: f32,
    pub cos_angle: f32,
    pub xy_speed: f32,
}

impl Jump {
    /// Leaves the ground travelling in `direction` (already normalised, as
    /// [`Motion::direction`] returns it) at `speed`.
    pub fn begin(direction: (f32, f32), speed: f32) -> Self {
        let (x, y) = direction;
        // A standing jump has no horizontal direction to decompose, and
        // normalising a zero vector would give NaN -- which travels into the
        // packet and into the server's copy of where we are.
        let moving = (x * x + y * y).sqrt() > f32::EPSILON;
        let (sin_angle, cos_angle) = if moving { (y, x) } else { (0.0, 1.0) };
        Self {
            height: 0.0,
            velocity: JUMP_VELOCITY,
            elapsed_ms: 0,
            sin_angle,
            cos_angle,
            xy_speed: if moving { speed } else { 0.0 },
        }
    }

    /// Leaves the ground with no upward impulse: the arc of a character who
    /// has walked off a ledge rather than pushed off one.
    ///
    /// The same integration as [`Jump::begin`] from a standing start, which
    /// is what a fall is -- and why both are one type. The caller decides
    /// what a body meets on the way down; see the viewer's `jump_landing`.
    pub fn stepping_off(direction: (f32, f32), speed: f32) -> Self {
        Self {
            velocity: 0.0,
            ..Self::begin(direction, speed)
        }
    }

    /// Advances the arc by `dt` seconds, reporting whether it has come back
    /// down to or below the height it started from.
    ///
    /// **That report is not a landing, and the arc is not clamped there.**
    /// It used to be both: `height` stopped at zero and said "the ground was
    /// reached", which silently made take-off height the lowest a body could
    /// ever be. A character who walked off a ledge therefore could not fall
    /// -- there was no arc able to express it -- and the only thing left to
    /// move them down was the ground snap, which teleports. Reported live as
    /// "falling is instant". What a body meets on the way down is the
    /// caller's business, because only the caller knows what is under it.
    ///
    /// Integrated with the midpoint term (`v*t - g*t^2/2`) rather than by
    /// adding `-g*dt` to the velocity and then stepping the height by it. At
    /// 60fps the difference is small; at a frame rate that stutters it is not,
    /// and a jump whose height depends on the frame rate is the kind of fault
    /// that only appears on someone else's machine.
    pub fn advance(&mut self, dt: f32) -> bool {
        if dt <= 0.0 {
            return false;
        }
        self.height += self.velocity * dt - 0.5 * GRAVITY * dt * dt;
        self.velocity -= GRAVITY * dt;
        self.elapsed_ms = self.elapsed_ms.saturating_add((dt * 1000.0) as u32);
        self.height <= 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moving(forward: bool, backward: bool, left: bool, right: bool) -> Motion {
        Motion {
            forward,
            backward,
            strafe_left: left,
            strafe_right: right,
        }
    }

    /// Opposite keys cancel. Holding both is standing still, not a race
    /// between them -- and the flags have to say so, because a packet claiming
    /// both directions at once is one no real client sends.
    #[test]
    fn opposite_keys_cancel_rather_than_fight() {
        let both = moving(true, true, true, true);
        assert_eq!(both.longitudinal(), None);
        assert_eq!(both.lateral(), None);
        assert_eq!(both.flags(), 0);
        assert!(!both.is_moving());
        assert_eq!(both.direction(0.0), (0.0, 0.0));
    }

    /// The two axes are independent: strafing while running forward has to
    /// carry both bits, or the server is told the character stopped running
    /// the moment it started strafing.
    #[test]
    fn the_axes_combine_rather_than_replace_each_other() {
        let diagonal = moving(true, false, true, false);
        assert_eq!(
            diagonal.flags(),
            movement_flags::FORWARD | movement_flags::STRAFE_LEFT
        );
    }

    /// A transition names only the axis that changed, and beginning to strafe
    /// must not send a stop for the axis still running.
    #[test]
    fn a_transition_names_only_the_axis_that_changed() {
        let running = moving(true, false, false, false);
        let running_and_strafing = moving(true, false, true, false);

        assert_eq!(
            Motion::transitions(running, running_and_strafing),
            vec![ClientOpcode::MoveStartStrafeLeft]
        );
        assert_eq!(
            Motion::transitions(running_and_strafing, running),
            vec![ClientOpcode::MoveStopStrafe]
        );
        // Letting go of everything at once ends both axes, in a fixed order.
        assert_eq!(
            Motion::transitions(running_and_strafing, Motion::default()),
            vec![ClientOpcode::MoveStop, ClientOpcode::MoveStopStrafe]
        );
        // And an unchanged state says nothing at all.
        assert!(Motion::transitions(running, running).is_empty());
    }

    /// Reversing an axis in one frame -- letting go of W while already holding
    /// S -- is one transition to the new direction, not a stop and a start.
    #[test]
    fn reversing_an_axis_is_a_single_transition() {
        assert_eq!(
            Motion::transitions(moving(true, false, false, false), moving(false, true, false, false)),
            vec![ClientOpcode::MoveStartBackward]
        );
    }

    /// Forward is `(cos, sin)` and left is a quarter turn anticlockwise from
    /// it, matching the axes the terrain and the camera already use.
    fn near(actual: (f32, f32), expected: (f32, f32)) -> bool {
        (actual.0 - expected.0).abs() < 1e-5 && (actual.1 - expected.1).abs() < 1e-5
    }

    fn length(v: (f32, f32)) -> f32 {
        (v.0 * v.0 + v.1 * v.1).sqrt()
    }

    #[test]
    fn forward_and_left_point_where_the_rest_of_the_client_thinks() {
        let forward = moving(true, false, false, false).direction(0.0);
        assert!(near(forward, (1.0, 0.0)), "{forward:?}");

        let left = moving(false, false, true, false).direction(0.0);
        assert!(near(left, (0.0, 1.0)), "{left:?}");

        // A quarter turn puts forward where left was.
        let turned = moving(true, false, false, false).direction(std::f32::consts::FRAC_PI_2);
        assert!(near(turned, (0.0, 1.0)), "{turned:?}");
    }

    /// Diagonal movement must not be faster than straight movement -- the bug
    /// every implementation of this gets exactly once.
    #[test]
    fn a_diagonal_is_not_faster_than_a_straight_line() {
        let straight = moving(true, false, false, false).direction(0.7);
        let diagonal = moving(true, false, true, false).direction(0.7);
        assert!((length(straight) - 1.0).abs() < 1e-5);
        assert!((length(diagonal) - 1.0).abs() < 1e-5, "{}", length(diagonal));
    }

    /// A jump goes up, comes down, and lands -- and reports the landing once.
    #[test]
    fn a_jump_rises_and_lands() {
        let mut jump = Jump::begin((1.0, 0.0), 7.0);
        assert_eq!(jump.height, 0.0);

        let mut landed = false;
        let mut peak: f32 = 0.0;
        let mut steps = 0;
        while !landed && steps < 10_000 {
            landed = jump.advance(1.0 / 60.0);
            peak = peak.max(jump.height);
            steps += 1;
        }

        assert!(landed, "the jump never came back down");
        // v^2 / 2g, which for the constants above is a bit over 1.6 units.
        let expected_peak = JUMP_VELOCITY * JUMP_VELOCITY / (2.0 * GRAVITY);
        assert!(
            (peak - expected_peak).abs() < 0.05,
            "peaked at {peak}, expected about {expected_peak}"
        );
        assert!(
            jump.height <= 0.0,
            "a jump that has run its course is back at or below take-off, not {}",
            jump.height
        );
        // 2v/g seconds in the air, in milliseconds.
        let expected_ms = (2.0 * JUMP_VELOCITY / GRAVITY * 1000.0) as u32;
        assert!(
            jump.elapsed_ms.abs_diff(expected_ms) < 50,
            "airborne {}ms, expected about {expected_ms}ms",
            jump.elapsed_ms
        );
    }

    /// **An arc has to be able to pass below where it began, or nothing can
    /// fall.** The height used to clamp at zero, which made take-off the
    /// floor of the whole model: a character who walked off a ledge had no
    /// arc that could carry them down, and the ground snap teleported them
    /// instead. Reported live as "falling is instant".
    #[test]
    fn an_arc_carries_on_below_the_height_it_started_from() {
        let mut fall = Jump::stepping_off((1.0, 0.0), 7.0);
        assert_eq!(fall.velocity, 0.0, "a fall begins with no upward push");
        assert_eq!(fall.height, 0.0);

        // A second of falling, at sixty frames to the second.
        for _ in 0..60 {
            fall.advance(1.0 / 60.0);
        }
        // s = -gt^2/2, a little under ten units in the first second.
        let expected = -0.5 * GRAVITY;
        assert!(
            (fall.height - expected).abs() < 0.2,
            "fell to {} in a second, expected about {expected}",
            fall.height
        );
        assert!(fall.velocity < 0.0, "a falling body is still gaining speed");
        assert!(
            fall.elapsed_ms.abs_diff(1000) < 50,
            "airborne {}ms, expected about a second",
            fall.elapsed_ms
        );
    }

    /// The arc must not depend on the frame rate. A jump that goes higher on a
    /// slow machine is a fault that only ever appears on someone else's.
    #[test]
    fn the_arc_does_not_depend_on_the_frame_rate() {
        fn peak_at(fps: f32) -> f32 {
            let mut jump = Jump::begin((0.0, 0.0), 0.0);
            let mut peak: f32 = 0.0;
            while !jump.advance(1.0 / fps) {
                peak = peak.max(jump.height);
            }
            peak
        }
        let smooth = peak_at(240.0);
        let choppy = peak_at(20.0);
        assert!(
            (smooth - choppy).abs() < 0.05,
            "240fps peaked at {smooth}, 20fps at {choppy}"
        );
    }

    /// A standing jump has no direction to decompose, and normalising a zero
    /// vector would put a NaN in the packet -- and therefore in the server's
    /// copy of where we are.
    #[test]
    fn a_standing_jump_carries_no_nan() {
        let jump = Jump::begin((0.0, 0.0), 7.0);
        assert!(jump.sin_angle.is_finite() && jump.cos_angle.is_finite());
        assert_eq!(jump.xy_speed, 0.0, "a standing jump is not travelling");
        // The angle still has to be a unit vector, whatever it points at.
        let length = (jump.sin_angle.powi(2) + jump.cos_angle.powi(2)).sqrt();
        assert!((length - 1.0).abs() < 1e-5);
    }

    /// A running jump keeps its take-off heading as a unit vector, which is
    /// what `sin_angle`/`cos_angle` mean on the wire.
    #[test]
    fn a_running_jump_decomposes_its_heading() {
        let direction = Motion {
            forward: true,
            ..Motion::default()
        }
        .direction(std::f32::consts::FRAC_PI_4);
        let jump = Jump::begin(direction, 7.0);
        let length = (jump.sin_angle.powi(2) + jump.cos_angle.powi(2)).sqrt();
        assert!((length - 1.0).abs() < 1e-5, "not a unit heading: {length}");
        assert_eq!(jump.xy_speed, 7.0);
    }
}
