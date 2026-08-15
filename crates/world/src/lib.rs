//! The 3.3.5a world server protocol.
//!
//! Where [`auth`] proves who you are and hands back a session key, this crate
//! spends that key: it completes the world handshake, starts the RC4 header
//! cipher the rest of the connection depends on, and reads the account's
//! characters.
//!
//! Written from public protocol documentation. Nothing here is derived from a
//! GPL server implementation -- see `docs/REUSE-POLICY.md`.

pub mod chat;
pub mod combat;
pub mod client;
pub mod crypt;
pub mod death;
pub mod motion;
pub mod movement;
pub mod names;
pub mod opcode;
pub mod protocol;
pub mod query;
pub mod spell;
pub mod state;
pub mod update;
pub mod weather;

pub use chat::{ChatMessage, ChatType};
pub use client::{Connection, DEFAULT_PORT};
pub use crypt::HeaderCrypt;
pub use movement::MovementInfo;
pub use names::Names;
pub use opcode::ClientOpcode;
pub use query::{CreatureInfo, PlayerName};
pub use spell::{InitialSpells, KnownSpell};
pub use protocol::{
    Appearance, AuthResponse, Character, Equipment, WorldPosition, EQUIPMENT_SLOTS,
};
pub use state::{Cast, Replication, WorldState};
pub use update::{Block, MonsterMove, ObjectType, Position};
pub use weather::{Precipitation, Weather, WeatherChange};

/// Names the playable races by id.
///
/// These live here rather than being read from `ChrRaces.dbc` on purpose: the
/// character list arrives over the network, and needing a game installation to
/// print a race name would make the protocol tools useless without one. The
/// ids are fixed for 3.3.5a.
pub fn race_name(race: u8) -> &'static str {
    match race {
        1 => "Human",
        2 => "Orc",
        3 => "Dwarf",
        4 => "Night Elf",
        5 => "Undead",
        6 => "Tauren",
        7 => "Gnome",
        8 => "Troll",
        10 => "Blood Elf",
        11 => "Draenei",
        _ => "unknown race",
    }
}

/// Names the classes by id, on the same reasoning as [`race_name`].
pub fn class_name(class: u8) -> &'static str {
    match class {
        1 => "Warrior",
        2 => "Paladin",
        3 => "Hunter",
        4 => "Rogue",
        5 => "Priest",
        6 => "Death Knight",
        7 => "Shaman",
        8 => "Mage",
        9 => "Warlock",
        11 => "Druid",
        _ => "unknown class",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two gaps in the id ranges are real, not transcription errors: 9 was
    /// Goblin, cut before release, and class 10 was a never-shipped class.
    /// Pinning them stops a later "fix" from shifting everything after.
    #[test]
    fn the_id_gaps_are_deliberate() {
        assert_eq!(race_name(9), "unknown race");
        assert_eq!(race_name(10), "Blood Elf");
        assert_eq!(class_name(10), "unknown class");
        assert_eq!(class_name(11), "Druid");
    }

    #[test]
    fn the_wrath_additions_are_present() {
        assert_eq!(class_name(6), "Death Knight");
        assert_eq!(race_name(11), "Draenei");
    }
}
