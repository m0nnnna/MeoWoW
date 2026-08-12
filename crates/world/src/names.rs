//! What things are called.
//!
//! Names arrive only when asked for, which makes this the first piece of state
//! in the client that has to decide *what to ask and when*. That is a different
//! problem from parsing, and it fails differently: the packets are all fine and
//! the client either floods the server with duplicate queries or sits forever
//! showing `Creature 299`.
//!
//! Three rules do the work:
//!
//! - **Ask once.** A guid that has been asked about is recorded as outstanding
//!   the moment the query goes out, not when the answer arrives. The interface
//!   asks every frame; without this it would send sixty queries a second for
//!   the same wolf.
//! - **A refusal is an answer.** The server replies "no such guid" for anything
//!   it has forgotten. That is cached like any other answer, because asking
//!   again will get the same reply forever.
//! - **Silence is not.** Some queries are simply never answered. Those time out
//!   and become askable again, or a creature that failed to resolve once stays
//!   nameless for the whole session.
//!
//! Creature names are keyed by **entry, not guid** -- every wolf of a kind
//! shares one answer, so a zone full of wolves costs one query rather than
//! forty. Player names are keyed by guid, because they genuinely differ.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::query::{CreatureInfo, PlayerName};

/// How long an unanswered query stays outstanding before it may be asked
/// again.
///
/// Long enough that a slow answer is not asked for twice, short enough that a
/// dropped one resolves within a few seconds of standing still.
pub const RETRY_AFTER: Duration = Duration::from_secs(5);

/// A name we asked for, and what came back.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    /// Asked at this time, nothing back yet.
    Pending(Instant),
    /// The server answered. `None` means it answered "no such thing", which is
    /// as final as a name.
    Known(Option<String>),
}

/// Names for players and creatures, and the bookkeeping to fetch them once.
#[derive(Debug, Default)]
pub struct Names {
    players: HashMap<u64, Entry>,
    creatures: HashMap<u32, Entry>,
    /// Every answer ever folded in, and every query handed out, so a caller
    /// can see whether the two are diverging without inspecting the maps.
    pub stats: NameStats,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NameStats {
    pub queries_issued: usize,
    pub answers: usize,
    /// Answers naming something never asked about. Not an error -- the server
    /// volunteers some -- but a steady stream means a query is being sent from
    /// somewhere that is not counting it.
    pub unsolicited: usize,
    /// Queries that were asked again after timing out.
    pub retried: usize,
}

impl Names {
    pub fn new() -> Self {
        Self::default()
    }

    /// The player's name, if it is known.
    ///
    /// `Some(None)` and `None` are deliberately different: the first is "the
    /// server says there is no such player", the second is "not asked yet".
    /// Collapsing them would either retry a settled question forever or give
    /// up on one never asked.
    pub fn player(&self, guid: u64) -> Option<Option<&str>> {
        match self.players.get(&guid)? {
            Entry::Known(name) => Some(name.as_deref()),
            Entry::Pending(_) => None,
        }
    }

    pub fn creature(&self, entry: u32) -> Option<Option<&str>> {
        match self.creatures.get(&entry)? {
            Entry::Known(name) => Some(name.as_deref()),
            Entry::Pending(_) => None,
        }
    }

    /// Whether a player query should be sent, marking it outstanding if so.
    ///
    /// Deliberately one call rather than a separate "should I?" and "I did":
    /// two calls can be got out of order, and the failure mode -- asking
    /// without recording it -- is a query flood that looks like a server
    /// problem.
    pub fn claim_player(&mut self, guid: u64, now: Instant) -> bool {
        Self::claim(&mut self.players, guid, now, &mut self.stats)
    }

    pub fn claim_creature(&mut self, entry: u32, now: Instant) -> bool {
        Self::claim(&mut self.creatures, entry, now, &mut self.stats)
    }

    fn claim<K: std::hash::Hash + Eq>(
        map: &mut HashMap<K, Entry>,
        key: K,
        now: Instant,
        stats: &mut NameStats,
    ) -> bool {
        match map.get(&key) {
            Some(Entry::Known(_)) => false,
            Some(Entry::Pending(asked)) => {
                if now.duration_since(*asked) < RETRY_AFTER {
                    return false;
                }
                stats.retried += 1;
                stats.queries_issued += 1;
                map.insert(key, Entry::Pending(now));
                true
            }
            None => {
                stats.queries_issued += 1;
                map.insert(key, Entry::Pending(now));
                true
            }
        }
    }

    /// Folds in a `SMSG_NAME_QUERY_RESPONSE`.
    pub fn apply_player(&mut self, answer: &PlayerName) {
        self.stats.answers += 1;
        if !matches!(self.players.get(&answer.guid), Some(Entry::Pending(_))) {
            self.stats.unsolicited += 1;
        }
        self.players
            .insert(answer.guid, Entry::Known(answer.name.clone()));
    }

