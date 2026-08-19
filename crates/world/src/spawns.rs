//! Where the questgivers this client has actually stood in front of were, and
//! what was over their heads at the time.
//!
//! **This is the only part of a native Questie that cannot come off the
//! wire.** Everything else 4.31 draws is a server answer: a quest's text and
//! objectives from `CMSG_QUEST_QUERY`, its map markers from
//! `CMSG_QUEST_POI_QUERY`, and the mark over an NPC's head from
//! `CMSG_QUESTGIVER_STATUS_QUERY`. All three answer about things the player is
//! already carrying or already standing near. None of them answers *"where is
//! there a quest I have not found yet"*, because the server streams the
//! creatures in visibility range and nothing else, and there is no opcode that
//! asks for more.
//!
//! An addon solves that by shipping a hand-collected database of every spawn
//! in the game. This client solves it by **remembering what it was already
//! being sent** -- keyed by realm, starting empty, filling as the player
//! explores. That is strictly less than a shipped database on day one and
//! strictly better afterwards: it cannot be stale, cannot be wrong about a
//! realm with custom content, and needs nobody's licence.
//!
//! ## Every pin here is a memory, and the interface has to say so
//!
//! A live mark is a fact -- it was asked for this frame, about an NPC in
//! range, under this quest log. A remembered one is a fact about **the past**,
//! and three separate things can have changed since:
//!
//! - **The quest log.** Taking a quest turns its giver's exclamation into
//!   nothing, and the server never volunteers that. The viewer already throws
//!   the whole live set away whenever the log changes, because a stale
//!   exclamation is worse than no mark; a remembered set cannot do that,
//!   because it would empty itself every time the player accepted anything.
//!   So [`Questgivers::forget_offering`] is given the log and drops the
//!   remembered mark of any giver whose known offer is now in it, and
//!   everything else stays with its timestamp attached.
//! - **The creature.** It may be dead, or it may wander. The position stored
//!   is the one it was last seen at, not a guess about where it is now.
//! - **The character.** A mark is per player: an exclamation remembered on one
//!   character says nothing about another. Hence [`Questgivers::path_for`]
//!   taking a character name as well as a realm.
//!
//! None of that makes the memory useless -- it makes it a memory. What would
//! make it dishonest is drawing it identically to a live mark, which is why
//! [`Remembered::live`] exists and why the pin is drawn dimmer.
//!
//! ## Why the key is a guid and not an entry
//!
//! An entry names a *kind* of creature and a guid names one **spawn**, and a
//! pin is a claim about a place. Innkeeper Farley's entry stands in a dozen
//! inns; the one this player walked past stands in exactly one.
//!
//! A creature's guid in 3.3.5 is built from its row in the server's `creature`
//! table, so it is the same number next session and the key survives a
//! restart. **A temporary summon's is not** -- it comes off a counter -- so a
//! summoned questgiver can be remembered under a number that later names
//! something else entirely. That is survivable because the record carries its
//! entry too, and a pin whose entry no longer matches what stands there is
//! dropped rather than drawn: see [`Questgivers::see`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

use crate::quest::QuestgiverMark;

