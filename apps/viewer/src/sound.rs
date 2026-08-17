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

use dbc::schema::{
    AreaTable, CreatureDisplayInfo, CreatureModelData, CreatureSoundData, Item, SoundAmbience,
    SoundEntries, SoundType, Spell, SpellVisual, SpellVisualKit, WeaponImpactSounds, ZoneMusic,
};
use mpq::Chain;

/// `SoundEntries` id for clicking a spell or an action-bar slot.
///
/// `SoundEntries` type 2 (257 entries, 94% of them under `Sound\Interface`)
/// is the interface click family, and this one row names itself:
/// `GAMESPELLBUTTONMOUSEDOWN`. Not guessed at from memory -- the string is
/// the confirmation, the same way `ItemDisplayInfo`'s texture columns
/// identify themselves by the component suffix in their own filenames.
/// A single id rather than a per-frame lookup because there is exactly one
/// interaction this client currently distinguishes with a sound: putting an
/// ability to use. Every other click (a loot row, the release prompt, a
/// dragged bag square) is unconfirmed and stays silent rather than reusing
/// this one on a guess.
pub const INTERFACE_CLICK: u32 = 83;

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
    /// Creature display id to the sounds it makes.
    ///
    /// Keyed by *display* id rather than by creature entry because that is
    /// what a replicated unit carries and what the renderer already resolves
    /// -- one lookup, and no second identity to keep in step.
    creatures: std::collections::HashMap<u32, CreatureVoice>,
    /// Item entry to its weapon subclass, for the weapons that have one.
    ///
    /// Only weapons are kept -- `Item.dbc` has 46,096 rows and a subclass
    /// means something different in every class, so storing all of them would
    /// be storing a number whose meaning depends on a column not stored
    /// beside it.
    weapon_subclass: std::collections::HashMap<u32, u32>,
    /// Weapon subclass to its `(flesh, flesh_critical)` impact sounds.
    impacts: std::collections::HashMap<u32, (u32, u32)>,
    /// Spell id to the sound it makes when the cast begins, resolved
    /// `Spell -> SpellVisual -> SpellVisualKit -> SoundEntries` at load.
    /// Absent for most spells -- not every moment names a sound, and this
    /// keeps only the ones that do rather than a zero standing in for
    /// silence. See `dbc::schema::SpellVisual`'s doc comment for how the
    /// column that means "cast" was identified.
    spell_cast: std::collections::HashMap<u32, u32>,
    /// Spell id to the sound it makes when the cast resolves against its
    /// target. Same resolution as [`Self::spell_cast`], through
    /// `SpellVisual::impact_kit` instead.
    spell_impact: std::collections::HashMap<u32, u32>,
}

/// What one kind of creature sounds like.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CreatureVoice {
    pub attack: Option<u32>,
    pub wound: Option<u32>,
    pub wound_critical: Option<u32>,
    pub death: Option<u32>,
    pub aggro: Option<u32>,
}

/// Which of a creature's sounds to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    Attack,
    Wound,
    Death,
    Aggro,
}

impl CreatureVoice {
    pub fn get(&self, which: Voice) -> Option<u32> {
        match which {
            Voice::Attack => self.attack,
            // Falls back to the ordinary wound sound, because plenty of
            // creatures set one and not the other and silence would read as a
            // missing feature rather than as missing data.
            Voice::Wound => self.wound.or(self.wound_critical),
            Voice::Death => self.death,
            Voice::Aggro => self.aggro,
        }
    }
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

