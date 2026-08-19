//! What a trainer will teach, and what it costs this character.
//!
//! `SMSG_TRAINER_LIST` answers a `CMSG_TRAINER_LIST`, or arrives when a gossip
//! option meaning "train me" is chosen -- the same two routes
//! [`crate::vendor`] documents, and for the same reason: a trainer with no
//! gossip menu still trains, and a client reopening the window should not have
//! to re-walk a conversation.
//!
//! **The record stride is the whole question here, and the greeting settled
//! it.** The body is a header, then `count` fixed-size records, then a string
//! -- and two strides are plausible enough that neither can be dismissed.
//! The layout this server sends carries two extra `u32`s per record for a
//! primary-profession learn-confirmation dialog, giving **38** bytes; a later
//! one drops them, giving 30. Nothing in the header distinguishes them, and
//! every field in the record is a small integer that reads as a plausible
//! spell id, price or level at either.
//!
//! **This project guessed 30 and the wire said 38**, which is the part worth
//! reading. The guess was not idle: the realm's database carries the *modern*
//! trainer tables -- `trainer`, `trainer_spell`, `creature_default_trainer`,
//! with `ReqAbility1..3` columns and no profession columns at all -- and the
//! obvious inference is that a server storing the new shape sends the new
//! shape. It does not. The schema was modernised and the packet builder was
//! not, so the two disagree inside one server. **A database schema is a fact
//! about storage and not about a wire**, and the only thing that settles a
//! wire is the wire.
//!
//! What settled it is the **greeting**. It sits after the last record, and a
//! stride wrong by even four bytes leaves the reader in the middle of a number
//! rather than at the start of a sentence. Llane Beshere, the Northshire
//! warrior trainer, greets with `Hello, warrior!  Ready for some training?`,
//! and of the four strides scored -- 26, 30, 34, 38 -- exactly one leaves that
//! sentence there and the other three leave binary. That is the same class of
//! evidence as the M2 event identifier being four printable bytes, as
//! `GroundEffectTexture`'s terrain column naming itself through texture
//! filenames, and as `CreatureSoundData`'s columns naming themselves through
//! `SoundEntries` labels: **a name in a binary format cannot be arrived at by
//! a coincidence of small integers.**
//!
//! [`measure_stride`] is the probe that asks, and it reads the bytes itself
//! rather than calling [`parse_trainer_list`], because the parser is what is
//! under test.
//!
//! **Which packets can answer, and which cannot, is arithmetic that has to be
//! done in advance.** A stride that *undershoots* leaves the reader inside a
//! record and is caught immediately. A stride that *overshoots* by `d` bytes
//! per row skips `count * d` bytes -- and if that is less than the greeting's
//! length, it lands inside the greeting and reads a perfectly printable
//! **suffix of it**. Llane's six-row packet cannot separate 38 from 42 for
//! exactly that reason: the overshoot is 24 bytes and the greeting is 41, so
//! 42 "fits" and returns `"or some training?"`. The probe reports both as
//! winners rather than picking one, which is the honest answer.
//!
//! What settles it is a packet where `count * d` exceeds any greeting: Ander
//! Germaine teaches **133** spells with the same 41-character greeting, and
//! 5,112 bytes is `16 + 133 * 38 + 42` exactly. There the overshoot is 532
//! bytes, 42 runs off the end, and 38 stands alone. Same lesson as the M2
//! particle stride, where 1,739 single-emitter models scored every candidate
//! identically: **work out which samples are incapable of separating the
//! candidates before reading the result**, and if the first sample is one of
//! them, go and find another.
//!
//! **The cost is not the table's cost.** The server multiplies by the reader's
//! reputation discount and truncates before sending, exactly as it does for a
//! vendor price. Llane's six spells cost 10 and 100 in `trainer_spell` and
//! arrive as **9 and 95** -- `floor(cost * 0.95)`, the same arithmetic and the
//! same 0.95 the vendor list showed. The wire is authoritative; the table is
//! not. This is the second place that rule has been needed and it cost nothing
//! the second time, which is the point of having written it down.
//!
//! **The list is filtered per character**, by class, by race and by a
//! prerequisite spell the reader may not have, so two characters at the same
//! NPC see different lists. That would normally raise the row-position trap
//! that loot slots, gossip option indices and vendor slots all sit in --
//! except that a purchase names the **spell id**, which is the one handle that
//! means the same thing to everybody. There is nothing here to get wrong.

