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
    /// How long the whole path takes, in milliseconds.
    pub duration: u32,
    pub stopped: bool,
    /// An explicit facing to arrive at, when the move type provides one.
    /// `FACING_ANGLE` carries it directly and is parsed; `FACING_SPOT` and
    /// `FACING_TARGET` also carry one, as a point or a guid to face rather
    /// than a bare angle, and are still only skipped -- the former is a small
    /// further step (this parser already has `from` to measure from), the
    /// latter needs another entity's live position, which is a `WorldState`
    /// lookup this parser has no access to. Neither `from` nor `to` ever
    /// carries an orientation of its own; those fields decode fixed at zero.
    pub facing: Option<f32>,
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
mod monster_spline_flags {
    pub const ANIMATION: u32 = 0x0000_0008;
    pub const PARABOLIC: u32 = 0x0000_0800;
    pub const CATMULLROM: u32 = 0x0004_0000;
    pub const CYCLIC: u32 = 0x0008_0000;
    /// Paths that carry every point rather than packed offsets.
    pub const FLYING: u32 = 0x0000_0200;
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
                duration: 0,
                stopped: true,
                facing: None,
            });
        }
        monster_move_type::FACING_SPOT => reader.skip(12)?,
        monster_move_type::FACING_TARGET => reader.skip(8)?,
        monster_move_type::FACING_ANGLE => facing = Some(reader.f32()?),
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
    let to = if flags & (monster_spline_flags::CATMULLROM | monster_spline_flags::FLYING) != 0 {
        // Every point in full; the destination is the last of them.
        let mut last = None;
        for _ in 0..count {
            last = Some(Position {
                x: reader.f32()?,
                y: reader.f32()?,
                z: reader.f32()?,
                orientation: 0.0,
            });
        }
        last
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

    pub const UNIT_HEALTH: u16 = 0x18;
    pub const UNIT_MAX_HEALTH: u16 = 0x20;
    pub const UNIT_LEVEL: u16 = 0x36;
    pub const UNIT_FACTION: u16 = 0x37;
    pub const UNIT_FLAGS: u16 = 0x3B;
    pub const UNIT_DISPLAY_ID: u16 = 0x43;
    pub const UNIT_NATIVE_DISPLAY_ID: u16 = 0x44;
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

    /// `FACING_ANGLE` is the one move type carrying a facing this parser can
    /// use directly (`FACING_SPOT` and `FACING_TARGET` need more than this
    /// parser alone has -- see `MonsterMove::facing`), so its float has to
    /// land in `facing`, not be silently skipped.
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
        assert_eq!(parsed.facing, Some(1.25));
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
