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
//! **What the eighteen colour bands mean is mostly not decided here.** They are
//! read and handed out by index. See [`bands`] for what has been established
//! about them, which is less than it is tempting to assume -- with one
//! exception: five of them are the sky, and that one is settled.

use crate::schema::{
    float_band_id, int_band_id, Light, LightFloatBand, LightIntBand, LightParams, LightRow,
    LightSkybox, DAY_HALF_MINUTES, FLOAT_BANDS_PER_PARAMS, INT_BANDS_PER_PARAMS,
};

/// A colour from a band, in 0..1 per channel.
pub type Colour = [f32; 3];

/// The sky, zenith first and horizon last -- see [`bands::SKY`].
///
/// Five colours rather than one because that is how the table says it: the
/// world's sky is a stack of layers, and collapsing it to a single clear colour
/// was this client's approximation, not the data's.
pub type SkyGradient = [Colour; bands::SKY.len()];

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
/// - **Bands 2 to 6 are the sky, in order from zenith to horizon.** See
///   [`SKY`]; this is the one band assignment here that is not a hypothesis.
/// - Band 12 has a single key of black on Azeroth's default light: unused.
///
/// Which of those is *diffuse* is the open question, and a wrong answer here
/// does not fail loudly -- it just makes the world the wrong colour, which is
/// why the constants are named in one place and rendered rather than reasoned
/// about. See `docs/RENDERING.md`.
pub mod bands {
    /// The sky, zenith first, horizon last.
    ///
    /// **Measured, and the measurement is not close.** At noon on Azeroth's
    /// default light these five read
    ///
    /// ```text
    /// 2 (  0, 31, 73)   3 ( 58,162,207)   4 (153,220,245)   5 (175,218,224)   6 (180,180,180)
    /// ```
    ///
    /// -- a deep blue overhead whitening into a grey haze. Red climbs
    /// monotonically across all five while the spread between the channels
    /// falls away to *exactly* neutral at the last one: 73, 149, 92, 49, 0. At
    /// midnight the same five run (0,0,0), (0,12,32), (0,40,78), (27,70,112),
    /// (49,86,123) -- monotone in all three channels, black overhead and
    /// faintly blue low down.
    ///
    /// **Dawn is what makes it unarguable**, because a sunrise is the one hour
    /// when a sky is not a simple ramp. At 06:00 they read (35,74,84),
    /// (68,140,128), (210,121,72), (255,171,64), (255,202,76): cool teal
    /// overhead, then a warm band that arrives abruptly at index 2 and
    /// brightens to yellow. Red minus blue runs -49, -60, +138, +191, +179 --
    /// it crosses zero exactly once, and the warm side is the horizon side.
    /// Sunset does the same thing. Nothing but a sky does that, and no other
    /// ordering of these five puts the crossing anywhere sensible.
    ///
    /// It also settles the byte order for good, because swapping the channels
    /// negates that difference: red-first, the sunrise would be directly
    /// overhead and the zenith would be the neutral end at noon.
    pub const SKY: [u32; 5] = [2, 3, 4, 5, 6];
    /// The sun, and the moon: one band for whichever is up.
    ///
    /// **Identified by the property that only it has.** Band 9 is the
    /// brightest band at *every* hour, and its brightness barely moves across
    /// a whole day -- 728, 615, 724, 675 summed over the channels at midnight,
    /// dawn, noon and dusk, where the next brightest is 389 and every band
    /// that lights the world drops to a fraction of itself at night. A curve
    /// that stays bright while the sky goes black is not lighting anything; it
    /// is a thing you look at.
    ///
    /// Its hue then says which thing. Cool white (232, 241, 255) right through
    /// the night, warm (255, 210, 150) at sunrise, warm white (255, 247, 222)
    /// at noon: a moon that becomes a sun. One band serves both because only
    /// one of them is ever up, which is also why this client draws a single
    /// disc and puts it wherever `sun_direction` says -- flipped to the
    /// opposite side of the sky once the sun has set.
    ///
    /// **The glow around it is derived from this band rather than named.**
    /// Band 10 behaves plausibly like a halo -- dim blue at midnight, orange
    /// at dawn -- but nothing has separated it from bands 11 and 13, and a
    /// halo is the disc's own light scattered, so taking the disc's colour and
    /// fading it is derivation instead of a guess. Same reasoning as the fog.
    pub const DISC: u32 = 9;
    /// Where the horizon sits in [`SKY`] and in a sampled [`super::Sample`]'s
    /// gradient. Named because "the last one" is true of the array and not of
    /// the idea.
    ///
    /// **The fog colour is derived from here, and there is no fog band.** Fog
    /// used to point at band 2, which is now known to be the *zenith* -- black
    /// at midnight, deep navy at noon -- so distant ground was fading into the
    /// colour of the sky directly overhead. That is positively refuted rather
    /// than merely unconfirmed, and the replacement is deliberately not another
    /// guess: fog is what the far distance resolves to, the far distance is the
    /// horizon, and this entry is a *measured* statement of what colour the
    /// horizon is at this hour under this weather. Distant terrain now meets
    /// the sky it is drawn against by construction, at every hour, instead of
    /// meeting a band that has to be right.
    ///
    /// Bands 7, 11 and 13 all behave plausibly like a separate fog colour --
    /// band 7 in particular tracks the sky's brightness while staying well
    /// below it, which is what "distant mountains" would look like. Nothing has
    /// separated them from each other, so none of them is named.
    pub const HORIZON: usize = 4;
    /// And the other end.
    pub const ZENITH: usize = 0;

