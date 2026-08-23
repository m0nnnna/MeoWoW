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
    AreaTable, CreatureDisplayInfo, CreatureModelData, CreatureSoundData, FootstepTerrainLookup,
    GroundEffectTexture, Item, SoundAmbience, SoundEntries, SoundType, Spell, SpellVisual,
    SpellVisualKit, TerrainType, WeaponImpactSounds, ZoneMusic,
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

/// How many parent hops a zone-sound lookup will walk before giving up.
///
/// `AreaTable`'s hierarchy is two or three levels deep in practice -- a
/// continent's zones and their sub-areas -- so this is generous headroom
/// against a cycle in the data rather than a limit anyone should expect to
/// hit. A lookup that walked forever on a malformed `parent_area_id` chain
/// would be indistinguishable from the client hanging.
const MAX_PARENT_HOPS: u32 = 8;

/// The music or ambience id a zone should use: its own, or its nearest
/// ancestor's if it names none.
///
/// **Most areas name neither** -- see [`ZoneSound`]'s doc comment, 1,359 of
/// 2,307 in this build -- and a flat, area-by-area lookup silently treated
/// every one of them as having no sound at all, rather than as inheriting
/// what the zone containing it plays. Live-reported as "the wind and birds
/// are just gone": Coldridge Valley (`AreaTable` id 132) is a sub-area of
/// Dun Morogh (id 1, `parent_area_id`) and, like 1,283 of those 1,359 areas,
/// has an ancestor that does name something -- Dun Morogh's own ambience
/// and music are both set. Only 76 of the 1,359 have no ancestor with
/// anything either, which is a real silence rather than a missed one.
///
/// **An id that names no row is treated exactly like a zero and the walk
/// continues.** This changes nothing on a stock install -- measured, rather
/// than assumed: of 2,307 areas in this build, **zero** name a `zone_music`
/// or `ambience_id` missing from `ZoneMusic.dbc`/`SoundAmbience.dbc`, so the
/// branch is unreachable against the reference data and is defensive only.
/// It is here because the alternative -- returning `None` at the first
/// unresolvable id -- makes an area with a dangling reference *silent*
/// rather than falling back, which is the same failure this whole function
/// exists to close, and a patched or partial install is the one place it
/// could appear.
fn resolve_zone_sound(
    areas: &std::collections::HashMap<u32, (u32, u32, u32)>,
    table: &std::collections::HashMap<u32, (u32, u32)>,
    area_id: u32,
    pick: impl Fn((u32, u32, u32)) -> u32,
) -> Option<(u32, u32)> {
    let mut current = area_id;
    for _ in 0..MAX_PARENT_HOPS {
        let Some(&row) = areas.get(&current) else {
            return None;
        };
        // The zero check stays explicit rather than being folded into the
        // lookup: zero means "names nothing" whatever the table happens to
        // hold, and a table that did carry a row 0 would otherwise turn
        // every inheriting area into a resolved one.
        let id = pick(row);
        if id != 0 {
            if let Some(sound) = table.get(&id) {
                return Some(*sound);
            }
        }
        let parent = row.2;
        if parent == 0 || parent == current {
            return None;
        }
        current = parent;
    }
    None
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

/// How many footfalls the animation clock passed between two readings.
///
/// A footfall is a **crossing**, not a state: the model names the moments a
/// foot lands and the question each frame is which of them the clock went past
/// since the last look. With no previous reading there is nothing to have
/// crossed, and the answer is none.
///
/// Its own function, out of the frame loop, because every rule in it is one
/// that can only be got wrong once a person is watching:
///
/// * **The wrap.** A looping cycle's clock goes backwards at the end, so the
///   interval is two pieces. Treating it as one drops every footfall at or near
///   zero, which on a walk is every other step.
/// * **A changed sequence fires nothing.** The new cycle's clock has no
///   relationship to the old one's, so any comparison is meaningless -- and the
///   meaningless answer is not zero, it is "everything before wherever the new
///   cycle happened to be entered", which stamps a step on every change of gait.
/// * **Half-open at the start.** A timestamp exactly at the previous reading
///   was already played; one exactly at this reading is due now. Closing both
///   ends plays a footfall twice whenever a frame lands exactly on one.
pub fn footfalls_crossed(
    previous: Option<(usize, u32)>,
    sequence: usize,
    now: u32,
    duration: u32,
    times: &[u32],
) -> usize {
    let Some(then) = previous
        .filter(|(was, _)| *was == sequence)
        .map(|(_, then)| then)
    else {
        return 0;
    };
    if now >= then {
        times.iter().filter(|t| **t > then && **t <= now).count()
    } else {
        times.iter().filter(|t| **t > then && **t < duration).count()
            + times.iter().filter(|t| **t <= now).count()
    }
}

/// What a character is standing on, from whichever of the two sources knows.
///
/// **An enum rather than a bare number, because the two are not the same
/// currency and one of them has already caught this project out.** The ground
/// outside names a `GroundEffectTexture` row, which has to be resolved to a
/// terrain and *then* to that terrain's sound id. A building's floor names the
/// `TerrainType` row directly. Both are small integers in overlapping ranges,
/// both resolve to something, and passing one where the other belongs plays a
/// plausible wrong sound -- exactly the shape of the `sound_id`-versus-row-id
/// trap in `FootstepTerrainLookup` that this milestone spent its evidence on.
/// Making them different variants means the compiler refuses the mix-up that
/// a shared `u32` would accept in silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Footing {
    /// Open terrain, named by the `GroundEffectTexture` id of the texture
    /// layer that wins at this point.
    Ground(u32),
    /// A building's floor, named by the `TerrainType` row its material gives.
    Surface(u32),
}