/// One questgiver, where it was, and what it was wearing over its head.
#[derive(Debug, Clone, PartialEq)]
pub struct Remembered {
    /// The spawn. See the module docs for why this rather than the entry.
    pub guid: u64,
    /// The creature's kind, kept so a name can be looked up and so a guid that
    /// has been reused for something else can be spotted.
    pub entry: u32,
    pub map: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// What the server last said belongs over this NPC's head, or `None` for
    /// a creature that has been seen and never asked about.
    ///
    /// **Three states, not two, and the third is the whole point.** An NPC
    /// whose quest has been taken answers [`QuestgiverMark::None`], and a
    /// cache that only wrote down the interesting answers would keep drawing
    /// yesterday's `!` over it forever while claiming to be up to date. But
    /// "the server said there is nothing here" and "nobody has asked yet" are
    /// different facts with different futures, and flattening them into one
    /// would make a questgiver this client simply has not got round to
    /// querying look exactly like one with nothing left to give. That is the
    /// absent-versus-default trap, in the one module whose entire job is
    /// remembering.
    /// Seconds since the Unix epoch when the mark was observed, or `0` for a
    /// record whose clock could not be read.
    ///
    /// Stored rather than an `Instant` because it outlives the process. Only
    /// ever shown to a person -- nothing branches on it, so a clock that jumped
    /// costs a misleading "seen 3 hours ago" and not a wrong pin.
    pub mark: Option<QuestgiverMark>,
    pub seen: u64,
    /// Whether this record was refreshed in *this* session, by an NPC actually
    /// in range. Never written to disk -- a loaded cache is all memory by
    /// definition, and a flag that said otherwise would be the one lie this
    /// module exists to avoid.
    pub live: bool,
    /// Quest ids this NPC has been *seen* to offer, from a gossip menu or a
    /// questgiver list the player opened.
    ///
    /// **Usually empty, and that is the ordinary case rather than a gap**: it
    /// fills only for an NPC the player actually talked to, where the mark
    /// fills for every NPC that came into range. It buys one thing --
    /// [`Questgivers::forget_offering`] can retire a remembered exclamation
    /// precisely, instead of every remembered mark having to be distrusted the
    /// moment any quest is accepted.
    pub offers: Vec<u32>,
}

impl Remembered {
    /// Whether this is worth drawing on a map at all.
    ///
    /// The same question [`QuestgiverMark::is_drawn`] answers, asked here so a
    /// caller filtering a map's pins does not have to reach through two
    /// levels to find it.
    pub fn is_drawn(&self) -> bool {
        self.mark.is_some_and(QuestgiverMark::is_drawn)
    }
}

/// Every questgiver this character has walked past on this realm.
#[derive(Debug, Default)]
pub struct Questgivers {
    by_guid: HashMap<u64, Remembered>,
    /// Whether anything has been learned since the last save, so a session
    /// that walked nowhere new does not rewrite the file.
    dirty: bool,
}

impl Questgivers {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many questgivers are remembered. For reporting, not for logic.
    pub fn len(&self) -> usize {
        self.by_guid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_guid.is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Everything remembered, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &Remembered> + '_ {
        self.by_guid.values()
    }

    /// What is remembered about one spawn.
    pub fn get(&self, guid: u64) -> Option<&Remembered> {
        self.by_guid.get(&guid)
    }

    /// Records where an NPC was seen standing.
    ///
    /// Called for every talkable creature in replicated state, every frame it
    /// is there, so it must be cheap and must not dirty the cache for a
    /// creature that has not moved. **A position is only rewritten when it has
    /// changed by more than a stride**: a creature idling in place jitters by
    /// fractions of a unit through interpolation, and writing every one of
    /// those would mark the cache dirty sixty times a second and rewrite the
    /// file on every save tick for the rest of the session.
    ///
    /// **A guid whose entry disagrees with what is remembered is a different
    /// creature**, and the old record is replaced outright rather than
    /// updated. That is the case the module docs warn about: a temporary
    /// summon's guid comes off a counter and can land on a number some
    /// permanent spawn used last session. Keeping the old mark and moving it
    /// to the new position would draw an exclamation over a creature that
    /// never had one.
    pub fn see(&mut self, guid: u64, entry: u32, map: u32, x: f32, y: f32, z: f32) {
        /// How far a remembered creature must move before the memory is
        /// rewritten. About a body's width -- smaller than anything a person
        /// could see on a zone map, larger than any amount of interpolation
        /// jitter.
        const MOVED: f32 = 2.0;

        match self.by_guid.get_mut(&guid) {
            Some(known) if known.entry == entry && known.map == map => {
                let moved = (known.x - x).abs() > MOVED
                    || (known.y - y).abs() > MOVED
                    || (known.z - z).abs() > MOVED;
                if moved {
                    known.x = x;
                    known.y = y;
                    known.z = z;
                    self.dirty = true;
                }
            }
            _ => {
                self.by_guid.insert(
                    guid,
                    Remembered {
                        guid,
                        entry,
                        map,
                        x,
                        y,
                        z,
                        // Not asked yet, which is not the same as asked and
                        // told there is nothing. See the field.
                        mark: None,
                        seen: 0,
                        live: false,
                        offers: Vec::new(),
                    },
                );
                self.dirty = true;
            }
        }
    }

