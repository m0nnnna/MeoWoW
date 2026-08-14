//! Resolving the lighting that applies at a place and an hour.
//!
//! Lives here, beside the tables, rather than in the renderer, because
//! `wow-cli light` is how the renderer's lighting gets checked -- and a
//! verification tool that computes its numbers a second way stops being
//! evidence about the first. Same rule as unprojecting the picking ray from the
//! matrix the scene was drawn with: independence is what you want when checking
//! whether something is *correct*, and a liability when two things must *stay*
//! consistent.
//!
//! **What the eighteen colour bands mean is not decided here.** They are read
//! and handed out by index. See [`Bands`] for what has been established about
//! them, which is less than it is tempting to assume.

use crate::schema::{
    float_band_id, int_band_id, Light, LightFloatBand, LightIntBand, LightParams, LightRow,
    DAY_HALF_MINUTES, FLOAT_BANDS_PER_PARAMS, INT_BANDS_PER_PARAMS,
};

/// A colour from a band, in 0..1 per channel.
pub type Colour = [f32; 3];

/// Which curve is which.
///
/// **These are a hypothesis under test, not a transcription.** What is actually
/// established, by sampling every band across a day:
///
/// - Blue is the low byte of a packed colour. Sky bands read blue-first give a
///   near-black-blue midnight, an orange dawn and a blue noon; red-first gives a
///   brown midnight and a blue dawn. One of those is a sky.
/// - Band 6 is the direct light: neutral grey (180, 180, 180) at noon, dim
///   blue (49, 86, 123) at midnight. Confirmed by rendering.
/// - Band 1 behaves like an ambient term: dark blue at midnight (29, 60, 84),
///   blue-grey by day (104, 130, 154), tracking the sun's presence.
/// - Band 0 is bright at every hour -- its brightness barely moves across the
///   day -- and shifts hue rather than intensity. It is *not* the direct light,
///   which a render settled: used as one, midday Elwynn had olive grass and an
///   orange road.
/// - Bands 3, 4 and 5 track the sky, brightening into noon.
/// - Band 12 has a single key of black on Azeroth's default light: unused.
///
/// Which of those is *diffuse* is the open question, and a wrong answer here
/// does not fail loudly -- it just makes the world the wrong colour, which is
/// why the constants are named in one place and rendered rather than reasoned
/// about. See `docs/RENDERING.md`.
pub mod bands {
    /// The direct light. **Band 0 was the first guess and a render refused
    /// it**: it reads (255, 136, 0) at noon, and Elwynn came back with olive
    /// grass and an orange road. Band 6 is neutral grey (180, 180, 180) at
    /// noon and dim blue (49, 86, 123) at midnight, which is what a sun that
    /// becomes a moon looks like.
    pub const DIFFUSE: u32 = 6;
    /// The ambient fill: dark blue (29, 60, 84) at midnight, blue-grey
    /// (104, 130, 154) by day. Paired with [`DIFFUSE`] it is the ordinary
    /// outdoor arrangement -- a neutral key light and a sky-coloured fill --
    /// and the pair lands close to the fixed 0.38/0.62 placeholder this
    /// replaced, which is a useful sanity check rather than a coincidence.
    pub const AMBIENT: u32 = 1;
    /// The colour to clear the sky to.
    ///
    /// Of the several bands that track the sky, this is the one that reads as
    /// the bulk of it: pale blue (153, 220, 245) at noon, dark blue (0, 40, 78)
    /// at midnight, orange through dawn. Not "the sky band" -- there are
    /// several and they layer into a gradient this client does not draw -- but
    /// the one that makes a flat clear colour look like the right sky.
    pub const SKY: u32 = 4;
    /// Fog colour. **Still unconfirmed, and currently invisible**: the fog
    /// distance band reads 18,000 units on Azeroth and fog starts at a quarter
    /// of that, so nothing within several kilometres of the camera is affected.
    /// Left pointing at a plausible band rather than at nothing, because the
    /// alternative -- disabling fog outright -- would hide the day the distance
    /// band turns out to mean something else.
    pub const FOG: u32 = 2;
}