/// Every table this needs, read once.
#[derive(Default)]
pub struct Sounds {
    entries: std::collections::HashMap<u32, Entry>,
    zones: std::collections::HashMap<u32, ZoneSound>,
    zone_music: std::collections::HashMap<u32, (u32, u32)>,
    ambience: std::collections::HashMap<u32, (u32, u32)>,
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
    /// `GroundEffectTexture` id to the terrain [`FootstepTerrainLookup`] keys
    /// on. Absent means the ground says nothing about its surface, which is
    /// most of it -- see [`Sounds::footstep`].
    footing_terrain: std::collections::HashMap<u32, u32>,
    /// `TerrainType` row id to the terrain [`FootstepTerrainLookup`] keys on.
    ///
    /// The other half of the same hop, kept separately because a building's
    /// floor arrives already resolved to a row and must not be sent through
    /// the ground effect table -- see [`Footing`].
    terrain_sound: std::collections::HashMap<u32, u32>,
    /// `(creature footstep group, terrain)` to `(footstep, splash)`.
    footsteps: std::collections::HashMap<(u32, u32), (u32, Option<u32>)>,
    /// Creature display id to the footstep group its feet use. Keyed by
    /// display for the same reason [`Sounds::creatures`] is.
    footstep_groups: std::collections::HashMap<u32, u32>,
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
            // Own (zone_music, ambience_id, parent_area_id) per area, read
            // once so the walk below is a hash lookup rather than a second
            // pass over the table per area.
            let rows: std::collections::HashMap<u32, (u32, u32, u32)> = areas
                .iter()
                .map(|a| (a.id(), (a.zone_music(), a.ambience_id(), a.parent_area_id())))
                .collect();

