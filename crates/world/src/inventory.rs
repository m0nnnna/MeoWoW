//! What the character is carrying, read out of the inventory slot array.
//!
//! The whole of this module is a view over data that has already arrived. A
//! character's items are replicated as ordinary [`ObjectType::Item`] objects in
//! the login burst -- seven of them for a starting warrior, four worn and three
//! in the backpack -- and the player's own object carries an array of guids
//! saying where each one sits. So a bag window resolves a slot by looking a
//! guid up in [`WorldState`], and needs **no item query at all** for anything
//! already held. That is worth stating plainly because the obvious design is a
//! query per slot, and it would be thirty-nine round trips to learn something
//! the client was told at login.
//!
//! **The slot vocabulary below is measured, and the measured part stops where
//! the comments say it stops.** Four equipment slots were confirmed against a
//! live realm by prediction (see [`update::fields::PLAYER_FIELD_INV_SLOT_HEAD`]);
//! the rest of the equipment names are *not* yet confirmed and are deliberately
//! absent rather than guessed. A wrong name for a slot never fails loudly -- it
//! draws a helm in the boots square and looks like a rendering bug -- which is
//! the same trap `describe_cast_failure` is built around.
//!
//! [`update::fields::PLAYER_FIELD_INV_SLOT_HEAD`]: crate::update::fields::PLAYER_FIELD_INV_SLOT_HEAD
//! [`ObjectType::Item`]: crate::ObjectType
//! [`WorldState`]: crate::WorldState

use crate::state::{Entity, WorldState};
use crate::update::fields;
use crate::update::ObjectType;

/// How many slots the array holds in total: 19 equipped, 4 bags, 16 backpack.
pub const SLOT_COUNT: u16 = 39;

/// The "bag" value meaning *not in a bag* -- an index into the player's own
/// slot array rather than into a container.
///
/// Every request that names an item names it as a (bag, slot) pair, and this
/// is the sentinel for the common case. Worth naming rather than writing 255
/// at each call site: it is not a count and not a maximum, and a bare 255 in a
/// packet body reads like either.
pub const OWN_SLOT_ARRAY: u8 = 255;

/// The first slot holding worn equipment, and how many there are.
pub const EQUIPPED_FIRST: u16 = 0;
pub const EQUIPPED_COUNT: u16 = 19;

/// The four slots holding *bags*, as opposed to what is inside them.
pub const BAG_FIRST: u16 = 19;
pub const BAG_COUNT: u16 = 4;

/// The backpack: the sixteen slots every character has without owning a bag.
pub const BACKPACK_FIRST: u16 = 23;
pub const BACKPACK_COUNT: u16 = 16;

/// Which of the three regions a slot index falls in.
///
/// An enum rather than three range checks at each call site, because the
/// boundaries are the part most likely to be got wrong twice in different
/// places -- and two copies of a boundary agree until one of them moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// Worn: armour and weapons. Slots 0-18.
    Equipped,
    /// One of the four bags themselves. Slots 19-22.
    Bag,
    /// The backpack's own sixteen slots. Slots 23-38.
    Backpack,
}

/// One position in the character's inventory array.
///
/// A newtype rather than a bare `u16` because this index is used two ways --
/// as an offset into the update-field array and as a position in a window --
/// and the two are different numbers. Conflating them reads the wrong field
/// and returns a real guid from somewhere else, which is exactly the failure
/// this project keeps paying for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InventorySlot(u16);