/// Scalar curves, by index. Only the first has been identified with any
/// confidence, and only by magnitude: it reads 18,000 on Azeroth, which is a
/// distance and not a fraction, where bands 1 to 4 are all between 0 and 1.
pub mod scalars {
    /// Believed to be the distance at which fog is total.
    pub const FOG_END: u32 = 0;
    /// Believed to scale where fog begins, as a fraction of [`FOG_END`].
    pub const FOG_START_SCALER: u32 = 1;
}

/// Every lighting table, read once.
pub struct Lighting {
    lights: Light,
    params: LightParams,
    int_bands: LightIntBand,
    float_bands: LightFloatBand,
}

/// The lighting in force somewhere, at some time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub diffuse: Colour,
    pub ambient: Colour,
    pub fog: Colour,
    /// What to clear the sky to.
    pub sky: Colour,
    /// Distance at which fog is total, in world units.
    pub fog_end: f32,
    /// Where fog begins, in world units.
    pub fog_start: f32,
    /// Which `LightParams` row this came from, for reporting: "the wrong light
    /// was chosen" and "this light looks like that" are indistinguishable
    /// without it.
    pub params_id: u32,
}

impl Lighting {
    /// `read` is `FnMut` because reading an MPQ advances the archive's own
    /// state; a `Fn` here would force every caller to wrap its chain.
    pub fn load(mut read: impl FnMut(&str) -> Option<Vec<u8>>) -> Option<Self> {
        Some(Self {
            lights: Light::parse(&read(Light::PATH)?).ok()?,
            params: LightParams::parse(&read(LightParams::PATH)?).ok()?,
            int_bands: LightIntBand::parse(&read(LightIntBand::PATH)?).ok()?,
            float_bands: LightFloatBand::parse(&read(LightFloatBand::PATH)?).ok()?,
        })
    }

    /// Which `LightParams` applies at a position, and how far the chosen light
    /// is from it.
    ///
    /// Lights are positional: a row sits at a point with an inner and outer
    /// radius, and a map also has a default whose position is the origin and
    /// whose radius is zero. Northshire is covered by none of Azeroth's 82
    /// lights -- the nearest is 124,000 units away -- so the default is not an
    /// edge case, it is the common case.
    pub fn params_at(&self, map_id: u32, x: f32, y: f32) -> Option<(u32, f32)> {
        self.params_at_in(map_id, x, y, false).map(|(id, _, d)| (id, d))
    }

