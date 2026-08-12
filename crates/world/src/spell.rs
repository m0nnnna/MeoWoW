//! Spells: what this character knows, and asking to cast one.
//!
//! `SMSG_INITIAL_SPELLS` arrives unprompted during the login burst and is the
//! only place the spellbook comes from -- there is no query for it. Miss it and
//! the character appears to know nothing, which looks like a missing feature
//! rather than a dropped packet.
//!
//! Two counted lists back to back, and that is the whole risk here: a wrong
//! width in the first list does not fail, it just consumes the wrong number of
//! bytes and reads the cooldown list from the middle of the spell list. The
//! cursor catches it at the end, which is the only place it can be caught.

use crate::protocol::{Error, Reader};
use crate::update::write_packed_guid;

/// One spell the character knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownSpell {
    pub id: u32,
    /// Zero in every observation so far. Read rather than skipped so its width
    /// is asserted along with everything else.
    pub slot: u16,
}

/// A spell or category that is not ready yet.
///
/// **The width here is confirmed against a live realm; the second field's
/// meaning is not.** The obvious shape to transcribe from public 3.3.5a
/// documentation is `item_id: u16, category: u16, cooldown_ms: u32,
/// category_cooldown_ms: u32` -- 16 bytes -- and that is what this struct
/// held until a login actually carried a real cooldown. It never had before:
/// every prior observation was an empty list, which exercises none of a
/// list's own entry width. When one arrived (a level-one warrior who had just
/// cast spell `59752`, "Every Man for Himself"), the packet held a count of
/// `4` entries in exactly `32` remaining bytes -- divisible evenly only at
/// `8` bytes each, not `16` -- and the first entry's first word decoded to
/// `59752` itself, confirming the split. What the second word *is* stays
/// open: it read `0` for that same active cooldown, which rules out "whole
/// milliseconds remaining" read verbatim, and this project's rule against
/// transcribing an unverified table applies as much to a field's meaning as
/// to a status code's name -- so it keeps a neutral name rather than a
/// guessed one until a clearer observation names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellCooldown {
    pub spell_id: u32,
    /// The cooldown entry's second word. Not yet confirmed to be a duration.
    pub second: u32,
}

/// The spellbook, as it arrives at login.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitialSpells {
    pub spells: Vec<KnownSpell>,
    pub cooldowns: Vec<SpellCooldown>,
}

/// Parses `SMSG_INITIAL_SPELLS`.
pub fn parse_initial_spells(body: &[u8]) -> Result<InitialSpells, Error> {
    let mut r = Reader::new(body, "SMSG_INITIAL_SPELLS");
    // Always zero. Not a count -- reading it as one truncates the spellbook to
    // nothing and looks exactly like a character that knows no spells.
    let _unknown = r.u8()?;

    let spell_count = r.u16()?;
    let mut spells = Vec::with_capacity(spell_count as usize);
    for _ in 0..spell_count {
        spells.push(KnownSpell {
            id: r.u32()?,
            slot: r.u16()?,
        });
    }

    let cooldown_count = r.u16()?;
    let mut cooldowns = Vec::with_capacity(cooldown_count as usize);
    for _ in 0..cooldown_count {
        cooldowns.push(SpellCooldown {
            spell_id: r.u32()?,
            second: r.u32()?,
        });
    }

    r.finish()?;
    Ok(InitialSpells { spells, cooldowns })
}

/// Which of a spell's possible targets the client is supplying.
///
/// Only the two this client can currently mean. The mask has a dozen more
/// bits, each adding its own field to the packet, so naming only what is sent
/// keeps the writer honest about what it can actually express.
pub mod target_flags {
    /// No target: the spell acts on the caster.
    pub const SELF: u32 = 0x0000_0000;
    /// A unit, whose guid follows packed.
    pub const UNIT: u32 = 0x0000_0002;
}

