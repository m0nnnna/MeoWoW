//! Object updates: how the server describes the world to the client.
//!
//! `SMSG_UPDATE_OBJECT` is the packet everything in the game world arrives
//! through -- creation, movement, and every change to a stat, aura or equipped
//! item. It is also the least forgiving thing in the protocol, for a reason
//! worth stating plainly:
//!
//! **Nothing in it is length-prefixed and every part is conditional on a flag
//! read a moment earlier.** A movement block's size depends on its movement
//! flags; a values block's size depends on a bitmask; the number of blocks
//! depends on a count at the front. There is no way to skip a part that is not
//! understood, because finding where it ends *is* the act of understanding it.
//! One misread bit and the remainder of the packet is garbage -- not detectably
//! so, just quietly wrong.
//!
//! The defence is the same one that caught every layout error in the handshake:
//! parse through a cursor and assert the packet was consumed exactly. A block
//! structure that is subtly wrong almost never lands on the exact end of the
//! buffer.

use crate::protocol::{Error, Reader};

/// What a block does to the object it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateType {
    /// Changed field values for an object the client already knows.
    Values,
    /// A movement change alone.
    Movement,
    /// A new object entering view.
    Create,
    /// The same, for objects the client should treat as newly spawned rather
    /// than merely newly visible.
    Create2,
    /// Objects that have left view.
    OutOfRange,
    NearObjects,
}

impl UpdateType {
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Values,
            1 => Self::Movement,
            2 => Self::Create,
            3 => Self::Create2,
            4 => Self::OutOfRange,
            5 => Self::NearObjects,
            _ => return None,
        })
    }
}

/// The eight kinds of thing that can exist in the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Object,
    Item,
    Container,
    Unit,
    Player,
    GameObject,
    DynamicObject,
    Corpse,
}

impl ObjectType {
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Object,
            1 => Self::Item,
            2 => Self::Container,
            3 => Self::Unit,
            4 => Self::Player,
            5 => Self::GameObject,
            6 => Self::DynamicObject,
            7 => Self::Corpse,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Item => "item",
            Self::Container => "container",
            Self::Unit => "unit",
            Self::Player => "player",
            Self::GameObject => "game object",
            Self::DynamicObject => "dynamic object",
            Self::Corpse => "corpse",
        }
    }
}

/// Which optional parts a create block's movement section carries.
pub mod update_flags {
    /// This object is the player receiving the packet.
    pub const SELF: u16 = 0x0001;
    pub const TRANSPORT: u16 = 0x0002;
    pub const HAS_TARGET: u16 = 0x0004;
    pub const UNKNOWN: u16 = 0x0008;
    pub const LOW_GUID: u16 = 0x0010;
    /// Carries a full movement block: something that can move under its own
    /// power.
    pub const LIVING: u16 = 0x0020;
    /// Carries a bare position: something that cannot.
    pub const STATIONARY_POSITION: u16 = 0x0040;
    pub const VEHICLE: u16 = 0x0080;
    pub const POSITION: u16 = 0x0100;
    pub const ROTATION: u16 = 0x0200;
}

/// The movement-state bits that change how much of a movement block is present.
///
/// Only the ones that affect the *layout* are named. The rest describe motion
/// the client would act on but does not need in order to find the packet's end.
pub mod movement_flags {
    /// Running or walking forwards. Set while moving, cleared on stop.
    pub const FORWARD: u32 = 0x0000_0001;
    pub const BACKWARD: u32 = 0x0000_0002;
    /// Sidestepping, without turning. Combines with [`FORWARD`] and
    /// [`BACKWARD`] -- a character running forward and strafing left carries
    /// both bits and travels the diagonal.
    ///
    /// Unlike most constants here these two were not needed until this client
    /// could strafe, so they arrive later than their neighbours. They are not
    /// transcribed from memory: the surrounding values in this module were
    /// established earlier and independently, and every one of them agrees
    /// with the same enum these came from, which is what makes reading a bit
    /// off the end of a confirmed run different from guessing at it.
    pub const STRAFE_LEFT: u32 = 0x0000_0004;
    pub const STRAFE_RIGHT: u32 = 0x0000_0008;
    pub const WALKING: u32 = 0x0000_0100;
    pub const ON_TRANSPORT: u32 = 0x0000_0200;
    pub const FALLING: u32 = 0x0000_1000;
    pub const SWIMMING: u32 = 0x0020_0000;
    pub const FLYING: u32 = 0x0200_0000;
    pub const SPLINE_ELEVATION: u32 = 0x0400_0000;
    pub const SPLINE_ENABLED: u32 = 0x0800_0000;
}

/// The one second-tier movement flag that affects layout.
pub const MOVEMENT_FLAG2_ALWAYS_ALLOW_PITCHING: u16 = 0x0010;
/// Interpolated transport movement appends an extra timestamp.
pub const MOVEMENT_FLAG2_INTERPOLATED_MOVEMENT: u16 = 0x0400;

/// Spline flags that change the layout of the spline block.
mod spline_flags {
    pub const FINAL_POINT: u32 = 0x0000_8000;
    pub const FINAL_TARGET: u32 = 0x0001_0000;
    pub const FINAL_ANGLE: u32 = 0x0002_0000;
}

/// A position in world space, in the same coordinates the terrain uses.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub orientation: f32,
}

/// What a create block says about where something is and how it is moving.
#[derive(Debug, Clone, Default)]
pub struct Movement {
    pub flags: u16,
    pub position: Option<Position>,
    pub movement_flags: u32,
    /// Walk, run, run-back, swim, swim-back, flight, flight-back, turn, pitch.
    pub speeds: Option<[f32; 9]>,
    pub target: Option<u64>,
    /// The full movement state, when this object carries one. Retained because
    /// the client's own movement packets echo the same structure back.
    pub info: Option<crate::movement::MovementInfo>,
}

impl Movement {
    pub fn is_self(&self) -> bool {
        self.flags & update_flags::SELF != 0
    }

    pub fn is_living(&self) -> bool {
        self.flags & update_flags::LIVING != 0
    }
}

/// The field values a block carries, as a sparse index-to-word map.
///
/// Sparse because that is genuinely how it arrives: a bitmask selects which of
/// an object's a-thousand-odd fields are present, and typical updates set a
/// handful. Interpreting an index needs a per-object-type table, which belongs
/// with the game logic rather than here; this layer's job is to get the words
/// out in the right order.
#[derive(Debug, Clone, Default)]
pub struct Fields {
    values: Vec<(u16, u32)>,
}

impl Fields {
    pub fn get(&self, index: u16) -> Option<u32> {
        self.values
            .binary_search_by_key(&index, |(at, _)| *at)
            .ok()
            .map(|found| self.values[found].1)
    }

    /// Reads a pair of adjacent fields as one 64-bit value, which is how guids
    /// are stored.
    pub fn get_u64(&self, index: u16) -> Option<u64> {
        let low = self.get(index)? as u64;
        let high = self.get(index + 1).unwrap_or(0) as u64;
        Some(low | (high << 32))
    }

    pub fn get_f32(&self, index: u16) -> Option<f32> {
        self.get(index).map(f32::from_bits)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.values.iter().copied()
    }

    /// Writes one field, keeping the set sorted.
    ///
    /// Exists for the packets that change a single field without carrying a
    /// whole update block -- `SMSG_POWER_UPDATE` is the first. Going through
    /// the same field table as an object update matters: a rage bar fed from
    /// somewhere else would be a second source of truth for a value the
    /// update blocks also carry, and the two would disagree the moment one
    /// path missed a packet.
    pub fn set(&mut self, index: u16, value: u32) {
        match self.values.binary_search_by_key(&index, |(at, _)| *at) {
            Ok(found) => self.values[found].1 = value,
            Err(insert) => self.values.insert(insert, (index, value)),
        }
    }

    /// Folds a later update into this set, overwriting fields it names.
    ///
    /// This is what makes a `Values` block meaningful. Such a block carries
    /// *only what changed*, so applying it as a replacement rather than a merge
    /// would silently discard every field the object still has -- a creature
    /// that took damage would lose its level, faction and model, and the loss
    /// would look like the create block having been mis-parsed rather than
    /// thrown away afterwards.
    pub fn merge(&mut self, newer: &Fields) {
        if newer.values.is_empty() {
            return;
        }
        // Both sides are sorted by index, so this is a linear merge rather than
        // a lookup per field.
        let mut merged = Vec::with_capacity(self.values.len() + newer.values.len());
        let (mut old, mut new) = (self.values.iter().peekable(), newer.values.iter().peekable());
        loop {
            match (old.peek(), new.peek()) {
                (Some((a, _)), Some((b, _))) if a < b => merged.push(*old.next().unwrap()),
                (Some((a, _)), Some((b, _))) if a > b => merged.push(*new.next().unwrap()),
                (Some(_), Some(_)) => {
                    // Same index: the newer value wins.
                    old.next();
                    merged.push(*new.next().unwrap());
                }
                (Some(_), None) => merged.push(*old.next().unwrap()),
                (None, Some(_)) => merged.push(*new.next().unwrap()),
                (None, None) => break,
            }
        }
        self.values = merged;
    }
}

/// One entry in an update packet.
#[derive(Debug, Clone)]
pub enum Block {
    Values {
        guid: u64,
        fields: Fields,
    },
    Movement {
        guid: u64,
        movement: Movement,
    },
    Create {
        guid: u64,
        object_type: ObjectType,
        /// True for `Create2`, which marks a spawn rather than a thing coming
        /// into view.
        spawned: bool,
        movement: Movement,
        fields: Fields,
    },
    OutOfRange {
        guids: Vec<u64>,
    },
    NearObjects {
        guids: Vec<u64>,
    },
}

impl Block {
    pub fn guid(&self) -> Option<u64> {
        match self {
            Self::Values { guid, .. }
            | Self::Movement { guid, .. }
            | Self::Create { guid, .. } => Some(*guid),
            _ => None,
        }
    }
}

/// Reads a packed guid.
///
/// A mask byte says which of the eight bytes are non-zero, and only those
/// follow. Guids are mostly small numbers in a 64-bit space, so this typically
/// One unit's power changing, from `SMSG_POWER_UPDATE`.
///
/// Not strictly a combat packet -- power regenerates out of combat too -- but
/// it was found in a combat capture, because that is when a warrior's rage
/// moves. Thirty of them arrived during one fight.
///
/// **Confirmed against something outside the packet.** Read as
/// `{packed guid, u8 power type, u32 value}`, all thirty named this client's
/// own guid, all carried power type `1`, and the last one read **500** -- and
/// `wow-cli world --units`, which gets its numbers from the entirely separate
/// object-update path, reported that character at `500/1000` rage at the end
/// of the same run. Two parsers with no code in common agreeing on a number
/// neither could have taken from the other is the strongest check available
/// here, and it is the reason [`Fields::set`] exists rather than this feeding
/// a bar of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerUpdate {
    pub guid: u64,
    /// Indexes the seven consecutive power fields from [`fields::UNIT_POWER1`],
    /// the same way [`fields::UNIT_BYTES_0`]'s fourth byte does. `1` is rage.
    pub power_type: u8,
    pub value: u32,
}

impl PowerUpdate {
    /// Which update field this writes, or `None` for a power index past the
    /// end of the array.
    ///
    /// Bounds-checked rather than trusted: a power type of 200 would otherwise
    /// write over whatever field happens to sit 200 slots past the powers, and
    /// that is a corruption with no error attached to it.
    pub fn field(&self) -> Option<u16> {
        (u16::from(self.power_type) < fields::POWER_COUNT)
            .then(|| fields::UNIT_POWER1 + u16::from(self.power_type))
    }
}

