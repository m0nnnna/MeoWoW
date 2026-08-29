//! What the server has said about which quests, kept between sessions.
//!
//! **This is the half of "quest data comes from the server" that makes the
//! decision affordable.** There is no enumerate-all opcode, so nothing can be
//! fetched in bulk; and a mass query at login would repeat the thirty-seven
//! second burst this project has already paid for once. So quests are asked
//! about when their ids are first seen -- from the log, from a questgiver's
//! list, from a gossip packet -- and the answers are kept.
//!
//! ## The rule that must not be got wrong
//!
//! **A missing answer is *unknown*, never *nothing*.** A quest whose reply has
//! not arrived and a quest with no objectives produce the same empty screen if
//! the two are conflated, and the wrong one of those is silent and permanent.
//! [`Answer`] therefore has no variant a caller can mistake for data: there is
//! no `get() -> Option<&QuestInfo>` here, because `None` would mean both
//! "still waiting" and "genuinely empty" and every call site would have to
//! remember which.
//!
//! This is the same trap as an absent update field reading as unknown instead
//! of zero, and the loot short form -- both cost a milestone.
//!
//! ## Why the cache holds bytes rather than structs
//!
//! Each entry is the packet body exactly as it arrived. That costs a parse on
//! load and buys two things worth more:
//!
//! - **A cache of parsed structs freezes a parse; a cache of bodies freezes an
//!   observation.** When [`crate::quest::parse_quest_query`] learns to read a
//!   field it currently skips, every already-cached quest gains it, with no
//!   version stamp and no migration. A struct cache would have to be thrown
//!   away instead, and the alternative -- reading old entries with the new
//!   field silently defaulted -- is a fabricated number, which this project
//!   treats as worse than a blank.
//! - It needs no serialisation format and no derive on thirty fields, so
//!   there is nothing to drift from the wire layout.
//!
//! ## What is deliberately not persisted
//!
//! Only answers are written. An id that was asked about and never answered
//! stays a fact about *this session* and is asked again next time, because the
//! two reasons for a silence -- there is no such quest, and the reply was lost
//! -- are indistinguishable here, and only one of them is permanent. Writing
//! the guess down would make a transient failure last forever.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;

use crate::quest::{parse_quest_query, QuestInfo};

/// What is known about one quest id.
///
/// Deliberately not an `Option`: the three states want three different things
/// on screen -- a quest's real objectives, a spinner, and a line saying the
/// server would not say -- and collapsing any two of them is the trap this
/// module exists to avoid.
// No `Eq`: a quest carries a float (the honor multiplier), so equality on it
// is `PartialEq` only. `assert_eq!` and `!=` need nothing more.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Answer<'a> {
    /// Never asked. The caller should ask.
    Unknown,
    /// Asked, and the answer has not arrived.
    Pending,
    /// Asked, and nothing came back. **Not the same as a quest with no
    /// content**, and it is retried in a later session.
    Unanswered,
    /// The server described it.
    Known(&'a QuestInfo),
}

/// Quests the server has described, and which ids have been asked about.
#[derive(Debug, Default)]
pub struct QuestCache {
    /// Parsed answers, and the bodies they were parsed from. Both are kept:
    /// the parse is what callers read every frame, and the body is what gets
    /// written to disk.
    known: HashMap<u32, (Vec<u8>, QuestInfo)>,
    /// Asked, still waiting. Session-scoped.
    pending: HashSet<u32>,
    /// Asked, nothing came back. Session-scoped -- see the module docs.
    unanswered: HashSet<u32>,
    /// Whether anything has been learned since the last save, so a session
    /// that discovers nothing does not rewrite the file.
    dirty: bool,
}

/// How many ids the server will answer in one `CMSG_QUEST_QUERY` burst before
/// it is worth pausing.
///
/// **Not a protocol limit** -- the query takes one id per request and has no
/// stated cap. It is a self-imposed one, because the failure this guards
/// against has happened: a drain loop with a packet bound and no clock spent
/// thirty-seven seconds on the login burst before the first frame was drawn.
/// Asking about a hundred quests at once would do the same thing again.
pub const ASK_AT_ONCE: usize = 16;