/// `CMSG_CAST_SPELL`'s body.
///
/// `target` is `None` for a spell on yourself. A wrong target mask here is the
/// dangerous kind of mistake: the server reads the following bytes according
/// to the mask it was given, so claiming a unit target and not supplying a
/// guid does not fail -- it reads the next fields as one.
pub fn cast_spell(spell_id: u32, target: Option<u64>) -> Vec<u8> {
    let mut body = Vec::new();
    // Distinguishes several casts of the same spell in flight at once. Zero is
    // correct for a client that casts one at a time.
    body.push(0u8);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.push(0u8); // cast flags

    match target {
        Some(guid) => {
            body.extend_from_slice(&target_flags::UNIT.to_le_bytes());
            write_packed_guid(guid, &mut body);
        }
        None => body.extend_from_slice(&target_flags::SELF.to_le_bytes()),
    }
    body
}

/// One spell now on cooldown, from `SMSG_SPELL_COOLDOWN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellCooldownEvent {
    pub spell_id: u32,
    pub cooldown_ms: u32,
}

/// Parses `SMSG_SPELL_COOLDOWN`.
///
/// Sent once per cast that actually starts a cooldown, unlike
/// `SMSG_INITIAL_SPELLS`'s login-time cooldown list. This one is a raw,
/// unpacked guid, a flags byte the client has no use for yet, and then
/// `(spellId: u32, cooldown: u32)` pairs with no count: whatever bytes remain
/// after the header are read eight at a time. That is still a cursor
/// invariant this parser can assert -- an odd number of trailing bytes fails
/// the same way a wrong count would elsewhere. The per-entry width matches
/// what `SpellCooldown` turned out to actually be on the wire (eight bytes,
/// not the sixteen public documentation suggested), which is some evidence
/// for this shape without being a direct observation of it.
///
/// **The opcode itself has not been observed.** Two live casts of spell
/// `59752` ("Every Man for Himself") against `wow1.nekos.farm`, one held open
/// six seconds afterward, both started a real cooldown -- confirmed by the
/// next login's `SMSG_INITIAL_SPELLS` carrying it, and by a recast being
/// refused -- but neither capture contained opcode `0x0134` at all, only
/// `SMSG_SPELL_START`/`SMSG_SPELL_GO` and an unrecognised `0x0496` twice.
/// Either this realm does not send `SMSG_SPELL_COOLDOWN` for this ability, or
/// the opcode number itself is wrong. This parser and its fold into
/// `WorldState` are therefore exercised only by unit tests today; the next
/// person chasing a cooldown sweep that never appears should start here, not
/// assume the sweep math is at fault.
pub fn parse_spell_cooldown(body: &[u8]) -> Result<(u64, Vec<SpellCooldownEvent>), Error> {
    let mut r = Reader::new(body, "SMSG_SPELL_COOLDOWN");
    let caster = r.u64()?;
    let _flags = r.u8()?;

    let mut cooldowns = Vec::new();
    while r.remaining() > 0 {
        cooldowns.push(SpellCooldownEvent {
            spell_id: r.u32()?,
            cooldown_ms: r.u32()?,
        });
    }
    r.finish()?;
    Ok((caster, cooldowns))
}

/// The header shared by `SMSG_SPELL_START` and `SMSG_SPELL_GO`: two packed
/// guids, a cast count, and the spell id -- identical byte-for-byte in a
/// live capture of the two packets from the same cast (see both parsers'
/// pinned regression tests). Reading it once keeps the two parsers from
/// silently drifting apart the way two independently-written copies of a
/// shared prefix tend to.
fn read_cast_header(r: &mut Reader) -> Result<(u64, u64, u8, u32), Error> {
    let cast_item = crate::update::read_packed_guid(r)?;
    let caster = crate::update::read_packed_guid(r)?;
    let cast_count = r.u8()?;
    let spell_id = r.u32()?;
    Ok((cast_item, caster, cast_count, spell_id))
}

/// The target-flags-then-guid-then-trailing-u32 tail shared by both packets,
/// past whatever each puts before it. Refuses anything other than
/// `target_flags::UNIT` -- see [`SpellStart`]'s doc comment for why.
fn read_unit_target_tail(r: &mut Reader, what: &'static str) -> Result<(u64, u32), Error> {
    let target_flags = r.u32()?;
    if target_flags != target_flags::UNIT {
        return Err(Error::UnsupportedSpellTarget {
            what,
            flags: target_flags,
        });
    }
    let target = crate::update::read_packed_guid(r)?;
    let trailing = r.u32()?;
    Ok((target, trailing))
}