    /// Folds in a `SMSG_CREATURE_QUERY_RESPONSE`.
    pub fn apply_creature(&mut self, answer: &CreatureInfo) {
        self.stats.answers += 1;
        if !matches!(self.creatures.get(&answer.entry), Some(Entry::Pending(_))) {
            self.stats.unsolicited += 1;
        }
        self.creatures
            .insert(answer.entry, Entry::Known(answer.name.clone()));
    }

    /// How many names are settled, and how many queries are still in flight.
    pub fn counts(&self) -> (usize, usize) {
        let known = self
            .players
            .values()
            .chain(self.creatures.values())
            .filter(|entry| matches!(entry, Entry::Known(_)))
            .count();
        let pending = self.players.len() + self.creatures.len() - known;
        (known, pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(guid: u64, name: Option<&str>) -> PlayerName {
        PlayerName {
            guid,
            name: name.map(str::to_string),
            realm: String::new(),
            race: 1,
            gender: 0,
            class: 1,
        }
    }

    fn creature(entry: u32, name: Option<&str>) -> CreatureInfo {
        CreatureInfo {
            entry,
            name: name.map(str::to_string),
            sub_name: String::new(),
            type_flags: 0,
            creature_type: 0,
            family: 0,
            rank: 0,
        }
    }

    /// The interface asks every frame. Without claiming, that is sixty
    /// identical queries a second for one wolf -- which is not a parse bug and
    /// would never show up as one.
    #[test]
    fn a_name_is_only_asked_for_once() {
        let mut names = Names::new();
        let now = Instant::now();
        assert!(names.claim_player(0x35, now));
        for _ in 0..60 {
            assert!(!names.claim_player(0x35, now), "asked twice");
        }
        assert_eq!(names.stats.queries_issued, 1);
    }

    /// And once answered, never again -- including when the answer is that
    /// there is no such player.
    #[test]
    fn an_answered_name_is_never_asked_again() {
        let mut names = Names::new();
        let now = Instant::now();
        names.claim_player(0x35, now);
        names.apply_player(&player(0x35, Some("Watcher")));
        assert!(!names.claim_player(0x35, now + RETRY_AFTER * 10));

        names.claim_player(0x99, now);
        names.apply_player(&player(0x99, None));
        assert!(
            !names.claim_player(0x99, now + RETRY_AFTER * 10),
            "a refusal is an answer; asking again gets the same refusal forever"
        );
        assert_eq!(names.player(0x99), Some(None));
    }

    /// A query that is never answered has to become askable again, or one
    /// dropped packet leaves something nameless for the whole session.
    #[test]
    fn an_unanswered_query_may_be_retried() {
        let mut names = Names::new();
        let now = Instant::now();
        assert!(names.claim_player(0x35, now));
        assert!(!names.claim_player(0x35, now + RETRY_AFTER / 2));
        assert!(names.claim_player(0x35, now + RETRY_AFTER));
        assert_eq!(names.stats.retried, 1);
    }

    /// "Not asked yet" and "asked, and there is no such thing" are different
    /// answers, and collapsing them breaks one case or the other.
    #[test]
    fn unknown_and_absent_are_distinguishable() {
        let mut names = Names::new();
        assert_eq!(names.player(0x35), None, "never asked");
        names.claim_player(0x35, Instant::now());
        assert_eq!(names.player(0x35), None, "asked, still waiting");
        names.apply_player(&player(0x35, Some("Watcher")));
        assert_eq!(names.player(0x35), Some(Some("Watcher")));
    }

    /// Creatures are keyed by entry, so a zone of forty wolves costs one
    /// query and one answer names them all.
    #[test]
    fn one_creature_query_names_every_instance() {
        let mut names = Names::new();
        let now = Instant::now();
        assert!(names.claim_creature(299, now));
        for _ in 0..40 {
            assert!(!names.claim_creature(299, now));
        }
        names.apply_creature(&creature(299, Some("Young Wolf")));
        assert_eq!(names.creature(299), Some(Some("Young Wolf")));
        assert_eq!(names.stats.queries_issued, 1);
    }

    /// An answer to something never asked is counted rather than ignored: a
    /// steady stream of them means a query is going out from somewhere that
    /// is not recording it, which is how the ask-once rule quietly stops
    /// holding.
    #[test]
    fn unsolicited_answers_are_counted() {
        let mut names = Names::new();
        names.apply_player(&player(0x35, Some("Watcher")));
        assert_eq!(names.stats.unsolicited, 1);
        assert_eq!(names.stats.answers, 1);

        names.claim_creature(299, Instant::now());
        names.apply_creature(&creature(299, Some("Young Wolf")));
        assert_eq!(names.stats.unsolicited, 1, "that one was asked for");
    }

    /// The books have to balance the same way replication's do.
    #[test]
    fn counts_add_up() {
        let mut names = Names::new();
        let now = Instant::now();
        names.claim_player(1, now);
        names.claim_player(2, now);
        names.claim_creature(299, now);
        assert_eq!(names.counts(), (0, 3));

        names.apply_player(&player(1, Some("A")));
        assert_eq!(names.counts(), (1, 2));
        names.apply_creature(&creature(299, None));
        assert_eq!(names.counts(), (2, 1));
    }
}