    /// The direct light. **Band 0 was the first guess and a render refused
    /// it**: it reads (255, 136, 0) at noon, and Elwynn came back with olive
    /// grass and an orange road. Band 6 is neutral grey (180, 180, 180) at
    /// noon and dim blue (49, 86, 123) at midnight, which is what a sun that
    /// becomes a moon looks like.
    ///
    /// **It is also the horizon end of [`SKY`], and that is now known.** The
    /// two claims do not conflict so much as explain each other: the colour a
    /// low sun arrives in *is* the colour it has painted the horizon, so a band
    /// authored for one reads correctly as the other. It stays here on the
    /// render's evidence rather than being renamed away, but it is no longer a
    /// band whose meaning is open -- it is a band being borrowed.
    ///
    /// What the table does **not** appear to contain is a sun that dims. Bands
    /// 0 and 9 are the two whose brightness barely moves across a whole day
    /// (389/358/391/373 and 728/615/724/675 summed over the channels), and band
    /// 9's hue -- cool white at midnight, orange at dawn, warm white at noon --
    /// is a sun and moon *disc*, whose contribution to the ground depends on
    /// where it is in the sky rather than on what colour it is. Whether the
    /// direct light should be band 9 modulated by elevation instead is open,
    /// and only a render can answer it.
    pub const DIFFUSE: u32 = 6;
    /// The ambient fill: dark blue (29, 60, 84) at midnight, blue-grey
    /// (104, 130, 154) by day. Paired with [`DIFFUSE`] it is the ordinary
    /// outdoor arrangement -- a neutral key light and a sky-coloured fill --
    /// and the pair lands close to the fixed 0.38/0.62 placeholder this
    /// replaced, which is a useful sanity check rather than a coincidence.
    pub const AMBIENT: u32 = 1;
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

/// The gradient a light with no sky bands gets: the flat blue this client used
/// before it drew a gradient, darkened towards the zenith so that "no data"
/// still reads as a sky rather than as a wall.
pub const DEFAULT_SKY: SkyGradient = [
    [0.20, 0.34, 0.60],
    [0.28, 0.42, 0.65],
    [0.36, 0.50, 0.68],
    [0.42, 0.55, 0.70],
    [0.52, 0.62, 0.72],
];

/// Every lighting table, read once.
pub struct Lighting {
    lights: Light,
    params: LightParams,
    int_bands: LightIntBand,
    float_bands: LightFloatBand,
    /// **Optional where the other four are required, and deliberately.** A
    /// missing skybox table costs the zone backdrops and the star dome --
    /// which is exactly what this client had before either existed. Making it
    /// required would let one absent file take the sky's *colours* with it,
    /// turning a missing feature into a missing world.
    skybox: Option<LightSkybox>,
}

/// The lighting in force somewhere, at some time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub diffuse: Colour,
    pub ambient: Colour,
    /// The sky from zenith to horizon.
    pub sky: SkyGradient,
    /// The sun or moon, whichever is up -- see [`bands::DISC`].
    pub disc: Colour,
    /// Distance at which fog is total, in world units.
    pub fog_end: f32,
    /// Where fog begins, in world units.
    pub fog_start: f32,
    /// Which `LightParams` row this came from, for reporting: "the wrong light
    /// was chosen" and "this light looks like that" are indistinguishable
    /// without it.
    pub params_id: u32,
    /// The `LightSkybox` row this place names, or 0 for none.
    ///
    /// Zero is the common answer and not a gap: Azeroth's default light names
    /// no skybox, which is why the five-band gradient *is* the ordinary
    /// outdoor sky rather than a stand-in for one. Carried as an id rather
    /// than a path so this stays `Copy`; resolve it with
    /// [`Lighting::skybox_model`].
    pub skybox_id: u32,
}

impl Sample {
    /// The sky directly overhead.
    pub fn zenith(&self) -> Colour {
        self.sky[bands::ZENITH]
    }

    /// The sky at the horizon -- and therefore, see [`bands::HORIZON`], the
    /// colour distant terrain fades into.
    pub fn horizon(&self) -> Colour {
        self.sky[bands::HORIZON]
    }