    /// Records what the server said belongs over an NPC's head.
    ///
    /// A no-op for a guid nothing has been [`seen`](Self::see) at -- the mark
    /// alone is not a pin, because a pin needs somewhere to be. That happens
    /// in the ordinary course of things: a reply can arrive after the creature
    /// has gone out of range and been dropped from replicated state.
    pub fn mark(&mut self, guid: u64, mark: QuestgiverMark, now: u64) {
        let Some(known) = self.by_guid.get_mut(&guid) else {
            return;
        };
        // Live either way: the answer is about this session regardless of
        // whether it changed anything.
        known.live = true;
        if known.mark != Some(mark) {
            known.mark = Some(mark);
            known.seen = now;
            self.dirty = true;
        } else if known.seen == 0 {
            known.seen = now;
            self.dirty = true;
        }
    }

    /// Records which quests an NPC was seen offering, from a gossip menu or a
    /// questgiver's list.
    pub fn offers(&mut self, guid: u64, quests: &[u32]) {
        let Some(known) = self.by_guid.get_mut(&guid) else {
            return;
        };
        let mut merged = known.offers.clone();
        for quest in quests {
            if !merged.contains(quest) {
                merged.push(*quest);
            }
        }
        merged.sort_unstable();
        if merged != known.offers {
            known.offers = merged;
            self.dirty = true;
        }
    }

    /// Retires the remembered mark of any NPC whose known offer is now in the
    /// player's quest log.
    ///
    /// **The precise version of a problem the live set solves with a
    /// sledgehammer.** The viewer throws away every live mark whenever the log
    /// changes, because accepting a quest turns its giver's exclamation into
    /// nothing and the server does not volunteer that. It can afford to: the
    /// NPCs are in range and are simply asked again. A remembered set cannot
    /// -- most of its NPCs are miles away and unaskable -- so doing the same
    /// would empty the map every time the player accepted anything.
    ///
    /// So the mark is dropped only where this client *knows* what the NPC was
    /// offering, which is the case for any NPC the player actually talked to.
    /// Dropped back to "not asked" rather than set to
    /// [`QuestgiverMark::None`]: the quest has been taken, so what belongs
    /// there now is a question this client has not asked, and next time the
    /// player walks past it will.
    ///
    /// Returns how many were retired, so a probe can say so.
    pub fn forget_offering(&mut self, log: &[u32]) -> usize {
        let mut retired = 0;
        for known in self.by_guid.values_mut() {
            if known.mark.is_none() || known.offers.is_empty() {
                continue;
            }
            // Every quest it was known to offer is now carried, so there is
            // nothing left for it to be exclaiming about.
            if known.offers.iter().all(|quest| log.contains(quest)) {
                known.mark = None;
                known.live = false;
                retired += 1;
                self.dirty = true;
            }
        }
        retired
    }

