//! The burst that plays once, the moment the character reaches a new level.
//!
//! Screen-anchored like [`crate::frames::status_text`] rather than
//! world-anchored like [`crate::frames::quest_mark`]: this has no creature to
//! follow, and pinning it to the world would let a badly-timed camera turn
//! carry the one moment this client is supposed to make memorable off
//! screen. Driven purely by elapsed time, exactly the shape `status_text`
//! and [`crate::frames::loot_sparkle`] already use -- no clock of its own, no
//! world state, nothing here a synthetic egui context could not reproduce.

use egui::{Align2, Color32, FontId, Painter, Pos2};

/// How long the whole effect plays, in seconds. Chosen by looking: long
/// enough that a glance at the corner of the screen still catches it, short
/// enough that it is a moment and not a window that lingers over whatever
/// the player does next.
pub const LEVEL_UP_DURATION: f32 = 2.4;

/// How many sparkle motes ring the burst. Enough to read as a burst rather
/// than a scatter of dots, few enough that the eye can still follow an
/// individual one rising.
const SPARKLES: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub struct LevelUpEffect {
    pub level: u32,
    /// Seconds since the level-up happened, on the caller's own clock.
    pub elapsed: f32,
}

/// `0.0` the instant the effect starts, `1.0` the instant it is done --
/// every curve below is stated in terms of this rather than of
/// `elapsed`/[`LEVEL_UP_DURATION`] directly, so there is exactly one place a
/// wrong duration could hide.
fn progress(elapsed: f32) -> f32 {
    (elapsed / LEVEL_UP_DURATION).clamp(0.0, 1.0)
}

/// Draws the burst, or nothing at all once `entry.elapsed` has run past
/// [`LEVEL_UP_DURATION`] -- the caller is not required to stop asking once
/// the effect is over, the same courtesy `status_text::draw` and
/// `loot_sparkle::draw` already extend.
pub fn draw(painter: &Painter, entry: &LevelUpEffect) {
    let t = progress(entry.elapsed);
    if t >= 1.0 {
        return;
    }
    let centre = painter.clip_rect().center();

    // **The flash.** A soft, fast-fading disc, front-loaded into the first
    // fifth of the effect -- the instant the ding lands, not something that
    // lingers and washes out everything drawn after it.
    let flash = (1.0 - t * 5.0).clamp(0.0, 1.0);
    if flash > 0.0 {
        let radius = painter.clip_rect().height() * (0.08 + 0.25 * flash);
        painter.circle_filled(
            centre,
            radius,
            Color32::from_rgba_unmultiplied(255, 240, 180, (flash * 90.0) as u8),
        );
    }

    // **The rings.** Two, offset in phase so the second is still expanding
    // when the first fades out -- a single ring reads as a flash, two reads
    // as a burst.
    for phase_offset in [0.0, 0.35] {
        let ring_t = ring_progress(t, phase_offset);
        if ring_t <= 0.0 {
            continue;
        }
        let radius = 20.0 + ring_t * 140.0;
        let alpha = ring_alpha(ring_t);
        painter.circle_stroke(
            centre,
            radius,
            egui::Stroke::new(2.5, Color32::from_rgba_unmultiplied(255, 215, 0, alpha)),
        );
    }

    // **The sparkles.** Spread evenly around the circle by index rather than
    // by anything random, so the shape is stable frame to frame -- a
    // twinkling *ring*, not noise. Each rises and drifts outward as `t`
    // advances, and fades over the same `t` the rings use so nothing
    // outlives the banner it is meant to accompany.
    let sparkle_alpha = ((1.0 - t) * 220.0) as u8;
    for i in 0..SPARKLES {
        let pos = sparkle_offset(centre, i, t);
        painter.circle_filled(pos, 2.5, Color32::from_rgba_unmultiplied(255, 230, 140, sparkle_alpha));
    }

    // **The banner.** Outlined the way `quest_mark::draw` outlines its
    // glyph: a single drop shadow disappears against exactly one background,
    // and this has to read over grass, stone and sky in turn just like that
    // mark does.
    let banner_alpha = ((1.0 - t).powf(0.6) * 255.0) as u8;
    let at = Pos2::new(centre.x, centre.y - 70.0 - t * 12.0);
    let text = format!("Level {}!", entry.level);
    let font = FontId::proportional(34.0);
    for (dx, dy) in [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)] {
        painter.text(
            Pos2::new(at.x + dx, at.y + dy),
            Align2::CENTER_CENTER,
            &text,
            font.clone(),
            Color32::from_black_alpha(banner_alpha),
        );
    }
    painter.text(
        at,
        Align2::CENTER_CENTER,
        &text,
        font,
        Color32::from_rgba_unmultiplied(255, 215, 0, banner_alpha),
    );
}