    /// What fog resolves to at its far end. Not a stored field: it *is* the
    /// horizon, and a copy would be a second thing to keep in step.
    pub fn fog(&self) -> Colour {
        self.horizon()
    }
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
            skybox: read(LightSkybox::PATH).and_then(|b| LightSkybox::parse(&b).ok()),
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
        let mut sky = clear.sky;
        // Every layer separately: a storm flattens the gradient as well as
        // greying it, and blending only one band would leave the sky's *shape*
        // clear while its colour rained.
        for (out, (a, b)) in sky.iter_mut().zip(clear.sky.iter().zip(stormy.sky.iter())) {
            *out = mix3(*a, *b);
        }
        Some(Sample {
            diffuse: mix3(clear.diffuse, stormy.diffuse),
            ambient: mix3(clear.ambient, stormy.ambient),
            disc: mix3(clear.disc, stormy.disc),
            sky,
            fog_end: mix(clear.fog_end, stormy.fog_end),
            fog_start: mix(clear.fog_start, stormy.fog_start),
            // The row actually being blended towards, so a report can say which
            // curves are in play rather than only where they came from.
            params_id: if storm >= 0.5 { stormy.params_id } else { clear.params_id },
            // **Not blended, and it cannot be.** A skybox is a model, and
            // there is no halfway between two of them -- so it switches with
            // the row rather than crossfading with the colours. Following the
            // params id keeps the two consistent: whichever row a report names
            // is the row whose sky is on screen.
            skybox_id: if storm >= 0.5 { stormy.skybox_id } else { clear.skybox_id },
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
        let mut sky = DEFAULT_SKY;
        for (out, band) in sky.iter_mut().zip(bands::SKY) {
            // A light with no sky band keeps the fixed gradient rather than
            // going black -- and per layer, because a row could fill some and
            // not others.
            if let Some(colour) = self.colour(params_id, band, at) {
                *out = colour;
            }
        }
        Some(Sample {
            diffuse: self.colour(params_id, bands::DIFFUSE, at).unwrap_or([1.0; 3]),
            ambient: self.colour(params_id, bands::AMBIENT, at).unwrap_or([0.35; 3]),
            disc: self.colour(params_id, bands::DISC, at).unwrap_or([1.0; 3]),
            sky,
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
            skybox_id: self.skybox_of(params_id),
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

    /// Which `LightSkybox` row a params row names, or 0 for none.
    pub fn skybox_of(&self, params_id: u32) -> u32 {
        self.params
            .iter()
            .find(|row| row.id() == params_id)
            .map_or(0, |row| row.light_skybox_id())
    }

    /// The model path a `LightSkybox` id names.
    ///
    /// **Returned with its `.mdx` extension intact.** This crate reads tables
    /// and knows nothing about model files; rewriting the extension is
    /// `m2::model_path`'s job, and doing it here would put a second copy of
    /// that rule in a crate that cannot test it.
    pub fn skybox_model(&self, skybox_id: u32) -> Option<String> {
        if skybox_id == 0 {
            return None;
        }
        self.skybox
            .as_ref()?
            .iter()
            .find(|row| row.id() == skybox_id)
            .map(|row| row.model().to_string())
            .filter(|path| !path.is_empty())
    }

    /// The star dome, found by name rather than by id.
    ///
    /// **`Environments\Stars\Stars.mdx` is a `LightSkybox` row like any
    /// other**, which is what makes drawing it a transcription instead of a
    /// decision -- the stars are in the same table as Stratholme's green
    /// murk. It is looked up by file name because an id is a number that could
    /// be anything and a name is the one thing in this table that cannot be a
    /// coincidence.
    ///
    /// It is *not* what any outdoor light names: Azeroth's default names
    /// skybox 0. The client draws it in addition to whatever the place names,
    /// which is why this is a separate accessor rather than a special case
    /// inside [`Lighting::skybox_model`].
    pub fn star_dome(&self) -> Option<String> {
        self.skybox.as_ref()?.iter().find_map(|row| {
            let path = row.model();
            let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
            file.eq_ignore_ascii_case("Stars.mdx")
                .then(|| path.to_string())
        })
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
    fn the_sky_bands_are_five_consecutive_ones_ending_at_the_diffuse() {
        // Three separate claims that have to stay true together, and each of
        // which a future edit could break silently:
        //
        // - the gradient is contiguous, because a stack of sky layers with a
        //   gap in it would be some other table's idea;
        // - `HORIZON` and `ZENITH` index the ends of the array they name, which
        //   is the kind of off-by-one that would tint the world upside down and
        //   still render;
        // - the diffuse light is the horizon band, which is a *borrowing* and
        //   not a coincidence. If someone moves the diffuse to a band of its
        //   own, this fails and points at the doc comment explaining why it
        //   was ever shared.
        assert!(bands::SKY.windows(2).all(|w| w[1] == w[0] + 1));
        assert_eq!(bands::ZENITH, 0);
        assert_eq!(bands::HORIZON, bands::SKY.len() - 1);
        assert_eq!(bands::SKY[bands::HORIZON], bands::DIFFUSE);
    }

    #[test]
    fn the_default_gradient_brightens_towards_the_horizon() {
        // The fallback has to have the property the real data has, or a light
        // with no sky bands would draw a gradient pointing the wrong way and
        // look like a bug in the sampler rather than like missing data.
        let value = |c: Colour| c[0] + c[1] + c[2];
        for pair in DEFAULT_SKY.windows(2) {
            assert!(
                value(pair[1]) > value(pair[0]),
                "the default sky must brighten from zenith to horizon: {pair:?}"
            );
        }
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
