//! Movement state, in both directions.
//!
//! `MovementInfo` is the one structure this protocol sends *back*. Everything
//! before it was read-only: a wrong guess about a field showed up as a parse
//! error, loudly, at a known offset. Writing changes the failure mode
//! completely -- a malformed movement packet is not rejected with a message, it
//! is read by the server as some *other* valid movement, and the first sign is
//! the character standing somewhere unexpected, or the server quietly correcting
//! a position that was never wrong.
//!
//! So the layout is defined once here and both directions go through it. The
//! read half serves object updates; the write half serves the client's own
//! movement. Sharing them means a field that is wrong is wrong symmetrically,
//! which a round trip can catch, rather than wrong in one direction only, which
//! nothing local can.

use crate::protocol::{Error, Reader};
use crate::update::{
    movement_flags, Position, MOVEMENT_FLAG2_ALWAYS_ALLOW_PITCHING,
    MOVEMENT_FLAG2_INTERPOLATED_MOVEMENT,
};

/// Riding on a boat, zeppelin or elevator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnTransport {
    pub guid: u64,
    /// Position relative to the transport, not to the world.
    pub position: Position,
    pub time: u32,
    pub seat: u8,
    /// Present only with interpolated movement.
    pub interpolated_time: Option<u32>,
}

/// In the air after a jump or a fall.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Falling {
    pub velocity: f32,
    /// Horizontal direction, pre-decomposed by the client that jumped.
    pub sin_angle: f32,
    pub cos_angle: f32,
    pub xy_speed: f32,
}

/// Everything the protocol says about where something is and how it is moving.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MovementInfo {
    pub flags: u32,
    pub flags2: u16,
    /// Client clock in milliseconds. The server uses it to order updates, so it
    /// must advance; it is not checked against the server's own clock.
    pub time: u32,
    pub position: Position,
    pub transport: Option<OnTransport>,
    pub pitch: Option<f32>,
    pub fall_time: u32,
    pub falling: Option<Falling>,
    pub spline_elevation: Option<f32>,
}

impl MovementInfo {
    /// Standing still at a position.
    pub fn standing(position: Position, time: u32) -> Self {
        Self {
            time,
            position,
            ..Self::default()
        }
    }

    /// Whether the flags say a pitch field is present.
    ///
    /// Not stored as a bool: it is derived from the flags on both sides, and a
    /// writer that decided independently could disagree with its own reader.
    fn has_pitch(flags: u32, flags2: u16) -> bool {
        flags & (movement_flags::SWIMMING | movement_flags::FLYING) != 0
            || flags2 & MOVEMENT_FLAG2_ALWAYS_ALLOW_PITCHING != 0
    }

    pub fn read(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let flags = reader.u32()?;
        let flags2 = reader.u16()?;
        let time = reader.u32()?;
        let position = Position {
            x: reader.f32()?,
            y: reader.f32()?,
            z: reader.f32()?,
            orientation: reader.f32()?,
        };

        let transport = if flags & movement_flags::ON_TRANSPORT != 0 {
            let guid = crate::update::read_packed_guid(reader)?;
            let position = Position {
                x: reader.f32()?,
                y: reader.f32()?,
                z: reader.f32()?,
                orientation: reader.f32()?,
            };
            let time = reader.u32()?;
            let seat = reader.u8()?;
            let interpolated_time = if flags2 & MOVEMENT_FLAG2_INTERPOLATED_MOVEMENT != 0 {
                Some(reader.u32()?)
            } else {
                None
            };
            Some(OnTransport {
                guid,
                position,
                time,
                seat,
                interpolated_time,
            })
        } else {
            None
        };

        let pitch = if Self::has_pitch(flags, flags2) {
            Some(reader.f32()?)
        } else {
            None
        };

        let fall_time = reader.u32()?;
        let falling = if flags & movement_flags::FALLING != 0 {
            Some(Falling {
                velocity: reader.f32()?,
                sin_angle: reader.f32()?,
                cos_angle: reader.f32()?,
                xy_speed: reader.f32()?,
            })
        } else {
            None
        };

        let spline_elevation = if flags & movement_flags::SPLINE_ELEVATION != 0 {
            Some(reader.f32()?)
        } else {
            None
        };

        Ok(Self {
            flags,
            flags2,
            time,
            position,
            transport,
            pitch,
            fall_time,
            falling,
            spline_elevation,
        })
    }

