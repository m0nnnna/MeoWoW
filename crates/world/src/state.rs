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

    pub fn max_health(&self) -> Option<u32> {
        self.fields.get(crate::update::fields::UNIT_MAX_HEALTH)
    }

    /// Race, class, gender and power type, in that order.
    ///
    /// Four independent values packed into one 32-bit field, which is worth
    /// naming rather than unpacking at each call site: the byte order is the
    /// kind of detail that gets transcribed correctly once and then guessed at
    /// wrongly the second time.
    pub fn bytes_0(&self) -> Option<[u8; 4]> {
        self.fields
            .get(crate::update::fields::UNIT_BYTES_0)
            .map(u32::to_le_bytes)
    }

    pub fn race(&self) -> Option<u8> {
        self.bytes_0().map(|bytes| bytes[0])
    }

    pub fn class(&self) -> Option<u8> {
        self.bytes_0().map(|bytes| bytes[1])
    }

    pub fn gender(&self) -> Option<u8> {
        self.bytes_0().map(|bytes| bytes[2])
    }

    /// Which resource this unit spends: mana, rage, energy and so on.
    pub fn power_type(&self) -> Option<u8> {
        self.bytes_0().map(|bytes| bytes[3])
    }

    /// The unit's current power, read from the one field its power type
    /// selects.
    ///
    /// The seven power fields are a parallel array and only one of them means
    /// anything for a given unit. Reading `UNIT_POWER1` regardless -- the
    /// obvious shortcut, since it is right for every caster -- reports zero for
    /// every rogue and warrior in the world, which looks like a replication
    /// failure rather than the wrong field.
    pub fn power(&self) -> Option<u32> {
        self.fields
            .get(crate::update::fields::UNIT_POWER1 + self.power_index()?)
    }

    pub fn max_power(&self) -> Option<u32> {
        self.fields
            .get(crate::update::fields::UNIT_MAX_POWER1 + self.power_index()?)
    }

    /// The offset into the power arrays, refusing anything that would read
    /// past them.
    ///
    /// A power type this client has never heard of is a number straight off
    /// the wire, and adding it to a base index unchecked would read whatever
    /// field happens to live there -- reporting a unit's level as its mana
    /// rather than reporting nothing.
    fn power_index(&self) -> Option<u16> {
        let index = self.power_type()? as u16;
        (index < crate::update::fields::POWER_COUNT).then_some(index)
    }

    /// What this unit has targeted, if anything. Zero means nothing, and is
    /// reported as `None` rather than as a guid no object will ever have.
    pub fn target(&self) -> Option<u64> {
        self.fields
            .get_u64(crate::update::fields::UNIT_TARGET)
            .filter(|guid| *guid != 0)
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

    /// How fast this entity is travelling along the move in flight, in world
    /// units per second, or `None` when it is not travelling at all.
    ///
    /// Derived from the path rather than read off the wire, because the wire
    /// does not carry it: `SMSG_MONSTER_MOVE` gives two endpoints and a
    /// duration, and the speed fields in a unit's update block describe what it
    /// is *capable* of, which is not what it is doing -- a creature ambling
    /// home moves at a fraction of its run speed without either number
    /// changing. Distance over duration is the only statement about this
    /// particular move.
    ///
    /// A caller drawing this entity gets the same answer the position it draws
    /// was interpolated from, which is the point: an entity animated as running
    /// while it crosses a metre a second looks broken in a way neither value
    /// alone reveals.
    pub fn move_speed(&self, now: std::time::Instant) -> Option<f32> {
        if !self.is_moving(now) {
            return None;
        }
        let (Some(start), Some(end), Some(duration)) =
            (self.position, self.destination, self.move_duration)
        else {
            return None;
        };
        // `is_moving` already implies this, since nothing is less than zero
        // elapsed; belt and braces against a divide by zero that would produce
        // an infinity nothing downstream checks for.
        if duration == 0 {
            return None;
        }
        let distance = ((end.x - start.x).powi(2)
            + (end.y - start.y).powi(2)
            + (end.z - start.z).powi(2))
        .sqrt();
        Some(distance / (duration as f32 / 1000.0))
    }
}

/// A spell mid-cooldown, timed from when this client learned about it rather
/// than from the server's own clock, which the client never sees.
#[derive(Debug, Clone, Copy)]
pub struct Cooldown {
    started: std::time::Instant,
    duration_ms: u32,
}

impl Cooldown {
    /// `1.0` the instant a cooldown starts, `0.0` once `duration_ms` has
    /// fully elapsed. `now` is a parameter for the same reason
    /// `Entity::interpolated_position` takes one: fixed input, not a
    /// hopefully-fast-enough clock read inside the test.
    pub fn remaining_fraction(&self, now: std::time::Instant) -> f32 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        let elapsed = now.saturating_duration_since(self.started).as_millis() as f32;
        (1.0 - elapsed / self.duration_ms as f32).clamp(0.0, 1.0)
    }
}

