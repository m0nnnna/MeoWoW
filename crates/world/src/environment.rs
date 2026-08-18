//! Drowning, fatigue and burning: what the world does to a character who
//! stays somewhere they should not.
//!
//! **This is the half of "lava hurts" that belongs to a client, and it is the
//! only half.** The server computes liquid state from its own copy of the same
//! terrain this client draws, runs the timers, and applies the damage; health
//! is a replicated field and nothing here writes it. A client that also
//! subtracted hit points would be inventing a number that disagrees with the
//! server's the moment either is looked at -- the same reason the vendor code
//! trusts the wire's price over `Item.dbc`'s, and the same reason
//! `describe_cast_failure` names one status code rather than a whole enum.
//!
//! So there are four packets here and all four are read-only:
//!
//! | opcode | what it says |
//! |---|---|
//! | `SMSG_START_MIRROR_TIMER` `0x01D9` | a bar has appeared, with a value and a rate |
//! | `SMSG_PAUSE_MIRROR_TIMER` `0x01DA` | the bar has stopped or resumed counting |
//! | `SMSG_STOP_MIRROR_TIMER` `0x01DB` | the bar is gone |
//! | `SMSG_ENVIRONMENTAL_DAMAGE_LOG` `0x01FC` | it just cost this much |
//!
//! The layouts came from AzerothCore's own packet writers, which rule 2 permits
//! reading -- source makes a hypothesis about a body cheap to form, and
//! observation still has to confirm it. Every one of these is fixed-length and
//! parsed through a cursor that must run out exactly, so a wrong layout fails
//! loudly at a known offset rather than producing a plausible number.
//!
//! # Lava sends no timer, and that is the protocol rather than a gap
//!
//! Breath and fatigue call `SendMirrorTimer`; the **fire timer does not**. It
//! counts down entirely server-side -- 2020ms, then a tick every 2020ms -- and
//! the only thing that reaches a client is the damage. So a client drawing
//! three bars would draw two, forever, and waiting for a lava bar to appear
//! before believing the feature works would be waiting on a packet that is
//! never sent. Confirmed live: standing in Searing Gorge produced
//! `SMSG_ENVIRONMENTAL_DAMAGE_LOG` on the tick and nothing else.
//!
//! **`.cheat god` is the way to watch it repeatedly.** The damage packet is
//! written and sent at `Player.cpp:804` and `Unit::DealDamage` -- which is
//! where `CHEAT_GOD` returns 0 -- is not called until line 806. So the log
//! arrives with its real 600-700 amount while the health field never moves.
//! `.gm on` is the *wrong* switch: that one satisfies
//! `IsImmuneToEnvironmentalDamage` and suppresses the packet entirely.

use crate::protocol::{Error, Reader};

/// Which of the three bars a mirror timer packet is about.
///
/// Three timers, and a character can carry more than one at once -- swimming
/// to the bottom of a lava lake runs the breath timer and the fire timer
/// together, which is why the packet names the bar rather than assuming there
/// is only ever one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MirrorTimer {
    /// Time in fatiguing water before exhaustion damage begins.
    Fatigue,
    /// Air left underwater.
    Breath,
    /// Seconds before the next tick of lava or slime damage.
    ///
    /// Named `Fire` after the server's own `FIRE_TIMER` rather than after
    /// lava, because it is the bar slime uses too.
    Fire,
    /// A bar this build does not use, kept as its raw number so a capture can
    /// still say what arrived.
    Unknown(u32),
}

impl MirrorTimer {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::Fatigue,
            1 => Self::Breath,
            2 => Self::Fire,
            other => Self::Unknown(other),
        }
    }
}

/// What hurt a character, in `SMSG_ENVIRONMENTAL_DAMAGE_LOG`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentalDamage {
    Exhausted,
    Drowning,
    Fall,
    Lava,
    Slime,
    Fire,
    /// A cause this build does not use. **Not guessed at**: a wrong name for a
    /// damage source does not fail, it quietly misexplains what killed
    /// somebody, which is precisely the failure `describe_cast_failure` exists
    /// to refuse.
    Unknown(u8),
}