    /// Appends this movement state in the wire layout.
    ///
    /// The optional parts are emitted from the **flags**, not from whether the
    /// corresponding field happens to be populated. That is deliberate: the
    /// reader on the other side has only the flags to go on, so a writer that
    /// consulted anything else could produce a packet that no reader --
    /// including this module's own -- could parse back.
    pub fn write(&self, into: &mut Vec<u8>) {
        into.extend_from_slice(&self.flags.to_le_bytes());
        into.extend_from_slice(&self.flags2.to_le_bytes());
        into.extend_from_slice(&self.time.to_le_bytes());
        write_position(&self.position, into);

        if self.flags & movement_flags::ON_TRANSPORT != 0 {
            let transport = self.transport.unwrap_or(OnTransport {
                guid: 0,
                position: Position::default(),
                time: 0,
                seat: 0,
                interpolated_time: None,
            });
            crate::update::write_packed_guid(transport.guid, into);
            write_position(&transport.position, into);
            into.extend_from_slice(&transport.time.to_le_bytes());
            into.push(transport.seat);
            if self.flags2 & MOVEMENT_FLAG2_INTERPOLATED_MOVEMENT != 0 {
                into.extend_from_slice(
                    &transport.interpolated_time.unwrap_or(0).to_le_bytes(),
                );
            }
        }

        if Self::has_pitch(self.flags, self.flags2) {
            into.extend_from_slice(&self.pitch.unwrap_or(0.0).to_le_bytes());
        }

        into.extend_from_slice(&self.fall_time.to_le_bytes());
        if self.flags & movement_flags::FALLING != 0 {
            let falling = self.falling.unwrap_or(Falling {
                velocity: 0.0,
                sin_angle: 0.0,
                cos_angle: 1.0,
                xy_speed: 0.0,
            });
            for value in [
                falling.velocity,
                falling.sin_angle,
                falling.cos_angle,
                falling.xy_speed,
            ] {
                into.extend_from_slice(&value.to_le_bytes());
            }
        }

        if self.flags & movement_flags::SPLINE_ELEVATION != 0 {
            into.extend_from_slice(&self.spline_elevation.unwrap_or(0.0).to_le_bytes());
        }
    }

    /// The bytes this would occupy, without building it.
    pub fn encoded_len(&self) -> usize {
        let mut buffer = Vec::new();
        self.write(&mut buffer);
        buffer.len()
    }
}