use crate::protocol::{Error, Reader};

/// Bytes in one spell record. **Measured against the greeting, not
/// transcribed** -- see the module comment and [`measure_stride`], and note
/// that the obvious inference from the server's own database predicted the
/// other number.
pub const SPELL_BYTES: usize = 38;

/// The stride of the later layout, kept because it is what the probe scores
/// against and because it is what this project first assumed. Two fewer `u32`s
/// per record: a server that had dropped the profession dialog would send
/// this, and this realm does not.
pub const SHORT_SPELL_BYTES: usize = 30;

/// Bytes before the first record: the trainer's guid and two `u32`s.
const HEADER_BYTES: usize = 8 + 4 + 4;

/// How many prerequisite spells a record names. Always three slots, padded
/// with zeroes -- the server writes exactly three whatever it has.
const REQUIRED_SPELLS: usize = 3;

/// Whether this character can learn a given spell right now, and if not, why
/// the original client would grey it out.
///
/// **The values were measured, not transcribed**, because this is precisely
/// the kind of small dense enum where a wrong name never errors -- it draws a
/// confident, plausible, wrong colour, which is the failure
/// `describe_cast_failure` exists to refuse.
///
/// **Each value was produced deliberately, one change at a time**, the same
/// method that named `SMSG_QUESTGIVER_STATUS`'s byte -- and each sample rules
/// out the readings the other cannot.
///
/// * `0` and `1` came from a **level-five** warrior at Llane Beshere, whose
///   six spells want levels 1, 4, 4, 6, 6 and 6. Such a reader must split
///   three-and-three exactly along that line, and does: the three at level 6
///   carry `1`, the three at or below 5 carry `0`, no exceptions and no
///   near-misses.
/// * `2` needed a sample where the level cannot be the explanation, because at
///   level five "already known" and "too low" are both live for different
///   rows. A **level twenty-six** warrior at the same NPC reads `0` on all six
///   -- nothing there can be out of reach -- and then reads `2` on exactly
///   the one spell a `.learn 6673` had granted between the two runs, with the
///   other five unmoved.
///
/// Each of those is a prediction that could have come out the other way, which
/// is what makes them evidence rather than a plausible reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainerSpellState {
    /// Learnable now, if the money is there. Drawn green. **Observed**: the
    /// three spells Llane offers at or below the reader's level.
    Available,
    /// Not yet -- the level, the skill rank or a prerequisite spell is
    /// missing. Drawn red. **Observed**: the three spells Llane offers at
    /// level 6 to a level-5 reader.
    ///
    /// Named for what the server checks rather than for the level alone: the
    /// same value covers an unmet skill rank and a missing prerequisite spell,
    /// and a client reporting "come back at level 6" to somebody short of a
    /// skill would be confidently wrong. The record carries
    /// [`TrainerSpell::required_level`], [`TrainerSpell::required_skill`] and
    /// [`TrainerSpell::required_spells`] beside it, so which one is the
    /// obstacle is answerable without guessing from this byte.
    Unavailable,
    /// Already known. Drawn grey. **Observed** on exactly the one spell a
    /// level-twenty-six reader had just been granted, where every other row of
    /// the same packet stayed `0`.
    Known,
    /// Anything else. Kept rather than defaulted, and logged rather than
    /// drawn, for the reason every unknown code in this crate is: a value
    /// nobody has produced deliberately has no name that can be trusted.
    Unknown(u8),
}