impl EnvironmentalDamage {
    pub fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::Exhausted,
            1 => Self::Drowning,
            2 => Self::Fall,
            3 => Self::Lava,
            4 => Self::Slime,
            5 => Self::Fire,
            other => Self::Unknown(other),
        }
    }

    /// A phrase for the combat log, in the same voice as `combat`'s lines.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Exhausted => "exhaustion",
            Self::Drowning => "drowning",
            Self::Fall => "falling",
            Self::Lava => "lava",
            Self::Slime => "slime",
            Self::Fire => "fire",
            Self::Unknown(_) => "the environment",
        }
    }
}

/// A bar appearing, or being restated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirrorTimerStart {
    pub timer: MirrorTimer,
    /// Milliseconds left.
    pub value: u32,
    /// Milliseconds the bar holds when full.
    pub max_value: u32,
    /// How fast it moves, and **signed**: `-1` while the bar is draining and
    /// `+10` while it refills, so a client that read this unsigned would draw
    /// a drowning bar racing upwards.
    pub scale: i32,
    pub paused: bool,
    /// The aura behind the bar, or 0. Non-zero for the scripted liquids --
    /// `LiquidType` 21, Naxxramas' slime, carries spell 28801.
    pub spell_id: u32,
}

impl MirrorTimerStart {
    pub fn parse(body: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::new(body, "SMSG_START_MIRROR_TIMER");
        let parsed = Self {
            timer: MirrorTimer::from_raw(reader.u32()?),
            value: reader.u32()?,
            max_value: reader.u32()?,
            scale: reader.i32()?,
            paused: reader.u8()? != 0,
            spell_id: reader.u32()?,
        };
        reader.finish()?;
        Ok(parsed)
    }
}

/// A bar stopping or resuming its count without disappearing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirrorTimerPause {
    pub timer: MirrorTimer,
    pub paused: bool,
}

impl MirrorTimerPause {
    pub fn parse(body: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::new(body, "SMSG_PAUSE_MIRROR_TIMER");
        let parsed = Self {
            timer: MirrorTimer::from_raw(reader.u32()?),
            paused: reader.u8()? != 0,
        };
        reader.finish()?;
        Ok(parsed)
    }
}

/// A bar disappearing.
pub fn parse_stop_mirror_timer(body: &[u8]) -> Result<MirrorTimer, Error> {
    let mut reader = Reader::new(body, "SMSG_STOP_MIRROR_TIMER");
    let timer = MirrorTimer::from_raw(reader.u32()?);
    reader.finish()?;
    Ok(timer)
}

/// One tick of environmental damage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvironmentalDamageLog {
    /// Who took it. Always the receiving player in practice -- the server does
    /// not relay other people's drowning -- but carried rather than assumed,
    /// because assuming it is our own guid would mislabel anything that is not.
    pub victim: u64,
    pub cause: EnvironmentalDamage,
    /// What actually came off, after the two below.
    pub amount: u32,
    pub resisted: u32,
    pub absorbed: u32,
}

impl EnvironmentalDamageLog {
    pub fn parse(body: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::new(body, "SMSG_ENVIRONMENTAL_DAMAGE_LOG");
        let parsed = Self {
            victim: reader.u64()?,
            cause: EnvironmentalDamage::from_raw(reader.u8()?),
            amount: reader.u32()?,
            resisted: reader.u32()?,
            absorbed: reader.u32()?,
        };
        reader.finish()?;
        Ok(parsed)
    }