/// The realm's calendar and clock, as `SMSG_LOGIN_SETTIMESPEED` reports it.
///
/// Wanted for lighting rather than for a clock face: every colour in
/// `LightIntBand` and every distance in `LightFloatBand` is a curve *over time
/// of day*, so a client that cannot say what hour it is cannot light the world
/// at all. See `docs/RENDERING.md`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GameTime {
    /// 0-59.
    pub minute: u8,
    /// 0-23.
    pub hour: u8,
    /// 0 is Sunday.
    pub weekday: u8,
    /// 0-based, so 12 is the thirteenth.
    pub day: u8,
    /// 0-based, so 7 is August.
    pub month: u8,
    /// Years since 2000.
    pub year: u8,
    /// Game minutes per real second. `1/60` on the test realm, which makes
    /// game time run at wall-clock rate; a realm may choose otherwise, and a
    /// client that hardcoded the rate would drift on one that did.
    pub speed: f32,
}

impl GameTime {
    /// Minutes since midnight, 0..1440.
    pub fn minute_of_day(&self) -> u32 {
        u32::from(self.hour) * 60 + u32::from(self.minute)
    }

    /// Advances the clock by a real-time duration, at the realm's own rate.
    ///
    /// The server reports the time once at login and never again, so a client
    /// that does not run the clock forward lights the whole session at the
    /// minute it happened to log in.
    pub fn advanced(&self, elapsed: std::time::Duration) -> Self {
        let minutes = self.speed as f64 * elapsed.as_secs_f64();
        let total = f64::from(self.minute_of_day()) + minutes;
        let wrapped = total.rem_euclid(1440.0) as u32;
        Self {
            minute: (wrapped % 60) as u8,
            hour: (wrapped / 60) as u8,
            ..*self
        }
    }
}

/// Reads the realm's clock.
///
/// **The bit layout was confirmed against the wall clock, not transcribed.**
/// One capture decoded to minute 2, hour 4, weekday 4, day 12, month 7, year
/// 26 -- and it was taken at 04:02 UTC on Thursday 13 August 2026. Every field
/// agrees, including the two that are zero-based and the one that counts from
/// 2000, which is six independent checks against a clock this project did not
/// write. A layout that was wrong could not have matched the date as well as
/// the time.
///
/// The trailing `u32` is not named. It is zero in the only capture on hand, and
/// this project's rule is that a number nobody can check is worse than a blank.
pub fn parse_login_set_time_speed(body: &[u8]) -> Result<GameTime, Error> {
    let mut r = Reader::new(body, "SMSG_LOGIN_SETTIMESPEED");
    let packed = r.u32()?;
    let speed = r.f32()?;
    let _unconfirmed_trailing = r.u32()?;
    r.finish()?;
    Ok(GameTime {
        minute: (packed & 0x3F) as u8,
        hour: ((packed >> 6) & 0x1F) as u8,
        weekday: ((packed >> 11) & 0x07) as u8,
        day: ((packed >> 14) & 0x3F) as u8,
        month: ((packed >> 20) & 0x0F) as u8,
        year: ((packed >> 24) & 0xFF) as u8,
        speed,
    })
}

#[cfg(test)]
mod time_tests {
    use super::*;

    /// The exact twelve bytes `wow1.nekos.farm` sent, and what the wall clock
    /// said when they arrived.
    ///
    /// A known-good constant, which is this project's convention for
    /// byte-level parsing -- and here it is unusually strong evidence, because
    /// the six decoded fields are checked against a calendar rather than
    /// against each other. Six agreements with something written by somebody
    /// else is not a layout that happens to parse.
    const CAPTURED: [u8; 12] = [
        0x02, 0x21, 0x73, 0x1a, // packed date and time
        0x8a, 0x88, 0x88, 0x3c, // game speed
        0x00, 0x00, 0x00, 0x00, // unconfirmed trailing word
    ];

    #[test]
    fn the_realms_clock_matches_the_wall_clock_it_was_captured_against() {
        let time = parse_login_set_time_speed(&CAPTURED).expect("captured live");
        // 04:02 UTC, Thursday 13 August 2026.
        assert_eq!((time.hour, time.minute), (4, 2));
        assert_eq!(time.weekday, 4, "Thursday, counting Sunday as zero");
        assert_eq!(time.day, 12, "zero-based, so the thirteenth");
        assert_eq!(time.month, 7, "zero-based, so August");
        assert_eq!(time.year, 26, "years since 2000");
        // 1/60 game minutes per real second: this realm runs at wall-clock
        // rate. Worth pinning because a client that assumed it would drift on
        // a realm that chose otherwise.
        assert!((time.speed - 1.0 / 60.0).abs() < 1e-6, "got {}", time.speed);
        assert_eq!(time.minute_of_day(), 4 * 60 + 2);
    }

    #[test]
    fn a_short_or_long_body_is_refused() {
        assert!(parse_login_set_time_speed(&CAPTURED[..8]).is_err());
        let mut long = CAPTURED.to_vec();
        long.push(0);
        assert!(
            parse_login_set_time_speed(&long).is_err(),
            "trailing bytes must be an error, not ignored"
        );
    }

    /// The clock has to run, because the server reports it once and never
    /// again. An hour of real time at this realm's rate is an hour of game
    /// time.
    #[test]
    fn the_clock_runs_forward_at_the_realms_rate() {
        let time = parse_login_set_time_speed(&CAPTURED).unwrap();
        let later = time.advanced(std::time::Duration::from_secs(3600));
        assert_eq!((later.hour, later.minute), (5, 2));
        // And it wraps at midnight rather than running to hour 25.
        let midnight = time.advanced(std::time::Duration::from_secs(24 * 3600));
        assert_eq!((midnight.hour, midnight.minute), (4, 2));
        let past = time.advanced(std::time::Duration::from_secs(20 * 3600));
        assert_eq!(past.hour, 0, "20 hours past 04:02 is just past midnight");
    }
}

pub fn parse_power_update(body: &[u8]) -> Result<PowerUpdate, Error> {
    let mut r = Reader::new(body, "SMSG_POWER_UPDATE");
    let guid = read_packed_guid(&mut r)?;
    let power_type = r.u8()?;
    let value = r.u32()?;
    r.finish()?;
    Ok(PowerUpdate {
        guid,
        power_type,
        value,
    })
}

/// saves five or six bytes per reference -- and object updates are nothing but
/// guid references.
pub fn read_packed_guid(reader: &mut Reader<'_>) -> Result<u64, Error> {
    let mask = reader.u8()?;
    let mut guid = 0u64;
    for bit in 0..8 {
        if mask & (1 << bit) != 0 {
            guid |= (reader.u8()? as u64) << (bit * 8);
        }
    }
    Ok(guid)
}

/// Writes a guid in the same packed form, for the client's own packets.
pub fn write_packed_guid(guid: u64, into: &mut Vec<u8>) {
    let mut mask = 0u8;
    let mut bytes = Vec::with_capacity(8);
    for bit in 0..8 {
        let byte = (guid >> (bit * 8)) as u8;
        if byte != 0 {
            mask |= 1 << bit;
            bytes.push(byte);
        }
    }
    into.push(mask);
    into.extend_from_slice(&bytes);
}

fn read_fields(reader: &mut Reader<'_>) -> Result<Fields, Error> {
    let block_count = reader.u8()? as usize;
    let mut mask = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        mask.push(reader.u32()?);
    }

    let mut values = Vec::new();
    for (word, bits) in mask.iter().enumerate() {
        if *bits == 0 {
            continue;
        }
        for bit in 0..32 {
            if bits & (1 << bit) != 0 {
                let index = (word * 32 + bit) as u16;
                values.push((index, reader.u32()?));
            }
        }
    }
    // Produced in ascending index order by construction, which `Fields::get`
    // relies on for its binary search.
    Ok(Fields { values })
}

/// Reads the movement state inside a create block.
///
/// Delegates to [`crate::movement::MovementInfo`] rather than reading the
/// fields here. The client sends this same structure back when it moves, and
/// two independent copies of a conditional layout would be free to drift --
/// with the outgoing half having no parse error to announce the drift.
fn read_movement_info(reader: &mut Reader<'_>) -> Result<crate::movement::MovementInfo, Error> {
    crate::movement::MovementInfo::read(reader)
}

/// Reads the spline block appended when something is following a path.
///
/// Nothing here is retained yet -- movement prediction is a later milestone --
/// but it must still be parsed exactly, because the bytes sit between the
/// movement block and the field values.
fn skip_spline(reader: &mut Reader<'_>) -> Result<(), Error> {
    let flags = reader.u32()?;
    // Exactly one facing mode, chosen by the first flag that matches.
    if flags & spline_flags::FINAL_ANGLE != 0 {
        let _angle = reader.f32()?;
    } else if flags & spline_flags::FINAL_TARGET != 0 {
        let _target = reader.u64()?;
    } else if flags & spline_flags::FINAL_POINT != 0 {
        reader.skip(12)?;
    }

    reader.skip(4 + 4 + 4)?; // time passed, duration, id
    reader.skip(4 + 4 + 4)?; // duration modifiers, vertical acceleration
    let _effect_start = reader.u32()?;

    let points = reader.u32()? as usize;
    reader.skip(points.checked_mul(12).ok_or(Error::Oversized { got: points })?)?;
    let _mode = reader.u8()?;
    reader.skip(12)?; // final destination
    Ok(())
}

fn read_movement(reader: &mut Reader<'_>) -> Result<Movement, Error> {
    let flags = reader.u16()?;
    let mut movement = Movement {
        flags,
        ..Movement::default()
    };

    if flags & update_flags::LIVING != 0 {
        let info = read_movement_info(reader)?;
        movement.movement_flags = info.flags;
        movement.position = Some(info.position);
        movement.info = Some(info);

        let mut speeds = [0f32; 9];
        for speed in speeds.iter_mut() {
            *speed = reader.f32()?;
        }
        movement.speeds = Some(speeds);

        if info.flags & movement_flags::SPLINE_ENABLED != 0 {
            skip_spline(reader)?;
        }
    } else if flags & update_flags::POSITION != 0 {
        // A transportable object. The layout is easy to get wrong by one field:
        // the position appears *twice* -- once absolute, once relative to the
        // transport -- but the orientation appears once, between them and the
        // trailing corpse-facing float. Eight floats, not nine.
        let _transport = read_packed_guid(reader)?;
        let (x, y, z) = (reader.f32()?, reader.f32()?, reader.f32()?);
        reader.skip(12)?; // the same point, relative to the transport
        let orientation = reader.f32()?;
        let _corpse_orientation = reader.f32()?;
        movement.position = Some(Position {
            x,
            y,
            z,
            orientation,
        });
    } else if flags & update_flags::STATIONARY_POSITION != 0 {
        movement.position = Some(Position {
            x: reader.f32()?,
            y: reader.f32()?,
            z: reader.f32()?,
            orientation: reader.f32()?,
        });
    }

    if flags & update_flags::UNKNOWN != 0 {
        let _unknown = reader.u32()?;
    }
    if flags & update_flags::LOW_GUID != 0 {
        let _low_guid = reader.u32()?;
    }
    if flags & update_flags::HAS_TARGET != 0 {
        movement.target = Some(read_packed_guid(reader)?);
    }
    if flags & update_flags::TRANSPORT != 0 {
        let _path_timer = reader.u32()?;
    }
    if flags & update_flags::VEHICLE != 0 {
        let _vehicle_id = reader.u32()?;
        let _facing = reader.f32()?;
    }
    if flags & update_flags::ROTATION != 0 {
        let _packed_rotation = reader.u64()?;
    }

    Ok(movement)
}

