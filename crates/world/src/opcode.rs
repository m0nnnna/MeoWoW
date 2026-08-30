//! World server opcodes.
//!
//! 3.3.5a defines roughly 1400 of these. Only the ones this client actually
//! sends or recognises are named -- an exhaustive table would be a large block
//! of unverified constants, and an unrecognised opcode is not an error anyway:
//! the server volunteers plenty the client is free to ignore.

/// Client-to-server opcodes.
///
/// On the wire these are 32-bit, unlike the server's 16-bit ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ClientOpcode {
    CharCreate = 0x0036,
    CharEnum = 0x0037,
    CharDelete = 0x0038,
    PlayerLogin = 0x003D,
    Ping = 0x01DC,
    AuthSession = 0x01ED,
    TimeSyncResp = 0x0391,
    /// What this client has selected. The interface could keep a target to
    /// itself, but the server is the one that decides whether a spell or an
    /// attack has a legal victim, so it has to be told.
    SetSelection = 0x013D,

    /// Nothing in an object update carries a name; these are how a client
    /// learns one. Players are asked for by guid, creatures by entry -- every
    /// wolf of a kind shares one answer.
    NameQuery = 0x0050,
    CreatureQuery = 0x0060,
    /// What *is* game object entry N? Body is `{u32 entry, u64 guid}`, the
    /// same shape as [`Self::CreatureQuery`].
    ///
    /// **The first question this client asks about an object's behaviour
    /// rather than its appearance.** A game object has been drawable since
    /// Phase 3 from a display id alone, and a display id says nothing about
    /// what the thing does -- so a mailbox and a mine cart are the same kind
    /// of fact until this is asked. Mail is the first feature that needs the
    /// difference.
    GameObjectQuery = 0x005E,
    /// Interacts with a game object -- opens the door, pulls the lever, takes
    /// the harvest basket. `foss-wow#141`: a quest like *Milly's Harvest*
    /// places a plain object in the world with nothing to loot and nothing
    /// to gossip about, and clicking it was previously indistinguishable
    /// from a click that missed nothing at all, because this client only
    /// ever asked a game object what it *was* ([`Self::GameObjectQuery`]),
    /// never told it to *do* anything.
    ///
    /// **From AzerothCore's `HandleGameObjectUseOpcode`, not yet confirmed
    /// against a live realm.** Body is the target's guid, sent **unpacked**
    /// like [`Self::GossipHello`]'s -- `ObjectGuid::operator>>` reads a plain
    /// `u64`, not the packed form. The server resolves what "use" means
    /// per object (a chest opens loot, a goober grants a quest item, a door
    /// swings) and answers with whatever effect that implies rather than a
    /// reply of its own; the viewer deliberately does not route a mailbox
    /// click through this, since that flow already works without it and
    /// changing a confirmed path to test an unconfirmed one would risk both.
    GameObjectUse = 0x00B1,
    /// What is item entry N? `Item.dbc` carries an item's *model*, and this
    /// client already reads it -- but not its **name**, which is server data
    /// and reaches a client only in answer to this. Every bag square,
    /// equipment slot and loot row showing `item 2224` is waiting on it.
    ///
    /// Keyed by entry like [`Self::CreatureQuery`], so one answer names every
    /// copy of a thing.
    ItemQuerySingle = 0x0056,
    /// Use the thing in this bag slot -- drink the water, eat the bread, go
    /// home on the hearthstone.
    ///
    /// **Not an equip and not a swap**, which is what the bag window used to
    /// do with every right-click: an item with an on-use spell was offered to
    /// the equipment slots, refused, and appeared to do nothing.
    UseItem = 0x00AB,
    MessageChat = 0x0095,
    /// Ask to cast. What comes back is either the world reacting or
    /// `SMSG_CAST_FAILED` explaining why not.
    CastSpell = 0x012E,

    /// Start and stop auto-attacking. Auto-attack is a *state*, not an action:
    /// one swing request starts an exchange the server then drives on its own
    /// timer, which is why there is no per-swing message to send.
    ///
    /// **These two numbers are the only unverified constants in this enum, and
    /// they are verified by reaction rather than by transcription.** Nothing
    /// acknowledges an opcode as such, so the test is that sending
    /// `AttackSwing` at a live hostile produces a stream of combat packets
    /// that was not arriving before, and `AttackStop` ends it. A wrong number
    /// here is worse than a wrong number in a parser -- an outgoing message
    /// can be read as some *other* valid request -- so it was sent first at a
    /// level-one character on a test realm with nothing to lose, and the
    /// reaction checked before either was trusted.
    AttackSwing = 0x0141,
    AttackStop = 0x0142,

    /// Draw or stow the weapon: one `u32` naming a [`crate::SheathState`].
    ///
    /// **Sheathing is a client-side decision, and this is the surprise.** A
    /// whole fight was driven against the realm -- selection, swings landing
    /// both ways, the in-combat flag appearing in `UNIT_FLAGS` -- and byte 0
    /// of `UNIT_FIELD_BYTES_2` never moved off zero. The server does not draw
    /// a weapon for you and never will; it only records what the client says
    /// here and republishes it so *other* players see it.
    ///
    /// Confirmed the way `AttackSwing` was, by varying the input rather than
    /// waiting for a reply: nothing acknowledges this, but sending each state
    /// in turn moves that byte to the matching value and back.
    SetSheathed = 0x01E0,

    /// Drop an aura this character is carrying: one `u32`, the spell id.
    ///
    /// **This is how a toggle is switched off, and re-casting is not.** Stealth
    /// looked like it should toggle -- press it again and stand up -- and
    /// `CMSG_CAST_SPELL` for a spell whose aura you already hold produces
    /// *nothing*: four attempts in one session drew no `SMSG_CAST_FAILED`, no
    /// `SMSG_SPELL_START`, and no opcode of any number. A silence with no
    /// refusal in it is the server declining before it has anything to say,
    /// and it is why the printout that lists every opcode seen was what
    /// identified this rather than a guess about the cast path.
    ///
    /// Nothing acknowledges the send -- every path in the server's handler
    /// returns silently, including the two that matter here (a spell with
    /// `SPELL_ATTR0_NO_AURA_CANCEL`, and any aura that is passive or not
    /// positive). Confirmed by consequence instead, the way `SetSheathed` was:
    /// the stealth bit in `UNIT_FIELD_BYTES_1` and the form byte in
    /// `UNIT_FIELD_BYTES_2` both clear in the next object update, and they
    /// clear only when the spell id names an aura actually held.
    CancelAura = 0x0136,

    /// Wear the item in a given inventory slot, letting the server choose
    /// which equipment slot it belongs in.
    ///
    /// **Confirmed by effect, not by transcription.** Nothing acknowledges
    /// this, and an outgoing number that is wrong is read as some *other*
    /// valid request rather than refused -- the trap `CMSG_ATTACKSWING`
    /// documents. What makes it checkable is that the result is loud: the
    /// item's guid leaves its slot in `PLAYER_FIELD_INV_SLOT_HEAD` and
    /// reappears at an equipment index, and both halves arrive in the next
    /// object update. Sending this and watching a specific guid move between
    /// two specific fields is a statement that could have failed.
    ///
    /// The body is two bytes: the source bag, then the source slot.
    /// [`crate::inventory::OWN_SLOT_ARRAY`] is the bag value meaning "not in a
    /// bag, this is an index into the player's own array".
    ///
    /// Deliberately the *auto* form rather than one naming a destination. The
    /// server picking the slot is what makes this useful twice over: it is the
    /// simplest possible write, and its choice is a fact about the item that
    /// this client would otherwise have to guess at -- which is how the
    /// equipment slot vocabulary beyond the four originally confirmed was
    /// filled in.
    AutoEquipItem = 0x010A,

    /// Auto-store an item into a bag, the server choosing which free square --
    /// the counterpart of [`Self::AutoEquipItem`] for taking gear *off*.
    ///
    /// Body is `{src_bag, src_slot, dst_bag}`, three bytes. `255` for a bag
    /// means the player's own array, as with [`Self::AutoEquipItem`]; with
    /// `src_bag` `255` and `src_slot` an equipped slot index and `dst_bag`
    /// `255`, the worn item lands in the first free backpack square.
    ///
    /// **Live-confirmed as a side effect of `foss-wow#55`** -- see
    /// [`Self::SwapItemCandidate`], which this was mistaken for by exactly
    /// one number. Sent against an equipped source slot the item was watched
    /// leaving the slot and appearing in the first free backpack square, a
    /// real state change the server did not decline.
    AutoStoreBagItem = 0x010B,

    /// Moving an item between two named slots, neither of which the server
    /// chooses -- unlike `AutoEquipItem`.
    ///
    /// Body is `{dst_bag, dst_slot, src_bag, src_slot}`. `255` for a bag
    /// means the player's own array, as with `AutoEquipItem`.
    ///
    /// **`foss-wow#55` is closed: this was the wrong opcode by exactly one,
    /// not a mysterious refusal.** `0x010B` is a different, real request
    /// (`CMSG_AUTOSTORE_BAG_ITEM`, a 3-byte `{src_bag, src_slot, dst_bag}`
    /// body that auto-stores an item into *any* free slot of a bag rather
    /// than a chosen one) -- this client's 4-byte body happened to line its
    /// first three bytes up with that shape closely enough to be read as a
    /// request, with the fourth byte silently unread. Sent against two
    /// occupied slots, byte 1 resolves to a real item and bytes 2-3 name the
    /// player's own array as both the unequip check and the auto-store
    /// target -- which the item is already validly sitting in, so the
    /// "move" is a no-op and the answer is `SMSG_INVENTORY_CHANGE_FAILURE`
    /// code 59, `EQUIP_ERR_NONE`: *there was nothing wrong with the request,
    /// nothing needed to happen.* Sent with an **equipped** source slot the
    /// same request is not a no-op -- unequipping is a real state change --
    /// and it was live-confirmed doing exactly that: the item left the
    /// equipped slot and landed in the first free backpack square, not at
    /// the slot this client asked for. That the destination was ignored is
    /// what named the bug: a real swap honours a *chosen* destination.
    ///
    /// The genuine `CMSG_SWAP_ITEM` sits one number up, at `0x010C`, wants
    /// exactly the four-byte body already built here, and was confirmed live
    /// the same way `AutoEquipItem` was: two backpack items requested by
    /// slot landed at each other's positions, a real two-way swap, nothing
    /// left in between.
    SwapItemCandidate = 0x010C,

    /// Destroys a carried item outright -- what a bag drag that ends nowhere
    /// (not a slot, not a vendor, not any other window) means.
    ///
    /// **From public documentation, not yet confirmed against a live
    /// realm** -- unlike every neighbouring opcode in this block, which was
    /// checked by watching a specific guid move. `0x0111` sits exactly where
    /// the table says it should, one below `SwapItemCandidate` and one above
    /// `INVENTORY_CHANGE_FAILURE` (`0x0112`, already confirmed and parsed in
    /// this crate as the generic refusal every inventory write shares), and
    /// the body is silent on success the same way `BuyItem` and `SellItem`
    /// are; a refusal answers on the shared `INVENTORY_CHANGE_FAILURE`
    /// opcode instead of a reply of its own.
    ///
    /// `{bag, slot, count}` plus three trailing zero bytes the documented
    /// handler reads and never uses -- six in total. Sent whole rather than
    /// as three, deliberately: packets are length-framed, so extra trailing
    /// bytes a handler does not read are merely unread within this one
    /// packet, where three bytes *short* risks the read running past this
    /// packet's own bound if the six-byte body is the real one -- the safer
    /// side to be wrong on between the two candidate lengths.
    ///
    /// What would confirm it the way `SwapItemCandidate` was confirmed: sending it
    /// against a known slot and watching the item actually leave
    /// `PLAYER_FIELD_PACK_SLOT_n` in the next object update, with no
    /// `INVENTORY_CHANGE_FAILURE` in between.
    DestroyItem = 0x0111,

    /// Take one slot off the corpse currently open, letting the server choose
    /// where it goes in the bags.
    ///
    /// Body is a single byte: the **loot slot**, which is the server's index
    /// into that corpse's loot and comes from
    /// [`LootItem::slot`](crate::LootItem). It is not a position in whatever
    /// list a client happens to have built -- a corpse whose first slot has
    /// already been taken still numbers the rest from where they were, so
    /// re-indexing a filtered list would take the wrong item.
    ///
    /// Acts on the loot **currently open**, so it is only meaningful after
    /// [`ClientOpcode::Loot`]. Nothing identifies the corpse in the body.
    ///
    /// Confirmed by effect, the same way the equip write was: the item leaves
    /// the corpse and appears in the player's own slot array.
    AutoStoreLootItem = 0x0108,

    /// Open the loot on a corpse.
    ///
    /// Body is the target's guid, unpacked -- eight bytes, not the packed
    /// form. Unlike most sends here this one is *answered*, which makes it far
    /// cheaper to confirm than the equip write: a reply arriving at all says
    /// the request was understood, and a reply that parses says the layout was
    /// right too.
    Loot = 0x015D,
    /// Take the money off a corpse already opened by [`ClientOpcode::Loot`].
    /// Empty body -- the server knows which corpse is open.
    LootMoney = 0x015E,
    /// Close the loot window, which is what releases the corpse.
    ///
    /// **Not optional.** A corpse stays locked to the looter until this
    /// arrives, so a client that opens loot and never releases it leaves
    /// bodies nobody else can touch.
    LootRelease = 0x015F,

    /// Release the spirit: give up the body and become a ghost at the nearest
    /// graveyard. Carries one byte the server reads and discards.
    ///
    /// Refused in silence when the player is alive or is already a ghost, which
    /// is worth knowing before concluding the opcode is wrong -- the first
    /// attempt at this produced nothing at all because the character had been
    /// killed in an earlier run and was already released.
    RepopRequest = 0x015A,
    /// Take the body back, standing at the corpse. Carries the corpse's guid,
    /// **unpacked** -- eight plain bytes, unlike almost every other guid this
    /// protocol sends.
    ///
    /// Refused in silence unless the player is dead, has released, has a
    /// corpse, is within reclaim range of it, and the reclaim delay has
    /// elapsed. Five ways to get nothing back, none of which says which.
    ReclaimCorpse = 0x01D2,

    /// Accept a spirit healer's resurrection: come back to life *here*, at the
    /// graveyard, with resurrection sickness, instead of running back to the
    /// body. Carries the spirit healer's guid, **unpacked** -- eight plain
    /// bytes, the same encoding [`Self::ReclaimCorpse`] uses.
    ///
    /// Sent only in answer to [`server::SPIRIT_HEALER_CONFIRM`], which the
    /// server volunteers after the `GOSSIP_OPTION_SPIRITHEALER` menu line is
    /// chosen. Nothing acknowledges it directly -- what confirms it is the
    /// ghost flag clearing and health coming back, replicated.
    SpiritHealerActivate = 0x021C,

    // Movement. These are `MSG_` rather than `CMSG_`: the same opcode travels
    // in both directions, the client reporting its own movement and the server
    // relaying someone else's. Only the framing differs -- inbound packets are
    // about a guid that is not ours.
    MoveStartForward = 0x00B5,
    MoveStartBackward = 0x00B6,
    MoveStop = 0x00B7,
    /// Sidestepping. A separate axis from forward and backward, with its own
    /// start and stop: a character can begin strafing without stopping running,
    /// and the opcode names only the axis that changed while the flags carry
    /// the whole state.
    ///
    /// These three are the same three that a real client's capture showed this
    /// project dropping -- `0x00B8`, `0x00B9` and `0x00BA` were among the five
    /// unnamed movement opcodes in `wow-cli moves`, each carrying a body that
    /// parsed as `{packed guid, MovementInfo}` and consumed to the byte. The
    /// capture said *these opcodes are movement*; it could not say which
    /// movement each was, which is why they went unnamed at the time.
    MoveStartStrafeLeft = 0x00B8,
    MoveStartStrafeRight = 0x00B9,
    MoveStopStrafe = 0x00BA,
    MoveJump = 0x00BB,
    /// Turning on the spot, the same shape as strafing: a separate axis with
    /// its own start and stop, confirmed by driving each one against the
    /// local realm and capturing what a second client sees relayed --
    /// `foss-wow#37`.
    MoveStartTurnLeft = 0x00BC,
    MoveStartTurnRight = 0x00BD,
    MoveStopTurn = 0x00BE,
    /// Toggling between a walk and a run. Carries `MovementInfo::flags`'
    /// `WALKING` bit rather than naming a speed -- the server already knows
    /// this character's walk and run speeds from its own state.
    MoveSetRunMode = 0x00C2,
    MoveSetWalkMode = 0x00C3,
    /// The end of a jump or a fall. Carries the total time spent in the air in
    /// `MovementInfo::fall_time`, which is what fall damage is computed from --
    /// so a client that jumps and never lands is one the server believes is
    /// still falling.
    MoveFallLand = 0x00C9,
    /// Entering and leaving liquid deep enough to swim in.
    ///
    /// **Two of the nine opcodes `MOVE_RELAYED` records as unconfirmed, and
    /// the reason it gave was that they "need water" -- which this client did
    /// not have.** Now it does, so they can be driven: the character's feet
    /// against the `MH2O` surface above them is the whole condition, and both
    /// go out carrying `movement_flags::SWIMMING`.
    ///
    /// That flag is not decoration. It adds a **pitch float** to every
    /// `MovementInfo` that carries it -- see `MovementInfo::has_pitch` -- so a
    /// client that sets the flag without emitting the field, or emits the field
    /// without the flag, desynchronises the server's reader mid-packet. The
    /// round trip in `movement.rs` is what keeps the two halves honest.
    MoveStartSwim = 0x00CA,
    MoveStopSwim = 0x00CB,
    MoveSetFacing = 0x00DA,
    MoveHeartbeat = 0x00EE,
    /// Confirming a teleport within the same map. The server sends this
    /// opcode, and the client must send it back before the move takes effect.
    ///
    /// **Not optional, and its absence is silent.** Until the acknowledgement
    /// arrives the server holds the character at the old position *and
    /// discards every movement packet the client sends* -- so a client that
    /// ignores this is frozen where it stood, while believing it is walking.
    /// Found because a released ghost reclaimed its corpse from 58 yards away
    /// when the limit is 39: the ghost had never actually left the body.
    MoveTeleportAck = 0x00C7,
    /// Confirming a transfer to another map after `SMSG_NEW_WORLD`.
    MoveWorldportAck = 0x00DC,

    /// Open a conversation with an NPC: the greeting that produces a menu.
    ///
    /// Body is the target's guid, **unpacked** -- eight plain bytes, the same
    /// shape as [`ClientOpcode::Loot`] and for the same reason.
    ///
    /// **Unconfirmed until something answers it**, and deliberately the first
    /// NPC request attempted for exactly that reason: gossip is *answered*,
    /// where the equip write had to be confirmed by watching a field move. A
    /// reply arriving at all says the number was understood. A silence says
    /// nothing on its own -- it is equally what a wrong opcode, a unit with no
    /// gossip bit, and a unit out of range all look like -- so
    /// `wow-cli world --gossip` refuses to send at range and reports the
    /// target's [`UNIT_NPC_FLAGS`](crate::update::fields::UNIT_NPC_FLAGS)
    /// alongside whatever comes back, to keep those three apart.
    GossipHello = 0x017B,

    /// Choose a line from the menu a greeting produced.
    ///
    /// Body is `{u64 npc guid, u32 menu id, u32 option index}`, followed by a
    /// null-terminated string when the option is a *coded* one -- a bank name,
    /// a guild name -- and by an empty one otherwise.
    ///
    /// **The index is the server's own option id and never a row position.**
    /// It comes from [`GossipOption::index`](crate::GossipOption), and menu
    /// 1291 is the standing proof of why: it has four rows in the database,
    /// the server filters one out for the season, and the three that arrive
    /// are numbered 1, 2 and 3 with the numbering *not* closed up. A client
    /// that sent a row number would ask for the wrong thing, and only at NPCs
    /// whose menus are conditional. The menu id travels too, so the server can
    /// tell which menu the answer belongs to.
    ///
    /// Confirmed by effect and the effect is loud: choosing an innkeeper's
    /// `I want to browse your goods.` produces `SMSG_LIST_INVENTORY`, a
    /// different opcode carrying a stock list, which nothing but a correctly
    /// understood selection would have caused.
    GossipSelectOption = 0x017C,

    /// Ask a vendor what it is selling, without going through a gossip menu.
    ///
    /// Body is the vendor's guid, unpacked. The reply is
    /// [`LIST_INVENTORY`](server::LIST_INVENTORY), the same packet a gossip
    /// option produces -- which is what makes this cheap to confirm: the
    /// answer's layout is already established, so a reply that parses says
    /// the request was understood.
    ///
    /// Worth having separately from the gossip route because a vendor with no
    /// gossip menu at all still sells, and because a client re-opening a shop
    /// window should not have to re-walk a conversation.
    ListInventory = 0x019E,

    /// Sell one item to the open vendor.
    ///
    /// Body is `{u64 vendor guid, u64 item guid, u32 count}` -- twenty bytes,
    /// with `0` for the count meaning "the whole stack". The count is a `u32`
    /// and not the `u8` a stack size would suggest, which is the sort of
    /// detail that produces silence rather than an error when guessed.
    ///
    /// **Confirmed by effect**, since nothing acknowledges it: the item's guid
    /// leaves the player's slot array and `PLAYER_FIELD_COINAGE` goes *up* by
    /// the sell price. Both halves are already read, and both moving together
    /// is a result that could have failed to appear.
    ///
    /// The item is named by **guid**, not by slot. That is the safer of the
    /// two and worth stating: a guid cannot be stale in the way a slot index
    /// can, so a sell request that races an inventory change refuses rather
    /// than selling whatever has moved into that slot.
    SellItem = 0x01A0,

    /// Buy one stack from the open vendor.
    ///
    /// Body is `{u64 vendor guid, u32 item entry, u32 vendor slot, u32 count,
    /// u8}` -- **twenty-one bytes, entry before slot**. The **vendor slot** is
    /// the server's own index from [`VendorItem::slot`](crate::VendorItem) and
    /// not a row position -- the same rule as a loot slot and a gossip option
    /// index -- and it travels alongside the entry so the server can check the
    /// two agree.
    ///
    /// **The first attempt guessed this and got total silence**, which is the
    /// least informative failure available: three bytes short, the two `u32`s
    /// transposed, and the count sent as a `u8`. What bounded the search was
    /// sending [`ClientOpcode::ListInventory`] first -- an *answered* opcode
    /// four below this one in the same block. It came back with 393 bytes of
    /// stock, which said the numbering was right and moved the whole question
    /// onto the body. One cheap answered request to bound a silent one is the
    /// same move that turned three failed attempts at chat into a one-run
    /// answer.
    ///
    /// The trailing byte is a bag slot and is left unnamed here: only `255`
    /// (the player's own array) has been sent.
    ///
    /// Confirmed by effect the same way the sell is: the item appears in the
    /// player's slot array and `PLAYER_FIELD_COINAGE` goes *down* by the
    /// price the stock list quoted -- the **discounted** price, which is what
    /// makes the coinage delta a check on
    /// [`crate::vendor`]'s reading of that field rather than just on the
    /// purchase.
    BuyItem = 0x01A2,

    /// Ask what mark belongs over one NPC's head -- an exclamation for a quest
    /// on offer, a question mark for one ready to hand in, and grey versions
    /// of each for a quest that cannot be taken or finished yet. Body is the
    /// NPC's guid, unpacked.
    ///
    /// **The server decides this, and that is the whole reason to ask.**
    /// Whether a quest is available depends on level, race, class, faction
    /// standing, every prerequisite in its chain and whatever else the realm
    /// has been scripted to check -- a client working it out from
    /// `quest_template` would be reimplementing the server's eligibility rules
    /// and would be wrong on any realm with custom content, which is the case
    /// this client is developed against.
    QuestgiverStatusQuery = 0x0182,

    /// The same question for every questgiver in range at once. Empty body.
    ///
    /// One request per NPC per frame is what a client that does not know this
    /// exists ends up sending; a starting zone has a dozen of them.
    QuestgiverStatusMultipleQuery = 0x0416,

    /// Open a questgiver's list. Body is the NPC's guid, unpacked.
    ///
    /// Distinct from [`ClientOpcode::GossipHello`] even though both greet the
    /// same creature: a questgiver with no gossip menu answers this and not
    /// that, and an NPC with both answers each differently.
    QuestgiverHello = 0x0184,

    /// Ask for one quest's offer text -- the scroll a player reads before
    /// accepting. Body is `{u64 npc guid, u32 quest id, u8}`.
    ///
    /// **Note the trailing byte is a `u8` here and a `u32` in
    /// [`ClientOpcode::QuestgiverAcceptQuest`].** The two requests are
    /// otherwise identical in shape, which makes the asymmetry exactly the
    /// kind of detail that gets guessed wrong once and then copied -- and a
    /// wrong-width trailing field produces silence, not an error.
    QuestgiverQueryQuest = 0x0186,

    /// Take the quest. Body is `{u64 npc guid, u32 quest id, u32}`.
    ///
    /// Confirmed by effect, since nothing acknowledges it directly: the quest
    /// appears in the player's own quest log fields, which are replicated.
    QuestgiverAcceptQuest = 0x0189,

    /// Offer a finished quest for hand-in. Body is `{u64 npc guid, u32 quest
    /// id}` -- no trailing field on this one.
    QuestgiverCompleteQuest = 0x018A,

    /// Take the reward and finish. Body is `{u64 npc guid, u32 quest id, u32
    /// reward index}`, where the index chooses among the optional rewards and
    /// is `0` for a quest that offers none.
    QuestgiverChooseReward = 0x018E,

    /// Ask what a quest actually is: title, objectives, reward list.
    ///
    /// Body is `{u32 quest id}`. **The backbone of the whole quest feature**,
    /// because `SMSG_GOSSIP_MESSAGE` and the questgiver list carry only a
    /// title and a level -- everything else has to be asked for, and there is
    /// no table on disk to read it from.
    ///
    /// There is deliberately **no bulk form**: this takes one id, so
    /// "prefetch everything" would presuppose a list of every quest id, which
    /// is the database this client is not shipping. Ids arrive naturally from
    /// the gossip quest block, the questgiver list and the quest log, and the
    /// answers are cached per realm.
    QuestQuery = 0x005C,

    /// Ask where a quest's objectives are on the map, for up to 25 quests at
    /// once. Body is `{u32 count, u32 quest id * count}`.
    ///
    /// **This is what makes a native quest tracker possible without shipping
    /// anyone's database.** WotLK shipped its own tracker, so the server
    /// already holds the markers and hands them over: per objective a map id,
    /// an area id and a polygon of points.
    ///
    /// **It answers only for quests in the player's own log**, which is the
    /// constraint that shapes the map work: it completely covers "where do I
    /// go for the quest I am on" and says nothing about a quest not yet
    /// accepted.
    QuestPoiQuery = 0x01E3,

    /// Ask the server where this character's body is. Empty request; the
    /// reply shares the opcode.
    ///
    /// The replicated world cannot answer this: corpse-type objects include
    /// the bones of bodies already reclaimed, they all carry their owner's
    /// guid, and a graveyard accumulates them. One live run saw seven while
    /// the server had two.
    CorpseQuery = 0x0216,

    /// Ask a player to join this character's group, **by name**.
    ///
    /// The one request in this enum that names its subject with a string
    /// rather than a guid, and that is not an accident of the protocol: an
    /// invite is the first thing a player does to someone they have only read
    /// off a chat line, who may be on the other side of the continent and so
    /// has never been replicated here at all. A guid-taking invite would only
    /// work on people already on screen.
    ///
    /// Body is the name, NUL-terminated, then a `u32` the server reads and
    /// discards.
    ///
    /// **Answered, which is why it is the party opcode to attempt first.**
    /// Every other request in this block is silent, and a silent write fails
    /// identically whether the number is wrong, the body is wrong or the
    /// server declined -- the trap `CMSG_BUY_ITEM` documents. This one
    /// produces `SMSG_PARTY_COMMAND_RESULT` **whether it succeeds or fails**,
    /// naming the person asked for, so a single send separates "not
    /// understood" from "understood and refused".
    GroupInvite = 0x006E,

    /// Say yes to an invite. Body is a `u32` the server reads and discards --
    /// there is nothing to identify, because a character can hold only one
    /// pending invite at a time.
    ///
    /// Confirmed by effect: `SMSG_GROUP_LIST` arrives at **both** clients
    /// afterwards, which is a packet neither of them was receiving before and
    /// which no misunderstood request could produce.
    GroupAccept = 0x0072,

    /// Say no to an invite. Empty body.
    GroupDecline = 0x0073,

    /// Leave the group -- or, as its leader, break it up. **One opcode for
    /// both**, with an empty body: the server already knows which of the two
    /// the sender is, and there is nothing for the client to decide.
    ///
    /// The name is the server's. What it does for a non-leader is leave.
    GroupDisband = 0x007B,

    /// Throw a member out, naming them by guid.
    ///
    /// The guid form rather than [`Self::GroupUninviteByName`], for the same
    /// reason a loot slot is the server's index: a party list arrives with
    /// guids in it, and re-deriving a name to send back is a round trip
    /// through a string that two players can share.
    ///
    /// Body is the guid then a `u32` (a vote-kick reason string length in
    /// 3.3.5; zero for an ordinary kick).
    GroupUninviteGuid = 0x0076,

    /// Hand leadership to another member, by guid. Body is the guid alone.
    GroupSetLeader = 0x0078,

    /// Changes the party's loot rule. Body is `{u32 method, u64 master
    /// (raw, not packed -- `AzerothCore`'s handler reads it with the same
    /// plain `operator>>` as every other guid this client sends unpacked),
    /// u32 threshold}`. **Silent, like every party request but the invite**:
    /// the server answers by resending `SMSG_GROUP_LIST` with the new rule
    /// in it, or not at all if it declined -- refused server-side for
    /// anyone but the leader, so `Party::is_leader` is checked before this is
    /// ever sent rather than after nothing comes back.
    GroupSetLootMethod = 0x007A,

    /// Throw a member out by name. Kept beside the guid form because the
    /// server accepts both and the two take different bodies; this client
    /// sends the guid form.
    GroupUninviteByName = 0x0075,

    /// Ask a trainer what it will teach. Body is the trainer's guid, unpacked.
    ///
    /// The reply is [`TRAINER_LIST`](server::TRAINER_LIST), so this is an
    /// *answered* request -- the cheap kind, and the reason trainers were
    /// taken before the other five services in this block. A reply that
    /// parses says the opcode and the body were both understood, with none of
    /// the three-way ambiguity a silent send leaves behind.
    ///
    /// Reachable two ways, exactly like [`ClientOpcode::ListInventory`]: a
    /// gossip option meaning "train me" produces the same packet, and an NPC
    /// with no gossip menu still trains.
    TrainerList = 0x01B0,

    /// Learn one spell from the open trainer. Body is `{u64 trainer guid,
    /// u32 spell id}` -- twelve bytes.
    ///
    /// **The spell is named by its id and never by a row position**, which is
    /// the one place this block escapes the trap that loot slots, gossip
    /// option indices and vendor slots all sit in: the server's list is
    /// filtered per character (by class, by race, and by a prerequisite spell
    /// the reader may not have), so a row number would mean different things
    /// to two characters standing at the same NPC. Here there is nothing to
    /// get wrong -- the id travels.
    ///
    /// **Answered on success and silent on refusal**, which is an unusually
    /// good diagnostic shape and worth stating: every failure path in the
    /// server's handler returns without sending anything at all, so the
    /// arrival of [`TRAINER_BUY_SUCCEEDED`](server::TRAINER_BUY_SUCCEEDED)
    /// naming the same spell id is the confirmation, and its absence is a
    /// refusal rather than a misunderstanding. That does mean a wrong opcode
    /// and a declined purchase look alike -- which is why this one is sent
    /// only after [`ClientOpcode::TrainerList`] has already answered from the
    /// same block, the same bounding move that rescued `CMSG_BUY_ITEM`.
    TrainerBuySpell = 0x01B2,

    /// Ask another player to trade. Body is their guid, unpacked.
    ///
    /// **Silent on success and answered on every failure**, which is the
    /// inverse of everything else in this block and is what makes it
    /// confirmable at all. A trade that starts is announced to the *partner*
    /// -- see [`TRADE_STATUS`](server::TRADE_STATUS) -- so the sender learns
    /// nothing until the other client acts. A trade that is refused comes
    /// straight back with a status naming the reason.
    ///
    /// So the bounding send for this whole block is this opcode aimed at a
    /// guid that is **not a player**: the reply is immediate, from one client,
    /// with nobody else logged in, and it confirms the opcode number, the
    /// eight-byte body and the reply layout together. Same move as
    /// `CMSG_LIST_INVENTORY` bounding `CMSG_BUY_ITEM`, except that here the
    /// bounding case is this request's own failure rather than a neighbour's
    /// success.
    InitiateTrade = 0x0116,

    /// Agree to open the window somebody else asked for. Empty body.
    ///
    /// **The packet that makes this milestone different.** Every request
    /// before it was one end asking the server for something; this one exists
    /// only because the far end is a person who has to say yes. Answered by
    /// [`TRADE_STATUS`](server::TRADE_STATUS) carrying `OPEN_WINDOW` **to both
    /// clients**, which is the first time a send from this client has produced
    /// a packet at somebody else's.
    BeginTrade = 0x0117,

    /// Refuse an offer to trade because this client is busy. Empty body.
    ///
    /// Sent instead of [`Self::BeginTrade`] and never alongside it: the
    /// original client sends one of the three answers exactly once, and the
    /// server closes the trade on either refusal.
    BusyTrade = 0x0118,

    /// Refuse an offer to trade from somebody on the ignore list. Empty body.
    ///
    /// Kept beside [`Self::BusyTrade`] and **not sent by this client**, since
    /// there is no ignore list here yet. Named rather than omitted because the
    /// three answers are one decision with three outcomes, and a reader
    /// finding only two would reasonably assume the third does not exist.
    IgnoreTrade = 0x0119,

    /// Accept what is on the table. Body is the token from `OPEN_WINDOW`.
    ///
    /// **The body is unconfirmable here and is sent anyway.** This server's
    /// handler reads nothing from it, so no observation available on this
    /// realm can separate a four-byte body from an empty one -- which means
    /// the choice is made on risk rather than on evidence, and the risk is
    /// asymmetric: a server that checks a minimum size refuses an empty body,
    /// and none refuse a body they ignore. The token is what goes in it
    /// because it is the only number the server has offered that belongs to
    /// this trade.
    ///
    /// Silent on success at the sender. The *partner* gets `TRADE_ACCEPT`, and
    /// both get `TRADE_COMPLETE` once the second accept lands -- so a client
    /// never sees its own accept acknowledged, only its consequences.
    AcceptTrade = 0x011A,

    /// Take an accept back. Empty body. The server answers **the other end**
    /// with `BACK_TO_TRADE`.
    UnacceptTrade = 0x011B,

    /// Call the whole thing off. Empty body.
    ///
    /// Also what the original client sends on logout, which is worth knowing
    /// because it means a trade left open by a disconnect does not linger.
    CancelTrade = 0x011C,

    /// Put one of this character's items on the table. Body is
    /// `{u8 trade slot, u8 bag, u8 slot}` -- three bytes.
    ///
    /// The `(bag, slot)` pair is addressed exactly as
    /// [`ClientOpcode::SwapItemCandidate`] and [`ClientOpcode::UseItem`]
    /// address theirs, so nothing new had to be measured for it.
    ///
    /// **Silent on success, and its effect is a packet at both ends**: the
    /// server answers by sending the whole offer again, to this client as
    /// *your* half and to the partner as *theirs*. That is the confirmation --
    /// an item appearing in the reflected offer is proof the three bytes were
    /// understood, and it is the only proof available.
    SetTradeItem = 0x011D,

    /// Take an item back off the table. Body is one byte, the trade slot.
    ClearTradeItem = 0x011E,

    /// Put money on the table. Body is one `u32` of copper.
    ///
    /// **Refused with `BUSY` when the sender does not have it**, which is the
    /// one place in this block where a request that is normally silent answers
    /// back -- and it answers with a code that means something else entirely
    /// everywhere else it appears. Reported raw for exactly that reason.
    SetTradeGold = 0x011F,

    /// Ask a flight master where it can send you. Body is its guid, unpacked.
    ///
    /// Answered by [`SHOW_TAXI_NODES`](server::SHOW_TAXI_NODES), whose body is
    /// a **fixed 72 bytes** -- which makes this the cheapest confirmation in
    /// the block, since there is no variable-length part to absorb a
    /// misreading.
    TaxiQueryAvailableNodes = 0x01AC,

    /// Fly from one node to another. Body is `{u64 npc, u32 from, u32 to}` --
    /// sixteen bytes, and the node ids are [`dbc::schema::TaxiNodes`] rows
    /// rather than anything the server sent as a list position.
    ///
    /// **Always answered**, accepted or refused, by
    /// [`ACTIVATE_TAXI_REPLY`](server::ACTIVATE_TAXI_REPLY). That is unusual
    /// enough in this project to be the reason flight paths are tractable:
    /// one send bounds the opcode, the body layout and the reply layout
    /// together, the same move `CMSG_GROUP_INVITE` made for parties.
    ///
    /// The `from` node must be the one the server itself named in the menu.
    /// A client that recomputed it from the player's position would be right
    /// almost everywhere and wrong exactly where two factions share a town.
    ActivateTaxi = 0x01AD,

    /// Post a letter. See [`crate::mail::send_mail_body`] for the body, which
    /// is the longest this client builds.
    ///
    /// **Answered either way** by
    /// [`SEND_MAIL_RESULT`](server::SEND_MAIL_RESULT), which is what makes
    /// mail cheap to bound after trade was not: a reply that echoes the
    /// *action* it was for ties itself to its request, so a probe can be wrong
    /// about the body and still learn that the opcode is right.
    SendMail = 0x0238,

    /// Ask a mailbox what is in it. Body is the mailbox's guid, unpacked, like
    /// the vendor's and the trainer's.
    ///
    /// **The guid has to be a mailbox the character can reach** -- a game
    /// object of the mailbox type within interaction range, or an NPC carrying
    /// the mailbox flag. The server also accepts the *reader's own guid* from
    /// a game master, which is a trap rather than a shortcut: every fixture
    /// account on this project's local realm is a game master, so the cheapest
    /// probe available is the one that would ship a client working only for
    /// them. See [`crate::mail`].
    GetMailList = 0x023A,

    /// Take the copper out of one letter. `{u64 mailbox, u32 mail id}`.
    MailTakeMoney = 0x0245,

    /// Take one attachment. `{u64 mailbox, u32 mail id, u32 item low guid}`.
    ///
    /// **The item is named by a bare 32-bit low guid**, which is the third
    /// way this client addresses an item and the only one available here: a
    /// mailed item is not a replicated object, so there is no full guid and no
    /// `(bag, slot)` pair to name it with.
    MailTakeItem = 0x0246,

    /// Mark a letter as opened. `{u64 mailbox, u32 mail id}`.
    ///
    /// **The one request in the mail block that is silent either way**, which
    /// is worth stating because every other one is answered: the server sets
    /// the flag and returns. Its effect is visible only in the *next*
    /// `SMSG_MAIL_LIST_RESULT`, so it is confirmable by re-asking and not at
    /// all by waiting.
    MailMarkAsRead = 0x0247,

    /// Send a letter back where it came from.
    /// `{u64 mailbox, u32 mail id, u64 original sender}` -- and the server
    /// reads the last field and never uses it, taking the sender off its own
    /// copy of the letter.
    MailReturnToSender = 0x0248,

    /// Throw a letter away.
    /// `{u64 mailbox, u32 mail id, u32 mail template id}`.
    ///
    /// **Refused for a letter with cash on delivery on it**, which is the
    /// server protecting the sender rather than the reader: deleting one would
    /// destroy goods somebody is owed money for.
    MailDelete = 0x0249,

    /// Copy a letter's text into a paper item. `{u64 mailbox, u32 mail id}`.
    MailCreateTextItem = 0x024A,

    /// Ask what is waiting, with **no mailbox involved**.
    ///
    /// A `MSG_` opcode: the request and the reply share this number and only
    /// the direction separates them. The request body is empty.
    ///
    /// This is the one thing that stands between "something arrived" and
    /// "walk to a mailbox" -- and it names at most two senders, because the
    /// server stops after two. See [`crate::mail::NextMailTime`].
    QueryNextMailTime = 0x0284,

    /// Ask what a guild is called. Body is the guild id, four bytes.
    ///
    /// **Answered for any guild id, by any character, in or out of a guild**
    /// -- the guild block's counterpart to `CMSG_QUEST_QUERY`, and the only
    /// request here that can name a guild this character has nothing to do
    /// with. A zero id is dropped without a reply, which is the one input that
    /// makes a silence mean something other than a wrong opcode.
    GuildQuery = 0x0054,

    /// Ask somebody to join. Body is their name, and the request is silent on
    /// success at the sender -- what arrives is `SMSG_GUILD_INVITE` at
    /// *them*, which is [`crate::trade`]'s shape: the effect of a request is
    /// visible at the other end.
    ///
    /// It is not entirely silent, though, and that is what makes it usable
    /// alone: every refusal is a
    /// [`COMMAND_RESULT`](server::GUILD_COMMAND_RESULT) naming
    /// [`GuildCommand::INVITE`](crate::guild::GuildCommand::INVITE), and a
    /// success is one too.
    GuildInvite = 0x0082,

    /// Accept the pending invitation. **Empty body** -- the server resolves
    /// which guild from the invitation it recorded, so there is nothing to
    /// name. See [`crate::guild::GuildInvitation`].
    GuildAccept = 0x0084,

    /// Decline the pending invitation. Empty body, and entirely silent: the
    /// server clears its own record and tells nobody, including the inviter.
    GuildDecline = 0x0085,

    /// Ask for the guild's summary -- name, founding date, member and account
    /// counts. Empty body. See [`crate::guild::GuildSummary`].
    GuildInfo = 0x0087,

    /// Ask for the roster. **Empty body, and answered either way**, which is
    /// what bounds the rest of this block: sent by a character in no guild it
    /// comes back as a
    /// [`COMMAND_RESULT`](server::GUILD_COMMAND_RESULT) rather than as
    /// silence, so one send from a character with no fixture at all confirms
    /// this number and that packet's layout together.
    GuildRoster = 0x0089,

    /// Move a member up one rank. Body is their name.
    ///
    /// **A name and not a guid**, so it reaches a member who has been offline
    /// for a month and whom this client has never replicated. See
    /// [`crate::guild::named_player_body`].
    GuildPromote = 0x008B,

    /// Move a member down one rank. Body is their name.
    GuildDemote = 0x008C,

    /// Leave the guild. Empty body.
    ///
    /// **Refused for the guild master**, with the result code that also means
    /// "you do not have permission" -- see
    /// [`GuildResult::PERMISSIONS_OR_LEADER_LEAVE`](crate::guild::GuildResult::PERMISSIONS_OR_LEADER_LEAVE).
    GuildLeave = 0x008D,

    /// Remove somebody else. Body is their name.
    GuildRemove = 0x008E,

    /// Disband the guild. Empty body, guild master only, and irreversible --
    /// which is why nothing in this client's interface sends it.
    GuildDisband = 0x008F,

    /// Hand the guild to somebody else. Body is their name.
    GuildLeader = 0x0090,

    /// Set the message of the day. Body is the text.
    GuildMotd = 0x0091,

    /// Set a member's public note. `{cstring member, cstring note}`.
    GuildSetPublicNote = 0x0234,

    /// Set a member's officer note. Same body as
    /// [`GuildSetPublicNote`](Self::GuildSetPublicNote) exactly, and a
    /// different number -- see [`crate::guild::member_note_body`], which is
    /// the one place both are built, because two copies of a layout that must
    /// agree is precisely the drift this project defines structures once to
    /// avoid.
    GuildSetOfficerNote = 0x0235,

    /// Set the longer information text. Body is the text.
    GuildInfoText = 0x02FC,

    /// Greet an auctioneer. A `MSG_` opcode: the request and the reply share
    /// this number, like [`QueryNextMailTime`](Self::QueryNextMailTime).
    /// Body is the auctioneer's guid, unpacked.
    ///
    /// The reply names the **auction house** the NPC belongs to, which is the
    /// thing a display id and a name cannot say: an auctioneer in Ironforge
    /// and one in Stormwind serve the same house, and one in Booty Bay serves
    /// a third that both factions share. See [`crate::auction::AuctionHouse`].
    AuctionHello = 0x0255,

    /// Post an auction. See [`crate::auction::sell_item_body`].
    ///
    /// **Answered either way** by
    /// [`AUCTION_COMMAND_RESULT`](server::AUCTION_COMMAND_RESULT) -- once the
    /// auctioneer resolves. Before that it is one of the silent ones, and the
    /// difference matters: the server drops this packet without a word when
    /// the guid is not an auctioneer in range, which is indistinguishable
    /// from a wrong opcode.
    AuctionSellItem = 0x0256,

    /// Cancel one of your own auctions. `{u64 auctioneer, u32 auction id}`.
    ///
    /// The goods come back **as mail**, not to the bag, which is why this
    /// milestone can only be checked end to end by a client that already has
    /// an inbox. 4.27 is a prerequisite for confirming 4.30 and neither
    /// milestone's opcodes touch the other's.
    AuctionRemoveItem = 0x0257,

    /// Search. The heaviest body this client builds, and the only request in
    /// the whole protocol that carries a **sort order** -- see
    /// [`crate::auction::list_items_body`].
    ///
    /// The reply is one **page**, and the page is the server's decision. See
    /// [`crate::auction::AuctionPage`] for what that costs a client.
    AuctionListItems = 0x0258,

    /// List what this character is selling. `{u64 auctioneer, u32 offset}`,
    /// and **the offset is read and ignored** -- the owner list has no paging
    /// because it cannot be long enough to need any.
    AuctionListOwnerItems = 0x0259,

    /// Bid. `{u64 auctioneer, u32 auction id, u32 price}`.
    ///
    /// A price equal to the buyout is a **buyout**; there is no separate
    /// opcode for one, which is worth stating because an interface that draws
    /// two buttons is drawing one request twice.
    AuctionPlaceBid = 0x025A,

    /// List what this character is bidding on.
    /// `{u64 auctioneer, u32 offset, u32 count, count * u32 auction id}`.
    ///
    /// The trailing ids are auctions the client believes it has been outbid
    /// on, and the server merely looks each one up and adds it to the reply
    /// -- so a client that sends none gets the same list minus nothing it did
    /// not already know about. AzerothCore's own comment on the field is
    /// *"which I'm honestly not entirely sure why?"*.
    AuctionListBidderItems = 0x0264,

    /// Ask for sales awaiting collection. Body is eight bytes the server
    /// **reads and discards**.
    ///
    /// **This is the auction block's bounding instrument**, and it is a
    /// stronger one than any other city service got. `CMSG_GUILD_ROSTER` is
    /// answered without a guild but still needs a character;
    /// `CMSG_TRAINER_LIST` needs a trainer standing in front of you. This
    /// handler checks *nothing at all* -- not the auctioneer, not the range,
    /// not the level, not the body it just read -- and always replies with
    /// [`AUCTION_LIST_PENDING_SALES`](server::AUCTION_LIST_PENDING_SALES).
    /// So one send, from anywhere in the world, with no fixture, separates
    /// "this client cannot talk to the auction house" from "there is nothing
    /// to talk about". Every other request in this block is conditional on an
    /// NPC and silent when the condition fails.
    AuctionListPendingSales = 0x048F,
}