impl InventorySlot {
    /// Every slot in the array, in order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..SLOT_COUNT).map(Self)
    }

    /// A slot by index, refusing anything past the end of the array.
    ///
    /// Refusing rather than clamping: an index past the end would otherwise
    /// read whichever field happens to live after the array and hand back a
    /// number that parses perfectly as a guid. Same reasoning as
    /// `Entity::power_index`, which refuses rather than clamps for exactly
    /// this reason.
    pub fn new(index: u16) -> Option<Self> {
        (index < SLOT_COUNT).then_some(Self(index))
    }

    pub fn index(self) -> u16 {
        self.0
    }

    pub fn kind(self) -> SlotKind {
        match self.0 {
            i if i < BAG_FIRST => SlotKind::Equipped,
            i if i < BACKPACK_FIRST => SlotKind::Bag,
            _ => SlotKind::Backpack,
        }
    }

    /// Where this slot's guid begins in the update-field array.
    ///
    /// The one place the stride is applied. See
    /// [`fields::PLAYER_FIELD_INV_SLOT_HEAD`] for how it was measured.
    pub fn field(self) -> u16 {
        fields::PLAYER_FIELD_INV_SLOT_HEAD + self.0 * fields::INV_SLOT_STRIDE
    }

    /// What this slot is for, when that is known.
    ///
    /// **Every name here was measured, and the one that was not is absent.**
    /// A wrong name for a slot never fails loudly -- it draws a helm in the
    /// boots square and reads as a rendering bug -- so none of these was
    /// written down from memory.
    ///
    /// Two independent methods produced them, and where they overlap they
    /// agree, which is the part worth keeping:
    ///
    /// - **Prediction.** A starting human warrior wears a shirt, legs, feet
    ///   and a main hand, and with the array's identified base those are
    ///   exactly the four slots a live realm filled in. That is what
    ///   identified the base itself -- see
    ///   [`fields::PLAYER_FIELD_INV_SLOT_HEAD`].
    /// - **Equipping, and watching where it landed.** `CMSG_AUTOEQUIP_ITEM`
    ///   lets the *server* choose the destination, so wearing one item of each
    ///   kind and recording which index it arrived at names the slot without
    ///   anyone guessing. Sixteen items, one run.
    ///
    /// The overlap is the check: boots equipped to slot 7 and a sword to slot
    /// 15, which the starting-gear prediction had already called Feet and Main
    /// Hand by completely unrelated reasoning. Two derivations agreeing is
    /// evidence in a way that either alone is not.
    ///
    /// **Slot 17 is unnamed because it could not be measured.** It is
    /// presumably the ranged slot -- it is the one gap in an otherwise
    /// contiguous run -- but every ranged item tried (a bow, a rifle, a
    /// fishing pole) came back refused with a single `0x0112`, because the
    /// character doing the testing is a warrior. "Presumably" is not a
    /// measurement, and an inferred name is exactly the kind that gets
    /// believed. A hunter would settle it in one run.
    pub fn label(self) -> Option<&'static str> {
        match self.0 {
            0 => Some("Head"),
            1 => Some("Neck"),
            2 => Some("Shoulders"),
            3 => Some("Shirt"),
            4 => Some("Chest"),
            5 => Some("Waist"),
            6 => Some("Legs"),
            7 => Some("Feet"),
            8 => Some("Wrists"),
            9 => Some("Hands"),
            10 => Some("Finger 1"),
            11 => Some("Finger 2"),
            12 => Some("Trinket 1"),
            13 => Some("Trinket 2"),
            14 => Some("Back"),
            15 => Some("Main Hand"),
            16 => Some("Off Hand"),
            // 17: see the doc comment. Measured as refused, not as absent.
            18 => Some("Tabard"),
            _ => None,
        }
    }
}

/// One occupied slot: where it is, and what is in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldItem {
    pub slot: InventorySlot,
    pub guid: u64,
    /// The item's entry, when its object has been replicated.
    ///
    /// Separate from the guid because the two answer different questions and
    /// arrive by different routes: the guid comes from the *player's* fields
    /// and says a slot is occupied, while the entry comes from the *item's*
    /// own object and says what occupies it. A slot can be legitimately
    /// occupied by an object we have not been sent, and that must read as
    /// "something is here" rather than as an empty square.
    pub entry: Option<u32>,
    /// How many are in the stack. One for anything that does not stack.
    ///
    /// Defaults to one rather than zero for a missing field, because a sparse
    /// field set omits zeros and there is no such thing as a stack of none --
    /// a zero here would draw an item with the quantity "0" beside it.
    pub count: u32,
    /// How many slots this holds, if it is a bag.
    ///
    /// `None` for an ordinary item. Note that a bag sitting *in* the backpack
    /// is still a container and still reports its capacity: this says what the
    /// object is, not whether it is in use.
    pub capacity: Option<u32>,
}