            for area in areas.iter() {
                let entry = ZoneSound {
                    music: resolve_zone_sound(&rows, &music, area.id(), |r| r.0),
                    ambience: resolve_zone_sound(&rows, &ambience, area.id(), |r| r.1),
                };
                if entry.music.is_some() || entry.ambience.is_some() {
                    sounds.zones.insert(area.id(), entry);
                }
            }
        }
        sounds.zone_music = music;
        sounds.ambience = ambience;

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
        let footstep_groups: std::collections::HashMap<u32, u32> = chain
            .read(CreatureSoundData::PATH)
            .ok()
            .and_then(|bytes| CreatureSoundData::parse(&bytes).ok())
            .map(|t| {
                t.iter()
                    .filter(|row| row.footstep_group() != 0)
                    .map(|row| (row.id(), row.footstep_group()))
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

        // Footsteps. Three tables and two hops, and the hops are in different
        // directions, which is the thing to get right:
        //
        //   the ground:   GroundEffectTexture -> a TerrainType *row id*
        //                 -> that row's `sound_id`
        //   the creature: CreatureSoundData   -> a footstep *group*
        //   together:     FootstepTerrainLookup(group, sound_id) -> a sound
        //
        // A `TerrainType` row id and its `sound_id` are off by one from each
        // other all the way down the table, so using either where the other
        // belongs plays snow on wood. See `FootstepTerrainLookup`'s doc
        // comment for the measurement that separates them.
        if let Some(terrain) = chain
            .read(TerrainType::PATH)
            .ok()
            .and_then(|bytes| TerrainType::parse(&bytes).ok())
        {
            let sound_of_row: std::collections::HashMap<u32, u32> =
                terrain.iter().map(|r| (r.id(), r.sound_id())).collect();
            sounds.terrain_sound = sound_of_row.clone();
            if let Some(textures) = chain
                .read(GroundEffectTexture::PATH)
                .ok()
                .and_then(|bytes| GroundEffectTexture::parse(&bytes).ok())
            {
                for row in textures.iter() {
                    // **Only the rows that say something.** 22,708 of 24,981
                    // name terrain 0, and an absent entry here means exactly
                    // what `FootstepTerrainLookup`'s own terrain 0 means --
                    // "this does not say" -- so storing them would be ten
                    // times the memory to reach the same sound by a longer
                    // road. That the two zeroes agree is luck rather than
                    // design, and it is why this is safe: row 0 is `Dirt` and
                    // the fallback rows are dirt sounds.
                    if row.terrain_type() == 0 {
                        continue;
                    }
                    if let Some(&sound) = sound_of_row.get(&row.terrain_type()) {
                        sounds.footing_terrain.insert(row.id(), sound);
                    }
                }
            }
        }
        if let Some(lookup) = chain
            .read(FootstepTerrainLookup::PATH)
            .ok()
            .and_then(|bytes| FootstepTerrainLookup::parse(&bytes).ok())
        {
            for row in lookup.iter() {
                sounds.footsteps.insert(
                    (row.creature_footstep_id(), row.terrain()),
                    (
                        row.sound(),
                        (row.sound_splash() != 0).then_some(row.sound_splash()),
                    ),
                );
            }
        }
        // Which group a creature's feet use, resolved display id first and
        // model second -- the same override this file already documents at
        // length for voices, and it matters more here rather than less: 738 of
        // 1,306 `CreatureSoundData` rows name a footstep group at all.
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
                if let Some(group) = footstep_groups.get(&sound_id) {
                    sounds.footstep_groups.insert(display.id(), *group);
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
        tracing::info!(
            "footsteps: {} ground textures name a terrain, {} (group, terrain) pairs,              {} displays with feet",
            sounds.footing_terrain.len(),
            sounds.footsteps.len(),
            sounds.footstep_groups.len(),
        );
        sounds
    }

    /// What a creature's foot sounds like landing on a patch of ground.
    ///
    /// `footing` is `None` -- a tile still streaming, a chunk with no layers,
    /// ground whose texture says nothing, or a floor whose material declines
    /// to -- and falls through to the lookup's **own** terrain 0, which is a
    /// real row rather than an invention: seventeen of its rows carry it, they
    /// reach dirt sounds, and that is what a client should play when it does
    /// not know what it is standing on.
    ///
    /// Splashing is the same row's other column, and it is `None` on 92 of 217
    /// rows -- a spider has no splash sound and gets its ordinary step rather
    /// than silence.
    ///
    /// `None` overall means this display has no feet the tables know about,
    /// which is 568 of the 1,306 `CreatureSoundData` rows and every model that
    /// floats, swims or hovers.
    pub fn footstep(
        &self,
        display_id: u32,
        footing: Option<Footing>,
        splashing: bool,
    ) -> Option<u32> {
        let group = self.footstep_groups.get(&display_id).copied()?;
        let terrain = footing
            .and_then(|footing| match footing {
                Footing::Ground(id) => self.footing_terrain.get(&id).copied(),
                Footing::Surface(row) => self.terrain_sound.get(&row).copied(),
            })
            .unwrap_or(0);
        // Falls back to the fallback: a group that has no row for this terrain
        // still has one for terrain 0, because every group in the table does.
        // Without this a creature with a short list would go silent on stone
        // rather than sounding wrong, and silence is the harder of the two to
        // notice.
        let (step, splash) = self
            .footsteps
            .get(&(group, terrain))
            .or_else(|| self.footsteps.get(&(group, 0)))
            .copied()?;
        Some(if splashing {
            splash.unwrap_or(step)
        } else {
            step
        })
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

    pub fn zone_with_overrides(
        &self,
        area_id: u32,
        zone_music: Option<u32>,
        ambience: Option<u32>,
    ) -> Option<ZoneSound> {
        let base = self.zone(area_id).unwrap_or_default();
        let music = zone_music
            .and_then(|id| self.zone_music.get(&id).copied())
            .or(base.music);
        let ambience = ambience
            .and_then(|id| self.ambience.get(&id).copied())
            .or(base.ambience);
        (music.is_some() || ambience.is_some()).then_some(ZoneSound { music, ambience })
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
    /// Track bytes by archive path -- see [`Channel::play`] for why a channel
    /// re-reads at all.
    clips: std::collections::HashMap<String, std::sync::Arc<[u8]>>,
    /// Archive reads against restarts. **Both**, because a channel that
    /// restarts constantly and a cache that has stopped working sound exactly
    /// the same, and the ratio is the only thing that separates them.
    reads: u64,
    starts: u64,
}

impl Channel {
    pub fn new() -> Self {
        Self {
            sink: None,
            playing: None,
            refused: std::collections::HashSet::new(),
            clips: std::collections::HashMap::new(),
            reads: 0,
            starts: 0,
        }
    }

    /// Archive reads performed, and times a track was started.
    pub fn track_reads(&self) -> (u64, u64) {
        (self.reads, self.starts)
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
        // **Re-read on every restart, and a restart is not rare.** The guard
        // at the top of this function lets a channel fall through the moment
        // its sink empties -- which is what happens every time a music track
        // or an ambience loop reaches its end -- and the file then comes back
        // out of the MPQ, decompressed from scratch. Measured as `sound`
        // sitting at 3.54 ms average with a 35.10 ms worst while the three
        // spans *inside* `update_sound` all read under 0.15 ms: the cost was
        // entirely in here, one line past where the timer stopped.
        let bytes = match self.clips.get(path) {
            Some(bytes) => std::sync::Arc::clone(bytes),
            None => {
                self.reads += 1;
                let Ok(raw) = chain.read(path) else {
                    // Expected: 7% of file references in this install do not
                    // resolve.
                    tracing::debug!("no audio at {path}");
                    self.refused.insert(id);
                    return;
                };
                let bytes: std::sync::Arc<[u8]> = raw.into();
                // **A far larger cap than the effects cache**, because these
                // are the long files by definition and there are only ever
                // two of them alive -- one music track and one ambience loop
                // for the area the player is standing in. Bounded all the
                // same: a table entry naming something enormous should not be
                // able to pin it in memory for the session.
                if bytes.len() <= MAX_CACHED_TRACK {
                    self.clips.insert(path.to_string(), std::sync::Arc::clone(&bytes));
                }
                bytes
            }
        };
        self.starts += 1;

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
    /// Clip bytes by archive path, so a sound is read out of the archive once
    /// rather than once per time it is heard.
    ///
    /// **Every footstep was re-reading and re-decompressing its own file.**
    /// A walking character fires several a second, `chain.read` decompresses
    /// out of the MPQ each time, and nothing kept the result -- measured at
    /// 3.60 ms of a 17.32 ms frame on average with a 32.70 ms worst, which
    /// made `update_sound` the largest single item in the frame once the
    /// minimap's index scan was fixed.
    ///
    /// `Arc<[u8]>` rather than `Vec<u8>`: `rodio::Decoder` wants an owned
    /// `Cursor`, and cloning an `Arc` to build one is a pointer copy where
    /// cloning the bytes would put back most of the cost being removed.
    clips: std::collections::HashMap<String, std::sync::Arc<[u8]>>,
    /// Archive reads this has actually performed, against clips played.
    ///
    /// **Both, always.** A cache that has quietly stopped caching plays the
    /// same sounds and sounds identical; only the ratio says otherwise. The
    /// same reasoning as the collision grid's probe and the minimap index's
    /// scan counter, and the third time this session that a count -- rather
    /// than a duration -- is what makes a fix checkable.
    reads: u64,
    plays: u64,
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
        self.plays += 1;
        let bytes = match self.clips.get(path) {
            Some(bytes) => std::sync::Arc::clone(bytes),
            None => {
                self.reads += 1;
                let Ok(raw) = chain.read(path) else {
                    self.refused.insert(id);
                    return;
                };
                let bytes: std::sync::Arc<[u8]> = raw.into();
                // **Only the small ones.** An effect is a footstep or a clang
                // -- tens of kilobytes, played over and over -- while the
                // long files in this table are the ones nothing repeats.
                // Keeping everything would trade a frame-time problem for a
                // memory one, which is not a trade this needs to make.
                if bytes.len() <= MAX_CACHED_CLIP {
                    self.clips.insert(path.to_string(), std::sync::Arc::clone(&bytes));
                }
                bytes
            }
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

    /// Archive reads performed, and clips played. See [`Effects::reads`].
    pub fn clip_reads(&self) -> (u64, u64) {
        (self.reads, self.plays)
    }
}

/// Largest music or ambience track kept in memory, in bytes. Only ever two
/// are alive at once -- the area's music and its ambience -- so this can be
/// generous, and is bounded anyway so one enormous table entry cannot pin
/// itself in memory for the session.
const MAX_CACHED_TRACK: usize = 8 * 1024 * 1024;

/// Largest clip kept in memory, in bytes. Comfortably above a footstep or an
/// impact and below anything long enough to be worth streaming.
const MAX_CACHED_CLIP: usize = 512 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// A building's floor and the ground outside are different currencies and
    /// resolve through different tables, and the enum is what keeps them
    /// apart. Both halves asserted, because a rule that sent everything
    /// through one table would still answer plausibly for the other.
    #[test]
    fn a_floor_and_the_ground_resolve_through_different_tables() {
        let sounds = footed();
        // Ground effect 42 says terrain 3 -- the stone step.
        assert_eq!(
            sounds.footstep(1, Some(Footing::Ground(42)), false),
            Some(653)
        );
        // `TerrainType` row 2 is `Stone`, whose sound id is 3: the same step,
        // reached the other way.
        assert_eq!(
            sounds.footstep(1, Some(Footing::Surface(2)), false),
            Some(653)
        );
        // And the numbers are genuinely not interchangeable: row 42 is not a
        // terrain at all, and ground effect 2 is not one this fixture knows,
        // so both fall through to the fallback rather than to each other's
        // answer.
        assert_eq!(
            sounds.footstep(1, Some(Footing::Surface(42)), false),
            Some(650)
        );
        assert_eq!(
            sounds.footstep(1, Some(Footing::Ground(2)), false),
            Some(650)
        );
    }

    /// The ordinary case, and the one that only shows up at the end of a
    /// cycle. Asserted together because a rule that ignores the wrap passes
    /// the first on its own and drops every other step in play.
    #[test]
    fn footfalls_are_counted_once_as_the_clock_passes_them() {
        let times = [266, 800];
        // Nothing to have crossed yet.
        assert_eq!(footfalls_crossed(None, 1, 300, 1000, &times), 0);
        // Straddling one.
        assert_eq!(footfalls_crossed(Some((1, 250)), 1, 280, 1000, &times), 1);
        // Between them.
        assert_eq!(footfalls_crossed(Some((1, 300)), 1, 340, 1000, &times), 0);
        // Round the end of the cycle: 800 is behind us, and the next reading
        // is at 40ms of the new lap.
        assert_eq!(footfalls_crossed(Some((1, 780)), 1, 40, 1000, &times), 1);
        // A whole lap in one step counts each footfall once, not twice.
        assert_eq!(footfalls_crossed(Some((1, 100)), 1, 90, 1000, &times), 2);
    }

    /// A frame landing exactly on a footfall plays it once, not twice: the
    /// interval is half-open, so the timestamp belongs to the reading that
    /// first reached it and to no other.
    #[test]
    fn a_footfall_exactly_on_a_frame_plays_once() {
        let times = [500];
        assert_eq!(footfalls_crossed(Some((1, 480)), 1, 500, 1000, &times), 1);
        assert_eq!(footfalls_crossed(Some((1, 500)), 1, 520, 1000, &times), 0);
    }

    /// Changing cycle fires nothing at all. The wrong answer here is not zero:
    /// a walk entered at 900ms would otherwise "cross" both of the run's
    /// earlier footfalls at once, so every change of gait would stamp a step.
    #[test]
    fn a_changed_cycle_fires_nothing() {
        let times = [266, 800];
        assert_eq!(footfalls_crossed(Some((2, 900)), 1, 300, 1000, &times), 0);
    }

    /// A quadruped's cycle has four contacts and they are counted
    /// independently -- the wolf's walk, which is the sample that identified
    /// the event in the first place.
    #[test]
    fn four_feet_are_four_footfalls() {
        let times = [34, 167, 534, 667];
        assert_eq!(footfalls_crossed(Some((0, 0)), 0, 200, 1000, &times), 2);
        assert_eq!(footfalls_crossed(Some((0, 200)), 0, 700, 1000, &times), 2);
    }

    /// Builds a `Sounds` with just enough footstep tables to answer, standing
    /// in for the real ones so the rules can be tested without an archive.
    fn footed() -> Sounds {
        let mut sounds = Sounds::default();
        // Display 1 walks on the group 8 the human male uses; display 2 has no
        // feet at all, which 568 of 1,306 creature sound rows genuinely do not.
        sounds.footstep_groups.insert(1, 8);
        // Terrain 3 is `Stone` and terrain 0 is the lookup's own fallback.
        sounds.footsteps.insert((8, 0), (650, Some(1054)));
        sounds.footsteps.insert((8, 3), (653, None));
        // Ground effect 42 is stone; 99 is a texture that names no terrain.
        sounds.footing_terrain.insert(42, 3);
        // And `TerrainType` row 2 (`Stone`) has sound id 3, which is the
        // off-by-one this whole feature turns on.
        sounds.terrain_sound.insert(2, 3);
        sounds
    }

    /// The ground chooses the sound, and ground that says nothing falls back
    /// to the lookup's own terrain 0 rather than to silence.
    ///
    /// Both halves asserted together: a rule that always answered with the
    /// fallback would pass the second on its own.
    #[test]
    fn a_footstep_takes_the_terrain_it_lands_on() {
        let sounds = footed();
        assert_eq!(sounds.footstep(1, Some(Footing::Ground(42)), false), Some(653));
        assert_eq!(sounds.footstep(1, Some(Footing::Ground(99)), false), Some(650));
        assert_eq!(sounds.footstep(1, None, false), Some(650));
    }

    /// A splash where the row has one, and the ordinary step where it does
    /// not -- 92 of 217 rows carry no splash and a spider wading should not go
    /// quiet.
    #[test]
    fn wading_splashes_only_where_the_row_says_so() {
        let sounds = footed();
        assert_eq!(sounds.footstep(1, None, true), Some(1054));
        assert_eq!(sounds.footstep(1, Some(Footing::Ground(42)), true), Some(653));
    }

    /// A display with no footstep group has no footsteps, rather than borrowing
    /// somebody else's. Most models are in this case.
    #[test]
    fn a_display_with_no_feet_is_silent() {
        assert_eq!(footed().footstep(2, Some(Footing::Ground(42)), false), None);
    }

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

    /// `(zone_music, ambience_id, parent_area_id)`, matching
    /// `resolve_zone_sound`'s own `areas` map.
    fn area(music: u32, ambience: u32, parent: u32) -> (u32, u32, u32) {
        (music, ambience, parent)
    }

    /// An area naming its own sound uses it, parent or not.
    #[test]
    fn an_areas_own_sound_is_used_over_its_parents() {
        let areas = [(1, area(10, 0, 0)), (2, area(20, 0, 1))].into_iter().collect();
        let table = [(20, (200, 201))].into_iter().collect();
        assert_eq!(resolve_zone_sound(&areas, &table, 2, |r| r.0), Some((200, 201)));
    }

    /// **The bug live-reported as "the wind and birds are gone".** Coldridge
    /// Valley (a sub-area) names no ambience of its own; Dun Morogh (its
    /// parent) does, and that is what a character standing in Coldridge
    /// Valley should hear.
    #[test]
    fn a_zoneless_area_inherits_its_parents_sound() {
        let areas =
            [(132, area(0, 0, 1)), (1, area(8, 42, 0))].into_iter().collect();
        let table = [(42, (420, 421))].into_iter().collect();
        assert_eq!(resolve_zone_sound(&areas, &table, 132, |r| r.1), Some((420, 421)));
    }

    /// The walk goes as many levels up as it has to -- a sub-sub-area
    /// inheriting from its grandparent, neither of the levels in between
    /// naming anything either.
    #[test]
    fn the_walk_climbs_more_than_one_level() {
        let areas = [
            (3, area(0, 0, 2)),
            (2, area(0, 0, 1)),
            (1, area(99, 0, 0)),
        ]
        .into_iter()
        .collect();
        let table = [(99, (990, 991))].into_iter().collect();
        assert_eq!(resolve_zone_sound(&areas, &table, 3, |r| r.0), Some((990, 991)));
    }

    /// An id naming a row that is not there is the same statement as naming
    /// nothing, and must not stop the climb.
    ///
    /// **No area in a stock build exercises this** -- 0 of 2,307 name a
    /// `zone_music` or `ambience_id` missing from its table, measured with
    /// `dbc rows AreaTable` against `dbc dump ZoneMusic/SoundAmbience` --
    /// so this pins a defensive branch rather than a reported bug. Kept
    /// because the failure it prevents is the one this whole function
    /// exists to close: an area going silent instead of inheriting.
    #[test]
    fn an_unresolvable_id_is_climbed_past_like_a_zero() {
        let areas = [(2, area(77, 0, 1)), (1, area(99, 0, 0))].into_iter().collect();
        // 77 names no row; 99 does.
        let table = [(99, (990, 991))].into_iter().collect();
        assert_eq!(resolve_zone_sound(&areas, &table, 2, |r| r.0), Some((990, 991)));
    }

    /// Some areas genuinely have nothing anywhere up their chain -- 76 of
    /// the 1,359 zoneless areas in this build, per this function's own doc
    /// comment -- and that has to read as real silence, not as a failed
    /// lookup that happens to look the same.
    #[test]
    fn an_area_with_no_ancestor_sound_resolves_to_none() {
        let areas = [(5, area(0, 0, 4)), (4, area(0, 0, 0))].into_iter().collect();
        let table = std::collections::HashMap::new();
        assert_eq!(resolve_zone_sound(&areas, &table, 5, |r| r.0), None);
    }

    /// A cycle in `parent_area_id` must terminate rather than hang -- this
    /// project's own rule about a loop over live data needing a bound, this
    /// time for one over a table rather than a network stream.
    #[test]
    fn a_cycle_terminates_rather_than_looping_forever() {
        let areas = [(1, area(0, 0, 2)), (2, area(0, 0, 1))].into_iter().collect();
        let table = std::collections::HashMap::new();
        assert_eq!(resolve_zone_sound(&areas, &table, 1, |r| r.0), None);
    }

    /// An area absent from the map entirely (should not happen -- every
    /// `AreaTable` row populates it -- but the function must not panic if
    /// one somehow is) resolves to nothing rather than panicking.
    #[test]
    fn an_unknown_area_resolves_to_none() {
        let areas = std::collections::HashMap::new();
        let table = std::collections::HashMap::new();
        assert_eq!(resolve_zone_sound(&areas, &table, 999, |r| r.0), None);
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