/// The server-to-client opcodes this client reacts to.
///
/// Anything not listed is passed through as a raw number; see [`describe`].
pub mod server {
    pub const CHAR_ENUM: u16 = 0x003B;
    pub const PONG: u16 = 0x01DD;
    pub const AUTH_CHALLENGE: u16 = 0x01EC;
    pub const AUTH_RESPONSE: u16 = 0x01EE;
    pub const TUTORIAL_FLAGS: u16 = 0x00FD;
    pub const ADDON_INFO: u16 = 0x02EF;
    pub const CLIENTCACHE_VERSION: u16 = 0x04AB;
    pub const LOGIN_VERIFY_WORLD: u16 = 0x0236;
    /// The destination map and position for a world transfer.
    pub const NEW_WORLD: u16 = 0x003E;
    /// Announces that a world transfer is about to begin.
    pub const TRANSFER_PENDING: u16 = 0x003F;
    /// Cancels a world transfer before the destination is entered.
    pub const TRANSFER_ABORTED: u16 = 0x0040;

    /// What is on a corpse, in answer to `CMSG_LOOT`. See [`crate::loot`] for
    /// the layout and how it was confirmed.
    pub const LOOT_RESPONSE: u16 = 0x0160;
    /// Which corpse was closed. **Also arrives in answer to `CMSG_LOOT`** when
    /// the corpse is empty: the server closes the window rather than sending
    /// an empty one, which is worth knowing before treating it as an
    /// acknowledgement of a release this client sent.
    pub const LOOT_RELEASE_RESPONSE: u16 = 0x0161;