/// The slots a bag window can actually draw: the backpack, and nothing inside
/// an equipped bag.
///
/// **What a bag's contents are addressed by is not yet known, which is why
/// this function exists rather than callers filtering [`held`] themselves.**
///
/// This is a gap with a reason, recorded so the next attempt does not start
/// where this one did. Everything above was measured against the live realm;
/// this could not be, and the obstacle was not the protocol.
///
/// A container replicates as an ordinary object carrying
/// [`fields::CONTAINER_FIELD_NUM_SLOTS`], so a bag announces its capacity the
/// moment it is replicated -- including one merely sitting in the backpack.
/// What it does *not* carry is any field naming its contents, and that is
/// consistent rather than surprising: the bags observed were empty, an
/// object-create block omits zero fields entirely, and an empty slot is a
/// zero. So an empty bag and a bag whose contents array we cannot find look
/// exactly the same, which means the array can only be located from a bag with
/// something in it.
///
/// Getting one proved to be the hard part. `.additem` places a bag in the
/// backpack, never in a bag slot, and moving it there by editing
/// `character_inventory` directly does not survive: the server's own loader
/// declines a hand-placed bag and relocates it and its contents into the
/// backpack, which is what the wire then reports. Three runs confirmed the
/// wire and the database disagreeing in exactly that way, with the database
/// unchanged afterwards because the session ended by closing the socket.
///
/// **The wire is what this client must believe, and the wire said no bags were
/// equipped** -- so nothing here is guessing at a layout it could not see. The
/// legitimate way to equip a bag is for a client to ask, which is a *write*,
/// and writing a format is the riskier direction: a wrong outgoing request is
/// accepted as some other valid one and misbehaves somewhere else. Moving
/// items between slots is its own feature and belongs with that write, not
/// ahead of it.
pub fn carried(state: &WorldState, player_guid: u64) -> Vec<HeldItem> {
    held(state, player_guid)
        .into_iter()
        .filter(|item| item.slot.kind() == SlotKind::Backpack)
        .collect()
}

/// The four bag slots, whether or not they hold anything.
///
/// Returned as a fixed-length array of options rather than as a list, because
/// an empty bag slot is a square the window still draws -- see [`held`] for
/// why the carried items are the other way round.
pub fn bags(state: &WorldState, player_guid: u64) -> [Option<HeldItem>; BAG_COUNT as usize] {
    let held = held(state, player_guid);
    std::array::from_fn(|i| {
        let index = BAG_FIRST + i as u16;
        held.iter()
            .find(|item| item.slot.index() == index)
            .copied()
    })
}

/// The nineteen worn slots, whether or not they hold anything.
pub fn equipped(
    state: &WorldState,
    player_guid: u64,
) -> [Option<HeldItem>; EQUIPPED_COUNT as usize] {
    let held = held(state, player_guid);
    std::array::from_fn(|i| {
        let index = EQUIPPED_FIRST + i as u16;
        held.iter()
            .find(|item| item.slot.index() == index)
            .copied()
    })
}

/// Reads one slot's guid out of a player's fields.
///
/// `None` for an empty slot. Note that an all-zero guid and an absent field are
/// the same statement here: an object-create block carries only non-zero
/// values, so an empty slot has no field at all rather than a field holding
/// zero. Treating absence as "unknown" is the mistake that left default-looking
/// players white -- see [`fields::PLAYER_BYTES`].
pub fn slot_guid(player: &Entity, slot: InventorySlot) -> Option<u64> {
    match player.fields.get_u64(slot.field()) {
        Some(0) | None => None,
        Some(guid) => Some(guid),
    }
}