    /// Everything remembered on one map that is worth drawing.
    ///
    /// Filtered by map rather than by page, because a page is a `ui` concept
    /// and this crate depends on neither the interface nor the game data. The
    /// caller projects and discards whatever falls outside its own rectangle,
    /// exactly as it already does for a quest objective's polygon.
    pub fn on_map(&self, map: u32) -> impl Iterator<Item = &Remembered> + '_ {
        self.by_guid
            .values()
            .filter(move |known| known.map == map && known.is_drawn())
    }

    /// Reads a cache file. **A missing file is an empty cache, not an error**
    /// -- the first run on a realm has none, and that is the ordinary case.
    /// A corrupt one is reported, because silently starting empty would throw
    /// the player's whole map away every launch with nothing to show for it.
    pub fn load(path: &Path) -> Result<Self, SpawnCacheError> {
        let mut cache = Self::new();
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(cache),
            Err(error) => return Err(SpawnCacheError::Io(error)),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(SpawnCacheError::Io)?;

        let mut r = crate::protocol::Reader::new(&bytes, "questgiver cache");
        let magic = r.bytes::<4>().map_err(|_| SpawnCacheError::Truncated)?;
        if magic != *MAGIC {
            return Err(SpawnCacheError::NotACache);
        }
        let version = r.u32().map_err(|_| SpawnCacheError::Truncated)?;
        if version != VERSION {
            // The *file layout* changed. Starting fresh costs a walk, which is
            // the honest price and is cheaper than reading old records under a
            // new layout and drawing whatever falls out.
            return Ok(cache);
        }
        let count = r.u32().map_err(|_| SpawnCacheError::Truncated)?;
        for _ in 0..count {
            let guid = r.u64().map_err(|_| SpawnCacheError::Truncated)?;
            let entry = r.u32().map_err(|_| SpawnCacheError::Truncated)?;
            let map = r.u32().map_err(|_| SpawnCacheError::Truncated)?;
            let x = r.f32().map_err(|_| SpawnCacheError::Truncated)?;
            let y = r.f32().map_err(|_| SpawnCacheError::Truncated)?;
            let z = r.f32().map_err(|_| SpawnCacheError::Truncated)?;
            let raw = r.u8().map_err(|_| SpawnCacheError::Truncated)?;
            let seen = r.u64().map_err(|_| SpawnCacheError::Truncated)?;
            let offers_count = r.u32().map_err(|_| SpawnCacheError::Truncated)?;
            let mut offers = Vec::with_capacity(offers_count as usize);
            for _ in 0..offers_count {
                offers.push(r.u32().map_err(|_| SpawnCacheError::Truncated)?);
            }
            cache.by_guid.insert(
                guid,
                Remembered {
                    guid,
                    entry,
                    map,
                    x,
                    y,
                    z,
                    // `NOT_ASKED` is this file's word, not the wire's -- see
                    // the constant.
                    mark: (raw != NOT_ASKED).then(|| QuestgiverMark::from_status(raw)),
                    seen,
                    // **Nothing loaded is live**, by definition. This is the
                    // field that keeps a remembered pin from being drawn as a
                    // fact, so it is the one field that must not survive a
                    // round trip through the disk.
                    live: false,
                    offers,
                },
            );
        }
        // Loading is not learning: a save straight after a load writes nothing.
        cache.dirty = false;
        Ok(cache)
    }

    /// Writes the cache, creating the parent directory if it is missing.
    ///
    /// Written to a temporary file and renamed, so an interrupted save leaves
    /// the previous cache intact rather than a half-written one -- the same
    /// shape [`crate::QuestCache::save`] uses, and for the same reason.
    /// **Takes `&mut self` and clears the dirty flag on success**, which the
    /// quest cache's does not. Without that a client whose first sighting made
    /// the cache dirty rewrites the same file every save tick for the rest of
    /// the session, which is invisible, harmless and wrong.
    pub fn save(&mut self, path: &Path) -> Result<(), SpawnCacheError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(SpawnCacheError::Io)?;
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(self.by_guid.len() as u32).to_le_bytes());
        // Sorted so two runs that learned the same NPCs in a different order
        // write the same file, which is what makes diffing two caches mean
        // anything.
        let mut guids: Vec<u64> = self.by_guid.keys().copied().collect();
        guids.sort_unstable();
        for guid in guids {
            let known = &self.by_guid[&guid];
            bytes.extend_from_slice(&known.guid.to_le_bytes());
            bytes.extend_from_slice(&known.entry.to_le_bytes());
            bytes.extend_from_slice(&known.map.to_le_bytes());
            bytes.extend_from_slice(&known.x.to_le_bytes());
            bytes.extend_from_slice(&known.y.to_le_bytes());
            bytes.extend_from_slice(&known.z.to_le_bytes());
            bytes.push(known.mark.map_or(NOT_ASKED, QuestgiverMark::to_status));
            bytes.extend_from_slice(&known.seen.to_le_bytes());
            bytes.extend_from_slice(&(known.offers.len() as u32).to_le_bytes());
            for quest in &known.offers {
                bytes.extend_from_slice(&quest.to_le_bytes());
            }
        }

        let temporary = path.with_extension("tmp");
        let mut file = std::fs::File::create(&temporary).map_err(SpawnCacheError::Io)?;
        file.write_all(&bytes).map_err(SpawnCacheError::Io)?;
        file.sync_all().map_err(SpawnCacheError::Io)?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(SpawnCacheError::Io)?;
        self.dirty = false;
        Ok(())
    }

    /// Where one character's cache on one realm lives.
    ///
    /// **Keyed by character as well as by realm, unlike the quest cache**, and
    /// the difference is the point. What a quest *is* is a fact about the
    /// realm and is the same for everybody on it. What is over an NPC's head
    /// is a fact about one character's progress: a quest one character has
    /// finished is still on offer to another, and sharing the file would draw
    /// a fresh alt a map with every exclamation already crossed off.
    pub fn path_for(base: &Path, realm: &str, character: &str) -> std::path::PathBuf {
        base.join(format!(
            "questgivers-{}-{}.cache",
            sanitise(realm),
            sanitise(character)
        ))
    }
}

