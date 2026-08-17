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
    pub const MESSAGECHAT: u16 = 0x0096;
    /// The spellbook, sent unprompted during the login burst. There is no
    /// query for it: miss the packet and the character appears to know nothing.
    /// The refusal to `ClientOpcode::SwapItemCandidate` and `AutoEquipItem`
    /// alike -- see [`ClientOpcode::SwapItemCandidate`] for how its 18-byte
    /// shape was confirmed against a live realm (`foss-wow#55`).
    pub const INVENTORY_CHANGE_FAILURE: u16 = 0x0112;
    pub const INITIAL_SPELLS: u16 = 0x012A;
    pub const CAST_FAILED: u16 = 0x0130;
    pub const SPELL_START: u16 = 0x0131;
    pub const SPELL_GO: u16 = 0x0132;
    pub const SPELL_COOLDOWN: u16 = 0x0134;

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
    /// A unit's threat list dropping someone: `{packed guid, packed guid}`,
    /// the list's owner then whoever left it. Arrives twice on death, once per
    /// creature that had us.
    pub const THREAT_REMOVE: u16 = 0x0484;
    /// The fight ending because one side stopped existing. Empty body.
    pub const CANCEL_COMBAT: u16 = 0x014E;
    /// Equipment losing durability from dying. Not parsed.
    pub const DURABILITY_DAMAGE_DEATH: u16 = 0x02BD;
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
        server::LOOT_RESPONSE => "SMSG_LOOT_RESPONSE",
        server::LOOT_RELEASE_RESPONSE => "SMSG_LOOT_RELEASE_RESPONSE",
        server::LOOT_REMOVED => "SMSG_LOOT_REMOVED",
        server::LOOT_CLEAR_MONEY => "SMSG_LOOT_CLEAR_MONEY",
        server::GOSSIP_MESSAGE => "SMSG_GOSSIP_MESSAGE",
        server::LIST_INVENTORY => "SMSG_LIST_INVENTORY",
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
        server::THREAT_REMOVE => "SMSG_THREAT_REMOVE",
        server::CANCEL_COMBAT => "SMSG_CANCEL_COMBAT",
        server::DURABILITY_DAMAGE_DEATH => "SMSG_DURABILITY_DAMAGE_DEATH",
        server::GM_MESSAGECHAT => "SMSG_GM_MESSAGECHAT",
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