/// One cast beginning to wind up, from `SMSG_SPELL_START`.
///
/// **Confirmed against exactly one live packet, and refuses to guess past
/// what that packet actually showed.** `wow-cli world --cast 5185
/// --cast-self` against `wow1.nekos.farm` (Healing Touch, cast by
/// `Testdruid`, guid `0x33`) produced a 27-byte body this decodes with
/// nothing left over -- see `a_spell_start_matches_a_captured_live_packet`.
/// The spell id reads `5185` and the cast time reads `1500` ms, both exactly
/// where they should be for that cast.
///
/// **Even `--cast-self` did not send `target_flags::SELF`.** The capture
/// carries an explicit `target_flags::UNIT` naming the caster's own guid as
/// the target. No capture of an actual `SELF` target, an item target, or a
/// positional target (ground-targeted AOE spells use those) exists, so this
/// parser errors on any target flags other than `UNIT` rather than reading
/// whatever would follow a shape nobody has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellStart {
    /// The first of the header's two packed guids. Equal to `caster` in the
    /// one capture this is confirmed against -- a spell not cast from an
    /// item -- so which of the two is really an item guid is not
    /// independently distinguished yet.
    pub cast_item: u64,
    pub caster: u64,
    pub cast_count: u8,
    pub spell_id: u32,
    /// Opaque. `0x0802` in the one capture on hand; no bit has been isolated.
    pub cast_flags: u32,
    pub cast_time_ms: u32,
    /// Who the cast is aimed at -- see the struct's own doc comment for why
    /// this is unconditionally a unit rather than an `Option`.
    pub target: u64,
    /// A u32 that followed the target in the one capture on hand: `100`,
    /// suspiciously close to a mana cost, but not confirmed to be one, and
    /// possibly gated by a `cast_flags` bit rather than always present.
    pub trailing: u32,
}

/// Parses `SMSG_SPELL_START`. See [`SpellStart`] for what is and is not
/// confirmed about this shape.
pub fn parse_spell_start(body: &[u8]) -> Result<SpellStart, Error> {
    let mut r = Reader::new(body, "SMSG_SPELL_START");
    let (cast_item, caster, cast_count, spell_id) = read_cast_header(&mut r)?;
    let cast_flags = r.u32()?;
    let cast_time_ms = r.u32()?;
    let (target, trailing) = read_unit_target_tail(&mut r, "SMSG_SPELL_START")?;
    r.finish()?;
    Ok(SpellStart {
        cast_item,
        caster,
        cast_count,
        spell_id,
        cast_flags,
        cast_time_ms,
        target,
        trailing,
    })
}

/// One cast landing, from `SMSG_SPELL_GO`.
///
/// Confirmed against the `SMSG_SPELL_GO` that arrived alongside the
/// `SMSG_SPELL_START` capture documented on [`SpellStart`] -- same cast, same
/// wire dump. The header is byte-for-byte identical to that packet's, which
/// is why [`parse_spell_go`] shares [`read_cast_header`] rather than
/// re-deriving it. Past the header this packet carries no cast time; instead
/// it carries a hit list, a miss list, and a server timestamp, then the same
/// target tail as `SMSG_SPELL_START`, refused on the same terms for anything
/// but `target_flags::UNIT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpellGo {
    pub cast_item: u64,
    pub caster: u64,
    pub cast_count: u8,
    pub spell_id: u32,
    /// Opaque, and not the same bit pattern as `SpellStart::cast_flags` in
    /// the one pair of captures on hand (`0x0900` here against `0x0802`
    /// there).
    pub cast_flags: u32,
    /// The server's own millisecond clock, not wall time -- a monotonic
    /// counter with no shared origin with anything this client keeps.
    pub timestamp: u32,
    /// Guids the cast actually hit. One entry in the capture -- the caster's
    /// own guid, for a self-heal -- each a raw unpacked guid rather than the
    /// packed form used everywhere else in this packet, which the capture
    /// shows clearly: eight bytes reading `33 00 00 00 00 00 00 00`, not a
    /// packed mask.
    pub hits: Vec<u64>,
    /// Who the cast is aimed at, and the same trailing u32 as `SpellStart`.
    /// See [`SpellStart`]'s doc comment for why this is unconditionally a
    /// unit target.
    pub target: u64,
    pub trailing: u32,
}