    /// One slot is gone from the open corpse. **One byte: the loot slot.**
    ///
    /// Without this a loot window keeps showing rows that have already been
    /// taken, and -- because it never empties -- never closes, so the corpse
    /// is never released. That was the symptom that found it.
    ///
    /// Identified by content rather than by its number: taking loot slot `0`
    /// produced a one-byte body holding `0`.
    pub const LOOT_REMOVED: u16 = 0x0162;

    /// The money is gone from the open corpse. **Empty body** -- the corpse is
    /// already known, and there is only one pile.
    ///
    /// A zero-length body is itself the identification here: nothing else
    /// arriving at that moment carries no payload at all.
    pub const LOOT_CLEAR_MONEY: u16 = 0x0165;

    /// What an NPC says when greeted: a menu of text, options and quests. See
    /// [`crate::gossip`] for the layout and how every field of it was checked
    /// against the server's own database.
    ///
    /// **Identified by content**, like [`LOOT_REMOVED`]: greeting an Innkeeper
    /// Farley produced a body carrying his menu id, his three menu options and
    /// their icons, all of which the database independently agrees with.
    pub const GOSSIP_MESSAGE: u16 = 0x017D;

    /// A vendor's stock list. See [`crate::vendor`] for the layout and how
    /// every field of it was checked.
    ///
    /// **Identified by content and by cause at once.** It arrived in answer to
    /// a gossip option reading `I want to browse your goods.`, so a reply at
    /// all confirms `CMSG_GOSSIP_SELECT_OPTION`; and its body holds exactly
    /// the twelve rows the server's own `npc_vendor` table lists for that
    /// creature, in order, each pairing an item entry with the display id
    /// `Item.dbc` independently gives it.
    pub const LIST_INVENTORY: u16 = 0x019F;