    /// The same, choosing between the clear and stormy sets of curves.
    ///
    /// Returns both, because a client does not switch between them -- it blends
    /// by how hard the weather is coming down, and needs the two ends to
    /// interpolate.
    ///
    /// **The storm column is the storm column, and the population statistic
    /// nearly said otherwise.** Across 200 outdoor lights the stormy row is
    /// darker only 55% of the time and pulls the fog *in* only 47% of the time,
    /// which is a coin flip and looked like a refutation. It was the sloppy
    /// version of the question: most positioned lights are decorative -- a
    /// glowing crater, a haunted wood -- and their weather columns are authored
    /// for effect. The row that matters is the one that actually lights a zone,
    /// and asking about *that* is unambiguous. Map 0's default light names
    /// clear params 12 and storm params 10, and row 10 is a flat neutral grey
    /// (0.32, 0.33, 0.32) at **every hour of the day**, with fog ending at
    /// 10,000 against clear's 18,000. No dawn orange, no midday white, no
    /// sunset. Map 1's default names the same row 10.
    pub fn params_at_in(
        &self,
        map_id: u32,
        x: f32,
        y: f32,
        storm: bool,
    ) -> Option<(u32, u32, f32)> {
        let pick = |row: &LightRow| {
            let clear = row.params_clear();
            // A light with no stormy row of its own keeps its clear one, which
            // is right rather than a gap: 337 of them share the two, and
            // blending a row with itself is the identity.
            let bad = if storm && row.params_storm() != 0 {
                row.params_storm()
            } else {
                clear
            };
            (clear, bad)
        };

        let mut best: Option<(f32, (u32, u32))> = None;
        let mut default: Option<(u32, u32)> = None;
        for row in self.lights.iter() {
            if row.map_id() != map_id {
                continue;
            }
            if row.x() == 0.0 && row.y() == 0.0 && row.z() == 0.0 {
                default = Some(pick(&row));
                continue;
            }
            let (dx, dy) = (row.x() - x, row.y() - y);
            let distance = (dx * dx + dy * dy).sqrt();
            if distance <= row.falloff_end() && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, pick(&row)));
            }
        }
        best.map(|(d, (clear, bad))| (clear, bad, d))
            .or_else(|| default.map(|(clear, bad)| (clear, bad, f32::INFINITY)))
    }

    /// The lighting at a position and a time of day.
    ///
    /// `minute_of_day` is 0..1440, as [`crate::schema`]'s band times are
    /// half-minutes; the conversion happens here so no caller has to remember
    /// which unit it is holding.
    pub fn sample(&self, map_id: u32, x: f32, y: f32, minute_of_day: u32) -> Option<Sample> {
        self.sample_in(map_id, x, y, minute_of_day, 0.0)
    }

    /// The lighting at a position and a time, under weather.
    ///
    /// `storm` is how far towards the stormy curves to go, 0 to 1 -- the
    /// intensity `SMSG_WEATHER` reports. Blended rather than switched because
    /// the server eases weather in and out, and a client that jumped between
    /// two sets of curves would turn the sky grey between one frame and the
    /// next.
    ///
    /// Every field is interpolated, including the fog distances: a storm pulls
    /// the horizon from 18,000 units to 10,000, and that is most of what makes
    /// rain feel like rain.
    pub fn sample_in(
        &self,
        map_id: u32,
        x: f32,
        y: f32,
        minute_of_day: u32,
        storm: f32,
    ) -> Option<Sample> {
        let storm = storm.clamp(0.0, 1.0);
        let clear = self.sample_from(map_id, x, y, minute_of_day, false)?;
        if storm <= 0.0 {
            return Some(clear);
        }
        let stormy = self.sample_from(map_id, x, y, minute_of_day, true)?;
        let mix = |a: f32, b: f32| a + (b - a) * storm;
        let mix3 = |a: Colour, b: Colour| {
            [
                mix(a[0], b[0]),
                mix(a[1], b[1]),
                mix(a[2], b[2]),
            ]
        };
        Some(Sample {
            diffuse: mix3(clear.diffuse, stormy.diffuse),
            ambient: mix3(clear.ambient, stormy.ambient),
            fog: mix3(clear.fog, stormy.fog),
            sky: mix3(clear.sky, stormy.sky),
            fog_end: mix(clear.fog_end, stormy.fog_end),
            fog_start: mix(clear.fog_start, stormy.fog_start),
            // The row actually being blended towards, so a report can say which
            // curves are in play rather than only where they came from.
            params_id: if storm >= 0.5 { stormy.params_id } else { clear.params_id },
        })
    }

    fn sample_from(
        &self,
        map_id: u32,
        x: f32,
        y: f32,
        minute_of_day: u32,
        storm: bool,
    ) -> Option<Sample> {
        let (clear_id, storm_id, _) = self.params_at_in(map_id, x, y, storm)?;
        let params_id = if storm { storm_id } else { clear_id };
        let at = (minute_of_day * 2) % DAY_HALF_MINUTES;
        let fog_end = self
            .scalar(params_id, scalars::FOG_END, at)
            .unwrap_or(1000.0);
        Some(Sample {
            diffuse: self.colour(params_id, bands::DIFFUSE, at).unwrap_or([1.0; 3]),
            ambient: self.colour(params_id, bands::AMBIENT, at).unwrap_or([0.35; 3]),
            fog: self.colour(params_id, bands::FOG, at).unwrap_or([0.6, 0.7, 0.8]),
            sky: self
                .colour(params_id, bands::SKY, at)
                // The old fixed clear colour, so a light with no sky band
                // looks exactly as it did before rather than black.
                .unwrap_or([0.42, 0.55, 0.70]),
            fog_end,
            // A scaler of 0.25 with a 18,000 end puts fog's start a quarter of
            // the way out. Guarded against a scaler of zero, which would put
            // the start at the camera and fog the whole world.
            fog_start: fog_end
                * self
                    .scalar(params_id, scalars::FOG_START_SCALER, at)
                    .unwrap_or(0.25)
                    .clamp(0.05, 0.95),
            params_id,
        })
    }

    /// One colour curve, sampled. `at` is in half-minutes.
    pub fn colour(&self, params_id: u32, band: u32, at: u32) -> Option<Colour> {
        debug_assert!(band < INT_BANDS_PER_PARAMS);
        let id = int_band_id(params_id, band);
        let row = self.int_bands.iter().find(|row| row.id() == id)?;
        let packed = sample_keys(row.count(), at, |index| row.key(index))?;
        Some(unpack(packed))
    }

    /// One scalar curve, sampled.
    pub fn scalar(&self, params_id: u32, band: u32, at: u32) -> Option<f32> {
        debug_assert!(band < FLOAT_BANDS_PER_PARAMS);
        let id = float_band_id(params_id, band);
        let row = self.float_bands.iter().find(|row| row.id() == id)?;
        sample_keys(row.count(), at, |index| row.key(index))
    }

    pub fn params(&self) -> &LightParams {
        &self.params
    }

    pub fn lights(&self) -> &Light {
        &self.lights
    }
}