/// Parses an `SMSG_UPDATE_OBJECT` body.
pub fn parse_update_object(body: &[u8]) -> Result<Vec<Block>, Error> {
    let mut reader = Reader::new(body, "SMSG_UPDATE_OBJECT");
    let count = reader.u32()? as usize;

    let mut blocks = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let code = reader.u8()?;
        let update_type =
            UpdateType::from_code(code).ok_or(Error::UnknownUpdateType { got: code })?;

        let block = match update_type {
            UpdateType::Values => {
                let guid = read_packed_guid(&mut reader)?;
                Block::Values {
                    guid,
                    fields: read_fields(&mut reader)?,
                }
            }
            UpdateType::Movement => {
                let guid = read_packed_guid(&mut reader)?;
                Block::Movement {
                    guid,
                    movement: read_movement(&mut reader)?,
                }
            }
            UpdateType::Create | UpdateType::Create2 => {
                let guid = read_packed_guid(&mut reader)?;
                let code = reader.u8()?;
                let object_type =
                    ObjectType::from_code(code).ok_or(Error::UnknownObjectType { got: code })?;
                let movement = read_movement(&mut reader)?;
                let fields = read_fields(&mut reader)?;
                Block::Create {
                    guid,
                    object_type,
                    spawned: update_type == UpdateType::Create2,
                    movement,
                    fields,
                }
            }
            UpdateType::OutOfRange | UpdateType::NearObjects => {
                let count = reader.u32()? as usize;
                let mut guids = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    guids.push(read_packed_guid(&mut reader)?);
                }
                if update_type == UpdateType::OutOfRange {
                    Block::OutOfRange { guids }
                } else {
                    Block::NearObjects { guids }
                }
            }
        };
        blocks.push(block);
    }

    // The check that makes the rest trustworthy. Every conditional branch above
    // is a chance to read the wrong number of bytes, and a block structure that
    // is subtly wrong will almost never end exactly on the buffer's end.
    reader.finish()?;
    Ok(blocks)
}

/// Parses `SMSG_COMPRESSED_UPDATE_OBJECT`: the same payload behind zlib.
///
/// The declared length is the *decompressed* size, matching the addon block in
/// the session handshake.
pub fn parse_compressed_update_object(body: &[u8]) -> Result<Vec<Block>, Error> {
    parse_update_object(&decompress_update_object(body)?)
}

/// Expands a compressed update packet without parsing it.
///
/// Split out so a payload that fails to parse can still be written to disk and
/// examined. A desync inside one of these is otherwise very hard to work on:
/// the bytes only exist inside the connection, and the failure is a byte offset
/// with no context.
pub fn decompress_update_object(body: &[u8]) -> Result<Vec<u8>, Error> {
    use std::io::Read;

    let mut reader = Reader::new(body, "SMSG_COMPRESSED_UPDATE_OBJECT");
    let expected = reader.u32()? as usize;
    if expected > crate::protocol::MAX_PACKET {
        return Err(Error::Oversized { got: expected });
    }

    let mut plain = Vec::with_capacity(expected);
    flate2::read::ZlibDecoder::new(&body[4..])
        .read_to_end(&mut plain)
        .map_err(Error::Compress)?;

    if plain.len() != expected {
        // The server sized the buffer from this number, so a mismatch means one
        // side is reading a different packet than the other thinks it wrote.
        return Err(Error::CompressedLength {
            declared: expected,
            got: plain.len(),
        });
    }
    Ok(plain)
}

/// A creature being sent along a server-computed path.
///
/// By a wide margin the most common packet in a populated zone -- a single
/// login burst in Northshire carried nearly four hundred of them.
#[derive(Debug, Clone)]
pub struct MonsterMove {
    pub guid: u64,
    /// Where the path starts, which is where the creature is now.
    pub from: Position,
    /// Where it ends. Absent when the packet is a stop rather than a move.
    pub to: Option<Position>,
    /// Every point of the path, when the server sent them in full.
    ///
    /// **Empty is not "no path", it is "the packet did not spell one out".**
    /// The two spline encodings are not equally informative: a flying or
    /// catmull-rom spline writes every point, while an ordinary ground move
    /// writes only the destination and then packs the intermediates as
    /// offsets from the midpoint, three to a word. This field carries the
    /// former and stays empty for the latter, where [`Self::to`] is the whole
    /// of what the packet said.
    ///
    /// It exists because of taxi flights. A creature walking across a field
    /// is served perfectly by a start, an end and a duration -- which is what
    /// `interpolated_position` was built on. A flight from Stormwind to
    /// Westfall is a curve around terrain, and interpolating its endpoints
    /// would fly the gryphon in a straight line through the hills between
    /// them.
    pub path: Vec<Position>,
    /// How long the whole path takes, in milliseconds.
    pub duration: u32,
    pub stopped: bool,
    /// An explicit facing to arrive at, when the move type provides one.
    /// See [`MoveFacing`] -- three of the four move types carry one, and only
    /// [`MoveFacing::Angle`] is usable without a world to look things up in.
    /// `FACING_ANGLE` carries it directly and is parsed; `FACING_SPOT` and
    /// `FACING_TARGET` also carry one, as a point or a guid to face rather
    /// than a bare angle, and are still only skipped -- the former is a small
    /// further step (this parser already has `from` to measure from), the
    /// latter needs another entity's live position, which is a `WorldState`
    /// lookup this parser has no access to. Neither `from` nor `to` ever
    /// carries an orientation of its own; those fields decode fixed at zero.
    pub facing: Option<MoveFacing>,
}

/// Where a monster move says the creature should end up looking.
///
/// Three of the four modes carry real information and the parser used to keep
/// only one of them, skipping the other two as padding. That is how a creature
/// came to stand side-on to the player it was chewing on: **a melee attacker
/// turns to face its victim with `FACING_TARGET`**, whose body is the victim's
/// guid, and eight skipped bytes are indistinguishable from a packet that said
/// nothing.
///
/// [`MoveFacing::Target`] is the one that cannot be resolved here. A guid is
/// not an angle until somebody knows where that unit is, and a parser has no
/// world to ask -- so it is carried out intact and turned into a heading by
/// [`crate::WorldState::facing_of`], which does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MoveFacing {
    /// A heading in radians, straight off the wire.
    Angle(f32),
    /// A point in the world to look at.
    Spot { x: f32, y: f32, z: f32 },
    /// Another unit to look at, by guid.
    Target(u64),
}

/// How the creature should be facing when it arrives.
mod monster_move_type {
    pub const NORMAL: u8 = 0;
    pub const STOP: u8 = 1;
    pub const FACING_SPOT: u8 = 2;
    pub const FACING_TARGET: u8 = 3;
    pub const FACING_ANGLE: u8 = 4;
}

/// Spline flags that add fields to a monster-move packet.
/// Spline flags, from `SMSG_MONSTER_MOVE`.
///
/// **Two of these were wrong for four milestones and neither ever errored.**
/// A wrong flag bit does not fail a parse: it steers the reader down the
/// other branch of a conditional layout, which then consumes a plausible
/// number of bytes and produces plausible-looking values. This is the
/// "a wrong field offset parses perfectly and returns nonsense" rule with a
/// conditional instead of an offset.
///
/// What surfaced them was a taxi flight going nowhere. The server sends a
/// flight as a spline with every point written out; this client read it as
/// the *packed-offset* shape and lost the route.
///
/// **How loudly that fails depends on arithmetic, which is worth knowing
/// before trusting either outcome.** The packed branch consumes twelve bytes
/// for a destination and four per remaining point, against twelve per point
/// for the real one -- so on the captured 27-point flight it consumed 116 of
/// 324 bytes and the cursor's trailing-byte rule refused the packet outright.
/// That is the good case: the discipline caught it, and what was missing was
/// somebody reading the log. On a route whose count makes the two arithmetics
/// coincide it would instead have parsed cleanly, kept a plausible
/// destination, and produced no path at all -- which the flight detector
/// declines in silence. Both were live; only the first happened to be the one
/// observed.
mod monster_spline_flags {
    /// **`0x0020_0000`, not `0x8`.** The low byte of the flag word holds an
    /// *animation id* rather than flags (`Mask_Animations = 0xFF`), so the
    /// old value was reading one bit of an id as a flag and skipping five
    /// bytes whenever that bit happened to be set. Latent rather than
    /// harmless: it desynchronises the rest of the packet when it fires.
    pub const ANIMATION: u32 = 0x0020_0000;
    pub const PARABOLIC: u32 = 0x0000_0800;
    pub const CATMULLROM: u32 = 0x0004_0000;
    pub const CYCLIC: u32 = 0x0008_0000;
    /// Paths that carry every point rather than packed offsets.
    ///
    /// **`0x0000_2000`, not `0x0000_0200`.** The old value is `Falling`, one
    /// bit position down -- which is why it never matched a real flight and
    /// never matched anything else either. A taxi flight is the only thing in
    /// this game that reliably sets it, so nothing before flight paths could
    /// have noticed.
    pub const FLYING: u32 = 0x0000_2000;
}

/// Whether a `SMSG_MONSTER_MOVE` body is about `guid`, without parsing it.
///
/// A drain of a busy zone carries hundreds of these and `WorldState` already
/// parses every one, so a caller looking for *its own* character wants a test
/// that costs nothing on the ones that are not.
///
/// **Defined once and used by both the viewer and `wow-cli`.** The obvious
/// alternative -- each caller re-encoding the packed guid inline -- is two
/// derivations of one fact, and this particular fact decides whether the
/// client believes the server is flying it. The two would agree until one was
/// touched, and the frame they disagree on is a character standing still while
/// its gryphon leaves.
pub fn monster_move_is_about(body: &[u8], guid: u64) -> bool {
    let mut packed = vec![0u8; 1];
    let mut mask = 0u8;
    for byte in 0..8 {
        let part = ((guid >> (byte * 8)) & 0xff) as u8;
        if part != 0 {
            mask |= 1 << byte;
            packed.push(part);
        }
    }
    packed[0] = mask;
    body.starts_with(&packed)
}