/// Parses `SMSG_SPELL_GO`. See [`SpellGo`] for what is and is not confirmed
/// about this shape.
///
/// **Refuses any miss entries.** The capture this is confirmed against had a
/// miss count of zero, so a miss entry's own shape -- believed elsewhere to
/// carry at least a reason byte -- has never actually been seen on the wire.
/// Reading a nonzero miss count as if it were more raw guids would be
/// exactly the kind of confident, unverified layout this project's own rules
/// warn against, so a nonzero count is an error instead.
pub fn parse_spell_go(body: &[u8]) -> Result<SpellGo, Error> {
    let mut r = Reader::new(body, "SMSG_SPELL_GO");
    let (cast_item, caster, cast_count, spell_id) = read_cast_header(&mut r)?;
    let cast_flags = r.u32()?;
    let timestamp = r.u32()?;

    let hit_count = r.u8()?;
    let mut hits = Vec::with_capacity(hit_count as usize);
    for _ in 0..hit_count {
        hits.push(r.u64()?);
    }

    let miss_count = r.u8()?;
    if miss_count != 0 {
        return Err(Error::UnconfirmedSpellMisses { count: miss_count });
    }

    let (target, trailing) = read_unit_target_tail(&mut r, "SMSG_SPELL_GO")?;
    r.finish()?;
    Ok(SpellGo {
        cast_item,
        caster,
        cast_count,
        spell_id,
        cast_flags,
        timestamp,
        hits,
        target,
        trailing,
    })
}

/// What `SMSG_CAST_FAILED` said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastFailed {
    pub cast_count: u8,
    pub spell_id: u32,
    pub reason: u8,
}

/// Parses `SMSG_CAST_FAILED`.
///
/// Deliberately **does not** call `finish`. Some reasons carry extra data
/// whose shape depends on the reason -- a required reagent, a missing skill --
/// and this client does not interpret those yet. Refusing the packet because
/// of trailing bytes would turn "you are out of range" into a decode error,
/// which is worse than ignoring a detail. Every other parser in this crate
/// asserts full consumption; this one says why it cannot.
pub fn parse_cast_failed(body: &[u8]) -> Result<CastFailed, Error> {
    let mut r = Reader::new(body, "SMSG_CAST_FAILED");
    Ok(CastFailed {
        cast_count: r.u8()?,
        spell_id: r.u32()?,
        reason: r.u8()?,
    })
}