/// Blue is the low byte -- see [`bands`] for what settled that.
pub fn unpack(packed: u32) -> Colour {
    [
        ((packed >> 16) & 0xFF) as f32 / 255.0,
        ((packed >> 8) & 0xFF) as f32 / 255.0,
        (packed & 0xFF) as f32 / 255.0,
    ]
}

/// Samples a curve of up to sixteen keys, holding at both ends.
///
/// Holding rather than wrapping at the end of the day is deliberate: the curves
/// are authored with a key at or near midnight, and wrapping back to the first
/// key would make the sky lurch at 23:59 instead of settling.
fn sample_keys<T>(count: u32, at: u32, key: impl Fn(usize) -> Option<(u32, T)>) -> Option<T>
where
    T: Lerp + Copy,
{
    if count == 0 {
        return None;
    }
    let mut previous = key(0)?;
    if at <= previous.0 {
        return Some(previous.1);
    }
    for index in 1..count as usize {
        let next = key(index)?;
        if at <= next.0 {
            let span = next.0.saturating_sub(previous.0).max(1);
            let t = (at - previous.0) as f32 / span as f32;
            return Some(previous.1.lerp(next.1, t));
        }
        previous = next;
    }
    Some(previous.1)
}

/// Interpolation for the two things a band can hold.
pub trait Lerp {
    fn lerp(self, to: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, to: Self, t: f32) -> Self {
        self + (to - self) * t
    }
}

impl Lerp for u32 {
    /// Packed colours blend per channel, not as integers. Blending the packed
    /// words directly would carry between channels and produce colours in
    /// neither key.
    fn lerp(self, to: Self, t: f32) -> Self {
        let mut out = 0u32;
        for shift in [0, 8, 16] {
            let a = ((self >> shift) & 0xFF) as f32;
            let b = ((to >> shift) & 0xFF) as f32;
            out |= ((a + (b - a) * t).round().clamp(0.0, 255.0) as u32) << shift;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blue_is_the_low_byte() {
        // The noon sky value that settled it.
        let [r, g, b] = unpack(0x3aa2cf);
        assert!(b > r, "a midday sky is blue, not ochre: got {r},{g},{b}");
        assert_eq!(
            (
                (r * 255.0).round() as u32,
                (g * 255.0).round() as u32,
                (b * 255.0).round() as u32
            ),
            (58, 162, 207)
        );
    }

    #[test]
    fn a_colour_blends_per_channel_not_as_an_integer() {
        // Halfway between pure blue and pure red must be a dark magenta, not
        // whatever the packed words average to.
        let half = 0x0000ffu32.lerp(0xff0000, 0.5);
        assert_eq!(unpack(half).map(|c| (c * 255.0).round() as u32), [128, 0, 128]);
    }

    #[test]
    fn a_curve_holds_at_both_ends_and_interpolates_between() {
        let keys = [(0u32, 0.0f32), (1440, 10.0), (2880, 0.0)];
        let at = |index: usize| keys.get(index).copied();
        assert_eq!(sample_keys(3, 0, at), Some(0.0));
        assert_eq!(sample_keys(3, 720, at), Some(5.0));
        assert_eq!(sample_keys(3, 1440, at), Some(10.0));
        assert_eq!(sample_keys(3, 2160, at), Some(5.0));
        // Past the last key it holds rather than wrapping to the first.
        assert_eq!(sample_keys(3, 5000, at), Some(0.0));
        // A band with no keys contributes nothing rather than defaulting.
        assert_eq!(sample_keys(0, 100, at), None);
    }
}