        // Creature voices, keyed by display id: CreatureDisplayInfo names a
        // CreatureSoundData row, and that row names the individual sounds.
        let voices: std::collections::HashMap<u32, CreatureVoice> = chain
            .read(CreatureSoundData::PATH)
            .ok()
            .and_then(|bytes| CreatureSoundData::parse(&bytes).ok())
            .map(|t| {
                t.iter()
                    .map(|row| {
                        let some = |id: u32| (id != 0).then_some(id);
                        (
                            row.id(),
                            CreatureVoice {
                                attack: some(row.attack()),
                                wound: some(row.wound()),
                                wound_critical: some(row.wound_critical()),
                                death: some(row.death()),
                                aggro: some(row.aggro()),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        // **A display's own sound id is an override, and most creatures do not
        // use it.** The Diseased Young Wolf's display carries `sound_id: 0`
        // and would have been silent; its *model* carries 43, which is the
        // row that actually holds a wolf's growls. Reading only the display
        // found a voice for 1,205 displays of 24,262, and every creature
        // anyone fights in a starting zone was in the silent majority -- which
        // presented as combat sounds simply not working.
        //
        // So: the display's id when it has one, the model's otherwise.
        let model_sounds: std::collections::HashMap<u32, u32> = chain
            .read(CreatureModelData::PATH)
            .ok()
            .and_then(|bytes| CreatureModelData::parse(&bytes).ok())
            .map(|t| t.iter().map(|row| (row.id(), row.sound_id())).collect())
            .unwrap_or_default();

        if let Some(displays) = chain
            .read(CreatureDisplayInfo::PATH)
            .ok()
            .and_then(|bytes| CreatureDisplayInfo::parse(&bytes).ok())
        {
            for display in displays.iter() {
                let sound_id = match display.sound_id() {
                    0 => model_sounds.get(&display.model_id()).copied().unwrap_or(0),
                    own => own,
                };
                if let Some(voice) = voices.get(&sound_id) {
                    sounds.creatures.insert(display.id(), *voice);
                }
            }
        }

        // Weapon impacts. `Item`'s class 2 is a weapon, and its subclass then
        // selects a `WeaponImpactSounds` row.
        const WEAPON_CLASS: u32 = 2;
        if let Some(items) = chain
            .read(Item::PATH)
            .ok()
            .and_then(|bytes| Item::parse(&bytes).ok())
        {
            for row in items.iter() {
                if row.class_id() == WEAPON_CLASS {
                    sounds.weapon_subclass.insert(row.id(), row.subclass_id());
                }
            }
        }
        sounds.impacts = chain
            .read(WeaponImpactSounds::PATH)
            .ok()
            .and_then(|bytes| WeaponImpactSounds::parse(&bytes).ok())
            .map(|t| {
                t.iter()
                    .map(|row| (row.weapon_subclass(), (row.flesh(), row.flesh_critical())))
                    .collect()
            })
            .unwrap_or_default();

        // Spell sounds: Spell -> SpellVisual -> SpellVisualKit -> sound id,
        // for the two moments this client can act on -- a cast beginning
        // (`SMSG_SPELL_START`) and a cast landing (`SMSG_SPELL_GO`). See
        // `dbc::schema::SpellVisual`'s doc comment for how `casting_kit` and
        // `impact_kit` were told apart from the table's other four columns.
        if let (Some(spells), Some(visuals), Some(kits)) = (
            chain
                .read(Spell::PATH)
                .ok()
                .and_then(|bytes| Spell::parse(&bytes).ok()),
            chain
                .read(SpellVisual::PATH)
                .ok()
                .and_then(|bytes| SpellVisual::parse(&bytes).ok()),
            chain
                .read(SpellVisualKit::PATH)
                .ok()
                .and_then(|bytes| SpellVisualKit::parse(&bytes).ok()),
        ) {
            let visual_by_id: std::collections::HashMap<u32, _> =
                visuals.iter().map(|v| (v.id(), v)).collect();
            let kit_sound: std::collections::HashMap<u32, u32> = kits
                .iter()
                .map(|k| (k.id(), k.sound()))
                .filter(|(_, sound)| *sound != 0)
                .collect();
            for spell in spells.iter() {
                let Some(visual) = visual_by_id.get(&spell.spell_visual()) else {
                    continue;
                };
                // A spell with no casting-kit sound (e.g. it only has a
                // precast one) still deserves a cast sound rather than
                // silence -- `Entry::pick`'s own reasoning, one level up.
                let cast = [visual.casting_kit(), visual.precast_kit()]
                    .into_iter()
                    .find_map(|kit| kit_sound.get(&kit).copied());
                if let Some(id) = cast {
                    sounds.spell_cast.insert(spell.id(), id);
                }
                if let Some(id) = kit_sound.get(&visual.impact_kit()).copied() {
                    sounds.spell_impact.insert(spell.id(), id);
                }
            }
        }

        tracing::info!(
            "sound tables loaded in {:?}: {} entries, {} areas with sound, {} creature voices, \
             {} spells with a cast sound, {} with an impact sound",
            started.elapsed(),
            sounds.entries.len(),
            sounds.zones.len(),
            sounds.creatures.len(),
            sounds.spell_cast.len(),
            sounds.spell_impact.len()
        );
        sounds
    }

    /// What a spell sounds like when the cast begins (`SMSG_SPELL_START`).
    pub fn spell_cast(&self, spell_id: u32) -> Option<u32> {
        self.spell_cast.get(&spell_id).copied()
    }

    /// What a spell sounds like when it resolves against its target
    /// (`SMSG_SPELL_GO`).
    pub fn spell_impact(&self, spell_id: u32) -> Option<u32> {
        self.spell_impact.get(&spell_id).copied()
    }

    /// What a creature with this display id sounds like.
    pub fn creature(&self, display_id: u32) -> Option<CreatureVoice> {
        self.creatures.get(&display_id).copied()
    }

    /// The sound an item makes when it lands on something unarmoured.
    ///
    /// **Flesh, always.** Chain and plate have their own columns and picking
    /// between them needs the target's armour, which this client cannot see --
    /// so rather than guess, it plays the one that is right for a creature and
    /// wrong for nothing it currently fights.
    pub fn weapon_impact(&self, item_entry: u32, critical: bool) -> Option<u32> {
        let subclass = self.weapon_subclass.get(&item_entry)?;
        let (flesh, flesh_critical) = self.impacts.get(subclass)?;
        let id = if critical { *flesh_critical } else { *flesh };
        (id != 0).then_some(id)
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

/// Plays one-off sounds -- a swing, a wound, a death cry.
///
/// Separate from [`Channel`] because the questions are opposite. A channel
/// holds *one* looping sound and must not restart it; this fires many
/// overlapping short ones and must not stop them. Two creatures dying at once
/// is two sounds, not the second replacing the first.
#[derive(Default)]
pub struct Effects {
    /// Sinks still playing, drained as they finish.
    ///
    /// **Kept, and that is not optional**: dropping a rodio sink stops it, so
    /// a fire-and-forget effect that is not held is silent. It has to be
    /// retained until it has actually finished, which is what `sweep` is for.
    playing: Vec<rodio::Sink>,
    refused: std::collections::HashSet<u32>,
    /// Sounds waiting for their moment, and when that moment is.
    ///
    /// **A hit sound has to land with the blade, not with the packet.** The
    /// server tells the client about a swing when it resolves it, and the
    /// client then *starts* the attack animation -- so playing the impact on
    /// arrival puts the clang before the sword arrives, which is exactly how
    /// it was reported: "the audio plays -> sword makes contact".
    ///
    /// The delay is a property of the animation rather than of the sound, so
    /// it is supplied by the caller and this only keeps the clock.
    delayed: Vec<(std::time::Instant, u32, f32)>,
}

impl Effects {
    /// How many sounds are in flight, after forgetting the finished ones.
    ///
    /// Called every frame. Without it the vector grows for the whole session
    /// -- one entry per sound ever played -- which is a slow leak rather than
    /// a loud one, and those are the ones that survive.
    pub fn sweep(&mut self) -> usize {
        self.playing.retain(|sink| !sink.empty());
        self.playing.len()
    }

    /// Queues a sound to play once `delay` has passed.
    pub fn play_after(&mut self, delay: std::time::Duration, id: u32, volume: f32) {
        self.delayed
            .push((std::time::Instant::now() + delay, id, volume));
    }

    /// Fires anything whose moment has come.
    ///
    /// Called once a frame. Frame-rate granularity is the limit on how
    /// accurately this can place a sound, which at 60fps is 16ms -- well under
    /// what anyone can hear against a sword swing.
    pub fn tick(
        &mut self,
        mixer: &rodio::mixer::Mixer,
        sounds: &Sounds,
        chain: &mut Chain,
        roll: f32,
    ) {
        let now = std::time::Instant::now();
        let mut due = Vec::new();
        self.delayed.retain(|(at, id, volume)| {
            if *at <= now {
                due.push((*id, *volume));
                false
            } else {
                true
            }
        });
        for (id, volume) in due {
            self.play(mixer, sounds, chain, id, volume, roll);
        }
    }

    /// Fires a sound once, if it can be loaded.
    pub fn play(
        &mut self,
        mixer: &rodio::mixer::Mixer,
        sounds: &Sounds,
        chain: &mut Chain,
        id: u32,
        volume: f32,
        roll: f32,
    ) {
        // A ceiling on simultaneous effects. A pack of creatures dying
        // together should not be able to queue an unbounded number of sinks,
        // and past a handful it is noise rather than detail anyway.
        const AT_ONCE: usize = 16;
        if self.refused.contains(&id) || self.sweep() >= AT_ONCE {
            return;
        }
        let Some(entry) = sounds.entry(id) else {
            self.refused.insert(id);
            return;
        };
        let Some(path) = entry.pick(roll) else {
            self.refused.insert(id);
            return;
        };
        let Ok(bytes) = chain.read(path) else {
            self.refused.insert(id);
            return;
        };
        match rodio::Decoder::new(Cursor::new(bytes)) {
            Ok(source) => {
                let sink = rodio::Sink::connect_new(mixer);
                sink.set_volume(volume * entry.volume);
                sink.append(source);
                self.playing.push(sink);
            }
            Err(error) => {
                tracing::debug!("{path} would not decode: {error}");
                self.refused.insert(id);
            }
        }
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

    /// `INTERFACE_CLICK` is a hardcoded id, which this project's own rule
    /// says to check against the *type* rather than trust because it
    /// parses -- see CLAUDE.md's "check the id resolves to a sound of the
    /// right type, not merely to a real row". A build that ever ships a
    /// renumbered `SoundEntries.dbc` would otherwise silently start playing
    /// whatever row 83 happens to be. Gated on `WOW_DATA` like every other
    /// real-data check in this repo, and skipped rather than failed when it
    /// is unset.
    #[test]
    fn the_interface_click_id_is_still_an_interface_sound() {
        let Some(data) = std::env::var_os("WOW_DATA") else {
            eprintln!("skipping: WOW_DATA not set");
            return;
        };
        let mut chain = Chain::open_wow_data(data, "enUS").expect("opening archives");
        let table = SoundEntries::parse(&chain.read(SoundEntries::PATH).unwrap()).unwrap();
        let row = table
            .iter()
            .find(|row| row.id() == INTERFACE_CLICK)
            .expect("INTERFACE_CLICK must name a real SoundEntries row");
        assert_eq!(
            SoundType::from_raw(row.sound_type()),
            SoundType::Other(2),
            "row {} is no longer an interface sound (type {})",
            row.id(),
            row.sound_type()
        );
        assert_eq!(row.name(), "GAMESPELLBUTTONMOUSEDOWN");
    }
}