impl TrainerSpellState {
    /// Reads the state byte.
    ///
    /// **`1` is "cannot" and `2` is "already known", which is the way round
    /// that surprises.** The natural guess orders them by how far along the
    /// character is -- learnable, learned, out of reach -- and the server does
    /// not. Reading them that way would grey out everything learnable and
    /// offer everything already learned, with no error anywhere.
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::Available,
            1 => Self::Unavailable,
            2 => Self::Known,
            other => Self::Unknown(other),
        }
    }

    /// Whether a purchase is worth sending. Only [`Self::Available`] is --
    /// the server declines the rest **in silence**, which is indistinguishable
    /// from a malformed request, so the client refuses locally rather than
    /// spending an unanswerable send to find out.
    pub fn is_learnable(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// One thing a trainer will teach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainerSpell {
    /// The spell to ask for, and the **only** handle a purchase uses. Not a
    /// row position: see the module comment on why the usual index trap does
    /// not apply here.
    pub spell: u32,
    /// Whether this character can learn it now.
    pub state: TrainerSpellState,
    /// What it costs in copper, **after the reader's reputation discount**.
    /// Not `trainer_spell.MoneyCost`; see the module comment.
    pub cost: u32,
    /// The level this character has to reach. A `u8` on the wire, sitting
    /// between two `u32`s -- which is what makes the stride awkward to guess
    /// and the greeting worth measuring against.
    pub required_level: u8,
    /// A row in `SkillLine` the character must have, or `None` for the
    /// overwhelming majority that want no skill at all.
    pub required_skill: Option<u32>,
    /// How much of [`TrainerSpell::required_skill`] is wanted. Meaningless
    /// without it, and left as sent rather than folded into the `Option` so
    /// that a zero rank on a real skill stays distinguishable from no skill.
    pub required_skill_value: u32,
    /// Up to three spells that must already be known -- the previous rank, or
    /// a talent that unlocks this one. Zeroes are dropped, so an empty list
    /// means no prerequisite rather than three unknown ones.
    pub required_spells: Vec<u32>,
    /// The two fields that make this record 38 bytes instead of 30, and the
    /// reason the stride needed measuring at all.
    ///
    /// **Deliberately one opaque pair rather than two named flags.** The
    /// server's source calls them the primary-profession learn-confirmation
    /// dialog and its enabled state, and that is a hypothesis rather than an
    /// observation: both are `0` on every record captured, because a warrior
    /// trainer teaches no professions. Naming a field from source alone is
    /// what rule 2 permits reading source *for* and not what it permits
    /// concluding. A profession trainer produces the first non-zero one and
    /// names them then.
    pub profession: (u32, u32),
}

/// Everything a trainer offers, and what it says while offering it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainerList {
    /// Who is teaching. Sent unpacked.
    pub trainer: u64,
    /// What kind of trainer this is -- class, mount, profession, pet.
    ///
    /// **Deliberately left as a number.** It is `0` on every trainer captured
    /// so far, so naming the other values would be transcribing an enum
    /// nothing here has produced, which is the mistake this project keeps a
    /// rule about. The first non-zero one names itself.
    pub kind: u32,
    pub spells: Vec<TrainerSpell>,
    /// What the trainer says. **The field that confirmed the record stride**
    /// -- see the module comment.
    pub greeting: String,
}

impl TrainerList {
    /// Whether this trainer has nothing to teach *this* character.
    ///
    /// A real state and not an error: the list is filtered per character, so a
    /// warrior at a mage trainer gets a well-formed packet with no rows in it.
    pub fn is_empty(&self) -> bool {
        self.spells.is_empty()
    }

    /// The rows a purchase may actually be sent for.
    pub fn learnable(&self) -> impl Iterator<Item = &TrainerSpell> {
        self.spells.iter().filter(|s| s.state.is_learnable())
    }
}