    /// The combat-log line, in the same voice as `combat`'s.
    ///
    /// `second_person` picks the verb: English does not conjugate "you" like a
    /// name, and the caller is the only thing that knows which this is. Passing
    /// the subject alone produced **"You takes 655 damage from lava."** in a
    /// live test -- which reads as a broken substitution rather than as
    /// grammar, and so invites someone to go looking at the parse.
    pub fn describe(&self, name: &str, second_person: bool) -> String {
        let verb = if second_person { "take" } else { "takes" };
        let mut line = format!("{name} {verb} {} damage from {}.", self.amount, self.cause.describe());
        // Only when there is something to say: a line reading "0 absorbed" on
        // every tick is noise that hides the tick where it mattered.
        if self.absorbed > 0 {
            line.push_str(&format!(" ({} absorbed)", self.absorbed));
        }
        if self.resisted > 0 {
            line.push_str(&format!(" ({} resisted)", self.resisted));
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layouts, pinned against bytes rather than against the code.
    ///
    /// Each is fixed-length and each parse ends with `finish`, so a field of
    /// the wrong width cannot pass: it either runs out of input or leaves some
    /// over. That is the check that four separate world-protocol bugs in this
    /// project were invisible without.
    #[test]
    fn a_started_timer_is_twenty_one_bytes() {
        let mut body = Vec::new();
        body.extend_from_slice(&2u32.to_le_bytes()); // FIRE_TIMER
        body.extend_from_slice(&9_000u32.to_le_bytes());
        body.extend_from_slice(&10_000u32.to_le_bytes());
        body.extend_from_slice(&(-1i32).to_le_bytes());
        body.push(0);
        body.extend_from_slice(&57_634u32.to_le_bytes());
        assert_eq!(body.len(), 21);

        let parsed = MirrorTimerStart::parse(&body).unwrap();
        assert_eq!(parsed.timer, MirrorTimer::Fire);
        assert_eq!(parsed.value, 9_000);
        assert_eq!(parsed.max_value, 10_000);
        // The sign is the point: unsigned, this would read as 4294967295 and
        // the bar would appear to be refilling while the character burns.
        assert_eq!(parsed.scale, -1);
        assert!(!parsed.paused);
        assert_eq!(parsed.spell_id, 57_634);
    }

    #[test]
    fn a_short_or_long_body_is_refused() {
        let good = {
            let mut b = Vec::new();
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&1i32.to_le_bytes());
            b.push(0);
            b.extend_from_slice(&0u32.to_le_bytes());
            b
        };
        assert!(MirrorTimerStart::parse(&good).is_ok());
        assert!(MirrorTimerStart::parse(&good[..good.len() - 1]).is_err());
        let mut long = good.clone();
        long.push(0);
        assert!(
            MirrorTimerStart::parse(&long).is_err(),
            "trailing bytes must be an error, not silently dropped"
        );
    }

    #[test]
    fn pause_and_stop_are_their_own_shapes() {
        let mut pause = Vec::new();
        pause.extend_from_slice(&1u32.to_le_bytes());
        pause.push(1);
        assert_eq!(pause.len(), 5);
        let parsed = MirrorTimerPause::parse(&pause).unwrap();
        assert_eq!(parsed.timer, MirrorTimer::Breath);
        assert!(parsed.paused);

        let stop = 0u32.to_le_bytes();
        assert_eq!(parse_stop_mirror_timer(&stop).unwrap(), MirrorTimer::Fatigue);
        // A stop body is four bytes and a pause body is five; parsing one as
        // the other has to fail rather than read the pause flag off the end.
        assert!(parse_stop_mirror_timer(&pause).is_err());
    }

    #[test]
    fn environmental_damage_names_its_cause() {
        let mut body = Vec::new();
        body.extend_from_slice(&4u64.to_le_bytes());
        body.push(3); // DAMAGE_LAVA
        body.extend_from_slice(&655u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&45u32.to_le_bytes());
        assert_eq!(body.len(), 21);

        let parsed = EnvironmentalDamageLog::parse(&body).unwrap();
        assert_eq!(parsed.victim, 4);
        assert_eq!(parsed.cause, EnvironmentalDamage::Lava);
        assert_eq!(parsed.amount, 655);
        assert_eq!(parsed.absorbed, 45);
        assert_eq!(
            parsed.describe("Testwolf", false),
            "Testwolf takes 655 damage from lava. (45 absorbed)"
        );
        // The second person is not the same sentence with a different noun.
        assert_eq!(
            parsed.describe("You", true),
            "You take 655 damage from lava. (45 absorbed)"
        );
    }

    /// An unrecognised cause is carried through rather than named, and reads
    /// as something a person can tell is unhandled.
    #[test]
    fn an_unknown_cause_is_not_given_a_name() {
        assert_eq!(EnvironmentalDamage::from_raw(9), EnvironmentalDamage::Unknown(9));
        assert_eq!(EnvironmentalDamage::from_raw(9).describe(), "the environment");
        assert_eq!(MirrorTimer::from_raw(7), MirrorTimer::Unknown(7));
    }
}