    /// What a trainer will teach: a list of spells with a price, a level and
    /// a per-character availability state, followed by the trainer's greeting.
    /// See [`crate::trainer`] for the layout and how the record stride was
    /// measured.
    ///
    /// **Identified by the greeting**, which is the strongest kind of evidence
    /// available in a binary format and the same move that settled the M2
    /// event stride: the string at the end of this body is the *name* of
    /// something, and a wrong record stride puts the reader in the middle of a
    /// number instead of at the start of a sentence.
    pub const TRAINER_LIST: u16 = 0x01B1;

    /// One spell has been learned, in answer to
    /// [`TrainerBuySpell`](crate::ClientOpcode::TrainerBuySpell). Body is
    /// `{u64 trainer guid, u32 spell id}`, echoing back what was asked for.
    ///
    /// **This is the only reply the purchase gets.** The server's handler
    /// returns silently on every refusal -- no such spell, not enough money,
    /// already known, level too low -- so the presence of this packet is the
    /// success and its absence is the failure. See
    /// [`TrainerBuySpell`](crate::ClientOpcode::TrainerBuySpell).
    pub const TRAINER_BUY_SUCCEEDED: u16 = 0x01B3;

    /// What just happened to the trade: a `u32` status, then a tail **whose
    /// shape the status decides**. See [`crate::trade`].
    ///
    /// **Identified by its refusal.** Aiming `CMSG_INITIATE_TRADE` at a guid
    /// that is not a player answers with this opcode carrying `NO_TARGET`,
    /// which is a reply arriving at one client with nobody else in the world
    /// -- so the opcode number, the request body and this layout are all
    /// bounded by a single send that needs no second person. Every other
    /// packet in the block then arrives during a trade that this one has
    /// already proved is being understood.
    ///
    /// The conditional tail is confirmed a second way, by length: a `BEGIN`
    /// body is twelve bytes and carries the initiator's guid, an `OPEN_WINDOW`
    /// is eight, and the rest are four. A reader treating the body as a bare
    /// `u32` leaves eight bytes unread on the first one, and the cursor
    /// refuses it.
    pub const TRADE_STATUS: u16 = 0x0120;