/// Parses `SMSG_TRAINER_LIST`.
///
/// Read through a cursor that must end exactly at the end of the body. That is
/// load-bearing rather than tidy here, and it earned its keep on the first
/// live capture: the records are fixed-size and followed by a string, so a
/// stride wrong by a word does not corrupt one field -- it walks the string
/// pointer into the middle of a record. The first run against this realm
/// reported `89 trailing bytes left unread` instead of returning a
/// confident-looking list with a rubbish greeting, which is the whole
/// difference between a measurement and a bug.
pub fn parse_trainer_list(body: &[u8]) -> Result<TrainerList, Error> {
    let mut r = Reader::new(body, "SMSG_TRAINER_LIST");

    let trainer = r.u64()?;
    let kind = r.u32()?;
    let count = r.u32()?;

    // **Check the count against the body before trusting it.** A `u32` row
    // count read from the wrong offset is an enormous number, and allocating
    // for it before discovering the mistake is the difference between an error
    // message and a dead process. The trailing string means this can only be a
    // lower bound -- hence `<` rather than `!=`, unlike the vendor list, whose
    // body ends with its last row.
    let need = count as usize * SPELL_BYTES;
    if r.remaining() < need {
        return Err(Error::TrainerRowCount {
            count,
            expected: need,
            got: r.remaining(),
        });
    }

    let mut spells = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let spell = r.u32()?;
        let state = TrainerSpellState::from_byte(r.u8()?);
        let cost = r.u32()?;
        let profession = (r.u32()?, r.u32()?);
        let required_level = r.u8()?;
        let required_skill = match r.u32()? {
            0 => None,
            skill => Some(skill),
        };
        let required_skill_value = r.u32()?;

        // Three slots, zero-padded. Dropping the zeroes here rather than in
        // the consumer keeps "no prerequisite" from being three separate
        // things a caller has to know to ignore.
        let mut required_spells = Vec::new();
        for _ in 0..REQUIRED_SPELLS {
            match r.u32()? {
                0 => {}
                required => required_spells.push(required),
            }
        }

        spells.push(TrainerSpell {
            spell,
            state,
            cost,
            required_level,
            required_skill,
            required_skill_value,
            required_spells,
            profession,
        });
    }

    let greeting = r.cstring()?;
    r.finish()?;

    Ok(TrainerList {
        trainer,
        kind,
        spells,
        greeting,
    })
}

/// How well one candidate record stride explains a captured body.
///
/// Produced by [`measure_stride`]. The informative field is
/// [`StrideFit::greeting_is_printable`]: a wrong stride still consumes a
/// plausible number of bytes and still finds *some* NUL to stop at, so "it
/// parsed" is nearly free. What is not free is what it leaves being a readable
/// sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrideFit {
    pub stride: usize,
    /// Whether the header, `count` records and one NUL-terminated string
    /// account for every byte of the body with none left over.
    pub accounts_for_body: bool,
    /// What lands where the greeting should be, if anything did.
    pub greeting: Option<String>,
    /// Whether that is entirely printable ASCII -- the discriminator. A stride
    /// out by a word puts this reader inside a spell id or a price, and the
    /// bytes of a small integer are overwhelmingly not printable.
    pub greeting_is_printable: bool,
}