/// A cast in progress, timed from when this client learned about it rather
/// than from the server's own clock -- same reasoning as [`Cooldown`].
///
/// A finished cast is harmless to leave in place: once `progress_fraction`
/// reaches `1.0` it reads as finished whether or not `SMSG_SPELL_GO` ever
/// arrives to say so explicitly, so a caller reading the fraction every frame
/// never needs the entry removed to draw the right thing. That matters
/// because [`SpellGo`](crate::spell::SpellGo) is refused for shapes this
/// parser has not confirmed live (a miss, an unrecognised target) -- see its
/// own doc comment -- so a `SMSG_SPELL_GO` this client cannot yet read must
/// not be able to leave a cast bar stuck forever.
///
/// **But harmless is not the same as free, and this map is not `Cooldown`.**
/// `cooldowns` is keyed by spell id and a character knows a few dozen spells,
/// so never pruning it is bounded by construction. `casts` is keyed by
/// *caster guid*: every creature that casts anything within visibility range
/// takes a slot, and the removals that would free them are exactly the ones
/// this crate declines to parse. So entries are dropped when the caster
/// leaves the world instead -- see [`WorldState::remove`] -- which is the
/// event that bounds the map, and which the "reads as finished anyway"
/// argument above says nothing about.
#[derive(Debug, Clone, Copy)]
pub struct Cast {
    pub spell_id: u32,
    started: std::time::Instant,
    pub duration_ms: u32,
}