/// Everything the character is carrying, in slot order.
///
/// Empty slots are skipped rather than yielded as `None`, because the *window*
/// decides how many squares to draw from the slot ranges above; a caller that
/// wanted a fixed-length array of options would be deriving the layout from
/// the data, which puts a hole in the grid whenever a slot happens to be
/// empty.
pub fn held(state: &WorldState, player_guid: u64) -> Vec<HeldItem> {
    let Some(player) = state.get(player_guid) else {
        return Vec::new();
    };

    InventorySlot::all()
        .filter_map(|slot| {
            let guid = slot_guid(player, slot)?;
            // The item's own object, when it arrived. See `HeldItem::entry`
            // for why a missing one is not the same as an empty slot.
            let object = state.get(guid).filter(|item| {
                matches!(item.object_type, ObjectType::Item | ObjectType::Container)
            });
            Some(HeldItem {
                slot,
                guid,
                entry: object.and_then(|item| item.fields.get(fields::OBJECT_ENTRY)),
                count: object
                    .and_then(|item| item.fields.get(fields::ITEM_FIELD_STACK_COUNT))
                    .unwrap_or(1),
                capacity: object
                    .filter(|item| item.object_type == ObjectType::Container)
                    .and_then(|item| item.fields.get(fields::CONTAINER_FIELD_NUM_SLOTS)),
            })
        })
        .collect()
}

/// The character's money, in copper.
///
/// Zero rather than `None` for an absent field, because a sparse field set
/// omits zeros and a character with no money is the commonest case there is.
pub fn coinage(state: &WorldState, player_guid: u64) -> u32 {
    state
        .get(player_guid)
        .and_then(|player| player.fields.get(fields::PLAYER_FIELD_COINAGE))
        .unwrap_or(0)
}

