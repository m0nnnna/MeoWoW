//! Zone music and ambience: choosing what to play, and playing it.
//!
//! Two halves, separated by testability rather than by taste.
//!
//! **Choosing is pure.** Which sound a zone wants, and which of a sound's ten
//! files to use, are questions about tables and a random roll -- no device, no
//! archive, no clock beyond "is it night". That half is all plain functions,
//! so the whole of it is covered by ordinary unit tests on a machine with no
//! sound card. That matters more here than usual: an audio bug is invisible to
//! every other check this project has, and "it went quiet" is not a stack
//! trace.
//!
//! **Playing is not**, and is kept as thin as possible: hand bytes to rodio,
//! keep a handle so it can be stopped.
//!
//! What this does *not* do is as worth stating as what it does. There is no
//! distance attenuation, no per-sound volume curve beyond a flat multiply, no
//! silence interval between tracks (`ZoneMusic` carries four columns of them,
//! transcribed and ignored), and no crossfade -- a zone change cuts. None of
//! those is guessed at here.

use std::io::Cursor;

use dbc::schema::{AreaTable, SoundAmbience, SoundEntries, SoundType, ZoneMusic};
use mpq::Chain;

/// Which of a zone's two tracks to use.
///
/// `ZoneMusic` and `SoundAmbience` each carry a day and a night id, and often
/// the same id twice -- the distinction only matters for the zones that
/// bothered, which is exactly what proved the two columns were a pair rather
/// than one value written twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeOfDay {
    Day,
    Night,
}

/// When night begins and ends, in hours.
///
/// **A guess, and marked as one.** Nothing in the tables says when night
/// starts: `Light.dbc` interpolates continuously and never names a threshold.
/// This is a plausible split and nothing more, which is why it is one named
/// constant rather than a magic number buried in the selection code.
const DAYLIGHT_HOURS: std::ops::Range<f32> = 6.0..20.0;

impl TimeOfDay {
    /// From the realm's own clock, in hours.
    pub fn at_hour(hour: f32) -> Self {
        if DAYLIGHT_HOURS.contains(&hour) {
            Self::Day
        } else {
            Self::Night
        }
    }
}

/// The sound ids an area names, as day/night pairs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ZoneSound {
    /// `(day, night)` `SoundEntries` ids for music, or `None` when the area
    /// names none. Most do not: 793 of 2,307 areas name music and 445 name
    /// ambience.
    pub music: Option<(u32, u32)>,
    pub ambience: Option<(u32, u32)>,
}

impl ZoneSound {
    /// The music and ambience ids for a given time, dropping the zeros.
    ///
    /// A zero id means "nothing at this hour", which some zones genuinely use
    /// -- and it has to read as silence rather than as sound id zero, which
    /// does not exist.
    pub fn for_time(&self, when: TimeOfDay) -> (Option<u32>, Option<u32>) {
        let pick = |pair: Option<(u32, u32)>| {
            pair.map(|(day, night)| match when {
                TimeOfDay::Day => day,
                TimeOfDay::Night => night,
            })
            .filter(|id| *id != 0)
        };
        (pick(self.music), pick(self.ambience))
    }
}

/// One `SoundEntries` row, flattened to what playing it needs.
#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: SoundType,
    pub volume: f32,
    /// Full archive paths, each with its weight.
    pub files: Vec<(String, u32)>,
}

impl Entry {
    /// Picks one file, weighted, given a roll in `0.0..1.0`.
    ///
    /// **Takes the roll rather than generating it**, which is what makes this
    /// testable at all: a weighted choice that owns its own randomness can
    /// only be checked statistically, and a statistical test of a ten-way
    /// choice is slow and flaky. Handed the roll, every boundary is exact.
    ///
    /// Weights of zero are still selectable if *every* weight is zero --
    /// otherwise a sound whose table row leaves them blank would be silent,
    /// and silence is the one failure mode nothing else here would catch.
    pub fn pick(&self, roll: f32) -> Option<&str> {
        if self.files.is_empty() {
            return None;
        }
        let total: u32 = self.files.iter().map(|(_, weight)| *weight).sum();
        if total == 0 {
            // No weights at all: treat them as equal rather than as silent.
            let index = ((roll.clamp(0.0, 1.0) * self.files.len() as f32) as usize)
                .min(self.files.len() - 1);
            return Some(&self.files[index].0);
        }

        let target = roll.clamp(0.0, 1.0) * total as f32;
        let mut running = 0.0;
        for (path, weight) in &self.files {
            running += *weight as f32;
            if target < running {
                return Some(path);
            }
        }
        // A roll of exactly 1.0 falls past the last boundary.
        self.files.last().map(|(path, _)| path.as_str())
    }
}

/// Every table this needs, read once.
#[derive(Default)]
pub struct Sounds {
    entries: std::collections::HashMap<u32, Entry>,
    zones: std::collections::HashMap<u32, ZoneSound>,
}

