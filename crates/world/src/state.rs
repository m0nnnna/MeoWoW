//! A replicated view of the world.
//!
//! The server never sends the world; it sends *changes* to it. An object
//! arrives once as a create block carrying everything, and from then on only
//! what altered: a `Values` block with three fields, a movement packet with a
//! position, a guid in an out-of-range list. Reconstructing the world means
//! folding that stream into state and keeping it there.
//!
//! Which makes this the first place in the project where a mistake **survives**.
//! Every parser so far was memoryless: a bad packet produced a bad answer once,
//! and the next packet was unaffected. Here a dropped update is permanent, a
//! merge that overwrites instead of merging quietly erases fields nothing will
//! resend, and a removal that misses leaves a ghost standing where nothing is.
//! None of it raises an error, and all of it compounds.
//!
//! The defences are therefore about *accounting* rather than parsing: every
//! change is counted, unknown guids are tracked rather than silently created,
//! and [`WorldState::stats`] is meant to be looked at. A replication bug shows
//! up as a number that does not add up long before it shows up as a wrong
//! world.

use std::collections::HashMap;

use crate::client::Packet;
use crate::movement::MovementInfo;
use crate::update::{Block, Fields, MonsterMove, ObjectType, Position};

/// One object as this client currently believes it to be.
#[derive(Debug, Clone)]
pub struct Entity {
    pub guid: u64,
    pub object_type: ObjectType,
    pub position: Option<Position>,
    /// Every field seen so far, with later updates folded over earlier ones.
    pub fields: Fields,
    pub movement: Option<MovementInfo>,
    /// Where a `SMSG_MONSTER_MOVE` says this creature is heading, if anywhere.
    pub destination: Option<Position>,
    /// How many updates of any kind have touched this object.
    pub updates: usize,
}

impl Entity {
    pub fn level(&self) -> Option<u32> {
        self.fields.get(crate::update::fields::UNIT_LEVEL)
    }

    pub fn display_id(&self) -> Option<u32> {
        self.fields.get(crate::update::fields::UNIT_DISPLAY_ID)
    }

    pub fn health(&self) -> Option<u32> {
        self.fields.get(crate::update::fields::UNIT_HEALTH)
    }

    pub fn is_player(&self) -> bool {
        self.object_type == ObjectType::Player
    }
}

/// Counters that make replication errors visible before they become wrong
/// worlds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub created: usize,
    /// Create blocks naming a guid already present. Normal in moderation --
    /// objects leave and re-enter view -- but a flood means removals are being
    /// missed.
    pub recreated: usize,
    pub value_updates: usize,
    pub movement_updates: usize,
    pub removed: usize,
    /// Updates naming a guid never created. Each one is a change applied to
    /// nothing, and a rising count means create blocks are being lost.
    pub orphaned: usize,
}

/// What one batch of packets did to a [`WorldState`].
///
/// Returned by [`WorldState::replicate`] rather than folded silently, because
/// a caller checking that replication is actually working needs to see it:
/// zero failures across hundreds of applied changes is the evidence, not an
/// assumption.
#[derive(Debug, Default)]
pub struct Replication {
    pub object_updates: usize,
    pub monster_moves: usize,
    pub relayed_moves: usize,
    pub destroys: usize,
    /// Packets that would not decode, with their payload for offline analysis.
    pub failures: Vec<(u16, crate::protocol::Error, Result<Vec<u8>, crate::protocol::Error>)>,
}

#[derive(Debug, Default)]
pub struct WorldState {
    entities: HashMap<u64, Entity>,
    stats: Stats,
}

impl WorldState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    pub fn get(&self, guid: u64) -> Option<&Entity> {
        self.entities.get(&guid)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    pub fn players(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values().filter(|e| e.is_player())
    }

    /// Folds one object-update packet's blocks into the world.
    pub fn apply(&mut self, blocks: &[Block]) {
        for block in blocks {
            match block {
                Block::Create {
                    guid,
                    object_type,
                    movement,
                    fields,
                    ..
                } => self.create(*guid, *object_type, movement, fields),
                Block::Values { guid, fields } => self.update_values(*guid, fields),
                Block::Movement { guid, movement } => {
                    self.update_movement(*guid, movement.position, movement.info)
                }
                Block::OutOfRange { guids } => {
                    for guid in guids {
                        self.remove(*guid);
                    }
                }
                // Not a removal: these are objects the client is told are
                // nearby, which it already knows about.
                Block::NearObjects { .. } => {}
            }
        }
    }