    /// One side's half of the open trade -- seven slots, the money, and a
    /// leading byte saying **whose half it is**. A fixed 532 bytes.
    ///
    /// The fixed size is its own confirmation, like `SMSG_SHOWTAXINODES`: a
    /// misread field width cannot be absorbed by anything variable-length, so
    /// it shows up as leftovers on the first capture. The seven records each
    /// begin with their own index, which localises a stride error to a record
    /// instead of to the packet.
    pub const TRADE_STATUS_EXTENDED: u16 = 0x0121;

    /// Where a flight master can send you: the node you are standing at, and
    /// a bit array of every node this character has visited. See
    /// [`crate::taxi`].
    ///
    /// **A fixed 72-byte body**, which is its own confirmation -- nothing
    /// variable-length can absorb a wrong mask width, so a misreading shows
    /// up as leftover bytes on the first packet.
    pub const SHOW_TAXI_NODES: u16 = 0x01A9;

    /// Whether a flight was accepted, in answer to
    /// [`ActivateTaxi`](crate::ClientOpcode::ActivateTaxi). One `u32`.
    ///
    /// Arrives either way, which is what makes the request above confirmable
    /// in one send rather than by effect.
    pub const ACTIVATE_TAXI_REPLY: u16 = 0x01AE;

    /// A route the server is adding to the player's known set mid-session.
    /// **Deliberately unparsed** -- nothing has produced one, and the menu is
    /// re-asked on every visit anyway.
    pub const NEW_TAXI_PATH: u16 = 0x01AF;

    /// What just happened to a mail request. See [`crate::mail::MailResult`].
    ///
    /// **Answers nearly every request in the block**, and echoes the action it
    /// was for -- which is the cheap bounding instrument trade did not have.
    /// The tail is conditional on the *result* first and the action second, so
    /// a take that failed on inventory space carries the error instead of the
    /// item rather than as well as it.
    pub const SEND_MAIL_RESULT: u16 = 0x0239;

    /// The inbox: a total, a count, and that many variable-length records. See
    /// [`crate::mail::parse_inbox`].
    ///
    /// **The total and the count are different numbers** -- the list is capped
    /// at fifty letters and again by the packet size -- so a client drawing
    /// the number of rows it received reports the wrong figure to exactly the
    /// people whose mailbox is full.
    pub const MAIL_LIST_RESULT: u16 = 0x023B;

    /// **Mail arrived.** Four bytes, and they are zero.
    ///
    /// The first packet this client reads that answers nothing: it exists
    /// because somebody else acted, at a moment with nothing outstanding to
    /// correlate it against. It carries no sender, no subject and no count, so
    /// the only honest thing to draw on receiving one is that something is
    /// unread. See [`crate::mail`].
    pub const RECEIVED_MAIL: u16 = 0x0285;

    /// The reply half of [`QueryNextMailTime`](crate::ClientOpcode::QueryNextMailTime),
    /// sharing its number. See [`crate::mail::NextMailTime`].
    pub const QUERY_NEXT_MAIL_TIME: u16 = 0x0284;

    /// Which mailbox the server has just opened, when one was reached through
    /// a gossip menu. Eight bytes, the object's guid.
    pub const SHOW_MAILBOX: u16 = 0x0297;

    /// The reply half of [`AuctionHello`](crate::ClientOpcode::AuctionHello),
    /// sharing its number: `{u64 auctioneer, u32 house id, u8 enabled}`.
    ///
    /// **The only thing here that names a house.** Every other auction packet
    /// is silent about which of the three sets of goods it belongs to, and a
    /// client that greeted an auctioneer in Booty Bay and then searched at one
    /// in Stormwind would get a different world with no packet ever saying so.
    pub const AUCTION_HELLO: u16 = 0x0255;

