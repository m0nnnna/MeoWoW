//! What the sky is doing.
//!
//! One packet, `SMSG_WEATHER`, carrying a state, an intensity and whether the
//! change was abrupt. The server sends it on entering a zone and whenever that
//! zone's weather turns, so a client that ignores it stands in permanent
//! sunshine.
//!
//! **Weather is a zone property the server owns**, unlike the sheath state
//! next door in [`crate::combat`], which the client decides and the server only
//! republishes. Nothing here is ever sent.

/// What the sky is doing, as the wire reports it.
///
/// The numbers are sparse and deliberately so -- 2 is absent, and the jump from
/// 8 to 22 to 41 is real. Only the values actually observed or driveable are
/// named; anything else is carried through as [`Weather::Unknown`] rather than
/// guessed at, because a wrong *name* for a weather state is the kind of thing
/// that never fails loudly, it just makes the next reader believe something
/// false.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Weather {
    /// Clear. The state a zone sits in unless something says otherwise.
    #[default]
    Fine,
    Fog,
    LightRain,
    MediumRain,
    HeavyRain,
    LightSnow,
    MediumSnow,
    HeavySnow,
    LightSandstorm,
    MediumSandstorm,
    HeavySandstorm,
    Thunders,
    BlackRain,
    BlackSnow,
    /// A state this client has not seen. Kept as its raw number so a capture
    /// can still say what arrived.
    Unknown(u32),
}

impl Weather {
    /// Reads the state number off the wire.
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Fine,
            1 => Self::Fog,
            3 => Self::LightRain,
            4 => Self::MediumRain,
            5 => Self::HeavyRain,
            6 => Self::LightSnow,
            7 => Self::MediumSnow,
            8 => Self::HeavySnow,
            22 => Self::LightSandstorm,
            41 => Self::MediumSandstorm,
            42 => Self::HeavySandstorm,
            86 => Self::Thunders,
            90 => Self::BlackRain,
            106 => Self::BlackSnow,
            other => Self::Unknown(other),
        }
    }

    /// Whether this state should be lit as a storm rather than as clear
    /// weather.
    ///
    /// `Light.dbc` gives every light two sets of curves, and which to use is
    /// the only decision a client has to make from the weather state before it
    /// can draw anything. **Fog counts as a storm**, because the difference
    /// that matters for lighting is not rain -- it is that the storm curves are
    /// a flat neutral grey with the horizon pulled in, which is exactly what
    /// fog looks like.
    ///
    /// An unknown state is lit as clear. That is the conservative direction: a
    /// zone drawn in ordinary daylight when it should be dim looks unremarkable,
    /// where the reverse looks broken.
    pub fn is_storm(self) -> bool {
        !matches!(self, Self::Fine | Self::Unknown(_))
    }
}

/// A parsed `SMSG_WEATHER`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WeatherChange {
    pub weather: Weather,
    /// How hard it is coming down, 0 to 1. The server sends 0 with
    /// [`Weather::Fine`].
    pub intensity: f32,
    /// Whether the change was instant rather than eased in. Carried because it
    /// is on the wire and says something a renderer could act on; nothing reads
    /// it yet.
    pub abrupt: bool,
}

/// Parses `SMSG_WEATHER`: `u32` state, `f32` intensity, `u8` abrupt.
///
/// Exactly nine bytes, and both running out and having bytes left over are
/// errors -- the rule the rest of this crate follows, and the one that has
/// caught four separate layout bugs.
pub fn parse(body: &[u8]) -> Result<WeatherChange, crate::protocol::Error> {
    const WANT: usize = 4 + 4 + 1;
    if body.len() < WANT {
        return Err(crate::protocol::Error::Truncated {
            what: "SMSG_WEATHER",
            at: 0,
            need: WANT,
            len: body.len(),
        });
    }
    if body.len() > WANT {
        return Err(crate::protocol::Error::Trailing {
            what: "SMSG_WEATHER",
            got: body.len() - WANT,
        });
    }
    let raw = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let intensity = f32::from_le_bytes(body[4..8].try_into().unwrap());
    Ok(WeatherChange {
        weather: Weather::from_raw(raw),
        // Clamped rather than trusted: this scales a blend between two sets of
        // light curves, and a value outside 0..1 would extrapolate past the
        // storm into colours neither table describes.
        intensity: if intensity.is_finite() {
            intensity.clamp(0.0, 1.0)
        } else {
            0.0
        },
        abrupt: body[8] != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real body: medium rain at full intensity, eased in.
    #[test]
    fn reads_a_weather_change() {
        let mut body = Vec::new();
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&1.0f32.to_le_bytes());
        body.push(0);
        let change = parse(&body).expect("a nine-byte body");
        assert_eq!(change.weather, Weather::MediumRain);
        assert_eq!(change.intensity, 1.0);
        assert!(!change.abrupt);
        assert!(change.weather.is_storm());
    }

    /// Clear weather is not a storm, and is what an unknown state falls back
    /// to. Both halves matter: a filter that called everything a storm would
    /// pass the first assertion alone.
    #[test]
    fn only_real_weather_is_a_storm() {
        assert!(!Weather::Fine.is_storm());
        assert!(!Weather::from_raw(0).is_storm());
        assert!(!Weather::Unknown(1234).is_storm());
        for raw in [1, 3, 4, 5, 6, 7, 8, 22, 41, 42, 86, 90, 106] {
            assert!(
                Weather::from_raw(raw).is_storm(),
                "state {raw} should be lit as a storm"
            );
            assert!(
                !matches!(Weather::from_raw(raw), Weather::Unknown(_)),
                "state {raw} should have a name"
            );
        }
    }

    /// A state nobody has seen keeps its number rather than becoming `Fine`,
    /// so a capture can still say what arrived.
    #[test]
    fn an_unknown_state_keeps_its_number() {
        assert_eq!(Weather::from_raw(99), Weather::Unknown(99));
    }

    /// Both directions of the length rule.
    #[test]
    fn a_body_of_the_wrong_length_is_an_error() {
        assert!(parse(&[0; 8]).is_err(), "short body accepted");
        assert!(parse(&[0; 10]).is_err(), "trailing bytes accepted");
    }

    /// Intensity scales a blend between two sets of light curves, so it must
    /// not leave 0..1 however odd the input.
    #[test]
    fn intensity_stays_in_range() {
        let build = |v: f32| {
            let mut body = Vec::new();
            body.extend_from_slice(&5u32.to_le_bytes());
            body.extend_from_slice(&v.to_le_bytes());
            body.push(1);
            parse(&body).unwrap().intensity
        };
        assert_eq!(build(2.5), 1.0);
        assert_eq!(build(-1.0), 0.0);
        assert_eq!(build(f32::NAN), 0.0);
        assert_eq!(build(0.5), 0.5);
    }
}