/// Both halves of a filename come off the wire, so both are reduced to
/// characters that cannot be a path.
fn sanitise(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Identifies the file, so pointing this at something else fails loudly.
const MAGIC: &[u8; 4] = b"OWQG";
/// The *file layout's* version, not the packet's.
const VERSION: u32 = 1;

/// The byte this file uses for "seen, never asked".
///
/// **A file's word, not the wire's.** The server sends the small numbers
/// [`QuestgiverMark`] documents; `0xFF` is not one of them, and if it ever
/// became one it would read back as [`QuestgiverMark::Unknown`], which draws
/// nothing. So the worst this sentinel can do is turn a mark this client has
/// never seen into one it has never asked for -- and both of those draw the
/// same blank.
const NOT_ASKED: u8 = 0xFF;

#[derive(Debug, thiserror::Error)]
pub enum SpawnCacheError {
    #[error("reading or writing the questgiver cache: {0}")]
    Io(#[from] std::io::Error),
    #[error("that file is not a questgiver cache")]
    NotACache,
    #[error("the questgiver cache ends in the middle of a record")]
    Truncated,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deputy Willem's real guid, off the capture in
    /// [`crate::quest`]'s tests.
    const WILLEM: u64 = 0xf130_0003_3701_2a8c;

    fn seen_willem() -> Questgivers {
        let mut givers = Questgivers::new();
        givers.see(WILLEM, 823, 0, -8928.0, -187.0, 82.0);
        givers
    }

    /// **The three states, and that the middle one is not the last one.**
    ///
    /// A creature seen and never asked about, a creature the server said has
    /// nothing, and a creature with a quest are three different facts. The
    /// first two are the pair that would silently collapse.
    #[test]
    fn seen_and_asked_and_told_nothing_are_three_states() {
        let mut givers = seen_willem();
        assert_eq!(givers.get(WILLEM).unwrap().mark, None);
        assert!(!givers.get(WILLEM).unwrap().is_drawn());

        givers.mark(WILLEM, QuestgiverMark::None, 100);
        assert_eq!(givers.get(WILLEM).unwrap().mark, Some(QuestgiverMark::None));
        assert!(
            !givers.get(WILLEM).unwrap().is_drawn(),
            "the server said there is nothing here"
        );

        givers.mark(WILLEM, QuestgiverMark::Available, 200);
        assert!(givers.get(WILLEM).unwrap().is_drawn());
        assert_eq!(givers.get(WILLEM).unwrap().seen, 200);
    }

    /// A mark about a creature nothing has been seen at is dropped: a pin
    /// needs somewhere to be, and a reply can outlive its NPC's visibility.
    #[test]
    fn a_mark_with_no_sighting_is_not_a_pin() {
        let mut givers = Questgivers::new();
        givers.mark(WILLEM, QuestgiverMark::Available, 1);
        assert!(givers.is_empty());
        assert!(!givers.is_dirty());
    }

    /// **The check that keeps the file from being rewritten sixty times a
    /// second.** An idling creature jitters through interpolation, and every
    /// frame of that would mark the cache dirty.
    #[test]
    fn a_creature_that_has_not_moved_does_not_dirty_the_cache() {
        let mut givers = seen_willem();
        assert!(givers.is_dirty(), "the first sighting is news");
        let file = std::env::temp_dir().join("owc-questgivers-jitter.cache");
        givers.save(&file).unwrap();
        assert!(!givers.is_dirty(), "a save is the end of the news");

        // Interpolation jitter.
        givers.see(WILLEM, 823, 0, -8928.4, -186.8, 82.1);
        assert!(!givers.is_dirty(), "a tenth of a unit is not a move");

        // A walk.
        givers.see(WILLEM, 823, 0, -8900.0, -187.0, 82.0);
        assert!(givers.is_dirty());
        assert_eq!(givers.get(WILLEM).unwrap().x, -8900.0);
    }

    /// **A guid whose entry disagrees is a different creature.** A temporary
    /// summon's guid comes off a counter and can land on a number a permanent
    /// spawn used last session; keeping the old mark would draw an
    /// exclamation over something that never had one.
    #[test]
    fn a_reused_guid_replaces_the_record_rather_than_moving_it() {
        let mut givers = seen_willem();
        givers.mark(WILLEM, QuestgiverMark::Available, 5);
        assert!(givers.get(WILLEM).unwrap().is_drawn());

        givers.see(WILLEM, 12345, 0, 100.0, 200.0, 300.0);
        let known = givers.get(WILLEM).unwrap();
        assert_eq!(known.entry, 12345);
        assert_eq!(known.mark, None, "the old mark did not follow the guid");
        assert!(!known.is_drawn());
    }

    /// The same rule across a map change: a creature on another continent at
    /// the same guid is not the one that was remembered.
    #[test]
    fn a_map_change_replaces_the_record_too() {
        let mut givers = seen_willem();
        givers.mark(WILLEM, QuestgiverMark::Complete, 5);
        givers.see(WILLEM, 823, 1, 100.0, 200.0, 300.0);
        assert_eq!(givers.get(WILLEM).unwrap().mark, None);
    }

    /// **The precise retirement, and the thing it must not do.**
    ///
    /// An NPC known to offer a quest that is now in the log stops exclaiming;
    /// an NPC whose offer nobody has ever seen keeps its mark, because
    /// dropping it would empty the map every time anything was accepted.
    #[test]
    fn accepting_a_quest_retires_only_the_giver_that_was_known_to_offer_it() {
        let mut givers = seen_willem();
        givers.mark(WILLEM, QuestgiverMark::Available, 1);
        givers.offers(WILLEM, &[783]);

        const STRANGER: u64 = 0xf130_0003_3701_9999;
        givers.see(STRANGER, 197, 0, -8900.0, -180.0, 82.0);
        givers.mark(STRANGER, QuestgiverMark::Available, 1);

        assert_eq!(givers.forget_offering(&[783]), 1);
        assert_eq!(givers.get(WILLEM).unwrap().mark, None);
        assert!(
            givers.get(STRANGER).unwrap().is_drawn(),
            "an NPC whose offer was never seen keeps its mark"
        );

        // And it is idempotent: a second pass retires nothing.
        assert_eq!(givers.forget_offering(&[783]), 0);
    }

    /// An NPC offering two quests keeps its mark until *both* are carried.
    #[test]
    fn a_giver_with_more_to_offer_keeps_exclaiming() {
        let mut givers = seen_willem();
        givers.mark(WILLEM, QuestgiverMark::Available, 1);
        givers.offers(WILLEM, &[783, 7]);
        assert_eq!(givers.forget_offering(&[783]), 0);
        assert!(givers.get(WILLEM).unwrap().is_drawn());
        assert_eq!(givers.forget_offering(&[783, 7]), 1);
    }

    /// `on_map` answers for one continent and only for marks worth drawing.
    #[test]
    fn only_drawable_marks_on_the_asked_for_map_come_back() {
        let mut givers = seen_willem();
        givers.mark(WILLEM, QuestgiverMark::Available, 1);
        const KALIMDOR: u64 = 0xf130_0003_3702_0000;
        givers.see(KALIMDOR, 1, 1, 0.0, 0.0, 0.0);
        givers.mark(KALIMDOR, QuestgiverMark::Available, 1);
        const SILENT: u64 = 0xf130_0003_3702_0001;
        givers.see(SILENT, 2, 0, 0.0, 0.0, 0.0);
        givers.mark(SILENT, QuestgiverMark::None, 1);

        let on_zero: Vec<u64> = givers.on_map(0).map(|known| known.guid).collect();
        assert_eq!(on_zero, vec![WILLEM]);
        assert_eq!(givers.on_map(1).count(), 1);
        assert_eq!(givers.on_map(2).count(), 0);
    }

    /// **The round trip, because this structure travels both ways.**
    ///
    /// A bad read fails at a known offset; a bad write is accepted as some
    /// other valid record. Every field is varied so a layout that swapped two
    /// of them could not survive, and the sample deliberately includes a mark
    /// this client cannot name, a record with no mark at all, and one with an
    /// offer list -- the three shapes a fixed-width reading would get wrong.
    #[test]
    fn a_cache_survives_a_save_and_a_load() {
        let mut givers = Questgivers::new();
        givers.see(WILLEM, 823, 0, -8928.5, -187.25, 82.75);
        givers.mark(WILLEM, QuestgiverMark::Available, 1_700_000_000);
        givers.offers(WILLEM, &[783, 7]);

        const NAMELESS: u64 = 0x1122_3344_5566_7788;
        givers.see(NAMELESS, 4242, 571, 1.5, -2.5, 3.5);
        givers.mark(NAMELESS, QuestgiverMark::Unknown(9), 42);

        const UNASKED: u64 = 0xdead_beef_0000_0001;
        givers.see(UNASKED, 1, 1, 0.0, 0.0, 0.0);

        let file = std::env::temp_dir().join("owc-questgivers-roundtrip.cache");
        givers.save(&file).unwrap();
        let back = Questgivers::load(&file).unwrap();
        std::fs::remove_file(&file).ok();

        assert_eq!(back.len(), 3);
        for guid in [WILLEM, NAMELESS, UNASKED] {
            // Every field but `live`, which deliberately does not survive --
            // see `a_loaded_record_is_never_live`.
            let mut before = givers.get(guid).unwrap().clone();
            before.live = false;
            assert_eq!(
                &before,
                back.get(guid).unwrap(),
                "record {guid:#018x} did not survive"
            );
        }
        // A mark the server sent and this client cannot name still comes back
        // as itself rather than as something plausible.
        assert_eq!(
            back.get(NAMELESS).unwrap().mark,
            Some(QuestgiverMark::Unknown(9))
        );
        // And "never asked" is still not "told there is nothing".
        assert_eq!(back.get(UNASKED).unwrap().mark, None);
        assert!(!back.is_dirty(), "loading is not learning");
    }

    /// **Nothing loaded is live.** This is the field that separates a
    /// remembered pin from a fact, so it is the one that must not come back
    /// off the disk set.
    #[test]
    fn a_loaded_record_is_never_live() {
        let mut givers = seen_willem();
        givers.mark(WILLEM, QuestgiverMark::Available, 7);
        assert!(givers.get(WILLEM).unwrap().live, "asked this session");

        let file = std::env::temp_dir().join("owc-questgivers-live.cache");
        givers.save(&file).unwrap();
        let back = Questgivers::load(&file).unwrap();
        std::fs::remove_file(&file).ok();
        assert!(!back.get(WILLEM).unwrap().live);
        assert!(back.get(WILLEM).unwrap().is_drawn(), "still worth drawing");
    }

    /// A missing file is an empty cache; a file that is something else is an
    /// error. Silently starting empty on a corrupt file would throw the
    /// player's map away every launch with nothing said.
    #[test]
    fn a_missing_file_is_empty_and_a_wrong_one_is_loud() {
        let missing = std::env::temp_dir().join("owc-questgivers-not-here.cache");
        std::fs::remove_file(&missing).ok();
        assert!(Questgivers::load(&missing).unwrap().is_empty());

        let wrong = std::env::temp_dir().join("owc-questgivers-wrong.cache");
        std::fs::write(&wrong, b"this is not a cache").unwrap();
        assert!(matches!(
            Questgivers::load(&wrong),
            Err(SpawnCacheError::NotACache)
        ));
        std::fs::remove_file(&wrong).ok();
    }

    /// A file cut short is an error rather than however many records it
    /// managed -- the same half of the cursor rule every wire parser here
    /// asserts.
    #[test]
    fn a_truncated_file_is_an_error() {
        let mut givers = seen_willem();
        givers.mark(WILLEM, QuestgiverMark::Available, 1);
        let file = std::env::temp_dir().join("owc-questgivers-short.cache");
        givers.save(&file).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() - 4]).unwrap();
        assert!(matches!(
            Questgivers::load(&file),
            Err(SpawnCacheError::Truncated)
        ));
        std::fs::remove_file(&file).ok();
    }

    /// **Two characters on one realm do not share a file.** A quest one has
    /// finished is still on offer to the other, and one shared cache would
    /// hand a fresh alt a map with every exclamation already crossed off.
    #[test]
    fn the_path_names_the_character_as_well_as_the_realm() {
        let base = Path::new("/tmp");
        let wolf = Questgivers::path_for(base, "AzerothCore", "Testwolf");
        let druid = Questgivers::path_for(base, "AzerothCore", "Testdruid");
        assert_ne!(wolf, druid);
        // And a realm name that is a path does not become one.
        let nasty = Questgivers::path_for(base, "../../etc", "a/b");
        assert_eq!(
            nasty.file_name().unwrap().to_str().unwrap(),
            "questgivers-______etc-a_b.cache"
        );
    }

    /// Offers accumulate and de-duplicate, and an unchanged list does not
    /// dirty the cache -- the same reason the position has a threshold.
    #[test]
    fn offers_merge_without_repeating_themselves() {
        let mut givers = seen_willem();
        givers.offers(WILLEM, &[783]);
        givers.offers(WILLEM, &[7, 783]);
        assert_eq!(givers.get(WILLEM).unwrap().offers, vec![7, 783]);

        let file = std::env::temp_dir().join("owc-questgivers-offers.cache");
        givers.save(&file).unwrap();
        std::fs::remove_file(&file).ok();
        givers.offers(WILLEM, &[783, 7]);
        assert!(!givers.is_dirty(), "nothing new was learned");
    }
}