impl Cast {
    /// `0.0` the instant a cast starts, `1.0` once `duration_ms` has fully
    /// elapsed. `now` is a parameter for the same reason
    /// `Cooldown::remaining_fraction` takes one.
    pub fn progress_fraction(&self, now: std::time::Instant) -> f32 {
        if self.duration_ms == 0 {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(self.started).as_millis() as f32;
        (elapsed / self.duration_ms as f32).clamp(0.0, 1.0)
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
    /// Names that arrived this batch, already folded into [`WorldState::names`].
    pub names: usize,
    /// Spells in the spellbook, if it arrived this batch.
    pub spells: usize,
    /// `SMSG_SPELL_START`s folded into [`WorldState::casts`] this batch.
    pub casts_started: usize,
    /// `SMSG_SPELL_GO`s that cleared an entry from [`WorldState::casts`] this
    /// batch -- an accounting counter, not a count of parsed packets: an
    /// instant-cast spell's `SMSG_SPELL_GO` arrives with no matching
    /// `SMSG_SPELL_START` and clears nothing, which is correct and not
    /// counted here.
    pub casts_landed: usize,
    /// Casts the server refused. Returned rather than stored for the same
    /// reason chat is: they are events, not state.
    pub cast_failures: Vec<crate::spell::CastFailed>,
    /// Chat received this batch.
    ///
    /// Handed back rather than stored: chat is a stream of events, not state,
    /// and the one thing a `WorldState` must never do is accumulate an
    /// unbounded log nobody drains. The caller owns the scrollback.
    pub chat: Vec<crate::chat::ChatMessage>,
    /// Melee swings landed or missed this batch, in the order they arrived.
    ///
    /// Handed back for the same reason as [`Self::chat`], and carrying the
    /// same hazard: this crate has now produced three categories a caller
    /// forgot to consume, and every one of them looked like the server not
    /// sending anything. A swing is an event -- what it *did* to a unit's
    /// health arrives separately, as an ordinary field update, and is already
    /// folded into the entity by the time this is read.
    pub swings: Vec<crate::combat::MeleeSwing>,
    /// `SMSG_POWER_UPDATE`s folded into an entity's fields this batch.
    pub power_updates: usize,
    /// Threat lists that arrived this batch. Returned rather than stored: no
    /// part of the interface reads a threat table yet, and keeping one would
    /// be state with a lifetime to manage and no consumer to justify it.
    pub threat: Vec<crate::combat::ThreatUpdate>,
    /// `SMSG_ATTACKSTART`s folded into [`WorldState::attacking`] this batch.
    pub attacks_started: usize,
    /// `SMSG_ATTACKSTOP`s that cleared an entry from
    /// [`WorldState::attacking`]. An accounting counter like
    /// [`Self::casts_landed`]: a stop for a fight this client never saw begin
    /// clears nothing and is not counted.
    pub attacks_stopped: usize,
    /// Packets that would not decode, with their payload for offline analysis.
    pub failures: Vec<(u16, crate::protocol::Error, Result<Vec<u8>, crate::protocol::Error>)>,
}

#[derive(Debug, Default)]
pub struct WorldState {
    entities: HashMap<u64, Entity>,
    stats: Stats,
    /// What this character knows how to cast. Arrives once, in the login
    /// burst, and is never resent.
    pub spells: crate::spell::InitialSpells,
    /// Spells currently on cooldown, keyed by spell id. Seeded from
    /// `SMSG_INITIAL_SPELLS`'s own cooldown list at login, then kept current
    /// by `SMSG_SPELL_COOLDOWN` as casts start new ones. A spell that has
    /// fully come off cooldown is left in place rather than pruned --
    /// `Cooldown::remaining_fraction` reads `0.0` for it either way, and there
    /// are at most a few dozen spells, so nothing is gained by removing it.
    pub cooldowns: HashMap<u32, Cooldown>,
    /// Casts in progress, keyed by caster guid. `SMSG_SPELL_START` inserts an
    /// entry; `SMSG_SPELL_GO` removes it. An instant-cast spell goes straight
    /// to `SMSG_SPELL_GO` with no bar ever worth showing, so removing an
    /// absent entry for one is a correct no-op, not a missed `SMSG_SPELL_START`.
    pub casts: HashMap<u64, Cast>,
    /// Who each unit is currently swinging at, keyed by attacker.
    /// `SMSG_ATTACKSTART` inserts, `SMSG_ATTACKSTOP` removes.
    ///
    /// Keyed by an unbounded thing -- a guid -- so it is cleared when the
    /// caster leaves the world too, the same as [`Self::casts`]. That is not a
    /// theoretical tidiness: a creature that dies mid-fight is destroyed
    /// rather than sending a stop for itself in every case, and the entry
    /// would otherwise claim forever that a corpse is still attacking.
    pub attacking: HashMap<u64, u64>,
    /// What things are called, and the bookkeeping that asks once.
    ///
    /// Lives here rather than beside the state because the answers arrive in
    /// the same packet stream that everything else does, and this crate's one
    /// standing rule about that stream is that it has exactly one dispatch
    /// table. A second one drifts, and a new opcode wired into one and not the
    /// other freezes whatever it should have moved.
    pub names: crate::names::Names,
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

    /// How much of a spell's cooldown remains, `0.0` if it is not on one at
    /// all.
    pub fn cooldown_fraction(&self, spell: u32, now: std::time::Instant) -> f32 {
        self.cooldowns
            .get(&spell)
            .map(|cooldown| cooldown.remaining_fraction(now))
            .unwrap_or(0.0)
    }

    /// The cast a given caster is in the middle of, if any -- `None` once
    /// `progress_fraction` would read `1.0`, not only once `SMSG_SPELL_GO`
    /// clears the entry. See [`Cast`]'s doc comment for why a caller must not
    /// need the packet to arrive for the bar to stop showing.
    pub fn active_cast(&self, caster: u64, now: std::time::Instant) -> Option<Cast> {
        let cast = *self.casts.get(&caster)?;
        (cast.progress_fraction(now) < 1.0).then_some(cast)
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
        // A cast belongs to whoever is casting it, so it leaves with them.
        // This is what bounds `casts`: its own removal path is a
        // `SMSG_SPELL_GO` that parses, and this crate deliberately refuses
        // that packet for shapes it has not confirmed, so entries for a
        // caster who walked out of range would otherwise accumulate for the
        // whole session. Nothing visible goes wrong without it -- a stale
        // entry reads as a finished cast -- which is exactly why it would
        // never have been noticed.
        self.casts.remove(&guid);
        // And any fight it was in, from both sides: a creature that dies is
        // destroyed rather than always sending a stop, so the entry naming it
        // as an attacker *and* the entries naming it as a victim would both
        // outlive it.
        self.attacking.remove(&guid);
        self.attacking.retain(|_, victim| *victim != guid);
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
                crate::opcode::server::NAME_QUERY_RESPONSE => {
                    match crate::query::parse_name_query_response(&packet.body) {
                        Ok(answer) => {
                            report.names += 1;
                            self.names.apply_player(&answer);
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                crate::opcode::server::CREATURE_QUERY_RESPONSE => {
                    match crate::query::parse_creature_query_response(&packet.body) {
                        Ok(answer) => {
                            report.names += 1;
                            self.names.apply_creature(&answer);
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                // A GM's line shares the body, so it shares the parser. It is
                // a separate opcode purely so a client can style it
                // differently.
                crate::opcode::server::MESSAGECHAT | crate::opcode::server::GM_MESSAGECHAT => {
                    match crate::chat::parse_message_chat(&packet.body) {
                        Ok(message) => report.chat.push(message),
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                // Arrives unprompted in the login burst and never again, so
                // the one place that could catch it is this dispatch. A caller
                // that folds packets anywhere else would miss the spellbook
                // entirely and see a character who knows nothing.
                crate::opcode::server::INITIAL_SPELLS => {
                    match crate::spell::parse_initial_spells(&packet.body) {
                        Ok(book) => {
                            report.spells = book.spells.len();
                            let now = std::time::Instant::now();
                            // `SpellCooldown::second` is not yet confirmed to
                            // be a millisecond duration -- see its doc
                            // comment -- so this seeds nothing visible today
                            // (`Cooldown::remaining_fraction` reads `0.0` for
                            // a `0` duration, which is what every observation
                            // so far has held). Folded in anyway because a
                            // later, better-understood value here should not
                            // need a second wiring-up to take effect.
                            for cooldown in &book.cooldowns {
                                self.cooldowns.insert(
                                    cooldown.spell_id,
                                    Cooldown {
                                        started: now,
                                        duration_ms: cooldown.second,
                                    },
                                );
                            }
                            self.spells = book;
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                // Sent once per cast that actually starts a cooldown, unlike
                // the login burst's cooldown list. Folded in here for the
                // same reason as `INITIAL_SPELLS`: this dispatch is the one
                // place that could catch it, and a caller polling anywhere
                // else would see a spell that never appears to cool down.
                // Not yet seen live -- see `parse_spell_cooldown`'s doc
                // comment for what was actually tried.
                crate::opcode::server::SPELL_COOLDOWN => {
                    match crate::spell::parse_spell_cooldown(&packet.body) {
                        Ok((_caster, cooldowns)) => {
                            let now = std::time::Instant::now();
                            for cooldown in cooldowns {
                                self.cooldowns.insert(
                                    cooldown.spell_id,
                                    Cooldown {
                                        started: now,
                                        duration_ms: cooldown.cooldown_ms,
                                    },
                                );
                            }
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                // A cast winding up. Folded in here for the same reason as
                // every other spell packet: this dispatch is the one place
                // that could catch it, and a caller polling anywhere else
                // would never see a cast bar start. Refused by
                // `parse_spell_start` for any target shape it has not
                // confirmed live -- see that parser's doc comment -- which
                // shows up here as an ordinary decode failure, not a panic.
                crate::opcode::server::SPELL_START => {
                    match crate::spell::parse_spell_start(&packet.body) {
                        Ok(start) => {
                            report.casts_started += 1;
                            self.casts.insert(
                                start.caster,
                                Cast {
                                    spell_id: start.spell_id,
                                    started: std::time::Instant::now(),
                                    duration_ms: start.cast_time_ms,
                                },
                            );
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                // A cast landing -- clears whatever `SMSG_SPELL_START` put in
                // `casts` for this caster. An instant-cast spell has no
                // matching `SMSG_SPELL_START`, so clearing an absent entry is
                // the correct, silent no-op for it rather than a sign
                // anything was missed.
                crate::opcode::server::SPELL_GO => {
                    match crate::spell::parse_spell_go(&packet.body) {
                        Ok(go) => {
                            if self.casts.remove(&go.caster).is_some() {
                                report.casts_landed += 1;
                            }
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                // Melee. The swing itself is an event and goes back to the
                // caller; what it did to anybody's health arrives separately
                // as an ordinary field update and needs nothing here.
                crate::opcode::server::ATTACKER_STATE_UPDATE => {
                    match crate::combat::parse_melee_swing(&packet.body) {
                        Ok(swing) => report.swings.push(swing),
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                // A single field changing, without a whole update block. Folded
                // into the entity's own field set rather than kept beside it,
                // so a rage bar has one source of truth: the object-update
                // path carries this value too, and two stores of it would
                // disagree the moment either missed a packet.
                crate::opcode::server::POWER_UPDATE => {
                    match crate::update::parse_power_update(&packet.body) {
                        Ok(power) => match (self.entities.get_mut(&power.guid), power.field()) {
                            (Some(entity), Some(field)) => {
                                entity.fields.set(field, power.value);
                                report.power_updates += 1;
                            }
                            // A power for something not in view is not an
                            // error, it is an update about a unit this client
                            // never saw created -- counted like any other
                            // orphan so a rising number is visible.
                            (None, _) => self.stats.orphaned += 1,
                            (Some(_), None) => report.failures.push((
                                packet.opcode,
                                crate::protocol::Error::UnknownPowerType {
                                    got: power.power_type,
                                },
                                Ok(packet.body.clone()),
                            )),
                        },
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                crate::opcode::server::THREAT_UPDATE => {
                    match crate::combat::parse_threat_update(&packet.body) {
                        Ok(threat) => report.threat.push(threat),
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                crate::opcode::server::ATTACK_START => {
                    match crate::combat::parse_attack_start(&packet.body) {
                        Ok(start) => {
                            report.attacks_started += 1;
                            self.attacking.insert(start.attacker, start.victim);
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                crate::opcode::server::ATTACK_STOP => {
                    match crate::combat::parse_attack_stop(&packet.body) {
                        Ok(stop) => {
                            if self.attacking.remove(&stop.attacker).is_some() {
                                report.attacks_stopped += 1;
                            }
                        }
                        Err(error) => report.failures.push((
                            packet.opcode,
                            error,
                            Ok(packet.body.clone()),
                        )),
                    }
                }
                crate::opcode::server::CAST_FAILED => {
                    match crate::spell::parse_cast_failed(&packet.body) {
                        Ok(failure) => report.cast_failures.push(failure),
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

    #[test]
    fn cooldown_remaining_fraction_counts_down_and_clamps() {
        let started = std::time::Instant::now();
        let cooldown = Cooldown {
            started,
            duration_ms: 4000,
        };
        assert_eq!(cooldown.remaining_fraction(started), 1.0);
        assert!(
            (cooldown.remaining_fraction(started + std::time::Duration::from_millis(2000)) - 0.5)
                .abs()
                < 0.01
        );
        assert_eq!(
            cooldown.remaining_fraction(started + std::time::Duration::from_secs(10)),
            0.0,
            "must clamp at zero rather than go negative"
        );

        let instant = Cooldown {
            started,
            duration_ms: 0,
        };
        assert_eq!(
            instant.remaining_fraction(started),
            0.0,
            "a zero-duration cooldown is never actually on cooldown"
        );
    }

    /// `SMSG_SPELL_COOLDOWN` is the one place a cooldown started by a cast
    /// reaches the client, so it has to land through `replicate` like every
    /// other opcode this crate dispatches.
    #[test]
    fn replicate_folds_spell_cooldown_into_the_world() {
        use crate::client::Packet;

        let mut body = 0x32u64.to_le_bytes().to_vec();
        body.push(0); // flags
        body.extend(78u32.to_le_bytes());
        body.extend(1500u32.to_le_bytes());

        let mut world = WorldState::new();
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::SPELL_COOLDOWN,
                body,
            }],
            None,
        );

        assert!(report.failures.is_empty());
        let now = std::time::Instant::now();
        assert_eq!(world.cooldown_fraction(78, now), 1.0);
        assert_eq!(
            world.cooldown_fraction(9999, now),
            0.0,
            "a spell never put on cooldown must read as ready"
        );
    }

    /// The login burst's own cooldown list must take effect immediately, not
    /// only once a fresh `SMSG_SPELL_COOLDOWN` happens to arrive -- a
    /// character who logs in mid-cooldown should see that on the bar from the
    /// first frame, once `SpellCooldown::second` is confirmed to actually
    /// carry a duration. This tests the wiring on that assumption; it does
    /// not claim the assumption itself is true yet.
    #[test]
    fn replicate_seeds_cooldowns_from_the_login_burst() {
        use crate::client::Packet;

        let mut body = vec![0u8]; // unknown/always-zero leading byte
        body.extend(0u16.to_le_bytes()); // no known spells
        body.extend(1u16.to_le_bytes()); // one cooldown
        body.extend(172u32.to_le_bytes()); // spell id
        body.extend(6000u32.to_le_bytes()); // second word

        let mut world = WorldState::new();
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::INITIAL_SPELLS,
                body,
            }],
            None,
        );

        assert!(report.failures.is_empty());
        assert_eq!(world.cooldown_fraction(172, std::time::Instant::now()), 1.0);
    }

    #[test]
    fn cast_progress_fraction_counts_up_and_clamps() {
        let started = std::time::Instant::now();
        let cast = Cast {
            spell_id: 5185,
            started,
            duration_ms: 4000,
        };
        assert_eq!(cast.progress_fraction(started), 0.0);
        assert!(
            (cast.progress_fraction(started + std::time::Duration::from_millis(2000)) - 0.5)
                .abs()
                < 0.01
        );
        assert_eq!(
            cast.progress_fraction(started + std::time::Duration::from_secs(10)),
            1.0,
            "must clamp at one rather than run past it"
        );

        let instant = Cast {
            spell_id: 6603,
            started,
            duration_ms: 0,
        };
        assert_eq!(
            instant.progress_fraction(started),
            1.0,
            "a zero-duration cast is already done the instant it starts"
        );
    }

    /// `SMSG_SPELL_START` and `SMSG_SPELL_GO`, straight off the pinned live
    /// capture in `crates/world/src/spell.rs`'s tests -- same cast, same wire
    /// dump. `replicate` is the one place that can catch either, the same
    /// reasoning as every other spell packet this crate dispatches.
    #[test]
    fn replicate_folds_spell_start_into_a_cast_and_spell_go_clears_it() {
        use crate::client::Packet;

        let start_body: [u8; 27] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0xdc,
            0x05, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x33, 0x64, 0x00, 0x00, 0x00,
        ];
        let mut world = WorldState::new();
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::SPELL_START,
                body: start_body.to_vec(),
            }],
            None,
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.casts_started, 1);
        let now = std::time::Instant::now();
        let cast = world
            .active_cast(0x33, now)
            .expect("the caster's cast must be visible immediately");
        assert_eq!(cast.spell_id, 5185);
        assert_eq!(cast.duration_ms, 1500);
        assert!(cast.progress_fraction(now) < 1.0);

        let go_body: [u8; 37] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x74,
            0xd4, 0x37, 0x5a, 0x01, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x01, 0x33, 0x51, 0x00, 0x00, 0x00,
        ];
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::SPELL_GO,
                body: go_body.to_vec(),
            }],
            None,
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.casts_landed, 1);
        assert!(
            world.active_cast(0x33, std::time::Instant::now()).is_none(),
            "SMSG_SPELL_GO must clear the cast it landed"
        );
    }

    /// An instant-cast spell goes straight to `SMSG_SPELL_GO` with no
    /// `SMSG_SPELL_START` before it, so clearing a caster nobody started a
    /// cast for has to be a silent no-op, not a sign anything was missed.
    #[test]
    fn a_spell_go_with_no_matching_start_clears_nothing() {
        use crate::client::Packet;

        let go_body: [u8; 37] = [
            0x01, 0x33, 0x01, 0x33, 0x00, 0x41, 0x14, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x74,
            0xd4, 0x37, 0x5a, 0x01, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x01, 0x33, 0x51, 0x00, 0x00, 0x00,
        ];
        let mut world = WorldState::new();
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::SPELL_GO,
                body: go_body.to_vec(),
            }],
            None,
        );
        assert!(report.failures.is_empty());
        assert_eq!(report.casts_landed, 0);
    }

    /// A cast bar must not stay stuck forever behind a `SMSG_SPELL_GO` this
    /// parser cannot yet read -- see `Cast`'s doc comment. `active_cast` reads
    /// `None` once `duration_ms` has fully elapsed even though nothing ever
    /// removed the entry from `WorldState::casts`.
    #[test]
    fn active_cast_expires_on_its_own_without_needing_spell_go() {
        let mut world = WorldState::new();
        let started = std::time::Instant::now();
        world.casts.insert(
            0x33,
            Cast {
                spell_id: 5185,
                started,
                duration_ms: 1500,
            },
        );
        assert!(world.active_cast(0x33, started).is_some());
        assert!(
            world
                .active_cast(0x33, started + std::time::Duration::from_millis(2000))
                .is_none(),
            "a cast past its own duration must stop showing on its own"
        );
    }

    /// A swing has to come back out of `replicate`, from the real bytes.
    ///
    /// The single most likely way combat breaks in this crate is not the
    /// parser: it is `replicate` handing an event back and a caller dropping
    /// it, which has now happened with chat, with cast failures, and with
    /// attack starts inside this project's own test tool. A count of swings
    /// applied would not catch it, because there is nothing to apply -- the
    /// damage arrives separately as a field update.
    #[test]
    fn replicate_hands_back_the_swings_it_parsed() {
        use crate::client::Packet;

        // Verbatim from the capture -- see `combat::tests`. This client
        // hitting a Northshire kobold for 4.
        let hit = [
            0x02, 0x00, 0x00, 0x00, 0x01, 0x32, 0xcb, 0xde, 0x0b, 0x06, 0x30, 0xf1, 0x04, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80,
            0x40, 0x04, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let mut world = WorldState::new();
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::ATTACKER_STATE_UPDATE,
                body: hit.to_vec(),
            }],
            None,
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.swings.len(), 1, "the swing never reached the caller");
        assert_eq!(report.swings[0].damage, 4);
        assert_eq!(report.swings[0].attacker, 0x32);
    }

    /// A power update has to reach the *entity's own field set*, not a store
    /// beside it: the object-update path carries this value too, and two
    /// copies disagree the moment either misses a packet.
    #[test]
    fn a_power_update_lands_in_the_entity_that_owns_it() {
        use crate::client::Packet;

        const ME: u64 = 0x32;
        let mut world = WorldState::new();
        // A player with rage already known, so there is something to overwrite
        // and the test cannot pass by merely inserting.
        let mut fields = crate::update::Fields::default();
        fields.set(crate::update::fields::UNIT_POWER1 + 1, 100);
        // Race, class, gender, power type -- one per byte, in that order. A
        // unit frame reads its power *through* this byte, so an entity
        // without it reports no power at all however many power fields it
        // has: human warrior, rage.
        fields.set(crate::update::fields::UNIT_BYTES_0, 1 | (1 << 8) | (1 << 24));
        world.create(
            ME,
            crate::update::ObjectType::Player,
            &crate::update::Movement::default(),
            &fields,
        );

        // Verbatim from the capture: rage reaching 500, the value `--units`
        // independently reported at the end of that run.
        let body = [0x01, 0x32, 0x01, 0xf4, 0x01, 0x00, 0x00];
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::POWER_UPDATE,
                body: body.to_vec(),
            }],
            None,
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.power_updates, 1);
        assert_eq!(
            world.get(ME).and_then(|e| e.power()),
            Some(500),
            "the update did not reach the field a unit frame reads"
        );
    }

    /// A power index past the end of the array must be refused, not written:
    /// it would otherwise overwrite whatever field sits that far past the
    /// powers, which is corruption with no error attached.
    #[test]
    fn a_power_type_past_the_array_is_refused() {
        use crate::client::Packet;

        let mut world = WorldState::new();
        world.create(
            0x32,
            crate::update::ObjectType::Player,
            &crate::update::Movement::default(),
            &crate::update::Fields::default(),
        );
        let body = [0x01, 0x32, 200, 0x01, 0x00, 0x00, 0x00];
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::POWER_UPDATE,
                body: body.to_vec(),
            }],
            None,
        );
        assert_eq!(report.power_updates, 0);
        assert_eq!(report.failures.len(), 1, "a bad power type was written anyway");
    }

    /// Attack start and stop have to balance, or the client believes a fight
    /// is still going after it ended.
    #[test]
    fn a_fight_starts_and_stops() {
        use crate::client::Packet;

        const ME: u64 = 0x32;
        const WOLF: u64 = 0xf130_0000_0600_0bde;
        let start = [
            0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xde, 0x0b, 0x00, 0x06, 0x00, 0x00,
            0x30, 0xf1,
        ];
        // The same fight ending -- and note this one packs its guids where the
        // start does not.
        let stop = [
            0x01, 0x32, 0xcb, 0xde, 0x0b, 0x06, 0x30, 0xf1, 0x00, 0x00, 0x00, 0x00,
        ];

        let mut world = WorldState::new();
        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::ATTACK_START,
                body: start.to_vec(),
            }],
            None,
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.attacks_started, 1);
        assert_eq!(world.attacking.get(&ME), Some(&WOLF));

        let report = world.replicate(
            &[Packet {
                opcode: crate::opcode::server::ATTACK_STOP,
                body: stop.to_vec(),
            }],
            None,
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.attacks_stopped, 1);
        assert!(world.attacking.is_empty(), "the fight outlived its stop");
    }

    /// A creature that dies is destroyed rather than always sending a stop, so
    /// the entries naming it -- on either side of the fight -- have to leave
    /// with it. Otherwise a corpse goes on being recorded as under attack, or
    /// as attacking.
    #[test]
    fn a_fight_leaves_with_whoever_died() {
        const ME: u64 = 0x32;
        const WOLF: u64 = 0xf130_0000_0600_0bde;

        let mut world = WorldState::new();
        world.attacking.insert(ME, WOLF);
        world.attacking.insert(WOLF, ME);

        world.remove(WOLF);
        assert!(
            world.attacking.is_empty(),
            "the dead unit is still in a fight: {:?}",
            world.attacking
        );
    }

    /// A caster leaving the world takes its cast with it.
    ///
    /// Nothing a player can see depends on this -- a stale entry reads as a
    /// finished cast and draws nothing, which is precisely why it would never
    /// have been noticed. It is here because `casts` is keyed by caster guid
    /// rather than by spell id, so without a removal path tied to an event
    /// that actually happens it grows for the whole session: the only other
    /// way an entry leaves is a `SMSG_SPELL_GO` that parses, and this crate
    /// deliberately refuses that packet for shapes it has not confirmed. The
    /// books are supposed to balance.
    #[test]
    fn a_cast_leaves_with_the_caster() {
        let mut world = WorldState::new();
        world.casts.insert(
            0x33,
            Cast {
                spell_id: 5185,
                started: std::time::Instant::now(),
                duration_ms: 1500,
            },
        );

        // Removing a guid the world never held still has to clear the cast:
        // an out-of-range block names guids that may have been created before
        // this client was watching.
        assert!(!world.remove(0x33), "no entity was ever created for it");
        assert!(
            world.casts.is_empty(),
            "the caster left and its cast stayed behind"
        );
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

    /// The distinction the renderer picks a walk or a run cycle from, so it
    /// has to come out in the units those speeds are quoted in: world units
    /// per second, from a path length and a duration in milliseconds.
    #[test]
    fn move_speed_is_the_paths_length_over_its_duration() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        // 30 units in 4 seconds: 7.5, a run.
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: Some(at(30.0, 0.0)),
            duration: 4000,
            stopped: false,
            facing: None,
        });

        let entity = world.get(7).unwrap();
        let started = entity.move_started.unwrap();
        let speed = entity
            .move_speed(started + std::time::Duration::from_millis(1000))
            .expect("a move is in flight");
        assert!((speed - 7.5).abs() < 1e-3, "got {speed}");

        // A move that has arrived has no speed at all, for the same reason
        // `is_moving` goes false: the creature is standing there.
        assert!(entity
            .move_speed(started + std::time::Duration::from_secs(30))
            .is_none());
    }

    /// The path is measured in three dimensions, so a creature climbing a
    /// slope is not reported as slower than one crossing flat ground.
    #[test]
    fn move_speed_counts_the_climb() {
        let mut world = WorldState::new();
        world.apply(&[create(7, ObjectType::Unit, Some(at(0.0, 0.0)), &[])]);
        let uphill = Position {
            x: 3.0,
            y: 0.0,
            z: 4.0,
            orientation: 0.0,
        };
        world.apply_monster_move(&MonsterMove {
            guid: 7,
            from: at(0.0, 0.0),
            to: Some(uphill),
            duration: 1000,
            stopped: false,
            facing: None,
        });

        let entity = world.get(7).unwrap();
        let started = entity.move_started.unwrap();
        let speed = entity.move_speed(started).expect("a move is in flight");
        assert!((speed - 5.0).abs() < 1e-3, "got {speed}, expected the 3-4-5 hypotenuse");
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

    /// Four values in one field, so the byte order is worth pinning: getting
    /// it wrong reports a night elf rogue as a human warrior and parses
    /// perfectly either way.
    #[test]
    fn bytes_0_unpacks_race_class_gender_and_power() {
        use crate::update::fields;
        let mut world = WorldState::new();
        world.apply(&[create(
            1,
            ObjectType::Unit,
            None,
            &[(fields::UNIT_BYTES_0, packed_bytes_0(4, 4, 1, 3))],
        )]);
        let entity = world.get(1).unwrap();
        assert_eq!(entity.race(), Some(4));
        assert_eq!(entity.class(), Some(4));
        assert_eq!(entity.gender(), Some(1));
        assert_eq!(entity.power_type(), Some(3));
    }

    /// The powers are a parallel array indexed by the unit's own type. Reading
    /// `UNIT_POWER1` regardless is the shortcut that works for every caster
    /// and reports zero for everyone else.
    #[test]
    fn power_comes_from_the_field_the_units_type_selects() {
        use crate::update::fields;
        let mut world = WorldState::new();
        world.apply(&[create(
            1,
            ObjectType::Unit,
            None,
            &[
                // A rogue: energy, which is power type 3.
                (fields::UNIT_BYTES_0, packed_bytes_0(1, 4, 0, 3)),
                (fields::UNIT_POWER1, 0),
                (fields::UNIT_POWER1 + 3, 87),
                (fields::UNIT_MAX_POWER1, 0),
                (fields::UNIT_MAX_POWER1 + 3, 100),
            ],
        )]);
        let entity = world.get(1).unwrap();
        assert_eq!(entity.power(), Some(87), "read POWER1 instead of POWER4");
        assert_eq!(entity.max_power(), Some(100));
    }

    /// A power type off the wire is an arbitrary number, and adding it to a
    /// base index unchecked reads whatever field happens to live there.
    ///
    /// Type 29 is not hypothetical arithmetic: `UNIT_POWER1 + 29` lands exactly
    /// on `UNIT_LEVEL`, so an unguarded read would report this unit's level as
    /// its current mana -- a plausible number, in the right kind of range, and
    /// wrong.
    #[test]
    fn a_power_type_past_the_array_reads_nothing_rather_than_a_neighbour() {
        use crate::update::fields;
        assert_eq!(
            fields::UNIT_POWER1 + 29,
            fields::UNIT_LEVEL,
            "the premise of this test: an unguarded index 29 lands on the level"
        );

        let mut world = WorldState::new();
        world.apply(&[create(
            1,
            ObjectType::Unit,
            None,
            &[
                (fields::UNIT_BYTES_0, packed_bytes_0(1, 1, 0, 29)),
                (fields::UNIT_LEVEL, 60),
            ],
        )]);
        let entity = world.get(1).unwrap();
        assert_eq!(entity.power(), None);
        assert_eq!(entity.max_power(), None);
        assert_eq!(entity.level(), Some(60), "the field itself is untouched");
    }

    /// An empty target field is zero, which is not a guid.
    #[test]
    fn nothing_targeted_reads_as_no_target() {
        use crate::update::fields;
        let mut world = WorldState::new();
        world.apply(&[
            create(1, ObjectType::Unit, None, &[(fields::UNIT_TARGET, 0)]),
            create(
                2,
                ObjectType::Unit,
                None,
                &[(fields::UNIT_TARGET, 0x1234), (fields::UNIT_TARGET + 1, 0xF13)],
            ),
        ]);
        assert_eq!(world.get(1).unwrap().target(), None);
        assert_eq!(world.get(2).unwrap().target(), Some(0x0000_0F13_0000_1234));
    }

    fn packed_bytes_0(race: u8, class: u8, gender: u8, power: u8) -> u32 {
        u32::from_le_bytes([race, class, gender, power])
    }
}