impl QuestCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is known about one quest.
    pub fn answer(&self, quest: u32) -> Answer<'_> {
        if let Some((_, info)) = self.known.get(&quest) {
            Answer::Known(info)
        } else if self.pending.contains(&quest) {
            Answer::Pending
        } else if self.unanswered.contains(&quest) {
            Answer::Unanswered
        } else {
            Answer::Unknown
        }
    }

    /// How many quests are cached. For reporting, not for logic.
    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    /// Picks the next ids worth asking about and marks them in flight.
    ///
    /// **Marking them here rather than at the send is deliberate**: a caller
    /// that asked and then asked again next frame would send the same query
    /// sixty times a second, and the server would answer every one. The
    /// contract is that the caller sends whatever this returns.
    ///
    /// Order follows the caller's list rather than the set's iteration order,
    /// so "the quest the player just clicked" is asked before whatever else
    /// happened to be unknown.
    pub fn take_unknown(&mut self, wanted: &[u32], limit: usize) -> Vec<u32> {
        let mut asking = Vec::new();
        for quest in wanted {
            if asking.len() >= limit {
                break;
            }
            // Zero is not a quest -- it is what an empty log slot reads as --
            // and asking about it would be answered with nothing forever.
            if *quest == 0 || self.answer(*quest) != Answer::Unknown {
                continue;
            }
            self.pending.insert(*quest);
            asking.push(*quest);
        }
        asking
    }

    /// Files an answer, straight from the wire.
    ///
    /// Takes the body rather than a parsed quest so that the cache and the
    /// parser cannot disagree about what was received: there is one parse, and
    /// its input is what gets written to disk.
    pub fn insert(&mut self, body: &[u8]) -> Result<u32, crate::protocol::Error> {
        let info = parse_quest_query(body)?;
        let id = info.id;
        self.pending.remove(&id);
        self.unanswered.remove(&id);
        self.known.insert(id, (body.to_vec(), info));
        self.dirty = true;
        Ok(id)
    }

    /// Records that an id was asked about and produced nothing.
    ///
    /// Does **not** mark the cache dirty: this is not knowledge and is not
    /// written down. See the module docs for why a silence is not a fact worth
    /// keeping.
    pub fn give_up(&mut self, quest: u32) {
        if self.pending.remove(&quest) {
            self.unanswered.insert(quest);
        }
    }

    /// Every id still waiting for a reply, so a caller can time them out.
    pub fn pending(&self) -> impl Iterator<Item = u32> + '_ {
        self.pending.iter().copied()
    }

    /// Every item entry a quest's "to find" objective names, for the quests
    /// in `log` this cache has an answer for.
    ///
    /// **What made a quest item show as `Item 11119` until the first copy was
    /// picked up.** An item objective's entry is not something the character
    /// carries yet, so it appears in nothing a bag walk sees, and nothing
    /// asked `CMSG_ITEM_QUERY_SINGLE` for it -- the query was sent only once a
    /// copy landed in a bag. This is the list a caller adds to that walk, the
    /// same way an open loot window's rows are.
    pub fn item_objective_entries(&self, log: &[u32]) -> Vec<u32> {
        log.iter()
            .filter_map(|id| match self.answer(*id) {
                Answer::Known(info) => Some(info),
                _ => None,
            })
            .flat_map(|info| info.item_objectives.iter().map(|item| item.item))
            .collect()
    }

    /// Whether anything has been learned since the last [`QuestCache::save`].
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Reads a cache file. **A missing file is an empty cache, not an error**
    /// -- the first run on a realm has none, and that is the ordinary case
    /// rather than a fault.
    ///
    /// A *corrupt* file is different and is reported, because silently
    /// starting empty would hide a bug that loses the player's whole cache
    /// every launch.
    pub fn load(path: &Path) -> Result<Self, CacheError> {
        let mut cache = Self::new();
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(cache),
            Err(error) => return Err(CacheError::Io(error)),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(CacheError::Io)?;

        let mut r = crate::protocol::Reader::new(&bytes, "quest cache");
        let magic = r.bytes::<4>().map_err(|_| CacheError::Truncated)?;
        if magic != *MAGIC {
            return Err(CacheError::NotACache);
        }
        let version = r.u32().map_err(|_| CacheError::Truncated)?;
        if version != VERSION {
            // A version bump means the *file format* changed, not the packet
            // layout -- the bodies inside are still whatever the server sent.
            // Starting fresh is the honest response and costs only re-asking.
            return Ok(cache);
        }
        let count = r.u32().map_err(|_| CacheError::Truncated)?;
        for _ in 0..count {
            let _id = r.u32().map_err(|_| CacheError::Truncated)?;
            let length = r.u32().map_err(|_| CacheError::Truncated)? as usize;
            let body = r.take(length).map_err(|_| CacheError::Truncated)?;
            // **A body that no longer parses is dropped, not fatal.** It will
            // simply be asked for again. The alternative -- refusing to load
            // the whole file -- turns one bad entry into a lost cache.
            let _ = cache.insert(body);
        }
        // Loading is not learning: a save straight after a load should write
        // nothing.
        cache.dirty = false;
        Ok(cache)
    }

    /// Writes the cache, creating the parent directory if it is missing.
    ///
    /// Written to a temporary file and renamed, so an interrupted save leaves
    /// the previous cache intact rather than a half-written one. The file is
    /// rebuilt from scratch every time, which is affordable because it is
    /// small and because an append-only format would need compaction.
    pub fn save(&self, path: &Path) -> Result<(), CacheError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(CacheError::Io)?;
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(self.known.len() as u32).to_le_bytes());
        // Sorted so the file is stable between runs that learned the same
        // quests in a different order -- which makes a diff of two caches mean
        // something.
        let mut ids: Vec<u32> = self.known.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let (body, _) = &self.known[&id];
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
            bytes.extend_from_slice(body);
        }

        let temporary = path.with_extension("tmp");
        let mut file = std::fs::File::create(&temporary).map_err(CacheError::Io)?;
        file.write_all(&bytes).map_err(CacheError::Io)?;
        file.sync_all().map_err(CacheError::Io)?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(CacheError::Io)?;
        Ok(())
    }

    /// Where one realm's cache lives.
    ///
    /// **Keyed by realm, and that is not a tidiness decision.** The whole
    /// point of asking the server rather than shipping a database is being
    /// correct on a realm with custom content; one shared file would let a
    /// private realm's altered quest 783 be shown on a different realm where
    /// it is the original. The realm name is sanitised because it comes off
    /// the wire and would otherwise be a path.
    pub fn path_for(base: &Path, realm: &str) -> std::path::PathBuf {
        let safe: String = realm
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        base.join(format!("quests-{safe}.cache"))
    }
}