impl Sounds {
    /// Reads the sound tables. Infallible in the same way the interface is:
    /// with no game data there is simply no sound, and the client still runs.
    pub fn load(chain: &mut Chain) -> Self {
        let started = std::time::Instant::now();
        let mut sounds = Sounds::default();

        let Some(table) = chain
            .read(SoundEntries::PATH)
            .ok()
            .and_then(|bytes| SoundEntries::parse(&bytes).ok())
        else {
            tracing::debug!("no sound tables; the client will be silent");
            return sounds;
        };
        for row in table.iter() {
            sounds.entries.insert(
                row.id(),
                Entry {
                    kind: SoundType::from_raw(row.sound_type()),
                    volume: row.volume(),
                    files: row.paths().into_iter().zip(row.weights()).collect(),
                },
            );
        }

        // Resolve both indirections once, at load, rather than on every zone
        // change: area -> ZoneMusic -> sound ids, area -> SoundAmbience ->
        // sound ids.
        // The two tables have the same *shape* -- an id and a day/night pair --
        // but they are separate types, so these are two reads rather than one
        // helper taking a path. A helper taking a path and hardcoding one of
        // the parsers is the shape that silently parses the wrong table.
        let music: std::collections::HashMap<u32, (u32, u32)> = chain
            .read(ZoneMusic::PATH)
            .ok()
            .and_then(|bytes| ZoneMusic::parse(&bytes).ok())
            .map(|t| {
                t.iter()
                    .map(|row| (row.id(), (row.day_sound(), row.night_sound())))
                    .collect()
            })
            .unwrap_or_default();
        let ambience: std::collections::HashMap<u32, (u32, u32)> = chain
            .read(SoundAmbience::PATH)
            .ok()
            .and_then(|bytes| SoundAmbience::parse(&bytes).ok())
            .map(|t| {
                t.iter()
                    .map(|row| (row.id(), (row.day_sound(), row.night_sound())))
                    .collect()
            })
            .unwrap_or_default();

        if let Some(areas) = chain
            .read(AreaTable::PATH)
            .ok()
            .and_then(|bytes| AreaTable::parse(&bytes).ok())
        {
            for area in areas.iter() {
                let entry = ZoneSound {
                    music: music.get(&area.zone_music()).copied(),
                    ambience: ambience.get(&area.ambience_id()).copied(),
                };
                if entry.music.is_some() || entry.ambience.is_some() {
                    sounds.zones.insert(area.id(), entry);
                }
            }
        }

        tracing::info!(
            "sound tables loaded in {:?}: {} entries, {} areas with sound",
            started.elapsed(),
            sounds.entries.len(),
            sounds.zones.len()
        );
        sounds
    }

    /// What an area sounds like. `None` for an area naming nothing, which is
    /// most of them.
    pub fn zone(&self, area_id: u32) -> Option<ZoneSound> {
        self.zones.get(&area_id).copied()
    }

    pub fn entry(&self, id: u32) -> Option<&Entry> {
        self.entries.get(&id)
    }
}

/// Plays one looping sound at a time, per channel.
///
/// Two of these exist -- music and ambience -- because they are independent:
/// a zone can change one and not the other, and stopping the music to start
/// ambience would be audible.
pub struct Channel {
    sink: Option<rodio::Sink>,
    /// Which `SoundEntries` id is playing, so an unchanged zone does not
    /// restart the track every frame. **This is the whole reason this struct
    /// holds state**: `play` is called continuously with whatever the current
    /// area wants, and without it the music would stutter at the frame rate.
    playing: Option<u32>,
    /// Sounds that would not load, so they are not attempted again.
    ///
    /// **Without this a failure retries at the frame rate.** A sound that will
    /// not decode will not decode a sixteenth of a second later either, and
    /// the first version of this logged the same warning sixty times a second
    /// -- which is exactly the trap `items::Items` documents for icons, walked
    /// into again one module over. A failure is an answer and gets remembered
    /// like any other.
    refused: std::collections::HashSet<u32>,
}

impl Channel {
    pub fn new() -> Self {
        Self {
            sink: None,
            playing: None,
            refused: std::collections::HashSet::new(),
        }
    }

    /// What is playing, if anything.
    pub fn playing(&self) -> Option<u32> {
        self.playing
    }