pub fn parse_monster_move(body: &[u8]) -> Result<MonsterMove, Error> {
    let mut reader = Reader::new(body, "SMSG_MONSTER_MOVE");
    let guid = read_packed_guid(&mut reader)?;
    // A flag byte that toggles the forward movement flag; not acted on.
    let _unknown = reader.u8()?;
    let from = Position {
        x: reader.f32()?,
        y: reader.f32()?,
        z: reader.f32()?,
        orientation: 0.0,
    };
    let _spline_id = reader.u32()?;

    // A stop ends the packet here. Reading on would consume whatever follows in
    // the stream, which is the next packet. A stop carries no facing of its
    // own -- there is nothing left to skip or parse for it, unlike the other
    // four types.
    let move_type = reader.u8()?;
    let mut facing = None;
    match move_type {
        monster_move_type::STOP => {
            reader.finish()?;
            return Ok(MonsterMove {
                guid,
                from,
                to: None,
                path: Vec::new(),
                duration: 0,
                stopped: true,
                facing: None,
            });
        }
        monster_move_type::FACING_SPOT => {
            facing = Some(MoveFacing::Spot {
                x: reader.f32()?,
                y: reader.f32()?,
                z: reader.f32()?,
            })
        }
        // A raw guid, not a packed one: this field is a fixed eight bytes,
        // which is why skipping it happened to keep the rest of the packet in
        // step and cost only the answer.
        monster_move_type::FACING_TARGET => facing = Some(MoveFacing::Target(reader.u64()?)),
        monster_move_type::FACING_ANGLE => facing = Some(MoveFacing::Angle(reader.f32()?)),
        monster_move_type::NORMAL => {}
        other => return Err(Error::UnknownUpdateType { got: other }),
    }

    let flags = reader.u32()?;
    if flags & monster_spline_flags::ANIMATION != 0 {
        reader.skip(1 + 4)?; // animation id, effect start time
    }
    let duration = reader.u32()?;
    if flags & monster_spline_flags::PARABOLIC != 0 {
        reader.skip(4 + 4)?; // vertical acceleration, effect start time
    }

    // The path. Two encodings, and picking the wrong one desynchronises the
    // rest of the packet rather than merely losing the waypoints.
    let count = reader.u32()? as usize;
    let mut path = Vec::new();
    let to = if flags & (monster_spline_flags::CATMULLROM | monster_spline_flags::FLYING) != 0 {
        // Every point in full. **All of them are kept**, not just the last:
        // for a taxi flight these points are the route, and the destination
        // is merely the one of them that happens to be at the end.
        path.reserve(count);
        for _ in 0..count {
            path.push(Position {
                x: reader.f32()?,
                y: reader.f32()?,
                z: reader.f32()?,
                orientation: 0.0,
            });
        }
        path.last().copied()
    } else {
        // The destination in full, then the *intermediate* points as offsets
        // from the midpoint, packed three to a word. Only the endpoint is
        // needed here, but the offsets still have to be consumed.
        let destination = Position {
            x: reader.f32()?,
            y: reader.f32()?,
            z: reader.f32()?,
            orientation: 0.0,
        };
        if count > 1 {
            reader.skip((count - 1) * 4)?;
        }
        Some(destination)
    };

    // Cyclic paths repeat and can carry a trailing point this does not model,
    // so the length check is skipped for them rather than reporting a bug that
    // is really an unimplemented case.
    if flags & monster_spline_flags::CYCLIC == 0 {
        reader.finish()?;
    }

    Ok(MonsterMove {
        guid,
        path,
        from,
        to,
        duration,
        stopped: false,
        facing,
    })
}

/// The handful of field indices this client interprets so far.
///
/// The full 3.3.5a table runs past a thousand entries per object type. These
/// are the ones needed to prove the parse is right -- a player's level and
/// health can be checked against what the character list already said.
pub mod fields {
    pub const OBJECT_GUID: u16 = 0x00;
    pub const OBJECT_TYPE: u16 = 0x02;
    pub const OBJECT_ENTRY: u16 = 0x03;
    pub const OBJECT_SCALE: u16 = 0x04;

    /// Whatever this unit currently has selected. Not the same as *our*
    /// selection, which the client decides and sends.
    pub const UNIT_TARGET: u16 = 0x12;
    /// Race, class, gender and power type, one per byte in that order.
    pub const UNIT_BYTES_0: u16 = 0x17;
    pub const UNIT_HEALTH: u16 = 0x18;
    /// The first of seven consecutive power fields, indexed by the unit's own
    /// power type -- see [`UNIT_BYTES_0`]. Reading this one unconditionally
    /// reports a rogue's mana, which is always zero.
    pub const UNIT_POWER1: u16 = 0x19;
    pub const UNIT_MAX_HEALTH: u16 = 0x20;
    /// The first of seven maximums, parallel to [`UNIT_POWER1`].
    pub const UNIT_MAX_POWER1: u16 = 0x21;
    pub const UNIT_LEVEL: u16 = 0x36;
    pub const UNIT_FACTION: u16 = 0x37;
    pub const UNIT_FLAGS: u16 = 0x3B;
    pub const UNIT_DISPLAY_ID: u16 = 0x43;

    /// The model this unit wears **when nothing is transforming it**.
    ///
    /// Declared since the display fields were first named and unused until
    /// there was a druid to point it at. It is the other half of a comparison,
    /// not a thing to draw: [`UNIT_DISPLAY_ID`] is what a client renders, and
    /// this says what that would have been.
    ///
    /// **The difference between the two is the only reliable statement on the
    /// wire that a unit is not itself.** The obvious alternative -- read the
    /// shapeshift form out of [`UNIT_BYTES_2`] and call anything non-zero a
    /// transformation -- is wrong in both directions: a warrior in Battle
    /// Stance carries form 17 with its own body, and a `.morph`ed player
    /// carries form 0 wearing a murloc. Measured on a night elf druid casting
    /// Bear Form: `0x43` moved 55 -> 29415 in the same update block that left
    /// `0x44` at 55.
    pub const UNIT_NATIVE_DISPLAY_ID: u16 = 0x44;

    /// Four packed bytes describing how a unit is *holding itself*, as opposed
    /// to what it is: byte 0 the stand state, byte 2 a set of visibility
    /// flags, byte 3 the animation tier.
    ///
    /// **Only byte 2 is measured here, and only one bit of it.** Casting
    /// Stealth on a rogue set this field from absent to `0x00020000` in the
    /// same update as the shapeshift form -- so bit `0x02` of byte 2 is the
    /// stealth flag, which the server's own source calls `CREEP`. See
    /// [`crate::state::Entity::stealthed`].
    ///
    /// The other two bytes are named from the server's enum and **nothing here
    /// has watched either of them move**, which is why neither has an accessor.
    /// The obvious probe for byte 0 is not the obvious command: AzerothCore's
    /// `.modify standstate` writes [`UNIT_NPC_EMOTESTATE`] instead, which this
    /// project found out by running it and diffing -- `0x53` moved and `0x4a`
    /// did not. A real stand state needs `CMSG_STANDSTATECHANGE`, which this
    /// client cannot send yet.
    ///
    /// Placed by arithmetic against fields already confirmed, the same check
    /// [`UNIT_BYTES_2`] records: `OBJECT_END + 0x44` with `OBJECT_END = 6`, and
    /// the same expression gives `0x43` for the display id and `0x52` for the
    /// NPC flags, both of which this client already reads and both of which
    /// hold what a live capture says they should.
    pub const UNIT_BYTES_1: u16 = 0x4A;

    /// Which byte of [`UNIT_BYTES_1`] carries the visibility flags.
    pub const UNIT_BYTES_1_VIS_FLAGS: usize = 2;

    /// The bit of [`UNIT_BYTES_1`]'s visibility byte that means "stealthed".
    pub const UNIT_VIS_FLAG_CREEP: u8 = 0x02;

    /// Four packed bytes; byte 0 is the sheath state -- see
    /// [`crate::combat::SheathState`] -- and byte 3 is the shapeshift form,
    /// see [`crate::state::Entity::shapeshift_form`].
    ///
    /// Located by arithmetic that was checked against fields already confirmed
    /// here rather than taken on trust: the index is `OBJECT_END + 0x74` with
    /// `OBJECT_END = 6`, and the same expression gives `0x36` for the level and
    /// `0x3B` for the unit flags, both of which this client already reads and
    /// both of which matched a live character (level 5, and the in-combat bit
    /// appearing exactly when a fight started). Then confirmed directly, by
    /// sending each sheath state and watching byte 0 follow.
    pub const UNIT_BYTES_2: u16 = 0x7A;

    /// Which byte of [`UNIT_BYTES_2`] carries the shapeshift form.
    ///
    /// Confirmed twice from opposite ends of the same field, which is what
    /// makes it more than an offset somebody wrote down: a rogue casting
    /// Stealth put `30` here with byte 0 untouched, and a druid casting Bear
    /// Form put `5` here on a field that had not been present at all. The
    /// sheath state has occupied byte 0 since 4.22 and did not move for
    /// either.
    pub const UNIT_BYTES_2_FORM: usize = 3;

    /// Which model a game object wears -- a row in `GameObjectDisplayInfo`,
    /// which is a different table from the one units use.
    ///
    /// **Found by search, and the search caught a false positive worth
    /// recording.** Thirty-two game objects arrive in Northshire's login burst;
    /// resolving every set field of every one of them against
    /// `GameObjectDisplayInfo` gives *two* fields that hit 100%: this one and
    /// `0x02`. The table has 3,790 rows spread over ids up to 9,624, so a 39%
    /// density makes "is it a valid id" nearly free -- exactly the trap
    /// `CLAUDE.md` describes for `Spell.dbc`'s duration column.
    ///
    /// What separates them is not validity but *variation*. `0x02` is the
    /// constant 33 on every object -- it is the type mask -- and resolves to
    /// one model, so all thirty-two would be identical powder kegs. This field
    /// takes seven distinct values resolving to inn benches, elevators, ships
    /// and a zeppelin, and the sixteen benches sit at the abbey the player is
    /// standing in.
    pub const GAMEOBJECT_DISPLAY_ID: u16 = 0x08;

    /// Skin, face, hairstyle and hair colour, one byte each, low byte first.
    ///
    /// This is how *another* player's appearance arrives: a player's display id
    /// is 49 for every human male alive and its `CreatureDisplayInfo` row has
    /// no textures at all, so without these five numbers every other player in
    /// the world renders as a white ghost.
    ///
    /// **Measured, not transcribed**, and the difference was not academic. The
    /// obvious route -- write down the documented index -- produces a client
    /// that parses perfectly and gives every stranger the wrong face, with
    /// nothing in the output to say which field was misread. Instead the same
    /// five numbers arrive twice by unrelated routes: `SMSG_CHAR_ENUM`, parsed
    /// and confirmed against a live realm since 3.2, and these fields. So
    /// `wow-cli world --enter <name> --appearance` packs the character list's
    /// answer and asks which field holds it.
    ///
    /// The first two runs of that search returned *two* candidates and settled
    /// nothing, because every character this project had ever created was made
    /// with an all-zero appearance and a search for zero matches every zero
    /// field in the object. A character deliberately created with five
    /// different non-zero values (`--skin 3 --char-face 5 --hair-style 7
    /// --hair-color 2 --facial-hair 4`) matched exactly one field, holding
    /// `0x02070503` -- which pins the byte *order* as well as the index, since
    /// any other packing would have matched nothing.
    pub const PLAYER_BYTES: u16 = 0x99;

    /// Which guild this character belongs to, or `0`/absent for none.
    ///
    /// **The roster does not carry the guild's id**, which is the reason this
    /// field matters rather than being a curiosity: `SMSG_GUILD_ROSTER` lists
    /// a guild's members and never names the guild, so the only route from
    /// "this character is in a guild" to "the guild is called X" runs through
    /// here and then through `CMSG_GUILD_QUERY`.
    ///
    /// It is also the field that puts a guild name under *another* player's
    /// name plate, because every replicated player carries it and the query
    /// answers for any guild id at all.
    ///
    /// Placed by the enum's own ordering between
    /// [`PLAYER_GHOST`](Self::PLAYER_GHOST) (`PLAYER_FLAGS`, `0x96`) and
    /// [`PLAYER_BYTES`](Self::PLAYER_BYTES) (`0x99`), which is a prediction
    /// and not a measurement -- and measured the way every field here is, by
    /// reading it off a character whose guild the realm's own database states
    /// independently. See `wow-cli world --guild`.
    pub const PLAYER_GUILDID: u16 = 0x97;

    /// Which rank within that guild, as an index into the roster's rank block
    /// and into `SMSG_GUILD_QUERY_RESPONSE`'s names. `0` is the guild master.
    ///
    /// Redundant with the reader's own row on the roster, and it is the
    /// redundancy that makes it worth reading: the two arrive by unrelated
    /// routes and must agree, which is the check that confirms both.
    pub const PLAYER_GUILDRANK: u16 = 0x98;
    /// Facial hair in the low byte; the rest is bank slots and rest state,
    /// which this client does not read.
    ///
    /// Confirmed by the same run: `0x02000004` with the character list saying
    /// facial hair 4.
    pub const PLAYER_BYTES_2: u16 = 0x9A;