/// A ring's own progress, given the whole effect's `t` and how far into it
/// this ring is staggered to start. Clamped at both ends: a ring due to
/// start later than `t` has not begun (`0.0`, drawn as nothing by the
/// caller's `<= 0.0` guard), and one whose own span has finished sits at
/// `1.0` rather than overshooting past the point its formulas were tuned for.
fn ring_progress(t: f32, phase_offset: f32) -> f32 {
    let start = phase_offset * 0.3;
    ((t - start).max(0.0) / (1.0 - start)).clamp(0.0, 1.0)
}

/// A ring's alpha at its own progress -- fully opaque (up to the constant
/// below) at birth, gone by the time it finishes expanding.
fn ring_alpha(ring_t: f32) -> u8 {
    ((1.0 - ring_t) * 200.0) as u8
}

/// Where one sparkle sits at the burst's progress `t`: spread evenly by
/// index around the centre, drifting outward and up as `t` advances.
fn sparkle_offset(centre: Pos2, index: usize, t: f32) -> Pos2 {
    let angle = (index as f32 / SPARKLES as f32) * std::f32::consts::TAU;
    let reach = 30.0 + t * 90.0;
    let rise = t * 60.0;
    Pos2::new(
        centre.x + angle.cos() * reach,
        centre.y + angle.sin() * reach * 0.6 - rise,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one place `LEVEL_UP_DURATION` is divided by -- everything else in
    /// this module reads `t`, so a wrong duration can only ever break the
    /// mapping tested here.
    #[test]
    fn progress_is_zero_at_the_start_and_one_once_its_over() {
        assert_eq!(progress(0.0), 0.0);
        assert_eq!(progress(LEVEL_UP_DURATION), 1.0);
        assert_eq!(progress(LEVEL_UP_DURATION + 10.0), 1.0, "must not overshoot 1.0");
        assert_eq!(progress(-5.0), 0.0, "must not undershoot 0.0 on a negative clock");
    }

    /// `draw`'s only guard is `t >= 1.0`; confirm the boundary agrees with
    /// `progress` rather than drifting from it after an edit to one side.
    #[test]
    fn the_guard_fires_exactly_when_progress_reaches_one() {
        assert!(progress(LEVEL_UP_DURATION) >= 1.0);
        assert!(progress(LEVEL_UP_DURATION - 0.01) < 1.0);
    }

    /// Every alpha this module computes has to stay in `u8` range across the
    /// whole run, sampled densely enough that a curve which briefly dips
    /// negative or spikes past 255 could not hide between samples.
    #[test]
    fn every_alpha_stays_in_range_across_the_whole_effect() {
        for hundredth in 0..=100 {
            let t = hundredth as f32 / 100.0;
            for phase_offset in [0.0, 0.35] {
                let ring_t = ring_progress(t, phase_offset);
                assert!((0.0..=1.0).contains(&ring_t), "ring_t {ring_t} at t={t}");
                let alpha = ring_alpha(ring_t);
                assert!(alpha as u32 <= 200, "ring alpha {alpha} out of range at t={t}");
            }
            let flash = (1.0 - t * 5.0).clamp(0.0, 1.0);
            assert!((0.0..=1.0).contains(&flash), "flash {flash} at t={t}");
            let banner_alpha = (1.0 - t).powf(0.6);
            assert!((0.0..=1.0).contains(&banner_alpha), "banner_alpha {banner_alpha} at t={t}");
        }
    }

    /// The sparkles are a *stable ring*, not noise -- two calls at the same
    /// `t` must place the same index at the same point, and the ring must
    /// actually expand as `t` advances rather than sitting still.
    #[test]
    fn sparkles_are_stable_and_expand_outward() {
        let centre = Pos2::new(100.0, 100.0);
        let early = sparkle_offset(centre, 0, 0.1);
        let early_again = sparkle_offset(centre, 0, 0.1);
        assert_eq!(early, early_again, "the same index and t must land on the same point");

        let late = sparkle_offset(centre, 0, 0.9);
        let early_reach = (early.x - centre.x).hypot(early.y - centre.y);
        let late_reach = (late.x - centre.x).hypot(late.y - centre.y);
        assert!(
            late_reach > early_reach,
            "a sparkle should be farther from centre later in the effect: {early_reach} -> {late_reach}"
        );
    }
}