    fn create(
        &mut self,
        guid: u64,
        object_type: ObjectType,
        movement: &crate::update::Movement,
        fields: &Fields,
    ) {
        if let Some(existing) = self.entities.get_mut(&guid) {
            // Re-entering view. Merge rather than replace: a re-create can
            // legitimately carry fewer fields than the original.
            self.stats.recreated += 1;
            existing.updates += 1;
            existing.object_type = object_type;
            existing.fields.merge(fields);
            if movement.position.is_some() {
                existing.position = movement.position;
            }
            if movement.info.is_some() {
                existing.movement = movement.info;
            }
            return;
        }

        self.stats.created += 1;
        self.entities.insert(
            guid,
            Entity {
                guid,
                object_type,
                position: movement.position,
                fields: fields.clone(),
                movement: movement.info,
                destination: None,
                updates: 1,
            },
        );
    }

    fn update_values(&mut self, guid: u64, fields: &Fields) {
        let Some(entity) = self.entities.get_mut(&guid) else {
            // A change to something never created. Counted rather than
            // fabricated: inventing an entity here would paper over a lost
            // create block and put an object with no type or position into the
            // world.
            self.stats.orphaned += 1;
            return;
        };
        self.stats.value_updates += 1;
        entity.updates += 1;
        entity.fields.merge(fields);
    }

    fn update_movement(
        &mut self,
        guid: u64,
        position: Option<Position>,
        info: Option<MovementInfo>,
    ) {
        let Some(entity) = self.entities.get_mut(&guid) else {
            self.stats.orphaned += 1;
            return;
        };
        self.stats.movement_updates += 1;
        entity.updates += 1;
        if position.is_some() {
            entity.position = position;
        }
        if info.is_some() {
            entity.movement = info;
        }
    }

    /// Applies a relayed `MSG_MOVE_*` from another mover.
    pub fn apply_relayed_movement(&mut self, guid: u64, info: &MovementInfo) {
        self.update_movement(guid, Some(info.position), Some(*info));
    }

    /// Applies a creature's path.
    ///
    /// The creature is placed at the path's *start*, not its end: the packet
    /// describes movement about to happen over `duration` milliseconds, and
    /// teleporting it to the destination on arrival of the packet would make
    /// every creature in the zone jump.
    pub fn apply_monster_move(&mut self, move_: &MonsterMove) {
        let Some(entity) = self.entities.get_mut(&move_.guid) else {
            self.stats.orphaned += 1;
            return;
        };
        self.stats.movement_updates += 1;
        entity.updates += 1;
        entity.position = Some(move_.from);
        entity.destination = move_.to;
    }

    pub fn remove(&mut self, guid: u64) -> bool {
        if self.entities.remove(&guid).is_some() {
            self.stats.removed += 1;
            true
        } else {
            false
        }
    }