/// Scores candidate record strides against a captured `SMSG_TRAINER_LIST`.
///
/// **Reads the bytes itself rather than calling [`parse_trainer_list`]**, for
/// the same reason `wow-cli m2 events --strides` does: the parser is what is
/// under test, so a probe built on it would only ever confirm its own
/// assumption. That mattered here -- the parser was written for the wrong
/// stride and this function is what said so.
///
/// Byte accounting alone is weaker than it looks. The body ends with a
/// variable-length string, so any stride leaving a NUL after the records
/// "accounts for the body" with no leftover to notice. It is the printability
/// of what it leaves that separates the candidates.
pub fn measure_stride(body: &[u8], candidates: &[usize]) -> Vec<StrideFit> {
    candidates
        .iter()
        .map(|&stride| {
            let mut fit = StrideFit {
                stride,
                accounts_for_body: false,
                greeting: None,
                greeting_is_printable: false,
            };

            if body.len() < HEADER_BYTES {
                return fit;
            }
            let count = u32::from_le_bytes([body[12], body[13], body[14], body[15]]) as usize;
            let Some(start) = count.checked_mul(stride).map(|n| n + HEADER_BYTES) else {
                return fit;
            };
            if start > body.len() {
                return fit;
            }

            let tail = &body[start..];
            let Some(nul) = tail.iter().position(|&b| b == 0) else {
                return fit;
            };
            // The string has to be the *last* thing in the body, so a NUL
            // anywhere but the final byte means this stride left the reader
            // somewhere it should not be.
            fit.accounts_for_body = nul + 1 == tail.len();

            let text = &tail[..nul];
            fit.greeting_is_printable =
                !text.is_empty() && text.iter().all(|&b| (0x20..0x7f).contains(&b));
            fit.greeting = Some(String::from_utf8_lossy(text).into_owned());
            fit
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Llane Beshere's list **exactly as the local realm sent it** to
    /// `Testwolf`, a level-five human warrior: 286 bytes, guid and all.
    ///
    /// Captured rather than constructed, and that distinction is the reason
    /// this fixture exists. The first version of this test was a body built
    /// from what this module *predicted* the server would send -- six rows at
    /// thirty bytes each -- and it passed, because a parser checked against
    /// its own author's assumption always does. The real packet is eight bytes
    /// per row longer.
    ///
    /// Six rows rather than one, deliberately: one row cannot distinguish a
    /// record stride from a header size, and cannot show the state byte
    /// varying at all.
    const LLANE: [u8; 286] = [
        0xa3, 0x9c, 0x00, 0x8f, 0x03, 0x00, 0x30, 0xf1, // trainer guid
        0x00, 0x00, 0x00, 0x00, // kind 0
        0x06, 0x00, 0x00, 0x00, // six spells
        // 100 Charge -- available, 95 copper, level 4
        0x64, 0x00, 0x00, 0x00, 0x00, 0x5f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // 772 Rend -- available, 95 copper, level 4
        0x04, 0x03, 0x00, 0x00, 0x00, 0x5f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // 3127 Parry -- level 6, and this reader is 5
        0x37, 0x0c, 0x00, 0x00, 0x01, 0x5f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // 6343 Thunder Clap -- level 6
        0xc7, 0x18, 0x00, 0x00, 0x01, 0x5f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // 6673 Battle Shout -- level 1, and the one row priced at 9
        0x11, 0x1a, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // 34428 Victory Rush -- level 6
        0x7c, 0x86, 0x00, 0x00, 0x01, 0x5f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // "Hello, warrior!  Ready for some training?"
        0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x77, 0x61, 0x72, 0x72, 0x69, 0x6f, 0x72, 0x21,
        0x20, 0x20, 0x52, 0x65, 0x61, 0x64, 0x79, 0x20, 0x66, 0x6f, 0x72, 0x20, 0x73, 0x6f, 0x6d,
        0x65, 0x20, 0x74, 0x72, 0x61, 0x69, 0x6e, 0x69, 0x6e, 0x67, 0x3f, 0x00,
    ];

    /// The six spells the server's own `trainer_spell` table lists for trainer
    /// id 2, with the cost the *table* gives -- which is deliberately not the
    /// cost on the wire. Ordered as the packet sends them.
    ///
    /// This is the cross-check that makes the parse evidence rather than
    /// self-agreement: the client is never sent this table, so a record read
    /// at the wrong offset could not reproduce it.
    const DATABASE: [(u32, u32, u8); 6] = [
        // spell, trainer_spell.MoneyCost, trainer_spell.ReqLevel
        (100, 100, 4),
        (772, 100, 4),
        (3127, 100, 6),
        (6343, 100, 6),
        (6673, 10, 1),
        (34428, 100, 6),
    ];

    /// The level of the character the capture was taken on. Every state
    /// assertion below is relative to this and meaningless without it.
    const READER_LEVEL: u8 = 5;

    #[test]
    fn parses_a_captured_trainer_list() {
        let list = parse_trainer_list(&LLANE).expect("Llane's list parses");
        assert_eq!(list.trainer, 0xf130_0003_8f00_9ca3);
        assert_eq!(list.kind, 0);
        assert_eq!(list.spells.len(), 6);
        assert_eq!(list.greeting, "Hello, warrior!  Ready for some training?");
    }

    /// Spell ids and required levels against the server's own table, which the
    /// client is never sent. Agreement across six rows at six different
    /// offsets is what says the record layout is right -- one row agreeing
    /// would say almost nothing.
    #[test]
    fn every_row_agrees_with_the_servers_table() {
        let list = parse_trainer_list(&LLANE).unwrap();
        for (parsed, &(spell, _, level)) in list.spells.iter().zip(DATABASE.iter()) {
            assert_eq!(parsed.spell, spell);
            assert_eq!(parsed.required_level, level, "spell {spell}");
        }
    }

    /// The check that makes the state byte a measurement rather than a
    /// transcription: at level five, every spell wanting level six is refused
    /// and every spell below it is offered. Three and three, no exceptions.
    /// A reading with the two values swapped fails both halves at once.
    #[test]
    fn state_splits_exactly_on_the_readers_level() {
        let list = parse_trainer_list(&LLANE).unwrap();
        for spell in &list.spells {
            let expected = if spell.required_level > READER_LEVEL {
                TrainerSpellState::Unavailable
            } else {
                TrainerSpellState::Available
            };
            assert_eq!(
                spell.state, expected,
                "spell {} wants level {} and the reader is {READER_LEVEL}",
                spell.spell, spell.required_level
            );
        }
        assert_eq!(list.learnable().count(), 3, "three of six, split on level");
    }

    /// The price is the discounted one. Stated as the *relationship* rather
    /// than as two magic numbers, because that is what generalises: every row
    /// is `floor(table * 0.95)`, across a value of 10 and a value of 100,
    /// which is the same arithmetic and the same factor the vendor list
    /// showed. A client displaying `trainer_spell.MoneyCost` would quote the
    /// wrong price to everyone not at neutral standing, and nothing about the
    /// result would look wrong.
    #[test]
    fn cost_is_the_discounted_one_not_the_tables() {
        let list = parse_trainer_list(&LLANE).unwrap();
        for (parsed, &(spell, table_cost, _)) in list.spells.iter().zip(DATABASE.iter()) {
            let discounted = (f64::from(table_cost) * 0.95).floor() as u32;
            assert_eq!(
                parsed.cost, discounted,
                "spell {spell}: table says {table_cost}, wire should say {discounted}"
            );
            assert!(parsed.cost < table_cost, "spell {spell} is not discounted");
        }
    }

    /// The measurement the module exists for, run against the real body. Of
    /// four candidate strides exactly one leaves a sentence where the greeting
    /// belongs, and it is not the one this parser was first written for.
    #[test]
    fn the_greeting_separates_the_strides() {
        // **42 is deliberately absent from this list**, and the reason is the
        // finding rather than an oversight: six rows overshooting by four
        // bytes each skips 24 bytes into a 41-character greeting and returns
        // a printable suffix of it, so this packet genuinely cannot tell 38
        // from 42. [`an_overshoot_shorter_than_the_greeting_is_not_separable`]
        // asserts that it cannot, so nobody later reads this list as a claim
        // that it could.
        let candidates = [26, SHORT_SPELL_BYTES, 34, SPELL_BYTES];
        let fits = measure_stride(&LLANE, &candidates);

        let winners: Vec<usize> = fits
            .iter()
            .filter(|f| f.accounts_for_body && f.greeting_is_printable)
            .map(|f| f.stride)
            .collect();
        assert_eq!(
            winners,
            vec![SPELL_BYTES],
            "exactly one stride should leave a readable greeting"
        );

        let winner = fits.iter().find(|f| f.stride == SPELL_BYTES).unwrap();
        assert_eq!(
            winner.greeting.as_deref(),
            Some("Hello, warrior!  Ready for some training?")
        );

        // The half that matters is the *failures*: the three losing strides
        // must leave binary rather than merely leaving something shorter. A
        // probe that only checked the winner would score any stride that
        // happened to land on a NUL.
        for fit in fits.iter().filter(|f| f.stride != SPELL_BYTES) {
            assert!(
                !fit.greeting_is_printable,
                "stride {} left readable text, so this probe does not discriminate",
                fit.stride
            );
        }
    }

    /// Both running out of input and having input left over are errors -- the
    /// rule four separate world-protocol bugs were invisible without, and the
    /// rule that caught this module's own wrong stride on its first live run.
    #[test]
    fn trailing_bytes_are_an_error() {
        let mut body = LLANE.to_vec();
        body.push(0);
        assert!(parse_trainer_list(&body).is_err());
    }

    /// Reading this body at the stride this module first assumed leaves the
    /// greeting pointer inside a record, which is exactly what the live run
    /// reported. Kept as a test so the wrong stride stays refuted rather than
    /// merely unused.
    #[test]
    fn the_short_stride_does_not_parse_the_real_body() {
        let short = HEADER_BYTES + 6 * SHORT_SPELL_BYTES;
        assert!(short < LLANE.len());
        let fit = &measure_stride(&LLANE, &[SHORT_SPELL_BYTES])[0];
        assert!(!fit.accounts_for_body);
        assert!(!fit.greeting_is_printable);
    }

    /// **The probe's own blind spot, asserted rather than left to be
    /// rediscovered.** An overshooting stride skips `count * delta` bytes; when
    /// that is shorter than the greeting it lands *inside* the greeting and
    /// returns a printable suffix, which passes both of this probe's checks.
    /// Six rows and a four-byte overshoot is 24 bytes into a 41-character
    /// sentence, so 42 looks exactly as good as 38 here.
    ///
    /// The fix is not a cleverer check, it is a bigger packet: Ander Germaine
    /// teaches 133 spells with the same greeting, and 5,112 bytes is
    /// `16 + 133 * 38 + 42` to the byte -- there the overshoot is 532 bytes,
    /// 42 runs past the end, and 38 stands alone. Live, all four candidates
    /// were scored against that body and exactly one survived.
    #[test]
    fn an_overshoot_shorter_than_the_greeting_is_not_separable() {
        let fits = measure_stride(&LLANE, &[SPELL_BYTES, SPELL_BYTES + 4]);
        assert!(fits.iter().all(|f| f.accounts_for_body && f.greeting_is_printable));
        assert_eq!(fits[1].greeting.as_deref(), Some("or some training?"));

        // And it *is* an overshoot into the greeting rather than a second real
        // reading: what the wrong stride returns is a suffix of what the right
        // one does.
        let right = fits[0].greeting.clone().unwrap();
        let wrong = fits[1].greeting.clone().unwrap();
        assert!(right.ends_with(&wrong) && right != wrong);
    }

    /// A row count read from the wrong offset is enormous, and the check has
    /// to happen before the allocation rather than after it.
    #[test]
    fn an_impossible_row_count_is_refused_not_allocated() {
        let mut body = LLANE.to_vec();
        body[12..16].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert!(matches!(
            parse_trainer_list(&body),
            Err(Error::TrainerRowCount { .. })
        ));
    }

    /// A trainer with nothing for this character still answers, and that is a
    /// state rather than a failure -- the list is filtered per character, so a
    /// warrior at a mage trainer gets a well-formed empty one.
    #[test]
    fn an_empty_list_is_not_an_error() {
        let mut body = Vec::new();
        body.extend_from_slice(&7u64.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(b"Nothing for you.\0");
        let list = parse_trainer_list(&body).unwrap();
        assert!(list.is_empty());
        assert_eq!(list.greeting, "Nothing for you.");
    }

    /// Only [`TrainerSpellState::Available`] earns a send. The server declines
    /// the rest in silence, which is indistinguishable from a wrong opcode, so
    /// the refusal has to happen here where the reason is still known.
    #[test]
    fn only_available_spells_are_worth_asking_for() {
        assert!(TrainerSpellState::Available.is_learnable());
        assert!(!TrainerSpellState::Unavailable.is_learnable());
        assert!(!TrainerSpellState::Known.is_learnable());
        assert!(!TrainerSpellState::Unknown(7).is_learnable());
    }
}