/// Identifies the file, so pointing this at something else fails loudly.
const MAGIC: &[u8; 4] = b"OWQC";
/// The *file layout's* version, not the packet's. See [`QuestCache::load`].
const VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("reading or writing the quest cache: {0}")]
    Io(#[from] std::io::Error),
    #[error("that file is not a quest cache")]
    NotACache,
    #[error("the quest cache ends in the middle of an entry")]
    Truncated,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest body `parse_quest_query` will accept: a 260-byte head,
    /// five empty strings, the objective block, and four empty strings.
    fn body(id: u32) -> Vec<u8> {
        body_with_item(id, 0, 0)
    }

    /// Same shape as `body`, with the first item-objective slot filled in --
    /// what a quest asking the player to find an item looks like on the wire.
    fn body_with_item(id: u32, item: u32, count: u32) -> Vec<u8> {
        let mut body = vec![0u8; 260];
        body[..4].copy_from_slice(&id.to_le_bytes());
        body.extend_from_slice(&[0; 5]); // five empty strings
        body.extend_from_slice(&[0u8; 4 * 4 * 4]); // four objectives
        body.extend_from_slice(&item.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
        body.extend_from_slice(&[0u8; 5 * 2 * 4]); // five more item objectives
        body.extend_from_slice(&[0; 4]); // four empty objective texts
        body
    }

    /// **The distinction the whole module exists for.** Three states, three
    /// answers, and none of them is an `Option` a caller could flatten.
    #[test]
    fn unknown_pending_and_unanswered_are_three_different_things() {
        let mut cache = QuestCache::new();
        assert_eq!(cache.answer(783), Answer::Unknown);

        assert_eq!(cache.take_unknown(&[783], 8), vec![783]);
        assert_eq!(cache.answer(783), Answer::Pending);

        cache.give_up(783);
        assert_eq!(cache.answer(783), Answer::Unanswered);
        assert!(
            !matches!(cache.answer(783), Answer::Known(_)),
            "a silence must never read as a described quest"
        );
    }

    /// Asking twice for the same id must send once. A caller that re-asked
    /// every frame would query sixty times a second and be answered every
    /// time.
    #[test]
    fn an_id_already_in_flight_is_not_asked_again() {
        let mut cache = QuestCache::new();
        assert_eq!(cache.take_unknown(&[783, 16], 8), vec![783, 16]);
        assert!(cache.take_unknown(&[783, 16], 8).is_empty());
    }

    /// Nor is one already answered.
    #[test]
    fn a_known_id_is_not_asked_again() {
        let mut cache = QuestCache::new();
        cache.insert(&body(783)).unwrap();
        assert!(cache.take_unknown(&[783], 8).is_empty());
    }

    /// Zero is what an empty quest-log slot reads as, and asking about it
    /// would be answered with silence forever.
    #[test]
    fn zero_is_never_asked_about() {
        let mut cache = QuestCache::new();
        assert!(cache.take_unknown(&[0], 8).is_empty());
        assert_eq!(cache.answer(0), Answer::Unknown);
    }

    /// The limit is honoured, and the ids taken are the caller's first ones
    /// rather than an arbitrary subset -- so the quest just clicked is asked
    /// before whatever else was outstanding.
    #[test]
    fn the_batch_limit_takes_the_front_of_the_list() {
        let mut cache = QuestCache::new();
        assert_eq!(cache.take_unknown(&[5, 6, 7, 8], 2), vec![5, 6]);
        assert_eq!(cache.answer(7), Answer::Unknown);
    }

    /// **The bug: a quest's item objective read as `Item 11119` until a copy
    /// was picked up.** The tracker names an item objective from whatever the
    /// cache holds, but nothing ever asked the server for it while the log
    /// only *wanted* the item rather than carrying it -- this is the list a
    /// caller adds to the ordinary bag walk so the query goes out as soon as
    /// the quest is known, not once the item is.
    #[test]
    fn a_logged_quests_item_objective_is_asked_for_before_its_carried() {
        let mut cache = QuestCache::new();
        cache.insert(&body_with_item(333, 11119, 1)).unwrap();

        assert_eq!(cache.item_objective_entries(&[333]), vec![11119]);
        // A quest still pending, or never asked about, names nothing yet --
        // there is no answer to read an objective out of.
        assert!(cache.item_objective_entries(&[16]).is_empty());
        // A quest with no item objective at all -- the ordinary case --
        // contributes nothing either.
        cache.insert(&body(85)).unwrap();
        assert!(cache.item_objective_entries(&[85]).is_empty());
    }

    /// A round trip through a file keeps every answer.
    #[test]
    fn a_saved_cache_loads_back() {
        let directory = std::env::temp_dir().join("owc-quest-cache-test");
        let _ = std::fs::remove_dir_all(&directory);
        let path = QuestCache::path_for(&directory, "AzerothCore");

        let mut cache = QuestCache::new();
        cache.insert(&body(783)).unwrap();
        cache.insert(&body(16)).unwrap();
        assert!(cache.is_dirty());
        cache.save(&path).unwrap();

        let loaded = QuestCache::load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(matches!(loaded.answer(783), Answer::Known(q) if q.id == 783));
        assert!(matches!(loaded.answer(16), Answer::Known(q) if q.id == 16));
        // **A load is not a discovery.** Saving straight afterwards must write
        // nothing, or every launch rewrites the file.
        assert!(!loaded.is_dirty());
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// **A silence is not written down**, so the next session asks again. The
    /// two reasons for one -- no such quest, and a lost reply -- are
    /// indistinguishable, and only one of them is permanent.
    #[test]
    fn an_unanswered_id_does_not_survive_a_save() {
        let directory = std::env::temp_dir().join("owc-quest-cache-silence");
        let _ = std::fs::remove_dir_all(&directory);
        let path = QuestCache::path_for(&directory, "AzerothCore");

        let mut cache = QuestCache::new();
        cache.take_unknown(&[783], 8);
        cache.give_up(783);
        cache.save(&path).unwrap();

        let loaded = QuestCache::load(&path).unwrap();
        assert_eq!(
            loaded.answer(783),
            Answer::Unknown,
            "a session that got no answer must not teach the next one to stop asking"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A missing file is the ordinary first run, not a fault.
    #[test]
    fn a_missing_file_loads_as_an_empty_cache() {
        let path = std::env::temp_dir().join("owc-quest-cache-absent/quests-Nowhere.cache");
        let _ = std::fs::remove_file(&path);
        let cache = QuestCache::load(&path).unwrap();
        assert!(cache.is_empty());
    }

    /// Pointing this at something that is not a cache must fail loudly rather
    /// than start empty, because silently discarding the player's cache every
    /// launch is a bug that would never be noticed.
    #[test]
    fn a_file_that_is_not_a_cache_is_refused() {
        let directory = std::env::temp_dir().join("owc-quest-cache-junk");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("junk.cache");
        std::fs::write(&path, b"this is not a quest cache at all").unwrap();
        assert!(matches!(
            QuestCache::load(&path),
            Err(CacheError::NotACache)
        ));
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Two realms must not share a file: the whole point of asking the server
    /// is being right about a realm with custom content.
    #[test]
    fn each_realm_gets_its_own_file() {
        let base = Path::new("/cache");
        assert_ne!(
            QuestCache::path_for(base, "AzerothCore"),
            QuestCache::path_for(base, "NekoCore")
        );
        // And a realm name off the wire cannot become a path.
        let nasty = QuestCache::path_for(base, "../../etc");
        assert_eq!(nasty.parent(), Some(base));
    }
}