    /// The base of the player's own quest log: one entry per quest, at
    /// `PLAYER_QUEST_LOG + slot * QUEST_LOG_STRIDE`, the first field of each
    /// entry being the quest id.
    ///
    /// **Measured, not transcribed, and the technique is the one that keeps
    /// working here: search for an answer you already know.** A quest id is
    /// a number *we* chose -- we asked the server to add it -- and nothing
    /// else in a player object has any reason to hold that exact value. So
    /// accepting a quest and asking which field changed *to that id* cannot
    /// come out right by luck, where "contains a plausible small integer"
    /// would match dozens of fields.
    ///
    /// Quest 783 landed at `0x9e`. Three more were then added -- 16, 85 and
    /// 106 -- and landed at `0xa3`, `0xa8` and `0xad`: an arithmetic
    /// progression of stride 5, with four ids we picked. That progression is
    /// the whole identification, and it had to be, because the same dump held
    /// a field at `0x184` coincidentally reading 85. One field matching one
    /// value is nearly free; four matching at a constant stride is not.
    ///
    /// **And the block's extent agrees with a neighbour measured separately.**
    /// The log holds 25 quests, so it spans `0x9e + 25 * 5 = 0x11b` -- which
    /// is exactly [`PLAYER_VISIBLE_ITEM_ENTRY_HEAD`], identified in a
    /// different milestone by a different method against different data.
    /// Two independent measurements meeting flush is corroboration nothing in
    /// either search assumed.
    pub const PLAYER_QUEST_LOG: u16 = 0x9E;

    /// How many update fields one quest log entry occupies.
    ///
    /// Two of the five are now named -- the id and [`QUEST_LOG_STATE`]. The
    /// remaining three carry objective counters and a timer in some order, and
    /// are deliberately **not** named: which is which has not been measured,
    /// and a wrong name on a counter would misreport progress rather than
    /// fail.
    pub const QUEST_LOG_STRIDE: u16 = 5;

    /// Offset within a quest-log entry of the field that says whether the
    /// quest is finished.
    ///
    /// **Measured against two quests in known and opposite states**, which is
    /// the only way this could be settled: every field of an entry holds a
    /// small integer, so "contains a plausible value" separates none of them.
    /// A character holding quest 783 -- which has no objectives at all and is
    /// therefore complete the moment it is taken -- and quest 38 -- which
    /// wants twelve items the character does not have -- read `1` and `0` here
    /// respectively, with all three remaining fields zero on both.
    ///
    /// That names the column and nothing more. See
    /// [`crate::state::Entity::quest_is_complete`] for why only one bit of it
    /// is read.
    pub const QUEST_LOG_STATE: u16 = 1;

    /// How many quests the log holds. Its product with the stride is what
    /// makes the block end exactly where the visible-item block starts.
    pub const QUEST_LOG_SLOTS: u16 = 25;

    /// Offset within a quest-log entry of the first objective counter --
    /// how many of each "kill 8 of these" the character has done.
    ///
    /// **Two counters per field, sixteen bits each, and that was measured
    /// rather than assumed.** Three readings of these bytes are all plausible
    /// -- four counters of eight bits in one field, two of sixteen across two
    /// fields, or one `u32` each -- and every one of them displays a small
    /// number for the first objective, which is the only objective most
    /// quests have. So the sample had to be a quest with **four** objectives,
    /// with all four counters non-zero at once.
    ///
    /// Quest 837 `Encroachment` wants four kills of each of four creatures.
    /// Taken and completed on a live realm, its log entry reads:
    ///
    /// ```text
    ///    quest          +1         +2         +3         +4
    ///      837           1     262148     262148          0
    /// ```
    ///
    /// `262148` is `0x0004_0004`. Two fields each holding two fours is the
    /// sixteen-bit reading and nothing else: eight-bit quarters would have put
    /// `0x04040404` in `+2` alone and left `+3` zero, and a `u32` per counter
    /// would need three fields for four counters and have nowhere left for the
    /// timer that `+4` holds. Quest 7, which has a single objective, agrees --
    /// four kobolds killed read `4` in the low half of `+2`, and the server's
    /// own `character_queststatus.mobcount1` said `4` at the same moment.
    pub const QUEST_LOG_COUNTS: u16 = 2;

    /// How many objective counters share one update field.
    pub const COUNTERS_PER_FIELD: u32 = 2;

    /// How many objectives a quest's counters cover. The same four slots
    /// `SMSG_QUEST_QUERY_RESPONSE` carries, in the same order.
    pub const QUEST_LOG_OBJECTIVES: u32 = 4;

    /// The base of another player's worn-item block: one item **entry** id
    /// per equipped slot, at `PLAYER_VISIBLE_ITEM_ENTRY_HEAD + 2 * slot` for
    /// `slot` in `0..EQUIPPED_COUNT` -- an entry, not a display id, which is
    /// why `Item.dbc` sits between this field and anything paintable. Our own
    /// character needs none of it: `SMSG_CHAR_ENUM` sends display ids for our
    /// own gear directly.
    ///
    /// **Measured by the same technique as [`PLAYER_BYTES`], for the same
    /// reason: a wrong index parses perfectly and dresses every stranger in
    /// the wrong armour, with nothing in the output to say so.** Our own
    /// equipment arrives twice by unrelated routes -- display ids in the
    /// character list, item entries here -- so `wow-cli world --enter <name>
    /// --visible-items` packs the known display id per slot, reads every set
    /// field of our own player object as an item entry through `Item.dbc`,
    /// and asks which fields' resolved display ids agree with which slot.
    ///
    /// Two characters gave the same answer: a dwarf hunter wearing five
    /// items matched fields `0x121, 0x127, 0x129, 0x139, 0x13d` for slots
    /// `3, 6, 7, 15, 17`, and a warrior wearing seventeen matched ten
    /// *unambiguous* slots (the rest wear a visually-identical pair -- two
    /// rings of the same kind, say -- so both fields in the pair resolve to
    /// both slots and neither could be assigned alone). Both sets fit exactly
    /// one `base + slot * 2`, with `base = 0x11b`, and the fit was checked
    /// against every unambiguous point rather than picked from the first two
    /// -- the same run-of-consecutive-fields shape that confirmed
    /// `PLAYER_FIELD_INV_SLOT_HEAD`, and this block ends just before it
    /// (`0x11b + 2*19 = 0x141`, three fields short of `0x144`) -- a fact
    /// nothing in the search assumed and one that did not have to come out
    /// true.
    ///
    /// **That last part is corroboration and not proof, and the gap is why.**
    /// Three unread fields sit between the two blocks, so "immediately
    /// before" would be overstating it: a base three fields further along
    /// would be just as adjacent and is ruled out by the *measurement*, not
    /// by the neighbourhood. The load-bearing evidence is the fit across
    /// every unambiguous slot on two characters, and it reproduces on demand
    /// -- `--visible-items` re-derives the base rather than asserting it, so
    /// running it is a check and not a restatement.
    pub const PLAYER_VISIBLE_ITEM_ENTRY_HEAD: u16 = 0x11b;
    /// How many update fields one visible-item slot occupies. The second
    /// field of the pair was never resolved -- likely an enchantment id --
    /// and this client does not read it.
    pub const VISIBLE_ITEM_STRIDE: u16 = 2;

    /// How many powers a unit has, and so how far past [`UNIT_POWER1`] a power
    /// index is allowed to reach.
    pub const POWER_COUNT: u16 = 7;

    /// `PLAYER_FIELD_BYTES`. Only one bit of it is read here:
    /// [`PLAYER_RELEASE_TIMER_BIT`].
    ///
    /// **This field is a cautionary tale about how a large sample can still be
    /// one sample.** It was found by diffing a living character's fields
    /// against a dead one's, and across *six* snapshots -- three characters,
    /// two accounts, two zones, a warrior and a druid -- it was the only field
    /// whose presence separated alive from not-alive with no exceptions. On
    /// that evidence it was named `PLAYER_NOT_ALIVE`, and it was wrong.
    ///
    /// Every one of those six came from the same *path*: a character dying
    /// naturally and staying dead. Given a server we control, a GM `revive`
    /// produced a character with full health and this bit still set, which no
    /// amount of watching natural deaths could have shown. The population was
    /// broad in characters, accounts, zones and classes, and narrow in the one
    /// dimension that mattered.
    ///
    /// It is the release-timer display flag -- "show the countdown to
    /// automatically releasing spirit" -- so it tracks *the release window
    /// being open*, not being dead. Those coincide for an ordinary death and
    /// come apart the moment anything else resurrects you.
    pub const PLAYER_FIELD_BYTES: u16 = 0x4AD;
    /// The release-timer bit of [`PLAYER_FIELD_BYTES`]. Set while the client
    /// should be showing a countdown to auto-release; **not** a death flag.
    pub const PLAYER_RELEASE_TIMER_BIT: u32 = 0x08;

    /// Whose body a corpse object is, as a guid pair at the start of a corpse's
    /// own fields.
    ///
    /// Needed because "the corpse in view" is not a well-formed question: a
    /// graveyard collects them, and a run that picked whichever came first out
    /// of a hash map would send a player to reclaim someone else's body. The
    /// answer is silence, since reclaiming a corpse that is not yours is one of
    /// the five conditions this request refuses without a word.
    pub const CORPSE_OWNER: u16 = 0x06;

    /// `PLAYER_FLAGS`. Only [`PLAYER_GHOST_BIT`] is read here.
    ///
    /// Set on a player who has **released** and is running back as a ghost, and
    /// not on one merely lying dead where they fell.
    ///
    /// **Dead and ghost are two states, not one, and the first snapshot said
    /// otherwise.** A single before-and-after on one character showed this
    /// field appearing exactly when that character stopped being alive, which
    /// is a complete and wrong explanation: that character had already
    /// released. Killing a *second* character and looking again showed it dead
    /// with health `0` and this field **absent**. Had the first observation
    /// been written down it would have been labelled "dead", and every later
    /// reader would have inherited a flag that is silent for the entire window
    /// between dying and releasing -- which is precisely the window a corpse
    /// run happens in.
    ///
    /// What the three states look like, over six snapshots:
    ///
    /// | state | health | [`PLAYER_NOT_ALIVE`] | this field |
    /// |---|---|---|---|
    /// | alive (3 characters) | > 1 | absent | absent |
    /// | dead, not yet released | `0` | `0x08` | absent |
    /// | ghost, released (2 characters) | `1` | `0x08` | `0x10` |
    ///
    /// Two independent structures agree with the split: the character list's
    /// own `flags & 0x2000` (see `Character::is_ghost`) reads `ghost` for both
    /// released characters and not for the freshly killed one, and it is parsed
    /// from `SMSG_CHAR_ENUM` with no code in common with the update-field path.
    /// The corpse *object* likewise only exists after releasing -- a player
    /// lying dead has none in view.
    pub const PLAYER_GHOST: u16 = 0x96;
    /// The bit of [`PLAYER_GHOST`] that was observed, and the only one.
    pub const PLAYER_GHOST_BIT: u32 = 0x10;

