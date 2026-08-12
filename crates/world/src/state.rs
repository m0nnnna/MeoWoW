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
    /// How long the move to `destination` takes, in milliseconds. `None`
    /// exactly when `destination` is `None`.
    pub move_duration: Option<u32>,
    /// When this client received the move that set `destination`. Paired
    /// with `move_duration` to interpolate between `position` and
    /// `destination` -- see [`Entity::interpolated_position`].
    pub move_started: Option<std::time::Instant>,
    /// An explicit facing to arrive at, from the current move
    /// (`MonsterMove::facing`), consulted only once that move has fully
    /// arrived. Storing it anywhere read *during* the move does not work: an
    /// active `SMSG_MONSTER_MOVE` is interpolated by direction of travel
    /// regardless, so a value read mid-flight is simply never seen. This has
    /// to live as its own field, checked only when `t >= 1.0`.
    pub arrival_facing: Option<f32>,
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

    /// Clears any monster-move path in flight.
    ///
    /// Called whenever a fresher, authoritative position arrives -- a relayed
    /// `MSG_MOVE_*`, an object update's own movement block, or a re-create.
    /// Any of those supersedes a path predicted from an older
    /// `SMSG_MONSTER_MOVE`; leaving the old destination in place would have
    /// the entity interpolate toward a point the server has already moved it
    /// away from.
    fn clear_predicted_move(&mut self) {
        self.destination = None;
        self.move_duration = None;
        self.move_started = None;
        self.arrival_facing = None;
    }

    /// Where this entity actually is right now, interpolated along a monster
    /// move's path if one is in flight, clamped to the endpoints.
    ///
    /// The wire only ever reports a path's start, its end, and how long the
    /// whole thing takes -- nothing between arrives from the server, which is
    /// why `position` alone (the path's start) makes a replicated creature
    /// jump instead of walk. `now` is supplied by the caller rather than read
    /// from the clock here, so the interpolation math is exercised by a fixed
    /// input rather than a hopefully-fast-enough sleep in tests.
    ///
    /// Facing has two regimes, computed here and nowhere else so neither can
    /// be bypassed: while still travelling (`t < 1.0`), it is the direction
    /// of travel -- `from`/`to` never carry their own orientation, the wire
    /// not reporting a *starting* facing. Once arrived (`t >= 1.0`, which a
    /// zero-duration move reaches immediately), `arrival_facing` takes over
    /// if the wire supplied one, falling back to the same direction-of-travel
    /// computation otherwise -- never to either endpoint's own hardcoded-zero
    /// orientation, which a duration of exactly zero would otherwise expose
    /// by returning `end` verbatim.
    pub fn interpolated_position(&self, now: std::time::Instant) -> Option<Position> {
        let (Some(start), Some(end), Some(duration), Some(started)) = (
            self.position,
            self.destination,
            self.move_duration,
            self.move_started,
        ) else {
            return self.position;
        };

        let t = if duration == 0 {
            1.0
        } else {
            let elapsed = now.saturating_duration_since(started).as_millis() as f32;
            (elapsed / duration as f32).clamp(0.0, 1.0)
        };

        let direction_of_travel = || (end.y - start.y).atan2(end.x - start.x);
        let orientation = if t >= 1.0 {
            self.arrival_facing.unwrap_or_else(direction_of_travel)
        } else {
            direction_of_travel()
        };

        Some(Position {
            x: start.x + (end.x - start.x) * t,
            y: start.y + (end.y - start.y) * t,
            z: start.z + (end.z - start.z) * t,
            orientation,
        })
    }

    /// Whether a monster move is still in flight at `now`, as opposed to
    /// having already arrived.
    ///
    /// `destination.is_some()` alone cannot answer this: it stays set to the
    /// last move's endpoint until a fresher update arrives, which for a
    /// creature that has stopped moving may be a long time -- checking only
    /// that would report a creature "moving" forever after its last move
    /// rather than for that move's actual duration, which is exactly the
    /// "every creature plays its walk cycle standing still" bug this method
    /// exists to let a caller avoid.
    pub fn is_moving(&self, now: std::time::Instant) -> bool {
        let (Some(duration), Some(started)) = (self.move_duration, self.move_started) else {
            return false;
        };
        now.saturating_duration_since(started).as_millis() < duration as u128
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
                existing.clear_predicted_move();
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
                move_duration: None,
                move_started: None,
                arrival_facing: None,
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
            entity.clear_predicted_move();
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
    /// every creature in the zone jump. `move_duration`, `move_started` and
    /// `arrival_facing` carry enough to interpolate between the two and to
    /// face the right way once there -- see [`Entity::interpolated_position`]
    /// -- rather than only ever showing the start. A stop (`move_.to` is
    /// `None`) clears all three the same way a fresher authoritative position
    /// does.
    ///
    /// `entity.position.orientation` is set here too, but only for the
    /// *next* time `destination` goes back to `None` and `interpolated_position`
    /// falls through to it verbatim -- while this move is in flight the value
    /// set below is never read, since `interpolated_position` computes facing
    /// dynamically from `arrival_facing` and the direction of travel, not
    /// from this field. `move_.from.orientation` itself is never usable
    /// directly: the wire never reports a *starting* orientation, so the
    /// parser always hands back zero there, and using it verbatim would turn
    /// every stop into "snap to face east." The best available guess is
    /// whatever the entity was already facing a moment ago -- mid-interpolation
    /// if a path was in flight, its last resting facing otherwise.
    pub fn apply_monster_move(&mut self, move_: &MonsterMove) {
        let Some(entity) = self.entities.get_mut(&move_.guid) else {
            self.stats.orphaned += 1;
            return;
        };
        self.stats.movement_updates += 1;
        entity.updates += 1;

        let orientation = move_.facing.unwrap_or_else(|| {
            entity
                .interpolated_position(std::time::Instant::now())
                .map(|p| p.orientation)
                .unwrap_or(0.0)
        });
        entity.position = Some(Position {
            orientation,
            ..move_.from
        });
        entity.destination = move_.to;
        entity.move_duration = move_.to.map(|_| move_.duration);
        entity.move_started = move_.to.map(|_| std::time::Instant::now());
        entity.arrival_facing = move_.facing;
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
            facing: None,
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

    /// The whole point of interpolation: read partway through a move, the
    /// entity should be partway along the path, not still at its start or
    /// already at its end.
    ///
    /// `now` is derived from the entity's own `move_started` rather than a
    /// freshly captured `Instant::now()`, so the test is not racing the
    /// `Instant::now()` call inside `apply_monster_move` -- a few
    /// microseconds either side of that race would silently change which
    /// instant is earlier and make this test flaky for a reason that has
    /// nothing to do with the interpolation math being tested.
    #[test]
    fn interpolated_position_lerps_between_endpoints_and_faces_the_direction_of_travel() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: Some(at(100.0, 0.0)),
            duration: 4000,
            stopped: false,
            facing: None,
        });

        let entity = world.get(7).unwrap();
        let started = entity.move_started.expect("a move with a destination must record when it started");

        let halfway = entity
            .interpolated_position(started + std::time::Duration::from_millis(2000))
            .unwrap();
        assert!(
            (halfway.x - 50.0).abs() < 0.01,
            "halfway through a 4s move along x should be near x=50, got {}",
            halfway.x
        );
        assert_eq!(halfway.y, 0.0);
        assert_eq!(
            halfway.orientation, 0.0,
            "travelling straight along +x should face angle 0"
        );
    }

    /// Before a move starts and after it should have finished, the position
    /// must clamp to the endpoints rather than extrapolate past them or sit
    /// at zero progress forever.
    #[test]
    fn interpolated_position_clamps_at_both_ends() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: Some(at(100.0, 0.0)),
            duration: 4000,
            stopped: false,
            facing: None,
        });

        let entity = world.get(7).unwrap();
        let started = entity.move_started.unwrap();

        let before = entity
            .interpolated_position(started - std::time::Duration::from_millis(500))
            .unwrap();
        assert_eq!(before.x, 0.0, "must not run backwards past the start");

        let after = entity
            .interpolated_position(started + std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(after.x, 100.0, "must clamp at the destination, not overshoot");
    }

    /// The gap a review of the first version of this code found: `facing`
    /// was parsed and stored, but nothing ever read it back, because
    /// `interpolated_position` computed direction of travel unconditionally
    /// -- at every `t`, including `t == 1.0`. An entity that had "arrived"
    /// kept reporting the heading it walked in on forever, and the wire's
    /// own arrival hint was silently discarded after being parsed.
    #[test]
    fn arrival_facing_takes_over_once_the_move_completes_but_not_before() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: Some(at(100.0, 0.0)), // due "east": direction of travel is 0 rad
            duration: 4000,
            stopped: false,
            facing: Some(2.5), // deliberately unrelated to the direction of travel
        });

        let entity = world.get(7).unwrap();
        let started = entity.move_started.unwrap();

        let mid = entity
            .interpolated_position(started + std::time::Duration::from_millis(2000))
            .unwrap();
        assert_eq!(
            mid.orientation, 0.0,
            "still en route: direction of travel, not the arrival hint"
        );

        let arrived = entity
            .interpolated_position(started + std::time::Duration::from_millis(4000))
            .unwrap();
        assert_eq!(
            arrived.orientation, 2.5,
            "arrived: the wire's arrival facing must actually take effect"
        );

        let long_after = entity
            .interpolated_position(started + std::time::Duration::from_secs(30))
            .unwrap();
        assert_eq!(long_after.orientation, 2.5, "and stay in effect, not just at the instant of arrival");
    }

    /// A duration of exactly zero reaches `t >= 1.0` immediately, so it has
    /// to go through the same arrival-facing logic as any other completed
    /// move -- not a separate early return that hands back the endpoint's
    /// own orientation, which the wire always reports as zero regardless of
    /// which way the creature actually ended up facing.
    #[test]
    fn a_zero_duration_move_prefers_the_arrival_facing_over_the_endpoint_default() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 5.0)), &[])]);
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 5.0),
            to: Some(at(0.0, 5.0)),
            duration: 0,
            stopped: false,
            facing: Some(1.1),
        });

        let entity = world.get(7).unwrap();
        let position = entity
            .interpolated_position(std::time::Instant::now())
            .unwrap();
        assert_eq!(position.orientation, 1.1);
    }

    /// And without a hint, a zero-duration move still must not fall back to
    /// the endpoint's hardcoded-zero orientation -- direction of travel is
    /// the same fallback a normal move gets.
    #[test]
    fn a_zero_duration_move_without_a_hint_faces_the_destination() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: Some(at(0.0, 100.0)), // due "north"
            duration: 0,
            stopped: false,
            facing: None,
        });

        let entity = world.get(7).unwrap();
        let position = entity
            .interpolated_position(std::time::Instant::now())
            .unwrap();
        assert!(
            (position.orientation - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
            "got {}",
            position.orientation
        );
    }

    /// An authoritative position -- here, relayed movement, but the same
    /// applies to an object update's own movement block or a re-create --
    /// must cancel a monster-move path in flight. Otherwise the entity
    /// interpolates back toward a destination the server has already
    /// contradicted, fighting the newer, truer position every frame.
    #[test]
    fn an_authoritative_position_clears_a_predicted_move() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: Some(at(100.0, 0.0)),
            duration: 4000,
            stopped: false,
            facing: Some(2.5),
        });
        assert!(world.get(7).unwrap().destination.is_some());
        assert!(world.get(7).unwrap().arrival_facing.is_some());

        let info = MovementInfo::standing(at(3.0, 4.0), 1);
        world.apply_relayed_movement(7, &info);

        let entity = world.get(7).unwrap();
        assert!(
            entity.destination.is_none(),
            "the predicted path must not survive a fresher authoritative position"
        );
        assert!(entity.move_duration.is_none());
        assert!(entity.move_started.is_none());
        assert!(
            entity.arrival_facing.is_none(),
            "a stale arrival hint must not survive a fresher authoritative position either"
        );
        assert_eq!(
            entity.interpolated_position(std::time::Instant::now()),
            Some(at(3.0, 4.0)),
            "with no path in flight, the position must be exactly what was reported"
        );
    }

    /// The bug `is_moving` exists to prevent: `destination` alone stays set
    /// long after a move has actually arrived, so a caller checking
    /// `destination.is_some()` to decide whether to play a walk animation
    /// would keep a creature walking in place forever after its one and only
    /// move -- observed live as "every creature plays its walk cycle
    /// standing still, and nothing ever goes idle."
    #[test]
    fn is_moving_is_true_only_for_the_moves_actual_duration() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: Some(at(100.0, 0.0)),
            duration: 4000,
            stopped: false,
            facing: None,
        });

        let entity = world.get(7).unwrap();
        let started = entity.move_started.unwrap();

        assert!(
            entity.is_moving(started + std::time::Duration::from_millis(2000)),
            "halfway through a 4s move, the creature is still moving"
        );
        assert!(
            !entity.is_moving(started + std::time::Duration::from_secs(30)),
            "long after the move should have arrived, it must not still read as moving, \
             even though destination is still set to the old target"
        );
        assert!(entity.destination.is_some(), "the premise: destination outlives the move");
    }

    /// A stop (`to: None`) must read as not moving, the same as a move that
    /// has simply finished.
    #[test]
    fn a_stopped_creature_is_not_moving() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: None,
            duration: 0,
            stopped: true,
            facing: None,
        });

        assert!(!world.get(7).unwrap().is_moving(std::time::Instant::now()));
    }
}