    /// Folds every packet that carries world state from one drained batch.
    ///
    /// One place rather than one per caller, because replication only works
    /// if *all* of the inputs are applied: object updates create and change
    /// things, relayed movement moves other players, monster moves move
    /// creatures, and destroys remove them. Living here rather than being
    /// reimplemented per caller is not just deduplication -- two independent
    /// opcode dispatch tables over the same state machine drift apart
    /// silently: a new opcode wired into one and not the other freezes
    /// whatever it should have moved, in exactly the "looked plausible and
    /// was quietly frozen" way this method exists to prevent, just one level
    /// up. A packet that fails to decode costs only itself and is counted in
    /// [`Replication::failures`] rather than treated as fatal or dropped
    /// silently -- a removal that fails to decode is a permanent ghost if
    /// nothing counts it, no less than a create block that goes missing.
    ///
    /// `blocks_out`, when given, collects every block from a successfully
    /// parsed object update in addition to folding it into `self` -- for
    /// callers that want to report create/out-of-range detail beyond what
    /// state itself tracks (`wow-cli` does). Callers that do not care pass
    /// `None` and pay nothing for the fields they ignore.
    pub fn replicate(
        &mut self,
        packets: &[Packet],
        mut blocks_out: Option<&mut Vec<Block>>,
    ) -> Replication {
        use crate::update;

        let mut report = Replication::default();

        for packet in packets {
            match packet.opcode {
                crate::opcode::server::UPDATE_OBJECT
                | crate::opcode::server::COMPRESSED_UPDATE_OBJECT => {
                    let compressed =
                        packet.opcode == crate::opcode::server::COMPRESSED_UPDATE_OBJECT;
                    let parsed = if compressed {
                        update::parse_compressed_update_object(&packet.body)
                    } else {
                        update::parse_update_object(&packet.body)
                    };
                    match parsed {
                        Ok(blocks) => {
                            report.object_updates += 1;
                            self.apply(&blocks);
                            if let Some(out) = blocks_out.as_deref_mut() {
                                out.extend(blocks);
                            }
                        }
                        Err(error) => {
                            let payload = if compressed {
                                update::decompress_update_object(&packet.body)
                            } else {
                                Ok(packet.body.clone())
                            };
                            report.failures.push((packet.opcode, error, payload));
                        }
                    }
                }
                crate::opcode::server::MONSTER_MOVE => {
                    match update::parse_monster_move(&packet.body) {
                        Ok(moved) => {
                            report.monster_moves += 1;
                            self.apply_monster_move(&moved);
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                crate::opcode::server::MOVE_START_FORWARD
                | crate::opcode::server::MOVE_STOP
                | crate::opcode::server::MOVE_HEARTBEAT => {
                    match crate::protocol::parse_movement(&packet.body) {
                        Ok((mover, info)) => {
                            report.relayed_moves += 1;
                            self.apply_relayed_movement(mover, &info);
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                crate::opcode::server::DESTROY_OBJECT => {
                    let mut reader =
                        crate::protocol::Reader::new(&packet.body, "SMSG_DESTROY_OBJECT");
                    match update::read_packed_guid(&mut reader) {
                        Ok(guid) => {
                            report.destroys += 1;
                            self.remove(guid);
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                _ => {}
            }
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Movement;

    /// One raw `SMSG_UPDATE_OBJECT` body: a single create block for a unit,
    /// with the given guid and fields. Split out from `fields` below so
    /// `replicate`'s tests can feed the bytes to a `Packet` directly instead
    /// of only ever going in through `parse_update_object` -- `replicate` is
    /// exactly the boundary that turns bytes into applied state, and that
    /// boundary needs bytes to test through, not pre-parsed blocks.
    fn update_object_body(guid: u64, entries: &[(u16, u32)]) -> Vec<u8> {
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

        let mut packet = 1u32.to_le_bytes().to_vec();
        packet.push(2); // create
        crate::update::write_packed_guid(guid, &mut packet);
        packet.push(3); // unit
        packet.extend_from_slice(&0u16.to_le_bytes()); // no update flags
        packet.extend_from_slice(&body);
        packet
    }

    fn fields(entries: &[(u16, u32)]) -> Fields {
        // Built through the real parser so the sort order this relies on is
        // the parser's, not the test's idea of it.
        let packet = update_object_body(1, entries);
        match crate::update::parse_update_object(&packet).unwrap().remove(0) {
            Block::Create { fields, .. } => fields,
            other => panic!("wrong block: {other:?}"),
        }
    }

    fn create(guid: u64, object_type: ObjectType, at: Option<Position>, f: &[(u16, u32)]) -> Block {
        Block::Create {
            guid,
            object_type,
            spawned: false,
            movement: Movement {
                position: at,
                ..Movement::default()
            },
            fields: fields(f),
        }
    }

    fn at(x: f32, y: f32) -> Position {
        Position {
            x,
            y,
            z: 0.0,
            orientation: 0.0,
        }
    }

    #[test]
    fn creating_then_reading_back() {
        let mut world = WorldState::new();
        world.apply(&[create(
            7,
            ObjectType::Unit,
            Some(at(1.0, 2.0)),
            &[
                (crate::update::fields::UNIT_LEVEL, 12),
                (crate::update::fields::UNIT_DISPLAY_ID, 49),
            ],
        )]);

        assert_eq!(world.len(), 1);
        let entity = world.get(7).expect("created");
        assert_eq!(entity.level(), Some(12));
        assert_eq!(entity.display_id(), Some(49));
        assert_eq!(entity.position, Some(at(1.0, 2.0)));
        assert_eq!(world.stats().created, 1);
    }

    /// The central property of replication: a `Values` block carries only what
    /// changed, so applying it must not disturb anything it does not name.
    ///
    /// Getting this wrong is invisible in the packet -- the update parses,
    /// the field it carries is right -- and shows up much later as an object
    /// that has lost attributes nothing will ever resend.
    #[test]
    fn a_values_update_merges_rather_than_replaces() {
        let mut world = WorldState::new();
        world.apply(&[create(
            7,
            ObjectType::Unit,
            Some(at(1.0, 2.0)),
            &[
                (crate::update::fields::UNIT_LEVEL, 12),
                (crate::update::fields::UNIT_HEALTH, 100),
                (crate::update::fields::UNIT_DISPLAY_ID, 49),
            ],
        )]);

        // Only health changes.
        world.apply(&[Block::Values {
            guid: 7,
            fields: fields(&[(crate::update::fields::UNIT_HEALTH, 60)]),
        }]);

        let entity = world.get(7).unwrap();
        assert_eq!(entity.health(), Some(60), "the change did not apply");
        assert_eq!(entity.level(), Some(12), "level was erased by the merge");
        assert_eq!(
            entity.display_id(),
            Some(49),
            "display id was erased by the merge"
        );
        assert_eq!(entity.position, Some(at(1.0, 2.0)), "position was disturbed");
        assert_eq!(world.stats().value_updates, 1);
    }

    #[test]
    fn out_of_range_removes() {
        let mut world = WorldState::new();
        world.apply(&[
            create(7, ObjectType::Unit, Some(at(1.0, 2.0)), &[]),
            create(8, ObjectType::Player, Some(at(3.0, 4.0)), &[]),
        ]);
        world.apply(&[Block::OutOfRange { guids: vec![7] }]);

        assert_eq!(world.len(), 1);
        assert!(world.get(7).is_none());
        assert!(world.get(8).is_some());
        assert_eq!(world.stats().removed, 1);
    }

    /// An update for something never created must be counted, not invented.
    ///
    /// Fabricating the entity would hide a lost create block and put an object
    /// with no type and no position into the world, which is worse than not
    /// having it at all.
    #[test]
    fn updates_for_unknown_objects_are_counted_not_invented() {
        let mut world = WorldState::new();
        world.apply(&[Block::Values {
            guid: 99,
            fields: fields(&[(crate::update::fields::UNIT_HEALTH, 5)]),
        }]);

        assert!(world.is_empty(), "an update conjured an entity");
        assert_eq!(world.stats().orphaned, 1);
        assert_eq!(world.stats().value_updates, 0);
    }

    /// Re-entering view is normal and must not duplicate or reset the object.
    #[test]
    fn recreating_merges_and_is_counted_separately() {
        let mut world = WorldState::new();
        world.apply(&[create(
            7,
            ObjectType::Unit,
            Some(at(1.0, 2.0)),
            &[(crate::update::fields::UNIT_LEVEL, 12)],
        )]);
        world.apply(&[create(
            7,
            ObjectType::Unit,
            Some(at(9.0, 9.0)),
            &[(crate::update::fields::UNIT_HEALTH, 30)],
        )]);

        assert_eq!(world.len(), 1, "a re-create duplicated the object");
        let entity = world.get(7).unwrap();
        assert_eq!(entity.level(), Some(12), "the original fields were lost");
        assert_eq!(entity.health(), Some(30));
        assert_eq!(entity.position, Some(at(9.0, 9.0)));
        assert_eq!(world.stats().created, 1);
        assert_eq!(world.stats().recreated, 1);
    }

    #[test]
    fn relayed_movement_moves_the_right_object() {
        let mut world = WorldState::new();
        world.apply(&[
            create(7, ObjectType::Player, Some(at(0.0, 0.0)), &[]),
            create(8, ObjectType::Player, Some(at(5.0, 5.0)), &[]),
        ]);

        let info = MovementInfo::standing(at(10.0, 20.0), 1);
        world.apply_relayed_movement(7, &info);

        assert_eq!(world.get(7).unwrap().position, Some(at(10.0, 20.0)));
        assert_eq!(
            world.get(8).unwrap().position,
            Some(at(5.0, 5.0)),
            "the wrong object moved"
        );
    }

    /// A monster move describes travel about to happen. Placing the creature at
    /// the destination on arrival would make every creature in the zone jump.
    #[test]
    fn a_monster_move_starts_at_the_path_start() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);

        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(1.0, 1.0),
            to: Some(at(50.0, 50.0)),
            duration: 5000,
            stopped: false,
        });

        let entity = world.get(7).unwrap();
        assert_eq!(entity.position, Some(at(1.0, 1.0)), "creature teleported");
        assert_eq!(entity.destination, Some(at(50.0, 50.0)));
    }

    /// The counters have to add up, because they are the only thing that makes
    /// a replication bug visible before the world is wrong.
    #[test]
    fn the_stats_account_for_every_change() {
        let mut world = WorldState::new();
        world.apply(&[
            create(1, ObjectType::Unit, Some(at(0.0, 0.0)), &[]),
            create(2, ObjectType::Unit, Some(at(0.0, 0.0)), &[]),
            create(3, ObjectType::Player, Some(at(0.0, 0.0)), &[]),
        ]);
        world.apply(&[Block::Values {
            guid: 1,
            fields: fields(&[(crate::update::fields::UNIT_HEALTH, 1)]),
        }]);
        world.apply(&[Block::OutOfRange { guids: vec![2, 404] }]);

        let stats = world.stats();
        assert_eq!(stats.created, 3);
        assert_eq!(stats.value_updates, 1);
        assert_eq!(stats.removed, 1, "removing an unknown guid was counted");
        assert_eq!(world.len(), stats.created - stats.removed);
        assert_eq!(world.players().count(), 1);
    }

    /// `replicate` is the dispatch every other test in this file bypasses --
    /// they all call `apply`/`apply_monster_move`/`apply_relayed_movement`
    /// directly, so none of them would notice a bug in the opcode table
    /// itself. This is what would have caught a `DESTROY_OBJECT` decode
    /// failure being silently dropped instead of counted: with an
    /// `if let Ok(...)` in place of the current `match`, `failures.len()`
    /// here comes back `0` and the assertion fails, even though a healthy
    /// live-realm run would never exercise this path -- it only sends
    /// well-formed packets.
    #[test]
    fn replicate_applies_good_packets_and_counts_bad_ones() {
        use crate::client::Packet;

        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);

        let mut good_destroy = Vec::new();
        crate::update::write_packed_guid(7, &mut good_destroy);

        let packets = vec![
            // A valid create for a second object, so the good path is proven
            // alongside the bad one rather than merely surviving it.
            Packet {
                opcode: crate::opcode::server::UPDATE_OBJECT,
                body: update_object_body(8, &[(crate::update::fields::UNIT_LEVEL, 5)]),
            },
            // Truncated: a mask byte claiming every one of the eight guid
            // bytes is present, with none of them supplied.
            Packet {
                opcode: crate::opcode::server::DESTROY_OBJECT,
                body: vec![0xFF],
            },
            Packet {
                opcode: crate::opcode::server::DESTROY_OBJECT,
                body: good_destroy,
            },
        ];

        let report = world.replicate(&packets, None);

        assert_eq!(report.object_updates, 1);
        assert_eq!(report.destroys, 1);
        assert_eq!(
            report.failures.len(),
            1,
            "the truncated destroy must be counted, not dropped"
        );
        assert_eq!(report.failures[0].0, crate::opcode::server::DESTROY_OBJECT);

        assert!(world.get(7).is_none(), "the valid destroy did not apply");
        assert!(world.get(8).is_some(), "the valid create did not apply");
    }
}