    /// `PLAYER_FIELD_INV_SLOT_HEAD`: the base of the character's inventory
    /// slot array. **Two fields per slot** -- a 64-bit item guid, low word
    /// first -- so slot *n* lives at `INV_SLOT_HEAD + 2n`.
    ///
    /// The slot *ranges* are laid out in [`InventorySlot`](crate::inventory::InventorySlot);
    /// what matters here is that this base was measured rather than
    /// transcribed, and measured twice by different arguments.
    ///
    /// **The stride came from adding items one at a time.** `.additem` on the
    /// live realm put a guid at `0x0174`; a second `.additem` put one at
    /// `0x0176`. Two consecutive backpack slots two fields apart is the stride
    /// and the guid width in one observation, and neither could be inferred
    /// from a single item.
    ///
    /// **The base came from a prediction, not from a plausible reading.** Any
    /// base within a few fields of the truth reads the slot array *slightly*
    /// misaligned, and a misaligned read is not blank -- it returns the high
    /// word of one guid beside the low word of the next, which still looks
    /// like a populated inventory. So the check was not "does this produce
    /// guids" but "does it put the guids where something *else* says they
    /// should be": with this base, a starting human warrior's four item guids
    /// land on slots 3, 6, 7 and 15, which is shirt, legs, feet and main hand.
    /// That is exactly what an AzerothCore human warrior begins wearing, and
    /// it is a claim the identification could have failed.
    ///
    /// Item guids carry a high word of [`ITEM_GUID_HIGH`], which is how one is
    /// recognised at a glance in a field dump.
    pub const PLAYER_FIELD_INV_SLOT_HEAD: u16 = 0x0144;

    /// How many update fields one inventory slot occupies: a guid is 64 bits
    /// and a field is 32.
    pub const INV_SLOT_STRIDE: u16 = 2;

    /// The high word every item guid carries.
    ///
    /// Not load-bearing -- the slot array already says which guids are items
    /// and where they sit -- but it is what makes an item recognisable in a
    /// raw field dump, which is how all of this was found.
    pub const ITEM_GUID_HIGH: u32 = 0x4000_0000;

    /// `PLAYER_FIELD_COINAGE`, in **copper**. Gold and silver are presentation:
    /// 100 copper to a silver, 100 silver to a gold, and the wire knows only
    /// the one number.
    ///
    /// `.modify money 123456` produced exactly `0x0001e240` here -- which is
    /// 123456 -- and changed no other field in the object. A single field
    /// moving to a value we chose is a stronger statement than a field merely
    /// holding a plausible amount of money, because we picked a number no
    /// other field would coincidentally hold.
    pub const PLAYER_FIELD_COINAGE: u16 = 0x0492;

    /// The base of the explored-areas bitfield: 128 fields, one bit per
    /// `AreaTable.dbc` row's [`area_bit`], set once the character has walked
    /// into that area.
    ///
    /// **This is the field that decides what a map shows.** A zone page's
    /// twelve base tiles are the *unexplored* picture -- coastline and
    /// nothing else -- and every road, building and name is a separate
    /// `WorldMapOverlay` patch drawn only where this says the player has been.
    /// Read it wrong and the map is either permanently blank or permanently
    /// complete, and both look deliberate.
    ///
    /// **Measured against two characters whose explored sets differ, which is
    /// what makes it an identification rather than a match.** A bitmask is a
    /// bad search target on its own: a single set word like `0x20000000` is
    /// just a power of two and a player object is full of flags. So two
    /// characters were dumped whose *word* index differs. `Watcher` has
    /// explored one area, `Northshire Valley`, whose area bit is 125 -- word
    /// 3 -- and holds `0x20000000` at field `0x0414`. `Huntertest` has
    /// explored one area in Dun Morogh, area bit 212 -- word 6 -- and holds
    /// `0x00100000` at field `0x0417`. Two fields three apart, for two bits
    /// three words apart, giving the same base from either character.
    ///
    /// The server's own `characters.exploredZones` is 128 space-separated
    /// words and agrees with both, which is the same class of evidence as
    /// `creature_template.npcflag`: a source no client is ever sent.
    ///
    /// [`area_bit`]: https://wowdev.wiki/DB/AreaTable
    pub const PLAYER_EXPLORED_ZONES: u16 = 0x0411;

    /// How many fields the explored-areas bitfield spans, so 4,096 area bits.
    ///
    /// Not guessed from the largest area bit in the table: the server stores
    /// exactly 128 words per character and both measured characters' words sit
    /// inside that span.
    pub const EXPLORED_ZONES_WORDS: u16 = 128;

    /// `UNIT_NPC_FLAGS`: what this creature will do if you talk to it --
    /// gossip, hand out quests, sell things, train, repair.
    ///
    /// **The gate for the whole NPC-interaction feature**, and confirmed
    /// against a source the client is never given. The server's own
    /// `creature_template.npcflag` is a number in its database that is not
    /// sent to a client as such, so a field carrying exactly that value is
    /// identified rather than guessed.
    ///
    /// Of seventy replicated units around Northshire, this field reads `0` on
    /// all sixty-nine creatures the database gives no flags -- wolves, kobolds,
    /// a rabbit, a deer -- and reads exactly **66179** on an Innkeeper Farley
    /// spawned in among them, which is precisely his `npcflag`. An arbitrary
    /// five-digit number matching on the one unit that should have it, with
    /// every other unit at zero, is not a coincidence of magnitude.
    ///
    /// Worth noting the field is *present and zero* on ordinary creatures
    /// rather than absent, which makes the discriminator unambiguous: this is
    /// one of the fields a create block sends regardless.
    ///
    /// **The individual bits are deliberately not named.** 66179 is
    /// `0x10283`, so at least five are set, and which bit means "vendor"
    /// rather than "innkeeper" cannot be read off one sample. They are also
    /// checkable by *behaviour* -- whether a gossip request to a unit carrying
    /// a given bit is answered -- which is a stronger test than any table
    /// lookup and is how the equip and loot writes were confirmed.
    pub const UNIT_NPC_FLAGS: u16 = 0x52;

    /// `UNIT_DYNAMIC_FLAGS`: whether this unit has loot on it, is tapped, or
    /// is tapped by a threat list rather than one player. `foss-wow#81`'s
    /// gate -- a corpse sparkles while this is lootable and stops the moment
    /// it is not.
    ///
    /// **Not read off a capture -- read off the offset from a field this
    /// client had already confirmed.** AzerothCore's `UpdateFields.h` names
    /// `UNIT_NPC_FLAGS` at `OBJECT_END + 0x4C`; this project's own,
    /// independently-measured `UNIT_NPC_FLAGS` constant is `0x52`, so
    /// `OBJECT_END` is `0x06` on this build, and the same header names
    /// `UNIT_DYNAMIC_FLAGS` at `OBJECT_END + 0x49` -- `0x4F`. Two
    /// independently-derived numbers agreeing on a base is the same
    /// cross-check `Spell::recovery_time` rests on.
    ///
    /// Confirmed live rather than left at the hypothesis: this field reads
    /// `0` on every one of twenty-odd nearby, living creatures, and
    /// `13` -- `UNIT_DYNFLAG_LOOTABLE | UNIT_DYNFLAG_TAPPED |
    /// UNIT_DYNFLAG_TAPPED_BY_PLAYER` -- on the one creature a real fight
    /// (not a GM `.die`, which this project has already learned rolls no
    /// loot) had just killed. Same field, same guid, before and after the
    /// one thing that changed.
    pub const UNIT_DYNAMIC_FLAGS: u16 = 0x4F;

    /// `UNIT_DYNFLAG_LOOTABLE`, see [`UNIT_DYNAMIC_FLAGS`]. Only this one bit
    /// is named -- which of the others fire under which conditions has not
    /// been checked against this realm, and this project does not transcribe
    /// enum members it has not itself confirmed.
    pub const UNIT_DYNFLAG_LOOTABLE: u32 = 0x1;

    /// `ITEM_FIELD_STACK_COUNT`: how many are in this stack.
    ///
    /// Measured by **variation**, which is the only thing that could separate
    /// it. An item object carries eight fields and most of them are small
    /// integers, so "contains a plausible stack size" is nearly free -- three
    /// of the eight hold the constant 1 on every item in the bags, and any of
    /// them would look like a stack count on a character carrying only single
    /// items.
    ///
    /// What settled it was asking for counts nobody would hold by accident.
    /// `.additem 2589 3`, `.additem 2592 5` and `.additem 4306 17` produced
    /// items reading 3, 5 and 17 in this field and 1 everywhere else, while
    /// every other field stayed constant across all three. The 17 is the part
    /// that matters: two values could be coincidence between neighbouring
    /// columns, but a field that tracks an arbitrary number we chose, three
    /// times, is reporting that number.
    ///
    /// Absent means one, not zero -- a sparse field set omits zeros and there
    /// is no such thing as a stack of none.
    pub const ITEM_FIELD_STACK_COUNT: u16 = 0x0E;

    /// `CONTAINER_FIELD_NUM_SLOTS`: how many slots this bag has.
    ///
    /// Confirmed the same way, against the server's own `item_template`: a
    /// Small Red Pouch (entry 805, `ContainerSlots` 6) reads 6 here and a Blue
    /// Leather Bag (entry 856, `ContainerSlots` 8) reads 8, in the same field
    /// of two objects that are otherwise identical. Checking one bag would
    /// have proved only that the field contains a number of about the right
    /// size.
    ///
    /// Set only on [`ObjectType::Container`](crate::ObjectType) objects, which
    /// also carry type mask 7 where a plain item carries 3.
    pub const CONTAINER_FIELD_NUM_SLOTS: u16 = 0x40;

    /// `CONTAINER_FIELD_SLOT_1`: the base of a bag's own contents array.
    ///
    /// Same shape as the player's inventory array -- a 64-bit guid per slot,
    /// low word first -- so slot *n* of a bag is at `SLOT_1 + 2n`.
    ///
    /// **This was the last deliberate gap in the inventory work, and it closed
    /// the moment a bag with something in it existed.** The obstacle was never
    /// the protocol. Every bag this project had seen was empty, an
    /// object-create block omits zero fields, and an empty slot is a zero --
    /// so an empty bag and a bag whose contents array we could not find were
    /// the same bytes. `.additem` never places a bag in a bag slot and
    /// hand-editing the database does not survive the server's loader, so no
    /// populated container had ever been observed.
    ///
    /// A **dwarf hunter starts with an ammo pouch already equipped and shot
    /// already in it**, which is a legitimate fixture the server built itself.
    /// Creating one produced a container carrying `0x42` immediately, and
    /// adding two more stacks put guids at `0x44` and `0x46` -- three pairs,
    /// stride two, each resolving to an item object that the *player's* slot
    /// array does not mention. That is confirmation by variation rather than
    /// by one lucky reading.
    pub const CONTAINER_FIELD_SLOT_1: u16 = 0x42;

    /// `ITEM_FIELD_OWNER`: whose item this is, as a guid pair. Always the
    /// player, wherever the item is sitting.
    ///
    /// See [`ITEM_FIELD_CONTAINED`] for why these two are documented together:
    /// separately, neither can be identified at all.
    pub const ITEM_FIELD_OWNER: u16 = 0x06;