    /// The answer to a post, a bid or a cancellation:
    /// `{u32 auction id, u32 action, u32 error}`, and **a fourth word only
    /// when the error is zero and the action is not**. See
    /// [`crate::auction::AuctionOutcome`].
    pub const AUCTION_COMMAND_RESULT: u16 = 0x025B;

    /// One **page** of a search. See [`crate::auction::parse_auction_page`].
    pub const AUCTION_LIST_RESULT: u16 = 0x025C;

    /// Everything this character is selling, in the same shape as
    /// [`AUCTION_LIST_RESULT`] -- and its `total` always equals its `count`,
    /// which is the cheapest confirmation available that the trailing word is
    /// a total rather than something else.
    pub const AUCTION_OWNER_LIST_RESULT: u16 = 0x025D;

    /// Everything this character is bidding on, same shape again.
    pub const AUCTION_BIDDER_LIST_RESULT: u16 = 0x0265;

    /// Somebody has outbid this character, or this character's bid has won.
    /// Arrives **unprompted**, like [`RECEIVED_MAIL`] -- the second packet in
    /// this client that answers nothing, and the first one that says what
    /// happened rather than merely that something did.
    pub const AUCTION_BIDDER_NOTIFICATION: u16 = 0x025E;

    /// One of this character's auctions has sold. Also unprompted.
    pub const AUCTION_OWNER_NOTIFICATION: u16 = 0x025F;

    /// An auction is gone. Unprompted, and this server never sends it.
    pub const AUCTION_REMOVED_NOTIFICATION: u16 = 0x028D;

    /// Sales awaiting collection, in answer to
    /// [`AuctionListPendingSales`](crate::ClientOpcode::AuctionListPendingSales).
    ///
    /// **Always sent, and on this realm always empty** -- a `u32` zero and
    /// nothing else, because the server's loop over the records is commented
    /// out in its own source. That is exactly what makes it the block's
    /// bounding instrument: a reply whose *content* is fixed cannot be
    /// mistaken for a reply that depended on state.
    pub const AUCTION_LIST_PENDING_SALES: u16 = 0x0490;

    /// A guild's name, rank names and tabard, in answer to
    /// [`GuildQuery`](crate::ClientOpcode::GuildQuery).
    ///
    /// **Ten rank names always travel** and the count saying how many are real
    /// arrives after them and after the tabard. See [`crate::guild`].
    pub const GUILD_QUERY_RESPONSE: u16 = 0x0055;

    /// Somebody has asked this character to join their guild. Two names and no
    /// guid at all. See [`crate::guild::GuildInvitation`].
    pub const GUILD_INVITE: u16 = 0x0083;

    /// The guild's summary -- name, founding date, member and account counts.
    /// See [`crate::guild::GuildSummary`], and note the date is a *calendar
    /// reading* rather than a timestamp.
    pub const GUILD_INFO: u16 = 0x0088;

    /// The whole roster: motd, information text, the rank table and every
    /// member.
    ///
    /// **The member record's four-byte offline field exists only for members
    /// who are offline**, which is a conditional layout in the middle of a
    /// variable-length record inside a list. See [`crate::guild`] for why the
    /// fixture that can refute it has to hold both kinds of member.
    pub const GUILD_ROSTER: u16 = 0x008A;

    /// Something happened to the guild -- a sign-on, a promotion, a new motd.
    ///
    /// The only push in the block, and the reason the roster is not polled.
    /// Its trailing guid is conditional on the event type. See
    /// [`crate::guild::GuildEvent`].
    pub const GUILD_EVENT: u16 = 0x0092;

    /// What the server said about a guild request. **Echoes the command it is
    /// about**, which is what ties an answer to a question, and is what bounds
    /// every silent request in this block. See [`crate::guild::CommandResult`].
    pub const GUILD_COMMAND_RESULT: u16 = 0x0093;

    /// What mark belongs over one NPC's head, in answer to
    /// [`QuestgiverStatusQuery`](crate::ClientOpcode::QuestgiverStatusQuery).
    pub const QUESTGIVER_STATUS: u16 = 0x0183;
    /// The same for every questgiver in range, in answer to
    /// [`QuestgiverStatusMultipleQuery`](crate::ClientOpcode::QuestgiverStatusMultipleQuery).
    pub const QUESTGIVER_STATUS_MULTIPLE: u16 = 0x0417;
    /// The quests one NPC is offering, in answer to
    /// [`QuestgiverHello`](crate::ClientOpcode::QuestgiverHello).
    pub const QUESTGIVER_QUEST_LIST: u16 = 0x0185;
    /// One quest's offer text, objectives and rewards -- the scroll shown
    /// before accepting.
    pub const QUESTGIVER_QUEST_DETAILS: u16 = 0x0188;
    /// What a quest still wants before it can be handed in.
    pub const QUESTGIVER_REQUEST_ITEMS: u16 = 0x018B;
    /// The reward screen for a quest ready to turn in.
    pub const QUESTGIVER_OFFER_REWARD: u16 = 0x018D;
    /// The quest is done. Arrives after the reward is chosen.
    pub const QUESTGIVER_QUEST_COMPLETE: u16 = 0x0191;
    /// Everything about one quest, in answer to
    /// [`QuestQuery`](crate::ClientOpcode::QuestQuery).
    pub const QUEST_QUERY_RESPONSE: u16 = 0x005D;
    /// Objective map markers, in answer to
    /// [`QuestPoiQuery`](crate::ClientOpcode::QuestPoiQuery).
    pub const QUEST_POI_QUERY_RESPONSE: u16 = 0x01E4;

    /// What the sky is doing: a state, an intensity, and whether it changed
    /// abruptly. Sent on entering a zone and whenever the zone's weather turns.
    pub const WEATHER: u16 = 0x02F4;

    /// Sent to the character it happened to, and nobody else -- see
    /// [`crate::levelup`].
    pub const LEVELUP_INFO: u16 = 0x01D4;

    /// The breath, fatigue and lava bars -- see [`crate::environment`].
    ///
    /// **These are how a client learns that standing in lava costs anything.**
    /// The server owns the whole calculation: it reads the liquid under the
    /// character out of its own copy of the terrain, runs the timer, and sends
    /// the damage. Nothing this client draws or sends causes any of it, which
    /// is why the swim code changes no health and this block only reads.
    pub const START_MIRROR_TIMER: u16 = 0x01D9;
    pub const PAUSE_MIRROR_TIMER: u16 = 0x01DA;
    pub const STOP_MIRROR_TIMER: u16 = 0x01DB;
    /// One tick of drowning, falling, lava or slime damage.
    pub const ENVIRONMENTAL_DAMAGE_LOG: u16 = 0x01FC;
    pub const CHAR_CREATE: u16 = 0x003A;
    pub const CHAR_DELETE: u16 = 0x003C;
    /// Login refused after the character was chosen, unlike the auth-stage
    /// refusals; carries its own reason code.
    pub const CHARACTER_LOGIN_FAILED: u16 = 0x0041;
    pub const UPDATE_OBJECT: u16 = 0x00A9;
    pub const DESTROY_OBJECT: u16 = 0x00AA;
    /// The same payload as [`UPDATE_OBJECT`], zlib-deflated behind a length.
    pub const COMPRESSED_UPDATE_OBJECT: u16 = 0x01F6;
    /// The server asks periodically; ignoring it eventually drops the session.
    pub const TIME_SYNC_REQ: u16 = 0x0390;
    pub const MOTD: u16 = 0x033D;
    pub const ACCOUNT_DATA_TIMES: u16 = 0x0209;
    pub const LOGIN_SETTIMESPEED: u16 = 0x0042;
    /// A creature following a server-computed path. The most common packet in
    /// a populated zone by a wide margin.
    pub const MONSTER_MOVE: u16 = 0x00DD;
    /// Relayed movement from another mover, sharing the client opcodes.
    pub const MOVE_START_FORWARD: u16 = 0x00B5;
    pub const MOVE_STOP: u16 = 0x00B7;
    pub const MOVE_HEARTBEAT: u16 = 0x00EE;

    /// Every `MSG_MOVE_*` that carries a plain `{packed guid, MovementInfo}`
    /// and is relayed to the players who can see the mover.
    ///
    /// **This client read three of these twenty-four and threw the rest away.**
    /// The gap was found by watching a real 3.3.5a client walk about and asking
    /// `wow-cli moves` which opcodes in the capture had that shape: five more
    /// turned up, and `MSG_MOVE_SET_FACING` alone was 93% of that client's
    /// entire movement stream. Every one of 1,202 packets across those five
    /// parsed as a packed guid followed by a `MovementInfo` and consumed its
    /// body **exactly**, and every one named the single player in view -- where
    /// the control in the same capture, `MONSTER_MOVE`, managed 75 of 944 with
    /// 358 left over across 23 different guids.
    ///
    /// The list is completed from the server's own dispatch table rather than
    /// from the five that happened to be observed, because "which opcodes
    /// exist" is a question about the protocol and not about what a particular
    /// player did in three minutes -- nobody swam, pitched or flew during that
    /// capture, and those bodies are the same shape regardless. The five
    /// confirmed by observation are `START_BACKWARD`, `START_STRAFE_LEFT`,
    /// `START_STRAFE_RIGHT`, `STOP_STRAFE` and `SET_FACING`, plus `FALL_LAND`
    /// in an earlier run and the three already handled.
    ///
    /// **`foss-wow#37` closed six more of the fifteen that were left**, driven
    /// against a local realm rather than merely observed: `JUMP`,
    /// `START_TURN_LEFT`, `START_TURN_RIGHT`, `STOP_TURN`, `SET_RUN_MODE` and
    /// `SET_WALK_MODE`, each captured by a second client and each scoring the
    /// same 100%/0-refused/one-mover result the original five did --
    /// `wow-cli world --turn <left|right>` and `--run-mode <run|walk>` are the
    /// tools that sent them, and `crates/world/src/client.rs`'s
    /// `turn_in_place`/`set_run_mode` are what building them required, since
    /// nothing before this ticket could send a turn or a run/walk toggle at
    /// all -- `crates/world/src/motion.rs`'s `Motion` only ever modelled the
    /// two translation axes.
    ///
    /// **The other nine stay unconfirmed, and deliberately so rather than
    /// silently.** `START_PITCH_UP`, `START_PITCH_DOWN`, `STOP_PITCH` and
    /// `SET_PITCH` need swimming or flight to produce at all; `START_SWIM`
    /// and `STOP_SWIM` need water; `START_ASCEND`, `STOP_ASCEND` and
    /// `START_DESCEND` need a flying mount. None of those states exist
    /// anywhere in this client yet -- not the terrain-height-vs-water-level
    /// check a swim needs, not a mount, not a pitch field on any outgoing
    /// packet -- so driving them is new client capability, not a test. A
    /// level-one human warrior standing on dry land cannot reach any of the
    /// nine, which is the outcome the ticket that closed the other six
    /// explicitly allowed for.
    ///
    /// The discriminator worth keeping is that `MSG_` travels in both
    /// directions and `CMSG_` does not: `CMSG_MOVE_FALL_RESET`,
    /// `CMSG_MOVE_SET_FLY` and `CMSG_MOVE_CHNG_TRANSPORT` share the same
    /// handler and are deliberately **not** here, because nothing relays them.
    pub const MOVE_RELAYED: [u16; 24] = [
        0x00B5, // START_FORWARD
        0x00B6, // START_BACKWARD
        0x00B7, // STOP
        0x00B8, // START_STRAFE_LEFT
        0x00B9, // START_STRAFE_RIGHT
        0x00BA, // STOP_STRAFE
        0x00BB, // JUMP
        0x00BC, // START_TURN_LEFT
        0x00BD, // START_TURN_RIGHT
        0x00BE, // STOP_TURN
        0x00BF, // START_PITCH_UP
        0x00C0, // START_PITCH_DOWN
        0x00C1, // STOP_PITCH
        0x00C2, // SET_RUN_MODE
        0x00C3, // SET_WALK_MODE
        0x00C9, // FALL_LAND
        0x00CA, // START_SWIM
        0x00CB, // STOP_SWIM
        0x00DA, // SET_FACING
        0x00DB, // SET_PITCH
        0x00EE, // HEARTBEAT
        0x0359, // START_ASCEND
        0x035A, // STOP_ASCEND
        0x03A7, // START_DESCEND
    ];