    /// Starts `wanted` if it is not already playing, or stops everything when
    /// `wanted` is `None`.
    ///
    /// Safe to call every frame -- see [`Channel::playing`].
    pub fn play(
        &mut self,
        mixer: &rodio::mixer::Mixer,
        sounds: &Sounds,
        chain: &mut Chain,
        wanted: Option<u32>,
        volume: f32,
        roll: f32,
    ) {
        if self.playing == wanted && self.sink.as_ref().is_some_and(|s| !s.empty()) {
            return;
        }
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.playing = wanted;

        let Some(id) = wanted else { return };
        if self.refused.contains(&id) {
            return;
        }
        let Some(entry) = sounds.entry(id) else {
            self.refused.insert(id);
            tracing::debug!("sound {id} is not in SoundEntries");
            return;
        };
        let Some(path) = entry.pick(roll) else {
            tracing::debug!("sound {id} ({:?}) names no files", entry.kind);
            self.refused.insert(id);
            return;
        };
        let Ok(bytes) = chain.read(path) else {
            // Expected: 7% of file references in this install do not resolve.
            tracing::debug!("no audio at {path}");
            self.refused.insert(id);
            return;
        };

        match rodio::Decoder::new(Cursor::new(bytes)) {
            Ok(source) => {
                let sink = rodio::Sink::connect_new(mixer);
                sink.set_volume(volume * entry.volume);
                // Looped, because zone music and ambience are continuous and
                // this client does not yet implement the silence intervals
                // `ZoneMusic` carries.
                sink.append(rodio::Source::repeat_infinite(source));
                tracing::debug!("playing {path} (sound {id})");
                self.sink = Some(sink);
            }
            Err(error) => {
                tracing::warn!("{path} would not decode: {error}");
                self.refused.insert(id);
            }
        }
    }
}

impl Default for Channel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(files: &[(&str, u32)]) -> Entry {
        Entry {
            kind: SoundType::Music,
            volume: 1.0,
            files: files
                .iter()
                .map(|(path, weight)| (path.to_string(), *weight))
                .collect(),
        }
    }

    /// The weighted pick, at every boundary. Exact because the roll is handed
    /// in rather than generated -- see [`Entry::pick`].
    #[test]
    fn a_weighted_pick_lands_in_the_right_band() {
        let sound = entry(&[("a", 1), ("b", 3)]);
        // Total 4: "a" owns 0.0..0.25, "b" owns the rest.
        assert_eq!(sound.pick(0.0), Some("a"));
        assert_eq!(sound.pick(0.24), Some("a"));
        assert_eq!(sound.pick(0.25), Some("b"));
        assert_eq!(sound.pick(0.99), Some("b"));
        // A roll of exactly 1.0 falls past the last boundary and must still
        // produce a file rather than nothing.
        assert_eq!(sound.pick(1.0), Some("b"));
    }

    /// A single-file sound is the common case and must not depend on the roll.
    #[test]
    fn one_file_is_always_chosen() {
        let sound = entry(&[("only", 1)]);
        for roll in [0.0, 0.5, 1.0] {
            assert_eq!(sound.pick(roll), Some("only"));
        }
    }

    /// **Silence is the failure nothing else here would catch.** A row whose
    /// weights are all zero must still play something rather than going quiet,
    /// because a silent sound is indistinguishable from a broken one.
    #[test]
    fn all_zero_weights_still_pick_a_file() {
        let sound = entry(&[("a", 0), ("b", 0), ("c", 0)]);
        assert_eq!(sound.pick(0.0), Some("a"));
        assert_eq!(sound.pick(0.5), Some("b"));
        assert_eq!(sound.pick(1.0), Some("c"));
    }

    #[test]
    fn a_sound_with_no_files_picks_nothing() {
        assert_eq!(entry(&[]).pick(0.5), None);
    }

    /// Out-of-range rolls are clamped rather than panicking or wrapping. The
    /// caller's random source is not this module's business.
    #[test]
    fn a_roll_outside_the_range_is_clamped() {
        let sound = entry(&[("a", 1), ("b", 1)]);
        assert_eq!(sound.pick(-5.0), Some("a"));
        assert_eq!(sound.pick(5.0), Some("b"));
    }

    #[test]
    fn day_and_night_split_at_the_named_hours() {
        assert_eq!(TimeOfDay::at_hour(6.0), TimeOfDay::Day);
        assert_eq!(TimeOfDay::at_hour(12.0), TimeOfDay::Day);
        assert_eq!(TimeOfDay::at_hour(19.9), TimeOfDay::Day);
        assert_eq!(TimeOfDay::at_hour(20.0), TimeOfDay::Night);
        assert_eq!(TimeOfDay::at_hour(3.0), TimeOfDay::Night);
        assert_eq!(TimeOfDay::at_hour(0.0), TimeOfDay::Night);
    }

    /// A zero id means silence at that hour, not sound id zero.
    #[test]
    fn a_zero_id_reads_as_silence() {
        let zone = ZoneSound {
            music: Some((100, 0)),
            ambience: None,
        };
        assert_eq!(zone.for_time(TimeOfDay::Day), (Some(100), None));
        assert_eq!(zone.for_time(TimeOfDay::Night), (None, None));
    }

    /// The day/night pair is the thing that identified the two columns, so
    /// picking the right one of them is worth pinning.
    #[test]
    fn night_uses_the_night_track() {
        let zone = ZoneSound {
            music: Some((2524, 2534)),
            ambience: Some((41, 42)),
        };
        assert_eq!(zone.for_time(TimeOfDay::Day), (Some(2524), Some(41)));
        assert_eq!(zone.for_time(TimeOfDay::Night), (Some(2534), Some(42)));
    }
}