    /// `ITEM_FIELD_CONTAINED`: what this item is *inside*, as a guid pair --
    /// the player for something held directly, the bag for something in a bag.
    ///
    /// **These two fields are indistinguishable until an item sits inside a
    /// bag, and that is exactly what identified them.** On every item a
    /// starting character owns they hold the same value: the player's own guid
    /// (`1` on one test character, `4` on another, each matching that
    /// character's guid). Two fields holding one constant tell you nothing
    /// about which is which, and a guess would have been believed.
    ///
    /// A hunter's ammo pouch separates them in one reading. Of ten items, the
    /// seven held directly have both fields equal to the player, and the three
    /// inside the pouch have this one holding the *pouch's* guid while
    /// [`ITEM_FIELD_OWNER`] still holds the player's. The field that changes
    /// when the containment changes is the containment field; the one that
    /// does not is the owner. Same reasoning as the storm column and the game
    /// object display id -- ask which candidate *varies the way the thing it
    /// names varies*.
    pub const ITEM_FIELD_CONTAINED: u16 = 0x08;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_guids_round_trip() {
        for guid in [
            0u64,
            1,
            0xFF,
            0x100,
            0x1234_5678,
            0xFFFF_FFFF_FFFF_FFFF,
            0x0000_0100_0000_0001,
        ] {
            let mut packed = Vec::new();
            write_packed_guid(guid, &mut packed);
            let mut reader = Reader::new(&packed, "guid");
            assert_eq!(read_packed_guid(&mut reader).unwrap(), guid, "{guid:#x}");
            reader.finish().unwrap();
        }
    }

    /// The point of the encoding: zero bytes are omitted entirely.
    #[test]
    fn packed_guids_omit_zero_bytes() {
        let mut packed = Vec::new();
        write_packed_guid(1, &mut packed);
        assert_eq!(packed, vec![0x01, 0x01], "a small guid should cost 2 bytes");

        let mut packed = Vec::new();
        write_packed_guid(0, &mut packed);
        assert_eq!(packed, vec![0x00], "a zero guid is just an empty mask");

        let mut packed = Vec::new();
        write_packed_guid(u64::MAX, &mut packed);
        assert_eq!(packed.len(), 9, "an all-bytes guid costs mask plus eight");
    }

    /// A guid whose zero bytes are in the middle must keep its byte positions,
    /// not compact them. This is the bug the mask exists to prevent.
    #[test]
    fn packed_guids_keep_byte_positions() {
        let guid = 0xAA00_0000_0000_00BBu64;
        let mut packed = Vec::new();
        write_packed_guid(guid, &mut packed);
        assert_eq!(packed, vec![0b1000_0001, 0xBB, 0xAA]);

        let mut reader = Reader::new(&packed, "guid");
        assert_eq!(read_packed_guid(&mut reader).unwrap(), guid);
    }

    fn fields_bytes(entries: &[(u16, u32)]) -> Vec<u8> {
        let highest = entries.iter().map(|(at, _)| *at).max().unwrap_or(0);
        let blocks = (highest as usize / 32) + 1;
        let mut mask = vec![0u32; blocks];
        for (at, _) in entries {
            mask[*at as usize / 32] |= 1 << (*at % 32);
        }

        let mut body = vec![blocks as u8];
        for word in &mask {
            body.extend_from_slice(&word.to_le_bytes());
        }
        let mut sorted = entries.to_vec();
        sorted.sort_by_key(|(at, _)| *at);
        for (_, value) in sorted {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body
    }

    #[test]
    fn sparse_fields_read_back_by_index() {
        let entries = [(0u16, 7u32), (2, 9), (54, 1), (67, 49)];
        let body = fields_bytes(&entries);
        let mut reader = Reader::new(&body, "fields");
        let parsed = read_fields(&mut reader).unwrap();
        reader.finish().unwrap();

        assert_eq!(parsed.len(), 4);
        for (index, value) in entries {
            assert_eq!(parsed.get(index), Some(value), "field {index}");
        }
        assert_eq!(parsed.get(1), None, "an unset field must be absent");
        assert_eq!(parsed.get(999), None);
    }

    /// Values arrive in ascending index order, and the reader must consume them
    /// in that order regardless of how the mask bits are spread across words.
    #[test]
    fn field_values_follow_mask_order_across_words() {
        // Deliberately out of order at the call site.
        let body = fields_bytes(&[(70, 0xAAAA), (3, 0xBBBB), (33, 0xCCCC)]);
        let mut reader = Reader::new(&body, "fields");
        let parsed = read_fields(&mut reader).unwrap();
        reader.finish().unwrap();

        assert_eq!(parsed.get(3), Some(0xBBBB));
        assert_eq!(parsed.get(33), Some(0xCCCC));
        assert_eq!(parsed.get(70), Some(0xAAAA));
    }

    #[test]
    fn a_guid_field_pair_joins_into_one_value() {
        let body = fields_bytes(&[(0, 0x5566_7788), (1, 0x1122_3344)]);
        let mut reader = Reader::new(&body, "fields");
        let parsed = read_fields(&mut reader).unwrap();
        assert_eq!(parsed.get_u64(0), Some(0x1122_3344_5566_7788));
    }

    #[test]
    fn an_empty_mask_yields_no_fields() {
        let mut reader = Reader::new(&[0u8], "fields");
        assert!(read_fields(&mut reader).unwrap().is_empty());
    }

    /// A mask claiming more values than the body holds must error rather than
    /// read past the end.
    #[test]
    fn a_mask_longer_than_its_values_is_rejected() {
        let mut body = fields_bytes(&[(0, 1), (1, 2), (2, 3)]);
        body.truncate(body.len() - 4);
        let mut reader = Reader::new(&body, "fields");
        assert!(read_fields(&mut reader).is_err());
    }

    /// A stationary create block: the smallest complete block there is, and the
    /// one that pins the header layout.
    fn stationary_create(guid: u64, object_type: u8) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(2); // UPDATETYPE_CREATE_OBJECT
        write_packed_guid(guid, &mut body);
        body.push(object_type);
        body.extend_from_slice(&update_flags::STATIONARY_POSITION.to_le_bytes());
        for value in [1.0f32, 2.0, 3.0, 0.5] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body.extend_from_slice(&fields_bytes(&[(fields::OBJECT_ENTRY, 1234)]));
        body
    }