/// A description of a cast failure.
///
/// **Deliberately almost empty**, and that is the interesting part.
///
/// The obvious thing to write here is the whole `SpellCastResult` table from
/// memory or from a wiki page. This milestone had just finished paying for
/// exactly that: `CHAT_MSG_SAY` was assumed to be `0x00` when `0x00` is
/// `CHAT_MSG_SYSTEM`, and the resulting silence cost three rounds of debugging.
/// A cast-failure table is the same hazard with a worse failure mode -- a wrong
/// name here does not error, it *confidently misexplains* why something did not
/// work, and sends the next person looking in the wrong place.
///
/// So reasons are named only once observed against a live realm, one at a time.
/// Everything else keeps its number, which is honest and still actionable.
pub fn describe_cast_failure(reason: u8) -> String {
    match reason {
        // Observed: a self-targeted Heroic Strike (a melee attack that
        // requires an enemy) against a live 3.3.5a realm. The name is this
        // codebase's own description of the observation, not a claim about
        // what the enum constant is called.
        0x0C => "the spell cannot be cast on that target".to_string(),
        other => format!("reason {other:#04x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spellbook(spells: &[u32], cooldowns: &[u32]) -> Vec<u8> {
        let mut body = vec![0u8];
        body.extend((spells.len() as u16).to_le_bytes());
        for id in spells {
            body.extend(id.to_le_bytes());
            body.extend(0u16.to_le_bytes());
        }
        body.extend((cooldowns.len() as u16).to_le_bytes());
        for id in cooldowns {
            body.extend(id.to_le_bytes());
            body.extend(1500u32.to_le_bytes()); // second word
        }
        body
    }

    #[test]
    fn a_spellbook_parses() {
        let parsed = parse_initial_spells(&spellbook(&[6603, 78, 2457], &[78])).unwrap();
        assert_eq!(
            parsed.spells.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![6603, 78, 2457]
        );
        assert_eq!(parsed.cooldowns.len(), 1);
        assert_eq!(parsed.cooldowns[0].second, 1500);
    }

    #[test]
    fn an_empty_spellbook_parses() {
        let parsed = parse_initial_spells(&spellbook(&[], &[])).unwrap();
        assert!(parsed.spells.is_empty());
        assert!(parsed.cooldowns.is_empty());
    }

    /// Pinned to an actual `SMSG_INITIAL_SPELLS` captured from the live test
    /// realm (`wow1.nekos.farm`) after casting spell `59752`, "Every Man for
    /// Himself" -- the packet that first exercised a non-empty cooldown list
    /// and found the 16-byte entry this module used to assume was wrong. Its
    /// cooldown section, verbatim: count `4`, then four `(spell_id, second)`
    /// pairs at 8 bytes each. The first entry's first word is `59752` itself,
    /// which is what proved the split; this test guards against silently
    /// reverting to the wider, never-actually-observed shape.
    #[test]
    fn the_cooldown_list_matches_a_captured_live_packet() {
        let mut body = vec![0u8]; // unknown leading byte
        body.extend(0u16.to_le_bytes()); // no known spells, to keep this focused
        body.extend(4u16.to_le_bytes()); // cooldown_count, as captured
        body.extend([
            0x68, 0xe9, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // (59752, 0)
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // (0, 0)
            0x30, 0x1c, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // (72752, 0)
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // (0, 0)
        ]);

        let parsed = parse_initial_spells(&body).unwrap();
        assert_eq!(
            parsed.cooldowns.iter().map(|c| (c.spell_id, c.second)).collect::<Vec<_>>(),
            vec![(59752, 0), (0, 0), (72752, 0), (0, 0)]
        );
    }

    /// Two counted lists back to back is the whole risk: a wrong width in the
    /// first does not fail, it reads the second from the middle of the first.
    /// Only the cursor running out -- or having bytes left -- says so.
    #[test]
    fn a_wrong_width_shows_up_as_leftovers() {
        let body = spellbook(&[6603, 78], &[]);

        let mut extra = body.clone();
        extra.push(0);
        assert!(parse_initial_spells(&extra).is_err());

        for cut in 1..body.len() {
            assert!(
                parse_initial_spells(&body[..cut]).is_err(),
                "{cut} bytes parsed as a whole spellbook"
            );
        }
    }

    /// A count that promises more than the packet holds must fail rather than
    /// allocate what it was told to.
    #[test]
    fn an_overstated_count_is_rejected() {
        let mut body = vec![0u8];
        body.extend(60_000u16.to_le_bytes());
        assert!(parse_initial_spells(&body).is_err());
    }

    fn cooldown_body(caster: u64, flags: u8, entries: &[(u32, u32)]) -> Vec<u8> {
        let mut body = caster.to_le_bytes().to_vec();
        body.push(flags);
        for (spell, cooldown) in entries {
            body.extend(spell.to_le_bytes());
            body.extend(cooldown.to_le_bytes());
        }
        body
    }

    #[test]
    fn a_spell_cooldown_parses_one_entry() {
        let body = cooldown_body(0x0000_0000_0000_0032, 0, &[(78, 1500)]);
        let (caster, cooldowns) = parse_spell_cooldown(&body).unwrap();
        assert_eq!(caster, 0x32);
        assert_eq!(cooldowns.len(), 1);
        assert_eq!(cooldowns[0].spell_id, 78);
        assert_eq!(cooldowns[0].cooldown_ms, 1500);
    }

    #[test]
    fn a_spell_cooldown_parses_several_entries_with_no_count_field() {
        let body = cooldown_body(0x32, 1, &[(78, 1500), (172, 6000), (2457, 0)]);
        let (_, cooldowns) = parse_spell_cooldown(&body).unwrap();
        assert_eq!(
            cooldowns.iter().map(|c| (c.spell_id, c.cooldown_ms)).collect::<Vec<_>>(),
            vec![(78, 1500), (172, 6000), (2457, 0)]
        );
    }

    #[test]
    fn an_empty_spell_cooldown_parses_to_no_entries() {
        let body = cooldown_body(0x32, 0, &[]);
        let (caster, cooldowns) = parse_spell_cooldown(&body).unwrap();
        assert_eq!(caster, 0x32);
        assert!(cooldowns.is_empty());
    }

    /// There is no count field here, so a trailing partial entry is the only
    /// evidence of a wrong width -- the same cursor discipline as every other
    /// parser in this crate, just without a count to get wrong in the first
    /// place.
    #[test]
    fn a_partial_trailing_entry_is_rejected() {
        let mut body = cooldown_body(0x32, 0, &[(78, 1500)]);
        body.push(0xFF); // one stray byte, not a whole entry
        assert!(parse_spell_cooldown(&body).is_err());
    }

    #[test]
    fn a_truncated_header_is_rejected() {
        for cut in 0..9 {
            let body = cooldown_body(0x32, 0, &[(78, 1500)]);
            assert!(
                parse_spell_cooldown(&body[..cut]).is_err(),
                "{cut} bytes parsed as a whole header"
            );
        }
    }

    /// Pinned to the real `SMSG_SPELL_START` captured from `wow1.nekos.farm`
    /// (`wow-cli world --cast 5185 --cast-self`, Healing Touch cast by
    /// `Testdruid`, guid `0x33`). Every field here is exactly what that
    /// capture held; see `SpellStart`'s doc comment for what is and is not
    /// confirmed about the shape.
    #[test]
    fn a_spell_start_matches_a_captured_live_packet() {
        let body: [u8; 27] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0xdc,
            0x05, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x33, 0x64, 0x00, 0x00, 0x00,
        ];
        let parsed = parse_spell_start(&body).unwrap();
        assert_eq!(parsed.cast_item, 0x33);
        assert_eq!(parsed.caster, 0x33);
        assert_eq!(parsed.cast_count, 0);
        assert_eq!(parsed.spell_id, 5185);
        assert_eq!(parsed.cast_flags, 0x0802);
        assert_eq!(parsed.cast_time_ms, 1500);
        assert_eq!(parsed.target, 0x33);
        assert_eq!(parsed.trailing, 100);
    }

    /// Pinned to the `SMSG_SPELL_GO` captured in the same batch as the
    /// `SMSG_SPELL_START` above -- same cast, same wire dump.
    #[test]
    fn a_spell_go_matches_a_captured_live_packet() {
        let body: [u8; 37] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x74,
            0xd4, 0x37, 0x5a, 0x01, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x01, 0x33, 0x51, 0x00, 0x00, 0x00,
        ];
        let parsed = parse_spell_go(&body).unwrap();
        assert_eq!(parsed.cast_item, 0x33);
        assert_eq!(parsed.caster, 0x33);
        assert_eq!(parsed.cast_count, 0);
        assert_eq!(parsed.spell_id, 5185);
        assert_eq!(parsed.cast_flags, 0x0900);
        assert_eq!(parsed.timestamp, 0x5a37d474);
        assert_eq!(parsed.hits, vec![0x33]);
        assert_eq!(parsed.target, 0x33);
        assert_eq!(parsed.trailing, 0x51);
    }

    /// Both parsers assert full consumption, the same cursor discipline as
    /// every other parser in this crate: a wrong field width here reads the
    /// next field from the middle of this one, and only a leftover or missing
    /// byte says so.
    #[test]
    fn a_wrong_width_in_spell_start_or_go_shows_up_as_leftovers_or_truncation() {
        let start: [u8; 27] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0xdc,
            0x05, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x33, 0x64, 0x00, 0x00, 0x00,
        ];
        let mut extra = start.to_vec();
        extra.push(0);
        assert!(parse_spell_start(&extra).is_err());
        for cut in 0..start.len() {
            assert!(
                parse_spell_start(&start[..cut]).is_err(),
                "{cut} bytes parsed as a whole SMSG_SPELL_START"
            );
        }

        let go: [u8; 37] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x74,
            0xd4, 0x37, 0x5a, 0x01, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x01, 0x33, 0x51, 0x00, 0x00, 0x00,
        ];
        let mut extra = go.to_vec();
        extra.push(0);
        assert!(parse_spell_go(&extra).is_err());
        for cut in 0..go.len() {
            assert!(
                parse_spell_go(&go[..cut]).is_err(),
                "{cut} bytes parsed as a whole SMSG_SPELL_GO"
            );
        }
    }

    /// A target shape other than `target_flags::UNIT` has never been
    /// captured live, and the parser says so with a specific error rather
    /// than misreading whatever would follow it.
    #[test]
    fn an_unrecognised_target_shape_is_refused_by_name() {
        let mut start: [u8; 27] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0xdc,
            0x05, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x33, 0x64, 0x00, 0x00, 0x00,
        ];
        // Flip the target flags to something other than UNIT (offset 17..21).
        start[17] = 0x00;
        let error = parse_spell_start(&start).unwrap_err();
        assert!(
            matches!(
                error,
                crate::protocol::Error::UnsupportedSpellTarget { flags: 0, .. }
            ),
            "{error}"
        );
    }

    /// A nonzero miss count has never been captured live either, and the
    /// per-entry shape it would need is exactly the kind of thing this
    /// project's rules say not to invent.
    #[test]
    fn a_nonzero_miss_count_is_refused_rather_than_guessed() {
        let mut go: [u8; 37] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x74,
            0xd4, 0x37, 0x5a, 0x01, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x01, 0x33, 0x51, 0x00, 0x00, 0x00,
        ];
        go[26] = 1; // miss_count, actually zero in the capture
        let error = parse_spell_go(&go).unwrap_err();
        assert!(
            matches!(
                error,
                crate::protocol::Error::UnconfirmedSpellMisses { count: 1 }
            ),
            "{error}"
        );
    }

    /// The target mask decides how the server reads everything after it, so a
    /// self cast and a targeted one are different lengths and must stay that
    /// way. Claiming a unit target without supplying a guid does not fail --
    /// it reads whatever comes next as one.
    #[test]
    fn a_targeted_cast_carries_a_guid_and_a_self_cast_does_not() {
        let on_self = cast_spell(6603, None);
        assert_eq!(on_self.len(), 1 + 4 + 1 + 4);
        assert_eq!(&on_self[6..10], &target_flags::SELF.to_le_bytes());

        let on_target = cast_spell(6603, Some(0xF130_0000_2B00_0BBA));
        assert!(on_target.len() > on_self.len());
        assert_eq!(&on_target[6..10], &target_flags::UNIT.to_le_bytes());

        // And the guid must be the packed form, which the update layer already
        // round-trips -- so read it back with the real reader.
        let mut r = Reader::new(&on_target[10..], "guid");
        assert_eq!(
            crate::update::read_packed_guid(&mut r).unwrap(),
            0xF130_0000_2B00_0BBA
        );
        r.finish().unwrap();
    }

    #[test]
    fn the_spell_id_survives_the_write() {
        let body = cast_spell(0x1234_5678, None);
        assert_eq!(&body[1..5], &0x1234_5678u32.to_le_bytes());
    }

    /// A failure this client has not observed keeps its number.
    ///
    /// A wrong name here would not error -- it would confidently misexplain
    /// why a cast failed and send the next reader looking in the wrong place.
    /// Only reasons actually seen against a live realm get words.
    #[test]
    fn unobserved_failures_keep_their_number() {
        assert!(describe_cast_failure(0x0C).contains("target"));
        assert!(describe_cast_failure(0xEE).contains("0xee"));
        assert!(describe_cast_failure(0x4A).contains("0x4a"));
    }

    /// The one parser here that does not assert full consumption, because the
    /// tail's shape depends on the reason and refusing it would turn "out of
    /// range" into a decode error.
    #[test]
    fn a_cast_failure_tolerates_data_it_does_not_understand() {
        let mut body = vec![0u8];
        body.extend(6603u32.to_le_bytes());
        body.push(0x4A);
        body.extend(1234u32.to_le_bytes()); // reason-specific detail

        let parsed = parse_cast_failed(&body).unwrap();
        assert_eq!(parsed.spell_id, 6603);
        assert_eq!(parsed.reason, 0x4A);
    }
}