    /// Answers to the two name queries. Neither is guaranteed to arrive: the
    /// server simply does not reply to a guid it has forgotten, which is why
    /// the name cache has to time requests out rather than wait.
    pub const NAME_QUERY_RESPONSE: u16 = 0x0051;
    pub const CREATURE_QUERY_RESPONSE: u16 = 0x0061;
    /// The answer to [`ClientOpcode::GameObjectQuery`]. Carries the object's
    /// **type**, which is the only thing on the wire that separates a mailbox
    /// from anything else with a model. See
    /// [`crate::query::parse_gameobject_query_response`].
    pub const GAMEOBJECT_QUERY_RESPONSE: u16 = 0x005F;
    /// The answer to [`ClientOpcode::ItemQuerySingle`] -- see
    /// [`crate::query::parse_item_query_response`], which parses it whole.
    pub const ITEM_QUERY_SINGLE_RESPONSE: u16 = 0x0058;
    pub const MESSAGECHAT: u16 = 0x0096;
    /// The spellbook, sent unprompted during the login burst. There is no
    /// query for it: miss the packet and the character appears to know nothing.
    /// The refusal to `ClientOpcode::SwapItemCandidate` and `AutoEquipItem`
    /// alike -- see [`ClientOpcode::SwapItemCandidate`] for how its 18-byte
    /// shape was confirmed against a live realm (`foss-wow#55`).
    pub const INVENTORY_CHANGE_FAILURE: u16 = 0x0112;
    pub const INITIAL_SPELLS: u16 = 0x012A;
    /// Where a rebind (hearthstone, innkeeper) sends the player. Sent once
    /// unprompted during the login burst -- `SendInitialPacketsBeforeAddToMap`
    /// on the server -- and again on every rebind, so a client that keeps only
    /// the latest is always current. Body is `x, y, z: f32`, then `map_id,
    /// area_id: u32`; see [`crate::query::parse_bind_point_update`].
    pub const BIND_POINT_UPDATE: u16 = 0x0155;
    pub const CAST_FAILED: u16 = 0x0130;
    pub const SPELL_START: u16 = 0x0131;
    pub const SPELL_GO: u16 = 0x0132;
    pub const SPELL_COOLDOWN: u16 = 0x0134;

    /// Everything sitting on one unit, replacing whatever was known about it.
    /// See [`crate::aura`].
    ///
    /// **Both aura opcodes arrived in every capture this project has ever
    /// taken and were logged as bare numbers for six milestones** -- 56 of
    /// them in one eight-second window. That is the instrument working:
    /// "printed every opcode seen, decoded or not" is what made them findable
    /// the moment there was a reason to want them, rather than a discovery
    /// that they existed at all.
    pub const AURA_UPDATE_ALL: u16 = 0x0495;

    /// One slot on one unit, amending rather than replacing. A `spell id` of
    /// zero empties the slot.
    ///
    /// **The number is one higher than `_ALL`, which is the wrong way round
    /// from the names** -- `SMSG_AURA_UPDATE_ALL` is `0x0495` and
    /// `SMSG_AURA_UPDATE` is `0x0496`. Worth stating because the opcode is the
    /// only thing that separates two byte-identical bodies, and swapping them
    /// makes a single-slot update erase every other aura the unit has.
    pub const AURA_UPDATE: u16 = 0x0496;

    /// Melee. All three arrived in one capture of a level-one warrior fighting
    /// a wolf, and each is named for what its body turned out to hold rather
    /// than from a table -- see [`crate::combat`].
    pub const ATTACK_START: u16 = 0x0143;
    pub const ATTACK_STOP: u16 = 0x0144;
    /// One swing landing or missing. The workhorse of combat: fifteen of these
    /// in a fight that lasted under a minute.
    pub const ATTACKER_STATE_UPDATE: u16 = 0x014A;
    /// Two empty-bodied refusals that arrive when a swing cannot happen. Both
    /// were produced by attacking from out of range while facing away, three
    /// times each, and *which* is which is **still** not established --
    /// `foss-wow#32` varied range and facing one at a time (`wow-cli world
    /// --swing-probe a|b|c`) specifically to separate them and could not:
    /// a swing wrong on exactly one axis produced **no reply at all**, nine
    /// times out of nine, where the control (right on both) produced
    /// `ATTACK_START` and real swings every time. Neither `A` nor `B` is
    /// what a single wrong condition sends -- the one instance of `A` seen
    /// across every probe run arrived on the *first* tick of an otherwise
    /// successful control attempt, which reads as a timing race (the
    /// server's own idea of position or facing had not caught up to a swing
    /// sent immediately after arriving) rather than a condition this pair
    /// of names was meant to describe. Left unrenamed rather than guessed at.
    pub const ATTACK_SWING_REFUSED_A: u16 = 0x0145;
    pub const ATTACK_SWING_REFUSED_B: u16 = 0x0146;
    /// One unit's power changing without a whole object update behind it.
    /// Confirmed by its last captured value agreeing with what the
    /// object-update path independently reported -- see
    /// [`crate::update::PowerUpdate`].
    pub const POWER_UPDATE: u16 = 0x0480;
    /// A rogue's (or a druid's, in cat form) combo points changing: a packed
    /// guid naming the target they are stacked against, then a `u8` count.
    /// There is no field for this in the ordinary object update -- combo
    /// points are private to the owner, so this is the only place they ever
    /// appear on the wire. See [`crate::combat::parse_combo_points`].
    pub const UPDATE_COMBO_POINTS: u16 = 0x039D;
    /// Who is on a unit's threat list. See [`crate::combat::ThreatUpdate`].
    /// Damage from a spell rather than a swing. Captured from a Wrath cast at
    /// a Young Nightsaber; see `combat::parse_spell_damage`.
    pub const SPELL_NON_MELEE_DAMAGE_LOG: u16 = 0x0250;
    pub const THREAT_UPDATE: u16 = 0x0483;

    /// The same body as [`MESSAGECHAT`], sent for a GM's lines.
    pub const GM_MESSAGECHAT: u16 = 0x03B3;

    /// The server moving this character within the current map, and asking to
    /// be told the client noticed: `{packed guid, u32 counter, MovementInfo}`.
    /// See [`crate::ClientOpcode::MoveTeleportAck`] for why answering matters.
    pub const MOVE_TELEPORT_ACK: u16 = 0x00C7;

    /// The answer to [`crate::ClientOpcode::CorpseQuery`], sharing its
    /// number. See [`crate::death::parse_corpse_query`].
    pub const CORPSE_QUERY: u16 = 0x0216;

    /// Where to run back to: `{u32 map, f32 x, f32 y, f32 z}` naming the
    /// graveyard a released ghost was sent to.
    ///
    /// **This is why the corpse run needs no `WorldSafeLocs.dbc`.** The
    /// obvious reading of a graveyard run is that the client picks the nearest
    /// graveyard out of the table and shows it; the server picks it and says
    /// which, so the table is only needed to put a *name* on the place. A map
    /// of `0xFFFFFFFF` with three zeroes is the same packet used to take the
    /// marker back off the minimap, which is what arrives on resurrection.
    pub const DEATH_RELEASE_LOC: u16 = 0x0378;
    /// Sent on death: how long before the corpse can be reclaimed, in
    /// milliseconds. Observed carrying exactly 30000.
    pub const CORPSE_RECLAIM_DELAY: u16 = 0x0269;

    /// The spirit healer offering to bring this character back to life at the
    /// graveyard. Eight bytes, the healer's guid, unpacked.
    ///
    /// Arrives after the graveyard spirit healer's gossip line is chosen --
    /// the server casts spell 17251 on the healer, whose only effect is to
    /// send this. The client answers with
    /// [`crate::ClientOpcode::SpiritHealerActivate`] to accept. See
    /// [`crate::death::parse_spirit_healer_confirm`].
    pub const SPIRIT_HEALER_CONFIRM: u16 = 0x0222;
    /// A unit's threat list dropping someone: `{packed guid, packed guid}`,
    /// the list's owner then whoever left it. Arrives twice on death, once per
    /// creature that had us.
    pub const THREAT_REMOVE: u16 = 0x0484;
    /// The fight ending because one side stopped existing. Empty body.
    pub const CANCEL_COMBAT: u16 = 0x014E;
    /// Equipment losing durability from dying. Not parsed.
    pub const DURABILITY_DAMAGE_DEATH: u16 = 0x02BD;

    /// Somebody has asked this character to join their group. See
    /// [`crate::group::parse_group_invite`].
    pub const GROUP_INVITE: u16 = 0x006F;
    /// The whole group, as it stands, sent to every member whenever anything
    /// about it changes. See [`crate::group`] -- this is the packet the party
    /// interface is drawn from, and the only one that has to be right.
    pub const GROUP_LIST: u16 = 0x007D;
    /// The answer to a group *command*: which operation, who it was about,
    /// and whether it worked. See [`crate::group::parse_party_command_result`].
    pub const PARTY_COMMAND_RESULT: u16 = 0x007F;
    /// One member's health, power, level or zone changing while they are out
    /// of visibility range. See [`crate::group::parse_party_member_stats`].
    pub const PARTY_MEMBER_STATS: u16 = 0x007E;
    /// The same fields, sent unprompted when a member first comes into the
    /// group rather than in response to a change. Same layout, different
    /// number, and it carries a leading byte the other does not.
    pub const PARTY_MEMBER_STATS_FULL: u16 = 0x02F2;
    /// This character has been thrown out of the group. Empty body -- there
    /// is only one group to be thrown out of.
    pub const GROUP_UNINVITE: u16 = 0x0077;
    /// The group is gone. Empty body.
    pub const GROUP_DESTROYED: u16 = 0x007C;
    /// An invite this character sent was withdrawn or timed out.
    pub const GROUP_CANCEL: u16 = 0x0071;
    /// Somebody declined an invite: the name, NUL-terminated.
    pub const GROUP_DECLINE: u16 = 0x0074;
    /// Leadership has moved: the new leader's name.
    pub const GROUP_SET_LEADER: u16 = 0x0079;
}

/// Whether this opcode carries `{packed guid, MovementInfo}` from another
/// mover.
///
/// One list, consulted by the dispatcher that folds these and by the capture
/// analysis that hunts for ones being missed. Two copies would drift, and the
/// way they would drift is the analysis reporting an opcode as handled after
/// someone removed it from the fold -- a tool agreeing with itself, which is
/// the one shape of evidence this project does not accept.
pub fn is_relayed_movement(opcode: u16) -> bool {
    server::MOVE_RELAYED.contains(&opcode)
}