    #[test]
    fn a_stationary_create_block_parses() {
        let mut body = 1u32.to_le_bytes().to_vec();
        body.extend_from_slice(&stationary_create(0x4321, 5));

        let blocks = parse_update_object(&body).unwrap();
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Create {
                guid,
                object_type,
                spawned,
                movement,
                fields,
            } => {
                assert_eq!(*guid, 0x4321);
                assert_eq!(*object_type, ObjectType::GameObject);
                assert!(!spawned);
                assert_eq!(movement.position.unwrap().x, 1.0);
                assert_eq!(movement.position.unwrap().orientation, 0.5);
                assert_eq!(fields.get(fields::OBJECT_ENTRY), Some(1234));
            }
            other => panic!("wrong block: {other:?}"),
        }
    }

    /// Several blocks in one packet, which is the normal case and the one where
    /// a length error in the first block corrupts everything after it.
    #[test]
    fn several_blocks_parse_in_sequence() {
        let mut body = 3u32.to_le_bytes().to_vec();
        body.extend_from_slice(&stationary_create(0x11, 5));
        body.extend_from_slice(&stationary_create(0x22, 7));
        body.push(4); // out of range
        body.extend_from_slice(&2u32.to_le_bytes());
        write_packed_guid(0x33, &mut body);
        write_packed_guid(0x44, &mut body);

        let blocks = parse_update_object(&body).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].guid(), Some(0x11));
        assert_eq!(blocks[1].guid(), Some(0x22));
        match &blocks[2] {
            Block::OutOfRange { guids } => assert_eq!(guids, &[0x33, 0x44]),
            other => panic!("wrong block: {other:?}"),
        }
    }

    /// A living create block, with the movement info and the nine speeds.
    fn living_create(guid: u64, movement_flags: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(3); // CREATE_OBJECT2
        write_packed_guid(guid, &mut body);
        body.push(4); // player
        body.extend_from_slice(&(update_flags::LIVING | update_flags::SELF).to_le_bytes());

        body.extend_from_slice(&movement_flags.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // flags2
        body.extend_from_slice(&12345u32.to_le_bytes()); // time
        for value in [-8949.95f32, -132.49, 83.53, 1.0] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        if movement_flags & (super::movement_flags::SWIMMING | super::movement_flags::FLYING) != 0 {
            body.extend_from_slice(&0.25f32.to_le_bytes()); // pitch
        }
        body.extend_from_slice(&0u32.to_le_bytes()); // fall time
        if movement_flags & super::movement_flags::FALLING != 0 {
            body.extend_from_slice(&[0u8; 16]);
        }
        for speed in [1.0f32, 7.0, 4.5, 4.72, 2.5, 7.0, 4.5, 3.14, 1.0] {
            body.extend_from_slice(&speed.to_le_bytes());
        }
        body.extend_from_slice(&fields_bytes(&[(fields::UNIT_LEVEL, 1)]));
        body
    }

    #[test]
    fn a_living_create_block_parses() {
        let mut body = 1u32.to_le_bytes().to_vec();
        body.extend_from_slice(&living_create(0x07, 0));

        let blocks = parse_update_object(&body).unwrap();
        match &blocks[0] {
            Block::Create {
                object_type,
                spawned,
                movement,
                fields,
                ..
            } => {
                assert_eq!(*object_type, ObjectType::Player);
                assert!(spawned, "CREATE_OBJECT2 marks a spawn");
                assert!(movement.is_self());
                assert!(movement.is_living());
                assert_eq!(movement.position.unwrap().x, -8949.95);
                assert_eq!(movement.speeds.unwrap()[1], 7.0, "run speed");
                assert_eq!(fields.get(fields::UNIT_LEVEL), Some(1));
            }
            other => panic!("wrong block: {other:?}"),
        }
    }

    /// Swimming and flying add a pitch float that nothing else announces. Miss
    /// it and every following byte shifts by four -- the exact failure this
    /// module is built to catch.
    #[test]
    fn pitch_is_present_only_when_the_flags_call_for_it() {
        for flags in [
            movement_flags::SWIMMING,
            movement_flags::FLYING,
            movement_flags::SWIMMING | movement_flags::FALLING,
        ] {
            let mut body = 1u32.to_le_bytes().to_vec();
            body.extend_from_slice(&living_create(0x07, flags));
            let blocks = parse_update_object(&body)
                .unwrap_or_else(|error| panic!("flags {flags:#x}: {error}"));
            match &blocks[0] {
                Block::Create { movement, .. } => {
                    assert_eq!(movement.speeds.unwrap()[1], 7.0, "flags {flags:#x}")
                }
                other => panic!("wrong block: {other:?}"),
            }
        }
    }

    /// The transportable-object layout, which cost a live debugging round.
    ///
    /// The position appears twice -- absolute, then relative to the transport --
    /// but the orientation appears once, between the second copy and a trailing
    /// corpse-facing float. That is eight floats. Reading it as two full
    /// four-float positions plus the trailing one gives nine, overruns by four
    /// bytes, and desynchronises everything after it in the packet.
    #[test]
    fn a_transportable_position_is_eight_floats() {
        let mut body = 1u32.to_le_bytes().to_vec();
        body.push(2);
        write_packed_guid(0x55, &mut body);
        body.push(5); // game object
        body.extend_from_slice(&update_flags::POSITION.to_le_bytes());
        write_packed_guid(0, &mut body); // no transport
        for value in [10.0f32, 20.0, 30.0] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        for value in [10.0f32, 20.0, 30.0] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body.extend_from_slice(&1.25f32.to_le_bytes()); // orientation
        body.extend_from_slice(&0f32.to_le_bytes()); // corpse facing
        body.extend_from_slice(&fields_bytes(&[(fields::OBJECT_ENTRY, 42)]));

        let blocks = parse_update_object(&body).expect("eight floats, not nine");
        match &blocks[0] {
            Block::Create {
                movement, fields, ..
            } => {
                let position = movement.position.expect("a position");
                assert_eq!((position.x, position.y, position.z), (10.0, 20.0, 30.0));
                assert_eq!(position.orientation, 1.25, "orientation read from the wrong float");
                assert_eq!(fields.get(fields::OBJECT_ENTRY), Some(42));
            }
            other => panic!("wrong block: {other:?}"),
        }
    }

    /// A trailing byte means the block layout is wrong, even though every field
    /// in it parsed. This is the whole defence.
    #[test]
    fn leftover_bytes_are_an_error() {
        let mut body = 1u32.to_le_bytes().to_vec();
        body.extend_from_slice(&stationary_create(0x4321, 5));
        body.push(0);
        assert!(matches!(
            parse_update_object(&body),
            Err(Error::Trailing { .. })
        ));
    }

    #[test]
    fn an_unknown_update_type_is_rejected() {
        let mut body = 1u32.to_le_bytes().to_vec();
        body.push(9);
        assert!(matches!(
            parse_update_object(&body),
            Err(Error::UnknownUpdateType { got: 9 })
        ));
    }

    #[test]
    fn an_unknown_object_type_is_rejected() {
        let mut body = 1u32.to_le_bytes().to_vec();
        body.extend_from_slice(&stationary_create(1, 99));
        assert!(matches!(
            parse_update_object(&body),
            Err(Error::UnknownObjectType { got: 99 })
        ));
    }

    /// A count larger than the body must fail rather than allocate wildly.
    #[test]
    fn an_overstated_block_count_is_rejected() {
        let mut body = 5000u32.to_le_bytes().to_vec();
        body.extend_from_slice(&stationary_create(1, 5));
        assert!(parse_update_object(&body).is_err());
    }

    #[test]
    fn the_compressed_form_matches_the_plain_one() {
        use std::io::Write;

        let mut plain = 1u32.to_le_bytes().to_vec();
        plain.extend_from_slice(&stationary_create(0x99, 5));

        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&plain).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut body = (plain.len() as u32).to_le_bytes().to_vec();
        body.extend_from_slice(&compressed);

        let from_compressed = parse_compressed_update_object(&body).unwrap();
        let from_plain = parse_update_object(&plain).unwrap();
        assert_eq!(from_compressed.len(), from_plain.len());
        assert_eq!(from_compressed[0].guid(), from_plain[0].guid());
    }

    /// The declared length is the decompressed size. A mismatch means the two
    /// sides disagree about the payload, which must not be parsed anyway.
    #[test]
    fn a_wrong_declared_length_is_rejected() {
        use std::io::Write;

        let mut plain = 1u32.to_le_bytes().to_vec();
        plain.extend_from_slice(&stationary_create(0x99, 5));
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&plain).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut body = (plain.len() as u32 + 1).to_le_bytes().to_vec();
        body.extend_from_slice(&compressed);
        assert!(matches!(
            parse_compressed_update_object(&body),
            Err(Error::CompressedLength { .. })
        ));
    }

    fn monster_move_body(move_type: u8, extra: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        write_packed_guid(7, &mut body);
        body.push(0); // the unacted-on flag byte
        for value in [1.0f32, 2.0, 3.0] {
            body.extend_from_slice(&value.to_le_bytes()); // from
        }
        body.extend_from_slice(&0u32.to_le_bytes()); // spline id
        body.push(move_type);
        body.extend_from_slice(extra);
        body
    }

    /// **A flying spline keeps every point, and the flag bit that decides
    /// that was wrong for four milestones.**
    ///
    /// The two encodings are a conditional layout, and a wrong flag constant
    /// does not fail -- it steers the reader down the other branch, which
    /// consumes a plausible number of bytes and yields a plausible
    /// destination. The old `FLYING` was `0x0200`, which is the server's
    /// `Falling`, one bit position down. So a taxi flight took the
    /// packed-offset branch, arrived with an empty point list, and the
    /// viewer's flight detection -- which needs two points to describe a
    /// route -- declined it in silence. The character stayed on the ground
    /// while the server flew them to Westfall.
    ///
    /// This asserts the *whole path*, not just the destination: reading only
    /// `to` is exactly what the broken version did successfully.
    #[test]
    fn a_flying_spline_keeps_every_point() {
        const FLYING: u32 = 0x0000_2000;
        let route = [
            [10.0f32, 20.0, 30.0],
            [40.0, 50.0, 60.0],
            [70.0, 80.0, 90.0],
        ];
        let mut extra = Vec::new();
        extra.extend_from_slice(&FLYING.to_le_bytes());
        extra.extend_from_slice(&4200u32.to_le_bytes()); // duration
        extra.extend_from_slice(&(route.len() as u32).to_le_bytes());
        for point in route {
            for value in point {
                extra.extend_from_slice(&value.to_le_bytes());
            }
        }
        let body = monster_move_body(monster_move_type::NORMAL, &extra);

        let parsed = parse_monster_move(&body).expect("a flying spline parses");
        assert_eq!(parsed.duration, 4200);
        assert_eq!(parsed.path.len(), 3, "every point is kept, not just the last");
        for (got, want) in parsed.path.iter().zip(route.iter()) {
            assert_eq!((got.x, got.y, got.z), (want[0], want[1], want[2]));
        }
        let to = parsed.to.expect("a flying spline has a destination");
        assert_eq!((to.x, to.y, to.z), (70.0, 80.0, 90.0));
    }

    /// The old constant, asserted to be the wrong branch rather than merely
    /// unused -- so the regression is named rather than absent.
    ///
    /// `0x0200` is the server's `Falling`, one bit down. A body written with
    /// it takes the packed-offset branch: twelve bytes for a destination,
    /// four per remaining point. Against the real twelve-per-point that
    /// leaves a surplus, and the cursor refuses it -- which is what actually
    /// happened to the captured 27-point flight, 116 bytes consumed of 324.
    ///
    /// The count here is chosen so the two arithmetics **coincide**, which is
    /// the nastier case and the one worth pinning: three points is 36 bytes
    /// the real way and 12 + 2*4 = 20 the packed way, so filling the
    /// difference makes the packet parse *cleanly* while losing the entire
    /// route. A test that only showed the loud failure would suggest this
    /// bug always announces itself, and it does not.
    #[test]
    fn the_old_flying_bit_reads_a_route_as_packed_offsets() {
        const WRONG: u32 = 0x0000_0200;
        let mut extra = Vec::new();
        extra.extend_from_slice(&WRONG.to_le_bytes());
        extra.extend_from_slice(&4200u32.to_le_bytes());
        extra.extend_from_slice(&3u32.to_le_bytes());
        // A destination, then two words that a full-point reading would have
        // taken as the rest of the route.
        for value in [70.0f32, 80.0, 90.0] {
            extra.extend_from_slice(&value.to_le_bytes());
        }
        extra.extend_from_slice(&0u32.to_le_bytes());
        extra.extend_from_slice(&0u32.to_le_bytes());
        let body = monster_move_body(monster_move_type::NORMAL, &extra);

        let parsed = parse_monster_move(&body).expect("it parses -- that is the problem");
        assert!(
            parsed.path.is_empty(),
            "the packed branch spells out no route, which is why the flight was declined"
        );
        assert!(parsed.to.is_some(), "and it still yields a plausible destination");
    }

    /// The two facing modes that used to be skipped as padding.
    ///
    /// **`FACING_TARGET` is how a creature in melee turns to face what it is
    /// hitting**, and its eight bytes were being discarded -- which reads, in
    /// the world, as a wolf chewing on you side-on. `FACING_SPOT`'s three
    /// floats went the same way. Both parsed to the byte the whole time,
    /// because a skip of the right *length* keeps the rest of the packet in
    /// step; the cursor discipline that catches every other layout error here
    /// cannot catch a field that is correctly sized and thrown away.
    #[test]
    fn the_facing_modes_that_name_something_are_kept_not_skipped() {
        fn tail() -> Vec<u8> {
            let mut extra = 0u32.to_le_bytes().to_vec(); // spline flags: none
            extra.extend_from_slice(&4000u32.to_le_bytes()); // duration
            extra.extend_from_slice(&1u32.to_le_bytes()); // one path point
            for value in [10.0f32, 20.0, 30.0] {
                extra.extend_from_slice(&value.to_le_bytes());
            }
            extra
        }

        // Facing a unit: a raw eight-byte guid, not a packed one.
        let mut extra = 0xF130_0000_4500_0A4Bu64.to_le_bytes().to_vec();
        extra.extend_from_slice(&tail());
        let parsed =
            parse_monster_move(&monster_move_body(monster_move_type::FACING_TARGET, &extra))
                .unwrap();
        assert_eq!(parsed.facing, Some(MoveFacing::Target(0xF130_0000_4500_0A4B)));

        // Facing a place: three floats.
        let mut extra = Vec::new();
        for value in [11.0f32, 22.0, 33.0] {
            extra.extend_from_slice(&value.to_le_bytes());
        }
        extra.extend_from_slice(&tail());
        let parsed =
            parse_monster_move(&monster_move_body(monster_move_type::FACING_SPOT, &extra)).unwrap();
        assert_eq!(
            parsed.facing,
            Some(MoveFacing::Spot { x: 11.0, y: 22.0, z: 33.0 })
        );
    }

    /// `FACING_ANGLE` is the one move type carrying a facing this parser can
    /// resolve on its own -- the other two name a place or a unit, which need
    /// a world to look up. Its float has to land in `facing`, not be silently
    /// skipped.
    #[test]
    fn facing_angle_is_parsed_into_facing() {
        let mut extra = 1.25f32.to_le_bytes().to_vec(); // the facing itself
        extra.extend_from_slice(&0u32.to_le_bytes()); // spline flags: none set
        extra.extend_from_slice(&4000u32.to_le_bytes()); // duration
        extra.extend_from_slice(&1u32.to_le_bytes()); // one path point
        for value in [10.0f32, 20.0, 30.0] {
            extra.extend_from_slice(&value.to_le_bytes()); // destination
        }
        let body = monster_move_body(monster_move_type::FACING_ANGLE, &extra);

        let parsed = parse_monster_move(&body).unwrap();
        assert_eq!(parsed.facing, Some(MoveFacing::Angle(1.25)));
        assert_eq!(
            parsed.from,
            Position { x: 1.0, y: 2.0, z: 3.0, orientation: 0.0 }
        );
        assert_eq!(
            parsed.to,
            Some(Position { x: 10.0, y: 20.0, z: 30.0, orientation: 0.0 })
        );
        assert_eq!(parsed.duration, 4000);
    }

    /// A stop carries no further fields at all, so it must not be mistaken
    /// for a move that happens to report no facing.
    #[test]
    fn a_stop_carries_no_facing() {
        let body = monster_move_body(monster_move_type::STOP, &[]);

        let parsed = parse_monster_move(&body).unwrap();
        assert!(parsed.stopped);
        assert!(parsed.to.is_none());
        assert_eq!(parsed.facing, None);
    }

    /// A plain move -- no facing hint of any kind -- must also come back
    /// `None` rather than some other type's field bleeding through.
    #[test]
    fn a_normal_move_has_no_facing() {
        let mut extra = 0u32.to_le_bytes().to_vec(); // spline flags: none set
        extra.extend_from_slice(&2500u32.to_le_bytes()); // duration
        extra.extend_from_slice(&1u32.to_le_bytes()); // one path point
        for value in [5.0f32, 6.0, 7.0] {
            extra.extend_from_slice(&value.to_le_bytes());
        }
        let body = monster_move_body(monster_move_type::NORMAL, &extra);

        let parsed = parse_monster_move(&body).unwrap();
        assert_eq!(parsed.facing, None);
        assert_eq!(parsed.duration, 2500);
    }
}