fn write_position(position: &Position, into: &mut Vec<u8>) {
    for value in [
        position.x,
        position.y,
        position.z,
        position.orientation,
    ] {
        into.extend_from_slice(&value.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(flags: u32, flags2: u16) -> MovementInfo {
        MovementInfo {
            flags,
            flags2,
            time: 123_456,
            position: Position {
                x: -8949.95,
                y: -132.49,
                z: 83.53,
                orientation: 1.25,
            },
            transport: Some(OnTransport {
                guid: 0x1234_5678,
                position: Position {
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                    orientation: 0.5,
                },
                time: 999,
                seat: 3,
                interpolated_time: Some(4242),
            }),
            pitch: Some(-0.25),
            fall_time: 77,
            falling: Some(Falling {
                velocity: 7.5,
                sin_angle: 0.5,
                cos_angle: 0.86,
                xy_speed: 4.5,
            }),
            spline_elevation: Some(12.5),
        }
    }

    /// Read and write must agree for every combination of the flags that change
    /// the layout.
    ///
    /// This is the test that makes writing safe. A field of the wrong width, or
    /// emitted under the wrong condition, cannot survive a round trip -- and
    /// unlike a parse error, a malformed movement packet sent to a real server
    /// produces no complaint at all, just a character in the wrong place.
    #[test]
    fn every_layout_combination_round_trips() {
        let layout_flags = [
            0,
            movement_flags::ON_TRANSPORT,
            movement_flags::FALLING,
            movement_flags::SWIMMING,
            movement_flags::FLYING,
            movement_flags::SPLINE_ELEVATION,
            movement_flags::ON_TRANSPORT | movement_flags::FALLING,
            movement_flags::SWIMMING | movement_flags::SPLINE_ELEVATION,
            movement_flags::ON_TRANSPORT
                | movement_flags::FALLING
                | movement_flags::FLYING
                | movement_flags::SPLINE_ELEVATION,
        ];
        let layout_flags2 = [
            0,
            MOVEMENT_FLAG2_ALWAYS_ALLOW_PITCHING,
            MOVEMENT_FLAG2_INTERPOLATED_MOVEMENT,
            MOVEMENT_FLAG2_ALWAYS_ALLOW_PITCHING | MOVEMENT_FLAG2_INTERPOLATED_MOVEMENT,
        ];

        for flags in layout_flags {
            for flags2 in layout_flags2 {
                let original = sample(flags, flags2);
                let mut bytes = Vec::new();
                original.write(&mut bytes);

                let mut reader = Reader::new(&bytes, "movement");
                let parsed = MovementInfo::read(&mut reader).unwrap();
                reader
                    .finish()
                    .unwrap_or_else(|e| panic!("flags {flags:#x}/{flags2:#x}: {e}"));

                // Only the parts the flags call for survive, so compare against
                // what a correct writer would have kept rather than the input.
                let mut expected = original;
                if flags & movement_flags::ON_TRANSPORT == 0 {
                    expected.transport = None;
                } else if flags2 & MOVEMENT_FLAG2_INTERPOLATED_MOVEMENT == 0 {
                    expected.transport.as_mut().unwrap().interpolated_time = None;
                }
                if !MovementInfo::has_pitch(flags, flags2) {
                    expected.pitch = None;
                }
                if flags & movement_flags::FALLING == 0 {
                    expected.falling = None;
                }
                if flags & movement_flags::SPLINE_ELEVATION == 0 {
                    expected.spline_elevation = None;
                }
                assert_eq!(parsed, expected, "flags {flags:#x}/{flags2:#x}");
            }
        }
    }

    /// The minimum packet, pinned against the layout rather than against the
    /// code: four bytes of flags, two of flags2, four of time, four floats of
    /// position, four of fall time.
    #[test]
    fn the_simplest_movement_is_thirty_bytes() {
        let info = MovementInfo::standing(Position::default(), 0);
        assert_eq!(info.encoded_len(), 4 + 2 + 4 + 16 + 4);
        assert_eq!(info.encoded_len(), 30);
    }

    /// Each optional part adds exactly its own width and nothing else.
    #[test]
    fn optional_parts_cost_what_the_layout_says() {
        let base = MovementInfo::standing(Position::default(), 0).encoded_len();

        let mut swimming = MovementInfo::standing(Position::default(), 0);
        swimming.flags = movement_flags::SWIMMING;
        assert_eq!(swimming.encoded_len(), base + 4, "pitch");

        let mut falling = MovementInfo::standing(Position::default(), 0);
        falling.flags = movement_flags::FALLING;
        assert_eq!(falling.encoded_len(), base + 16, "jump velocity and angles");

        let mut elevated = MovementInfo::standing(Position::default(), 0);
        elevated.flags = movement_flags::SPLINE_ELEVATION;
        assert_eq!(elevated.encoded_len(), base + 4, "spline elevation");
    }

    /// The optional parts follow the flags, not the fields. A populated struct
    /// with the flag clear must not emit it, or the reader loses its place.
    #[test]
    fn unflagged_fields_are_not_written() {
        let mut info = sample(0, 0);
        info.flags = 0;
        info.flags2 = 0;
        assert_eq!(
            info.encoded_len(),
            30,
            "a populated struct emitted fields its flags did not claim"
        );

        // And the converse: a flag set with the field absent still emits, so
        // the packet stays parseable.
        let mut claimed = MovementInfo::standing(Position::default(), 0);
        claimed.flags = movement_flags::FALLING;
        claimed.falling = None;
        assert_eq!(claimed.encoded_len(), 30 + 16);
        let mut bytes = Vec::new();
        claimed.write(&mut bytes);
        let mut reader = Reader::new(&bytes, "movement");
        assert!(MovementInfo::read(&mut reader).is_ok());
        assert!(reader.finish().is_ok());
    }

    /// Position must survive exactly: it is the whole point of the packet, and
    /// a rounded coordinate is a character standing somewhere else.
    #[test]
    fn the_position_is_not_disturbed() {
        let info = MovementInfo::standing(
            Position {
                x: -8949.951,
                y: -132.4907,
                z: 83.53125,
                orientation: 5.7014,
            },
            1,
        );
        let mut bytes = Vec::new();
        info.write(&mut bytes);
        let mut reader = Reader::new(&bytes, "movement");
        let parsed = MovementInfo::read(&mut reader).unwrap();
        assert_eq!(parsed.position, info.position);
    }
}