/// A human-readable name for an incoming opcode, for logs and dumps.
///
/// Unknown opcodes render as their number rather than being rejected, because
/// the server sends a good many this client has no interest in.
pub fn describe(opcode: u16) -> String {
    let name = match opcode {
        server::CHAR_ENUM => "SMSG_CHAR_ENUM",
        server::PONG => "SMSG_PONG",
        server::AUTH_CHALLENGE => "SMSG_AUTH_CHALLENGE",
        server::AUTH_RESPONSE => "SMSG_AUTH_RESPONSE",
        server::TUTORIAL_FLAGS => "SMSG_TUTORIAL_FLAGS",
        server::ADDON_INFO => "SMSG_ADDON_INFO",
        server::CLIENTCACHE_VERSION => "SMSG_CLIENTCACHE_VERSION",
        server::LOGIN_VERIFY_WORLD => "SMSG_LOGIN_VERIFY_WORLD",
        server::NEW_WORLD => "SMSG_NEW_WORLD",
        server::TRANSFER_PENDING => "SMSG_TRANSFER_PENDING",
        server::TRANSFER_ABORTED => "SMSG_TRANSFER_ABORTED",
        server::LOOT_RESPONSE => "SMSG_LOOT_RESPONSE",
        server::LOOT_RELEASE_RESPONSE => "SMSG_LOOT_RELEASE_RESPONSE",
        server::LOOT_REMOVED => "SMSG_LOOT_REMOVED",
        server::LOOT_CLEAR_MONEY => "SMSG_LOOT_CLEAR_MONEY",
        server::GOSSIP_MESSAGE => "SMSG_GOSSIP_MESSAGE",
        server::LIST_INVENTORY => "SMSG_LIST_INVENTORY",
        server::TRAINER_LIST => "SMSG_TRAINER_LIST",
        server::SHOW_TAXI_NODES => "SMSG_SHOWTAXINODES",
        server::ACTIVATE_TAXI_REPLY => "SMSG_ACTIVATETAXIREPLY",
        server::NEW_TAXI_PATH => "SMSG_NEW_TAXI_PATH",
        server::TRAINER_BUY_SUCCEEDED => "SMSG_TRAINER_BUY_SUCCEEDED",
        server::TRADE_STATUS => "SMSG_TRADE_STATUS",
        server::TRADE_STATUS_EXTENDED => "SMSG_TRADE_STATUS_EXTENDED",
        server::SEND_MAIL_RESULT => "SMSG_SEND_MAIL_RESULT",
        server::MAIL_LIST_RESULT => "SMSG_MAIL_LIST_RESULT",
        server::RECEIVED_MAIL => "SMSG_RECEIVED_MAIL",
        server::QUERY_NEXT_MAIL_TIME => "MSG_QUERY_NEXT_MAIL_TIME",
        server::SHOW_MAILBOX => "SMSG_SHOW_MAILBOX",
        server::AUCTION_HELLO => "MSG_AUCTION_HELLO",
        server::AUCTION_COMMAND_RESULT => "SMSG_AUCTION_COMMAND_RESULT",
        server::AUCTION_LIST_RESULT => "SMSG_AUCTION_LIST_RESULT",
        server::AUCTION_OWNER_LIST_RESULT => "SMSG_AUCTION_OWNER_LIST_RESULT",
        server::AUCTION_BIDDER_LIST_RESULT => "SMSG_AUCTION_BIDDER_LIST_RESULT",
        server::AUCTION_BIDDER_NOTIFICATION => "SMSG_AUCTION_BIDDER_NOTIFICATION",
        server::AUCTION_OWNER_NOTIFICATION => "SMSG_AUCTION_OWNER_NOTIFICATION",
        server::AUCTION_REMOVED_NOTIFICATION => "SMSG_AUCTION_REMOVED_NOTIFICATION",
        server::AUCTION_LIST_PENDING_SALES => "SMSG_AUCTION_LIST_PENDING_SALES",
        server::QUESTGIVER_STATUS => "SMSG_QUESTGIVER_STATUS",
        server::QUESTGIVER_STATUS_MULTIPLE => "SMSG_QUESTGIVER_STATUS_MULTIPLE",
        server::QUESTGIVER_QUEST_LIST => "SMSG_QUESTGIVER_QUEST_LIST",
        server::QUESTGIVER_QUEST_DETAILS => "SMSG_QUESTGIVER_QUEST_DETAILS",
        server::QUESTGIVER_REQUEST_ITEMS => "SMSG_QUESTGIVER_REQUEST_ITEMS",
        server::QUESTGIVER_OFFER_REWARD => "SMSG_QUESTGIVER_OFFER_REWARD",
        server::QUESTGIVER_QUEST_COMPLETE => "SMSG_QUESTGIVER_QUEST_COMPLETE",
        server::QUEST_QUERY_RESPONSE => "SMSG_QUEST_QUERY_RESPONSE",
        server::QUEST_POI_QUERY_RESPONSE => "SMSG_QUEST_POI_QUERY_RESPONSE",
        server::CHAR_CREATE => "SMSG_CHAR_CREATE",
        server::CHAR_DELETE => "SMSG_CHAR_DELETE",
        server::CHARACTER_LOGIN_FAILED => "SMSG_CHARACTER_LOGIN_FAILED",
        server::UPDATE_OBJECT => "SMSG_UPDATE_OBJECT",
        server::DESTROY_OBJECT => "SMSG_DESTROY_OBJECT",
        server::COMPRESSED_UPDATE_OBJECT => "SMSG_COMPRESSED_UPDATE_OBJECT",
        server::TIME_SYNC_REQ => "SMSG_TIME_SYNC_REQ",
        server::MOTD => "SMSG_MOTD",
        server::ACCOUNT_DATA_TIMES => "SMSG_ACCOUNT_DATA_TIMES",
        server::LOGIN_SETTIMESPEED => "SMSG_LOGIN_SETTIMESPEED",
        server::MONSTER_MOVE => "SMSG_MONSTER_MOVE",
        server::MOVE_START_FORWARD => "MSG_MOVE_START_FORWARD",
        server::MOVE_STOP => "MSG_MOVE_STOP",
        server::MOVE_HEARTBEAT => "MSG_MOVE_HEARTBEAT",
        server::NAME_QUERY_RESPONSE => "SMSG_NAME_QUERY_RESPONSE",
        server::CREATURE_QUERY_RESPONSE => "SMSG_CREATURE_QUERY_RESPONSE",
        server::GAMEOBJECT_QUERY_RESPONSE => "SMSG_GAMEOBJECT_QUERY_RESPONSE",
        server::MESSAGECHAT => "SMSG_MESSAGECHAT",
        server::INVENTORY_CHANGE_FAILURE => "SMSG_INVENTORY_CHANGE_FAILURE",
        server::INITIAL_SPELLS => "SMSG_INITIAL_SPELLS",
        server::CAST_FAILED => "SMSG_CAST_FAILED",
        server::SPELL_START => "SMSG_SPELL_START",
        server::SPELL_GO => "SMSG_SPELL_GO",
        server::SPELL_COOLDOWN => "SMSG_SPELL_COOLDOWN",
        server::ATTACK_START => "SMSG_ATTACKSTART",
        server::ATTACK_STOP => "SMSG_ATTACKSTOP",
        server::ATTACKER_STATE_UPDATE => "SMSG_ATTACKERSTATEUPDATE",
        server::ATTACK_SWING_REFUSED_A => "SMSG_ATTACKSWING_REFUSED(0x0145)",
        server::ATTACK_SWING_REFUSED_B => "SMSG_ATTACKSWING_REFUSED(0x0146)",
        server::POWER_UPDATE => "SMSG_POWER_UPDATE",
        server::SPELL_NON_MELEE_DAMAGE_LOG => "SMSG_SPELLNONMELEEDAMAGELOG",
        server::THREAT_UPDATE => "SMSG_THREAT_UPDATE",
        server::MOVE_TELEPORT_ACK => "MSG_MOVE_TELEPORT_ACK",
        server::CORPSE_QUERY => "MSG_CORPSE_QUERY",
        server::DEATH_RELEASE_LOC => "SMSG_DEATH_RELEASE_LOC",
        server::CORPSE_RECLAIM_DELAY => "SMSG_CORPSE_RECLAIM_DELAY",
        server::SPIRIT_HEALER_CONFIRM => "SMSG_SPIRIT_HEALER_CONFIRM",
        server::THREAT_REMOVE => "SMSG_THREAT_REMOVE",
        server::CANCEL_COMBAT => "SMSG_CANCEL_COMBAT",
        server::DURABILITY_DAMAGE_DEATH => "SMSG_DURABILITY_DAMAGE_DEATH",
        server::GM_MESSAGECHAT => "SMSG_GM_MESSAGECHAT",
        server::GROUP_INVITE => "SMSG_GROUP_INVITE",
        server::GROUP_LIST => "SMSG_GROUP_LIST",
        server::PARTY_COMMAND_RESULT => "SMSG_PARTY_COMMAND_RESULT",
        server::PARTY_MEMBER_STATS => "SMSG_PARTY_MEMBER_STATS",
        server::PARTY_MEMBER_STATS_FULL => "SMSG_PARTY_MEMBER_STATS_FULL",
        server::GROUP_UNINVITE => "SMSG_GROUP_UNINVITE",
        server::GROUP_DESTROYED => "SMSG_GROUP_DESTROYED",
        server::GROUP_CANCEL => "SMSG_GROUP_CANCEL",
        server::GROUP_DECLINE => "SMSG_GROUP_DECLINE",
        server::GROUP_SET_LEADER => "SMSG_GROUP_SET_LEADER",
        server::GUILD_QUERY_RESPONSE => "SMSG_GUILD_QUERY_RESPONSE",
        server::GUILD_INVITE => "SMSG_GUILD_INVITE",
        server::GUILD_INFO => "SMSG_GUILD_INFO",
        server::GUILD_ROSTER => "SMSG_GUILD_ROSTER",
        server::GUILD_EVENT => "SMSG_GUILD_EVENT",
        server::GUILD_COMMAND_RESULT => "SMSG_GUILD_COMMAND_RESULT",
        // Understood as movement without this client caring which movement it
        // is: every one of them is a position for a mover, and that is all it
        // does with them. The number stays visible so a log still says which.
        other if is_relayed_movement(other) => {
            return format!("MSG_MOVE_* relayed ({other:#06x})")
        }
        other => return format!("opcode {other:#06x}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client and server halves of a pair share a number in 3.3.5a --
    /// `CMSG_CHAR_ENUM` is 0x37 and `SMSG_CHAR_ENUM` is 0x3B, which are
    /// *different*, unlike the auth pair. Pin both so a transcription slip
    /// cannot quietly swap them.
    #[test]
    fn the_handshake_opcodes_are_the_documented_values() {
        assert_eq!(ClientOpcode::AuthSession as u32, 0x01ED);
        assert_eq!(server::AUTH_CHALLENGE, 0x01EC);
        assert_eq!(server::AUTH_RESPONSE, 0x01EE);
        assert_eq!(ClientOpcode::CharEnum as u32, 0x0037);
        assert_eq!(server::CHAR_ENUM, 0x003B);
    }

    #[test]
    fn unknown_opcodes_describe_as_numbers() {
        assert_eq!(describe(server::CHAR_ENUM), "SMSG_CHAR_ENUM");
        assert_eq!(describe(0x1234), "opcode 0x1234");
    }
}