/// Splits copper into gold, silver and copper for display.
///
/// Presentation only -- the wire carries one number. Kept here beside
/// [`coinage`] so the two cannot drift, rather than in the drawing code where
/// it would be reimplemented by whoever needs it next.
pub fn purse(copper: u32) -> (u32, u32, u32) {
    (copper / 10_000, (copper / 100) % 100, copper % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::{parse_update_object, write_packed_guid, Block};

    /// Builds a create block through the real parser rather than by
    /// constructing `Fields` directly, so these tests exercise the same sparse
    /// decoding the wire goes through -- including the rule that an absent
    /// field and a zero are the same statement, which several of them turn on.
    fn create(guid: u64, object_type: u8, entries: &[(u16, u32)]) -> Block {
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
        write_packed_guid(guid, &mut packet);
        packet.push(object_type);
        packet.extend_from_slice(&0u16.to_le_bytes());
        packet.extend_from_slice(&body);
        parse_update_object(&packet).unwrap().remove(0)
    }

    /// Object type bytes as the wire uses them.
    const TYPE_ITEM: u8 = 1;
    const TYPE_CONTAINER: u8 = 2;
    const TYPE_PLAYER: u8 = 4;

    /// A player holding the given (slot, item guid) pairs, with each item
    /// replicated as its own object carrying an entry and a stack count.
    fn world(slots: &[(u16, u64, u32, u32)]) -> (WorldState, u64) {
        const PLAYER: u64 = 1;
        let mut state = WorldState::new();

        let mut player_fields = vec![(fields::PLAYER_FIELD_COINAGE, 123_456)];
        let mut blocks = Vec::new();
        for (slot, guid, entry, count) in slots {
            let at = InventorySlot::new(*slot).unwrap().field();
            player_fields.push((at, *guid as u32));
            player_fields.push((at + 1, (*guid >> 32) as u32));
            blocks.push(create(
                *guid,
                TYPE_ITEM,
                &[
                    (fields::OBJECT_ENTRY, *entry),
                    (fields::ITEM_FIELD_STACK_COUNT, *count),
                ],
            ));
        }
        blocks.insert(0, create(PLAYER, TYPE_PLAYER, &player_fields));
        state.apply(&blocks);
        (state, PLAYER)
    }

    /// The three regions must tile the array exactly: no slot in two of them,
    /// none in none of them. Written as a property over every index rather
    /// than as three boundary assertions, because the failure this guards
    /// against is an off-by-one at a boundary and boundary assertions are
    /// where an off-by-one gets copied.
    #[test]
    fn the_three_regions_tile_the_array() {
        assert_eq!(EQUIPPED_COUNT + BAG_COUNT + BACKPACK_COUNT, SLOT_COUNT);
        assert_eq!(EQUIPPED_FIRST + EQUIPPED_COUNT, BAG_FIRST);
        assert_eq!(BAG_FIRST + BAG_COUNT, BACKPACK_FIRST);

        for slot in InventorySlot::all() {
            let kind = slot.kind();
            let expected = match slot.index() {
                0..=18 => SlotKind::Equipped,
                19..=22 => SlotKind::Bag,
                _ => SlotKind::Backpack,
            };
            assert_eq!(kind, expected, "slot {}", slot.index());
        }
    }

    /// Past the end is refused rather than clamped -- a clamp would read a
    /// real field belonging to something else.
    #[test]
    fn a_slot_past_the_end_is_refused() {
        assert!(InventorySlot::new(SLOT_COUNT - 1).is_some());
        assert!(InventorySlot::new(SLOT_COUNT).is_none());
        assert!(InventorySlot::new(u16::MAX).is_none());
    }

    /// The stride, stated as the thing that was actually observed: adding one
    /// item put a guid at `0x0174` and adding a second put one at `0x0176`.
    /// Those are backpack slots 24 and 25.
    #[test]
    fn consecutive_slots_are_two_fields_apart() {
        let first = InventorySlot::new(24).unwrap();
        let second = InventorySlot::new(25).unwrap();
        assert_eq!(first.field(), 0x0174);
        assert_eq!(second.field(), 0x0176);
        assert_eq!(second.field() - first.field(), fields::INV_SLOT_STRIDE);
    }

    /// The check that identified the base, kept as a test because it is the
    /// only one that could have failed. A starting human warrior wears a
    /// shirt, legs, feet and a main hand; those are slots 3, 6, 7 and 15, and
    /// the fields they resolve to are what a live realm actually filled in.
    #[test]
    fn a_starting_warriors_four_items_land_where_predicted() {
        let filled: Vec<u16> = [3u16, 6, 7, 15]
            .into_iter()
            .map(|i| InventorySlot::new(i).unwrap().field())
            .collect();
        assert_eq!(filled, vec![0x014A, 0x0150, 0x0152, 0x0162]);

        for slot in [3u16, 6, 7, 15] {
            assert_eq!(
                InventorySlot::new(slot).unwrap().kind(),
                SlotKind::Equipped,
                "slot {slot} should be worn"
            );
        }
    }

    /// Only measured slots are named, and **the silence is asserted as
    /// firmly as the names**.
    ///
    /// Slot 17 is the whole point of this test. It is the single gap in an
    /// otherwise contiguous run of eighteen, which makes "it must be ranged"
    /// overwhelmingly tempting and still not a measurement -- every ranged
    /// item tried came back refused because the test character is a warrior.
    /// A later session that fills it in from memory has to delete an assertion
    /// that exists to say no.
    #[test]
    fn only_measured_slots_are_labelled() {
        let named: Vec<u16> = InventorySlot::all()
            .filter(|slot| slot.label().is_some())
            .map(|slot| slot.index())
            .collect();
        assert_eq!(
            named,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18],
            "slot 17 is unmeasured; see InventorySlot::label"
        );

        assert_eq!(
            InventorySlot::new(17).unwrap().label(),
            None,
            "slot 17 was named without being measured"
        );

        // Nothing outside the equipped region has a name: a backpack square
        // is a position, not a purpose.
        for slot in InventorySlot::all().filter(|s| s.kind() != SlotKind::Equipped) {
            assert_eq!(slot.label(), None, "slot {} is not worn", slot.index());
        }
    }

    /// The two slots where the prediction and the equip sweep overlap, kept
    /// because agreement between independent derivations is the evidence.
    ///
    /// Boots equipped to slot 7 and a sword to slot 15; the starting-gear
    /// prediction had already called those Feet and Main Hand by unrelated
    /// reasoning.
    #[test]
    fn the_two_independently_derived_slots_agree() {
        assert_eq!(InventorySlot::new(7).unwrap().label(), Some("Feet"));
        assert_eq!(InventorySlot::new(15).unwrap().label(), Some("Main Hand"));
    }

    /// The whole read path, against the layout the live realm actually
    /// produced: four worn items, three in the backpack, and the money that
    /// was set with `.modify money 123456`.
    #[test]
    fn a_characters_slots_read_back_where_they_were_put() {
        let (state, player) = world(&[
            (3, 0x4000_0000_0000_0002, 38, 1),
            (6, 0x4000_0000_0000_0004, 39, 1),
            (7, 0x4000_0000_0000_0006, 40, 1),
            (15, 0x4000_0000_0000_0008, 49778, 1),
            (23, 0x4000_0000_0000_000A, 6948, 1),
            (24, 0x4000_0000_0000_001F, 2589, 3),
            (25, 0x4000_0000_0000_0020, 2592, 5),
        ]);

        let held = held(&state, player);
        assert_eq!(held.len(), 7);
        assert_eq!(
            held.iter().map(|i| i.slot.index()).collect::<Vec<_>>(),
            vec![3, 6, 7, 15, 23, 24, 25],
            "slots must come back in order"
        );

        // The guid is 64 bits and arrives as two fields. Reading only the low
        // word would still produce a guid that looks fine, so assert the high
        // word survived.
        assert_eq!(held[0].guid, 0x4000_0000_0000_0002);
        assert_eq!(held[0].entry, Some(38));

        assert_eq!(coinage(&state, player), 123_456);
        assert_eq!(purse(coinage(&state, player)), (12, 34, 56));
    }

    /// The three stacks that identified the count field, and the ones that
    /// did not stack. Both halves matter: a count field read from the wrong
    /// place would most likely come back as a constant, which is exactly what
    /// the non-stacking items legitimately look like.
    #[test]
    fn stack_counts_are_read_and_absent_means_one() {
        let (state, player) = world(&[
            (23, 0x4000_0000_0000_000A, 6948, 1),
            (24, 0x4000_0000_0000_001F, 2589, 3),
            (25, 0x4000_0000_0000_0020, 2592, 5),
            (26, 0x4000_0000_0000_0021, 4306, 17),
        ]);

        let counts: Vec<u32> = carried(&state, player).iter().map(|i| i.count).collect();
        assert_eq!(counts, vec![1, 3, 5, 17]);

        // A stack of one is written as the field being absent, because a
        // create block omits zeros -- and one is what that must read as.
        let mut state = WorldState::new();
        state.apply(&[
            create(
                1,
                TYPE_PLAYER,
                &[(InventorySlot::new(23).unwrap().field(), 0x0A)],
            ),
            create(0x0A, TYPE_ITEM, &[(fields::OBJECT_ENTRY, 6948)]),
        ]);
        assert_eq!(carried(&state, 1)[0].count, 1);
    }

    /// A bag reports its capacity and an ordinary item reports none. The
    /// second half is the one that could regress silently: reading the field
    /// unconditionally would give every item in the game a bag capacity taken
    /// from whatever lives at that index.
    #[test]
    fn only_containers_report_a_capacity() {
        let mut state = WorldState::new();
        let pouch = 0x4000_0000_0000_0023u64;
        let cloth = 0x4000_0000_0000_0021u64;
        let bag_slot = InventorySlot::new(27).unwrap().field();
        let item_slot = InventorySlot::new(26).unwrap().field();
        state.apply(&[
            create(
                1,
                TYPE_PLAYER,
                &[
                    (bag_slot, pouch as u32),
                    (bag_slot + 1, (pouch >> 32) as u32),
                    (item_slot, cloth as u32),
                    (item_slot + 1, (cloth >> 32) as u32),
                ],
            ),
            create(
                pouch,
                TYPE_CONTAINER,
                &[
                    (fields::OBJECT_ENTRY, 805),
                    (fields::CONTAINER_FIELD_NUM_SLOTS, 6),
                ],
            ),
            // Deliberately carries a value at the container field's index. An
            // ordinary item would not, but the point is that the *type* is
            // what gates the read, not the field happening to be absent.
            create(
                cloth,
                TYPE_ITEM,
                &[
                    (fields::OBJECT_ENTRY, 4306),
                    (fields::CONTAINER_FIELD_NUM_SLOTS, 99),
                ],
            ),
        ]);

        let held = held(&state, 1);
        let bag = held.iter().find(|i| i.guid == pouch).unwrap();
        let item = held.iter().find(|i| i.guid == cloth).unwrap();
        assert_eq!(bag.capacity, Some(6));
        assert_eq!(
            item.capacity, None,
            "a plain item must not report a capacity even with a value there"
        );
    }

    /// An occupied slot whose object never arrived reads as occupied, not as
    /// empty. A dropped item object is a replication gap and an empty square
    /// is a fact about the character; conflating them makes the first look
    /// like the second and it never gets investigated.
    #[test]
    fn a_slot_without_its_object_is_still_occupied() {
        let mut state = WorldState::new();
        let at = InventorySlot::new(24).unwrap().field();
        state.apply(&[create(1, TYPE_PLAYER, &[(at, 0x1F), (at + 1, 0x4000_0000)])]);

        let held = held(&state, 1);
        assert_eq!(held.len(), 1, "the slot is occupied");
        assert_eq!(held[0].guid, 0x4000_0000_0000_001F);
        assert_eq!(held[0].entry, None, "and we do not know what is in it");
    }

    /// The three views must partition what `held` returns, and each must land
    /// its items at the index the window will draw them at.
    #[test]
    fn the_views_agree_with_the_slot_regions() {
        let (state, player) = world(&[
            (3, 0x4000_0000_0000_0002, 38, 1),
            (15, 0x4000_0000_0000_0008, 49778, 1),
            (19, 0x4000_0000_0000_0023, 805, 1),
            (23, 0x4000_0000_0000_000A, 6948, 1),
            (25, 0x4000_0000_0000_0020, 2592, 5),
        ]);

        let equipped = equipped(&state, player);
        assert_eq!(equipped[3].map(|i| i.entry), Some(Some(38)));
        assert_eq!(equipped[15].map(|i| i.entry), Some(Some(49778)));
        assert!(equipped[0].is_none() && equipped[18].is_none());

        let bags = bags(&state, player);
        assert_eq!(bags[0].map(|i| i.entry), Some(Some(805)));
        assert!(bags[1..].iter().all(|b| b.is_none()));

        let carried = carried(&state, player);
        assert_eq!(carried.len(), 2);
        assert!(carried.iter().all(|i| i.slot.kind() == SlotKind::Backpack));

        // Nothing counted twice, nothing lost.
        let total = equipped.iter().flatten().count()
            + bags.iter().flatten().count()
            + carried.len();
        assert_eq!(total, held(&state, player).len());
    }

    #[test]
    fn a_purse_splits_at_a_hundred_each_way() {
        assert_eq!(purse(0), (0, 0, 0));
        assert_eq!(purse(99), (0, 0, 99));
        assert_eq!(purse(100), (0, 1, 0));
        assert_eq!(purse(9_999), (0, 99, 99));
        assert_eq!(purse(10_000), (1, 0, 0));
        // The number `.modify money` was actually given, and what the field
        // came back holding.
        assert_eq!(purse(123_456), (12, 34, 56));
    }
}
