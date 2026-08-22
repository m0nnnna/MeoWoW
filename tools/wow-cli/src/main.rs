//! Inspection tool for a WoW 3.3.5a installation's data files.
//!
//! Points at a `Data` directory and reads through the patch chain exactly as
//! the client would, so what it prints is what the engine would see.

use std::path::PathBuf;

use anyhow::{Context, Result};

mod light;
use clap::{Parser, Subcommand};
use mpq::{Archive, Chain};

/// A guid typed as bare hex, e.g. `f11002771500069d` -- what the viewer's own
/// `tracing::info!` logging prints for a picked entity, so a guid copied
/// straight out of a log line parses without editing.
fn parse_hex_guid(s: &str) -> Result<u64, String> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

#[derive(Parser)]
#[command(name = "wow-cli", about = "Inspect WoW 3.3.5a client data")]
struct Cli {
    /// Path to the installation's `Data` directory.
    #[arg(long, short, global = true, env = "WOW_DATA")]
    data: Option<PathBuf>,

    /// Locale subdirectory holding the localized archives.
    #[arg(long, global = true, default_value = "enUS")]
    locale: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarize each archive in the load order.
    Info,
    /// List files, optionally filtered by a substring.
    Ls {
        /// Case-insensitive substring to match.
        filter: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Extract one file to disk.
    Extract {
        /// Archive path, e.g. `World\Maps\Azeroth\Azeroth.wdt`.
        name: String,
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Report which archive in the chain wins for a path.
    Which { name: String },
    /// Read every listed file and report failures. Slow but thorough.
    Verify {
        /// Stop after this many files.
        #[arg(long)]
        limit: Option<usize>,
        /// Only check files matching this substring.
        filter: Option<String>,
    },
    /// Inspect client database tables.
    #[command(subcommand)]
    Dbc(DbcCommand),
    /// Inspect and export textures.
    #[command(subcommand)]
    Blp(BlpCommand),
    /// Inspect models.
    #[command(subcommand)]
    M2(M2Command),
    /// Inspect items: what an equipped thing looks like and what it hangs off.
    #[command(subcommand)]
    Item(ItemCommand),
    /// Inspect sounds: what the client can play, and where its files are.
    #[command(subcommand)]
    Sound(SoundCommand),
    /// Inspect flight paths: the nodes, the routes, and where they actually go.
    #[command(subcommand)]
    Taxi(TaxiCommand),
    /// Inspect spell description templates.
    #[command(subcommand)]
    Spell(SpellCommand),
    /// Inspect world objects: buildings, dungeons, bridges.
    #[command(subcommand)]
    Wmo(WmoCommand),
    /// Inspect terrain.
    #[command(subcommand)]
    Adt(AdtCommand),
    /// Inspect world map pages and the world-to-page projection.
    #[command(subcommand)]
    Map(MapCommand),
    /// Inspect the minimap's hashed tile art.
    #[command(subcommand)]
    Minimap(MinimapCommand),
    /// Resolve the lighting that applies at a place and an hour.
    ///
    /// Prints every colour and scalar curve owned by the chosen light, sampled
    /// now and across the day, because which of the eighteen bands is which has
    /// not been confirmed against the data and is not guessed at here. What
    /// changes with the sun is a colour that follows the sun.
    Light {
        /// `Map.dbc` id: 0 is Azeroth.
        #[arg(long, default_value_t = 0)]
        map: u32,
        /// Clap reads a bare negative number as a flag, so pass `--x=-8950`.
        #[arg(long, allow_hyphen_values = true, default_value_t = -8950.0)]
        x: f32,
        #[arg(long, allow_hyphen_values = true, default_value_t = -132.5)]
        y: f32,
        /// Game hour, 0 to 24.
        #[arg(long, default_value_t = 12.0)]
        hour: f32,
        /// Instead of reporting one point, ask whether `Light.dbc`'s storm
        /// column really is the stormy one, across every light on every map.
        #[arg(long)]
        weather_check: bool,
        /// Instead of reporting one point, ask what *kind* of thing each of
        /// the eighteen colour bands is, across every `LightParams` row.
        #[arg(long)]
        band_survey: bool,
    },
    /// Log in to a realm's logon server and list its realms.
    ///
    /// Needs no game files, only an account on the server.
    Auth {
        /// Logon server hostname.
        host: String,
        #[arg(long, short)]
        user: String,
        /// Password. Prefer `WOW_PASSWORD` so it stays out of shell history.
        #[arg(long, short, env = "WOW_PASSWORD", hide_env_values = true)]
        password: String,
        #[arg(long, default_value_t = auth::client::DEFAULT_PORT)]
        port: u16,
        #[arg(long, default_value_t = 8)]
        timeout: u64,
    },
    /// Log in, connect to a realm's world server, and list its characters.
    ///
    /// Runs the whole chain: SRP6 against the logon server, then the world
    /// handshake and its RC4 header cipher. Needs no game files.
    World {
        /// Logon server hostname. The world server's address comes from the
        /// realm list, not from here.
        host: String,
        #[arg(long, short)]
        user: String,
        /// Password. Prefer `WOW_PASSWORD` so it stays out of shell history.
        #[arg(long, short, env = "WOW_PASSWORD", hide_env_values = true)]
        password: String,
        /// Realm to enter, by name. Defaults to the only realm offered.
        #[arg(long, short)]
        realm: Option<String>,
        /// Create a character with this name before listing.
        ///
        /// Exists to give the character-list parser real data to read: an
        /// empty list exercises none of its field offsets.
        #[arg(long)]
        create: Option<String>,
        /// Race and class for `--create`; defaults to a human warrior.
        #[arg(long, default_value_t = 1)]
        race: u8,
        #[arg(long, default_value_t = 1)]
        class: u8,
        /// Appearance for `--create`. Defaults to all zeros, which is what
        /// every character this tool has made so far looks like -- and that is
        /// precisely why these exist. `--appearance` searches the update
        /// fields for the packed appearance, and a character whose five
        /// numbers are all zero matches every zero field in the object,
        /// proving nothing. Give them distinct non-zero values and the search
        /// identifies both the field *and* the byte order.
        #[arg(long, default_value_t = 0)]
        skin: u8,
        /// Named `--char-face` because `--face` already means a heading to
        /// turn to.
        #[arg(long = "char-face", default_value_t = 0)]
        char_face: u8,
        #[arg(long, default_value_t = 0)]
        hair_style: u8,
        #[arg(long, default_value_t = 0)]
        hair_color: u8,
        #[arg(long, default_value_t = 0)]
        facial_hair: u8,
        /// Delete the named character before listing.
        #[arg(long)]
        delete: Option<String>,
        /// Enter the world as this character and report where it landed.
        #[arg(long)]
        enter: Option<String>,
        /// Write object-update payloads that failed to parse here, already
        /// decompressed, for offline analysis.
        #[arg(long)]
        dump_failed: Option<PathBuf>,
        /// After entering, walk this many units forward.
        ///
        /// Verify it by running the command again without `--enter`: the
        /// character list reports the position the *server* has stored.
        #[arg(long)]
        walk: Option<f32>,
        /// Heading to walk along, in degrees. Defaults to the way the
        /// character is already facing.
        #[arg(long)]
        heading: Option<f32>,
        /// Sidestep instead of walking forward: `left` or `right`.
        ///
        /// The character keeps facing its heading and travels at a right angle
        /// to it, which is the whole difference between strafing and walking --
        /// and the reason this is worth driving from here. The check is not
        /// that the packets were accepted (nothing acknowledges them) but that
        /// the position the *server* stores moved sideways: run again without
        /// `--enter`, or read `characters.position_x` out of the database.
        #[arg(long)]
        strafe: Option<Strafe>,
        /// After entering, jump on the spot.
        ///
        /// Exercises the one part of `MovementInfo` that only a jump populates
        /// -- the falling block, and `fall_time` with it.
        #[arg(long)]
        jump: bool,
        /// Turn on the spot to this heading, in degrees, without walking.
        #[arg(long)]
        face: Option<f32>,
        /// After entering, turn continuously on the spot for two seconds,
        /// the way holding `A` or `D` alone does.
        ///
        /// Confirms `MoveStartTurnLeft`/`Right`/`MoveStopTurn` -- three of
        /// the fifteen relayed movement opcodes `foss-wow#37` set out to
        /// drive, and the only ones this client had no way to send at all
        /// before this ticket.
        #[arg(long)]
        turn: Option<Turn>,
        /// After entering, toggle between running and walking.
        ///
        /// Confirms `MoveSetRunMode`/`MoveSetWalkMode`.
        #[arg(long)]
        run_mode: Option<RunMode>,
        /// After entering, hold the connection open this many seconds,
        /// answering keepalives. Proves the session survives rather than being
        /// dropped a minute in.
        #[arg(long, default_value_t = 0)]
        stay: u64,
        /// After entering, print what a unit frame would show for this
        /// character and the nearest N replicated units.
        ///
        /// The fields a unit frame reads -- health, power, level, and the
        /// race/class/gender/power-type byte -- are the ones where a wrong
        /// guess parses perfectly and returns nonsense, so they get a dump
        /// command like every other format here.
        #[arg(long)]
        units: Option<usize>,
        /// After entering, print every replicated game object with its
        /// position and every field it has set.
        ///
        /// Game objects -- doors, chests, mailboxes, signposts -- are created
        /// and then dropped by this client, because `Entity::display_id` reads
        /// only the *unit* display field. Which field carries a game object's
        /// is not guessed at here: the printout is the raw material for the
        /// same search that settled `PLAYER_BYTES`, and the answer is the field
        /// whose value resolves to a real `GameObjectDisplayInfo` row for every
        /// object rather than for some of them.
        #[arg(long)]
        objects: bool,
        /// After entering, print every replicated item, with its fields, beside
        /// the character's own inventory slot array.
        ///
        /// The two halves are printed together deliberately. A list of item
        /// objects alone says what is held but not *where*, and a slot array
        /// alone is a column of guids with nothing to resolve them against --
        /// each is half of the same question. Printed side by side, the slot
        /// array's identification predicts which guids appear and where, which
        /// is the check that separated it from a plausible-looking neighbour.
        ///
        /// This is also the instrument for the fields that are still unknown:
        /// add a stack of 3 and a stack of 5 (`--say ".additem 2589 3"`) and
        /// diff one run's item fields against another's. A field that tracks
        /// the count is the stack count; a field that reads 3 in both is not.
        #[arg(long)]
        items: bool,
        /// Wear the item in this inventory slot, and report where it went.
        ///
        /// **The instrument that confirms `CMSG_AUTOEQUIP_ITEM`.** Nothing
        /// acknowledges the send, so this prints the whole slot array before
        /// and after and names every guid that moved. A wrong opcode moves
        /// nothing, which is a different printout rather than a similar one.
        ///
        /// The server chooses the destination, and its choice is the useful
        /// half: it names which equipment index an item of that kind belongs
        /// in, which is how the slot vocabulary is filled in without guessing.
        #[arg(long)]
        equip: Vec<u16>,
        /// Swap two of the player's own slots and report what moved --
        /// `<from>:<to>`, e.g. `--swap 25:26`.
        ///
        /// Sends `ClientOpcode::SwapItemCandidate` (`CMSG_SWAP_ITEM`,
        /// `0x010C`), confirmed live against this realm -- see the opcode's
        /// own doc comment for `foss-wow#55`'s finding. Diffs the slot array
        /// the same way `--equip` does, and prints any
        /// `SMSG_INVENTORY_CHANGE_FAILURE` in full if the server declines.
        #[arg(long)]
        swap: Option<String>,
        /// Open the loot on the nearest dead unit and print every byte that
        /// comes back.
        ///
        /// **A survey, not a feature.** Nothing loot-related is parsed by this
        /// client, and the point of this flag is to stop that being true:
        /// pair it with `--select --target <name> --say ".die"` to make a
        /// corpse, and it reports every opcode that arrives with its body in
        /// full. Bodies rather than lengths, because a packet that is seen and
        /// dropped is the one packet that could have answered the question.
        #[arg(long)]
        loot: bool,
        /// Greet the nearest NPC that carries any `UNIT_NPC_FLAGS` bit, and
        /// print every byte that comes back.
        ///
        /// **A survey, and the first send of the NPC-interaction work.**
        /// Nothing gossip-related is parsed by this client; this exists to
        /// produce the packet that makes parsing it possible. Pair it with
        /// `--say ".npc add 295"` to put an Innkeeper Farley within reach --
        /// he carries gossip, quest, vendor and innkeeper bits at once, so a
        /// single greeting exercises more of the reply than a plain
        /// questgiver does.
        ///
        /// Refuses to send at range rather than trying anyway, because a
        /// silence is the one answer that means nothing: a wrong opcode, an
        /// NPC with no gossip, and an NPC across the field are indistinguishable
        /// from each other. That distinction cost three runs on `--loot`.
        ///
        /// Takes an optional **creature entry** to prefer -- `--gossip 197` for
        /// Marshal McBride -- because the interesting comparison is between
        /// *different* NPCs and `.npc add` puts every spawn at the caller's
        /// feet, where "the nearest one" picks arbitrarily between them. Entry
        /// rather than name so it needs no `--names` round trip, and it is what
        /// `creature_template` is keyed by anyway.
        #[arg(long, num_args = 0..=1, default_missing_value = "0")]
        gossip: Option<u32>,
        /// After greeting, choose this menu option and report what comes back.
        ///
        /// **The number is the server's own option id**, exactly as `--gossip`
        /// prints it, and *not* a row position: a filtered menu leaves holes in
        /// the numbering, so counting from the top asks for the wrong line. The
        /// menu id is taken from the reply rather than from anything typed
        /// here, so the request cannot name a menu the greeting did not
        /// produce.
        ///
        /// `--gossip 295 --gossip-select 3` chooses an innkeeper's
        /// `I want to browse your goods.`, which is the cheapest route to a
        /// vendor list: the answer is a different opcode carrying stock, so
        /// nothing but a correctly understood selection could have caused it.
        #[arg(long)]
        gossip_select: Option<u32>,
        /// After a stock list arrives, buy this **vendor slot** and report the
        /// effect.
        ///
        /// Confirmed by consequence, since nothing acknowledges a purchase:
        /// the item appears in the slot array and `PLAYER_FIELD_COINAGE`
        /// drops. The money that leaves is compared against the *quoted*
        /// price, which makes this a check on `vendor::VendorItem::price`
        /// being the discounted figure rather than only on the buy working.
        ///
        /// The number is the server's own vendor slot, as `--gossip-select`
        /// prints it, and the item entry is looked up from the list rather
        /// than typed -- the server checks that the pair agree.
        #[arg(long)]
        buy: Option<u32>,
        /// Sell whatever `--buy` just bought straight back to the same vendor.
        ///
        /// What makes the probe repeatable -- one that slowly fills the bags
        /// with vendor water is one nobody runs twice -- and it exercises the
        /// sell path against a guid this run just learned, which is how a real
        /// shop window works. Expect a net loss: a vendor buys back below its
        /// sale price.
        #[arg(long)]
        sell_back: bool,
        /// Walk to the nearest trainer, ask what it teaches, and report the
        /// whole reply -- including the stride measurement that says the
        /// record layout is right.
        ///
        /// **The reply is what makes this the cheap one.** `CMSG_TRAINER_LIST`
        /// is answered, so a body coming back at all confirms the opcode and
        /// the request body together, with none of the three-way ambiguity a
        /// silent send leaves. Everything else in the trainer/mail/auction
        /// block is then bounded by this one, the same way
        /// `CMSG_LIST_INVENTORY` bounded `CMSG_BUY_ITEM`.
        ///
        /// Takes an optional **creature entry** to prefer, like `--gossip`.
        /// `--trainer 911` is Llane Beshere, the Northshire warrior trainer:
        /// six spells at required levels 1, 4, 4, 6, 6, 6, which is a
        /// population that can actually separate the availability states
        /// instead of showing one value six times. `.npc add 911` puts him at
        /// the caller's feet.
        #[arg(long, num_args = 0..=1, default_missing_value = "0")]
        trainer: Option<u32>,
        /// After a trainer list arrives, learn this **spell id** and report
        /// the effect.
        ///
        /// The number is a spell id from the list and never a row position --
        /// the server filters the list per character, so a position means
        /// different things to two readers standing at the same NPC while an
        /// id does not.
        ///
        /// Confirmed two ways at once, which is worth having because they fail
        /// differently: `SMSG_TRAINER_BUY_SUCCEEDED` echoes the spell id back,
        /// and `PLAYER_FIELD_COINAGE` drops by the **quoted** price -- so the
        /// money check tests `trainer::TrainerSpell::cost` being the
        /// discounted figure rather than only that the purchase worked.
        #[arg(long)]
        learn: Option<u32>,
        /// Walk to the nearest flight master, ask where it can send you, and
        /// report the whole reply.
        ///
        /// **The fixed 72-byte body is the confirmation.** There is no
        /// variable-length block in `SMSG_SHOWTAXINODES`, so a wrong mask
        /// width shows up as leftover bytes immediately rather than as a
        /// plausible wrong answer.
        ///
        /// Takes an optional creature entry to prefer, like `--gossip`.
        /// A level-one character knows one or two nodes; `.cheat taxi on`
        /// makes the server send the whole set, which is what separates
        /// "the mask parsed" from "the mask is in the right place".
        #[arg(long, num_args = 0..=1, default_missing_value = "0")]
        taxi: Option<u32>,
        /// After a taxi menu arrives, buy a flight to this **node id** and
        /// report what happens.
        ///
        /// The departure node is the one the *server* named, never one this
        /// client worked out from the player's position -- a flight master
        /// stands near its pad rather than on it, and two factions can share
        /// a town.
        ///
        /// Unlike almost everything else in this tool, the send is
        /// **answered** whether it works or not, so this can tell a refusal
        /// from a misunderstanding. What it then watches for is the thing
        /// this milestone exists to establish: whether the ride arrives as a
        /// `SMSG_MONSTER_MOVE` naming *the player's own guid*, which would be
        /// the first time the server has ever moved this character.
        #[arg(long)]
        fly_to: Option<u32>,
        /// Ask the nearest reachable mailbox what is in the inbox, and print
        /// every record with **its announced length beside its real one**.
        ///
        /// The mailbox is found by asking what each replicated game object in
        /// range *is* -- `CMSG_GAMEOBJECT_QUERY` -- because a display id says
        /// how to draw a thing and nothing about what it does. Spawn one with
        /// `.gobject add 142075` if there is none nearby; Northshire has no
        /// mailbox and the closest is 537 units away in Goldshire.
        #[arg(long)]
        mail_list: bool,
        /// Post a letter to this character by name.
        ///
        /// **The bounding send for the whole block**: it is answered either
        /// way by `SMSG_SEND_MAIL_RESULT`, whose action field echoes the
        /// request, so one send confirms the opcode, the body and the reply
        /// layout together. Trade had no such neighbour and had to be bounded
        /// by its own refusal; mail does, and this is it.
        #[arg(long)]
        mail_to: Option<String>,
        /// The subject line. Also the thing that makes a letter findable in
        /// the realm's own database afterwards.
        #[arg(long, default_value = "probe")]
        mail_subject: String,
        /// The body text. An empty body is a *different* letter as far as the
        /// check mask is concerned -- the server sets `HAS_BODY` only when
        /// there is one -- which is worth varying deliberately.
        #[arg(long, default_value = "")]
        mail_text: String,
        /// Enclose this many copper. The server charges thirty on top.
        #[arg(long, default_value_t = 0)]
        mail_money: u32,
        /// Attach the carried item with this **entry**.
        ///
        /// The delivery delay is the thing this varies: the server applies an
        /// hour's wait only to mail that **carries items** and only when the
        /// recipient is on another account, so item mail to a character on the
        /// same account should arrive at once. That claim comes from the
        /// server's source and is unconfirmed until this has been run both
        /// ways.
        #[arg(long)]
        mail_item: Option<u32>,
        /// Take everything out of the first letter that has anything in it,
        /// then re-ask the inbox.
        ///
        /// Re-asking rather than editing the local copy, for the reason the
        /// trainer list is re-asked after a purchase: a request the server may
        /// have declined must not be drawn as though it went through.
        #[arg(long)]
        mail_take: bool,
        /// Sit still for this many seconds and report every `SMSG_RECEIVED_MAIL`
        /// that lands.
        ///
        /// **The measurement this milestone is really about.** Nothing is sent
        /// during the wait, so anything that arrives arrived because somebody
        /// else acted -- which no other packet this client reads has ever done.
        /// Drive it from a second session or from `.send mail` at the console.
        #[arg(long, default_value_t = 0)]
        mail_wait: u64,
        /// Throw away every letter that has nothing left in it, then re-ask.
        ///
        /// Exercises `CMSG_MAIL_DELETE`, which is answered like the rest of
        /// the block -- and it is worth sending once deliberately rather than
        /// leaving implemented and never used, which is how a request nobody
        /// has confirmed ends up documented as though it worked.
        #[arg(long)]
        mail_clear: bool,
        /// Ask `CMSG_GET_MAIL_LIST` with the **reader's own guid** as the
        /// mailbox.
        ///
        /// The server accepts that from a game master and from nobody else,
        /// and every fixture account on this realm is one -- so this is
        /// measured deliberately in order to be ruled out. A client built on
        /// it would work for the person who wrote it and for no player.
        #[arg(long)]
        mail_own_guid: bool,
        /// Ask for the guild roster, and **score the record layout** rather
        /// than parse it and believe the answer.
        ///
        /// The roster's member record carries four bytes that exist only for
        /// members who are *offline*, which is a conditional layout in the
        /// middle of a variable-length record inside a list -- so a wrong
        /// reading desynchronises everything after it rather than leaving
        /// bytes at the end. Two other readings also draw a picture, and this
        /// reports which of the three the body is consistent with **and
        /// whether the body is capable of separating them at all**: a roster
        /// where everybody is offline cannot, and neither can one where
        /// everybody is online.
        ///
        /// It is also the cheapest bound in the whole city-services block. A
        /// character in no guild is answered by `SMSG_GUILD_COMMAND_RESULT`
        /// rather than by silence, so this confirms the opcode and the result
        /// layout with no fixture whatsoever.
        #[arg(long)]
        guild: bool,
        /// Probe the auction house, and **measure the record stride** rather
        /// than parse it and believe the answer.
        ///
        /// Takes the auctioneer's `creature_template` entry to walk to, or
        /// nothing to use the nearest NPC that will talk.
        ///
        /// The run is deliberately in an order that keeps the silences apart.
        /// It sends `CMSG_AUCTION_LIST_PENDING_SALES` **first, before finding
        /// anybody**, because that handler checks nothing and always answers:
        /// its reply confirms the socket and the opcode block with no fixture
        /// at all, and every send after it can then be believed or disbelieved
        /// on its own. Nine of this block's ten requests are silent when the
        /// auctioneer does not resolve, which is indistinguishable from a
        /// wrong opcode without that first bound.
        #[arg(long)]
        auction: bool,
        /// Which auctioneer to walk to, by `creature_template` entry.
        #[arg(long)]
        auctioneer: Option<u32>,
        /// Search for this substring instead of listing everything.
        #[arg(long)]
        auction_search: Option<String>,
        /// Start the search at this **row**, not this page.
        ///
        /// The reply says how many rows it holds and how many matched, and
        /// nothing about where in the match they sit -- so this number is also
        /// handed to `WorldState::expect_auction_page`, and the probe prints
        /// both so a disagreement is visible.
        #[arg(long, default_value_t = 0)]
        auction_offset: u32,
        /// Post one auction per matching stack in the bags, by item entry.
        ///
        /// Repeatable. Each stack becomes its own auction, which is how a
        /// fixture large enough to page is built -- a page is fifty rows and
        /// nothing smaller can show a total that exceeds a count.
        #[arg(long)]
        auction_sell: Option<u32>,
        /// Opening bid, in copper, for `--auction-sell`.
        #[arg(long, default_value_t = 100)]
        auction_bid: u32,
        /// Buyout, in copper, for `--auction-sell`. Zero offers none.
        #[arg(long, default_value_t = 0)]
        auction_buyout: u32,
        /// Bid this many copper on the auction named by `--auction-id`.
        ///
        /// A price equal to the buyout **is** the buyout; there is no second
        /// opcode, which is worth seeing rather than reading.
        #[arg(long)]
        auction_place: Option<u32>,
        /// Which auction `--auction-place` or `--auction-cancel` acts on.
        #[arg(long)]
        auction_id: Option<u32>,
        /// Cancel the auction named by `--auction-id`. The goods come back as
        /// **mail**, so `--mail-list` is what confirms it.
        #[arg(long)]
        auction_cancel: bool,
        /// Ask this player, by name, to join the guild.
        ///
        /// Half of a two-client rig: the invitation arrives at *their*
        /// session and nothing comes back here but a command result.
        #[arg(long)]
        guild_invite: Option<String>,
        /// Wait for a guild invitation and accept it with an **empty body**.
        ///
        /// The other half. Worth running because the accept identifies
        /// nothing at all -- the server resolves which guild from the
        /// invitation it recorded when the invite went out.
        #[arg(long)]
        guild_accept: bool,
        /// Set a member's public note as `Name=text`, then **re-ask the
        /// roster** and report whether it took.
        ///
        /// The request is silent on success, so this is confirmed by effect
        /// and never by drawing the intention.
        #[arg(long)]
        guild_note: Option<String>,
        /// Set the message of the day, and report the event the change is
        /// pushed as.
        #[arg(long)]
        guild_motd: Option<String>,
        /// Sit still for this many seconds and report every
        /// `SMSG_GUILD_EVENT`.
        ///
        /// The only push in the block, and the reason a roster is not polled.
        /// Drive it from a second session by logging a guild member in or out.
        #[arg(long, default_value_t = 0)]
        guild_wait: u64,
        /// Say this on guild chat.
        ///
        /// Half of a two-client rig: guild chat reaches every member who is
        /// online and nobody else, so the only way to know it went out is for
        /// a *second* session to print it. A guildless character's guild line
        /// is dropped by the server in silence, which is exactly the failure
        /// this pairs with `--guild-wait` to rule out.
        #[arg(long)]
        guild_say: Option<String>,
        /// Ask this **player**, by name, to trade -- then drive the whole
        /// exchange and report every packet.
        ///
        /// **Half of a two-client rig and useless alone.** Trade is the first
        /// thing in this block where the far end is another person's client
        /// rather than the server, so a successful request is answered by a
        /// packet at *somebody else's* session and by nothing at all here. The
        /// other client runs `--trade-wait`.
        ///
        /// The name is matched against replicated players, so the partner has
        /// to be in visibility range -- which they must be anyway, since the
        /// server refuses a trade past ten units.
        #[arg(long)]
        trade: Option<String>,
        /// Wait for somebody to offer a trade, answer it, and drive the rest.
        ///
        /// The other half of `--trade`. Answers with `CMSG_BEGIN_TRADE` unless
        /// `--trade-decline` is given.
        #[arg(long)]
        trade_wait: bool,
        /// Answer an offered trade with "busy" instead of opening the window.
        ///
        /// Worth its own run: a decline is one of three mutually exclusive
        /// answers and produces a *different* status at the initiator, which
        /// is the only way to tell a refusal from a partner who never replied.
        #[arg(long)]
        trade_decline: bool,
        /// The bounding send, and the one part of this milestone that needs
        /// only one client.
        ///
        /// Aims `CMSG_INITIATE_TRADE` at a guid that is **not a player**. The
        /// server answers immediately with a status naming the reason, so a
        /// single send confirms the opcode number, the eight-byte body and the
        /// reply layout together -- with nobody else logged in. Every other
        /// request in this block is silent on success, so without this there
        /// would be nothing to bound them against.
        #[arg(long)]
        trade_nobody: bool,
        /// Put the carried item with this **entry** on the table.
        ///
        /// Confirmed by consequence and by nothing else: the request is
        /// silent, and the server answers it by restating the whole offer to
        /// both clients. The item appearing in the reflected offer is the
        /// proof -- and it is the reflection that is read, never this
        /// client's own intention.
        #[arg(long = "trade-item")]
        trade_item: Option<u32>,
        /// Put this many copper on the table.
        ///
        /// Pick a number the character actually has: the server answers an
        /// amount it cannot cover with a status that means "busy" everywhere
        /// else, which is a good thing to have seen once deliberately.
        #[arg(long = "trade-gold")]
        trade_gold: Option<u32>,
        /// Press accept once the window is open and the offer has been staged.
        ///
        /// Re-sent if the server withdraws it -- changing an offer resets both
        /// accepts, and a scripted run where the two clients stage at slightly
        /// different times hits that every time.
        #[arg(long)]
        trade_accept: bool,
        /// How long to drive the trade for, in seconds.
        ///
        /// Long enough that the two clients need not be started in lockstep:
        /// the initiator waits for the partner's client to answer, and a login
        /// takes about ninety seconds.
        #[arg(long, default_value_t = 60)]
        trade_seconds: u64,
        /// Ask the server what these item entries are, and check the answers
        /// against `Item.dbc`.
        ///
        /// **The cross-check is the point, not the names.** A ninety-field
        /// packet with one variable-length block parses perfectly when it is
        /// wrong; what makes an answer trustworthy is that its display id
        /// matches the one `Item.dbc` gives that entry, and the server never
        /// sends that table. Same evidence as the entry-to-display-id pairing
        /// that confirmed `SMSG_LOOT_RESPONSE`. Needs `--data` for the check;
        /// without it the names still print and the check says so.
        ///
        /// With no entries given, asks about everything the character is
        /// actually carrying -- a spread of real types beats a hand-picked
        /// one, and `Huntertest` on `OWC34` carries a gun, a quiver and a
        /// bag with contents.
        #[arg(long = "item-query", num_args = 0.., value_delimiter = ',')]
        item_query: Option<Vec<u32>>,
        /// Use the carried item with this entry, and report what changed.
        ///
        /// **A write nothing acknowledges, confirmed by consequence.** The
        /// item's stack count is read before and after: a consumable that
        /// drops by one was used, and nothing else in a quiet session moves
        /// that field. Pick something stackable and harmless -- entry 159,
        /// `Refreshing Spring Water`, is what this was confirmed against --
        /// rather than a hearthstone, which works but moves the character.
        ///
        /// The on-use spell is looked up with `CMSG_ITEM_QUERY_SINGLE`
        /// first, which also bounds the silence: that request *is* answered,
        /// so a run that gets a name and then no effect has a use-item
        /// problem rather than an addressing one.
        #[arg(long = "use-item")]
        use_item: Option<u32>,
        /// Greet a questgiver by creature entry and dump everything it says.
        ///
        /// Separate from `--gossip` because the two greetings are different
        /// requests: a questgiver with no gossip menu answers this and not
        /// that. Pair with `--quest-accept <id>` to drive the whole flow --
        /// ask for the scroll, then take the quest.
        #[arg(long, num_args = 0..=1, default_missing_value = "0")]
        quest: Option<u32>,
        /// Accept this quest id from the NPC `--quest` greeted, and report
        /// which of our own update fields changed.
        ///
        /// **This is a measurement, not just an action.** Where the quest log
        /// lives in the update fields is not known, and transcribing an index
        /// from memory is what this project keeps paying for. The quest id is
        /// an answer we already have, the server is about to store it, and no
        /// other field has a reason to hold that exact number -- so whichever
        /// index changes to it is the log. Same technique that found
        /// `PLAYER_BYTES` and the visible-item block.
        #[arg(long)]
        quest_accept: Option<u32>,
        /// Hand this quest in to the NPC `--quest` greeted.
        ///
        /// The other end of `--quest-accept`, and it goes to a **different
        /// creature** for most quests -- the ender rather than the starter.
        /// Offers the quest, reads the reward screen, takes reward index 0.
        ///
        /// Judged by the quest log emptying, never by a packet: the first of
        /// the two sends is answered and the second is not, and the reply that
        /// looks like a refusal on this flow is not one (see
        /// `--quest-accept`'s note on `0x018F`).
        #[arg(long)]
        quest_turnin: Option<u32>,
        /// Ask this player, by name, to join a group.
        ///
        /// **The one group request that is answered**, which is why it is the
        /// first one to send. `SMSG_PARTY_COMMAND_RESULT` comes back whether
        /// it worked or not, so a deliberately misspelt name is a complete
        /// test of the opcode and the body on its own: the reply echoes the
        /// name back, which proves the server read the string out of the
        /// offset this client wrote it to. Everything else in this block is
        /// silent, and a silent write fails identically whether the number,
        /// the body or the permission was wrong.
        #[arg(long)]
        party_invite: Option<String>,
        /// Answer an invite that arrives while holding the connection:
        /// `accept` or `decline`.
        ///
        /// Needs `--stay`, and needs the *other* client to be the one running
        /// `--party-invite`. A party cannot be made by one session, so this is
        /// half of a two-client rig and useless alone.
        #[arg(long)]
        party_answer: Option<PartyAnswer>,
        /// Leave the group -- or break it up, if this character leads it --
        /// after everything else has run.
        #[arg(long)]
        party_leave: bool,
        /// Throw this member out, by name. Resolved to a guid against the
        /// party list, because the kick request names a guid.
        #[arg(long)]
        party_kick: Option<String>,
        /// Dump every field of every occupied quest-log slot.
        ///
        /// **A measurement, not a display.** Only the first field of a slot
        /// has been identified; run this against a character whose quests are
        /// in a state you chose -- one finished, one not -- and whichever
        /// field differs the way completion differs is the completion field.
        #[arg(long)]
        quest_log: bool,
        /// Ask what *every* quest id up to this one is, and report how many
        /// answers parse whole.
        ///
        /// **The regression net for this packet.** A variable-length body with
        /// two array blocks after five strings is the class where a wrong
        /// reading parses one quest perfectly and desynchronises on the next,
        /// and a handful of hand-picked samples cannot show that. A systematic
        /// error shows up here as one large bucket rather than as scattered
        /// noise -- the same reason `dbc check` and `m2 survey` exist.
        #[arg(long)]
        quest_sweep: Option<u32>,
        /// Ask where the objectives of every quest in the log are.
        ///
        /// The check the whole native-tracker plan rests on: WotLK shipped its
        /// own quest tracker, so the server already holds the map markers.
        /// Answers **only for quests in the player's own log**, so an empty
        /// log makes this vacuous rather than negative -- which is why the log
        /// is printed alongside.
        #[arg(long)]
        quest_poi: bool,
        /// Ask what mark belongs over every nearby NPC's head.
        ///
        /// **A probe for two opcodes at once, and the pair is the point.**
        /// `CMSG_QUESTGIVER_STATUS_QUERY` asks about one guid and
        /// `CMSG_QUESTGIVER_STATUS_MULTIPLE_QUERY` asks about everything in
        /// range with an empty body; sending both in one run means a silence
        /// from either is bounded by the other's answer rather than being one
        /// more thing that might be a wrong opcode.
        ///
        /// The status numbers are printed raw and **not named**, because what
        /// each value means is exactly what this exists to find out: run it
        /// against NPCs whose state you already know -- one offering a quest,
        /// one whose quest is in your log unfinished, one with nothing to say
        /// -- and the numbers name themselves.
        #[arg(long)]
        questgiver_status: bool,
        /// Record every talkable NPC in range into a questgiver cache, ask
        /// what mark each one wears, then save and reload the cache.
        ///
        /// **The bounding instrument for the one thing 4.31 draws that is not
        /// a server answer.** Everything else the tracker and the map show
        /// came off the wire and can be checked against the realm's own
        /// database; a remembered questgiver is this client's memory of what
        /// it was streamed, and the only question that matters about it is
        /// whether the memory survives being written down. So the round trip
        /// is part of the probe rather than a unit test's business: a
        /// disagreement here is a real cache the client would have lost.
        #[arg(long)]
        questgivers: bool,
        /// Ask what a quest actually is: title, objectives, rewards.
        ///
        /// **The backbone of the whole quest feature.** Unlike the details a
        /// questgiver shows, this works for *any* quest id and needs no NPC --
        /// which is what a tracker and a map need, since they have to describe
        /// quests the player is not standing in front of. Repeatable.
        #[arg(long)]
        quest_info: Vec<u32>,
        /// After entering, find which update field carries the character's
        /// appearance by searching for the answer the character list gives.
        ///
        /// Dressing *other* players needs those five numbers off the wire, and
        /// the indices that hold them have never been checked against this
        /// build. A wrong one parses perfectly and gives every stranger the
        /// wrong face.
        #[arg(long)]
        appearance: bool,
        /// After entering, find which update fields carry another player's
        /// worn items, by the same measure-not-transcribe technique as
        /// `--appearance`: our own character's equipment arrives twice, once
        /// as display ids in the character list and once as item entries in
        /// our own update fields, and `Item.dbc` bridges the two.
        ///
        /// `foss-wow#23`.
        #[arg(long)]
        visible_items: bool,
        /// After entering, print every replicated *unit* with its entry, its
        /// name and every field it has set.
        ///
        /// The `--own-fields` of other people's units, and the instrument the
        /// NPC-interaction work starts from: which field marks a creature as a
        /// questgiver or a vendor is a number nobody should write down from
        /// memory, and the server's own `creature_template.npcflag` is an
        /// independent answer to check a candidate against.
        #[arg(long)]
        unit_fields: bool,
        /// Print every field set on this character's own object, for diffing
        /// one state against another -- alive against dead, say.
        #[arg(long)]
        own_fields: bool,
        /// After entering, name what state every replicated unit is in --
        /// stealthed, transformed, which shapeshift form -- rather than
        /// printing the fields those readings come from.
        ///
        /// **The half `--own-fields` cannot do.** A before/after diff of raw
        /// fields is how these were found, and it is the wrong instrument for
        /// checking they are still read correctly: it says `0x4a` moved to
        /// `0x00020000` where the question is whether anything concluded
        /// "stealthed" from it. Ordinary parse/report separation, applied to
        /// state that lives in a field rather than in a packet.
        ///
        /// **Prints every unit, not only the interesting ones**, with the
        /// counts stated. "3 of 96 units are in some state" and "0 of 96" are
        /// both measurements; a list that only ever shows what it found cannot
        /// tell a quiet zone from a reader that concludes nothing.
        ///
        /// Pair it with a second client to check the half that matters: run
        /// this as the observer while the actor casts Stealth or a shapeshift,
        /// and it says whether the flags crossed the wire. Our own character
        /// is printed first and separately, because the two are answered by
        /// different routes and only the first is proof about *replication*.
        #[arg(long)]
        states: bool,
        /// Keep fighting until this character dies, for capturing the death
        /// and corpse flow.
        ///
        /// **The only way to see those packets is to be killed by something**,
        /// so this picks a fight, holds it, and picks another when the target
        /// dies -- health does not recover between rounds, so a level-one
        /// character loses eventually. Bounded by a wall clock, because "fight
        /// until dead" against something too weak to win is otherwise a loop
        /// with no exit.
        ///
        /// Pair with `--target Wolf`: the closest thing to a starting character
        /// is usually a friendly guard, and swinging at one is refused.
        #[arg(long)]
        until_death: bool,
        /// Vary one condition of a swing at a time, to tell apart the two
        /// empty-bodied refusals seen when both range and facing were wrong
        /// at once -- see `foss-wow#32`.
        ///
        /// `a` closes to melee range and deliberately faces away; `b` stays
        /// 8-10 units out and faces correctly; `c` is the control (in range
        /// and facing) and must produce `SMSG_ATTACKSTART` and real swings,
        /// or the run proves nothing about `a` and `b` either. Repeated
        /// three times each, since a creature wandering mid-approach can
        /// turn a single refusal into a false reading of either condition.
        #[arg(long)]
        swing_probe: Option<SwingProbe>,
        /// Select the nearest unit whose name contains this, rather than the
        /// nearest unit of any kind.
        ///
        /// The nearest thing to a character is very often a friendly quest
        /// giver, and a damage spell aimed at one is refused with "cannot be
        /// cast on that target" -- which looks exactly like a malformed cast.
        /// Naming the target makes a capture rig reproducible. Implies
        /// `--names`, since a name has to arrive before it can be matched.
        #[arg(long)]
        target: Option<String>,
        /// After entering, select the nearest unit and tell the server.
        ///
        /// Nothing is sent back, so this proves only that the packet is not
        /// rejected -- which is worth proving on its own: a malformed one
        /// drops the session.
        #[arg(long)]
        select: bool,
        /// Target your own character, the way clicking your own portrait
        /// does. Several GM commands act on the caller only once
        /// `UNIT_FIELD_TARGET` is set.
        #[arg(long)]
        select_self: bool,
        /// Release the spirit: give up the body and become a ghost at the
        /// nearest graveyard. Only does anything to a character who is
        /// dead and has not already released.
        #[arg(long)]
        release: bool,
        /// Take the body back. Needs the ghost to be standing at its own
        /// corpse and the reclaim delay to have run out, so it is the end
        /// of a corpse run rather than a shortcut past one.
        #[arg(long)]
        reclaim: bool,
        /// Draw or stow the weapon, then report the sheath state the server
        /// echoes back.
        ///
        /// The whole confirmation for `CMSG_SET_SHEATHED` in one flag: nothing
        /// acknowledges the send, but the server writes the value into byte 0
        /// of the sender's `UNIT_FIELD_BYTES_2`, so passing each state in turn
        /// and watching the byte follow proves the opcode and the field at
        /// once.
        #[arg(long)]
        sheath: Option<u32>,
        /// Select the nearest unit and swing at it, then hold the line.
        ///
        /// Auto-attack is a state rather than an action: one swing request
        /// starts it, and the server drives the exchange from there. Pair with
        /// `--stay` to see the exchange, and `--capture` to keep the bytes.
        #[arg(long)]
        attack: bool,
        /// Write every packet received to this file as `opcode length hex`.
        ///
        /// The login burst and everything `--stay` collects. Exists because a
        /// count of an opcode says only that a shape arrived, and writing a
        /// parser needs the shape itself -- several parsers in `world::spell`
        /// deliberately refuse layouts nobody has captured, and this is how
        /// one stops being uncaptured.
        #[arg(long)]
        capture: Option<PathBuf>,
        /// Ask the server what everything replicated is called, and wait for
        /// the answers.
        #[arg(long)]
        names: bool,
        /// After entering, say this out loud. Received chat is printed by
        /// `--stay`, so two clients prove the round trip.
        ///
        /// **Repeatable, and it has to be.** Chat is also how this rig issues
        /// GM commands, and the useful ones come in pairs: `.additem` followed
        /// by `.save` is one exchange, not two runs, because an item added in
        /// a session that ends by closing the socket is never written to the
        /// database. Each line is sent in the order given.
        #[arg(long)]
        say: Vec<String>,
        /// Yell rather than say. `/say` carries about 25 yards, which two
        /// characters in the same starting zone can easily exceed; a yell
        /// crosses the zone.
        #[arg(long)]
        yell: bool,
        /// Print the character's spellbook, as `SMSG_INITIAL_SPELLS` sent it.
        #[arg(long)]
        spells: bool,
        /// Cast this spell id at the nearest unit, or at yourself with
        /// `--cast-self`.
        #[arg(long)]
        cast: Option<u32>,
        #[arg(long)]
        cast_self: bool,
        /// Cast `--cast`'s spell at this game object guid (hex, e.g.
        /// `f11002771500069d`) instead of a unit -- a lock-opening probe for
        /// `foss-wow#141`, which candidate "Opening" spell (of several in
        /// `Spell.dbc` with the same name) satisfies a given lock's
        /// `EffectMiscValue` is not derivable from this project's own
        /// transcribed columns and has to be found by trying one live.
        #[arg(long, value_parser = parse_hex_guid)]
        cast_at_object: Option<u64>,
        /// Whisper `--say` to this character instead of speaking aloud.
        ///
        /// The only chat with no range at all, which makes it the one that
        /// tests delivery between two clients without their positions being
        /// part of the experiment.
        #[arg(long)]
        whisper: Option<String>,
        #[arg(long, default_value_t = auth::client::DEFAULT_PORT)]
        port: u16,
        #[arg(long, default_value_t = 8)]
        timeout: u64,
    },
    /// Ask which opcodes in a `--capture` file carry relayed movement.
    ///
    /// The question this answers came out of a live run: watching one real
    /// client walk about produced nine movement-shaped opcodes, of which this
    /// client folds exactly three. Every other one is a position thrown away,
    /// which does not error and shows up only as a mover whose samples look
    /// further apart than they are.
    ///
    /// **Nothing is named from memory.** The test is structural and is the one
    /// this project already trusts: a relayed movement packet is a packed guid
    /// followed by a `MovementInfo`, and it must consume its body *exactly*.
    /// An opcode where every sample parses clean and every guid names the same
    /// handful of movers is carrying movement; one where the cursor is left
    /// holding bytes is not, whatever it is called elsewhere. The parser used
    /// is the shipped one rather than a second copy written for this command,
    /// because two parsers for one layout can disagree and only one of them is
    /// the one the client actually runs.
    Moves {
        /// A file written by `world --capture`.
        capture: PathBuf,
        /// Also print the movers and sample intervals for this opcode.
        #[arg(long)]
        detail: Option<String>,
    },
}

#[derive(Subcommand)]
enum AdtCommand {
    /// Summarize a map: which tiles exist and how alpha is stored.
    Map { map: String },
    /// Show one terrain tile.
    Tile {
        map: String,
        x: usize,
        y: usize,
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Parse every tile of a map, checking that chunks meet at their edges.
    Survey {
        /// Map directory name, e.g. `Azeroth`. Omit to sweep every map.
        map: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Ground height at a world position, the way the viewer resolves it.
    ///
    /// Takes network coordinates -- the ones the server reports and the ones
    /// the viewer walks in -- not the permuted form placements are stored in.
    Height {
        map: String,
        /// Clap reads a bare negative number as a flag, so pass `--x=-8950`.
        #[arg(long, allow_hyphen_values = true)]
        x: f32,
        #[arg(long, allow_hyphen_values = true)]
        y: f32,
    },
    /// Survey a map's liquid, and check it lies in the low ground.
    ///
    /// **This is the experiment that could refute the reader**, not a report on
    /// it. An `MH2O` instance covers a sub-rectangle of its chunk, and nothing
    /// in the file says which of the two axes `x_offset` indexes -- both
    /// readings parse, and both draw an entirely convincing pond, one of them a
    /// quarter of a chunk from where the pond is.
    ///
    /// What separates them is that **water collects in low ground**. Every
    /// liquid cell is measured against the terrain height beneath it under both
    /// readings, and the one that puts its water in the valleys is the one this
    /// client uses. A reading that lands sheets on hillsides shows up as a
    /// large fraction of cells whose liquid surface is *below* the ground it is
    /// supposed to be sitting on -- which is not a thing rivers do.
    Liquid {
        /// Map directory name, e.g. `Azeroth`. Omit to sweep every map.
        map: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum MapCommand {
    /// List every page: the world rectangle it shows and where its art lives.
    Pages {
        /// Only pages on this `Map.dbc` id.
        #[arg(long)]
        map: Option<u32>,
        /// Only pages whose directory contains this.
        filter: Option<String>,
    },
    /// Resolve a world position to a page and a pixel on it.
    ///
    /// Prints every step -- which pages contain the point, which one wins, the
    /// fraction and then the pixel -- because a pin in the wrong place because
    /// the wrong *page* was chosen and one in the wrong place because the
    /// projection is wrong look identical as a single coordinate, and want
    /// opposite investigations. Same reasoning as `adt height`.
    Locate {
        #[arg(long, default_value_t = 0)]
        map: u32,
        /// Clap reads a bare negative number as a flag, so pass `--x=-8950`.
        #[arg(long, allow_hyphen_values = true)]
        x: f32,
        #[arg(long, allow_hyphen_values = true)]
        y: f32,
    },
    /// Measure how much of a page's twelve tiles is art rather than padding.
    ///
    /// The tiles make a 1024x768 image and the picture stops short of its
    /// right and bottom edges, so a client that treats the whole grid as the
    /// map puts every pin a couple of percent off -- small enough to look like
    /// something else and never to fail. The art states where it ends, in its
    /// own alpha channel, so this asks it rather than assuming.
    Canvas {
        /// Page directory, e.g. `Elwynn`. Omit to check every page.
        directory: Option<String>,
    },
    /// List the explored-art patches drawn on a page, and check their files.
    ///
    /// The twelve base tiles are the *unexplored* picture; every road and
    /// building is one of these, revealed when the area it covers is explored.
    /// A patch larger than one tile is split into files the way a page is, and
    /// **that rule is confirmed here rather than assumed**: `--verify` resolves
    /// every tile of every patch by path, which is the only way to ask an MPQ
    /// whether a file exists -- a listing is a different question and has
    /// answered it wrongly here before.
    Overlays {
        /// Page directory, e.g. `Elwynn`. Omit to sweep every page.
        directory: Option<String>,
        /// Resolve every tile file, and report the ones that are missing.
        #[arg(long)]
        verify: bool,
    },
    /// Fit the world-to-page projection against the terrain, and report it.
    ///
    /// **The experiment that chose the projection, kept so it can be re-run.**
    /// Every `WorldMapOverlay` row states in page pixels where an area sits;
    /// every terrain chunk states in world coordinates which area it belongs
    /// to. Projecting the second and regressing it against the first fits a
    /// slope and an offset per axis.
    ///
    /// It presupposes neither answer it is testing. A flipped axis fits a
    /// **negative** slope, and the page's pixel size is whatever the slope's
    /// magnitude comes out as rather than a number chosen in advance -- so
    /// this can refute the projection instead of agreeing with it, which is
    /// the only kind of check worth running.
    Calibrate {
        /// Map directory, e.g. `Azeroth`. Omit for every map with pages.
        map: Option<String>,
        /// Report every page's own fit as well as the totals.
        #[arg(long)]
        verbose: bool,
    },
}

/// The minimap's art: the hashed-tile index, and the two things it does not
/// state.
#[derive(Subcommand)]
enum MinimapCommand {
    /// Read `md5translate.trs` and report what it names.
    ///
    /// The art is stored under the MD5 of its own contents, so this file is
    /// the only thing that says which picture belongs to which tile, and a
    /// directory listing cannot replace it -- one picture can serve many
    /// tiles, and a fifth of the files in the folder are referenced by
    /// nothing at all.
    Index {
        /// Resolve every referenced file by path, and report the ones that
        /// are missing. Resolution, not a listing: an MPQ answers by hash,
        /// and a listing has answered the wrong question here before.
        #[arg(long)]
        verify: bool,
    },
    /// Compare a map's minimap tiles against the tiles its `WDT` says exist.
    ///
    /// **This is what settles which of `map<a>_<b>` is which.** The two
    /// numbers could be `(x, y)` or `(y, x)` and both readings resolve a
    /// file for every tile, so nothing about a single lookup can tell them
    /// apart. What can is the *set*: a continent's tiles are not symmetric
    /// under exchanging the pair, so one order matches the terrain's own set
    /// exactly and the other cannot.
    Tiles {
        /// Map directory, e.g. `Azeroth`. Omit to sweep every map.
        map: Option<String>,
    },
    /// Fit the tile's pixel orientation against the water in the terrain.
    ///
    /// **The experiment that chose the orientation, kept so it can be re-run.**
    /// A 256x256 tile has eight plausible readings -- either axis can run
    /// either way, and they can be exchanged -- and every one of them draws a
    /// picture. So the art is scored against a fact stated by a different
    /// file: `MH2O` says which of a tile's 256 chunks are under water, and
    /// water is drawn blue.
    ///
    /// A chunk votes only if it is unambiguous (covered or dry, nothing in
    /// between) and only if the candidates actually disagree about it -- a
    /// tile that is all ocean or all forest agrees with every reading and
    /// would bury the ones that can answer, which is how `Light.dbc`'s storm
    /// column came back a coin flip.
    Orient {
        /// Map directory, e.g. `Azeroth`. Omit to sweep every map.
        map: Option<String>,
        /// Stop after this many tiles.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Score the same eight readings by whether the tiles join up.
    ///
    /// The companion to `orient`, and deliberately built on a different
    /// input: it reads no terrain at all. Under the right reading the last
    /// column of one tile's art is the ground immediately beside the first
    /// column of its neighbour's, so the seam is invisible; under a flipped
    /// one it is two pieces of ground five hundred yards apart.
    ///
    /// Neither direction settles it alone -- a reading that flips only the
    /// down axis walks the same across-seam backwards and scores identically
    /// on it -- so both are reported and it is the pair that separates all
    /// eight.
    Seams {
        /// Map directory, e.g. `Azeroth`. Omit to sweep every map.
        map: Option<String>,
        /// Stop after this many tiles.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Draw what the minimap would show at a world position, to a PNG.
    ///
    /// **The instrument that lets the composed picture be seen as itself.**
    /// The frame is assembled from up to four tiles placed by
    /// `adt::minimap::Viewport`, and this draws from *that same* type rather
    /// than from a second copy of the arithmetic -- so a picture that comes
    /// out right is evidence about the frame. Anything assembled in memory
    /// from a dozen files gets a dump command; this is the minimap's.
    Stitch {
        /// Map directory, e.g. `Azeroth`.
        #[arg(long, default_value = "Azeroth")]
        map: String,
        /// Clap reads a bare negative number as a flag, so pass `--x=-8950`.
        #[arg(long, allow_hyphen_values = true)]
        x: f32,
        #[arg(long, allow_hyphen_values = true)]
        y: f32,
        /// World units across the picture.
        #[arg(long, default_value_t = 200.0)]
        range: f32,
        /// Edge of the output image, in pixels.
        #[arg(long, default_value_t = 512)]
        pixels: u32,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write one tile's picture to a PNG.
    Export {
        /// Map directory, e.g. `Azeroth`.
        map: String,
        /// Clap reads a bare negative number as a flag, so pass `--x=32`.
        #[arg(long)]
        x: usize,
        #[arg(long)]
        y: usize,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum WmoCommand {
    /// Show a root file and its groups.
    Info {
        /// Archive path of the root `.wmo`, not a `_000` group file.
        path: String,
        #[arg(long, default_value_t = 12)]
        limit: usize,
    },
    /// Parse every root and group in the archives, validating the arrays.
    Survey {
        filter: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Ask what a material's `ground_type` holds, and whether the floor inside
    /// a building can be told from the wall beside it.
    Footing {
        filter: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum M2Command {
    /// Show a model's header, textures, materials, and skin geometry.
    Info {
        /// Archive path; `.mdx` is rewritten to `.m2` automatically.
        path: String,
        /// Level of detail to describe.
        #[arg(long, default_value_t = 0)]
        lod: u32,
        /// How many batches to list. A character model has sixty-odd and the
        /// question is usually "which geoset groups does this model contain",
        /// which a truncated list cannot answer.
        #[arg(long, default_value_t = 24)]
        limit: usize,
    },
    /// Parse every model and its skins, validating the index tables.
    Survey {
        filter: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Resolve a creature display id to its model, the way the renderer will.
    Creature { display_id: u32 },
    /// Find where a moving attachment goes, by asking which other attachment
    /// it comes closest to over an animation.
    ///
    /// Identifies the sheath points without transcribing a table. A character
    /// model carries animations named `Sheath` and `HipSheath`, and during
    /// them the hand carries the weapon to wherever it is stowed -- so the
    /// static attachment the *hand* approaches most nearly is the resting
    /// place, measured rather than assumed.
    AttachTrace {
        path: String,
        /// Sequence index to play. `m2 anims` lists them by name.
        #[arg(long)]
        anim: usize,
        /// The attachment that moves. Defaults to the right hand.
        #[arg(long, default_value_t = m2::Attachment::HAND_RIGHT)]
        track: u32,
        /// How many closest candidates to report.
        #[arg(long, default_value_t = 6)]
        limit: usize,
    },
    /// List a model's animations and how much of the skeleton each moves.
    Anims {
        path: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// List the points where other models hang off this one, with the bone
    /// each names and where that bone sits.
    Attachments {
        path: String,
        /// Pose the skeleton with this sequence index before reporting, and
        /// show how far each attachment's bone is turned.
        ///
        /// The bind pose of an M2 is the identity by construction, so an
        /// attachment that must hold something at an *angle* -- a sword lying
        /// diagonally across a back -- can only get that angle from its bone's
        /// animation. A large rotation on a torso-mounted point is therefore
        /// evidence about what hangs there.
        #[arg(long)]
        anim: Option<usize>,
        /// Report every attachment id across the whole archive set instead,
        /// tallying how many models carry each and which side of the model
        /// they sit on. A stride error shows up here as a flood of ids.
        #[arg(long)]
        survey: bool,
    },
    /// List a model's particle and ribbon emitters: what it burns, trails or
    /// sprays, and the curves each does it along.
    ///
    /// None of this is in the `.skin` file -- an emitter is a description and
    /// the geometry is made at runtime -- so this is the only way to look at
    /// it before the renderer does.
    Emitters {
        /// Archive path, or a substring to filter on when `--survey` is set.
        path: String,
        /// Read every model in the archives instead, and check the emitter
        /// blocks account for their own bytes.
        ///
        /// The question is the record stride, and it is asked in the one way
        /// that can come out the other way: every offset a record refers to
        /// must land *after* the whole block of records, and the first of them
        /// must land within one 16-byte alignment pad of the block's end. A
        /// stride a word short leaves the last record's tracks inside the
        /// block; a word long runs the block into the data it points at.
        #[arg(long)]
        survey: bool,
        /// Score a range of candidate strides rather than only the one the
        /// parser uses, and print the tally for each.
        ///
        /// A single stride agreeing with the data proves less than the
        /// neighbours disagreeing with it, which is the whole reason this
        /// exists.
        #[arg(long)]
        strides: bool,
    },
    /// List a model's timed events: the moments inside an animation when
    /// something is supposed to happen.
    ///
    /// Everything else in an M2 is continuous -- where a bone is at time `t`.
    /// An event is a bare list of timestamps with no value, and it is where
    /// the answers to "when does the foot land" and "when does the blade
    /// connect" live. This client guessed at the second of those with a
    /// hand-dialled constant precisely because nothing read this block.
    Events {
        /// Archive path, or a substring to filter on when `--survey` is set.
        path: String,
        /// Also load the external `.anim` files, so the sequences whose data
        /// moved out of the `.m2` report their timestamps.
        ///
        /// **A character's walk and run cycles are exactly those sequences**,
        /// so without this a footfall reads as an event that never fires.
        #[arg(long)]
        anims: bool,
        /// Read every model in the archives and tally which identifiers exist,
        /// how many models carry each, and how many sequences they fire in.
        #[arg(long)]
        survey: bool,
        /// Score candidate record strides instead.
        ///
        /// The identifier is four ASCII characters, which no neighbouring
        /// stride can produce by accident -- it shifts the name into the
        /// middle of a float. So the measurement is the share of records whose
        /// four bytes are printable, and the wrong answers are punctuation
        /// rather than plausible names.
        #[arg(long)]
        strides: bool,
        /// Pose the skeleton through this sequence and report where each
        /// event's own point is lowest, against the times it actually fires.
        ///
        /// Which of a model's event families marks a footfall cannot be read
        /// off the four-letter names without recalling a table nobody here has
        /// checked. It can be *measured*: a foot that is planted is a foot at
        /// the bottom of its travel.
        #[arg(long)]
        trace: Option<usize>,
    },
}

#[derive(Subcommand)]
enum ItemCommand {
    /// Show one item display's held geometry and textures.
    Display { display_id: u32 },
    /// Tally which inventory slots carry held geometry, and in which hand.
    ///
    /// The question this answers is not "could inventory type 17 be a weapon"
    /// but "is `model_right` filled in *because* an item is a weapon" -- a
    /// distinction this project has paid for before. Join `Item.dbc` to
    /// `ItemDisplayInfo` and the slots that hang a model off the skeleton
    /// separate from the ones that only paint the skin, without transcribing
    /// a single constant.
    Held,
    /// Cross-tabulate `Item.dbc`'s `sheathe_type` against inventory type.
    ///
    /// Says how many places a stowed weapon can rest and which slots share
    /// each, which is what decides how many attachment points sheathing needs
    /// before any of them are identified.
    Sheath,
}

#[derive(Subcommand)]
enum BlpCommand {
    /// Show a texture's encoding and mip chain.
    Info { path: String },
    /// Export a mip level to PNG.
    Export {
        path: String,
        #[arg(long, default_value_t = 0)]
        level: usize,
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Parse every texture in the archives and tally what the format space
    /// actually looks like.
    Survey {
        /// Only survey paths matching this substring.
        filter: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum DbcCommand {
    /// List every DBC in the archives with its shape.
    List {
        /// Case-insensitive substring to match.
        filter: Option<String>,
    },
    /// Show a table's header and inferred column types.
    ///
    /// Column types are not stored in the file, so this guesses them from the
    /// data. Use it to transcribe a table that has no schema yet.
    Info { table: String },
    /// Dump rows using inferred column types.
    Dump {
        table: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Dump rows through a transcribed schema.
    Rows {
        /// One of: Map, AreaTable, CreatureDisplayInfo,
        /// CreatureDisplayInfoExtra, CreatureModelData, AnimationData,
        /// Spell, SpellIcon, SkillLineAbility, SpellDuration, SpellRadius,
        /// CharSections, CharHairGeosets, ChrClasses, ChrRaces, Item,
        /// ItemDisplayInfo, Light,
        /// LightParams, LightIntBand, LightFloatBand,
        /// GameObjectDisplayInfo, SoundEntries, WorldSafeLocs,
        /// SpellVisualKit. Run `dbc check` for the current, authoritative
        /// list.
        table: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Show only these ids. Repeatable.
        ///
        /// Tables like `Spell` have fifty thousand rows, and the question is
        /// almost always about a handful of them.
        #[arg(long)]
        id: Vec<u32>,
    },
    /// Check every transcribed schema against the files in this install.
    Check,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    // The network commands touch no game files, so they must not demand a data
    // directory.
    match &cli.command {
        Command::Auth {
            host,
            user,
            password,
            port,
            timeout,
        } => return auth_login(host, *port, user, password, &cli.locale, *timeout),
        // Reads a capture file, not an archive.
        Command::Moves { capture, detail } => {
            return report_capture_moves(capture, detail.as_deref())
        }
        Command::World {
            host,
            user,
            password,
            realm,
            create,
            race,
            class,
            skin,
            char_face,
            hair_style,
            hair_color,
            facial_hair,
            delete,
            enter,
            dump_failed,
            walk,
            heading,
            strafe,
            jump,
            face,
            turn,
            run_mode,
            stay,
            units,
            objects,
            unit_fields,
            states,
            items,
            equip,
            swap,
            loot,
            gossip,
            gossip_select,
            buy,
            sell_back,
            trainer,
            learn,
            taxi,
            fly_to,
            mail_list,
            mail_to,
            mail_subject,
            mail_text,
            mail_money,
            mail_item,
            mail_take,
            mail_clear,
            mail_wait,
            mail_own_guid,
            guild,
            auction,
            auctioneer,
            auction_search,
            auction_offset,
            auction_sell,
            auction_bid,
            auction_buyout,
            auction_place,
            auction_id,
            auction_cancel,
            guild_invite,
            guild_accept,
            guild_note,
            guild_motd,
            guild_wait,
            guild_say,
            trade,
            trade_wait,
            trade_decline,
            trade_nobody,
            trade_item,
            trade_gold,
            trade_accept,
            trade_seconds,
            quest,
            item_query,
            use_item,
            quest_accept,
            quest_turnin,
            quest_sweep,
            party_invite,
            party_answer,
            party_leave,
            party_kick,
            quest_log,
            quest_poi,
            questgivers,
            questgiver_status,
            quest_info,
            target,
            own_fields,
            until_death,
            swing_probe,
            appearance,
            visible_items,
            select,
            select_self,
            release,
            reclaim,
            attack,
            sheath,
            capture,
            names,
            say,
            yell,
            whisper,
            spells,
            cast,
            cast_self,
            cast_at_object,
            port,
            timeout,
        } => {
            return world_login(WorldRequest {
                dump_failed: dump_failed.as_deref(),
                walk: *walk,
                heading: *heading,
                strafe: *strafe,
                jump: *jump,
                face: *face,
                turn: *turn,
                run_mode: *run_mode,
                stay: *stay,
                units: *units,
                objects: *objects,
                unit_fields: *unit_fields,
                states: *states,
                items: *items,
                equip,
                swap: swap.as_deref(),
                loot: *loot,
                gossip: *gossip,
                gossip_select: *gossip_select,
                buy: *buy,
                sell_back: *sell_back,
                trainer: *trainer,
                learn: *learn,
                taxi: *taxi,
                fly_to: *fly_to,
                mail_list: *mail_list,
                mail_to: mail_to.as_deref(),
                mail_subject,
                mail_text,
                mail_money: *mail_money,
                mail_item: *mail_item,
                mail_take: *mail_take,
                mail_clear: *mail_clear,
                mail_wait: *mail_wait,
                mail_own_guid: *mail_own_guid,
                guild: *guild,
                auction: *auction,
                auctioneer: *auctioneer,
                auction_search: auction_search.as_deref(),
                auction_offset: *auction_offset,
                auction_sell: *auction_sell,
                auction_bid: *auction_bid,
                auction_buyout: *auction_buyout,
                auction_place: *auction_place,
                auction_id: *auction_id,
                auction_cancel: *auction_cancel,
                guild_invite: guild_invite.as_deref(),
                guild_accept: *guild_accept,
                guild_note: guild_note.as_deref(),
                guild_motd: guild_motd.as_deref(),
                guild_wait: *guild_wait,
                guild_say: guild_say.as_deref(),
                trade: trade.as_deref(),
                trade_wait: *trade_wait,
                trade_decline: *trade_decline,
                trade_nobody: *trade_nobody,
                trade_item: *trade_item,
                trade_gold: *trade_gold,
                trade_accept: *trade_accept,
                trade_seconds: *trade_seconds,
                quest: *quest,
                item_query: item_query.as_deref(),
                use_item: *use_item,
                quest_accept: *quest_accept,
                quest_turnin: *quest_turnin,
                quest_sweep: *quest_sweep,
                party_invite: party_invite.as_deref(),
                party_answer: *party_answer,
                party_leave: *party_leave,
                party_kick: party_kick.as_deref(),
                quest_log: *quest_log,
                quest_poi: *quest_poi,
                questgivers: *questgivers,
                questgiver_status: *questgiver_status,
                quest_info,

                target: target.as_deref(),
                own_fields: *own_fields,
                until_death: *until_death,
                swing_probe: *swing_probe,
                appearance: *appearance,
                visible_items: *visible_items,
                select: *select,
                select_self: *select_self,
                release: *release,
                reclaim: *reclaim,
                attack: *attack,
                sheath: *sheath,
                capture: capture.as_deref(),
                names: *names,
                say,
                yell: *yell,
                whisper: whisper.as_deref(),
                spells: *spells,
                cast: *cast,
                cast_self: *cast_self,
                cast_at_object: *cast_at_object,
                host,
                port: *port,
                user,
                password,
                realm: realm.as_deref(),
                create: create.as_deref(),
                race: *race,
                class: *class,
                look: world::Appearance {
                    race: *race,
                    class: *class,
                    gender: 0,
                    skin: *skin,
                    face: *char_face,
                    hair_style: *hair_style,
                    hair_color: *hair_color,
                    facial_hair: *facial_hair,
                },
                delete: delete.as_deref(),
                enter: enter.as_deref(),
                locale: &cli.locale,
                timeout: *timeout,
                data: cli.data.as_deref(),
            })
        }
        _ => {}
    }

    let data = cli
        .data
        .context("no data directory: pass --data or set WOW_DATA")?;

    let mut chain = Chain::open_wow_data(&data, &cli.locale)
        .with_context(|| format!("opening archives under {}", data.display()))?;

    match cli.command {
        Command::Info => info(&mut chain),
        Command::Ls { filter, limit } => ls(&mut chain, filter.as_deref(), limit),
        Command::Extract { name, out } => extract(&mut chain, &name, out),
        Command::Which { name } => which(&chain, &name),
        Command::Verify { limit, filter } => verify(&mut chain, limit, filter.as_deref()),
        Command::Dbc(cmd) => dbc_cmd(&mut chain, cmd),
        Command::Blp(cmd) => blp_cmd(&mut chain, cmd),
        Command::M2(cmd) => m2_cmd(&mut chain, cmd),
        Command::Item(cmd) => item_cmd(&mut chain, cmd),
        Command::Sound(cmd) => sound_cmd(&mut chain, &cmd),
        Command::Taxi(cmd) => taxi_cmd(&mut chain, &cmd),
        Command::Spell(cmd) => spell_cmd(&mut chain, &cmd),
        Command::Wmo(cmd) => wmo_cmd(&mut chain, cmd),
        Command::Adt(cmd) => adt_cmd(&mut chain, cmd),
        Command::Map(cmd) => map_cmd(&mut chain, cmd),
        Command::Minimap(cmd) => minimap_cmd(&mut chain, cmd),
        Command::Light {
            map,
            x,
            y,
            hour,
            weather_check,
            band_survey,
        } => {
            if band_survey {
                light::band_survey(&mut chain)
            } else if weather_check {
                light::weather_check(&mut chain, hour)
            } else {
                light::report(&mut chain, map, x, y, hour)
            }
        }
        // Handled before the archives are opened.
        Command::Auth { .. } | Command::World { .. } | Command::Moves { .. } => unreachable!(),
    }
}

fn auth_login(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    locale: &str,
    timeout: u64,
) -> Result<()> {
    println!("connecting to {host}:{port} as {user}");
    let session = auth::login(
        host,
        port,
        user,
        password,
        locale,
        std::time::Duration::from_secs(timeout),
    )?;

    // The key itself is a secret; showing its length and a fingerprint is
    // enough to confirm it was negotiated.
    println!(
        "\nauthenticated. session key is {} bytes, fingerprint {:02x}{:02x}..{:02x}{:02x}",
        session.session_key.len(),
        session.session_key[0],
        session.session_key[1],
        session.session_key[38],
        session.session_key[39],
    );

    println!("\n{} realm(s):", session.realms.len());
    for realm in &session.realms {
        println!(
            "  {:<28} {:<24} {:>3} characters, population {:.2}{}{}",
            realm.name,
            realm.address,
            realm.characters,
            realm.population,
            if realm.locked { ", locked" } else { "" },
            if realm.is_offline() { ", offline" } else { "" },
        );
    }
    Ok(())
}

struct WorldRequest<'a> {
    host: &'a str,
    port: u16,
    user: &'a str,
    password: &'a str,
    realm: Option<&'a str>,
    create: Option<&'a str>,
    race: u8,
    class: u8,
    /// The full appearance `--create` should ask for, assembled from the
    /// individual flags so the caller can make a character whose numbers are
    /// all different -- see the `--skin` flag.
    look: world::Appearance,
    delete: Option<&'a str>,
    enter: Option<&'a str>,
    dump_failed: Option<&'a std::path::Path>,
    walk: Option<f32>,
    heading: Option<f32>,
    strafe: Option<Strafe>,
    jump: bool,
    face: Option<f32>,
    turn: Option<Turn>,
    run_mode: Option<RunMode>,
    stay: u64,
    units: Option<usize>,
    objects: bool,
    unit_fields: bool,
    states: bool,
    items: bool,
    equip: &'a [u16],
    swap: Option<&'a str>,
    loot: bool,
    gossip: Option<u32>,
    gossip_select: Option<u32>,
    buy: Option<u32>,
    sell_back: bool,
    trainer: Option<u32>,
    learn: Option<u32>,
    taxi: Option<u32>,
    fly_to: Option<u32>,
    mail_list: bool,
    mail_to: Option<&'a str>,
    mail_subject: &'a str,
    mail_text: &'a str,
    mail_money: u32,
    mail_item: Option<u32>,
    mail_take: bool,
    mail_clear: bool,
    mail_wait: u64,
    mail_own_guid: bool,
    guild: bool,
    auction: bool,
    auctioneer: Option<u32>,
    auction_search: Option<&'a str>,
    auction_offset: u32,
    auction_sell: Option<u32>,
    auction_bid: u32,
    auction_buyout: u32,
    auction_place: Option<u32>,
    auction_id: Option<u32>,
    auction_cancel: bool,
    guild_invite: Option<&'a str>,
    guild_accept: bool,
    guild_note: Option<&'a str>,
    guild_motd: Option<&'a str>,
    guild_wait: u64,
    guild_say: Option<&'a str>,
    trade: Option<&'a str>,
    trade_wait: bool,
    trade_decline: bool,
    trade_nobody: bool,
    trade_item: Option<u32>,
    trade_gold: Option<u32>,
    trade_accept: bool,
    trade_seconds: u64,
    quest: Option<u32>,
    item_query: Option<&'a [u32]>,
    use_item: Option<u32>,
    quest_accept: Option<u32>,
    quest_turnin: Option<u32>,
    quest_sweep: Option<u32>,
    party_invite: Option<&'a str>,
    party_answer: Option<PartyAnswer>,
    party_leave: bool,
    party_kick: Option<&'a str>,
    quest_log: bool,
    quest_poi: bool,
    questgivers: bool,
    questgiver_status: bool,
    quest_info: &'a [u32],
    target: Option<&'a str>,
    own_fields: bool,
    until_death: bool,
    swing_probe: Option<SwingProbe>,
    appearance: bool,
    visible_items: bool,
    select: bool,
    select_self: bool,
    release: bool,
    reclaim: bool,
    attack: bool,
    sheath: Option<u32>,
    capture: Option<&'a std::path::Path>,
    names: bool,
    say: &'a [String],
    yell: bool,
    whisper: Option<&'a str>,
    spells: bool,
    cast: Option<u32>,
    cast_self: bool,
    cast_at_object: Option<u64>,
    locale: &'a str,
    timeout: u64,
    /// Only touched by `--visible-items`, which is a game-file question
    /// wearing a network command's clothes -- see the comment in `main`
    /// about why `World` otherwise never demands a data directory.
    data: Option<&'a std::path::Path>,
}

/// What to do with a party invite that arrives while holding the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum PartyAnswer {
    Accept,
    Decline,
}

/// Which way to sidestep.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Strafe {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Turn {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum RunMode {
    Run,
    Walk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum SwingProbe {
    A,
    B,
    C,
}

/// Logs in and walks all the way through to the character list.
///
/// The two halves are deliberately one command: the session key only exists
/// inside a single logon, so there is nothing sensible to hand a separate
/// "connect to the world server" command short of caching a secret to disk.
fn world_login(request: WorldRequest<'_>) -> Result<()> {
    let WorldRequest {
        host,
        port,
        user,
        password,
        realm: realm_name,
        create,
        race,
        class,
        look,
        delete,
        enter,
        dump_failed,
        walk,
        heading,
        strafe,
        jump,
        face,
        turn,
        run_mode,
        stay,
        units,
        objects,
        unit_fields,
        states,
        items,
        equip,
        swap,
        loot,
        gossip,
        gossip_select,
        buy,
        sell_back,
        trainer,
        learn,
        taxi,
        fly_to,
        mail_list,
        mail_to,
        mail_subject,
        mail_text,
        mail_money,
        mail_item,
        mail_take,
        mail_clear,
        mail_wait,
        mail_own_guid,
        guild,
        auction,
        auctioneer,
        auction_search,
        auction_offset,
        auction_sell,
        auction_bid,
        auction_buyout,
        auction_place,
        auction_id,
        auction_cancel,
        guild_invite,
        guild_accept,
        guild_note,
        guild_motd,
        guild_wait,
        guild_say,
        trade,
        trade_wait,
        trade_decline,
        trade_nobody,
        trade_item,
        trade_gold,
        trade_accept,
        trade_seconds,
        quest,
        item_query,
        use_item,
        quest_accept,
        quest_turnin,
        quest_sweep,
        party_invite,
        party_answer,
        party_leave,
        party_kick,
        quest_log,
        quest_poi,
        questgivers,
        questgiver_status,
        quest_info,
        target,
        own_fields,
        until_death,
        swing_probe,
        appearance,
        visible_items,
        select,
        select_self,
        release,
        reclaim,
        attack,
        sheath,
        capture,
        names,
        say,
        yell,
        whisper,
        spells,
        cast,
        cast_self,
        cast_at_object,
        locale,
        data,
        timeout,
    } = request;
    let timeout = std::time::Duration::from_secs(timeout);

    println!("logging in to {host}:{port} as {user}");
    let session = auth::login(host, port, user, password, locale, timeout)?;

    let realm = pick_realm(&session.realms, realm_name)?;
    println!("realm {:?} at {} (id {})", realm.name, realm.address, realm.id);
    if realm.is_offline() {
        // Worth saying out loud: the handshake will simply fail to connect, and
        // that looks like a client bug rather than a realm that is down.
        println!("  note: the realm list marks this realm offline");
    }

    let (world_host, world_port) = world::client::split_realm_address(&realm.address)?;
    println!("connecting to the world server at {world_host}:{world_port}");

    let mut connection = world::Connection::open(
        &format!("{world_host}:{world_port}"),
        user,
        realm.id as u32,
        &session.session_key,
        timeout,
    )?;
    println!(
        "session accepted, header cipher running (expansion {})",
        connection.expansion
    );

    if let Some(name) = delete {
        let guid = connection
            .characters()?
            .into_iter()
            .find(|character| character.name.eq_ignore_ascii_case(name))
            .with_context(|| format!("no character named {name:?} to delete"))?
            .guid;
        let code = connection.delete_character(guid)?;
        println!(
            "delete {name:?}: {} ({code:#04x})",
            world::protocol::describe_char_result(code)
        );
    }

    if let Some(name) = create {
        let code = connection.create_character(name, &look)?;
        println!(
            "create {name:?} ({} {}, skin {} face {} hair {}/{} facial {}): {} ({code:#04x})",
            world::race_name(race),
            world::class_name(class),
            look.skin,
            look.face,
            look.hair_style,
            look.hair_color,
            look.facial_hair,
            world::protocol::describe_char_result(code)
        );
    }

    let characters = connection.characters()?;
    if characters.is_empty() {
        println!("\nno characters on this realm");
        return Ok(());
    }

    println!("\n{} character(s):", characters.len());
    for character in &characters {
        // Race and class are padded as one field: separately, a long race name
        // pushes every following column out of line.
        let kind = format!(
            "{} {}",
            world::race_name(character.race),
            world::class_name(character.class)
        );
        println!(
            "  {:<14} level {:<3} {kind:<24} map {:<4} at {:.1}, {:.1}, {:.1}{}{}",
            character.name,
            character.level,
            character.map,
            character.position[0],
            character.position[1],
            character.position[2],
            if character.is_ghost() { ", ghost" } else { "" },
            if character.needs_rename() {
                ", must be renamed"
            } else {
                ""
            },
        );
        // What the character is wearing, which the list has always carried and
        // never shown. These are *display* ids, which is what makes our own
        // body cheap to dress: another player's visible items arrive as entry
        // ids and need `Item.dbc` to get here.
        let worn: Vec<String> = character
            .equipment
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.display_id != 0)
            .map(|(index, slot)| {
                format!("{index}:display {} type {}", slot.display_id, slot.inventory_type)
            })
            .collect();
        if !worn.is_empty() {
            println!("      wearing {}", worn.join(", "));
        }
    }

    if let Some(name) = enter {
        let character = characters
            .iter()
            .find(|character| character.name.eq_ignore_ascii_case(name))
            .with_context(|| format!("no character named {name:?} on this realm"))?;

        println!("\nentering the world as {:?}", character.name);
        let position = connection.enter_world(character.guid)?;
        println!(
            "  in world on map {} at {:.1}, {:.1}, {:.1} facing {:.2} rad",
            position.map, position.x, position.y, position.z, position.orientation
        );
        // The character list already said where this character was. Agreement
        // between two packets that derive it separately is worth checking:
        // it is the cheapest confirmation that neither parse drifted.
        let drift = (position.x - character.position[0]).abs()
            + (position.y - character.position[1]).abs()
            + (position.z - character.position[2]).abs();
        if drift > 0.1 {
            println!("  note: differs from the character list position by {drift:.2}");
        }

        // Nothing marks the end of the login burst, so read until it stops.
        let rest = connection.drain(std::time::Duration::from_millis(1500), 512)?;
        let capture_path = capture;
        let mut capture = match capture_path {
            Some(path) => Some(Capture::create(path)?),
            None => None,
        };
        if let Some(capture) = capture.as_mut() {
            capture.record(&rest)?;
        }
        let mut state = report_object_updates(&rest, character.guid, dump_failed)?;

        if let Some(degrees) = face {
            let at = world::Position {
                x: position.x,
                y: position.y,
                z: position.z,
                orientation: position.orientation,
            };
            let heading = degrees.to_radians();
            connection.set_facing(character.guid, at, heading)?;
            // Stay on the line briefly. A packet sent and immediately followed
            // by a disconnect can be lost: the server has not processed it by
            // the time the socket closes, and the result is indistinguishable
            // from the packet having been malformed. This one cost a wrong
            // conclusion about the opcode before the pause was added.
            connection.drain(std::time::Duration::from_millis(500), 64)?;
            println!("\nturned to face {heading:.2} rad ({degrees:.0} deg)");
            println!("  re-enter to confirm the server kept it");
        }

        if let Some(distance) = walk {
            let from = world::Position {
                x: position.x,
                y: position.y,
                z: position.z,
                orientation: position.orientation,
            };
            let heading = heading.map(f32::to_radians).unwrap_or(from.orientation);
            // The default run speed for a character on foot. Walking faster
            // than this is what a server's movement checks exist to catch.
            const RUN_SPEED: f32 = 7.0;

            let motion = match strafe {
                None => world::motion::Motion {
                    forward: true,
                    ..Default::default()
                },
                Some(Strafe::Left) => world::motion::Motion {
                    strafe_left: true,
                    ..Default::default()
                },
                Some(Strafe::Right) => world::motion::Motion {
                    strafe_right: true,
                    ..Default::default()
                },
            };
            let (dx, dy) = motion.direction(heading);
            println!(
                "\n{} {distance:.0} units, facing {heading:.2} rad, travelling ({dx:+.2}, {dy:+.2})",
                match strafe {
                    None => "walking",
                    Some(Strafe::Left) => "strafing left",
                    Some(Strafe::Right) => "strafing right",
                }
            );
            println!("  movement flags {:#06x}", motion.flags());
            let (arrived, seen) =
                connection.travel(character.guid, from, heading, motion, distance, RUN_SPEED)?;
            println!(
                "  client is now at {:.1}, {:.1}, {:.1}",
                arrived.x, arrived.y, arrived.z
            );

            let mut counts: std::collections::BTreeMap<u16, (usize, Vec<u8>)> = Default::default();
            for packet in &seen {
                let entry = counts.entry(packet.opcode).or_default();
                entry.0 += 1;
                entry.1 = packet.body.clone();
            }
            println!("  server sent {} packets while walking:", seen.len());
            for (opcode, (count, body)) in &counts {
                // Short bodies are printed in full: an unrecognised opcode that
                // arrives once per packet sent is worth identifying, and one
                // byte of payload is usually enough to guess what it means.
                let preview: String = body
                    .iter()
                    .take(8)
                    .map(|b| format!("{b:02x} "))
                    .collect();
                println!(
                    "    {:<32} x{count:<4} {} bytes [{}]",
                    world::opcode::describe(*opcode),
                    body.len(),
                    preview.trim_end()
                );
            }
            // Movement is never acknowledged, so the only honest confirmation
            // is to ask the server again later. Re-entering is the quick way;
            // the character list also works but only once the previous
            // session's logout-save has landed, which takes tens of seconds.
            // Checking it immediately reads the save *before* last and looks
            // exactly like movement having been ignored.
            println!(
                "  nothing acknowledges movement. Re-enter to see the position\n  \
                 the server holds, or re-read the character list after ~30s."
            );
        }

        if jump {
            let at = world::Position {
                x: position.x,
                y: position.y,
                z: position.z,
                orientation: position.orientation,
            };
            println!("\njumping on the spot at {:.1}, {:.1}, {:.1}", at.x, at.y, at.z);
            let seen = connection.jump_in_place(character.guid, at)?;
            println!(
                "  take-off velocity {:.4}, gravity {:.4}",
                world::motion::JUMP_VELOCITY,
                world::motion::GRAVITY
            );
            println!("  server sent {} packets during the jump", seen.len());
            for packet in &seen {
                println!(
                    "    {:<32} {} bytes",
                    world::opcode::describe(packet.opcode),
                    packet.body.len()
                );
            }
        }

        if let Some(direction) = turn {
            let at = world::Position {
                x: position.x,
                y: position.y,
                z: position.z,
                orientation: position.orientation,
            };
            // A slow, deliberate rate: fast enough to be unambiguously a
            // turn in a two-second capture, slow enough that a real client
            // holding the key would produce the same shape of packets.
            const RADIANS_PER_SEC: f32 = 1.5;
            let clockwise = direction == Turn::Right;
            println!(
                "\nturning {} on the spot for 2s",
                if clockwise { "right" } else { "left" }
            );
            let seen = connection.turn_in_place(
                character.guid,
                at,
                clockwise,
                std::time::Duration::from_secs(2),
                RADIANS_PER_SEC,
            )?;
            println!("  server sent {} packets while turning", seen.len());
            for packet in &seen {
                println!(
                    "    {:<32} {} bytes",
                    world::opcode::describe(packet.opcode),
                    packet.body.len()
                );
            }
        }

        if let Some(mode) = run_mode {
            let at = world::Position {
                x: position.x,
                y: position.y,
                z: position.z,
                orientation: position.orientation,
            };
            let walking = mode == RunMode::Walk;
            connection.set_run_mode(character.guid, at, walking)?;
            connection.drain(std::time::Duration::from_millis(500), 64)?;
            println!(
                "\nswitched to {}",
                if walking { "walking" } else { "running" }
            );
        }

        // A named target, resolved through the same name table the unit frames
        // read. Selected before `--cast` and `--attack` so both act on it.
        let mut chosen = None;
        if let Some(wanted) = target {
            // Names first: a target is matched by name, and until the queries
            // come back every unit is a bare guid. This is why `--target`
            // implies `--names` rather than merely recommending it -- the first
            // version matched against numbers and silently found nothing.
            println!("
resolving names to find {wanted:?}:");
            resolve_names(&mut connection, &mut state, 2)?;
            let mut candidates = nearest_ordered(&state, character.guid);
            candidates.retain(|(_, entity)| {
                unit_label(&state, entity)
                    .to_lowercase()
                    .contains(&wanted.to_lowercase())
            });
            match candidates.first() {
                Some((distance, entity)) => {
                    connection.set_selection(entity.guid)?;
                    connection.drain(std::time::Duration::from_millis(500), 64)?;
                    println!(
                        "
targeting {} ({:#x}), {distance:.1} units away",
                        unit_label(&state, entity),
                        entity.guid
                    );
                    chosen = Some(entity.guid);
                }
                None => println!("
nothing replicated matches {wanted:?}"),
            }
        }

        // Targeting yourself, which a real client does whenever the player
        // clicks their own portrait -- and which several GM commands require
        // before they will act on you. `.die` is the one that matters here: it
        // falls back to the caller when nothing is selected, but then refuses
        // unless `UNIT_FIELD_TARGET` is actually set, so a session that has
        // never targeted anything cannot kill itself. That is a one-line
        // difference between "the command does nothing" and a death capture.
        if select_self {
            connection.set_selection(character.guid)?;
            connection.drain(std::time::Duration::from_millis(500), 64)?;
            println!("\nselected self ({:#x})", character.guid);
        }

        if select && chosen.is_none() {
            match nearest_unit(&state, character.guid) {
                Some((guid, distance)) => {
                    connection.set_selection(guid)?;
                    // Nothing acknowledges a selection, so the only signal is
                    // whether the session survives being given one. A packet
                    // sent and immediately followed by a disconnect is often
                    // never processed at all -- the same trap that once got a
                    // facing opcode written off as wrong.
                    connection.drain(std::time::Duration::from_millis(500), 64)?;
                    println!("\nselected {guid:#x}, {distance:.1} units away");
                    println!("  the session survived it, which is all a selection can prove");
                }
                None => println!("\nnothing replicated to select"),
            }
        }

        if let Some(asked) = sheath {
            use world::combat::SheathState;

            // Read the byte back before and after, because the value alone
            // proves nothing: this field is zero by default and stays zero
            // through an entire fight, so "it is 0" and "the send was ignored"
            // are the same observation. What separates them is the byte
            // *following* the value asked for.
            let before = own_sheath(&state, character.guid);
            let wanted = match asked {
                1 => SheathState::Melee,
                2 => SheathState::Ranged,
                _ => SheathState::Unarmed,
            };
            connection.set_sheathed(wanted)?;
            // Give the server time to act. A packet sent and immediately
            // followed by a disconnect is often never processed at all.
            let packets = connection.drain(std::time::Duration::from_millis(1500), 256)?;
            state.replicate(&packets, None);
            let after = own_sheath(&state, character.guid);
            println!("\nasked for sheath state {asked} ({wanted:?})");
            println!(
                "  UNIT_FIELD_BYTES_2 byte 0: {before:?} -> {after:?}{}",
                if after == Some(wanted) {
                    "  (the server took it)"
                } else {
                    "  (unchanged -- the send was not understood, or nothing was resent)"
                }
            );
        }

        // **Where this character actually is**, as opposed to where replicated
        // state believes it is.
        //
        // These are not the same and the difference grows with every step. The
        // server never relays a client's own movement back, so our entry in
        // `WorldState` holds the *login* position forever -- a fact documented
        // on `Entity::position` and walked into again here. `--attack` closes
        // to melee and `--loot` then measured the corpse's distance from the
        // login spot, reported 15 units, and refused a request that would have
        // succeeded. Anything downstream of a walk has to measure from this.
        let mut here = world::Position {
            x: position.x,
            y: position.y,
            z: position.z,
            orientation: position.orientation,
        };

        if attack {
            // Walk into melee range and turn to face the target before
            // swinging. The first attempt skipped both, and the server
            // answered with two different empty-bodied refusals, three times
            // each, and no damage -- a swing is refused for being out of range
            // and for facing the wrong way, and neither refusal produces the
            // damage packet this exists to capture.
            const MELEE_REACH: f32 = 2.5;
            const RUN_SPEED: f32 = 7.0;
            // Creatures wander, so one approach is not enough: by the time a
            // walk finishes, the target has moved. Re-measured and repeated
            // rather than assumed.
            const APPROACHES: usize = 4;

            let mut chosen = None;
            for attempt in 0..APPROACHES {
                let Some((guid, distance)) = nearest_unit_from(&state, character.guid, here) else {
                    break;
                };
                chosen = Some(guid);
                let Some(there) = state.get(guid).and_then(|e| e.position) else {
                    break;
                };
                let heading = (there.y - here.y).atan2(there.x - here.x);
                if distance <= MELEE_REACH {
                    connection.set_facing(character.guid, here, heading)?;
                    println!("  in reach of {guid:#x} at {distance:.1} units, facing it");
                    break;
                }
                let close = distance - MELEE_REACH;
                println!(
                    "  approach {}: closing {close:.1} units on {guid:#x}",
                    attempt + 1
                );
                let (arrived, _) =
                    connection.walk(character.guid, here, heading, close, RUN_SPEED)?;
                // Where the *client* now is. The server never relays our own
                // movement back, so `state` still believes the login position
                // and measuring the next approach from it would aim at where
                // we used to be.
                here = arrived;
                here.orientation = heading;
                let batch = connection.drain(std::time::Duration::from_millis(400), 128)?;
                // Not discarded: a fold hands back the events it will never
                // store, and dropping them here is what made a later summary
                // report more attacks stopped than ever started.
                let report = state.replicate(&batch, None);
                print_events(&report, &state, character.guid);
            }

            match chosen {
                Some(guid) => {
                    // Selected first: the server decides whether an attack has
                    // a legal victim, and it decides that about the *selected*
                    // target. Sending a swing at something never selected is a
                    // different request than the client ever makes.
                    connection.set_selection(guid)?;
                    connection.attack_swing(guid)?;
                    println!("swinging at {guid:#x}");

                    // And check it actually started, rather than assuming.
                    // A creature keeps wandering while the approach walk runs,
                    // so a swing sent at where it was is refused for range and
                    // nothing happens -- which reads as the whole command
                    // being broken rather than as the target having moved two
                    // yards. Confirmed by watching for swings, not by hoping.
                    let mut started = false;
                    for _ in 0..3 {
                        let batch = connection.drain(std::time::Duration::from_millis(1200), 128)?;
                        // **Into the capture, which it was not.** This loop is
                        // where the interesting swings arrive -- a level-one
                        // fight can be over before `--stay` begins -- and a
                        // `--capture` run that recorded only the quiet phase
                        // came back with 506 monster moves and not one
                        // `SMSG_ATTACKERSTATEUPDATE`. Same rule as printing a
                        // refused packet's body rather than its length: the
                        // phase that produces the evidence is the one that has
                        // to keep it.
                        if let Some(capture) = capture.as_mut() {
                            capture.record(&batch)?;
                        }
                        let report = state.replicate(&batch, None);
                        print_events(&report, &state, character.guid);
                        if !report.swings.is_empty() {
                            started = true;
                            break;
                        }
                        connection.attack_swing(guid)?;
                    }
                    if !started {
                        println!(
                            "  no swings came back. The target has probably moved out of \
                             reach again -- rerun, or watch the census under --stay."
                        );
                    }
                }
                None => println!("\nnothing replicated to attack"),
            }
        }

        // Before the fight as well as after it, because the question this flag
        // exists to answer is *which field changed*, and one snapshot of a dead
        // character cannot answer it. A field that is set on both sides says
        // nothing; a field that appears, vanishes or moves is the answer.
        if own_fields {
            report_own_fields(&state, character.guid, "before");
        }

        if until_death {
            fight_until_death(&mut connection, &mut state, character, target, capture.as_mut())?;
        }

        if let Some(probe) = swing_probe {
            here = run_swing_probe(
                &mut connection,
                &mut state,
                character,
                probe,
                target,
                here,
                capture.as_mut(),
            )?;
        }

        // Before `--stay`, so the names are in hand when chat starts arriving
        // and a line from another player is attributed rather than numbered.
        if names {
            println!("\nresolving names:");
            resolve_names(&mut connection, &mut state, 2)?;
        }

        for text in say {
            // The character's own language, not Universal -- see
            // `chat::language_for_race`. Read from replicated state rather
            // than from the character list so it comes from the same place the
            // interface would read it.
            let language = world::chat::language_for_race(
                state
                    .get(character.guid)
                    .and_then(|entity| entity.race())
                    .unwrap_or(character.race),
            );
            // Whisper wins over yell: it is the only chat with no range at
            // all, which makes it the one that tests delivery between two
            // clients without their positions being part of the experiment.
            let (kind, target) = match (whisper, yell) {
                (Some(name), _) => (world::ChatType::Whisper, name),
                (None, true) => (world::ChatType::Yell, ""),
                (None, false) => (world::ChatType::Say, ""),
            };
            connection.say(kind, language, target, text)?;
            println!(
                "\n{} {text:?}{} in language {language}",
                kind.label(),
                if target.is_empty() {
                    String::new()
                } else {
                    format!(" to {target}")
                }
            );
            // What comes back is the server relaying it to everyone in range,
            // this client included -- so the echo below is itself the proof
            // that the send was well formed.
            let batch = connection.drain(std::time::Duration::from_millis(900), 128)?;
            // Every opcode that came back, not just the ones that parsed.
            // "Nothing was echoed" and "the echo arrived and this client could
            // not read it" are completely different problems, and printing
            // only the successes makes them look identical -- which cost a
            // wrong conclusion here before this histogram existed.
            let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
            for packet in &batch {
                *seen.entry(packet.opcode).or_default() += 1;
            }
            let report = state.replicate(&batch, None);
            print_events(&report, &state, character.guid);
            for (opcode, error, body) in &report.failures {
                println!(
                    "  undecodable {}: {error}",
                    world::opcode::describe(*opcode)
                );
                if let Ok(body) = body {
                    println!("    {} bytes: {}", body.len(), hex_preview(body, 48));
                }
            }
            if report.chat.is_empty() {
                println!("  no chat came back. What did arrive:");
                for (opcode, count) in &seen {
                    println!("    {:<32} x{count}", world::opcode::describe(*opcode));
                }
            }
        }

        // **After `--say`, deliberately.** Releasing is only legal for a
        // character who is already dead, and the way this rig kills one is a GM
        // command sent as chat. Run in the order the flags are declared instead
        // and the release goes out while the character is still alive, is
        // refused in silence, and reads as the opcode being wrong -- which is
        // exactly what the first attempt looked like, right down to the
        // reclaim-delay packet arriving afterwards to prove the death had
        // happened all along.
        // Where the release left us, threaded into the reclaim: a ghost is
        // somewhere the server chose and this client was told once, and the
        // replicated copy of our own position is frozen at login because the
        // server never relays our movement back to us.
        let mut ghost_at = None;
        if release {
            ghost_at = report_release(&mut connection, &mut state, character, capture.as_mut())?;
        }

        if reclaim {
            report_reclaim(
                &mut connection,
                &mut state,
                character,
                ghost_at,
                capture.as_mut(),
            )?;
        }

        if spells {
            report_spells(&state);
        }

        if let (Some(spell_id), None) = (cast, cast_at_object) {
            let target = if cast_self {
                None
            } else {
                // A `--target` name wins over "nearest", which is very often a
                // friendly standing closer than anything worth casting at.
                chosen.or_else(|| nearest_unit(&state, character.guid).map(|(guid, _)| guid))
            };

            // Up to four attempts, re-facing before each. **A wandering target
            // is the whole reason this is a loop.** The same cast at the same
            // creature was refused `0x61` at 28 units and accepted at 44 -- not
            // because of range, but because the creature had turned a corner
            // between the facing packet and the cast. `--attack` learned this
            // first and re-swings for the same reason; one attempt makes a
            // working command look broken half the time, which is worse than
            // failing every time.
            for attempt in 1..=4 {
                if let Some(guid) = target {
                    // Tell the server what is selected before asking it to cast
                    // at it: the server decides whether a spell has a legal
                    // victim.
                    connection.set_selection(guid)?;
                    // And turn to face it. A spell aimed at something behind
                    // the caster is refused with an empty body -- the same
                    // shape that made a melee swing look like a wrong opcode
                    // until the approach loop turned first.
                    if let (Some(here), Some(there)) = (
                        state.get(character.guid).and_then(|e| e.position),
                        state.get(guid).and_then(|e| e.position),
                    ) {
                        let heading = (there.y - here.y).atan2(there.x - here.x);
                        connection.set_facing(character.guid, here, heading)?;
                        // Half a second: a packet sent immediately before the
                        // next one is often not processed first.
                        let settle =
                            connection.drain(std::time::Duration::from_millis(500), 64)?;
                        state.replicate(&settle, None);
                    }
                }
                connection.cast_spell(spell_id, target)?;
                println!(
                    "
cast {spell_id} at {} (attempt {attempt})",
                    match target {
                        Some(guid) => format!("{guid:#x}"),
                        None => "yourself".into(),
                    }
                );

                let batch = connection.drain(std::time::Duration::from_millis(900), 128)?;
                // Into the capture as well as the tally. Counting an opcode
                // says a shape arrived; writing a parser needs the shape. This
                // drain is where a spell's own reply lands -- the damage log,
                // the threat update -- and it was counted and dropped, which is
                // the exact failure `CLAUDE.md` records for
                // SMSG_ATTACKERSTATEUPDATE: the one packet that could answer
                // the question, seen and lost.
                if let Some(capture) = capture.as_mut() {
                    capture.record(&batch)?;
                }
                let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
                for packet in &batch {
                    *seen.entry(packet.opcode).or_default() += 1;
                }
                let report = state.replicate(&batch, None);
                for failure in &report.cast_failures {
                    println!(
                        "  refused: {} (spell {})",
                        world::spell::describe_cast_failure(failure.reason),
                        failure.spell_id
                    );
                }
                for message in &report.chat {
                    println!("  {}", describe_chat(message, &state));
                }
                // The whole point of the cast: what it did. Printed as the
                // sentence the interface would show, built by the same
                // function, so the two cannot describe one fight differently.
                for hit in &report.spell_damage {
                    println!(
                        "  {}",
                        world::combat::describe_spell_damage(
                            hit,
                            character.guid,
                            |guid| unit_label_for(&state, guid),
                            |_| None,
                        )
                    );
                    println!(
                        "    school {:#x}, {} unread trailing byte(s){}",
                        hit.school_mask,
                        hit.trailing.len(),
                        if hit.trailing.iter().any(|b| *b != 0) {
                            format!(": {:02x?} -- NOT all zero, worth a look", hit.trailing)
                        } else {
                            String::new()
                        }
                    );
                }
                if report.cast_failures.is_empty() {
                    println!("  not refused. What arrived:");
                    for (opcode, count) in &seen {
                        println!("    {:<32} x{count}", world::opcode::describe(*opcode));
                    }
                    break;
                }
            }
        }

        // A lock-opening probe for `foss-wow#141`: which of several
        // identically-named `Spell.dbc` "Opening" candidates actually
        // satisfies a given lock is not derivable from this project's own
        // transcribed columns, so this tries one live and reports the
        // server's own answer -- `SMSG_CAST_FAILED` naming a reason, or
        // silence plus a lootable state, which is what success looks like
        // for a spell nothing acknowledges directly.
        if let (Some(spell_id), Some(guid)) = (cast, cast_at_object) {
            connection.set_selection(guid)?;
            connection.cast_spell_at_gameobject(spell_id, guid)?;
            println!("\ncast {spell_id} at game object {guid:#018x}");
            let batch = connection.drain(std::time::Duration::from_millis(900), 128)?;
            let report = state.replicate(&batch, None);
            for start in &report.cast_starts {
                println!(
                    "  SMSG_SPELL_START: caster {:#018x}, spell {}, cast_time_ms {}, target {:#018x}",
                    start.caster, start.spell_id, start.cast_time_ms, start.target
                );
            }
            for (opcode, error, _) in &report.failures {
                println!("  parse failure on {}: {error}", world::opcode::describe(*opcode));
            }
            if report.cast_failures.is_empty() {
                let lootable = state
                    .get(guid)
                    .is_some_and(|entity| entity.lootable());
                println!(
                    "  not refused -- lootable now: {lootable}, loot open: {}",
                    state.loot.is_some()
                );
                let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
                for packet in &batch {
                    *seen.entry(packet.opcode).or_default() += 1;
                }
                for (opcode, count) in &seen {
                    println!("    {:<32} x{count}", world::opcode::describe(*opcode));
                }
            }
            for failure in &report.cast_failures {
                println!(
                    "  refused: {} (spell {})",
                    world::spell::describe_cast_failure(failure.reason),
                    failure.spell_id
                );
            }
        }

        // Before the hold, deliberately: the invitee is the client that is
        // holding, so the invite has to go out while they are still watching.
        if let Some(name) = party_invite {
            invite_to_party(&mut connection, &mut state, name)?;
        }

        if stay > 0 {
            hold_connection(
                &mut connection,
                std::time::Duration::from_secs(stay),
                &mut state,
                character.guid,
                capture.as_mut(),
                party_answer,
            )?;
        }

        // After the hold, so there is a group to act on: both of these are
        // silent requests, and their only evidence is the `SMSG_GROUP_LIST`
        // that follows.
        if let Some(name) = party_kick {
            match state
                .party
                .as_ref()
                .and_then(|p| p.members.iter().find(|m| m.name.eq_ignore_ascii_case(name)))
                .map(|m| m.guid)
            {
                Some(guid) => {
                    println!("\nthrowing {name:?} ({guid:#018x}) out of the group");
                    connection.group_uninvite(guid)?;
                    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
                    dump_party_packets(&batch);
                    let report = state.replicate(&batch, None);
                    print_events(&report, &state, character.guid);
                }
                // Named rather than guessed at: the kick request takes a
                // guid, and inventing one from a name the party does not
                // hold would send a well-formed request about nobody.
                None => println!(
                    "\nno party member called {name:?} -- the group holds {:?}",
                    state
                        .party
                        .as_ref()
                        .map(|p| p.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>())
                        .unwrap_or_default()
                ),
            }
        }

        if party_leave {
            println!("\nleaving the group");
            connection.group_disband()?;
            let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
            dump_party_packets(&batch);
            let report = state.replicate(&batch, None);
            print_events(&report, &state, character.guid);
            // The proof, and it is a state read rather than a packet: the
            // server says "you are in no group" with an empty
            // `SMSG_GROUP_LIST`, so what settles this is what the party
            // *is* afterwards, not that anything arrived.
            report_party(&state);
        }

        if let Some(limit) = units {
            report_units(&state, character.guid, limit);
        }

        if objects {
            report_game_objects(&state);
        }

        for slot in equip {
            equip_and_report(&mut connection, &mut state, character.guid, *slot)?;
        }

        if let Some(pair) = swap {
            swap_and_report(&mut connection, &mut state, character.guid, pair)?;
        }

        if loot {
            survey_loot(&mut connection, &mut state, character.guid, here)?;
        }

        if let Some(entry) = gossip {
            // Zero is `--gossip` with no value: no preference, take the
            // nearest. A real creature entry is never 0, so the two cannot be
            // confused.
            let prefer = (entry != 0).then_some(entry);
            survey_gossip(
                &mut connection,
                &mut state,
                character.guid,
                &mut here,
                prefer,
                gossip_select,
                buy,
                sell_back,
            )?;
        }

        if let Some(entry) = trainer {
            let prefer = (entry != 0).then_some(entry);
            survey_trainer(
                &mut connection,
                &mut state,
                character.guid,
                &mut here,
                prefer,
                learn,
            )?;
        }


        if auction {
            survey_auction(
                &mut connection,
                &mut state,
                character.guid,
                &mut here,
                AuctionProbe {
                    prefer: auctioneer,
                    search: auction_search,
                    offset: auction_offset,
                    sell: auction_sell,
                    bid: auction_bid,
                    buyout: auction_buyout,
                    place: auction_place,
                    id: auction_id,
                    cancel: auction_cancel,
                },
            )?;
        }

        if let Some(entry) = taxi {
            let prefer = (entry != 0).then_some(entry);
            survey_taxi(
                &mut connection,
                &mut state,
                character.guid,
                &mut here,
                prefer,
                fly_to,
            )?;
        }

        if mail_list
            || mail_to.is_some()
            || mail_take
            || mail_clear
            || mail_wait > 0
            || mail_own_guid
        {
            survey_mail(
                &mut connection,
                &mut state,
                character.guid,
                &mut here,
                MailDrive {
                    list: mail_list,
                    to: mail_to,
                    subject: mail_subject,
                    text: mail_text,
                    money: mail_money,
                    item: mail_item,
                    take: mail_take,
                    clear: mail_clear,
                    wait: mail_wait,
                    own_guid: mail_own_guid,
                },
            )?;
        }

        if guild
            || guild_invite.is_some()
            || guild_accept
            || guild_note.is_some()
            || guild_motd.is_some()
            || guild_wait > 0
            || guild_say.is_some()
        {
            survey_guild(
                &mut connection,
                &mut state,
                character.guid,
                GuildDrive {
                    roster: guild,
                    invite: guild_invite,
                    accept: guild_accept,
                    note: guild_note,
                    motd: guild_motd,
                    wait: guild_wait,
                    say: guild_say,
                },
            )?;
        }

        if trade_nobody || trade.is_some() || trade_wait {
            survey_trade(
                &mut connection,
                &mut state,
                character.guid,
                &mut here,
                TradeDrive {
                    partner: trade,
                    wait: trade_wait,
                    decline: trade_decline,
                    nobody: trade_nobody,
                    item: trade_item,
                    gold: trade_gold,
                    accept: trade_accept,
                    seconds: trade_seconds,
                },
            )?;
        }

        if let Some(entry) = quest {
            let prefer = (entry != 0).then_some(entry);
            survey_quests(
                &mut connection,
                &mut state,
                character.guid,
                &mut here,
                prefer,
                quest_accept,
                quest_turnin,
                capture.as_mut(),
            )?;
        }

        if quest_poi {
            survey_quest_poi(&mut connection, &mut state, character.guid, data, locale)?;
        }

        if questgiver_status {
            survey_questgiver_status(&mut connection, &mut state, character.guid)?;
        }

        if questgivers {
            survey_questgivers(&mut connection, &mut state, character.guid, character.map)?;
        }

        for quest in quest_info {
            survey_quest_info(&mut connection, *quest, capture.as_mut())?;
        }

        if let Some(highest) = quest_sweep {
            sweep_quests(&mut connection, highest)?;
        }

        if quest_log {
            report_quest_log(&state, character.guid);
        }

        if unit_fields {
            report_unit_fields(&state, character.guid);
        }

        if states {
            report_states(&state, character.guid);
        }

        if items {
            report_items(&state, character.guid);
        }

        if appearance {
            report_appearance(&state, character);
        }

        if visible_items {
            report_visible_items(&state, character, data, locale)?;
        }

        if let Some(entry) = use_item {
            survey_use_item(&mut connection, &mut state, character.guid, entry)?;
        }

        if let Some(entries) = item_query {
            survey_item_query(
                &mut connection,
                &mut state,
                character.guid,
                entries,
                data,
                locale,
            )?;
        }

        if own_fields {
            report_own_fields(&state, character.guid, "after");
        }

        // **Last, after every survey.** This used to sit halfway up the
        // function, which silently made the capture file a record of the login
        // burst and nothing else: any survey below it recorded into a writer
        // that had already been flushed and reported. The quest surveys are
        // below it and are exactly the ones whose packets are worth keeping.
        if let (Some(capture), Some(path)) = (capture, capture_path) {
            capture.finish(path)?;
        }
    }
    Ok(())
}

/// Finds which update fields carry another player's worn items, by the same
/// measure-not-transcribe technique [`report_appearance`] uses for a face.
///
/// **The bridge is `Item.dbc`, and that is why this is the one `world` flag
/// that touches game files at all** (see the comment in `main` about `World`
/// otherwise never demanding a data directory): the wire's visible-item
/// fields hold item *entry* ids, and only `Item.dbc` says which display id an
/// entry resolves to.
///
/// The known answer is `SMSG_CHAR_ENUM`'s own equipment block, which already
/// gives our own character's worn items as display ids per slot. So for each
/// set field of our own player object, read the value as an entry id,
/// resolve it through `Item.dbc`, and report which fields' resolved display
/// ids agree with which equipped slot. Where at least two slots each name
/// exactly one field, a consistent `base + slot * stride` explaining all of
/// them is reported too -- the same shape of evidence
/// `PLAYER_FIELD_INV_SLOT_HEAD` was confirmed with, and it is what
/// `foss-wow#23` wants named in `crates/world/src/update.rs` once it holds.
/// Uses a carried item and reports what changed.
///
/// **Nothing acknowledges a use, so this is confirmed by consequence** --
/// the same shape as the buy that was proved by `PLAYER_FIELD_COINAGE`
/// dropping. The item's stack count is read before and after; a consumable
/// that drops by one was used, and nothing else in a quiet session moves
/// that field.
///
/// The silence is bounded first, the way `CMSG_LIST_INVENTORY` bounded
/// `CMSG_BUY_ITEM`: the on-use spell has to be fetched with
/// `CMSG_ITEM_QUERY_SINGLE` anyway, and that request *is* answered. A run
/// that resolves the name and spell and then sees no effect has a use-item
/// problem; one that never resolves the name has a session problem, and the
/// two want opposite investigations.
fn survey_use_item(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    entry: u32,
) -> Result<()> {
    // Where the thing actually is, including inside a bag -- `Where` carries
    // the distinction and `address` turns it into the pair the wire wants.
    let found = world::inventory::carried(state, own_guid)
        .into_iter()
        .find(|carried| carried.item.entry == Some(entry));
    let Some(carried) = found else {
        println!("\n--use-item: nothing with entry {entry} is in the bags");
        return Ok(());
    };
    let (bag, slot) = carried.at.address();
    println!(
        "\nusing entry {entry}: guid {:#x} at bag {bag} slot {slot}, stack {}",
        carried.item.guid, carried.item.count
    );

    // The answered request first: it supplies the spell id the use needs and
    // proves the session is live before anything silent is sent.
    connection.ask_item(entry, carried.item.guid)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut info = None;
    while std::time::Instant::now() < deadline && info.is_none() {
        let batch = connection.drain(std::time::Duration::from_millis(700), 256)?;
        for packet in &batch {
            if packet.opcode == world::opcode::server::ITEM_QUERY_SINGLE_RESPONSE {
                if let Ok(parsed) = world::query::parse_item_query_response(&packet.body) {
                    if parsed.entry == entry {
                        info = Some(parsed);
                    }
                }
            }
        }
        state.replicate(&batch, None);
    }
    let Some(info) = info else {
        println!("  the item query was never answered -- the session, not the use, is the problem");
        return Ok(());
    };
    println!(
        "  it is {:?}, spells {:?}",
        info.name.as_deref().unwrap_or("<unnamed>"),
        info.spells
            .iter()
            .filter(|s| s.id != 0)
            .collect::<Vec<_>>()
    );
    let Some(spell_id) = info.use_spell() else {
        println!("  no on-use spell: this item does nothing when clicked, which is not a bug");
        return Ok(());
    };
    println!("  on-use spell {spell_id}");

    let before = world::inventory::carried(state, own_guid)
        .into_iter()
        .find(|c| c.item.guid == carried.item.guid)
        .map(|c| c.item.count);
    connection.use_item(bag, slot, carried.item.guid, spell_id, None)?;

    // Long enough for a cast to finish: a consumable is instant, but the
    // stack does not move until the spell lands.
    let mut seen: Vec<u16> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        let batch = connection.drain(std::time::Duration::from_millis(700), 256)?;
        for packet in &batch {
            if !seen.contains(&packet.opcode) {
                seen.push(packet.opcode);
            }
        }
        state.replicate(&batch, None);
    }

    let after = world::inventory::carried(state, own_guid)
        .into_iter()
        .find(|c| c.item.guid == carried.item.guid)
        .map(|c| c.item.count);
    println!("  stack {before:?} -> {after:?}");
    match (before, after) {
        (Some(before), Some(after)) if after < before => {
            println!("  USED: the stack dropped by {}", before - after)
        }
        (Some(_), None) => println!("  USED: the last one was consumed and the item is gone"),
        _ => println!(
            "  no change. The query above answered, so the session is fine and the \
             use is the problem -- opcode, body shape, or a refusal."
        ),
    }
    // Printed whatever the verdict: an opcode nobody decoded is the evidence
    // that separates "never understood" from "understood and declined", and
    // this project has twice found the answer in this list rather than in
    // the effect.
    println!(
        "  opcodes seen after the send: {}",
        seen.iter()
            .map(|op| format!("{} ({op:#06x})", world::opcode::describe(*op)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

/// Asks the server about item entries and checks what comes back.
///
/// **This exists to confirm a parse, not to fetch names.** `foss-wow#56`'s
/// response is some ninety fields with one variable-length block in the
/// middle, which is precisely the shape this project's notes call the most
/// expensive: every field the right width but one, and the packet still
/// parses. Three things are therefore reported rather than just the name:
///
/// - **The display id against `Item.dbc`.** The server never sends that
///   table, so agreement is evidence from outside the packet -- the same
///   check that confirmed `SMSG_LOOT_RESPONSE`'s entry/display pairing.
/// - **Whether the parse consumed the whole body.** A failure prints the
///   bytes rather than the length, because a shape that is refused and
///   discarded is a capture nobody can look at.
/// - **Entries the server refuses.** "No such item" is an answer with its
///   own encoding (the entry's top bit), and it has to be distinguishable
///   from silence.
fn survey_item_query(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    asked: &[u32],
    data: Option<&std::path::Path>,
    locale: &str,
) -> Result<()> {
    // With nothing named, ask about what the character is actually carrying.
    // A hand-picked entry exercises one shape; a real inventory brings
    // weapons, containers, stackable trade goods and quest items, which is
    // the population that can show a field being wrong.
    let mut entries: Vec<u32> = asked.to_vec();
    if entries.is_empty() {
        if state.get(own_guid).is_none() {
            println!("\n--item-query: our own player object is not in replicated state yet");
            return Ok(());
        }
        // Through the inventory accessors rather than by walking the slot
        // array here: it is an array of guid *pairs*, and reading each word
        // as a guid happens to work for the low guids a test character's
        // items have and silently stops working for anything else.
        let held = world::inventory::held(state, own_guid);
        let mut carried: Vec<u32> = held.iter().filter_map(|item| item.entry).collect();
        // A bag's contents are the interesting half -- `Huntertest`'s quiver
        // carries shot, and a container's contents exercise a different
        // path from the squares that hold them.
        for bag in held.iter().filter(|item| item.capacity.is_some()) {
            carried.extend(
                world::inventory::bag_contents(state, *bag)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.entry),
            );
        }
        carried.sort_unstable();
        carried.dedup();
        entries = carried;
    }

    if entries.is_empty() {
        println!("\n--item-query: nothing to ask about -- name entries explicitly");
        return Ok(());
    }

    // `Item.dbc` is the independent half of the check. Without it the names
    // still print, and the tool says plainly that nothing confirmed them --
    // an instrument that quietly does less than its help text promises has
    // already cost this project an afternoon.
    let entry_to_display: std::collections::HashMap<u32, u32> = match data {
        Some(data) => {
            let mut chain = Chain::open_wow_data(data, locale)
                .with_context(|| format!("opening archives under {}", data.display()))?;
            dbc::schema::Item::parse(&chain.read(dbc::schema::Item::PATH)?)?
                .iter()
                .map(|row| (row.id(), row.display_info_id()))
                .collect()
        }
        None => {
            println!(
                "\n--item-query: no --data, so nothing checks the answers -- \
                 names will print unconfirmed"
            );
            std::collections::HashMap::new()
        }
    };

    println!("\nasking about {} item entr(y/ies)", entries.len());
    for entry in &entries {
        connection.ask_item(*entry, 0)?;
    }

    // A wall clock as well as a packet count. Northshire is never quiet --
    // it emits a monster move fourteen times a second -- so a drain bounded
    // only by a packet count runs until it has collected that many pieces of
    // background traffic, which is how a two-minute job became twenty.
    let mut answers: Vec<world::query::ItemInfo> = Vec::new();
    let mut failures: Vec<(u32, String, Vec<u8>)> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && answers.len() + failures.len() < entries.len() {
        let batch = connection.drain(std::time::Duration::from_millis(800), 256)?;
        if batch.is_empty() {
            continue;
        }
        for packet in &batch {
            if packet.opcode != world::opcode::server::ITEM_QUERY_SINGLE_RESPONSE {
                continue;
            }
            match world::query::parse_item_query_response(&packet.body) {
                Ok(info) => answers.push(info),
                Err(error) => {
                    // The body, not the length. A parser that declines a
                    // shape is only useful if the shape survives the
                    // refusal -- this project has lost the one packet that
                    // could have answered a question exactly this way.
                    let entry = packet
                        .body
                        .get(..4)
                        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .unwrap_or(0);
                    failures.push((entry, error.to_string(), packet.body.clone()));
                }
            }
        }
        state.replicate(&batch, None);
        println!(
            "  ... {} answered, {} failed, {} still out",
            answers.len(),
            failures.len(),
            entries.len().saturating_sub(answers.len() + failures.len())
        );
    }

    let mut agreed = 0usize;
    let mut disagreed = 0usize;
    let mut uncheckable = 0usize;
    println!("\nanswers:");
    for info in &answers {
        match &info.name {
            None => println!("  {:>6}: server has no such item", info.entry),
            Some(name) => {
                let check = match entry_to_display.get(&info.entry) {
                    Some(&dbc) if dbc == info.display_id => {
                        agreed += 1;
                        format!("display {} == Item.dbc", info.display_id)
                    }
                    Some(&dbc) => {
                        disagreed += 1;
                        format!(
                            "display {} != Item.dbc's {dbc}  <-- THE PARSE IS WRONG",
                            info.display_id
                        )
                    }
                    None => {
                        uncheckable += 1;
                        format!("display {} (no Item.dbc row to check)", info.display_id)
                    }
                };
                println!(
                    "  {:>6}: {name:<40} q{} ilvl{} req{} stack {} {check}",
                    info.entry, info.quality, info.item_level, info.required_level, info.stackable
                );
                if !info.description.is_empty() {
                    println!("          \"{}\"", info.description);
                }
            }
        }
    }

    for (entry, error, body) in &failures {
        println!("\n  entry {entry}: PARSE FAILED -- {error}");
        println!("    {} bytes: {}", body.len(), hex_preview(body, 512));
    }

    // **Parsing an answer and *storing* it are different claims.** The loop
    // above parses each body directly, which says nothing about whether
    // `WorldState::replicate`'s dispatch arm exists or reaches the cache --
    // and a parser wired to nothing is exactly the failure this project has
    // hit before (a value stored into a field nothing consulted). So read
    // the answers back out of the cache the interface will read.
    let cached = entries
        .iter()
        .filter(|entry| state.names.item(**entry).is_some())
        .count();
    let named = entries
        .iter()
        .filter(|entry| {
            state
                .names
                .item(**entry)
                .flatten()
                .is_some_and(|info| info.name.is_some())
        })
        .count();
    println!("cache: {cached} of {} settled, {named} with a name", entries.len());
    if cached < answers.len() {
        println!(
            "  <-- answers parsed here that the cache does not hold: the \
             dispatch arm in WorldState::replicate is not reaching it"
        );
    }

    let silent = entries
        .len()
        .saturating_sub(answers.len() + failures.len());
    println!(
        "\n{} asked, {} answered, {} parse failures, {} never answered",
        entries.len(),
        answers.len(),
        failures.len(),
        silent
    );
    println!(
        "cross-check: {agreed} agreed with Item.dbc, {disagreed} disagreed, \
         {uncheckable} had no row to check against"
    );
    if disagreed > 0 {
        println!(
            "A disagreement is the useful result: the packet parsed and the \
             display id is wrong, which means a field above it has the wrong \
             width. Nothing else in this run can tell you that."
        );
    } else if agreed > 0 {
        println!(
            "Every checkable answer agrees with a table the server never sent, \
             which is what makes the ninety fields above the display id \
             trustworthy rather than merely well-formed."
        );
    }
    Ok(())
}

fn report_visible_items(
    state: &world::WorldState,
    character: &world::Character,
    data: Option<&std::path::Path>,
    locale: &str,
) -> Result<()> {
    let Some(data) = data else {
        println!(
            "\n--visible-items needs game files: pass --data or set WOW_DATA \
             (Item.dbc is what turns an item entry into a display id)"
        );
        return Ok(());
    };

    let mut chain = Chain::open_wow_data(data, locale)
        .with_context(|| format!("opening archives under {}", data.display()))?;
    let item_rows = dbc::schema::Item::parse(&chain.read(dbc::schema::Item::PATH)?)?;
    let entry_to_display: std::collections::HashMap<u32, u32> = item_rows
        .iter()
        .map(|row| (row.id(), row.display_info_id()))
        .collect();

    let worn: Vec<(usize, u32)> = character
        .equipment
        .iter()
        .take(world::inventory::EQUIPPED_COUNT as usize)
        .enumerate()
        .filter(|(_, slot)| slot.display_id != 0)
        .map(|(index, slot)| (index, slot.display_id))
        .collect();

    println!("\nworn items, as the character list reports them:");
    for (index, display_id) in &worn {
        println!("  slot {index}: display {display_id}");
    }

    let Some(own) = state.get(character.guid) else {
        println!("  our own player object is not in replicated state; nothing to search");
        return Ok(());
    };
    println!(
        "  searching {} set fields, read as item entries through Item.dbc \
         ({} entries), against {} worn display ids",
        own.fields.len(),
        entry_to_display.len(),
        worn.len(),
    );

    // Exactly one match per slot is what makes the base/stride fit below
    // trustworthy; an ambiguous slot is reported but excluded from it, the
    // same way `PLAYER_BYTES`' search treated more than one hit as "the
    // question needs narrowing" rather than picking one arbitrarily.
    let mut unambiguous: Vec<(usize, u16)> = Vec::new();
    for (index, display_id) in &worn {
        let fields: Vec<u16> = own
            .fields
            .iter()
            .filter(|(_, value)| entry_to_display.get(value) == Some(display_id))
            .map(|(field, _)| field)
            .collect();
        match fields.as_slice() {
            [] => println!("  slot {index} (display {display_id}): no field resolves to it"),
            [one] => {
                println!("  slot {index} (display {display_id}): field {one:#06x}");
                unambiguous.push((*index, *one));
            }
            many => println!(
                "  slot {index} (display {display_id}): {} fields resolve to it ({many:#06x?}) -- ambiguous",
                many.len()
            ),
        }
    }

    // Look for one `base + slot * stride` that explains every unambiguous
    // pair, trying each pair as the anchor since worn slots are rarely
    // contiguous (an empty slot leaves no field to anchor from).
    let fit = unambiguous.iter().enumerate().find_map(|(i, &(s0, f0))| {
        unambiguous[i + 1..].iter().find_map(|&(s1, f1)| {
            if s1 == s0 {
                return None;
            }
            let (delta_field, delta_slot) = (f1 as i64 - f0 as i64, s1 as i64 - s0 as i64);
            if delta_field % delta_slot != 0 {
                return None;
            }
            let stride = delta_field / delta_slot;
            let base = f0 as i64 - s0 as i64 * stride;
            unambiguous
                .iter()
                .all(|&(s, f)| base + s as i64 * stride == f as i64)
                .then_some((base, stride))
        })
    });
    match fit {
        Some((base, stride)) if unambiguous.len() >= 2 => println!(
            "\n  fits base {base:#06x} stride {stride} across {} unambiguous slot(s)",
            unambiguous.len()
        ),
        _ => println!(
            "\n  no single base/stride explains the {} unambiguous slot(s) found \
             -- more worn slots, or a character with fewer collisions, would help",
            unambiguous.len()
        ),
    }

    Ok(())
}

/// Prints the spellbook exactly as it arrived.
///
/// Ids only: turning one into a name needs `Spell.dbc`, and this command
/// deliberately works without a game installation -- the protocol tools are
/// useful on a machine that has no client data, and requiring `--data` to see
/// whether a packet parsed would be the wrong trade. `wow-cli dbc rows Spell`
/// resolves them when the data is there.
fn report_spells(state: &world::WorldState) {
    let book = &state.spells;
    println!(
        "
spellbook: {} spell(s), {} cooldown(s)",
        book.spells.len(),
        book.cooldowns.len()
    );
    if book.spells.is_empty() {
        println!("  none -- SMSG_INITIAL_SPELLS arrives in the login burst and is never resent,");
        println!("  so an empty book here means the packet was missed, not that nothing is known");
        return;
    }
    let ids: Vec<String> = book.spells.iter().map(|s| s.id.to_string()).collect();
    for chunk in ids.chunks(12) {
        println!("  {}", chunk.join(" "));
    }
    for cooldown in &book.cooldowns {
        // `second` rather than a named field: this list's entry width is
        // confirmed against a live realm, but what its second word actually
        // measures is not -- see `SpellCooldown`'s doc comment.
        println!("  cooldown: spell {}, second word {}", cooldown.spell_id, cooldown.second);
    }
}

/// Writes every packet received to a file, for reading a shape offline.
///
/// One line per packet: the opcode in hex, its length, then the body. Plain
/// text on purpose -- the point is to be greppable by opcode and readable by
/// eye, and a capture that needs its own decoder to inspect would just move
/// the problem. Nothing here interprets anything, which is what makes it
/// usable on the packets that have no parser yet.
struct Capture {
    file: std::io::BufWriter<std::fs::File>,
    written: usize,
}

impl Capture {
    fn create(path: &std::path::Path) -> Result<Self> {
        let file = std::fs::File::create(path)
            .with_context(|| format!("creating capture file {}", path.display()))?;
        Ok(Self {
            file: std::io::BufWriter::new(file),
            written: 0,
        })
    }

    fn record(&mut self, packets: &[world::client::Packet]) -> Result<()> {
        use std::io::Write;
        for packet in packets {
            let hex: Vec<String> = packet.body.iter().map(|b| format!("{b:02x}")).collect();
            writeln!(
                self.file,
                "{:#06x} {:<40} {:>5} {}",
                packet.opcode,
                world::opcode::describe(packet.opcode),
                packet.body.len(),
                hex.join(" ")
            )?;
            self.written += 1;
        }
        Ok(())
    }

    fn finish(mut self, path: &std::path::Path) -> Result<()> {
        use std::io::Write;
        self.file.flush()?;
        println!("\ncaptured {} packets to {}", self.written, path.display());
        Ok(())
    }
}

/// The first `limit` bytes of a body, for eyeballing a layout that would not
/// parse.
fn hex_preview(body: &[u8], limit: usize) -> String {
    let shown: String = body
        .iter()
        .take(limit)
        .map(|b| format!("{b:02x} "))
        .collect();
    if body.len() > limit {
        format!("{}...", shown.trim_end())
    } else {
        shown.trim_end().to_string()
    }
}

/// What to call a unit in the table: its resolved name, or its guid.
///
/// Falling back to the guid rather than to a blank keeps the two states
/// distinguishable at a glance -- a column of guids means names were never
/// asked for, a column of names with one guid in it means one query went
/// unanswered, and those want different investigations.
fn unit_label(state: &world::WorldState, entity: &world::state::Entity) -> String {
    let resolved = if entity.is_player() {
        state.names.player(entity.guid).flatten()
    } else {
        entity
            .fields
            .get(world::update::fields::OBJECT_ENTRY)
            .and_then(|entry| state.names.creature(entry).flatten())
    };
    match resolved {
        Some(name) => name.to_string(),
        None => format!("{:#x}", entity.guid),
    }
}

/// Names the operation a `SMSG_PARTY_COMMAND_RESULT` is answering.
///
/// Partial on purpose, like everything else here that turns a number into a
/// sentence: only the operations this tool actually sends are named, and
/// anything else comes back as its number rather than as a plausible guess.
fn describe_party_operation(operation: u32) -> String {
    match operation {
        world::PartyOperation::INVITE => "invite".to_string(),
        world::PartyOperation::UNINVITE => "kick".to_string(),
        world::PartyOperation::LEAVE => "leave".to_string(),
        other => format!("party operation {other}"),
    }
}

/// Prints the group as it currently stands, with whatever is known about each
/// member and *where that knowledge came from*.
///
/// The source is printed because it is the thing under test. A party member in
/// view is a fully replicated player and every number is exact; a member out
/// of view exists only as whatever `SMSG_PARTY_MEMBER_STATS` last said. Those
/// two look identical on a frame and want completely different investigations
/// when one of them is wrong.
fn report_party(state: &world::WorldState) {
    let Some(party) = state.party.as_ref() else {
        println!("  party: not in a group");
        return;
    };
    println!(
        "  party: group {:#018x} (type {:#04x}, update #{}), {} other member(s), leader {:#018x}",
        party.guid,
        party.group_type,
        party.counter,
        party.members.len(),
        party.leader
    );
    for member in &party.members {
        let vitals = state.party_member_vitals(member.guid);
        let health = match (vitals.health, vitals.max_health) {
            (Some(now), Some(max)) => format!("{now}/{max}"),
            (Some(now), None) => format!("{now}/?"),
            _ => "?/?".to_string(),
        };
        // Printed with its *type*, because the pair is meaningless without
        // it: `0/1000` is a full rage bar's worth of nothing for a warrior
        // and an empty mana pool for a mage, and the number that separates
        // them is the one a client defaulting to zero would have invented.
        // A `?` here is a real state -- a member out of view whose stats
        // packet has not arrived has no power fields at all.
        let power = match (vitals.power, vitals.max_power, vitals.power_type) {
            (Some(now), Some(max), Some(kind)) => format!("{now}/{max} type {kind}"),
            (Some(now), Some(max), None) => format!("{now}/{max} type ?"),
            _ => "?/?".to_string(),
        };
        println!(
            "    {:<14} {:#018x}  status {:#04x}{}{}  hp {health}  power {power}  level {}  zone {}  [{}]",
            member.name,
            member.guid,
            member.status,
            if member.is_online() { " online" } else { " OFFLINE" },
            if member.is_dead() { " dead" } else { "" },
            vitals
                .level
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".to_string()),
            vitals
                .zone
                .map(|z| z.to_string())
                .unwrap_or_else(|| "?".to_string()),
            if vitals.in_view {
                "replicated"
            } else {
                "party packets only"
            }
        );
        if party.is_leader(member.guid) {
            println!("      (leads the group)");
        }
    }
    if let Some(loot) = party.loot {
        println!(
            "    loot: method {}, threshold {}, master {:#018x}, difficulty {}/{}",
            loot.method, loot.threshold, loot.master, loot.dungeon_difficulty, loot.raid_difficulty
        );
    }
}

/// Prints the raw bytes of anything party-shaped in a batch.
///
/// **Bodies, not lengths.** Every layout in `world::group` was measured from a
/// live capture rather than transcribed, and the moment this stops dumping
/// them is the moment the next unrecognised shape becomes invisible -- which
/// is exactly how a 46-byte swing was seen and lost once already.
fn dump_party_packets(batch: &[world::client::Packet]) {
    use world::opcode::server;
    const PARTY_OPCODES: [u16; 9] = [
        server::GROUP_INVITE,
        server::GROUP_LIST,
        server::PARTY_COMMAND_RESULT,
        server::PARTY_MEMBER_STATS,
        server::PARTY_MEMBER_STATS_FULL,
        server::GROUP_UNINVITE,
        server::GROUP_DESTROYED,
        server::GROUP_CANCEL,
        server::GROUP_DECLINE,
    ];
    for packet in batch {
        if !PARTY_OPCODES.contains(&packet.opcode) {
            continue;
        }
        println!(
            "  raw {} ({:#06x}), {} bytes: {}",
            world::opcode::describe(packet.opcode),
            packet.opcode,
            packet.body.len(),
            hex_preview(&packet.body, 256)
        );
    }
}

/// Asks a player by name to join the group, and reports what came back.
///
/// **The one group request that is answered**, and therefore the one worth
/// building a probe around. `SMSG_PARTY_COMMAND_RESULT` arrives whether the
/// invite worked or not, which is what separates the three failures that
/// otherwise look identical: an opcode the server did not understand, a body
/// it could not read, and a request it understood and declined. A misspelt
/// name is a complete test on its own -- the reply echoes the string back, so
/// getting it at all proves the server read it from the offset written here.
fn invite_to_party(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    name: &str,
) -> Result<()> {
    println!("\ninviting {name:?} to the group");
    connection.group_invite(name)?;
    // Give the server time to answer before concluding it ignored us. A
    // packet sent immediately before a disconnect is often never processed,
    // and that has been mistaken for a wrong opcode in this tool before.
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    dump_party_packets(&batch);
    let report = state.replicate(&batch, None);
    print_events(&report, state, 0);
    if report.party_results.is_empty() {
        println!("  nothing answered. Three things look exactly like this and");
        println!("  want opposite investigations: the opcode was not understood,");
        println!("  the body was read at the wrong offsets, or the server is");
        println!("  taking longer than the drain waited. The first two are");
        println!("  separable by inviting a name that certainly does not exist --");
        println!("  a refusal is still an answer.");
    }
    for failure in &report.failures {
        println!("  undecodable {}: {}", world::opcode::describe(failure.0), failure.1);
    }
    Ok(())
}

/// Prints everything a fold *returns* rather than stores: chat and swings.
///
/// One function because there are now three places that drain packets, and
/// the standing hazard in this crate is a caller that folds a batch and drops
/// the events it hands back. The count that made this worth extracting was a
/// summary reading "0 attacks started, 2 stopped" -- impossible, and caused by
/// the approach loop folding the starts and discarding the report.
fn print_events(report: &world::Replication, state: &world::WorldState, own_guid: u64) {
    // First, because it is the only answer any group request gets. Every
    // outgoing party message but the invite is silent, so a party result
    // dropped here puts this tool straight back into the failure mode the
    // whole block was built to escape.
    for result in &report.party_results {
        println!(
            "  party: {} -> {}{}",
            describe_party_operation(result.operation),
            world::group::describe_party_result(result.result),
            if result.member.is_empty() {
                String::new()
            } else {
                format!(" (about {:?})", result.member)
            }
        );
    }
    if report.party_lists > 0 {
        report_party(state);
    }
    for message in &report.chat {
        println!("  {}", describe_chat(message, state));
    }
    for swing in &report.swings {
        println!(
            "  {}",
            world::combat::describe_swing(swing, own_guid, |guid| combat_name(state, guid))
        );
        // **The raw mask beside the sentence.** `describe_swing` deliberately
        // names only the bits this project has confirmed, so a run that is
        // *investigating* a bit -- which is how every named one got named --
        // has nothing to read otherwise. This is what identified the off-hand
        // bit: two runs of the same fight, one weapon and then two, with only
        // this line differing between them.
        println!(
            "    hit_info {:#010x}, damage {}, victim_state {}{}",
            swing.hit_info,
            swing.damage,
            swing.victim_state,
            match swing.extra_amount {
                Some(extra) => format!(", extra {extra}"),
                None => String::new(),
            }
        );
    }
    // Threat is returned rather than stored -- nothing in the interface reads
    // a threat table yet -- so printing it here is the only thing keeping the
    // parse honest. A category nobody consumes is a category nobody notices
    // has stopped working.
    for threat in &report.threat {
        let entries: Vec<String> = threat
            .entries
            .iter()
            .map(|(who, value)| format!("{} {value}", combat_name(state, *who)))
            .collect();
        println!(
            "  threat on {}: {}",
            combat_name(state, threat.victim),
            entries.join(", ")
        );
    }
}

/// What to call a guid in a combat line, named as well as the cache allows.
///
/// Falls back to the guid rather than to a blank, for the same reason
/// [`unit_label`] does: a line reading "Unit 0xf13... hits you" still says what
/// happened, and a line with a hole in it does not.
fn combat_name(state: &world::WorldState, guid: u64) -> String {
    match state.get(guid) {
        Some(entity) => unit_label(state, entity),
        None => format!("{guid:#x}"),
    }
}

/// One line of chat, named as well as the name cache currently allows.
fn describe_chat(message: &world::ChatMessage, state: &world::WorldState) -> String {
    let who = speaker(message, state);
    match message.chat_type {
        world::ChatType::Channel => format!(
            "[{}] {who}: {}",
            message.channel.as_deref().unwrap_or("?"),
            message.text
        ),
        world::ChatType::Emote | world::ChatType::TextEmote | world::ChatType::MonsterEmote => {
            format!("* {who} {}", message.text)
        }
        world::ChatType::System => format!("[system] {}", message.text),
        other => format!("[{}] {who}: {}", other.label(), message.text),
    }
}

/// Who said it: the name carried inline if there was one, then the name cache,
/// then the guid.
///
/// The three-way fallback is the honest picture. A creature names itself in the
/// packet; a player does not, and is anonymous until a query comes back.
fn speaker(message: &world::ChatMessage, state: &world::WorldState) -> String {
    if let Some(name) = &message.sender_name {
        return name.clone();
    }
    if let Some(Some(name)) = state.names.player(message.sender) {
        return name.to_string();
    }
    if message.sender == 0 {
        return "server".into();
    }
    format!("{:#x}", message.sender)
}

/// Asks for every name the replicated world needs, and waits for the answers.
///
/// Two rounds rather than one: the first round's queries are answered while
/// the second round is being sent, and a single drain routinely returns before
/// the last few arrive. Anything still missing after that is reported as
/// missing rather than retried forever -- some guids are simply never
/// answered, and this is a dump command, not the client.
fn resolve_names(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    rounds: usize,
) -> Result<()> {
    use world::update::fields;

    for _ in 0..rounds {
        let now = std::time::Instant::now();
        let mut wanted: Vec<(bool, u64, u32)> = Vec::new();
        for entity in state.iter() {
            let entry = entity.fields.get(fields::OBJECT_ENTRY).unwrap_or(0);
            wanted.push((entity.is_player(), entity.guid, entry));
        }

        let mut asked = 0usize;
        for (is_player, guid, entry) in wanted {
            if is_player {
                if state.names.claim_player(guid, now) {
                    connection.ask_player_name(guid)?;
                    asked += 1;
                }
            } else if entry != 0 && state.names.claim_creature(entry, now) {
                connection.ask_creature_name(entry, guid)?;
                asked += 1;
            }
        }
        if asked == 0 {
            break;
        }
        println!("  asked for {asked} name(s)");

        // Give the answers time to arrive. A query sent and immediately
        // followed by a read that gives up is indistinguishable from one the
        // server ignored -- the same trap that briefly condemned a facing
        // opcode.
        let batch = connection.drain(std::time::Duration::from_millis(900), 512)?;
        let report = state.replicate(&batch, None);
        for (opcode, error, _) in &report.failures {
            println!("  undecodable {}: {error}", world::opcode::describe(*opcode));
        }
        // Chat arrives on the same stream as everything else, so a drain done
        // for another reason still collects it -- and dropping it here loses
        // it for good. That is not hypothetical: a two-client test looked like
        // chat never being delivered, when the other client's line had landed
        // in this drain and been discarded. `replicate` has one dispatch table
        // by design; its *callers* are where a category goes quietly missing.
        for message in &report.chat {
            println!("  {}", describe_chat(message, state));
        }
    }

    let (known, pending) = state.names.counts();
    let stats = state.names.stats;
    println!(
        "  {known} name(s) resolved, {pending} unanswered ({} queries, {} answers, {} unsolicited)",
        stats.queries_issued, stats.answers, stats.unsolicited
    );
    Ok(())
}

/// The closest replicated unit to the player, if there is one.
fn nearest_unit(state: &world::WorldState, own_guid: u64) -> Option<(u64, f32)> {
    let own = state.get(own_guid)?.position?;
    nearest_unit_from(state, own_guid, own)
}

/// The nearest unit to a *given* point rather than to the replicated one.
///
/// Needed because the server never relays this client's own movement back to
/// it: walk twenty units and `state`'s idea of where we are is still the login
/// position. Measuring from it after a walk reports distances to somewhere we
/// left, which showed up as an attack command that closed the right number of
/// units and still arrived out of range.
/// Whether this entity is something a test run may act on.
///
/// **Other players are excluded, and this cost a real incident to learn.** The
/// name filters here are substring matches, and `--target Wolf` matched
/// `Testwolf` -- a character belonging to the person running the test, logged
/// in on the other account at the time. The run then selected that player and
/// the `.die` sent behind it killed them. Nothing malfunctioned: the selection
/// registered correctly, on exactly what was asked for.
///
/// The documented trap next door is that `.die` falls back to *self* when a
/// selection has not registered. This is its mirror and is worse, because it
/// looks like it worked. A substring of a creature's name is very often a
/// substring of somebody's character name -- that is how players name
/// characters -- so the filter cannot be made safe by choosing better words.
///
/// Excluding players is also just correct for what these helpers are for:
/// every one of them exists to find something to walk to, swing at, or loot,
/// and none of those should ever land on another person's character.
fn is_a_legal_test_target(entity: &world::state::Entity) -> bool {
    !matches!(entity.object_type, world::ObjectType::Player)
}

fn nearest_unit_from(
    state: &world::WorldState,
    own_guid: u64,
    own: world::Position,
) -> Option<(u64, f32)> {
    state
        .iter()
        .filter(|entity| entity.guid != own_guid)
        .filter(|entity| is_a_legal_test_target(entity))
        .filter_map(|entity| {
            let at = entity.position?;
            let distance = ((at.x - own.x).powi(2) + (at.y - own.y).powi(2) + (at.z - own.z).powi(2))
                .sqrt();
            Some((entity.guid, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Prints what a unit frame would show, for the player and its neighbours.
///
/// This is the dump command the interface's fields should have had before they
/// were wired into a renderer -- `CLAUDE.md`'s rule, skipped once and repaid
/// here. It exists because every value below is one where a wrong field index
/// parses perfectly: a warrior reported with mana instead of rage, or a level
/// read out of the power array, looks like data rather than like a bug.
fn report_units(state: &world::WorldState, own_guid: u64, limit: usize) {
    println!("\nunit fields, as a unit frame would read them:");

    let mut rows: Vec<(f32, &world::state::Entity)> = Vec::new();
    if let Some(own) = state.get(own_guid) {
        rows.push((0.0, own));
    }
    let mut others: Vec<(f32, &world::state::Entity)> = nearest_ordered(state, own_guid);
    others.truncate(limit);
    rows.append(&mut others);

    println!(
        "  {:<22} {:<9} {:>5}  {:>13}  {:>13}  {}",
        "name", "kind", "level", "health", "power", "race/class/gender/power"
    );
    for (distance, entity) in rows {
        let bytes = entity
            .bytes_0()
            .map(|b| format!("{} / {} / {} / {}", b[0], b[1], b[2], b[3]))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:<22} {:<9} {:>5}  {:>6}/{:<6}  {:>6}/{:<6}  {bytes}{}",
            unit_label(state, entity),
            entity.object_type.name(),
            entity
                .level()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "-".into()),
            entity.health().unwrap_or(0),
            entity.max_health().unwrap_or(0),
            entity.power().unwrap_or(0),
            entity.max_power().unwrap_or(0),
            if distance > 0.0 {
                format!("   {distance:.0}u away")
            } else {
                "   (you)".into()
            },
        );
    }
    println!(
        "  power type: 0 mana, 1 rage, 2 focus, 3 energy, 6 runic power.\n  \
         A warrior reading anything but rage means the power array was indexed wrong."
    );
}

/// Prints every replicated game object, with its position and its set fields.
///
/// Deliberately raw. This command works without a game installation -- the
/// protocol tools are useful on a machine that has no client data -- so it
/// cannot resolve a display id against `GameObjectDisplayInfo` itself. What it
/// can do is lay out the evidence: thirty-odd objects, each with a handful of
/// fields, where exactly one field index will resolve for *all* of them.
fn report_game_objects(state: &world::WorldState) {
    // Corpses as well as game objects, and labelled, because they are the same
    // question asked twice: an object in the world that is not a unit, whose
    // fields nothing here reads yet. A corpse is what a graveyard run runs
    // back to, so leaving it out of the one command that dumps world objects
    // meant the object at the centre of the death flow was the only one that
    // could not be looked at.
    let objects: Vec<&world::state::Entity> = state
        .iter()
        .filter(|entity| {
            matches!(
                entity.object_type,
                world::ObjectType::GameObject | world::ObjectType::Corpse
            )
        })
        .collect();
    println!("
{} game object(s) and corpse(s):", objects.len());
    for entity in &objects {
        let position = entity
            .position
            .map(|p| format!("{:.1}, {:.1}, {:.1} facing {:.2}", p.x, p.y, p.z, p.orientation))
            .unwrap_or_else(|| "no position".into());
        println!("  {:#x}  {:?}  {position}", entity.guid, entity.object_type);
        let fields: Vec<String> = entity
            .fields
            .iter()
            .map(|(index, value)| format!("{index:#x}={value}"))
            .collect();
        println!("    {}", fields.join(" "));
    }
    if objects.is_empty() {
        println!("  none in range -- stand somewhere with a door, a chest or a corpse");
    }
}

/// Dumps every replicated item beside the character's own slot array.
///
/// **The point is the pairing.** Either half alone is unfalsifiable: a list of
/// item objects says what is held but not where, and a column of slot guids has
/// nothing to resolve against. Printed together, the slot array's identified
/// base *predicts* which guids appear and at which indices, and a wrong base
/// shows up immediately as guids that belong to no object -- where a wrong base
/// read on its own would return plausible-looking numbers made of two halves of
/// neighbouring guids.
///
/// It is also the instrument for the fields still unknown. `ITEM_FIELD_STACK_COUNT`
/// has not been located: the technique is to add a stack of 3 and a stack of 5
/// and diff two runs' item fields, keeping the field that reads 3 in one and 5
/// in the other. A field holding 3 in both is a coincidence of magnitude, which
/// is the trap this project's notes call "validity is nearly free; variation is
/// the discriminator". So every field of every item is printed raw, with no
/// interpretation applied whatsoever.
/// Dumps every replicated unit's fields, for identifying the ones nothing
/// reads yet.
///
/// **The point is that a candidate field can be checked against an answer from
/// outside the packet.** `creature_template.npcflag` in the server's own
/// database says which creatures are questgivers, vendors and innkeepers, and
/// it is not something the client is told directly -- so a field whose value
/// equals that number, for several creatures with several different values, is
/// identified rather than guessed. Innkeeper Farley is 66179 and a wolf is 0.
fn report_unit_fields(state: &world::WorldState, own_guid: u64) {
    let units: Vec<&world::state::Entity> = state
        .iter()
        .filter(|entity| entity.guid != own_guid)
        .filter(|entity| entity.object_type == world::ObjectType::Unit)
        .collect();

    println!("
{} replicated unit(s):", units.len());
    for entity in &units {
        let entry = entity
            .fields
            .get(world::update::fields::OBJECT_ENTRY)
            .unwrap_or(0);
        // The entry, not the name: it is what `creature_template` is keyed
        // by, and cross-referencing against that table is the whole point.
        println!("
  {:#018x}  entry {entry}", entity.guid);
        let fields: Vec<String> = entity
            .fields
            .iter()
            .map(|(index, value)| format!("{index:#x}={value}"))
            .collect();
        println!("    {}", fields.join(" "));
    }
}

/// Says what state every replicated unit is in, ours first.
///
/// **A report, not a dump**, and the split is the point: `--own-fields` prints
/// the numbers these readings are made from, and this prints the readings. The
/// two catch different things. A wrong offset shows up in the dump; a correct
/// offset nothing acts on shows up only here.
///
/// One line per unit whatever it says, and a count of how many said anything.
/// A survey that printed only the stealthed units could not tell "nobody is
/// hiding" from "the creep bit is never read", which are the two answers this
/// exists to separate.
fn report_states(state: &world::WorldState, own_guid: u64) {
    // Named here rather than in `world::state`, where only the two forms this
    // project has actually watched arrive are named. A probe may say "form 5"
    // and let the reader look it up; a client may not invent a name.
    let describe = |entity: &world::state::Entity| -> String {
        let mut parts = Vec::new();
        if entity.stealthed() {
            parts.push("stealthed".to_string());
        }
        if entity.transformed() {
            parts.push(format!(
                "transformed (wearing {}, native {})",
                entity.display_id().unwrap_or(0),
                entity.native_display_id().unwrap_or(0),
            ));
        }
        match entity.shapeshift_form() {
            Some(0) | None => {}
            Some(form) => parts.push(format!("form {form}")),
        }
        if parts.is_empty() {
            "-".to_string()
        } else {
            parts.join(", ")
        }
    };

    println!("\nunit states:");
    match state.get(own_guid) {
        Some(own) => println!(
            "  own {own_guid:#x}  display {}  native {}  {}",
            own.display_id().map_or("?".into(), |id| id.to_string()),
            own.native_display_id().map_or("?".into(), |id| id.to_string()),
            describe(own),
        ),
        None => println!("  own object not replicated"),
    }

    let others: Vec<&world::state::Entity> = state
        .iter()
        .filter(|entity| entity.guid != own_guid)
        .filter(|entity| {
            matches!(
                entity.object_type,
                world::ObjectType::Unit | world::ObjectType::Player
            )
        })
        .collect();
    let interesting = others
        .iter()
        .filter(|entity| entity.stealthed() || entity.transformed())
        .count();
    println!(
        "  {interesting} of {} other unit(s) are in some state",
        others.len()
    );
    for entity in &others {
        println!(
            "    {:#018x}  {:<6}  entry {:<6} display {:<6} {}",
            entity.guid,
            match entity.object_type {
                world::ObjectType::Player => "player",
                _ => "unit",
            },
            entity.entry().unwrap_or(0),
            entity.display_id().unwrap_or(0),
            describe(entity),
        );
    }
}

fn report_items(state: &world::WorldState, own_guid: u64) {
    use world::inventory::{self, SlotKind};

    let held = inventory::held(state, own_guid);
    let copper = inventory::coinage(state, own_guid);
    let (gold, silver, copper_only) = inventory::purse(copper);

    println!(
        "\ninventory slot array (base {:#06x}), {} of {} slots occupied:",
        world::update::fields::PLAYER_FIELD_INV_SLOT_HEAD,
        held.len(),
        inventory::SLOT_COUNT,
    );
    for item in &held {
        let region = match item.slot.kind() {
            SlotKind::Equipped => "equipped",
            SlotKind::Bag => "bag",
            SlotKind::Backpack => "backpack",
        };
        let label = item.slot.label().unwrap_or("-");
        // An occupied slot whose object never arrived is called out rather
        // than printed as a blank: it is a real and different state, and
        // silently showing nothing would make a replication gap look like an
        // empty bag.
        let entry = match item.entry {
            Some(entry) => format!("entry {entry}"),
            None => "OBJECT NOT REPLICATED".into(),
        };
        println!(
            "  slot {:>2} ({region:>8}, {label:>9}) field {:#06x}  guid {:#018x}  {entry}",
            item.slot.index(),
            item.slot.field(),
            item.guid,
        );
    }
    if held.is_empty() {
        println!("  none -- the slot array read empty, which for a character");
        println!("  wearing anything at all means the base is wrong");
    }

    println!("\nmoney: {copper} copper ({gold}g {silver}s {copper_only}c)");

    // Every item object, including any the slot array does not mention -- a
    // bag's *contents* may well arrive as objects the player's own array never
    // names, and that difference is exactly what has to be visible to work out
    // how containers address what is inside them.
    let items: Vec<&world::state::Entity> = state
        .iter()
        .filter(|entity| {
            matches!(
                entity.object_type,
                world::ObjectType::Item | world::ObjectType::Container
            )
        })
        .collect();
    let in_slots: std::collections::BTreeSet<u64> = held.iter().map(|item| item.guid).collect();

    println!("\n{} replicated item object(s):", items.len());
    for entity in &items {
        let slot = held.iter().find(|item| item.guid == entity.guid);
        let placement = match slot {
            Some(item) => format!("slot {}", item.slot.index()),
            // The interesting case. Something is holding this and it is not
            // one of the thirty-nine slots on the player.
            None => "NOT IN THE PLAYER'S SLOT ARRAY".into(),
        };
        println!(
            "  {:#018x}  {:?}  {placement}",
            entity.guid, entity.object_type
        );
        for (index, value) in entity.fields.iter() {
            println!("    {index:#06x} = {value:#010x} ({value})");
        }
    }

    let missing = in_slots.len() - items.iter().filter(|e| in_slots.contains(&e.guid)).count();
    if missing > 0 {
        println!("\n{missing} slot(s) name a guid with no object -- see above");
    }
    if items.is_empty() {
        println!("  none -- items are replicated in the login burst, so an");
        println!("  empty list here means the burst was cut short, not that");
        println!("  the character is carrying nothing");
    }

    // The unknowns, restated at the point of use so a run of this command is
    // self-describing. Nothing below is implemented and nothing should be
    // guessed from a table.
    println!("\nstill unidentified, and to be measured rather than transcribed:");
    println!("  ITEM_FIELD_STACK_COUNT -- diff a stack of 3 against a stack of 5");
    println!("  how a container addresses its contents -- see any item above");
    println!("  marked NOT IN THE PLAYER'S SLOT ARRAY");
}

/// Opens the loot on the nearest dead unit and reports everything that arrives.
///
/// **This is a survey and deliberately parses nothing.** Nothing in
/// `crates/world` understands a loot packet yet, and the way that stops being
/// true is by looking at real ones -- so every reply is printed as its opcode
/// and its bytes, in full. Printing a length instead is how this project once
/// saw and lost the single packet that could have settled
/// `SMSG_ATTACKERSTATEUPDATE`.
///
/// The confirmation this run provides is stronger than the equip write's,
/// because loot is *answered*. A reply arriving at all says the opcode was
/// understood; the equip write had to be confirmed by watching a field move,
/// since nothing acknowledged it.
fn survey_loot(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    // Where the character has actually walked to. **Not** read out of
    // replicated state, which holds the login position forever -- see the
    // declaration of `here` in `world_login`. Taking it as a parameter rather
    // than looking it up is the point: a lookup is what got this wrong.
    here: world::Position,
) -> Result<()> {
    // A corpse to open. Deliberately "a unit at zero health" rather than a
    // `Corpse` object: a corpse object is what a *player* leaves behind after
    // releasing, and a creature you have just killed is still a unit. Looking
    // for the wrong one finds nothing in a field full of bodies.
    // Loot has a range, and a corpse across the field is not a corpse you can
    // open. The first run of this opened something 37 units away and came back
    // with no reply at all -- which reads as a wrong opcode and was nothing of
    // the kind. A distance check turns that into a refusal this tool can
    // explain rather than a silence it cannot.
    const LOOT_REACH: f32 = 5.0;

    let mut dead: Vec<(u64, f32)> = state
        .iter()
        .filter(|entity| entity.guid != own_guid)
        // See `is_a_legal_test_target`. A dead *player* is somebody's corpse.
        .filter(|entity| is_a_legal_test_target(entity))
        .filter(|entity| entity.health() == Some(0))
        .filter_map(|entity| {
            let there = entity.position?;
            let d = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
            Some((entity.guid, d))
        })
        .collect();
    dead.sort_by(|a, b| a.1.total_cmp(&b.1));

    let Some(&(corpse, distance)) = dead.first() else {
        println!("\nno dead creature anywhere in replicated state.");
        println!("Make one first: --select --target <name> --attack --say \".die\"");
        return Ok(());
    };
    if distance > LOOT_REACH {
        // Refused loudly rather than attempted, because the failure would be
        // silent and would look exactly like a wrong opcode.
        println!("\nnearest corpse {corpse:#018x} is {distance:.1} units away, past");
        println!("the {LOOT_REACH:.0}-unit reach. Not sending: a refusal at this range");
        println!("would be indistinguishable from an opcode the server ignored.");
        println!("Add --attack so the run closes to melee before killing.");
        return Ok(());
    }
    println!("\n{} dead creature(s); opening {corpse:#018x} at {distance:.1}", dead.len());

    connection.loot(corpse)?;
    // Give the server time to answer before concluding it ignored us.
    let batch = connection.drain(std::time::Duration::from_millis(1500), 128)?;

    let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
    for packet in &batch {
        *seen.entry(packet.opcode).or_default() += 1;
    }

    // The interesting packets are the ones this client has never seen. Movement
    // and time-sync arrive constantly and would bury the answer, so they are
    // summarised rather than dumped -- but nothing is *hidden*: the histogram
    // below lists everything, and only the bodies are filtered.
    const NOISE: [u16; 6] = [0x00DD, 0x00A9, 0x01F6, 0x0390, 0x0085, 0x0086];
    println!("\nevery packet that came back, routine traffic aside:");
    let mut shown = 0;
    for packet in &batch {
        if NOISE.contains(&packet.opcode) {
            continue;
        }
        println!(
            "  {} ({:#06x}), {} bytes",
            world::opcode::describe(packet.opcode),
            packet.opcode,
            packet.body.len()
        );
        println!("    {}", hex_preview(&packet.body, 128));
        shown += 1;
        if shown >= 24 {
            println!("  ... stopping after {shown}");
            break;
        }
    }
    if shown == 0 {
        println!("  none -- nothing but the usual traffic came back");
    }

    // Now that the layout is known, say what the corpse actually holds --
    // while still printing the raw bytes above, because the moment this stops
    // dumping bodies is the moment the next unknown shape becomes invisible.
    for packet in &batch {
        if packet.opcode != world::opcode::server::LOOT_RESPONSE {
            continue;
        }
        match world::loot::parse_loot_response(&packet.body) {
            Ok(loot) if loot.error.is_some() => {
                println!("\nthe corpse is empty (short form, status {:?})", loot.error);
            }
            Ok(loot) => {
                println!("\n{} copper and {} item(s):", loot.money, loot.items.len());
                for item in &loot.items {
                    println!(
                        "  loot slot {:>2}: entry {} x{} (display {}, slot type {})",
                        item.slot, item.entry, item.count, item.display_id, item.slot_type
                    );
                }
            }
            // Printed rather than swallowed: an unparseable response is the
            // single most interesting thing this command can produce.
            Err(error) => println!("\nSMSG_LOOT_RESPONSE did not parse: {error}"),
        }
    }

    println!("\nevery opcode seen:");
    for (opcode, count) in &seen {
        println!("  {:<34} ({opcode:#06x}) x{count}", world::opcode::describe(*opcode));
    }

    // **Then take it, and prove it arrived by looking at the inventory.**
    //
    // Neither the money request nor the take-an-item request is acknowledged,
    // so both are confirmed the same way the equip write was: by a
    // consequence that could have failed to appear. Money shows up in
    // `PLAYER_FIELD_COINAGE` and an item shows up in the player's own slot
    // array, and both are read here before and after.
    state.replicate(&batch, None);
    let money_before = world::inventory::coinage(state, own_guid);
    let held_before: std::collections::BTreeSet<u64> = world::inventory::held(state, own_guid)
        .into_iter()
        .map(|item| item.guid)
        .collect();

    let taken: Vec<u8> = batch
        .iter()
        .filter(|p| p.opcode == world::opcode::server::LOOT_RESPONSE)
        .filter_map(|p| world::loot::parse_loot_response(&p.body).ok())
        .flat_map(|loot| loot.items)
        // The server's own slot index, not an index into this list. See
        // `ClientOpcode::AutoStoreLootItem` for why that distinction bites.
        .map(|item| item.slot)
        .collect();

    if money_before > 0 || !taken.is_empty() {
        connection.loot_money()?;
        for slot in &taken {
            connection.loot_item(*slot)?;
        }
        let after = connection.drain(std::time::Duration::from_millis(1500), 128)?;
        state.replicate(&after, None);

        let money_after = world::inventory::coinage(state, own_guid);
        let held_after: std::collections::BTreeSet<u64> = world::inventory::held(state, own_guid)
            .into_iter()
            .map(|item| item.guid)
            .collect();
        let gained: Vec<u64> = held_after.difference(&held_before).copied().collect();

        println!(
            "\ntook {} slot(s): money {money_before} -> {money_after}, {} new item(s) in the bags",
            taken.len(),
            gained.len()
        );
        for guid in &gained {
            let entry = state
                .get(*guid)
                .and_then(|item| item.fields.get(world::update::fields::OBJECT_ENTRY));
            println!("  {guid:#018x} entry {entry:?}");
        }
        if gained.is_empty() && money_after == money_before {
            println!("  nothing moved -- which is what a wrong opcode looks like,");
            println!("  and also what a full bag looks like. Check the bags first.");
        }

        // **What the server says when loot is taken**, which is a different
        // question from whether it was taken. A window has to stop showing a
        // row that is gone, and the only thing that can tell it is one of
        // these -- so they get printed with their bodies for the same reason
        // the open did.
        println!("\nafter taking, every opcode with a body:");
        let mut seen_after: std::collections::BTreeMap<u16, usize> = Default::default();
        for packet in &after {
            *seen_after.entry(packet.opcode).or_default() += 1;
        }
        for (opcode, count) in &seen_after {
            println!(
                "  {:<34} ({opcode:#06x}) x{count}",
                world::opcode::describe(*opcode)
            );
        }
        for packet in &after {
            if NOISE.contains(&packet.opcode) {
                continue;
            }
            println!(
                "  {} ({:#06x}) {} bytes: {}",
                world::opcode::describe(packet.opcode),
                packet.opcode,
                packet.body.len(),
                hex_preview(&packet.body, 32)
            );
        }
    }

    // **Released last, and this order is the whole of it.** The first
    // version of this released the corpse here and then went on to take
    // things off it, which is a closed corpse: the money still moved, the
    // item did not, and the printout said "0 new item(s)" as though the
    // opcode were wrong. Anything that acts on the open loot has to happen
    // before this.
    //
    // Releasing at all matters even in a survey: a corpse stays locked to
    // whoever opened it, so a run that walks away leaves a body nobody else
    // on the realm can touch.
    connection.loot_release(corpse)?;
    let after = connection.drain(std::time::Duration::from_millis(600), 64)?;
    println!("\nafter release, {} more packet(s):", after.len());
    for packet in &after {
        if NOISE.contains(&packet.opcode) {
            continue;
        }
        println!(
            "  {} ({:#06x}) {} bytes: {}",
            world::opcode::describe(packet.opcode),
            packet.opcode,
            packet.body.len(),
            hex_preview(&packet.body, 64)
        );
    }

    state.replicate(&batch, None);
    Ok(())
}

/// Asks the server about every quest id up to `highest` and reports how many
/// answers the parser consumed whole.
///
/// **Requests go out in blocks and replies are collected in bulk**, because
/// one query per drain would take four hours over nine thousand quests. The
/// server answers each independently and the reply carries its own quest id,
/// so nothing depends on the order they come back in.
///
/// A missing answer is *not* counted as a failure: an id with no quest behind
/// it is simply never answered, and the table is sparse. What is counted is
/// the split between answers that parsed and answers that did not -- the only
/// two outcomes that say anything about the reading.
fn sweep_quests(connection: &mut world::Connection, highest: u32) -> Result<()> {
    // Big enough that the round trips amortise, small enough that the server's
    // send buffer is not asked to hold nine thousand replies at once.
    const BLOCK: u32 = 200;
    // **A wall clock, because a packet count is not one.** The first version of
    // this drained with a 4,096-packet bound and no clock, against a zone that
    // emits a monster move fourteen times a second and is *never* quiet -- so
    // every block collected four thousand packets of background traffic to
    // find two hundred answers, and a seven-minute sweep had not finished
    // after twenty. Exactly the bug written up for the login burst, walked
    // into again by the next loop that had a limit and no deadline.
    const BLOCK_BUDGET: std::time::Duration = std::time::Duration::from_secs(6);
    // Short, so a block that has all its answers moves on immediately instead
    // of sitting out the budget.
    const SIP: std::time::Duration = std::time::Duration::from_millis(120);

    println!("
asking about every quest from 1 to {highest}");
    let started = std::time::Instant::now();
    let mut answered = 0usize;
    let mut parsed = 0usize;
    let mut failures: Vec<(u32, String)> = Vec::new();
    let mut lengths = (u32::MAX, 0u32);

    let mut id = 1;
    while id <= highest {
        let end = (id + BLOCK).min(highest + 1);
        let asked = (end - id) as usize;
        for quest in id..end {
            connection.query_quest_info(quest)?;
        }

        let deadline = std::time::Instant::now() + BLOCK_BUDGET;
        let mut in_block = 0usize;
        while in_block < asked && std::time::Instant::now() < deadline {
            let batch = connection.drain(SIP, 512)?;
            if batch.is_empty() {
                continue;
            }
            for packet in &batch {
                if packet.opcode != world::opcode::server::QUEST_QUERY_RESPONSE {
                    continue;
                }
                in_block += 1;
                answered += 1;
                lengths.0 = lengths.0.min(packet.body.len() as u32);
                lengths.1 = lengths.1.max(packet.body.len() as u32);
                match world::quest::parse_quest_query(&packet.body) {
                    Ok(_) => parsed += 1,
                    Err(error) => {
                        let quest = packet
                            .body
                            .get(..4)
                            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                            .unwrap_or(0);
                        if failures.len() < 20 {
                            failures.push((quest, error.to_string()));
                        }
                    }
                }
            }
        }

        // **Progress, because a silent twenty-minute run is indistinguishable
        // from a hung one.** That is how the first version of this wasted
        // twenty minutes before anyone could tell it was stuck.
        println!(
            "  {:>6}..{:<6} {in_block:>4} answered   {parsed}/{answered} parsed   {:.0}s elapsed",
            id,
            end - 1,
            started.elapsed().as_secs_f32()
        );
        id = end;
    }

    println!("
{answered} answered, {parsed} parsed whole");
    if answered > 0 {
        println!(
            "  {:.1}% -- bodies from {} to {} bytes",
            100.0 * parsed as f64 / answered as f64,
            lengths.0,
            lengths.1
        );
    }
    if failures.is_empty() {
        println!("  no failures. A body that ends anywhere but its last byte is an");
        println!("  error here, so this is the layout confirmed across the table and");
        println!("  not merely across the quests somebody chose to look at.");
    } else {
        println!("
{} failure(s), first {} shown:", failures.len(), failures.len());
        for (quest, error) in &failures {
            println!("  quest {quest}: {error}");
        }
    }
    Ok(())
}

/// Dumps the quest log's raw slots.
///
/// Prints all five fields of each occupied slot under headings that say what
/// is known and what is not, rather than under four guessed names. A wrong
/// name on a counter would misreport progress rather than fail, which is the
/// class of mistake this project pays most for.
fn report_quest_log(state: &world::WorldState, own_guid: u64) {
    let Some(player) = state.get(own_guid) else {
        println!("
no replicated player object to read a quest log from");
        return;
    };
    let slots = player.quest_log_slots();
    println!("
quest log: {} occupied slot(s)", slots.len());
    println!("  {:>6}  {:>10} {:>10} {:>10} {:>10}", "quest", "+1", "+2", "+3", "+4");
    for (id, rest) in &slots {
        println!(
            "  {id:>6}  {:>10} {:>10} {:>10} {:>10}",
            rest[0], rest[1], rest[2], rest[3]
        );
    }
    if !slots.is_empty() {
        println!("
  Only the quest id is identified. The four columns are printed");
        println!("  unnamed on purpose -- run this against a character with one quest");
        println!("  finished and one not, and the column that differs the way");
        println!("  completion differs is the one worth naming.");
    }
}

/// Prints a parsed quest the way a quest log would show it.
///
/// **Every line here is a field that was checked against the realm's own
/// `quest_template`**, and the fields that were not -- the faction
/// requirements, the reward-faction override array, the point's `option` --
/// are not printed at all rather than printed under a guessed name. A wrong
/// name on a number nobody can check is this project's most expensive kind of
/// mistake.
fn print_quest_info(q: &world::QuestInfo) {
    println!("  [{}] {}", q.id, q.title);
    println!("  level {} (min {}), sort {}", q.level, q.min_level, q.sort);
    if !q.objectives_text.is_empty() {
        println!("  objective: {}", q.objectives_text);
    }
    for objective in &q.objectives {
        let what = match objective.target {
            Some(world::ObjectiveTarget::Creature(id)) => format!("creature {id}"),
            Some(world::ObjectiveTarget::GameObject(id)) => format!("game object {id}"),
            None => "nothing to kill".to_string(),
        };
        let drop = if objective.item_drop != 0 {
            format!(", drops item {}", objective.item_drop)
        } else {
            String::new()
        };
        let text = if objective.text.is_empty() {
            String::new()
        } else {
            format!(" -- {}", objective.text)
        };
        println!("    x{} {what}{drop}{text}", objective.count);
    }
    for item in &q.item_objectives {
        println!("    x{} item {}", item.count, item.item);
    }
    if q.money != 0 {
        println!("  reward: {} copper", q.money);
    }
    for reward in &q.reward_items {
        println!("  reward: item {} x{}", reward.item, reward.count);
    }
    for choice in &q.reward_choices {
        println!("  choice: item {} x{}", choice.item, choice.count);
    }
    for (faction, value) in &q.reward_factions {
        println!("  faction {faction}: QuestFactionReward row {value}");
    }
    if q.next_quest != 0 {
        println!("  next in chain: {}", q.next_quest);
    }
    if q.start_item != 0 {
        println!("  starts you holding item {}", q.start_item);
    }
    if let Some(point) = q.point {
        println!("  point: map {} at {:.1}, {:.1}", point.map, point.x, point.y);
    }
    // **The flag field's low half only.** Printing it as "flags" without this
    // note invites the next reader to test 0x80000 against it, which can never
    // be set. See `QuestInfo::flags`.
    println!("  flags {:#x} (low 16 bits only -- auto-accept cannot appear)", q.flags);
}

/// Asks what one quest is, and dumps the answer.
///
/// **Needs no NPC and no quest log**, which is the whole point: a tracker has
/// to describe quests the player is not standing in front of, and a map has to
/// label a pin for a quest that has not been taken. This is the request that
/// makes both possible without shipping a database.
fn survey_quest_info(
    connection: &mut world::Connection,
    quest: u32,
    // **A packet printed truncated is a packet seen and lost.** This body runs
    // to hundreds of bytes and `dump_unexpected` stops at 640 characters of
    // hex, which is a fifth of it. The capture file is what the layout gets
    // worked out from, so the survey that produces the packet has to feed it.
    mut capture: Option<&mut Capture>,
) -> Result<()> {
    println!("
asking what quest {quest} is");
    connection.query_quest_info(quest)?;
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    if let Some(capture) = capture.as_mut() {
        capture.record(&batch)?;
    }
    dump_unexpected(&batch, &format!("after CMSG_QUEST_QUERY for {quest}"));

    match batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::QUEST_QUERY_RESPONSE)
    {
        Some(reply) => {
            println!(
                "
SMSG_QUEST_QUERY_RESPONSE for {quest}, {} bytes",
                reply.body.len()
            );
            match world::quest::parse_quest_query(&reply.body) {
                Ok(info) => print_quest_info(&info),
                // **Loud, and with the bytes.** A body this size that a cursor
                // refuses is the single most informative packet this command
                // can produce, and printing only the error would throw it
                // away -- which is how `SMSG_ATTACKERSTATEUPDATE` was once
                // seen and lost.
                Err(error) => {
                    println!("  PARSE FAILED: {error}");
                    println!("  {}", hex_preview(&reply.body, 4096));
                }
            }
        }
        None => {
            println!("
no answer. A quest query needs no NPC and no log entry, so");
            println!("a silence here is about the opcode or the body and nothing else.");
        }
    }
    Ok(())
}

/// Asks the server where the objectives of every quest in the log are.
///
/// **The whole native-quest-tracker plan rests on this working.** WotLK
/// shipped its own tracker, so the server already holds the map markers and
/// will hand them over -- which is why this client does not need to ship
/// anybody's quest database. If it comes back empty, that plan needs
/// rethinking, so the command is deliberately loud about which of the two
/// reasons an empty answer has.
/// Asks what mark belongs over every nearby NPC's head, both ways.
///
/// **Two sends, because a silence from one is bounded by the other's answer.**
/// Nothing acknowledges an outgoing opcode, so a wrong number and a request
/// the server declined look identical -- exactly the trap that cost three runs
/// on `CMSG_BUY_ITEM`. Asking per guid and asking for everything at once are
/// separate opcodes with separate replies, and either arriving proves the
/// numbering of that half.
///
/// Statuses are printed raw. What each value means is the question, and the
/// answer comes from running this against NPCs whose state is already known.
fn survey_questgiver_status(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
) -> Result<()> {
    let talkers: Vec<(u64, u32, u32)> = state
        .iter()
        .filter(|entity| entity.guid != own_guid)
        .filter(|entity| entity.will_talk())
        .map(|entity| {
            (
                entity.guid,
                entity
                    .fields
                    .get(world::update::fields::OBJECT_ENTRY)
                    .unwrap_or(0),
                entity.npc_flags().unwrap_or(0),
            )
        })
        .collect();

    println!("\n{} talker(s) in range", talkers.len());
    if talkers.is_empty() {
        println!("  nothing to ask about, which makes a silence below mean nothing.");
        println!("  Put one in reach: --say \".npc add 823\"");
        return Ok(());
    }

    for (guid, entry, flags) in &talkers {
        connection.query_questgiver_status(*guid)?;
        println!("  asked about {guid:#018x} entry {entry} npcflag {flags} ({flags:#x})");
    }
    connection.query_questgiver_status_multiple()?;
    println!("  asked about everything in range at once");

    let batch = connection.drain(std::time::Duration::from_millis(3000), 256)?;
    state.replicate(&batch, None);

    let singles: Vec<_> = batch
        .iter()
        .filter(|p| p.opcode == world::opcode::server::QUESTGIVER_STATUS)
        .collect();
    let multiple: Vec<_> = batch
        .iter()
        .filter(|p| p.opcode == world::opcode::server::QUESTGIVER_STATUS_MULTIPLE)
        .collect();

    println!(
        "\n{} single reply(s), {} multiple reply(s)",
        singles.len(),
        multiple.len()
    );
    if singles.is_empty() && multiple.is_empty() {
        println!("  nothing came back at all -- a wrong opcode, a wrong body, or a");
        println!("  server that answers neither. Every opcode that did arrive:");
        let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
        for packet in &batch {
            *seen.entry(packet.opcode).or_default() += 1;
        }
        for (opcode, count) in seen {
            println!("    {:<32} x{count}", world::opcode::describe(opcode));
        }
        return Ok(());
    }

    let name_of = |guid: u64| -> String {
        state
            .get(guid)
            .and_then(|e| e.fields.get(world::update::fields::OBJECT_ENTRY))
            .map(|entry| format!("entry {entry}"))
            .unwrap_or_else(|| "not replicated".into())
    };

    for packet in &singles {
        println!(
            "\nSMSG_QUESTGIVER_STATUS, {} bytes: {}",
            packet.body.len(),
            packet
                .body
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if packet.body.len() >= 9 {
            let guid = u64::from_le_bytes(packet.body[..8].try_into().unwrap());
            println!(
                "  guid {guid:#018x} ({})  status byte {}",
                name_of(guid),
                packet.body[8]
            );
        }
    }

    for packet in &multiple {
        println!(
            "\nSMSG_QUESTGIVER_STATUS_MULTIPLE, {} bytes",
            packet.body.len()
        );
        if packet.body.len() < 4 {
            continue;
        }
        let count = u32::from_le_bytes(packet.body[..4].try_into().unwrap());
        println!("  {count} entries");
        // Nine bytes each if the status is a byte, twelve if it is a `u32`.
        // Both are printed rather than one assumed: the arithmetic below says
        // which reading the length agrees with, and a reading that does not
        // divide the body exactly is wrong however plausible it looks.
        let rest = packet.body.len() - 4;
        for (width, label) in [(9usize, "u64 + u8"), (12, "u64 + u32")] {
            if count > 0 && rest == count as usize * width {
                println!("  body divides exactly as {count} x {width} bytes ({label})");
                for i in 0..count as usize {
                    let at = 4 + i * width;
                    let guid = u64::from_le_bytes(packet.body[at..at + 8].try_into().unwrap());
                    let status = if width == 9 {
                        u32::from(packet.body[at + 8])
                    } else {
                        u32::from_le_bytes(packet.body[at + 8..at + 12].try_into().unwrap())
                    };
                    println!("    guid {guid:#018x} ({})  status {status}", name_of(guid));
                }
            }
        }
    }

    Ok(())
}

/// Records every talkable NPC in range, asks what mark each wears, and
/// round-trips the cache through a file.
///
/// **The one part of the native tracker that is not a server answer.** Every
/// other thing 4.31 draws -- a quest's objectives, its map markers, the mark
/// over an NPC's head -- came off the wire and can be checked against the
/// realm's own tables. A remembered questgiver is this client's memory of what
/// it was streamed, and the only question worth asking about a memory is
/// whether it survives being written down. So the save and the reload happen
/// here rather than only in a unit test: a disagreement in this run is a cache
/// a real session would have lost.
fn survey_questgivers(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    map: u32,
) -> Result<()> {
    let mut givers = world::Questgivers::new();

    // Everything in range that will talk, with its entry and its position.
    // The same filter the viewer uses, and it is deliberately `will_talk`
    // rather than a questgiver-flag test: what an NPC will *do* is settled by
    // sending the request, and the flag word has bits this client has not
    // named.
    let seen: Vec<(u64, u32, f32, f32, f32)> = state
        .iter()
        .filter(|entity| entity.guid != own_guid && entity.will_talk())
        .filter_map(|entity| {
            let entry = entity.entry()?;
            let at = entity.position?;
            Some((entity.guid, entry, at.x, at.y, at.z))
        })
        .collect();
    println!("\n{} talkable NPC(s) in range on map {map}", seen.len());
    if seen.is_empty() {
        println!("  nothing to record. This is vacuous rather than negative --");
        println!("  the cache fills from replicated state, so an empty street");
        println!("  says nothing about whether recording works. Stand near an");
        println!("  innkeeper or a questgiver and try again.");
        return Ok(());
    }
    for (guid, entry, x, y, z) in &seen {
        givers.see(*guid, *entry, map, *x, *y, *z);
    }

    // What is over each head. Asked one at a time, because the reply names
    // the guid it is about and that pairing is the only thing that confirms
    // the request went out as the right opcode.
    for (guid, ..) in &seen {
        connection.query_questgiver_status(*guid)?;
    }
    let batch = connection.drain(std::time::Duration::from_millis(2000), 256)?;
    dump_unexpected(&batch, "after CMSG_QUESTGIVER_STATUS_QUERY");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let mut answered = 0;
    for packet in &batch {
        if packet.opcode != world::opcode::server::QUESTGIVER_STATUS {
            continue;
        }
        match world::quest::parse_questgiver_status(&packet.body) {
            Ok(status) => {
                answered += 1;
                givers.mark(status.npc, status.mark, now);
            }
            Err(error) => println!("  a status would not parse: {error}"),
        }
    }
    // **Both numbers, always.** A counter that only speaks on failure cannot
    // tell "none were wrong" from "there were none".
    println!(
        "{answered} of {} answered; {} recorded, {} worth drawing",
        seen.len(),
        givers.len(),
        givers.on_map(map).count()
    );

    for known in givers.on_map(map) {
        println!(
            "  {:#018x}  entry {:>6}  ({:>8.1}, {:>8.1}, {:>7.1})  {:?}",
            known.guid, known.entry, known.x, known.y, known.z, known.mark
        );
    }

    // The round trip. Written to a temporary file and read straight back,
    // because what this module has to be right about is exactly that.
    let path = std::env::temp_dir().join("wow-cli-questgivers.cache");
    givers.save(&path)?;
    let back = world::Questgivers::load(&path)?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    std::fs::remove_file(&path).ok();

    let mut differed = 0;
    for before in givers.iter() {
        match back.get(before.guid) {
            // `live` deliberately does not survive: a loaded record is a
            // memory by definition, and that is the field that keeps a
            // remembered pin from being drawn as a fact.
            Some(after) if after.live => {
                println!("  {:#018x} came back marked live, which it must not", before.guid);
                differed += 1;
            }
            Some(after)
                if after.entry == before.entry
                    && after.map == before.map
                    && after.x == before.x
                    && after.y == before.y
                    && after.z == before.z
                    && after.mark == before.mark
                    && after.seen == before.seen
                    && after.offers == before.offers => {}
            Some(after) => {
                println!("  {:#018x} changed: {before:?} -> {after:?}", before.guid);
                differed += 1;
            }
            None => {
                println!("  {:#018x} did not survive the save at all", before.guid);
                differed += 1;
            }
        }
    }
    println!(
        "cache round trip: {} bytes, {} of {} records identical, {differed} differed",
        bytes,
        givers.len() - differed,
        givers.len()
    );

    Ok(())
}

fn survey_quest_poi(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    data: Option<&std::path::Path>,
    locale: &str,
) -> Result<()> {
    let log = quest_log_ids(state, own_guid);
    println!("\nquest log holds {} quest(s): {log:?}", log.len());
    if log.is_empty() {
        println!("  nothing to ask about. The POI query answers only for quests");
        println!("  in the log, so an empty log makes this test vacuous rather");
        println!("  than negative. Take a quest first, or --say \".quest add 16\".");
        return Ok(());
    }

    connection.query_quest_poi(&log)?;
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    dump_unexpected(&batch, "after CMSG_QUEST_POI_QUERY");
    state.replicate(&batch, None);

    let Some(reply) = batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::QUEST_POI_QUERY_RESPONSE)
    else {
        println!("\nno POI response at all. That is a wrong opcode or a wrong body,");
        println!("*not* an absence of data -- the two look identical here, which is");
        println!("why the log contents are printed above.");
        return Ok(());
    };
    println!("\nSMSG_QUEST_POI_QUERY_RESPONSE, {} bytes", reply.body.len());

    let sets = world::quest::parse_quest_poi(&reply.body)?;
    println!(
        "{} quest(s) answered, {} marker(s), {} point(s)",
        sets.len(),
        sets.iter().map(|s| s.markers.len()).sum::<usize>(),
        sets.iter()
            .flat_map(|s| &s.markers)
            .map(|m| m.points.len())
            .sum::<usize>()
    );

    // The projection, from the same module the viewer draws with rather than
    // recomputed here -- two copies of a coordinate transform agree only until
    // one of them changes, and this exists to check the one the window uses.
    let atlas = match data {
        Some(data) => {
            let mut chain = Chain::open_wow_data(data, locale)
                .with_context(|| format!("opening archives under {}", data.display()))?;
            Some(load_atlas(&mut chain)?)
        }
        None => {
            println!(
                "\n(pass --data or set WOW_DATA to see where each marker lands on \
                 its page; WorldMapArea.dbc is what turns a world coordinate into \
                 a pixel)"
            );
            None
        }
    };

    for set in &sets {
        println!("\nquest {}: {} marker(s)", set.quest_id, set.markers.len());
        for poi in &set.markers {
            let page = atlas.as_ref().and_then(|a| a.page(poi.world_map_area_id));
            let name = page.map_or_else(
                || "no such page".to_string(),
                |page| page.directory.clone(),
            );
            println!(
                "  marker {} objective {:?} map {} page {} ({name}) {} point(s)",
                poi.id,
                poi.objective_index,
                poi.map_id,
                poi.world_map_area_id,
                poi.points.len()
            );
            let Some(page) = page else { continue };
            // Every point, not a centroid: a marker whose points are right and
            // whose middle is wrong and one whose points are all wrong look
            // the same from a single averaged number.
            for (x, y) in &poi.points {
                let (px, py) = page.project_pixels(*x as f32, *y as f32);
                let on_page = (0.0..=dbc::worldmap::PAGE_WIDTH).contains(&px)
                    && (0.0..=dbc::worldmap::PAGE_HEIGHT).contains(&py);
                println!(
                    "    world {x:>7}, {y:>7}  ->  pixel {px:>7.1}, {py:>7.1}{}",
                    if on_page { "" } else { "   OFF THE PAGE" }
                );
            }
        }
    }

    Ok(())
}

/// Greets a questgiver, and optionally reads and accepts one of its quests.
///
/// **A survey first.** Nothing quest-related is parsed by this client yet, so
/// every reply is printed as its opcode and its bytes in full -- the moment
/// this stops dumping bodies is the moment the next unknown shape becomes
/// invisible.
///
/// The flow it drives is the real one a client uses, in order, because each
/// step's reply is what makes the next legal: greet the NPC, ask for a
/// specific quest's scroll, accept it. Accepting is confirmed by effect -- the
/// quest appears in the player's own replicated log -- since nothing
/// acknowledges the send.
fn survey_quests(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    here: &mut world::Position,
    prefer: Option<u32>,
    accept: Option<u32>,
    turn_in: Option<u32>,
    mut capture: Option<&mut Capture>,
) -> Result<()> {
    let Some(npc) = approach_talker(connection, state, own_guid, here, prefer)? else {
        return Ok(());
    };

    connection.set_selection(npc.guid)?;
    println!(
        "\ngreeting questgiver {:#018x} entry {} at {:.1} units, npcflag {} ({:#x})",
        npc.guid, npc.entry, npc.distance, npc.flags, npc.flags
    );
    // **Before the greeting, not after it.** Greeting a questgiver can itself
    // put a quest in the log -- see [`accept_one_quest`] -- so a log read after
    // the hello cannot tell "the character already had it" from "this run just
    // caused it", and those want opposite next steps.
    let log_at_entry = quest_log_ids(state, own_guid);

    connection.questgiver_hello(npc.guid)?;
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    state.replicate(&batch, None);
    if let Some(capture) = capture.as_mut() {
        capture.record(&batch)?;
    }

    dump_unexpected(&batch, "after CMSG_QUESTGIVER_HELLO");

    if let Some(wanted) = accept {
        accept_one_quest(
            connection,
            state,
            own_guid,
            &npc,
            wanted,
            &log_at_entry,
            capture.as_deref_mut(),
        )?;
    }
    if let Some(wanted) = turn_in {
        turn_in_one_quest(connection, state, own_guid, &npc, wanted, capture)?;
    }
    Ok(())
}

/// Reads one quest's scroll and takes it, reporting which of our own fields
/// moved.
///
/// Split out of [`survey_quests`] when the turn-in half was added: both halves
/// need the same greeting first, and a second copy of the approach-and-greet
/// would have drifted from this one.
///
/// **Asking for a quest's scroll can accept it.** A quest carrying
/// `QUEST_FLAGS_AUTO_ACCEPT` is added to the log by the server when
/// `CMSG_QUESTGIVER_QUERY_QUEST` arrives, before this function has sent
/// anything at all -- so on such a quest the accept that follows is a no-op
/// against a log that already holds it, and every effect this measures by has
/// already happened. Only 179 of 9,464 quests on the development realm are
/// like that, which is precisely why it is worth reporting rather than
/// stumbling into: it is rare enough to look like a bug and common enough to
/// hit the starting-zone chain, which is what a first end-to-end test uses.
///
/// `log_at_entry` is therefore read *before the greeting* and passed in. With
/// it, "the character already held this quest" and "this run's own scroll
/// request took it" are separable, and they want opposite next steps -- the
/// first is fixed by clearing the quest, the second is not fixable at all and
/// means choosing a different quest.
fn accept_one_quest(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    npc: &Talker,
    wanted: u32,
    log_at_entry: &[u32],
    mut capture: Option<&mut Capture>,
) -> Result<()> {

    // Ask for the scroll before accepting, in that order, because that is the
    // order a real client uses and the server checks the NPC actually offers
    // the quest at each step. Skipping straight to the accept would work or
    // not for reasons this survey could not tell apart.
    println!("\nasking for quest {wanted}'s details");
    connection.query_quest(npc.guid, wanted)?;
    let details = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    state.replicate(&details, None);
    if let Some(capture) = capture.as_mut() {
        capture.record(&details)?;
    }
    dump_unexpected(&details, "after CMSG_QUESTGIVER_QUERY_QUEST");

    // **Snapshot every field of our own object, not a quest log we cannot yet
    // read.**
    //
    // Where the quest log lives in the update fields is *not known*, and
    // transcribing an index from memory is the mistake this project keeps
    // paying for -- a wrong one parses perfectly and reports somebody else's
    // number as a quest. So the accept is confirmed the way `PLAYER_BYTES` and
    // the visible-item block were found: by searching for an answer already
    // known from somewhere else.
    //
    // The quest id is that answer. We chose it, the server is about to store
    // it, and no other field has any reason to hold it -- so whichever index
    // changes *to that exact value* is the log, and the search cannot come out
    // right by luck the way "contains a plausible small integer" can.
    let before = own_fields_snapshot(state, own_guid);
    let log_before = quest_log_ids(state, own_guid);
    println!("\nsnapshotted {} set fields before accepting", before.len());
    println!("quest log at login:      {log_at_entry:?}");
    println!("quest log after the scroll: {log_before:?}");

    println!("accepting quest {wanted}");
    connection.accept_quest(npc.guid, wanted)?;
    let accepted = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    state.replicate(&accepted, None);
    dump_unexpected(&accepted, "after CMSG_QUESTGIVER_ACCEPT_QUEST");

    let after = own_fields_snapshot(state, own_guid);
    let mut changed: Vec<(u16, Option<u32>, u32)> = Vec::new();
    for (index, value) in &after {
        if before.get(index) != Some(value) {
            changed.push((*index, before.get(index).copied(), *value));
        }
    }
    changed.sort_by_key(|(index, _, _)| *index);

    println!("\n{} field(s) of our own object changed:", changed.len());
    for (index, was, now) in &changed {
        let note = if *now == wanted {
            "  <-- the quest id we asked for"
        } else {
            ""
        };
        println!("  {index:#06x}: {was:?} -> {now}{note}");
    }

    let log_after = quest_log_ids(state, own_guid);
    println!("quest log after:  {log_after:?}");

    match changed.iter().find(|(_, _, now)| *now == wanted) {
        Some((index, _, _)) => {
            println!("\n-- quest {wanted} landed in field {index:#06x}, which is the");
            println!("   accept confirmed by a number nothing else had reason to hold.");
        }
        None if log_after.contains(&wanted) && !log_before.contains(&wanted) => {
            println!("\n-- quest {wanted} is in the log now and was not before, so the");
            println!("   accept took even though this run did not catch the field");
            println!("   update in its own drain.");
        }
        // **Two ways to already hold it, and only one of them is your fault.**
        // Telling a caller to clear a quest that this very run's scroll
        // request will re-accept sends them round the same loop forever, which
        // is the "nothing happened is two findings wearing one sentence" trap
        // wearing a third hat.
        None if log_after.contains(&wanted) && !log_at_entry.contains(&wanted) => {
            println!("\n-- quest {wanted} entered the log during THIS run, but before the");
            println!("   accept was sent -- it was not there at login and was there by");
            println!("   the time the scroll came back. That is an auto-accept quest:");
            println!("   the server adds it on CMSG_QUESTGIVER_QUERY_QUEST, so the");
            println!("   accept had nothing left to do and this run does not test it.");
            println!("   Clearing the quest will NOT help; pick a quest whose");
            println!("   quest_template.Flags lacks 0x80000 and whose");
            println!("   quest_template_addon.SpecialFlags lacks 0x4.");
        }
        None if log_after.contains(&wanted) => {
            println!("\n-- quest {wanted} was ALREADY in the log at login, so nothing here");
            println!("   tests the accept. Clear it first:");
            println!("   --say \".quest remove {wanted}\"");
        }
        None if changed.is_empty() => {
            println!("\n-- nothing changed and the quest is not in the log. That is");
            println!("   what a wrong opcode looks like, and also what a declined");
            println!("   quest looks like. The bodies above are what separates them.");
        }
        None => {
            println!("\n-- fields moved, but none of them holds {wanted}, and the log");
            println!("   does not have it. The accept was probably declined.");
        }
    }

    // **A refusal packet here does not mean the accept failed**, and reading it
    // that way cost a whole investigation. `SMSG_QUESTGIVER_QUEST_INVALID`
    // (`0x018F`) arrived carrying 13 on every single run -- including runs
    // where the quest demonstrably *was* accepted, confirmed both by the
    // server's own database and by the questgiver no longer offering it
    // afterwards. The accept handler never sends that packet at all; it comes
    // from a re-evaluation after the quest is already in the log, which is
    // exactly when "you are already on that quest" is true.
    //
    // So it is reported as an observation and explicitly not as a verdict.
    for packet in &accepted {
        if packet.opcode != 0x018F || packet.body.len() != 4 {
            continue;
        }
        let reason = u32::from_le_bytes(packet.body[..4].try_into().unwrap());
        println!("\nnote: 0x018F arrived carrying {reason}. This is NOT a verdict on");
        println!("      the accept -- it shows up even when the quest was taken.");
        println!("      Judge the accept by the log above, not by this packet.");
    }

    Ok(())
}

/// Hands a finished quest in: offer it, read the reward screen, take the
/// reward.
///
/// **Two sends, and neither of them is acknowledged as such.** The pair had
/// existed in `client.rs` unfired since the milestone opened, which is exactly
/// the situation this project's notes say is expensive: a write nothing
/// acknowledges fails identically whether the opcode is wrong, the body is
/// wrong, or the request was declined.
///
/// What makes it tractable is that the *first* of the two talks back.
/// `CMSG_QUESTGIVER_COMPLETE_QUEST` is answered -- with the reward screen if
/// the quest is finished, and with the still-wanted list if it is not -- so it
/// bounds the silent second send the same way `CMSG_LIST_INVENTORY` bounded
/// `CMSG_BUY_ITEM`. Which of those two replies arrives is itself the
/// diagnosis, and they are reported as different outcomes rather than as one
/// "no reward screen".
///
/// The verdict is by effect and never by a packet: **the quest leaves
/// `PLAYER_QUEST_LOG`**, which is a field this project measured rather than
/// transcribed. A turn-in that produced every expected packet and left the
/// quest in the log did not happen.
fn turn_in_one_quest(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    npc: &Talker,
    wanted: u32,
    mut capture: Option<&mut Capture>,
) -> Result<()> {
    let log_before = quest_log_ids(state, own_guid);
    println!("\nquest log before turning in: {log_before:?}");
    if !log_before.contains(&wanted) {
        println!("\n-- quest {wanted} is NOT in the log, so this run cannot test a");
        println!("   turn-in: an untaken quest is refused for a reason that has");
        println!("   nothing to do with whether the two sends are right. Accept it");
        println!("   first (--quest <starter> --quest-accept {wanted}).");
        return Ok(());
    }

    // The reward screen is asked for by offering the quest, and the offer is
    // sent to *this* NPC -- which has to be the quest's ender rather than its
    // starter. The two are different creatures for most quests, and sending it
    // to the starter is refused in silence.
    println!("\noffering quest {wanted} to entry {} for completion", npc.entry);
    connection.complete_quest(npc.guid, wanted)?;
    let offered = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    state.replicate(&offered, None);
    if let Some(capture) = capture.as_mut() {
        capture.record(&offered)?;
    }
    dump_unexpected(&offered, "after CMSG_QUESTGIVER_COMPLETE_QUEST");

    let reward_screen = offered
        .iter()
        .find(|p| p.opcode == world::opcode::server::QUESTGIVER_OFFER_REWARD);
    let still_wants = offered
        .iter()
        .find(|p| p.opcode == world::opcode::server::QUESTGIVER_REQUEST_ITEMS);

    match (reward_screen, still_wants) {
        (Some(reply), _) => println!(
            "\nSMSG_QUESTGIVER_OFFER_REWARD, {} bytes -- the quest is finished and\n\
             the server is showing what it pays.",
            reply.body.len()
        ),
        (None, Some(reply)) => {
            println!(
                "\nSMSG_QUESTGIVER_REQUEST_ITEMS, {} bytes -- the send was understood",
                reply.body.len()
            );
            println!("and the quest's objectives are simply not done yet. That is a");
            println!("statement about the character, not about the opcode.");
            return Ok(());
        }
        (None, None) => {
            println!("\nneither reward screen nor wanted-list came back. Both replies");
            println!("are ordinary answers to this send, so a silence is about the");
            println!("opcode, the body, or this NPC not being the quest's ender --");
            println!("and the bodies above are what separates those.");
            return Ok(());
        }
    }

    // `0` is not a guess here. Quest 783 has no `RewardChoiceItemID1` at all,
    // so there is exactly one thing the index could mean and no ambiguity to
    // resolve. A quest that *does* offer a choice needs this measured against
    // which item actually arrives, which is a different run.
    println!("\ntaking reward index 0");
    connection.choose_quest_reward(npc.guid, wanted, 0)?;
    let finished = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    state.replicate(&finished, None);
    if let Some(capture) = capture.as_mut() {
        capture.record(&finished)?;
    }
    dump_unexpected(&finished, "after CMSG_QUESTGIVER_CHOOSE_REWARD");

    if let Some(reply) = finished
        .iter()
        .find(|p| p.opcode == world::opcode::server::QUESTGIVER_QUEST_COMPLETE)
    {
        println!(
            "\nSMSG_QUESTGIVER_QUEST_COMPLETE, {} bytes",
            reply.body.len()
        );
    }

    let log_after = quest_log_ids(state, own_guid);
    println!("quest log after turning in:  {log_after:?}");

    if log_after.contains(&wanted) {
        println!("\n-- quest {wanted} is STILL in the log. Whatever packets arrived,");
        println!("   the turn-in did not take: the log is the effect this is judged");
        println!("   by, and it did not move.");
    } else {
        println!("\n-- quest {wanted} has left the log, which is the turn-in confirmed");
        println!("   by effect. The log is a field this client measured, and no");
        println!("   packet had to be believed for this line to be true.");
    }

    Ok(())
}

/// Every quest id in the player's own log.
///
/// The confirmation instrument for accepting, and it reads *ids* rather than
/// counting entries on purpose: a count going up says something happened,
/// where the id appearing says the right thing happened.
fn quest_log_ids(state: &world::WorldState, own_guid: u64) -> Vec<u32> {
    state
        .get(own_guid)
        .map(|player| player.quest_log_ids())
        .unwrap_or_default()
}

/// Every field set on our own player object, for diffing one state against
/// another.
fn own_fields_snapshot(
    state: &world::WorldState,
    own_guid: u64,
) -> std::collections::BTreeMap<u16, u32> {
    state
        .get(own_guid)
        .map(|player| player.fields.iter().collect())
        .unwrap_or_default()
}

/// Prints every packet that is not constant background traffic, body and all.
///
/// Shared by the quest steps because each one wants the same thing and a
/// second copy would drift. Bodies rather than lengths: a packet that is seen
/// and dropped is the one packet that could have answered the question.
/// Prints every packet in a batch, decoded or not, minus the routine traffic.
///
/// **It is not a filter for surprises and must not say that it is.** It has
/// always printed everything that is not movement or time-sync, so the
/// questgiver probe's own `SMSG_QUESTGIVER_STATUS` replies were listed under
/// a heading calling them unexpected -- which sends a reader looking for a
/// fault in the twenty-one packets that arrived exactly as asked for. The
/// instrument being served is "print every opcode seen, decoded or not",
/// which this project has needed five times; a heading that misdescribes it
/// costs the next person that same look.
fn dump_unexpected(batch: &[world::client::Packet], what: &str) {
    const NOISE: [u16; 6] = [0x00DD, 0x00A9, 0x01F6, 0x0390, 0x0085, 0x0086];
    println!("\nevery packet that came back {what}, routine traffic aside:");
    let mut shown = 0;
    for packet in batch {
        if NOISE.contains(&packet.opcode) {
            continue;
        }
        println!(
            "  {} ({:#06x}), {} bytes",
            world::opcode::describe(packet.opcode),
            packet.opcode,
            packet.body.len()
        );
        println!("    {}", hex_preview(&packet.body, 640));
        shown += 1;
        if shown >= 16 {
            println!("  ... stopping after {shown}");
            break;
        }
    }
    if shown == 0 {
        println!("  none -- nothing but the usual traffic came back.");
    }
}

/// Finds a talker, walks into range, and reports who is actually in front of
/// you.
///
/// **Shared by every NPC survey rather than copied into each.** The approach
/// loop has already had one real bug -- it walked to *exactly* its interaction
/// reach and so could never get inside it -- and a second copy would have
/// re-armed exactly that. Reusing a mechanism is also how this project audits
/// one: the right-click gesture found a four-milestone-old bug in the left
/// button's by mirroring it.
///
/// Returns `None` having already explained why, so callers stop rather than
/// send into a situation whose answer they could not interpret.
fn approach_talker(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    // Written back: this function walks, and replicated state holds our login
    // position forever. Handing the next caller a stale one re-arms a trap
    // three separate callers have already hit.
    here: &mut world::Position,
    prefer: Option<u32>,
) -> Result<Option<Talker>> {
    // **Two distances, and collapsing them into one was a real bug.**
    //
    // `INTERACT_RANGE` is how far the server will talk from -- a server-side
    // constant this client cannot observe, so it is used only to decide
    // whether sending is worth it and never to explain a refusal.
    // `APPROACH_TO` is where the walk aims, and it has to be *comfortably
    // inside* that.
    //
    // The first version used one number for both, so the approach closed
    // `distance - reach` and arrived at exactly the edge. An NPC 4.04 units
    // away with a reach of 4.0 then produced three rounds of "closing 0.0
    // units" and a refusal to send -- a loop asymptotically approaching the
    // threshold it was waiting to cross. Walking past the line rather than up
    // to it is the whole fix, and it is the same shape as any controller that
    // must not settle on its own set point.
    const INTERACT_RANGE: f32 = 5.0;
    const APPROACH_TO: f32 = 3.0;
    // Below this, a walk is not worth sending: the server rounds, creatures
    // drift, and a stream of sub-metre corrections is indistinguishable from
    // the stall above.
    const WORTH_WALKING: f32 = 0.5;
    const RUN_SPEED: f32 = 7.0;
    // Same reason as `--attack`'s: an NPC that wanders is not where the walk
    // aimed by the time the walk ends.
    const APPROACHES: usize = 3;

    // Every unit that will talk, not the nearest unit of any kind. Choosing by
    // proximity is what makes a silence ambiguous: the nearest thing to a
    // starting character is usually a wolf, and a wolf has nothing to say.
    let mut talkers: Vec<(u64, u32, u32, f32)> = state
        .iter()
        .filter(|entity| entity.guid != own_guid)
        .filter(|entity| is_a_legal_test_target(entity))
        .filter(|entity| entity.will_talk())
        .filter_map(|entity| {
            let there = entity.position?;
            let d = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
            let entry = entity
                .fields
                .get(world::update::fields::OBJECT_ENTRY)
                .unwrap_or(0);
            Some((entity.guid, entry, entity.npc_flags().unwrap_or(0), d))
        })
        .collect();
    talkers.sort_by(|a, b| a.3.total_cmp(&b.3));

    println!(
        "\n{} replicated unit(s) with any UNIT_NPC_FLAGS bit set:",
        talkers.len()
    );
    for (guid, entry, flags, distance) in &talkers {
        println!(
            "  {guid:#018x}  entry {entry:>6}  flags {flags:>10} ({flags:#x})  {distance:>6.1} units"
        );
    }
    if talkers.is_empty() {
        println!("  none. Every unit in range reads UNIT_NPC_FLAGS as 0, which is");
        println!("  what a field of wolves looks like -- it is not a replication");
        println!("  failure. Put one within reach: --say \".npc add 295\" spawns an");
        println!("  Innkeeper Farley, whose npcflag is 66179.");
        return Ok(None);
    }

    // A requested entry that is not there is refused rather than silently
    // falling back to the nearest: greeting the wrong NPC and reading its
    // answer as the requested one's is precisely how a flag bit would get
    // named wrongly, and the printout would look completely normal.
    let Some(&(mut chosen, ..)) = (match prefer {
        Some(wanted) => talkers.iter().find(|(_, entry, _, _)| *entry == wanted),
        None => talkers.first(),
    }) else {
        println!("\nno replicated talker has entry {:?}.", prefer);
        println!("Spawn one: --say \".npc add {}\"", prefer.unwrap_or(0));
        return Ok(None);
    };

    // Walk to it. The distance is re-measured each time round rather than
    // assumed from the first reading, and the *nearest talker* is re-chosen
    // with it -- an NPC that has wandered off is not the one to keep chasing.
    //
    // `None` rather than a sentinel distance: an NPC that leaves replicated
    // state mid-approach is a different outcome from one that is merely too
    // far, and giving it a very large number would report it as "3.4e38 units
    // away", which is a printout that explains nothing.
    let mut distance = None;
    for attempt in 0..APPROACHES {
        let Some(there) = state.get(chosen).and_then(|entity| entity.position) else {
            break;
        };
        let d = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
        distance = Some(d);
        // Already close enough to be answered: do not walk at all. This is
        // the test the *send* depends on, and it is deliberately the server's
        // range rather than the walk's target.
        if d <= INTERACT_RANGE {
            break;
        }
        let heading = (there.y - here.y).atan2(there.x - here.x);
        // Aim past the threshold, not at it -- see APPROACH_TO.
        let close = d - APPROACH_TO;
        if close < WORTH_WALKING {
            break;
        }
        println!(
            "\n  approach {}: closing {close:.1} units on {chosen:#x}",
            attempt + 1
        );
        let (arrived, _) = connection.walk(own_guid, *here, heading, close, RUN_SPEED)?;
        *here = arrived;
        here.orientation = heading;
        let batch = connection.drain(std::time::Duration::from_millis(400), 128)?;
        state.replicate(&batch, None);
        // Re-pick, in case something closer will talk now that we have moved --
        // but **only when no entry was asked for**. Re-picking under a
        // preference would quietly greet a different creature than the one
        // named, and the printout would look entirely normal while attributing
        // one NPC's answer to another.
        if prefer.is_none() {
            if let Some((nearer, nearer_d)) = nearest_talker_from(state, own_guid, *here) {
                if nearer_d + 1.0 < d {
                    chosen = nearer;
                }
            }
        }
    }

    let Some(distance) = distance.filter(|d| *d <= INTERACT_RANGE) else {
        // Refused loudly rather than attempted. A greeting sent from here
        // would be declined for range, and that decline is a silence -- which
        // is indistinguishable from the opcode being wrong. Better to send
        // nothing than to collect an answer that cannot be interpreted.
        match distance {
            Some(d) => {
                println!("\n{chosen:#018x} is still {d:.1} units away after {APPROACHES} approach(es),");
                println!("past the {INTERACT_RANGE:.0}-unit reach. Not sending: a refusal at this range");
                println!("would be indistinguishable from an opcode the server ignored.");
            }
            None => {
                println!("\n{chosen:#018x} left replicated state during the approach --");
                println!("despawned, or moved out of visibility. Nothing was sent.");
            }
        }
        return Ok(None);
    };

    // **Re-read the entry and the flags from whoever is actually about to be
    // greeted**, rather than carrying the pair captured at selection time. The
    // approach loop may have switched targets, and a printout that labelled
    // one NPC's answer with another NPC's entry would look completely normal
    // while attributing a menu to the wrong creature -- which is the single
    // way this command could produce a confidently wrong flag-bit finding.
    let (entry, flags) = match state.get(chosen) {
        Some(npc) => (
            npc.fields
                .get(world::update::fields::OBJECT_ENTRY)
                .unwrap_or(0),
            npc.npc_flags().unwrap_or(0),
        ),
        None => (0, 0),
    };

    // **A dead NPC answers nothing, and nothing is the one reply that means
    // nothing.** `Entity::will_talk` reads only the flag word, so a corpse
    // keeps every bit it had in life and is selected exactly as readily as
    // the living -- and the server then declines the greeting in total
    // silence, which is what a wrong opcode looks like.
    //
    // This cost two runs. A trainer spawned at a character's feet in
    // Northshire was killed by the wildlife between one run and the next; the
    // same request that had come back with 286 bytes came back with nothing
    // twice, from the same character at the same distance to the same
    // creature entry, and the printout said only that no reply arrived. Said
    // rather than refused, deliberately: a greeting to a corpse is still an
    // observation worth having as long as the report names what it is.
    if state
        .get(chosen)
        .is_some_and(|npc| npc.is_dead_or_ghost())
    {
        println!("\n  WARNING: {chosen:#018x} is DEAD. Its flag word is unchanged -- a corpse");
        println!("  keeps every npcflag bit it had alive -- but the server declines to");
        println!("  interact with it, in silence. Any silence below is explained by this");
        println!("  and says nothing about the opcode. Respawn it with `.npc add {entry}`.");
    }

    Ok(Some(Talker {
        guid: chosen,
        entry,
        flags,
        distance,
    }))
}

/// An NPC that will talk, as [`approach_talker`] left it: within range, its
/// entry and flags re-read *after* the walk rather than captured before it.
struct Talker {
    guid: u64,
    /// `creature_template` entry. Re-read at the end of the approach, because
    /// the loop may have switched targets -- labelling one NPC's answer with
    /// another's entry is the single way these surveys could produce a
    /// confidently wrong finding.
    entry: u32,
    flags: u32,
    distance: f32,
}

/// Greets the nearest NPC that will talk, and reports everything that arrives.
///
/// **A survey, and deliberately parses nothing.** Nothing in `crates/world`
/// understands a gossip packet yet, and this is how that stops being true: the
/// reply is printed as an opcode and its bytes, in full. Printing a length
/// instead is how this project once saw and lost the one packet that could
/// have settled `SMSG_ATTACKERSTATEUPDATE`.
///
/// **The reason gossip is the right request to attempt first** is that it is
/// *answered*. Nothing acknowledges an opcode as such, and an outgoing number
/// that is wrong is read as some other valid request rather than refused -- so
/// `CMSG_AUTOEQUIP_ITEM` had to be confirmed by watching a field move. A reply
/// arriving here at all says the number was understood, and a reply that
/// parses says the layout was right too.
///
/// **The whole design of this command is about keeping the silences apart.**
/// A greeting that produces nothing is equally what a wrong opcode, a unit
/// with no gossip bit, and a unit out of range look like -- three different
/// investigations behind one printout, which is the shape that cost `--loot`
/// three runs. So the target is chosen by [`Entity::will_talk`] rather than by
/// proximity, its flags are printed before the send, the approach walk is
/// reported, and a target still out of reach is refused *loudly* instead of
/// being greeted anyway.
fn survey_gossip(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    // Taken by reference and **written back**, unlike `survey_loot`'s copy.
    // This function walks, and the fact that replicated state holds our login
    // position forever has now been rediscovered by three separate callers --
    // so the walked position is threaded through rather than looked up, and
    // handing the next caller a stale one would just re-arm the same trap.
    here: &mut world::Position,
    // Which creature entry to greet, or `None` for whichever talker is
    // nearest. `.npc add` spawns everything at the caller's feet, so a run
    // that has put two NPCs down has no meaningful "nearest" -- and comparing
    // what *different* NPCs answer is the whole method for naming the flag
    // bits, since nothing else can distinguish them.
    prefer: Option<u32>,
    // Which menu option to choose after greeting, by the **server's** option
    // id as printed in the reply -- not a row number. `None` greets and stops.
    select: Option<u32>,
    // Which vendor slot to buy, once a stock list has arrived. Again the
    // server's own slot and not a row position.
    buy: Option<u32>,
    // Whether to sell the thing just bought straight back, which is what makes
    // the run self-cleaning and tests both directions against one item.
    sell_back: bool,
) -> Result<()> {
    let Some(npc) = approach_talker(connection, state, own_guid, here, prefer)? else {
        return Ok(());
    };
    let (chosen, entry, flags, distance) = (npc.guid, npc.entry, npc.flags, npc.distance);

    // Selected first, for the same reason `--attack` does it: the server
    // decides what a request may act on, and it decides that about the
    // selected target. It is also what a real client does on a click.
    connection.set_selection(chosen)?;
    println!(
        "\ngreeting {chosen:#018x} entry {entry} at {distance:.1} units, npcflag {flags} ({flags:#x})"
    );
    connection.gossip_hello(chosen)?;

    // Give the server time to answer before concluding it ignored us. A packet
    // sent immediately before a disconnect is often never processed at all,
    // and that has been mistaken for a wrong opcode here before.
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;

    let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
    for packet in &batch {
        *seen.entry(packet.opcode).or_default() += 1;
    }

    // The same noise list `survey_loot` uses. Nothing is hidden -- the
    // histogram below lists every opcode that arrived, and only the bodies of
    // the constant traffic are filtered out so the answer is not buried.
    const NOISE: [u16; 6] = [0x00DD, 0x00A9, 0x01F6, 0x0390, 0x0085, 0x0086];
    println!("\nevery packet that came back, routine traffic aside:");
    let mut shown = 0;
    for packet in &batch {
        if NOISE.contains(&packet.opcode) {
            continue;
        }
        println!(
            "  {} ({:#06x}), {} bytes",
            world::opcode::describe(packet.opcode),
            packet.opcode,
            packet.body.len()
        );
        // Generously long. A gossip menu is text and several options, so a
        // preview cut to 128 bytes would show the header and hide every
        // option -- and the options are the part nothing else can supply.
        println!("    {}", hex_preview(&packet.body, 512));
        shown += 1;
        if shown >= 24 {
            println!("  ... stopping after {shown}");
            break;
        }
    }
    if shown == 0 {
        println!("  none -- nothing but the usual traffic came back.");
        println!("  Three things look exactly like this and want opposite");
        println!("  investigations: the opcode was not understood, this NPC's");
        println!("  flags do not include whatever bit gates a greeting, or the");
        println!("  server declined for a reason it does not report. The flags");
        println!("  above are the first thing to check against creature_template.");
    }

    // Now that the layout is known, say what the NPC actually offered -- while
    // still printing the raw bytes above, because the moment this stops
    // dumping bodies is the moment the next unknown shape becomes invisible.
    for packet in &batch {
        if packet.opcode != world::opcode::server::GOSSIP_MESSAGE {
            continue;
        }
        match world::gossip::parse_gossip_message(&packet.body) {
            Ok(menu) => {
                println!(
                    "\nmenu {} (greeting text {}), {} option(s), {} quest(s):",
                    menu.menu_id,
                    menu.text_id,
                    menu.options.len(),
                    menu.quests.len()
                );
                for option in &menu.options {
                    // The index is printed first and labelled, because it is
                    // the number a reply has to carry and it is *not* the row
                    // position -- a filtered menu leaves holes in it.
                    println!(
                        "  option {:>2} (icon {}, coded {}, {} copper): {:?}{}",
                        option.index,
                        option.icon,
                        option.coded,
                        option.money,
                        option.message,
                        if option.box_message.is_empty() {
                            String::new()
                        } else {
                            format!("  box {:?}", option.box_message)
                        }
                    );
                }
                for quest in &menu.quests {
                    println!(
                        "  quest {:>6} (icon {}, level {}, flags {:#x}, repeatable {}): {:?}",
                        quest.quest_id,
                        quest.icon,
                        quest.level,
                        quest.flags,
                        quest.repeatable,
                        quest.title
                    );
                }
                if menu.is_empty() {
                    println!("  nothing but greeting text -- a real reply, not an empty one.");
                }
                // The check worth making every run, and it needs the database
                // rather than the packet: these numbers are the server's own
                // and are what identified the header in the first place.
                println!(
                    "  cross-check: SELECT gossip_menu_id FROM creature_template WHERE entry={entry};"
                );
                println!("               SELECT TextID FROM gossip_menu WHERE MenuID={};", menu.menu_id);
            }
            // Printed rather than swallowed: an unparseable menu is the single
            // most interesting thing this command can produce.
            Err(error) => println!("\nSMSG_GOSSIP_MESSAGE did not parse: {error}"),
        }
    }

    println!("\nevery opcode seen:");
    for (opcode, count) in &seen {
        println!(
            "  {:<34} ({opcode:#06x}) x{count}",
            world::opcode::describe(*opcode)
        );
    }

    state.replicate(&batch, None);

    // **Then choose a line, if one was asked for.**
    //
    // The menu id and the option index both come out of the reply just
    // parsed rather than from anything the caller typed about the menu --
    // the caller names an option and this looks it up. That is not
    // convenience: an option index is the *server's* id and a filtered menu
    // leaves holes in the numbering, so a number invented here would ask for
    // a different line than the one printed above, and the printout would
    // look right.
    if let Some(wanted) = select {
        let Some(menu) = batch
            .iter()
            .filter(|p| p.opcode == world::opcode::server::GOSSIP_MESSAGE)
            .find_map(|p| world::gossip::parse_gossip_message(&p.body).ok())
        else {
            println!("\nnothing to select from -- no menu came back.");
            return Ok(());
        };

        let Some(option) = menu.options.iter().find(|o| o.index == wanted) else {
            println!("\nthis menu has no option {wanted}. It offers: {:?}",
                menu.options.iter().map(|o| o.index).collect::<Vec<_>>());
            println!("Those are the server's own ids, not row numbers -- see");
            println!("ClientOpcode::GossipSelectOption for why that distinction bites.");
            return Ok(());
        };

        println!("\nchoosing option {} of menu {}: {:?}", option.index, menu.menu_id, option.message);
        if option.coded != 0 {
            println!("  note: this option is *coded* -- the original client would");
            println!("  open a text box for it, and this sends an empty string.");
        }
        connection.gossip_select(chosen, menu.menu_id, option.index)?;
        let after = connection.drain(std::time::Duration::from_millis(2000), 128)?;

        println!("\nbodies of everything unexpected after the choice:");
        let mut shown = 0;
        for packet in &after {
            if NOISE.contains(&packet.opcode) {
                continue;
            }
            println!(
                "  {} ({:#06x}), {} bytes",
                world::opcode::describe(packet.opcode),
                packet.opcode,
                packet.body.len()
            );
            println!("    {}", hex_preview(&packet.body, 512));
            shown += 1;
            if shown >= 24 {
                println!("  ... stopping after {shown}");
                break;
            }
        }
        if shown == 0 {
            println!("  none. A choice that is understood does *something* --");
            println!("  a new menu, a stock list, a quest. Silence here is the");
            println!("  same three-way ambiguity the greeting had: wrong opcode,");
            println!("  wrong body, or an option the server declined.");
        }

        // Now that the layout is known, say what the vendor actually stocks --
        // while still dumping the raw body above, for the usual reason.
        for packet in &after {
            if packet.opcode != world::opcode::server::LIST_INVENTORY {
                continue;
            }
            match world::vendor::parse_vendor_list(&packet.body) {
                Ok(list) => {
                    println!("\nvendor {:#018x} stocks {} item(s):", list.vendor, list.items.len());
                    for item in &list.items {
                        println!(
                            "  slot {:>2}: entry {:>6} (display {:>6}) {:>8} copper, {} per buy, stock {}{}",
                            item.slot,
                            item.entry,
                            item.display_id,
                            item.price,
                            item.buy_count,
                            match item.remaining {
                                Some(left) => left.to_string(),
                                None => "unlimited".into(),
                            },
                            match item.extended_cost {
                                Some(row) => format!(", extended cost {row}"),
                                None => String::new(),
                            },
                        );
                    }
                    // **The price is the discounted one and the table's is
                    // not**, so the cross-check has to be stated as the
                    // relationship rather than as equality -- somebody
                    // comparing these to `BuyPrice` and finding them lower
                    // should find the explanation here rather than file a bug.
                    println!("  cross-check: SELECT item FROM npc_vendor WHERE entry={entry} ORDER BY slot;");
                    println!("               SELECT entry,displayid,BuyPrice FROM item_template WHERE entry IN (...);");
                    println!("  note: prices above are AFTER the buyer's reputation discount,");
                    println!("        so they are *below* item_template.BuyPrice. That is correct.");

                    if let Some(wanted) = buy {
                        trade_and_report(
                            connection, state, own_guid, &list, wanted, sell_back,
                        )?;
                    }
                }
                Err(error) => println!("\nSMSG_LIST_INVENTORY did not parse: {error}"),
            }
        }

        println!("\nevery opcode seen after the choice:");
        let mut seen_after: std::collections::BTreeMap<u16, usize> = Default::default();
        for packet in &after {
            *seen_after.entry(packet.opcode).or_default() += 1;
        }
        for (opcode, count) in &seen_after {
            println!(
                "  {:<34} ({opcode:#06x}) x{count}",
                world::opcode::describe(*opcode)
            );
        }
        state.replicate(&after, None);
    }
    Ok(())
}

/// Asks a trainer what it teaches, measures the record stride against the
/// reply, and optionally learns one spell.
///
/// **The stride measurement is the reason this prints more than a list.** Two
/// record layouts are plausible for `SMSG_TRAINER_LIST` -- 30 bytes and 38 --
/// and both parse a real body without complaining, because every field in the
/// record is a small integer that reads as a plausible spell id, price or
/// level at either. So the probe reads the bytes *itself*, without going
/// through [`world::trainer::parse_trainer_list`], for the same reason
/// `m2 events --strides` does: the parser is what is under test.
///
/// What separates them is the **greeting** at the end of the body. A stride
/// out by a word leaves the reader inside a number, and the bytes of a small
/// integer are not printable. Same evidence as the M2 event identifier being
/// four printable characters, and stronger than any amount of range-checking:
/// a name cannot be arrived at by a coincidence of small integers.
/// Collects until one of `wanted` arrives or the clock runs out.
///
/// **A packet limit does not bound time, and this probe proved it again.**
/// The first version of `survey_auction` drained with `(2s quiet, 256
/// packets)`, which is the shape the login burst once used: Elwynn never goes
/// quiet for two seconds and 256 packets at a few per second is minutes. It
/// looked exactly like a hang -- no output, no CPU -- and "a stuck run and a
/// slow one look identical" is precisely why this prints a line per round.
///
/// So: a wall clock, short slices, an early exit the moment the answer is
/// here, and something printed every round. That is the rule this project
/// wrote after the 37-second login and then walked into twice more.
fn await_reply(
    connection: &mut world::Connection,
    seconds: u64,
    wanted: &[u16],
    what: &str,
) -> Result<Vec<world::client::Packet>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut collected: Vec<world::client::Packet> = Vec::new();
    let mut round = 0usize;
    while std::time::Instant::now() < deadline {
        round += 1;
        let batch = connection.drain(std::time::Duration::from_millis(250), 64)?;
        let new = batch.len();
        collected.extend(batch);
        if collected.iter().any(|p| wanted.contains(&p.opcode)) {
            println!("  ({what}: answered on round {round}, {} packets seen)", collected.len());
            return Ok(collected);
        }
        // Every round says something, so a slow wait and a stuck one are
        // distinguishable without waiting for the end of either.
        if round % 4 == 0 {
            println!("  ({what}: {round} rounds, {} packets, {new} last round, still waiting)",
                collected.len());
        }
    }
    println!("  ({what}: gave up after {seconds}s and {} packets)", collected.len());
    Ok(collected)
}

/// What `--auction` needs beyond a connection.
struct AuctionProbe<'a> {
    /// Which auctioneer to walk to, by `creature_template` entry.
    prefer: Option<u32>,
    search: Option<&'a str>,
    offset: u32,
    sell: Option<u32>,
    bid: u32,
    buyout: u32,
    place: Option<u32>,
    id: Option<u32>,
    cancel: bool,
}

/// Probes the auction house, and **measures the record stride** rather than
/// trusting `world::auction`'s.
///
/// The order of the run is the point. Nine of the block's ten requests are
/// dropped in silence when the auctioneer does not resolve, and a silent send
/// is indistinguishable from a wrong opcode -- the failure this project walked
/// into in the vendor block, the party block, the trainer block and all ten of
/// trade's opcodes. So the **first** thing sent is
/// `CMSG_AUCTION_LIST_PENDING_SALES`, from wherever the character happens to
/// be standing, before any NPC has been found: its handler checks nothing at
/// all -- not the auctioneer, not the range, not the level, not the body it
/// just read -- and always answers. If that reply arrives, the socket and this
/// half of the opcode table are proven, and every silence after it is a fact
/// about the request rather than about the client.
///
/// It is a stronger bound than any other city service got. `CMSG_GUILD_ROSTER`
/// is answered without a guild but still describes state; this one's answer is
/// a fixed four bytes of zero, because the server's own loop over the records
/// is commented out. **A reply that cannot vary cannot be mistaken for a reply
/// that varied.**
fn survey_auction(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    // Written back, like every other walking probe here: replicated state
    // holds our login position forever.
    here: &mut world::Position,
    probe: AuctionProbe<'_>,
) -> Result<()> {
    use world::auction;

    // ---- the bound, before anything else ----------------------------------
    println!("\n== the bound: CMSG_AUCTION_LIST_PENDING_SALES, with no auctioneer ==");
    println!("Sent from where the character is standing, naming nothing. This handler");
    println!("checks nothing and always answers, so what comes back separates \"the");
    println!("opcode block is wrong\" from \"the request was declined\".");
    connection.auction_list_pending_sales(0)?;
    let batch = await_reply(
        connection,
        6,
        &[world::opcode::server::AUCTION_LIST_PENDING_SALES],
        "the bound",
    )?;
    match batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::AUCTION_LIST_PENDING_SALES)
    {
        Some(reply) => {
            println!(
                "  SMSG_AUCTION_LIST_PENDING_SALES came back, {} bytes: {}",
                reply.body.len(),
                hex_preview(&reply.body, 32)
            );
            match auction::parse_pending_sales(&reply.body) {
                Ok(count) => println!("  parses, {count} records. The block is reachable."),
                Err(error) => println!("  did NOT parse: {error}"),
            }
        }
        None => {
            println!("  NOTHING came back. Everything that did arrive:");
            dump_unexpected(&batch, "the pending-sales bound");
            println!("  This is the informative failure: the one request in the block that");
            println!("  cannot be declined was not answered, so the opcode number or the");
            println!("  socket is wrong and nothing below is worth reading.");
        }
    }
    state.replicate(&batch, None);

    // ---- find an auctioneer ----------------------------------------------
    let Some(npc) = approach_talker(connection, state, own_guid, here, probe.prefer)? else {
        return Ok(());
    };
    let (chosen, entry, flags, distance) = (npc.guid, npc.entry, npc.flags, npc.distance);
    connection.set_selection(chosen)?;
    println!(
        "\nasking {chosen:#018x} entry {entry} at {distance:.1} units, npcflag {flags} ({flags:#x})"
    );

    // Say what the flag word claims before sending, so a refusal has somewhere
    // to be explained -- the same move `--trainer` makes. The bit is a
    // hypothesis from the server's own header and is confirmed here by
    // behaviour: a unit carrying it answers the greeting and a unit without it
    // does not.
    const AUCTIONEER_BIT: u32 = 0x0020_0000;
    if flags & AUCTIONEER_BIT == 0 {
        println!("  note: bit 0x200000 is NOT set on this unit, so the server will refuse.");
        println!("  Sending anyway: a silence from a unit that admits it is not an");
        println!("  auctioneer is a *different* observation from silence at one that is,");
        println!("  and only the pair says the bit means what it claims.");
    }

    // ---- the greeting, and the only packet that names a house -------------
    println!("\n== MSG_AUCTION_HELLO ==");
    connection.auction_hello(chosen)?;
    let batch = await_reply(connection, 8, &[world::opcode::server::AUCTION_HELLO], "the greeting")?;
    match batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::AUCTION_HELLO)
    {
        Some(reply) => {
            println!("  {} bytes: {}", reply.body.len(), hex_preview(&reply.body, 32));
            match auction::parse_auction_hello(&reply.body) {
                Ok(house) => println!(
                    "  house {} , enabled {} , auctioneer {:#018x}",
                    house.house, house.enabled, house.auctioneer
                ),
                Err(error) => println!("  did NOT parse: {error}"),
            }
        }
        None => {
            println!("  no MSG_AUCTION_HELLO. Everything that arrived:");
            dump_unexpected(&batch, "the auction greeting");
        }
    }
    state.replicate(&batch, None);

    // ---- post, before listing, so the list has something in it ------------
    if let Some(wanted) = probe.sell {
        post_auctions(
            connection,
            state,
            own_guid,
            chosen,
            wanted,
            probe.bid,
            probe.buyout,
        )?;
    }

    if let Some(auction_id) = probe.place {
        let target = probe.id.unwrap_or(0);
        println!("\n== CMSG_AUCTION_PLACE_BID: {auction_id} copper on auction {target} ==");
        println!("A price equal to the buyout *is* the buyout -- one opcode, two buttons.");
        if !connection.auction_place_bid(chosen, target, auction_id)? {
            println!("  refused here rather than in silence: a zero id or a zero price is");
            println!("  dropped by the server without a word.");
        } else {
            let batch = await_reply(
                connection,
                8,
                &[world::opcode::server::AUCTION_COMMAND_RESULT],
                "the bid",
            )?;
            report_auction_results(&batch);
            state.replicate(&batch, None);
        }
    }

    if probe.cancel {
        let target = probe.id.unwrap_or(0);
        println!("\n== CMSG_AUCTION_REMOVE_ITEM: auction {target} ==");
        println!("The goods come back as *mail*, not to the bag. --mail-list confirms it.");
        connection.auction_remove_item(chosen, target)?;
        let batch = await_reply(
            connection,
            8,
            &[world::opcode::server::AUCTION_COMMAND_RESULT],
            "the cancellation",
        )?;
        report_auction_results(&batch);
        state.replicate(&batch, None);
    }

    // ---- the owner list: the cheapest check on the footer -----------------
    println!("\n== CMSG_AUCTION_LIST_OWNER_ITEMS ==");
    println!("This list never pages, so its total must equal its count. A body whose");
    println!("two numbers disagree says the footer is not where the parser thinks.");
    connection.auction_list_owner_items(chosen)?;
    let batch = await_reply(
        connection,
        10,
        &[world::opcode::server::AUCTION_OWNER_LIST_RESULT],
        "the owner list",
    )?;
    let owner = batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::AUCTION_OWNER_LIST_RESULT)
        .map(|p| p.body.clone());
    match &owner {
        Some(body) => report_auction_page(body, 0, "SMSG_AUCTION_OWNER_LIST_RESULT"),
        None => {
            println!("  no owner list came back. Everything that arrived:");
            dump_unexpected(&batch, "the owner list");
        }
    }
    state.replicate(&batch, None);

    // ---- the search --------------------------------------------------------
    let mut search = match probe.search {
        Some(name) => auction::AuctionSearch::named(name),
        None => auction::AuctionSearch::any(),
    };
    // Sorted by nothing on purpose. The server only *applies* a sort block
    // when the match exceeds one page, so asking for one over a small fixture
    // would produce a result indistinguishable from asking for none -- and a
    // test that cannot come out the other way is not evidence.
    search.sort.clear();

    println!(
        "\n== CMSG_AUCTION_LIST_ITEMS, from row {} , name {:?} ==",
        probe.offset,
        probe.search.unwrap_or("")
    );
    // The pairing this block exists to exercise: the offset goes out on the
    // wire and is *also* told to the state, because the reply does not carry
    // it. Adjacent on purpose.
    state.expect_auction_page(probe.offset);
    connection.auction_list_items(chosen, probe.offset, &search)?;
    let batch = await_reply(
        connection,
        10,
        &[world::opcode::server::AUCTION_LIST_RESULT],
        "the search",
    )?;
    let first = batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::AUCTION_LIST_RESULT)
        .map(|p| p.body.clone());
    match &first {
        Some(body) => report_auction_page(body, probe.offset, "SMSG_AUCTION_LIST_RESULT"),
        None => {
            println!("  no search result came back. Everything that arrived:");
            dump_unexpected(&batch, "the auction search");
        }
    }
    state.replicate(&batch, None);
    if let Some(page) = &state.auctions {
        println!(
            "  state holds one page: offset {} , {} rows, {} matched, {} pages",
            page.offset,
            page.auctions.len(),
            page.total,
            page.pages()
        );
        if let Some(next) = page.next_offset() {
            println!("  the match does not fit in this packet; next row is {next}");
        }
    }

    // ---- the measurement --------------------------------------------------
    //
    // **Two packets with different counts is the whole measurement.** With a
    // fixed-width record and a fixed footer, `len = 4 + count * stride + 8`,
    // so the difference of two lengths over the difference of two counts is
    // the stride and neither the header nor the footer appears in it. One
    // packet cannot do this: it can only say which candidate accounts for the
    // body, and that answer is only unique because the footer's width is
    // already assumed.
    println!("\n== the record stride, measured ==");
    let counted = |body: &Vec<u8>| {
        u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize
    };
    match (&owner, &first) {
        (Some(a), Some(b)) if a.len() >= 4 && b.len() >= 4 => {
            let (ca, cb) = (counted(a), counted(b));
            println!("  owner list: {ca} rows in {} bytes", a.len());
            println!("  search:     {cb} rows in {} bytes", b.len());
            match auction::stride_between((a.len(), ca as u32), (b.len(), cb as u32)) {
                Some(stride) => {
                    println!("  two counts differ by {}, two lengths by {} -> stride {stride}",
                        (cb as i64 - ca as i64).abs(),
                        (b.len() as i64 - a.len() as i64).abs());
                    println!(
                        "  world::auction::RECORD_BYTES is {} -- {}",
                        auction::RECORD_BYTES,
                        if stride == auction::RECORD_BYTES { "AGREES" } else { "DISAGREES" }
                    );
                }
                None => {
                    println!("  the two packets carry the SAME number of rows, so they cannot");
                    println!("  separate a stride from a header. That is the honest answer --");
                    println!("  post or cancel an auction and run it again.");
                }
            }
        }
        _ => println!("  need both an owner list and a search result to measure."),
    }
    for body in [&owner, &first].into_iter().flatten() {
        let fits = auction::measure_stride(
            body,
            &[
                auction::RECORD_BYTES - 8,
                auction::RECORD_BYTES - 4,
                auction::RECORD_BYTES,
                auction::RECORD_BYTES + 4,
            ],
        );
        println!("  single-packet scoring of a {}-byte body:", body.len());
        for fit in &fits {
            println!(
                "    {:>4} bytes: {} body, total {:?} {}, delay {:?}",
                fit.stride,
                if fit.accounts_for_body { "accounts for" } else { "does NOT account for" },
                fit.total,
                if fit.total_is_possible { "possible" } else { "IMPOSSIBLE" },
                fit.delay,
            );
        }
    }

    Ok(())
}

/// Posts one auction per matching stack in the bags.
///
/// **One auction per stack, not one per item**, which is what makes a fixture
/// big enough to page: a page is fifty rows and nothing smaller can show a
/// total that exceeds a count. The server merges every stack named in a single
/// request into one auction, so several requests are the only way.
fn post_auctions(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    auctioneer: u64,
    entry: u32,
    bid: u32,
    buyout: u32,
) -> Result<()> {
    use world::auction::AuctionDuration;

    let stacks: Vec<(u64, u32)> = world::inventory::carried(state, own_guid)
        .iter()
        .filter(|c| c.item.entry == Some(entry))
        .map(|c| (c.item.guid, c.item.count.max(1)))
        .collect();

    println!("\n== CMSG_AUCTION_SELL_ITEM: {} stacks of entry {entry} ==", stacks.len());
    if stacks.is_empty() {
        println!("  nothing in the bags matches. `.additem {entry} <n>` first -- and note");
        println!("  that `.additem` needs `.save`, because a session that ends by closing");
        println!("  the socket never writes the inventory back.");
        return Ok(());
    }

    let mut posted = 0usize;
    let mut refused = 0usize;
    for (guid, count) in &stacks {
        if !connection.auction_sell_item(
            auctioneer,
            &[(*guid, *count)],
            bid,
            buyout,
            AuctionDuration::TwelveHours,
        )? {
            refused += 1;
            continue;
        }
        posted += 1;
        // The server states its own search delay in every list result; a burst
        // of posts has no such number, and a party loot control clicked
        // repeatedly once closed the socket outright. Paced deliberately.
        let batch = await_reply(
            connection,
            4,
            &[world::opcode::server::AUCTION_COMMAND_RESULT],
            "the post",
        )?;
        report_auction_results(&batch);
        state.replicate(&batch, None);
    }
    // Both numbers, always: a counter that only speaks on failure cannot tell
    // "none were wrong" from "there were none".
    println!("  {posted} sent, {refused} refused before sending");
    Ok(())
}

/// Prints every `SMSG_AUCTION_COMMAND_RESULT` in a batch, and says so when
/// there are none.
fn report_auction_results(batch: &[world::client::Packet]) {
    use world::auction;

    let mut seen = 0usize;
    for packet in batch {
        if packet.opcode != world::opcode::server::AUCTION_COMMAND_RESULT {
            continue;
        }
        seen += 1;
        match auction::parse_command_result(&packet.body) {
            Ok(outcome) => println!(
                "  {} auction {}: {} (error {}, bid error {:?})",
                outcome.action.label(),
                outcome.auction,
                auction::describe_auction_error(outcome.error),
                outcome.error,
                outcome.bid_error,
            ),
            Err(error) => println!(
                "  SMSG_AUCTION_COMMAND_RESULT, {} bytes, did NOT parse: {error} -- {}",
                packet.body.len(),
                hex_preview(&packet.body, 32)
            ),
        }
    }
    if seen == 0 {
        println!("  no SMSG_AUCTION_COMMAND_RESULT. Everything that arrived:");
        dump_unexpected(batch, "an auction command");
    }
}

/// Prints one list result: its bytes, its two counts, and its rows.
fn report_auction_page(body: &[u8], offset: u32, what: &'static str) {
    use world::auction;

    // **The body, not the length.** A parser that declines an unconfirmed
    // shape is only useful if the shape survives the refusal.
    println!("  {what}, {} bytes:", body.len());
    println!("    {}", hex_preview(body, 320));
    match auction::parse_auction_page(body, offset, what) {
        Ok(page) => {
            println!(
                "    {} rows in this packet, {} matched in the house, delay {}ms",
                page.auctions.len(),
                page.total,
                page.search_delay_ms
            );
            if page.past_the_end() {
                // Printed as its own case rather than as a page number,
                // because the naive arithmetic says "page 3 of 1" here -- and
                // it printed exactly that before this branch existed.
                println!(
                    "    PAST THE END: row {} of a {}-row match, {} page(s)",
                    page.offset,
                    page.total,
                    page.pages()
                );
            } else if page.is_whole() {
                println!("    this packet holds the whole match");
            } else {
                println!(
                    "    THE PAGE IS NOT THE LIST: {} of {} , page {} of {}",
                    page.auctions.len(),
                    page.total,
                    page.offset / auction::PAGE_ROWS as u32 + 1,
                    page.pages()
                );
            }
            for a in page.auctions.iter().take(12) {
                println!(
                    "    #{:<6} item {:<6} x{:<3} start {:<8} bid {:<8} +{:<6} buyout {:<8} {:<9} owner {:#018x}",
                    a.id,
                    a.item,
                    a.count,
                    a.start_bid,
                    a.bid,
                    a.min_increment,
                    a.buyout,
                    a.band().label(),
                    a.owner,
                );
            }
            if page.auctions.len() > 12 {
                println!("    ... and {} more in this packet", page.auctions.len() - 12);
            }
        }
        Err(error) => println!("    did NOT parse: {error}"),
    }
}

fn survey_trainer(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    // Written back, like every other walking probe here: replicated state
    // holds our login position forever and three callers have now had to
    // relearn it.
    here: &mut world::Position,
    prefer: Option<u32>,
    // Which spell to learn afterwards, by **spell id** and never a row
    // position -- the list is filtered per character.
    learn: Option<u32>,
) -> Result<()> {
    use world::trainer;

    let Some(npc) = approach_talker(connection, state, own_guid, here, prefer)? else {
        return Ok(());
    };
    let (chosen, entry, flags, distance) = (npc.guid, npc.entry, npc.flags, npc.distance);

    connection.set_selection(chosen)?;
    println!(
        "\nasking {chosen:#018x} entry {entry} at {distance:.1} units, npcflag {flags} ({flags:#x})"
    );
    // **Say what the flag word claims before sending**, so a refusal has
    // somewhere to be explained. The bit map is a hypothesis from the
    // server's own header and is confirmed here by *combination*: an NPC
    // whose name says "Warrior Trainer" carries 0x10 and 0x20 and no vendor
    // bit, and an innkeeper carries the vendor and innkeeper bits and neither
    // trainer bit. Two flag words that differ, each agreeing with a role
    // known independently of the wire.
    const TRAINER_BIT: u32 = 0x10;
    if flags & TRAINER_BIT == 0 {
        println!("  note: bit 0x10 is NOT set on this unit, so the server will refuse.");
        println!("  Sending anyway, because a refusal from a unit that admits it is");
        println!("  not a trainer is a *different* observation from silence at one");
        println!("  that is -- and only the pair says the bit means what it claims.");
    }

    connection.trainer_list(chosen)?;

    // Generously long, and for the reason a packet sent immediately before a
    // disconnect is often never processed: half a second of waiting once
    // turned a facing opcode from "wrong" into "works every time".
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;

    let Some(reply) = batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::TRAINER_LIST)
    else {
        println!("\nNO SMSG_TRAINER_LIST came back. Everything that did arrive:");
        for packet in &batch {
            println!(
                "  {} ({:#06x}), {} bytes",
                world::opcode::describe(packet.opcode),
                packet.opcode,
                packet.body.len()
            );
        }
        println!("\nThis is the informative failure: an *answered* opcode that did not");
        println!("answer means the number is wrong, or this unit does not train. The");
        println!("npcflag printed above says which -- that is what it is printed for.");
        state.replicate(&batch, None);
        return Ok(());
    };

    // **The body, not the length.** A parser that declines an unconfirmed
    // shape is only useful if the shape survives the refusal, and two tools
    // in this tree have already logged a length and dropped the one packet
    // that could have answered the question.
    println!("\nSMSG_TRAINER_LIST, {} bytes:", reply.body.len());
    println!("  {}", hex_preview(&reply.body, 512));

    println!("\nrecord stride, measured against the greeting:");
    let fits = trainer::measure_stride(
        &reply.body,
        &[
            trainer::SHORT_SPELL_BYTES,
            trainer::SPELL_BYTES - 4,
            trainer::SPELL_BYTES,
            trainer::SPELL_BYTES + 4,
        ],
    );
    for fit in &fits {
        println!(
            "  {:>3} bytes: {} body, greeting {} {:?}",
            fit.stride,
            if fit.accounts_for_body { "accounts for" } else { "does NOT account for" },
            if fit.greeting_is_printable { "READABLE" } else { "binary" },
            fit.greeting.as_deref().unwrap_or("<none>"),
        );
    }
    let winners: Vec<usize> = fits
        .iter()
        .filter(|f| f.accounts_for_body && f.greeting_is_printable)
        .map(|f| f.stride)
        .collect();
    match winners.as_slice() {
        [only] => println!("  -> exactly one stride leaves a sentence where the greeting is: {only}"),
        [] => println!("  -> NONE fit. The header is not where this thinks it is."),
        many => println!("  -> {many:?} all fit, so this packet cannot separate them. Needs a\n     trainer with more rows, or one whose greeting is longer."),
    }

    match trainer::parse_trainer_list(&reply.body) {
        Ok(list) => {
            println!(
                "\ntrainer {:#018x}, kind {}, {} spell(s):",
                list.trainer,
                list.kind,
                list.spells.len()
            );
            println!("  greeting: {:?}", list.greeting);
            for spell in &list.spells {
                println!(
                    "  spell {:>6}  {:<12} {:>6} copper  level {:>2}{}{}",
                    spell.spell,
                    match spell.state {
                        trainer::TrainerSpellState::Available => "available".to_string(),
                        trainer::TrainerSpellState::Unavailable => "cannot yet".to_string(),
                        trainer::TrainerSpellState::Known => "known".to_string(),
                        trainer::TrainerSpellState::Unknown(n) => format!("UNKNOWN({n})"),
                    },
                    spell.cost,
                    spell.required_level,
                    match spell.required_skill {
                        Some(skill) => format!(", skill {skill} at {}", spell.required_skill_value),
                        None => String::new(),
                    },
                    if spell.required_spells.is_empty() {
                        String::new()
                    } else {
                        format!(", needs {:?}", spell.required_spells)
                    },
                );
            }

            // **The cross-check is a relationship, not an equality.** Somebody
            // comparing these prices to the table and finding them lower
            // should find the explanation here rather than file a bug -- the
            // same note the vendor probe carries, for the same reason.
            println!("\n  cross-check, against the server's own tables:");
            println!("    SELECT TrainerId FROM creature_default_trainer WHERE CreatureId={entry};");
            println!("    SELECT SpellId,MoneyCost,ReqLevel,ReqSkillLine,ReqSkillRank");
            println!("      FROM trainer_spell WHERE TrainerId=<that>;");
            println!("    SELECT Greeting FROM trainer WHERE Id=<that>;");
            println!("  note: costs above are AFTER the reader's reputation discount, so they");
            println!("        are *below* trainer_spell.MoneyCost. That is correct, and it is");
            println!("        the same finding the vendor price made -- the wire is");
            println!("        authoritative for price and the table is not.");

            // **The state byte is the one field a single packet can only
            // half-confirm**, so say what would refute it rather than
            // declaring it right. Availability depends on the reader's level,
            // and a list where every row agrees proves nothing.
            let levels: Vec<u8> = list.spells.iter().map(|s| s.required_level).collect();
            let mut distinct: Vec<world::TrainerSpellState> = Vec::new();
            for spell in &list.spells {
                if !distinct.contains(&spell.state) {
                    distinct.push(spell.state);
                }
            }
            println!("\n  state column: {} distinct value(s) over required levels {levels:?}", distinct.len());
            if distinct.len() < 2 {
                println!("    ONE value across every row, which cannot separate a state column");
                println!("    from a constant. Run again on a character whose level falls");
                println!("    *between* the required levels above -- that is the population");
                println!("    that can refute this, and a list that all agrees is not evidence.");
            } else {
                println!("    More than one, so this list can and does distinguish them. Check");
                println!("    the split falls exactly on the reader's level in the rows above.");
            }

            if let Some(wanted) = learn {
                learn_and_report(connection, state, own_guid, &list, wanted)?;
            }
        }
        Err(error) => println!("\nSMSG_TRAINER_LIST did not parse: {error}"),
    }

    println!("\nevery opcode seen:");
    let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
    for packet in &batch {
        *seen.entry(packet.opcode).or_default() += 1;
    }
    for (opcode, count) in &seen {
        println!(
            "  {:<34} ({opcode:#06x}) x{count}",
            world::opcode::describe(*opcode)
        );
    }
    state.replicate(&batch, None);
    Ok(())
}

/// Learns one spell from an open trainer and reports both effects.
///
/// **Two independent confirmations, and they fail differently**, which is why
/// both are checked rather than either: `SMSG_TRAINER_BUY_SUCCEEDED` echoes
/// the spell id back, and `PLAYER_FIELD_COINAGE` drops by the price the list
/// quoted. The reply says the request was understood; the money says the
/// *quoted* price was the real one, which is a statement about
/// [`world::trainer::TrainerSpell::cost`] rather than about the purchase.
fn learn_and_report(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    list: &world::TrainerList,
    wanted: u32,
) -> Result<()> {
    use world::inventory;

    // Looked up in the list rather than trusted from the command line, so the
    // state can be checked before a send that cannot report its own failure.
    let Some(spell) = list.spells.iter().find(|s| s.spell == wanted) else {
        println!("\nthis trainer does not teach spell {wanted}. It offers: {:?}",
            list.spells.iter().map(|s| s.spell).collect::<Vec<_>>());
        println!("Those are spell ids, not row numbers -- the list is filtered per");
        println!("character, so a row number means different things to two readers.");
        return Ok(());
    };

    // **Refuse locally rather than spending an unanswerable send.** Every
    // failure path in the server's handler returns without sending anything,
    // so a refusal and a wrong opcode look identical -- and this client has
    // already paid for that confusion once, on `CMSG_BUY_ITEM`.
    if !spell.state.is_learnable() {
        println!("\nspell {wanted} is not learnable by this character right now ({:?}).", spell.state);
        println!("Not sending: the server declines this in *silence*, which is");
        println!("indistinguishable from a wrong opcode, so the send would tell us");
        println!("nothing while looking exactly like a protocol failure.");
        return Ok(());
    }

    let money_before = inventory::coinage(state, own_guid);
    println!(
        "\nlearning spell {} at {} copper; purse holds {money_before}",
        spell.spell, spell.cost
    );
    connection.trainer_buy_spell(list.trainer, spell.spell)?;

    let after = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    state.replicate(&after, None);

    let echoed = after
        .iter()
        .find(|p| p.opcode == world::opcode::server::TRAINER_BUY_SUCCEEDED)
        .and_then(|p| {
            (p.body.len() >= 12).then(|| {
                u32::from_le_bytes([p.body[8], p.body[9], p.body[10], p.body[11]])
            })
        });
    match echoed {
        Some(id) if id == spell.spell => {
            println!("  SMSG_TRAINER_BUY_SUCCEEDED echoed spell {id} -- the same one asked for.")
        }
        Some(id) => println!("  SMSG_TRAINER_BUY_SUCCEEDED echoed spell {id}, NOT the {} sent.", spell.spell),
        None => {
            println!("  no SMSG_TRAINER_BUY_SUCCEEDED. The server declines in silence, so");
            println!("  this is either a refusal it did not explain or a request it did");
            println!("  not understand -- and CMSG_TRAINER_LIST answering above says the");
            println!("  opcode block is numbered right, which leaves the body.");
        }
    }

    let money_after = inventory::coinage(state, own_guid);
    let spent = money_before.saturating_sub(money_after);
    println!("  purse {money_before} -> {money_after} (spent {spent}, quoted {})", spell.cost);
    if spent == spell.cost && spell.cost > 0 {
        println!("  exactly the quoted price left the purse, so the cost field is the");
        println!("  discounted one the server actually charges and not the table's.");
    } else if money_after == money_before {
        println!("  nothing left the purse. Either the coinage field has not been");
        println!("  resent yet or nothing was bought -- the echo above says which.");
    }
    Ok(())
}

/// Buys one row from an open vendor, and optionally sells it straight back.
///
/// **Both writes are confirmed by effect, because nothing acknowledges
/// either.** The money moves and the slot array changes, and both of those are
/// already read -- so this snapshots them, sends, waits, and reports the
/// difference. A wrong opcode moves nothing, which is a different printout
/// rather than a similar one.
///
/// **The coinage delta is a check on more than the purchase.** The stock list
/// quotes the *discounted* price, so if the money that leaves the purse equals
/// that quote rather than `Item.dbc`'s `BuyPrice`, the reading of the price
/// field in [`world::vendor`] is confirmed by an independent consequence
/// rather than by a table lookup. That is the whole reason to compare the
/// numbers here instead of just noting that money moved.
///
/// Selling the same item back is what makes the run repeatable: a probe that
/// slowly fills the bags with water is one nobody runs twice. It also tests the
/// sell path against a guid this run just learned, which is exactly how a real
/// client's shop window works.
fn trade_and_report(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    list: &world::VendorList,
    slot: u32,
    sell_back: bool,
) -> Result<()> {
    use world::inventory;

    // The server's slot, looked up in the list rather than trusted from the
    // command line, so the entry sent alongside it cannot disagree -- the
    // server checks the pair, and inventing either half is the mistake the
    // slot-is-not-a-row rule exists to prevent.
    let Some(item) = list.items.iter().find(|item| item.slot == slot) else {
        println!("\nthis vendor has no slot {slot}. It offers: {:?}",
            list.items.iter().map(|i| i.slot).collect::<Vec<_>>());
        return Ok(());
    };

    // **Test the block's numbering with an opcode that answers, before
    // sending one that does not.**
    //
    // `CMSG_BUY_ITEM` is confirmed only by effect, so a silent failure has
    // three causes and no way to tell them apart. `CMSG_LIST_INVENTORY` sits
    // four below it and is *answered* with a packet whose layout is already
    // established -- so if it comes back, the numbering around here is right
    // and a silent buy is about the body. If it does not, the whole block is
    // in the wrong place and the buy was never going to work.
    //
    // One cheap answered request to bound the search is the same move that
    // turned three failed attempts at chat into a one-run answer.
    println!("\nchecking the opcode block: CMSG_LIST_INVENTORY at the vendor");
    connection.list_inventory(list.vendor)?;
    let probe = connection.drain(std::time::Duration::from_millis(1500), 128)?;
    state.replicate(&probe, None);
    match probe
        .iter()
        .find(|p| p.opcode == world::opcode::server::LIST_INVENTORY)
    {
        Some(reply) => println!(
            "  answered: {} bytes of stock. The block is numbered correctly,",
            reply.body.len()
        ),
        None => {
            println!("  NO ANSWER. An opcode that should be answered was not, so the");
            println!("  numbering around here is wrong and a silent buy proves nothing.");
            println!("  Fix this before reading anything into the purchase below.");
        }
    }
    if probe
        .iter()
        .any(|p| p.opcode == world::opcode::server::LIST_INVENTORY)
    {
        println!("  so a silent purchase below is about the *body*, not the number.");
    }

    let money_before = inventory::coinage(state, own_guid);
    let held_before: std::collections::BTreeSet<u64> = inventory::held(state, own_guid)
        .into_iter()
        .map(|held| held.guid)
        .collect();

    println!(
        "\nbuying vendor slot {} (entry {}) at {} copper; purse holds {money_before}",
        item.slot, item.entry, item.price
    );
    if money_before < item.price {
        // Refused loudly rather than attempted: a purchase that cannot be
        // afforded is declined, and that decline is a silence -- which looks
        // exactly like a wrong opcode. Same reasoning as refusing to greet
        // from out of range.
        println!("  not enough money ({money_before} < {}). Not sending: a refusal", item.price);
        println!("  here would be indistinguishable from an opcode the server ignored.");
        println!("  Top up first: --say \".modify money 10000\" --say \".save\"");
        return Ok(());
    }

    connection.buy_item(
        list.vendor,
        item.slot,
        item.entry,
        1,
        inventory::OWN_SLOT_ARRAY,
    )?;
    let batch = connection.drain(std::time::Duration::from_millis(1500), 128)?;
    state.replicate(&batch, None);

    let money_after = inventory::coinage(state, own_guid);
    let held_after = inventory::held(state, own_guid);
    let gained: Vec<&world::HeldItem> = held_after
        .iter()
        .filter(|held| !held_before.contains(&held.guid))
        .collect();

    let spent = money_before.saturating_sub(money_after);
    println!("  money {money_before} -> {money_after} (spent {spent})");
    for held in &gained {
        let stack = state
            .get(held.guid)
            .and_then(|item| item.fields.get(world::update::fields::ITEM_FIELD_STACK_COUNT));
        println!(
            "  gained {:#018x} at slot {} entry {:?} stack {:?}",
            held.guid,
            held.slot.index(),
            held.entry,
            stack
        );
    }

    if gained.is_empty() && spent == 0 {
        println!("  nothing moved -- which is what a wrong opcode looks like, and");
        println!("  also what a full bag looks like. Check the bags first.");
        return Ok(());
    }

    // **The measurement, not just the confirmation.** Whether the price is per
    // purchase or per item is a real question this run can answer rather than
    // assume, and it is answered by what actually left the purse against what
    // actually arrived in the bag.
    println!(
        "  quoted {} copper for a buy_count of {}; spent {spent}",
        item.price, item.buy_count
    );
    if spent == item.price {
        println!("  -- the quote is the price of one purchase, and the stock list's");
        println!("     discounted figure is what is actually charged. That confirms");
        println!("     vendor::VendorItem::price against a consequence rather than");
        println!("     against a table.");
    } else if spent != 0 {
        println!("  -- spent differs from the quote. Worth investigating before");
        println!("     anything displays a price: {spent} vs {}.", item.price);
    }

    if !sell_back {
        return Ok(());
    }

    let Some(bought) = gained.first() else {
        println!("\nnothing was gained, so there is nothing to sell back.");
        return Ok(());
    };

    // Named by guid, which is the point: this is the guid the purchase just
    // produced, not a slot index that something else could have moved into.
    println!("\nselling {:#018x} back", bought.guid);
    connection.sell_item(list.vendor, bought.guid, 0)?;
    let batch = connection.drain(std::time::Duration::from_millis(1500), 128)?;
    state.replicate(&batch, None);

    let money_end = inventory::coinage(state, own_guid);
    let still_held = inventory::held(state, own_guid)
        .iter()
        .any(|held| held.guid == bought.guid);
    println!(
        "  money {money_after} -> {money_end} (gained {})",
        money_end.saturating_sub(money_after)
    );
    println!(
        "  the item is {}",
        if still_held {
            "STILL in the bags -- the sell did not take"
        } else {
            "gone from the bags -- the sell took"
        }
    );
    // A vendor buys back for less than it sells for, so the purse should not
    // return to where it started. Saying so stops that being read as a bug.
    println!("  net over both trades: {} -> {money_end}", money_before);
    println!("  (a vendor buys back below its sale price, so this is expected");
    println!("   to be a loss rather than a wash.)");

    Ok(())
}

/// The nearest unit that will talk, measured from where the character actually
/// is.
///
/// Split out from [`survey_gossip`] because the approach loop re-asks it after
/// every step: a walk changes the answer, and reusing the first one is how
/// `--attack` used to swing at where a creature had been.
fn nearest_talker_from(
    state: &world::WorldState,
    own_guid: u64,
    own: world::Position,
) -> Option<(u64, f32)> {
    state
        .iter()
        .filter(|entity| entity.guid != own_guid)
        .filter(|entity| is_a_legal_test_target(entity))
        .filter(|entity| entity.will_talk())
        .filter_map(|entity| {
            let at = entity.position?;
            let distance = ((at.x - own.x).powi(2) + (at.y - own.y).powi(2)).sqrt();
            Some((entity.guid, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Wears an item and reports what moved, which is what confirms the opcode.
///
/// **The whole point is the diff.** `CMSG_AUTOEQUIP_ITEM` is acknowledged by
/// nothing, and this project has already paid for the lesson that a wrong
/// outgoing opcode is not refused -- it is read as some other valid request,
/// and the silence looks identical either way. What separates them is that a
/// *correct* send has a loud consequence: one guid leaves one field of the
/// player's own object and appears at another. So this snapshots the slot
/// array, sends, waits, and prints every slot whose occupant changed.
///
/// Nothing here interprets the destination. Which equipment index the server
/// picked is the *answer* being collected, not something to be checked against
/// a table we do not have -- see `world::inventory::InventorySlot::label`.
fn equip_and_report(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    slot: u16,
) -> Result<()> {
    use world::inventory::{self, InventorySlot};

    let Some(source) = InventorySlot::new(slot) else {
        println!(
            "\nslot {slot} is past the end of the array ({} slots)",
            inventory::SLOT_COUNT
        );
        return Ok(());
    };

    let before: std::collections::BTreeMap<u16, u64> = inventory::held(state, own_guid)
        .into_iter()
        .map(|item| (item.slot.index(), item.guid))
        .collect();

    let Some(&moving) = before.get(&slot) else {
        // Worth refusing loudly. Sending an equip for an empty slot is
        // ignored by the server, and "nothing moved" would then be
        // indistinguishable from a wrong opcode -- which is precisely the
        // confusion this command exists to avoid.
        println!("\nslot {slot} is empty; nothing to equip, and a run against an");
        println!("empty slot cannot tell a working opcode from a broken one");
        return Ok(());
    };
    println!("\nequipping {moving:#018x} from slot {slot}");

    connection.equip_item(source)?;

    // Give the server time to act before concluding it ignored us -- a send
    // immediately before a read is often never processed, and that failure is
    // indistinguishable from having sent the wrong thing.
    let batch = connection.drain(std::time::Duration::from_millis(1200), 128)?;
    state.replicate(&batch, None);

    let after: std::collections::BTreeMap<u16, u64> = inventory::held(state, own_guid)
        .into_iter()
        .map(|item| (item.slot.index(), item.guid))
        .collect();

    let mut moved = Vec::new();
    for index in 0..inventory::SLOT_COUNT {
        let (was, now) = (before.get(&index), after.get(&index));
        if was != now {
            moved.push((index, was.copied(), now.copied()));
        }
    }

    if moved.is_empty() {
        // **Nothing moved is two completely different findings**, and printing
        // only "nothing moved" makes them look like one: a wrong opcode the
        // server ignored, and a correct opcode the server deliberately
        // refused. What separates them is whether anything came back at all --
        // a refusal is a packet. So this prints every opcode that arrived,
        // decoded or not, which is the move that turned three failed attempts
        // at chat into a one-run answer.
        println!("\nnothing moved. Two different things look like this: an opcode");
        println!("the server ignored, and a refusal it sent back. What arrived:");
        let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
        for packet in &batch {
            *seen.entry(packet.opcode).or_default() += 1;
        }
        for (opcode, count) in &seen {
            println!("    {:<34} x{count}", world::opcode::describe(*opcode));
        }
        if seen.is_empty() {
            println!("    nothing at all");
        }
        return Ok(());
    }

    println!("\n{} slot(s) changed:", moved.len());
    for (index, was, now) in &moved {
        let describe = |guid: &Option<u64>| match guid {
            Some(guid) => format!("{guid:#018x}"),
            None => "empty".into(),
        };
        let here = InventorySlot::new(*index).unwrap();
        println!(
            "  slot {index:>2} ({:>8}, {:>9}): {} -> {}",
            match here.kind() {
                inventory::SlotKind::Equipped => "equipped",
                inventory::SlotKind::Bag => "bag",
                inventory::SlotKind::Backpack => "backpack",
            },
            here.label().unwrap_or("-"),
            describe(was),
            describe(now),
        );
    }

    // The identification this run is actually for: where the server decided
    // the item belongs. Printed on its own line because it is the output worth
    // copying into `InventorySlot::label`.
    if let Some((index, _, _)) = moved
        .iter()
        .find(|(_, _, now)| *now == Some(moving))
    {
        println!("\n{moving:#018x} now sits at equipment slot {index}");
        println!("-- that is what this item's inventory type equips to");
    }

    Ok(())
}

/// Sends `SwapItemCandidate` (`CMSG_SWAP_ITEM`, confirmed -- `foss-wow#55`)
/// between two of the player's own slots and reports what moved -- the same
/// diff `equip_and_report` uses.
fn swap_and_report(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    pair: &str,
) -> Result<()> {
    use world::inventory::{self, InventorySlot};

    let Some((a, b)) = pair.split_once(':') else {
        anyhow::bail!("--swap wants <from>:<to>, e.g. 25:26");
    };
    let (a, b): (u16, u16) = (a.parse()?, b.parse()?);
    let (Some(from), Some(to)) = (InventorySlot::new(a), InventorySlot::new(b)) else {
        anyhow::bail!("slot past the end of the array ({} slots)", inventory::SLOT_COUNT);
    };

    let before: std::collections::BTreeMap<u16, u64> = inventory::held(state, own_guid)
        .into_iter()
        .map(|item| (item.slot.index(), item.guid))
        .collect();
    println!(
        "\nswapping slot {a} ({:?}) and slot {b} ({:?})",
        before.get(&a).map(|g| format!("{g:#018x}")),
        before.get(&b).map(|g| format!("{g:#018x}")),
    );
    if !before.contains_key(&a) && !before.contains_key(&b) {
        println!("both slots are empty; there is nothing here for a swap to move.");
        return Ok(());
    }

    connection.swap_item_candidate(
        inventory::OWN_SLOT_ARRAY,
        to.index() as u8,
        inventory::OWN_SLOT_ARRAY,
        from.index() as u8,
    )?;
    let batch = connection.drain(std::time::Duration::from_millis(1200), 128)?;
    let report = state.replicate(&batch, None);

    // A refusal is a real answer and worth showing whole -- print the body,
    // not just its length, the same rule every silent-send probe here
    // follows.
    for failure in &report.inventory_failures {
        println!(
            "  refused: {} (items {:#018x}, {:#018x})",
            world::inventory::describe_inventory_failure(failure.code),
            failure.item_a,
            failure.item_b
        );
    }

    let after: std::collections::BTreeMap<u16, u64> = inventory::held(state, own_guid)
        .into_iter()
        .map(|item| (item.slot.index(), item.guid))
        .collect();

    let mut moved = Vec::new();
    for index in 0..inventory::SLOT_COUNT {
        let (was, now) = (before.get(&index), after.get(&index));
        if was != now {
            moved.push((index, was.copied(), now.copied()));
        }
    }

    if moved.is_empty() {
        println!("\nnothing moved.");
        return Ok(());
    }

    println!("\n{} slot(s) changed:", moved.len());
    for (index, was, now) in &moved {
        let describe = |guid: &Option<u64>| match guid {
            Some(guid) => format!("{guid:#018x}"),
            None => "empty".into(),
        };
        println!("  slot {index:>2}: {} -> {}", describe(was), describe(now));
    }
    let swapped = moved.len() == 2
        && moved.iter().any(|(i, _, now)| *i == a && *now == before.get(&b).copied())
        && moved.iter().any(|(i, _, now)| *i == b && *now == before.get(&a).copied());
    println!(
        "\n{}",
        if swapped {
            "-- both guids landed at each other's slot: a real swap."
        } else {
            "-- something moved, but not a two-way swap of this exact pair."
        }
    );

    Ok(())
}

/// Finds which update field carries a player's appearance, by searching for an
/// answer that is already known from somewhere else.
///
/// **This exists because the obvious alternative is the mistake this project
/// keeps paying for.** The five numbers that describe a character -- skin,
/// face, hairstyle, hair colour, facial hair -- are packed into two update
/// fields whose indices are widely documented and have never been checked
/// against this build here. Transcribing an index from memory produces a
/// client that parses perfectly and dresses every other player wrongly, and
/// nothing about the result says which field was misread.
///
/// The search is possible because our *own* character's appearance arrives
/// twice by two unrelated routes: once in `SMSG_CHAR_ENUM`, parsed and
/// confirmed against a live realm since 3.2, and once in the update fields of
/// our own player object. So pack the known answer and ask which field holds
/// it. One match is a measurement; several would mean the question needs
/// narrowing, and none would mean the packing order is not what was assumed --
/// all three are more informative than a transcribed constant.
fn report_appearance(state: &world::WorldState, character: &world::Character) {
    println!("\nappearance, as the character list reports it:");
    println!(
        "  race {} class {} gender {}, skin {} face {} hair {}/{} facial {}",
        character.race,
        character.class,
        character.gender,
        character.skin,
        character.face,
        character.hair_style,
        character.hair_color,
        character.facial_hair
    );

    let Some(own) = state.get(character.guid) else {
        println!("  our own player object is not in replicated state; nothing to search");
        return;
    };

    // The order is the hypothesis under test, not an assumption: if the four
    // bytes are packed some other way, nothing matches and that is the answer.
    let packed = u32::from_le_bytes([
        character.skin,
        character.face,
        character.hair_style,
        character.hair_color,
    ]);
    println!(
        "  searching {} set fields for {packed:#010x} \
         (skin, face, hair style, hair colour, low byte first)",
        own.fields.len()
    );

    let matches: Vec<u16> = own
        .fields
        .iter()
        .filter(|(_, value)| *value == packed)
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [] => println!("  no field holds it -- the packing order is not this"),
        indices => {
            for index in indices {
                println!("  field {index:#x} holds it");
                // The facial hair lives in the *next* field's low byte in
                // every account of this layout, so report what is actually
                // there rather than assuming it.
                if let Some(next) = own.fields.get(index + 1) {
                    let bytes = next.to_le_bytes();
                    println!(
                        "    field {:#x} is {next:#010x}, bytes {bytes:?}; \
                         facial hair from the character list is {}",
                        index + 1,
                        character.facial_hair
                    );
                }
            }
        }
    }

    // Worth printing whether or not the search succeeded: a second player is
    // the case this is all for, and their fields either carry the same shape
    // or the search proved nothing about anyone but us.
    for entity in state.players() {
        if entity.guid == character.guid {
            continue;
        }
        let at = |index: u16| {
            entity
                .fields
                .get(index)
                .map(|v| format!("{v:#010x} {:?}", v.to_le_bytes()))
                .unwrap_or_else(|| "unset".into())
        };
        println!("  another player {:#x}:", entity.guid);
        // The constants the search settled on, always -- not only the indices
        // that matched *this* character. A stranger whose appearance fields
        // never arrived and one whose fields we read from the wrong place look
        // identical from the outside, and only the raw values separate them.
        for index in [
            world::update::fields::PLAYER_BYTES,
            world::update::fields::PLAYER_BYTES_2,
        ] {
            println!("    field {index:#x} = {}", at(index));
        }
        match entity.appearance() {
            Some(look) => println!(
                "    reads as skin {} face {} hair {}/{} facial {}",
                look.skin, look.face, look.hair_style, look.hair_color, look.facial_hair
            ),
            None => println!("    no appearance could be read -- this player renders white"),
        }
    }
}

/// Prints every field set on this character's own object.
///
/// For diffing one state against another. A character that is dead and a
/// character that is alive differ in some field, and which one is the question
/// -- asking it by *comparing two states* rather than by looking up a flag is
/// the same technique that found `PLAYER_BYTES` and the game-object display id.
/// Byte 0 of our own `UNIT_FIELD_BYTES_2`, as a sheath state.
///
/// `None` when the object is not replicated at all, which is a different thing
/// from being unarmed and needs to stay distinguishable: a probe that cannot
/// see itself proves nothing either way.
fn own_sheath(
    state: &world::WorldState,
    own_guid: u64,
) -> Option<world::combat::SheathState> {
    let field = state.get(own_guid)?.fields.get(world::update::fields::UNIT_BYTES_2)?;
    Some(world::combat::SheathState::from_bytes_2(field))
}

fn report_own_fields(state: &world::WorldState, own_guid: u64, when: &str) {
    let Some(entity) = state.get(own_guid) else {
        println!("\nown object not replicated ({when})");
        return;
    };
    println!(
        "\nown object {own_guid:#x} ({when}), {} field(s) set:",
        entity.fields.len()
    );
    for (index, value) in entity.fields.iter() {
        println!("  {index:#06x} = {value:#010x} ({value})");
    }
}

/// Picks fights until this character is killed, and reports what arrives.
///
/// **A death cannot be captured without dying**, and a level-one character wins
/// most single fights, so this keeps going: when a target falls and we are
/// still standing, it picks the next one. Health does not recover between
/// rounds, so the outcome is only a question of how many rounds it takes.
///
/// Everything is reported rather than interpreted. Nothing here is parsed as
/// "you died" -- this client has never seen the death flow, and finding which
/// opcodes carry it is the whole point of the run. What it prints is own health
/// each round, every event the fold produced, and every opcode seen.
fn fight_until_death(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    character: &world::Character,
    target: Option<&str>,
    mut capture: Option<&mut Capture>,
) -> Result<()> {
    use std::time::{Duration, Instant};

    const MELEE_REACH: f32 = 2.5;
    const RUN_SPEED: f32 = 7.0;
    // Long enough for several rounds, short enough that a fight this character
    // cannot lose still ends.
    let deadline = Instant::now() + Duration::from_secs(540);

    let mut here = state
        .get(character.guid)
        .and_then(|entity| entity.position)
        .unwrap_or_default();
    let mut round = 0;
    let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();

    // **A character that is already dead cannot be killed, and looks exactly
    // like one that is nearly dead.** A ghost has one health, so `1/79` reads
    // as a warrior about to fall over rather than as a warrior who already
    // has. Four runs of this command were spent swinging as a ghost: every
    // request came back `SMSG_ATTACKSTOP` with no `SMSG_ATTACKSTART`, no
    // swings, no refusals, and the distances closing correctly the whole time,
    // which is indistinguishable from an attack opcode that has stopped
    // working. The character list said `ghost` on every one of those runs and
    // nothing read it.
    //
    // The general form is already in `CLAUDE.md` -- confirm the thing being
    // read is the thing being written -- and this is its other face: confirm
    // the precondition still holds before concluding the action failed.
    if character.is_ghost() {
        println!(
            "\n{} is already a ghost, and a ghost cannot fight. Its one health is\n\
             the ghost's, not a survivor's. Release or resurrect it first, or run\n\
             this against a living character.",
            character.name
        );
        return Ok(());
    }

    println!("\nfighting until dead (up to 5 minutes)");
    let own_max = state
        .get(character.guid)
        .and_then(|entity| entity.max_health())
        .unwrap_or(0);

    while Instant::now() < deadline {
        round += 1;
        let own_health = state
            .get(character.guid)
            .and_then(|entity| entity.health())
            .unwrap_or(0);
        println!("\nround {round}: {own_health}/{own_max}");
        if own_max > 0 && own_health == 0 {
            println!("  dead.");
            break;
        }

        // The nearest thing matching the name, or the nearest of anything --
        // measured from `here`, which is where this client has actually walked
        // to, not from the login position replicated state still believes in.
        let chosen = {
            let mut rows = nearest_ordered_from(state, character.guid, here);
            if let Some(wanted) = target {
                let wanted = wanted.to_lowercase();
                rows.retain(|(_, entity)| {
                    unit_label(state, entity).to_lowercase().contains(&wanted)
                });
            }
            // Something already dead is not worth swinging at, and is exactly
            // what the previous round left lying there.
            rows.retain(|(_, entity)| entity.health().is_none_or(|hp| hp > 0));
            // Whatever is already hitting us wins, wherever it is in the list.
            //
            // Without this the run stalls in a way that looks like nothing at
            // all: a wolf chews the character down to 4 health, the next round
            // picks a *different* wolf thirty units away, walking there breaks
            // combat, and health regenerates on the way. The character then
            // never dies, having spent every round walking between fights it
            // keeps leaving. Being killed is the entire purpose of this
            // command, so the thing killing us is the thing to stand still and
            // let finish.
            let engaged = rows
                .iter()
                .find(|(_, entity)| {
                    state.attacking.get(&entity.guid) == Some(&character.guid)
                })
                .map(|(distance, entity)| (entity.guid, *distance));
            engaged.or_else(|| rows.first().map(|(distance, entity)| (entity.guid, *distance)))
        };
        let Some((guid, distance)) = chosen else {
            println!("  nothing left to fight");
            break;
        };

        // Close, face, swing -- the three steps `--attack` established,
        // repeated per round because the next target is somewhere else.
        //
        // **Closed on the horizontal distance, not the straight-line one**, and
        // both are printed so the difference is visible rather than inferred.
        // This client's own Z is not tracked while walking: `Connection::walk`
        // advances x and y and carries the starting altitude along unchanged,
        // because the terrain height that would correct it lives in the
        // renderer. So the vertical term of a 3D distance is a known-wrong
        // number, and including it in the approach makes the walk stop short by
        // however wrong it is -- observed as a fight that closed from 97 units
        // to 10.5 and then refused to close any further, one wolf-length short
        // of melee, for as long as the run lasted.
        if let Some(there) = state.get(guid).and_then(|e| e.position) {
            let flat = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
            println!(
                "  target {guid:#x} at {flat:.1} units ({distance:.1} straight line, \
                 dz {:.1})",
                there.z - here.z
            );
            if flat > MELEE_REACH {
                let heading = (there.y - here.y).atan2(there.x - here.x);
                let (arrived, _) =
                    connection.walk(character.guid, here, heading, flat - MELEE_REACH, RUN_SPEED)?;
                here = arrived;
                here.orientation = heading;
                // **And take the target's altitude as our own.**
                //
                // `Connection::walk` advances x and y and carries the starting
                // Z along, so a walk of ninety units across Northshire's hills
                // tells the server this character is hovering wherever the
                // ground was at login. That is not a display problem: the
                // position we send is the position the server believes, and it
                // measures melee range in three dimensions, so a swing from
                // two yards away and six above is refused exactly as if it came
                // from eight yards away. Observed here as a fight that closed
                // to 2.2 flat units and never landed a blow, with `dz 5.1`
                // printed beside it.
                //
                // The right answer is the terrain height, which `wow-cli adt
                // height` already computes to within three centimetres -- but
                // it needs `--data`, and these protocol commands deliberately
                // work on a machine with no game installation. A creature
                // stands *on* the ground, so the altitude of the thing we just
                // walked up to is the best statement about the ground here that
                // this command can make without one. It is an approximation,
                // and it is the target's own number rather than a guess.
                here.z = there.z;
            }
        } else {
            println!("  target {guid:#x} at {distance:.1} units, position unknown");
        }
        if let Some(there) = state.get(guid).and_then(|e| e.position) {
            let heading = (there.y - here.y).atan2(there.x - here.x);
            connection.set_facing(character.guid, here, heading)?;
            here.orientation = heading;
        }
        connection.set_selection(guid)?;
        connection.attack_swing(guid)?;

        // **Swing once to be noticed, then stop swinging and let it win.**
        //
        // `CMSG_ATTACKSWING` starts an auto-attack that repeats until stopped,
        // and that is the wrong thing for a command whose goal is to be killed:
        // a level-one character beats a level-one creature most of the time, so
        // the rig killed each attacker in turn and regenerated on the walk to
        // the next one. Two runs of this ended with more health than they
        // started, which reads as the damage not being applied rather than as
        // the rig winning fights it was trying to lose.
        //
        // So the aggro is kept and the auto-attack is dropped. Health then only
        // moves in one direction, and the fight ends the way this command needs
        // it to. `--attack` remains the flag for actually fighting something.
        let mut soaking = false;
        let mut last_health = own_health;
        let mut last_drop = Instant::now();

        let round_end = Instant::now() + Duration::from_secs(45);
        while Instant::now() < round_end && Instant::now() < deadline {
            let batch = connection.drain(Duration::from_millis(800), 128)?;
            if let Some(capture) = capture.as_mut() {
                capture.record(&batch)?;
            }
            for packet in &batch {
                *seen.entry(packet.opcode).or_default() += 1;
            }
            let report = state.replicate(&batch, None);
            print_events(&report, state, character.guid);

            let own = state
                .get(character.guid)
                .and_then(|entity| entity.health())
                .unwrap_or(0);
            if own == 0 && own_max > 0 {
                println!("  health reached zero");
                break;
            }

            // Once anything has actually landed on us, stop hitting back.
            if !soaking && own < last_health {
                connection.attack_stop()?;
                soaking = true;
                println!("  taking damage -- stopping the auto-attack and soaking");
            }
            if own < last_health {
                last_drop = Instant::now();
            }
            last_health = own;

            // Nothing has hurt us for a while: whatever had aggro lost it or
            // died, so go and find something else rather than standing in an
            // empty field for the rest of the round.
            if soaking && last_drop.elapsed() > Duration::from_secs(15) {
                println!("  nothing is hitting us any more");
                break;
            }
            // Only relevant before the soak begins; once soaking we no longer
            // care whether this particular target is alive, only whether
            // something is still hitting us.
            if !soaking
                && state
                    .get(guid)
                    .and_then(|entity| entity.health())
                    .is_none_or(|hp| hp == 0)
            {
                println!("  target down");
                break;
            }
        }
    }

    // Then hold with nothing being sent. Whatever the server says about a
    // corpse arrives after the killing blow rather than with it, and a client
    // that disconnects immediately would never see it -- the same trap that
    // once got a facing opcode written off as wrong.
    println!("\nholding after the fight, sending nothing:");
    let quiet_end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < quiet_end {
        let batch = connection.drain(Duration::from_millis(1000), 128)?;
        if let Some(capture) = capture.as_mut() {
            capture.record(&batch)?;
        }
        for packet in &batch {
            *seen.entry(packet.opcode).or_default() += 1;
        }
        let report = state.replicate(&batch, None);
        print_events(&report, state, character.guid);
    }

    let health = state
        .get(character.guid)
        .and_then(|entity| entity.health())
        .unwrap_or(0);
    println!("\nfinal health: {health}/{own_max}");
    println!("every opcode seen during the fight:");
    for (opcode, count) in &seen {
        println!("  {:<34} x{count}", world::opcode::describe(*opcode));
    }
    Ok(())
}

/// Varies one condition of a swing at a time -- range or facing, never both
/// -- so a refusal can be attributed to the condition that changed rather
/// than left ambiguous between two.
///
/// `--attack`'s first run swung from five units away without facing the
/// target, and got two different empty-bodied refusals with nothing to say
/// which was which: two variables changed in one experiment, the classic way
/// to learn nothing. This holds one of range or facing wrong and the other
/// correct, so whatever comes back is attributable.
///
/// Returns the walked-to position, the same way `Connection::walk` does --
/// replicated state still believes the login position, so a caller chaining
/// probes has to carry the real one forward itself.
fn run_swing_probe(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    character: &world::Character,
    probe: SwingProbe,
    target: Option<&str>,
    mut here: world::Position,
    mut capture: Option<&mut Capture>,
) -> Result<world::Position> {
    use std::f32::consts::PI;
    use std::time::Duration;

    const MELEE_REACH: f32 = 2.5;
    const IN_RANGE: f32 = MELEE_REACH - 0.5;
    // The ticket's own band: comfortably past the server's reach check
    // without being so far the approach walk takes long enough for the
    // target to wander somewhere else entirely.
    const OUT_OF_RANGE: f32 = 9.0;
    const RUN_SPEED: f32 = 7.0;
    const ATTEMPTS: usize = 3;

    let label = match probe {
        SwingProbe::A => "A: in melee range, facing away",
        SwingProbe::B => "B: out of range, facing correctly",
        SwingProbe::C => "C (control): in range and facing",
    };
    println!("\nswing probe {label}");

    for attempt in 1..=ATTEMPTS {
        // Named, not merely nearest: the nearest unit to a starting
        // character is very often a friendly NPC, and a swing at one is
        // refused regardless of range or facing -- which would read as
        // exactly the ambiguous result this probe exists to avoid.
        let mut candidates = nearest_ordered_from(state, character.guid, here);
        if let Some(wanted) = target {
            let wanted = wanted.to_lowercase();
            candidates
                .retain(|(_, entity)| unit_label(state, entity).to_lowercase().contains(&wanted));
        }
        let Some(&(_, chosen)) = candidates.first() else {
            println!("  nothing replicated to probe against");
            break;
        };
        let guid = chosen.guid;
        let Some(there) = state.get(guid).and_then(|e| e.position) else {
            println!("  target {guid:#x} has no known position");
            continue;
        };

        // Close or open the distance to the band this probe wants, and
        // nothing else -- a walk toward the target changes only range, never
        // facing, since the heading sent below is chosen independently.
        let flat = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
        let wanted = match probe {
            SwingProbe::A | SwingProbe::C => IN_RANGE,
            SwingProbe::B => OUT_OF_RANGE,
        };
        if (flat - wanted).abs() > 1.0 {
            let toward = (there.y - here.y).atan2(there.x - here.x);
            let (heading, distance) = if flat > wanted {
                (toward, flat - wanted)
            } else {
                (toward + PI, wanted - flat)
            };
            let (arrived, _) =
                connection.walk(character.guid, here, heading, distance, RUN_SPEED)?;
            here = arrived;
        }

        // Re-measured after any walk: the target may have moved during it,
        // and both the facing this probe sends and the range it reports
        // have to agree with where things actually are now.
        let Some(there) = state.get(guid).and_then(|e| e.position) else {
            continue;
        };
        let flat = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
        let correct_heading = (there.y - here.y).atan2(there.x - here.x);
        let sent_heading = match probe {
            SwingProbe::A => correct_heading + PI,
            SwingProbe::B | SwingProbe::C => correct_heading,
        };
        connection.set_facing(character.guid, here, sent_heading)?;
        here.orientation = sent_heading;
        connection.set_selection(guid)?;

        println!(
            "  attempt {attempt}: {flat:.1} units from {guid:#x}, facing {}",
            if probe == SwingProbe::A { "away" } else { "correctly" }
        );
        connection.attack_swing(guid)?;

        let batch = connection.drain(Duration::from_millis(1200), 128)?;
        if let Some(capture) = capture.as_mut() {
            capture.record(&batch)?;
        }
        let mut counts: std::collections::BTreeMap<u16, usize> = Default::default();
        for packet in &batch {
            *counts.entry(packet.opcode).or_default() += 1;
        }
        println!("    server sent {} packets:", batch.len());
        for (opcode, count) in &counts {
            println!("      {:<32} x{count}", world::opcode::describe(*opcode));
        }
        let report = state.replicate(&batch, None);
        print_events(&report, state, character.guid);
        if !report.swings.is_empty() {
            println!("    -> landed a real swing (SMSG_ATTACKERSTATEUPDATE)");
        }
    }
    Ok(here)
}

/// What to call a guid, for a line of combat log.
fn unit_label_for(state: &world::WorldState, guid: u64) -> String {
    match state.get(guid) {
        Some(entity) => unit_label(state, entity),
        None => format!("{guid:#x}"),
    }
}

/// Replicated units other than the player, nearest first.
fn nearest_ordered(state: &world::WorldState, own_guid: u64) -> Vec<(f32, &world::state::Entity)> {
    let Some(own) = state.get(own_guid).and_then(|entity| entity.position) else {
        return Vec::new();
    };
    nearest_ordered_from(state, own_guid, own)
}

/// The same ordering, measured from a position the caller supplies.
///
/// **Which caller needs this is the whole point.** [`nearest_ordered`] measures
/// from replicated state, and replicated state is wrong about where *we* are:
/// the server never relays this client's own movement back to it, so an entity
/// that has walked anywhere is still recorded at the position it logged in at.
/// That is fine for a caller that has not moved, and silently wrong for one
/// that has -- it computes every approach from the login spot, arrives out of
/// range, and reads as the attack opcode being broken rather than as a stale
/// origin. That exact bug cost 4.4 a debugging session; it came back in
/// `fight_until_death`, where it looked like a fight that never closed to melee
/// while the printed distance sat unchanged across two rounds.
fn nearest_ordered_from(
    state: &world::WorldState,
    own_guid: u64,
    own: world::Position,
) -> Vec<(f32, &world::state::Entity)> {
    let mut rows: Vec<(f32, &world::state::Entity)> = state
        .iter()
        .filter(|entity| entity.guid != own_guid)
        // See `is_a_legal_test_target`: a substring name filter will match
        // another person's character sooner or later, and did.
        .filter(|entity| is_a_legal_test_target(entity))
        .filter_map(|entity| {
            let at = entity.position?;
            let distance = ((at.x - own.x).powi(2) + (at.y - own.y).powi(2) + (at.z - own.z).powi(2))
                .sqrt();
            // A distance of exactly zero would read as "(you)" in the table.
            Some((distance.max(f32::MIN_POSITIVE), entity))
        })
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows
}

/// Stays in the world, answering keepalives, and reports what kept arriving.
///
/// The point is not the packet count but that the connection survives: a client
/// that stops pinging is dropped, and the drop arrives as a plain close that
/// looks exactly like a parser losing its place in the stream.
fn hold_connection(
    connection: &mut world::Connection,
    duration: std::time::Duration,
    state: &mut world::WorldState,
    own_guid: u64,
    mut capture: Option<&mut Capture>,
    // What to do about a party invite that arrives while holding. This is the
    // invitee half of the two-client rig, and it has to live inside the hold
    // rather than beside it: an invite can only be answered while it is open,
    // and it arrives on the other client's schedule, not this one's.
    party_answer: Option<PartyAnswer>,
) -> Result<()> {
    // Not tunable: pinging faster gets the session killed. See PING_INTERVAL.
    const PING_EVERY: std::time::Duration = world::client::PING_INTERVAL;

    println!("\nholding the connection for {}s", duration.as_secs());
    let until = std::time::Instant::now() + duration;
    let mut packets = 0usize;
    let mut pings = 0usize;
    let mut last_ping = std::time::Instant::now();
    let mut totals = world::Replication::default();
    let mut swings = 0usize;

    /// Where a replicated object was first and last seen, so a journey can be
    /// reported rather than only a final position.
    struct Track {
        first: world::Position,
        last: world::Position,
    }
    let mut tracks: std::collections::BTreeMap<u64, Track> = Default::default();
    // Every opcode seen, decoded or not. Without this, "the server never sent
    // it" and "it arrived and we could not read it" are the same observation,
    // and they want opposite investigations.
    let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();

    while std::time::Instant::now() < until {
        // A small batch on purpose. `drain` returns when the stream goes quiet
        // *or* the limit is hit, and a populated zone is rarely quiet for
        // 500 ms -- so a large limit means few, long iterations, and the
        // sampling below sees a handful of coarse snapshots instead of a path.
        let batch = connection.drain(std::time::Duration::from_millis(300), 48)?;
        packets += batch.len();
        for packet in &batch {
            *seen.entry(packet.opcode).or_default() += 1;
        }
        if let Some(capture) = capture.as_mut() {
            capture.record(&batch)?;
        }

        dump_party_packets(&batch);

        let report = state.replicate(&batch, None);
        totals.object_updates += report.object_updates;
        totals.monster_moves += report.monster_moves;
        totals.relayed_moves += report.relayed_moves;
        totals.names += report.names;
        totals.party_lists += report.party_lists;
        totals.party_stat_updates += report.party_stat_updates;

        // Answered here, inside the loop, because an invite is only
        // answerable while it is open. `WorldState` clears it as soon as any
        // group list arrives, so a check after the hold would find nothing
        // and report a silence that never happened.
        if let (Some(answer), Some(invite)) = (party_answer, state.party_invite.as_ref()) {
            let from = invite.from.clone();
            match answer {
                PartyAnswer::Accept => {
                    println!("  invite from {from:?} -- accepting");
                    connection.group_accept()?;
                }
                PartyAnswer::Decline => {
                    println!("  invite from {from:?} -- declining");
                    connection.group_decline()?;
                }
            }
            // Cleared locally too. The accept is silent, and the group list
            // that confirms it takes a moment to arrive; without this the
            // next iteration sends a second accept for the same invite.
            state.party_invite = None;
        }
        // Printed as it arrives rather than summarised at the end: the whole
        // point of holding the connection is to see whether the other client's
        // line comes through, and a count of two would not say what was said.
        // Printed as it arrives, like chat and for the same reason: a count of
        // fifteen swings does not say who hit whom for how much.
        print_events(&report, state, own_guid);
        swings += report.swings.len();
        totals.attacks_started += report.attacks_started;
        totals.attacks_stopped += report.attacks_stopped;
        totals.power_updates += report.power_updates;
        for (opcode, error, body) in &report.failures {
            // Printed rather than counted: a packet that will not decode here
            // is a layout error in something the replicated world depends on.
            println!("  undecodable {}: {error}", world::opcode::describe(*opcode));
            // With its bytes. Several parsers here deliberately refuse shapes
            // nobody has captured and say a capture is what would settle
            // them -- and the first refusal of a 46-byte swing printed only
            // its length, so the one packet that could have resolved the
            // question was seen and thrown away.
            if let Ok(body) = body {
                println!("    {} bytes: {}", body.len(), hex_preview(body, 64));
            }
        }
        totals.failures.extend(report.failures);

        // Sample the replicated positions of everything that is not us, so the
        // summary can show movement rather than a snapshot.
        for entity in state.iter() {
            let Some(position) = entity.position else {
                continue;
            };
            if entity.guid == own_guid {
                continue;
            }
            tracks
                .entry(entity.guid)
                .and_modify(|track| track.last = position)
                .or_insert(Track {
                    first: position,
                    last: position,
                });
        }

        if last_ping.elapsed() >= PING_EVERY {
            let sent = std::time::Instant::now();
            connection.ping(0)?;
            pings += 1;
            println!("  pong after {} ms", sent.elapsed().as_millis());
            last_ping = std::time::Instant::now();
        }
    }

    let stats = state.stats();
    println!(
        "  still connected: {packets} packets seen, {pings} keepalives answered"
    );
    println!(
        "  applied: {} object updates, {} monster moves, {} relayed moves, {} undecodable",
        totals.object_updates,
        totals.monster_moves,
        totals.relayed_moves,
        totals.failures.len()
    );
    if swings > 0 || totals.attacks_started > 0 {
        println!(
            "  combat: {swings} swings, {} attacks started, {} stopped,              {} still swinging, {} power updates",
            totals.attacks_started,
            totals.attacks_stopped,
            state.attacking.len(),
            totals.power_updates
        );
    }
    println!(
        "  replicated world: {} objects ({} created, {} recreated, {} removed, \
         {} value updates, {} moves, {} orphaned)",
        state.len(),
        stats.created,
        stats.recreated,
        stats.removed,
        stats.value_updates,
        stats.movement_updates,
        stats.orphaned
    );
    report_relayed_movement(&stats);

    // Other players are the interesting case: their movement arrives by a
    // different route than creatures' and is what proves replication of a
    // second client.
    for entity in state.players() {
        if entity.guid == own_guid {
            continue;
        }
        let moved = tracks.get(&entity.guid).map(|track| {
            let distance = ((track.last.x - track.first.x).powi(2)
                + (track.last.y - track.first.y).powi(2))
            .sqrt();
            (track.first, track.last, distance)
        });
        match moved {
            // The update count comes from the entity itself rather than from
            // the sampling above: polling can only see as many positions as it
            // takes snapshots, whereas this counts every update actually
            // applied.
            // Altitude is printed as well as the two horizontal axes, and the
            // distance stays horizontal. The client drives Z off the terrain
            // now, so what the *server* relays about another player's altitude
            // is a thing worth being able to read -- and a check against
            // `wow-cli adt height` at the same x,y, which is the one comparison
            // that can say whether an altitude this client sent was accepted or
            // quietly corrected.
            Some((first, last, distance)) => println!(
                "  player {:#x}: {:.1}, {:.1}, {:.1} -> {:.1}, {:.1}, {:.1} \
                 ({distance:.1} units over {} applied updates)",
                entity.guid,
                first.x,
                first.y,
                first.z,
                last.x,
                last.y,
                last.z,
                entity.updates
            ),
            None => println!("  player {:#x}: no position replicated", entity.guid),
        }
    }

    // Creatures that were given somewhere to be. A zone with none of these
    // means monster moves are not being applied.
    let heading_somewhere = state.iter().filter(|e| e.destination.is_some()).count();
    println!("  {heading_somewhere} creature(s) currently following a path");

    // Everything that arrived, decoded or not. A packet this client ignores is
    // not an error -- the server volunteers plenty -- but when something
    // expected never shows up, the difference between "never sent" and "sent
    // and unread" is the whole investigation.
    println!("  opcodes seen:");
    for (opcode, count) in &seen {
        println!("    {:<32} x{count}", world::opcode::describe(*opcode));
    }
    Ok(())
}

/// Parses every object update in a burst and reports what the world looks like.
///
/// Failures are counted rather than fatal, and the first is printed in full.
/// One malformed packet says something specific about one code path; aborting
/// on it would hide how many of the rest were fine, which is the number that
/// says whether a layout is wrong in general or only in a rare branch.
fn report_object_updates(
    packets: &[world::client::Packet],
    own_guid: u64,
    dump_failed: Option<&std::path::Path>,
) -> Result<world::WorldState> {
    use world::update::{self, Block};

    let mut state = world::WorldState::new();
    let mut blocks = Vec::new();
    let replication = state.replicate(packets, Some(&mut blocks));
    let parsed = replication.object_updates;
    let failures = replication.failures;

    println!("\nobject updates: {parsed} parsed, {} failed", failures.len());
    for (index, (opcode, error, payload)) in failures.iter().enumerate() {
        println!("  {}: {error}", world::opcode::describe(*opcode));
        if let Some(directory) = dump_failed {
            let Ok(payload) = payload else { continue };
            std::fs::create_dir_all(directory)?;
            let path = directory.join(format!("failed-{index}.bin"));
            std::fs::write(&path, payload)
                .with_context(|| format!("writing {}", path.display()))?;
            println!("    wrote {} bytes to {}", payload.len(), path.display());
        }
    }
    if blocks.is_empty() {
        return Ok(state);
    }

    let mut by_type: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut created = 0usize;
    let mut removed = 0usize;
    for block in &blocks {
        match block {
            Block::Create { object_type, .. } => {
                created += 1;
                *by_type.entry(object_type.name()).or_default() += 1;
            }
            Block::OutOfRange { guids } => removed += guids.len(),
            _ => {}
        }
    }
    println!(
        "  {} blocks: {created} created, {removed} left view",
        blocks.len()
    );
    for (name, count) in &by_type {
        println!("    {name:<16} x{count}");
    }

    // The strongest check available: find our own player object and compare it
    // against what the character list already said. Two packets built by
    // different server code agreeing is real evidence; a parse checked against
    // itself is not.
    let own = blocks.iter().find_map(|block| match block {
        Block::Create {
            guid,
            fields,
            movement,
            object_type,
            ..
        } if *guid == own_guid && *object_type == world::ObjectType::Player => {
            Some((fields, movement))
        }
        _ => None,
    });
    let Some((fields, movement)) = own else {
        println!("  own player object not found in the burst");
        return Ok(state);
    };

    println!("  own player object (guid {own_guid:#x}):");
    if let Some(position) = movement.position {
        println!(
            "    at {:.1}, {:.1}, {:.1}   self={} living={}",
            position.x,
            position.y,
            position.z,
            movement.is_self(),
            movement.is_living()
        );
    }
    for (label, index) in [
        ("level", update::fields::UNIT_LEVEL),
        ("health", update::fields::UNIT_HEALTH),
        ("max health", update::fields::UNIT_MAX_HEALTH),
        ("faction", update::fields::UNIT_FACTION),
        ("display id", update::fields::UNIT_DISPLAY_ID),
    ] {
        if let Some(value) = fields.get(index) {
            println!("    {label:<12} {value}");
        }
    }
    if let Some(guid) = fields.get_u64(update::fields::OBJECT_GUID) {
        // The guid appears twice: in the block header and again as a field.
        // They must agree, and they are written by different code paths.
        println!(
            "    guid field   {guid:#x} {}",
            if guid == own_guid { "(matches)" } else { "(MISMATCH)" }
        );
    }
    println!("    {} fields set", fields.len());

    let stats = state.stats();
    println!(
        "  replicated world: {} objects ({} created, {} removed, {} orphaned updates)",
        state.len(),
        stats.created,
        stats.removed,
        stats.orphaned
    );
    report_relayed_movement(&stats);
    Ok(state)
}

/// Answers any teleport the server is waiting on, and says where it went.
///
/// Called after every drain in the flows that can be teleported. The
/// alternative -- answering it in one place and forgetting the others -- is how
/// this project has lost four returned categories already, and this one fails
/// worse than the rest: the server discards movement from a client that has not
/// acknowledged, so the character silently stops moving while the client
/// carries on believing it walked.
fn answer_teleport(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
) -> Result<Option<world::update::Position>> {
    let Some(teleport) = state.pending_teleport.take() else {
        return Ok(None);
    };
    if teleport.mover != own_guid {
        println!(
            "  a teleport for {:#x}, which is not us -- not answering it",
            teleport.mover
        );
        return Ok(None);
    }
    connection.acknowledge_teleport(teleport.mover, teleport.counter)?;
    let at = teleport.info.position;
    println!(
        "  teleported to {:.1}, {:.1}, {:.1} (acknowledged)",
        at.x, at.y, at.z
    );
    Ok(Some(at))
}

/// Releases the spirit and reports what the state became.
///
/// **Nothing acknowledges this request**, and there are two silent refusals
/// behind it -- being alive, and having already released -- so the only useful
/// output is a before-and-after of the things that must change: the ghost flag,
/// health going to one, a corpse object appearing, and the server naming a
/// graveyard. A run that prints the same state twice is a refusal, and says so
/// rather than claiming success.
fn report_release(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    character: &world::Character,
    mut capture: Option<&mut Capture>,
) -> Result<Option<world::update::Position>> {
    use std::time::Duration;

    let mut moved_to = None;
    let was_ghost = state.get(character.guid).is_some_and(|e| e.is_ghost());
    println!(
        "\nreleasing: ghost before = {was_ghost}, health {:?}",
        state.get(character.guid).and_then(|e| e.health())
    );
    connection.release_spirit()?;

    // Give the server time to act before concluding it ignored us. A packet
    // sent immediately before disconnecting is often never processed, and that
    // is indistinguishable from having sent the wrong thing.
    for _ in 0..6 {
        let batch = connection.drain(Duration::from_millis(700), 128)?;
        if let Some(capture) = capture.as_mut() {
            capture.record(&batch)?;
        }
        let report = state.replicate(&batch, None);
        print_events(&report, state, character.guid);
        // Releasing *is* a teleport -- to the graveyard -- so this is the flow
        // that most needs answering, and the one where forgetting it looked
        // like success: an unanswered teleport leaves the ghost standing on its
        // own corpse, which then reclaims from well inside the range check that
        // should have refused it.
        if let Some(at) = answer_teleport(connection, state, character.guid)? {
            moved_to = Some(at);
        }
    }

    let entity = state.get(character.guid);
    let is_ghost = entity.is_some_and(|e| e.is_ghost());
    println!(
        "  ghost after = {is_ghost}, health {:?}",
        entity.and_then(|e| e.health())
    );
    match state.release_location {
        Some(at) => println!(
            "  graveyard: map {:?} at {:.1}, {:.1}, {:.1}",
            at.map, at.x, at.y, at.z
        ),
        None => println!("  no graveyard named"),
    }
    if let Some(delay) = state.reclaim_delay_ms {
        println!("  reclaim delay: {delay}ms");
    }
    // Counted, not identified. Which of ours is the *current* body is a
    // question only `MSG_CORPSE_QUERY` answers, and claiming one here would be
    // the guess that already sent a run back to a previous death site.
    println!(
        "  {} corpse(s) in view, {} of them ours",
        state.corpses().count(),
        state.own_corpses(character.guid).count()
    );
    if was_ghost && is_ghost {
        println!("  nothing changed -- already a ghost, which is one of the two silent refusals");
    }
    Ok(moved_to)
}

/// Takes the body back, and reports whether it worked.
///
/// Five separate conditions refuse this in silence -- alive, not released, no
/// corpse, out of range, delay not elapsed -- so as with the release the report
/// is a before-and-after rather than a claim.
fn report_reclaim(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    character: &world::Character,
    from: Option<world::update::Position>,
    mut capture: Option<&mut Capture>,
) -> Result<()> {
    use std::time::{Duration, Instant};

    /// Well inside the server's 39-yard reclaim radius, so that arriving a
    /// little short of the corpse still counts as arriving.
    const CLOSE_ENOUGH: f32 = 12.0;
    const GHOST_SPEED: f32 = 10.0;

    // **Ask the server where the body is rather than looking around for it.**
    // Corpse-shaped objects include the bones of bodies already reclaimed, they
    // all carry their owner's guid, and a graveyard accumulates them -- one run
    // here saw seven while the server had two, picked a stale one at a previous
    // death site, ran fifty-eight yards to it and was refused in silence.
    connection.query_corpse()?;
    for _ in 0..4 {
        let batch = connection.drain(Duration::from_millis(500), 128)?;
        if let Some(capture) = capture.as_mut() {
            capture.record(&batch)?;
        }
        state.replicate(&batch, None);
        answer_teleport(connection, state, character.guid)?;
        if state.corpse_location.is_some() {
            break;
        }
    }
    let Some(body_at) = state.corpse_location else {
        println!("\nthe server says this character has no corpse");
        return Ok(());
    };
    println!(
        "\nthe server puts our body on map {} at {:.1}, {:.1}, {:.1}",
        body_at.map, body_at.x, body_at.y, body_at.z
    );

    // The guid still has to come from a replicated object, since the query
    // answers *where* and not *which*. Nearest-to-the-answer among the ones we
    // own is the discriminator: two of our own corpses are never in the same
    // place, and the stale ones are exactly the ones that are somewhere else.
    let Some(corpse) = state
        .own_corpses(character.guid)
        .filter_map(|c| c.position.map(|p| (c.guid, p)))
        .min_by(|a, b| {
            let d = |p: &world::update::Position| {
                (p.x - body_at.x).powi(2) + (p.y - body_at.y).powi(2)
            };
            d(&a.1).total_cmp(&d(&b.1))
        })
        .map(|(guid, _)| guid)
    else {
        println!(
            "  but no corpse object of ours is replicated here -- {} in view belong to \
             someone else",
            state.corpses().count()
        );
        return Ok(());
    };
    println!(
        "\nreclaiming corpse {corpse:#x}: ghost before = {}",
        state.get(character.guid).is_some_and(|e| e.is_ghost())
    );

    // **Run back to the body.** This is the corpse run, and it is a real one:
    // the server puts the ghost at a graveyard and refuses a reclaim from
    // further than 39 yards away, so a client that does not walk is refused in
    // silence. This was invisible until teleports were acknowledged -- before
    // that the ghost never left the corpse, the range check passed at nought
    // yards, and the whole feature looked finished.
    let corpse_at = Some(world::update::Position {
        x: body_at.x,
        y: body_at.y,
        z: body_at.z,
        orientation: 0.0,
    });
    if let (Some(mut here), Some(there)) = (from, corpse_at) {
        let flat = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
        println!("  corpse is {flat:.1} units away");
        if flat > CLOSE_ENOUGH {
            let heading = (there.y - here.y).atan2(there.x - here.x);
            let (arrived, seen) = connection.walk(
                character.guid,
                here,
                heading,
                flat - CLOSE_ENOUGH,
                GHOST_SPEED,
            )?;
            if let Some(capture) = capture.as_mut() {
                capture.record(&seen)?;
            }
            state.replicate(&seen, None);
            here = arrived;
            here.z = there.z;
            let left = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
            println!("  ran back, {left:.1} units short of the body");
        }
    } else {
        println!("  no idea where we are, so no run back -- reclaiming from where we stand");
    }

    // **Wait out the delay the server named, rather than sending and hoping.**
    // The body cannot be taken back until it has lain there for
    // `SMSG_CORPSE_RECLAIM_DELAY`, and a request sent early is refused in
    // silence like the other four refusals -- so an impatient client cannot
    // tell "too soon" from "wrong opcode". The delay is not a constant either:
    // it was 30s on a first death, 60s on the second and 120s on the third,
    // because dying repeatedly stacks it. Anything that hardcoded thirty
    // seconds would work exactly once per character.
    if let Some(delay) = state.reclaim_delay_ms {
        // Capped so a server with an unusual penalty cannot hang the run; the
        // cap is announced rather than silently applied.
        const CAP_MS: u32 = 180_000;
        let waiting = delay.min(CAP_MS);
        if waiting < delay {
            println!("  delay is {delay}ms; waiting only {waiting}ms and expecting a refusal");
        } else {
            println!("  waiting {waiting}ms for the corpse to become reclaimable");
        }
        let until = Instant::now() + Duration::from_millis(waiting as u64 + 1_000);
        while Instant::now() < until {
            // Drained rather than slept: a session that stops answering
            // keepalives is dropped, and the drop looks exactly like a
            // desynchronised cipher.
            let batch = connection.drain(Duration::from_millis(1_000), 128)?;
            if let Some(capture) = capture.as_mut() {
                capture.record(&batch)?;
            }
            state.replicate(&batch, None);
            answer_teleport(connection, state, character.guid)?;
        }
    }

    connection.reclaim_corpse(corpse)?;

    for _ in 0..6 {
        let batch = connection.drain(Duration::from_millis(700), 128)?;
        if let Some(capture) = capture.as_mut() {
            capture.record(&batch)?;
        }
        let report = state.replicate(&batch, None);
        print_events(&report, state, character.guid);
        answer_teleport(connection, state, character.guid)?;
    }

    let entity = state.get(character.guid);
    println!(
        "  ghost after = {}, health {:?}",
        entity.is_some_and(|e| e.is_ghost()),
        entity.and_then(|e| e.health())
    );
    Ok(())
}

/// Asks which opcodes in a capture carry relayed movement, structurally.
///
/// See [`Command::Moves`] for why this is asked rather than looked up. The
/// discriminator has three parts, and the third is the one that matters:
///
/// - the body parses as a packed guid followed by a `MovementInfo`,
/// - **and consumes the body exactly** -- leftovers mean the layout is not this
///   one, which is the check that has caught four separate world-protocol bugs
///   here,
/// - **and the guids it names are few and repeat.** Any body of the right
///   length parses as *something*; a movement opcode names the same one or two
///   movers over and over, because there are one or two movers in view. An
///   opcode that yields a different guid every time is being misread. That is
///   the validity-versus-variation rule, pointed the other way round: here the
///   *lack* of variation is the evidence.
fn report_capture_moves(path: &std::path::Path, detail: Option<&str>) -> Result<()> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Tally {
        packets: usize,
        parsed: usize,
        leftovers: usize,
        failed: usize,
        movers: BTreeMap<u64, usize>,
        times: Vec<(u64, u32)>,
    }

    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading capture {}", path.display()))?;
    let mut tallies: BTreeMap<u16, Tally> = BTreeMap::new();

    for line in text.lines() {
        // `<opcode> <name> <len> <hex bytes...>`, as `Capture::record` writes it
        // -- but **the name is not one token**. `opcode::describe` renders
        // anything it does not know as `opcode 0x00da`, with a space in it, so
        // splitting on whitespace and taking the third field lands on the
        // *length* for exactly the opcodes this command exists to investigate.
        // That shifted every unknown body by one byte and reported them all as
        // "not movement" -- a confident negative result produced entirely by
        // the tool's own formatting, and the second time in this project that
        // a capture has been seen and effectively thrown away.
        //
        // So the body is found from the right instead: it is the trailing run
        // of two-character hex tokens, and the token before it must be its
        // length. Cross-checking those two is what makes the parse
        // self-verifying rather than merely different.
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(opcode) = tokens.first() else {
            continue;
        };
        let Ok(opcode) = u16::from_str_radix(opcode.trim_start_matches("0x"), 16) else {
            continue;
        };
        // The length field is the anchor, found by *agreeing with what follows
        // it*: the first token that parses as a number equal to the count of
        // tokens after it. Scanned left to right, not right to left -- a body
        // ending in `00` also satisfies "parses as 0, and 0 tokens follow", so
        // searching from the right finds a byte rather than the length. And
        // the length cannot simply be taken as the third token, because
        // `opcode::describe` renders an unknown opcode as `opcode 0x00da`,
        // which is two tokens; nor can the body be taken as the trailing run
        // of two-hex-digit tokens, because a 32-byte packet's length is `32`,
        // which is itself two hex digits.
        //
        // Each of those three readings was tried and each produced a confident,
        // wrong answer with no error anywhere -- the first reported every
        // unknown opcode as "not movement", which is precisely the conclusion
        // this command exists to avoid drawing by accident.
        let Some(length_at) = (1..tokens.len())
            .find(|index| tokens[*index].parse::<usize>() == Ok(tokens.len() - index - 1))
        else {
            continue;
        };
        let body: Vec<u8> = tokens[length_at + 1..]
            .iter()
            .filter_map(|byte| u8::from_str_radix(byte, 16).ok())
            .collect();
        if body.len() != tokens.len() - length_at - 1 {
            continue;
        }

        let tally = tallies.entry(opcode).or_default();
        tally.packets += 1;

        let mut reader = world::protocol::Reader::new(&body, "capture");
        let parsed = world::update::read_packed_guid(&mut reader)
            .and_then(|guid| world::movement::MovementInfo::read(&mut reader).map(|i| (guid, i)));
        match parsed {
            Ok((guid, info)) => {
                if reader.remaining() == 0 {
                    tally.parsed += 1;
                    *tally.movers.entry(guid).or_default() += 1;
                    tally.times.push((guid, info.time));
                } else {
                    tally.leftovers += 1;
                }
            }
            Err(_) => tally.failed += 1,
        }
    }

    println!(
        "{:<8} {:>7} {:>7} {:>6} {:>6} {:>7}  movers",
        "opcode", "packets", "movement", "extra", "refused", "handled"
    );
    for (opcode, tally) in &tallies {
        // Nothing to say about an opcode that never looked like movement.
        if tally.parsed == 0 {
            continue;
        }
        let handled = world::opcode::is_relayed_movement(*opcode);
        println!(
            "{:#06x} {:>7} {:>7} {:>6} {:>6} {:>7}  {}",
            opcode,
            tally.packets,
            tally.parsed,
            tally.leftovers,
            tally.failed,
            if handled { "yes" } else { "NO" },
            tally.movers.len()
        );
    }

    if let Some(detail) = detail {
        let Ok(wanted) = u16::from_str_radix(detail.trim_start_matches("0x"), 16) else {
            anyhow::bail!("--detail wants an opcode like 0x00da");
        };
        let Some(tally) = tallies.get(&wanted) else {
            anyhow::bail!("no {wanted:#06x} in this capture");
        };
        println!("\n{wanted:#06x} in detail:");
        for (guid, count) in &tally.movers {
            println!("  mover {guid:#x}: {count} sample(s)");
        }
        // The mover's own clock, differenced. This is the number the relayed
        // path duration is taken from, so seeing it directly is the point:
        // a run whose intervals are all 100ms came from this client, and one
        // spread around a few hundred did not.
        let mut previous: BTreeMap<u64, u32> = BTreeMap::new();
        let mut intervals = Vec::new();
        for (guid, time) in &tally.times {
            if let Some(before) = previous.insert(*guid, *time) {
                intervals.push(time.wrapping_sub(before));
            }
        }
        intervals.sort_unstable();
        if !intervals.is_empty() {
            let median = intervals[intervals.len() / 2];
            println!(
                "  {} interval(s): min {}ms, median {median}ms, max {}ms",
                intervals.len(),
                intervals[0],
                intervals[intervals.len() - 1]
            );
        }
    }
    Ok(())
}

/// How the relayed movement of *other* players was handled.
///
/// Silent when none arrived, which is the common case: a run with nobody else
/// in view has nothing to say here, and a line of zeroes would read as a
/// failure rather than as an empty room.
///
/// This exists because the fix for `foss-wow#22` rests on a claim about another
/// client's behaviour -- that it samples its own position every few hundred
/// milliseconds -- and the claim has never been measured. Two copies of this
/// client agreeing proves nothing about it: they share a 100ms heartbeat no
/// real client sends, which is precisely how 3.5 came to declare replicated
/// players smooth when they were not. The mean interval below is that
/// measurement, and it only means anything when the mover at the other end is
/// software this project did not write.
fn report_relayed_movement(stats: &world::state::Stats) {
    let snapped = stats.relayed_first_sample + stats.relayed_gap + stats.relayed_teleport;
    let total = stats.relayed_paths + snapped;
    if total == 0 {
        return;
    }
    print!(
        "  relayed movement: {total} sample(s), {} walked",
        stats.relayed_paths
    );
    if stats.relayed_paths > 0 {
        print!(
            " (mean interval {}ms)",
            stats.relayed_interval_ms / stats.relayed_paths as u64
        );
    }
    println!();
    // Broken out rather than summed: these are three different statements, and
    // only one of them would be a defect. A first sighting has nothing to
    // measure against, and a gap is a mover who stopped and started again --
    // both snap correctly. A teleport that is not a teleport would be the
    // interesting one.
    println!(
        "    snapped: {} first sighting, {} after a pause, {} too fast to be a walk",
        stats.relayed_first_sample, stats.relayed_gap, stats.relayed_teleport
    );
}

/// Chooses which realm to enter.
///
/// With one realm the choice is obvious; with several, guessing would silently
/// send the player somewhere they did not ask for, so an ambiguous case lists
/// the options and stops.
fn pick_realm<'a>(realms: &'a [auth::Realm], wanted: Option<&str>) -> Result<&'a auth::Realm> {
    if let Some(wanted) = wanted {
        return realms
            .iter()
            .find(|realm| realm.name.eq_ignore_ascii_case(wanted))
            .with_context(|| {
                let names: Vec<&str> = realms.iter().map(|realm| realm.name.as_str()).collect();
                format!("no realm named {wanted:?}; this account sees {names:?}")
            });
    }
    match realms {
        [] => anyhow::bail!("the logon server offered no realms"),
        [only] => Ok(only),
        many => {
            let names: Vec<&str> = many.iter().map(|realm| realm.name.as_str()).collect();
            anyhow::bail!("{} realms available: {names:?}; pass --realm", many.len())
        }
    }
}

fn adt_cmd(chain: &mut Chain, cmd: AdtCommand) -> Result<()> {
    match cmd {
        AdtCommand::Map { map } => adt_map(chain, &map),
        AdtCommand::Tile { map, x, y, limit } => adt_tile(chain, &map, x, y, limit),
        AdtCommand::Survey { map, limit } => adt_survey(chain, map.as_deref(), limit),
        AdtCommand::Height { map, x, y } => adt_height(chain, &map, x, y),
        AdtCommand::Liquid { map, limit } => adt_liquid(chain, map.as_deref(), limit),
    }
}

/// Resolves a world position to its tile, its chunk and the ground under it.
///
/// The point of printing every step rather than just the answer: a height that
/// is wrong because the position landed on the wrong *tile* and one that is
/// wrong because the interpolation is wrong look identical as a single number,
/// and want completely different investigations.
fn adt_height(chain: &mut Chain, map: &str, x: f32, y: f32) -> Result<()> {
    // Both axes run inwards from the grid corner, and they swap: see
    // `docs/RENDERING.md`.
    let tile = (
        (32.0 - y / adt::TILE_SIZE).floor() as i64,
        (32.0 - x / adt::TILE_SIZE).floor() as i64,
    );
    println!("{map} at {x:.2}, {y:.2}");
    println!("  tile {},{}", tile.0, tile.1);
    if !(0..adt::TILES_PER_MAP as i64).contains(&tile.0)
        || !(0..adt::TILES_PER_MAP as i64).contains(&tile.1)
    {
        anyhow::bail!("that position is off the {map} grid");
    }

    let wdt = load_wdt(chain, map)?;
    if !wdt.has_tile(tile.0 as usize, tile.1 as usize) {
        anyhow::bail!("{map} has no tile at {},{}", tile.0, tile.1);
    }
    let path = adt::tile_path(map, tile.0 as usize, tile.1 as usize);
    let bytes = chain.read(&path).with_context(|| format!("reading {path}"))?;
    let parsed = adt::Adt::parse(&bytes, wdt.big_alpha())?;

    // Whichever chunk's footprint contains the point, found by asking rather
    // than by indexing: this is a tool for checking the convention, so it
    // should not assume it.
    let found = parsed
        .chunks
        .iter()
        .enumerate()
        .find_map(|(i, chunk)| chunk.height_at(x, y).map(|h| (i, chunk, h)));
    match found {
        Some((index, chunk, height)) => {
            println!(
                "  chunk {index} (stored index {},{}) with origin {:.2}, {:.2}, {:.2}",
                chunk.index.0, chunk.index.1, chunk.position[0], chunk.position[1], chunk.position[2]
            );
            println!("  ground at {height:.3}");
        }
        None => {
            // A hole is the ordinary reason, and worth saying so: it means the
            // ADT genuinely describes no terrain there, not that anything
            // failed.
            let holed = parsed.chunks.iter().filter(|c| c.holes != 0).count();
            println!("  no ground here -- a hole, or off this tile ({holed} chunks on the tile have holes)");
        }
    }
    Ok(())
}

/// Loads a map's WDT, which is where the alpha-map storage flag lives.
fn load_wdt(chain: &mut Chain, map: &str) -> Result<adt::Wdt> {
    let path = adt::wdt_path(map);
    let bytes = chain.read(&path).with_context(|| format!("reading {path}"))?;
    adt::Wdt::parse(&bytes).with_context(|| format!("parsing {path}"))
}

fn adt_map(chain: &mut Chain, map: &str) -> Result<()> {
    let wdt = load_wdt(chain, map)?;
    println!("{}", adt::wdt_path(map));
    println!(
        "  flags {:#x}, {} of {} tiles present, alpha maps are {}-bit",
        wdt.flags,
        wdt.tile_count(),
        adt::TILES_PER_MAP * adt::TILES_PER_MAP,
        if wdt.big_alpha() { 8 } else { 4 }
    );

    // A coarse picture of which part of the grid the map occupies.
    let tiles = wdt.tiles();
    if let (Some(min_x), Some(max_x)) = (
        tiles.iter().map(|t| t.0).min(),
        tiles.iter().map(|t| t.0).max(),
    ) {
        let min_y = tiles.iter().map(|t| t.1).min().unwrap_or(0);
        let max_y = tiles.iter().map(|t| t.1).max().unwrap_or(0);
        println!("  occupied region: x {min_x}..={max_x}, y {min_y}..={max_y}");
        println!("  first tiles: {:?}", &tiles[..tiles.len().min(6)]);
    }
    Ok(())
}

fn adt_tile(chain: &mut Chain, map: &str, x: usize, y: usize, limit: usize) -> Result<()> {
    let wdt = load_wdt(chain, map)?;
    let path = adt::tile_path(map, x, y);
    let bytes = chain.read(&path).with_context(|| format!("reading {path}"))?;
    let tile = adt::Adt::parse(&bytes, wdt.big_alpha())?;

    println!("{path}");
    println!(
        "  {} textures, {} doodad models, {} object models",
        tile.textures.len(),
        tile.doodad_models.len(),
        tile.object_models.len()
    );
    println!(
        "  {} doodad placements, {} world object placements",
        tile.doodads.len(),
        tile.objects.len()
    );

    let heights: Vec<f32> = tile
        .chunks
        .iter()
        .flat_map(|c| c.heights.iter().map(move |h| h + c.position[2]))
        .collect();
    let low = heights.iter().copied().fold(f32::MAX, f32::min);
    let high = heights.iter().copied().fold(f32::MIN, f32::max);
    println!("  elevation {low:.1} to {high:.1}");
    match tile.validate() {
        Ok(()) => println!("  chunk edges meet"),
        Err(e) => println!("  SEAM: {e}"),
    }

    println!("\n  textures:");
    for texture in tile.textures.iter().take(limit) {
        println!("    {texture}");
    }

    println!("\n  chunks (first {limit}):");
    for c in tile.chunks.iter().take(limit) {
        println!(
            "    {:>2},{:<2} area {:>5} {} layers, {} alpha maps, {} doodads, {} objects{}",
            c.index.0,
            c.index.1,
            c.area_id,
            c.layers.len(),
            c.alpha_maps.len(),
            c.doodad_refs.len(),
            c.object_refs.len(),
            if c.holes != 0 { format!(" holes {:#06x}", c.holes) } else { String::new() },
        );
    }

    if !tile.objects.is_empty() {
        println!("\n  world objects placed here:");
        for o in tile.objects.iter().take(limit) {
            println!(
                "    {} at [{:.0} {:.0} {:.0}] set {}",
                o.path, o.position[0], o.position[1], o.position[2], o.doodad_set
            );
        }
    }
    Ok(())
}

/// Sweeps a map's liquid and asks the one question that can refute the reader.
///
/// See [`AdtCommand::Liquid`] for why the question is "is the water in the low
/// ground" rather than "did it parse". Both axis readings parse every byte of
/// every file; only one of them puts rivers in valleys.
fn adt_liquid(chain: &mut Chain, map: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    // Names for the type ids, so the tally reads as `Slow Water` rather than
    // `5`. A missing table is not fatal: the geometry question this answers
    // does not depend on knowing what the liquid is called.
    let types = dbc::schema::LiquidType::parse(&chain.read(dbc::schema::LiquidType::PATH)?).ok();
    let describe = |id: u16| -> String {
        let Some(table) = types.as_ref() else {
            return format!("type {id}");
        };
        match table.iter().find(|row| row.id() == id as u32) {
            Some(row) => format!("{id} {} ({:?})", row.name(), row.kind()),
            None => format!("{id} <not in LiquidType.dbc>"),
        }
    };

    let maps: Vec<String> = match map {
        Some(m) => vec![m.to_string()],
        None => {
            let table = dbc::schema::Map::parse(&chain.read(dbc::schema::Map::PATH)?)?;
            let mut names: Vec<String> = table
                .iter()
                .map(|m| m.directory().to_string())
                .filter(|d| !d.is_empty())
                .collect();
            names.sort_unstable();
            names.dedup();
            names
        }
    };

    let mut by_type: BTreeMap<u16, u64> = BTreeMap::new();
    // One tile per type, so a type that exists somewhere can actually be gone
    // and looked at. A survey that says "123 sheets of Slow Magma" and cannot
    // say *where* leaves the only lava in the game untestable.
    let mut example: BTreeMap<u16, String> = BTreeMap::new();
    let mut by_format: BTreeMap<String, u64> = BTreeMap::new();
    let (mut tiles_read, mut tiles_wet, mut chunks_wet, mut instances) = (0u64, 0u64, 0u64, 0u64);
    // The measurement. `chosen` is the reading the parser uses; `swapped` is
    // the same rectangle with its two axes exchanged. A cell counts as sane
    // when its liquid surface sits at or above the terrain under it.
    //
    // **The decisive tally is the one that matters.** A full 8x8 sheet lying
    // flat is symmetric under transposition, so it agrees with both readings
    // and contributes nothing -- and the open ocean, which is most of the
    // liquid in the game, is exactly that. Counting every cell buries the few
    // hundred that can actually answer under a million that cannot, which is
    // the same way `Light.dbc`'s storm column came back a coin flip.
    let (mut cells, mut chosen_above, mut swapped_above) = (0u64, 0u64, 0u64);
    let (mut decisive, mut decisive_chosen, mut decisive_swapped) = (0u64, 0u64, 0u64);
    let mut budget = limit.unwrap_or(usize::MAX);

    for name in &maps {
        let Ok(wdt) = load_wdt(chain, name) else {
            continue;
        };
        for (x, y) in wdt.tiles() {
            if budget == 0 {
                break;
            }
            let Ok(bytes) = chain.read(&adt::tile_path(name, x, y)) else {
                continue;
            };
            budget -= 1;
            let Ok(tile) = adt::Adt::parse(&bytes, wdt.big_alpha()) else {
                continue;
            };
            tiles_read += 1;
            if tile.liquid.is_empty() {
                continue;
            }
            tiles_wet += 1;
            chunks_wet += tile.liquid.chunks_with_liquid() as u64;

            for (index, sheet) in tile.liquid.instances() {
                instances += 1;
                *by_type.entry(sheet.liquid_type).or_default() += 1;
                example
                    .entry(sheet.liquid_type)
                    .or_insert_with(|| format!("{name} {x},{y}"));
                *by_format
                    .entry(format!("{:?}", sheet.vertex_format))
                    .or_default() += 1;

                let Some(chunk) = tile.chunks.get(index) else {
                    continue;
                };
                // **Only an asymmetric rectangle can vote.** Transposing a
                // sheet that covers the whole chunk -- which is what the open
                // ocean is, 86,222 of the 92,219 sheets in Azeroth -- produces
                // the identical footprint, so it agrees with both readings
                // whatever the terrain beneath it does. Letting those in
                // drowns the few thousand partial rectangles that genuinely
                // land somewhere else under a million that cannot move.
                let asymmetric =
                    (sheet.x_offset, sheet.width) != (sheet.y_offset, sheet.height);
                for j in 0..sheet.height as usize {
                    for i in 0..sheet.width as usize {
                        if !sheet.cell_exists(i, j) {
                            continue;
                        }
                        cells += 1;
                        // The cell's centre, half a unit in from its corner.
                        let major = sheet.y_offset as f32 + j as f32 + 0.5;
                        let minor = sheet.x_offset as f32 + i as f32 + 0.5;
                        // Corner-relative interpolation of the sheet is the
                        // parser's business; here the flat level is enough,
                        // and using it keeps the two readings comparable.
                        let level = sheet.vertex_height(i, j);

                        let ground = |a: f32, b: f32| -> Option<f32> {
                            let gx = chunk.position[0] - a * adt::UNIT_SIZE;
                            let gy = chunk.position[1] - b * adt::UNIT_SIZE;
                            chunk.height_at(gx, gy)
                        };
                        let here = ground(major, minor);
                        let there = ground(minor, major);
                        let ok = |g: Option<f32>| g.is_some_and(|g| level >= g);
                        if ok(here) {
                            chosen_above += 1;
                        }
                        if ok(there) {
                            swapped_above += 1;
                        }
                        // A cell only votes when the two readings sample
                        // genuinely different ground. Where they land on the
                        // same height -- a flat sheet, or a symmetric position
                        // within it -- both are right and neither is evidence.
                        let differs = match (here, there) {
                            (Some(a), Some(b)) => (a - b).abs() > 0.05,
                            (a, b) => a.is_some() != b.is_some(),
                        };
                        if asymmetric && differs {
                            decisive += 1;
                            if ok(here) {
                                decisive_chosen += 1;
                            }
                            if ok(there) {
                                decisive_swapped += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    println!("liquid across {} map(s), {tiles_read} tiles read", maps.len());
    println!("  {tiles_wet} tiles carry liquid, {chunks_wet} chunks, {instances} sheets");
    println!();
    println!("  by type:");
    for (id, count) in &by_type {
        println!("    {count:>8}  {}", describe(*id));
        if let Some(where_) = example.get(id) {
            println!("             first seen on tile {where_}");
        }
    }
    println!("  by vertex format:");
    for (format, count) in &by_format {
        println!("    {count:>8}  {format}");
    }
    println!();
    println!("  is the water in the low ground? ({cells} cells with terrain under them)");
    let percent = |n: u64| if cells == 0 { 0.0 } else { n as f64 * 100.0 / cells as f64 };
    println!(
        "    as read      {chosen_above:>8} at or above the ground  ({:.1}%)",
        percent(chosen_above)
    );
    println!(
        "    axes swapped {swapped_above:>8} at or above the ground  ({:.1}%)",
        percent(swapped_above)
    );
    println!();
    println!(
        "  of those, {decisive} cells in a rectangle that transposing actually moves, \
         over ground that differs -- the only ones that can answer:"
    );
    let share = |n: u64| {
        if decisive == 0 {
            0.0
        } else {
            n as f64 * 100.0 / decisive as f64
        }
    };
    println!(
        "    as read      {decisive_chosen:>8} above the ground  ({:.1}%)",
        share(decisive_chosen)
    );
    println!(
        "    axes swapped {decisive_swapped:>8} above the ground  ({:.1}%)",
        share(decisive_swapped)
    );
    println!();
    if decisive == 0 {
        println!(
            "  no cell could tell the two readings apart. This population cannot \
             answer the question -- sweep a map with rivers rather than open sea."
        );
    } else if decisive_swapped > decisive_chosen {
        println!(
            "  the swapped reading fits better -- `LiquidInstance::height_at` has \
             its two axes the wrong way round"
        );
    } else {
        println!("  the reading in use fits best");
    }
    Ok(())
}

fn adt_survey(chain: &mut Chain, map: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    // Maps are named by their directory, which is what Map.dbc records.
    let maps: Vec<String> = match map {
        Some(m) => vec![m.to_string()],
        None => {
            let table = dbc::schema::Map::parse(&chain.read(dbc::schema::Map::PATH)?)?;
            let mut names: Vec<String> = table
                .iter()
                .map(|m| m.directory().to_string())
                .filter(|d| !d.is_empty())
                .collect();
            names.sort_unstable();
            names.dedup();
            names
        }
    };

    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut tiles_ok, mut tiles_missing, mut maps_ok) = (0usize, 0usize, 0usize);
    let (mut doodads, mut objects, mut budget) = (0u64, 0u64, limit.unwrap_or(usize::MAX));

    for name in &maps {
        let wdt = match load_wdt(chain, name) {
            Ok(w) => w,
            Err(_) => continue,
        };
        maps_ok += 1;
        for (x, y) in wdt.tiles() {
            if budget == 0 {
                break;
            }
            let path = adt::tile_path(name, x, y);
            let Ok(bytes) = chain.read(&path) else {
                tiles_missing += 1;
                continue;
            };
            budget -= 1;
            match adt::Adt::parse(&bytes, wdt.big_alpha()) {
                Ok(tile) => {
                    tiles_ok += 1;
                    doodads += tile.doodads.len() as u64;
                    objects += tile.objects.len() as u64;
                    if let Err(e) = tile.validate() {
                        let key = format!("edges: {}", first_clause(&e));
                        failures.entry(key).or_insert((0, path.clone())).0 += 1;
                    }
                }
                Err(e) => {
                    let key = e.to_string();
                    let key = key.split(" (").next().unwrap_or(&key).to_string();
                    failures.entry(key).or_insert((0, path.clone())).0 += 1;
                }
            }
        }
        if budget == 0 {
            break;
        }
        tracing::info!("{name}: done");
    }

    println!("\n{maps_ok} maps, {tiles_ok} tiles parsed, {tiles_missing} declared but absent");
    println!("  {doodads} doodad placements, {objects} world object placements");
    if failures.is_empty() {
        println!("\nno failures");
    } else {
        println!("\nfailures:");
        for (kind, (count, example)) in &failures {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

fn minimap_cmd(chain: &mut Chain, cmd: MinimapCommand) -> Result<()> {
    match cmd {
        MinimapCommand::Index { verify } => minimap_index(chain, verify),
        MinimapCommand::Tiles { map } => minimap_tiles(chain, map.as_deref()),
        MinimapCommand::Orient { map, limit } => minimap_orient(chain, map.as_deref(), limit),
        MinimapCommand::Seams { map, limit } => minimap_seams(chain, map.as_deref(), limit),
        MinimapCommand::Stitch {
            map,
            x,
            y,
            range,
            pixels,
            out,
        } => minimap_stitch(chain, &map, x, y, range, pixels, out),
        MinimapCommand::Export { map, x, y, out } => minimap_export(chain, &map, x, y, out),
    }
}

fn load_minimap_index(chain: &mut Chain) -> Result<adt::minimap::Translate> {
    let path = adt::minimap::TRANSLATE_PATH;
    let bytes = chain.read(path).with_context(|| format!("reading {path}"))?;
    // Lossy on purpose: the file is ASCII apart from a handful of directory
    // names, and one unmappable byte must not cost the other 18,643 entries.
    Ok(adt::minimap::Translate::parse(&String::from_utf8_lossy(
        &bytes,
    )))
}

/// Map directories from `Map.dbc`, optionally narrowed to one.
///
/// From the table rather than from the index's own directory list, because
/// the question this command exists to ask is whether the two agree.
fn minimap_directories(chain: &mut Chain, only: Option<&str>) -> Result<Vec<String>> {
    let table = dbc::schema::Map::parse(&chain.read(dbc::schema::Map::PATH)?)?;
    let mut names: Vec<String> = table
        .iter()
        .map(|m| m.directory().to_string())
        .filter(|d| !d.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    if let Some(only) = only {
        names.retain(|d| d.eq_ignore_ascii_case(only));
        if names.is_empty() {
            anyhow::bail!("no map directory called {only}");
        }
    }
    Ok(names)
}

fn minimap_index(chain: &mut Chain, verify: bool) -> Result<()> {
    use std::collections::{BTreeMap, HashMap};

    let index = load_minimap_index(chain)?;
    let mut uses: HashMap<&str, usize> = HashMap::new();
    let (mut terrain, mut other) = (0usize, 0usize);
    let mut per_map: BTreeMap<String, usize> = BTreeMap::new();
    for (logical, file) in index.iter() {
        *uses.entry(file).or_default() += 1;
        match logical.rsplit_once('\\') {
            Some((dir, name)) if !dir.contains('\\') && name.starts_with("map") => {
                terrain += 1;
                *per_map.entry(dir.to_string()).or_default() += 1;
            }
            _ => other += 1,
        }
    }
    let shared: usize = uses.values().filter(|n| **n > 1).count();
    let mut most: Vec<(&&str, &usize)> = uses.iter().collect();
    most.sort_by(|a, b| b.1.cmp(a.1));

    println!("{}", adt::minimap::TRANSLATE_PATH);
    println!("  {} entries naming {} distinct files", index.len(), uses.len());
    println!("  {terrain} terrain tiles across {} maps, {other} elsewhere (WMO interiors)", per_map.len());
    println!("  {shared} files serve more than one tile -- so a file name does not identify a tile");
    for (file, count) in most.iter().take(3) {
        println!("    {file} is named {count} times");
    }

    if verify {
        // Resolution by path, never a listing: an MPQ answers by hash, and a
        // file absent from `(listfile)` reads perfectly. That distinction has
        // already sunk one coverage check in this project.
        let mut checked: Vec<&str> = uses.keys().copied().collect();
        checked.sort_unstable();
        let missing: Vec<&str> = checked
            .iter()
            .copied()
            .filter(|file| !chain.contains(&adt::minimap::art_path(file)))
            .collect();
        println!(
            "\n  resolved {} of {} referenced files by path",
            checked.len() - missing.len(),
            checked.len()
        );
        for file in missing.iter().take(20) {
            println!("    missing {file}");
        }
    }

    println!("\n{:<28} {:>6}", "map", "tiles");
    for (dir, count) in &per_map {
        println!("{dir:<28} {count:>6}");
    }
    Ok(())
}

/// Compare each map's minimap tile set against its `WDT`'s, in both orders.
///
/// See [`MinimapCommand::Tiles`] for why the *set* is the evidence and a
/// single successful lookup is not.
fn minimap_tiles(chain: &mut Chain, map: Option<&str>) -> Result<()> {
    use std::collections::BTreeSet;

    let index = load_minimap_index(chain)?;
    let directories = minimap_directories(chain, map)?;

    println!(
        "{:<24} {:>7} {:>7} {:>12} {:>12}",
        "map", "terrain", "minimap", "as written", "transposed"
    );
    let (mut maps, mut separating) = (0usize, 0usize);
    let (mut exact, mut exact_transposed) = (0usize, 0usize);
    let (mut total_terrain, mut total_named) = (0u64, 0u64);
    let (mut total_matched, mut total_matched_transposed) = (0u64, 0u64);
    for directory in &directories {
        let named: BTreeSet<(usize, usize)> = index.tiles(directory).into_iter().collect();
        let Ok(wdt) = load_wdt(chain, directory) else {
            if !named.is_empty() {
                println!("{directory:<24} {:>7} {:>7}", "no wdt", named.len());
            }
            continue;
        };
        let terrain: BTreeSet<(usize, usize)> = wdt.tiles().into_iter().collect();
        if terrain.is_empty() && named.is_empty() {
            continue;
        }
        maps += 1;
        let flipped: BTreeSet<(usize, usize)> = named.iter().map(|(a, b)| (*b, *a)).collect();
        let hit = named.intersection(&terrain).count();
        let hit_flipped = flipped.intersection(&terrain).count();
        total_terrain += terrain.len() as u64;
        total_named += named.len() as u64;
        total_matched += hit as u64;
        total_matched_transposed += hit_flipped as u64;
        // A map whose tile set is symmetric under exchanging the pair agrees
        // with both orders and votes for neither -- the square instance maps
        // are all like that. Counting them among the agreements would make a
        // unanimous result unanimous about nothing.
        if hit != hit_flipped {
            separating += 1;
        }
        if hit == terrain.len() && hit == named.len() {
            exact += 1;
        }
        if hit_flipped == terrain.len() && hit_flipped == named.len() {
            exact_transposed += 1;
        }
        println!(
            "{directory:<24} {:>7} {:>7} {:>12} {:>12}",
            terrain.len(),
            named.len(),
            hit,
            hit_flipped
        );
    }

    println!("\n{maps} maps, {total_terrain} terrain tiles, {total_named} tiles named");
    println!("  as written: {total_matched} tiles land on real terrain, {exact} maps exact");
    println!(
        "  transposed: {total_matched_transposed} tiles land on real terrain, \
         {exact_transposed} maps exact"
    );
    println!("  {separating} of {maps} maps can tell the two readings apart at all");
    Ok(())
}

/// The eight ways a tile's texels could be laid out over its chunks.
///
/// `(row, col)` is the chunk's position in the tile measured from the corner
/// its own stored world position identifies -- so the terrain half of this
/// comparison presupposes nothing about the art. The result is `(across,
/// down)` in chunk-sized blocks.
type Orientation = fn(usize, usize, usize) -> (usize, usize);

const ORIENTATIONS: [(&str, Orientation); 8] = [
    ("as drawn", |row, col, _| (col, row)),
    ("across flipped", |row, col, n| (n - 1 - col, row)),
    ("down flipped", |row, col, n| (col, n - 1 - row)),
    ("both flipped", |row, col, n| (n - 1 - col, n - 1 - row)),
    ("transposed", |row, col, _| (row, col)),
    ("transposed, across flipped", |row, col, n| (n - 1 - row, col)),
    ("transposed, down flipped", |row, col, n| (row, n - 1 - col)),
    ("transposed, both flipped", |row, col, n| (n - 1 - row, n - 1 - col)),
];

/// How blue a block has to be before it is called water, in 0..255.
///
/// Stated once and reported alongside the separation it produces, because a
/// threshold picked to make an answer come out is not evidence. The
/// separation between the wet and dry populations is printed so the number
/// can be seen to be arbitrary within a wide margin rather than load-bearing.
const BLUE_MARGIN: f32 = 12.0;

fn minimap_orient(chain: &mut Chain, map: Option<&str>, limit: Option<usize>) -> Result<()> {
    use dbc::schema::{LiquidCategory, LiquidType};

    let index = load_minimap_index(chain)?;
    let directories = minimap_directories(chain, map)?;
    // Which liquid ids are blue. Lava and slime are liquid and are not water,
    // and a chunk under either is excluded rather than counted as dry: its
    // block is neither blue nor plain ground, so it can only add noise.
    let liquids = LiquidType::parse(&chain.read(LiquidType::PATH)?)?;
    let category = |id: u16| -> Option<LiquidCategory> {
        liquids
            .iter()
            .find(|row| row.id() == id as u32)
            .map(|row| row.kind())
    };

    // Per candidate: agreements over every classified chunk, and over the
    // chunks the candidates actually disagree about.
    let mut agree = [0u64; ORIENTATIONS.len()];
    let mut agree_decisive = [0u64; ORIENTATIONS.len()];
    // Mean blueness of the wet and dry populations under each candidate, so
    // the result is a separation and not only a percentage.
    let mut wet_blue = [0f64; ORIENTATIONS.len()];
    let mut dry_blue = [0f64; ORIENTATIONS.len()];
    let (mut wet_n, mut dry_n) = (0u64, 0u64);
    let (mut scored, mut decisive) = (0u64, 0u64);
    let (mut tiles_read, mut tiles_without_art) = (0u64, 0u64);
    let mut budget = limit.unwrap_or(usize::MAX);

    'maps: for directory in &directories {
        let Ok(wdt) = load_wdt(chain, directory) else {
            continue;
        };
        for (tx, ty) in wdt.tiles() {
            if budget == 0 {
                break 'maps;
            }
            let Ok(bytes) = chain.read(&adt::tile_path(directory, tx, ty)) else {
                continue;
            };
            let Ok(tile) = adt::Adt::parse(&bytes, wdt.big_alpha()) else {
                continue;
            };
            // A tile with no water cannot separate anything, and reading its
            // art costs a megabyte of decode. Skipping it is not a filter on
            // the result -- it is a filter on chunks that were never going to
            // vote.
            if tile.liquid.is_empty() || tile.chunks.len() != adt::CHUNK_COUNT {
                continue;
            }
            let Some(art) = index.tile_path(directory, tx, ty) else {
                tiles_without_art += 1;
                continue;
            };
            let Ok(art_bytes) = chain.read(&art) else {
                tiles_without_art += 1;
                continue;
            };
            let Ok(picture) = blp::Blp::parse(&art_bytes) else {
                tiles_without_art += 1;
                continue;
            };
            let (width, height) = picture.level_size(0);
            let texels = adt::minimap::TILE_TEXELS as u32;
            if (width, height) != (texels, texels) {
                tiles_without_art += 1;
                continue;
            }
            let Some(rgba) = picture.decode_rgba(0) else {
                tiles_without_art += 1;
                continue;
            };
            budget -= 1;
            tiles_read += 1;

            // The tile's origin corner, taken from the chunks themselves. Both
            // axes run inwards from it, so it is the largest of each.
            let origin_x = tile
                .chunks
                .iter()
                .map(|c| c.position[0])
                .fold(f32::MIN, f32::max);
            let origin_y = tile
                .chunks
                .iter()
                .map(|c| c.position[1])
                .fold(f32::MIN, f32::max);

            // Mean blueness of every one of the 256 blocks, computed once.
            let mut blueness = [0f32; adt::CHUNK_COUNT];
            for (block, slot) in blueness.iter_mut().enumerate() {
                let (bu, bv) = (block % adt::CHUNKS_PER_TILE, block / adt::CHUNKS_PER_TILE);
                let (mut red, mut blue) = (0f32, 0f32);
                let step = adt::minimap::TEXELS_PER_CHUNK;
                for v in bv * step..(bv + 1) * step {
                    for u in bu * step..(bu + 1) * step {
                        let at = (v * adt::minimap::TILE_TEXELS + u) * 4;
                        red += rgba[at] as f32;
                        blue += rgba[at + 2] as f32;
                    }
                }
                let texels = (step * step) as f32;
                *slot = (blue - red) / texels;
            }
            let block_at = |bu: usize, bv: usize| blueness[bv * adt::CHUNKS_PER_TILE + bu];

            for (chunk_index, chunk) in tile.chunks.iter().enumerate() {
                let row = ((origin_x - chunk.position[0]) / adt::CHUNK_SIZE).round();
                let col = ((origin_y - chunk.position[1]) / adt::CHUNK_SIZE).round();
                if !(0.0..adt::CHUNKS_PER_TILE as f32).contains(&row)
                    || !(0.0..adt::CHUNKS_PER_TILE as f32).contains(&col)
                {
                    continue;
                }
                let (row, col) = (row as usize, col as usize);

                // Covered, dry, or neither. A shoreline chunk half under water
                // is honestly ambiguous and is dropped rather than guessed at.
                let sheets = tile.liquid.chunk(chunk_index);
                let mut covered = 0usize;
                let mut harmful = false;
                for sheet in sheets {
                    match category(sheet.liquid_type) {
                        Some(LiquidCategory::Water) | Some(LiquidCategory::Ocean) => {}
                        Some(_) => harmful = true,
                        None => harmful = true,
                    }
                    for j in 0..sheet.height as usize {
                        for i in 0..sheet.width as usize {
                            if sheet.cell_exists(i, j) {
                                covered += 1;
                            }
                        }
                    }
                }
                if harmful {
                    continue;
                }
                // Eight cells to a chunk edge, so 64 is complete cover.
                let wet = match covered {
                    0 => false,
                    n if n >= 56 => true,
                    _ => continue,
                };
                scored += 1;
                if wet {
                    wet_n += 1;
                } else {
                    dry_n += 1;
                }

                let calls: Vec<bool> = ORIENTATIONS
                    .iter()
                    .map(|(_, place)| {
                        let (bu, bv) = place(row, col, adt::CHUNKS_PER_TILE);
                        block_at(bu, bv) > BLUE_MARGIN
                    })
                    .collect();
                // Only a chunk the candidates disagree about can separate
                // them. A tile that is all ocean puts the same block under
                // every reading and votes for all eight equally -- the same
                // trap the `MH2O` axis survey had to exclude before its vote
                // meant anything.
                let separates = calls.iter().any(|c| *c != calls[0]);
                if separates {
                    decisive += 1;
                }
                for (i, (_, place)) in ORIENTATIONS.iter().enumerate() {
                    let (bu, bv) = place(row, col, adt::CHUNKS_PER_TILE);
                    let value = block_at(bu, bv);
                    if wet {
                        wet_blue[i] += value as f64;
                    } else {
                        dry_blue[i] += value as f64;
                    }
                    if calls[i] == wet {
                        agree[i] += 1;
                        if separates {
                            agree_decisive[i] += 1;
                        }
                    }
                }
            }
        }
    }

    println!(
        "{tiles_read} wet tiles read, {tiles_without_art} skipped for art that would not load"
    );
    println!("{scored} chunks classified ({wet_n} covered, {dry_n} dry), {decisive} decisive\n");
    if scored == 0 {
        anyhow::bail!("nothing was classified, so this measured nothing");
    }
    println!(
        "{:<28} {:>10} {:>10} {:>18}",
        "reading", "all", "decisive", "wet-dry blueness"
    );
    for (i, (name, _)) in ORIENTATIONS.iter().enumerate() {
        let all = agree[i] as f64 / scored as f64 * 100.0;
        let dec = if decisive == 0 {
            f64::NAN
        } else {
            agree_decisive[i] as f64 / decisive as f64 * 100.0
        };
        let separation = wet_blue[i] / wet_n.max(1) as f64 - dry_blue[i] / dry_n.max(1) as f64;
        println!("{name:<28} {all:>9.1}% {dec:>9.1}% {separation:>18.1}");
    }
    println!(
        "\nblue margin {BLUE_MARGIN:.0}/255. The separation column is threshold-free: it is the \
         mean of (blue - red) over covered chunks minus the same over dry ones."
    );
    Ok(())
}

/// Score the eight readings by whether they make neighbouring tiles join up.
///
/// See [`MinimapCommand::Seams`] for why this is worth running beside
/// `minimap orient`: it reads no terrain at all, so the two experiments share
/// no input but the tile grid.
fn minimap_seams(chain: &mut Chain, map: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeSet;

    let index = load_minimap_index(chain)?;
    let directories = minimap_directories(chain, map)?;
    let edge = adt::minimap::TILE_TEXELS;

    // Summed absolute channel difference along each seam, per candidate and
    // per direction, and how many texel pairs went into each sum.
    let mut across = [0f64; ORIENTATIONS.len()];
    let mut down = [0f64; ORIENTATIONS.len()];
    let (mut across_n, mut down_n) = (0u64, 0u64);
    let (mut seams_across, mut seams_down) = (0u64, 0u64);
    let mut budget = limit.unwrap_or(usize::MAX);

    'maps: for directory in &directories {
        let named = index.tiles(directory);
        let present: BTreeSet<(usize, usize)> = named.iter().copied().collect();
        for (x, y) in &named {
            if budget == 0 {
                break 'maps;
            }
            let Some(here) = minimap_pixels(chain, &index, directory, *x, *y) else {
                continue;
            };
            budget -= 1;

            // A step along `x` continues the direction the in-tile column
            // index runs, and a step along `y` the row index -- which is what
            // `minimap tiles` settled. So the ground at column 255 of this
            // tile abuts the ground at column 0 of the next one along.
            if present.contains(&(x + 1, *y)) {
                if let Some(next) = minimap_pixels(chain, &index, directory, x + 1, *y) {
                    seams_across += 1;
                    for row in 0..edge {
                        for (i, (_, place)) in ORIENTATIONS.iter().enumerate() {
                            let (au, av) = place(row, edge - 1, edge);
                            let (bu, bv) = place(row, 0, edge);
                            across[i] += texel_difference(
                                &here,
                                av * edge + au,
                                &next,
                                bv * edge + bu,
                            );
                        }
                        across_n += 1;
                    }
                }
            }
            if present.contains(&(*x, y + 1)) {
                if let Some(next) = minimap_pixels(chain, &index, directory, *x, y + 1) {
                    seams_down += 1;
                    for col in 0..edge {
                        for (i, (_, place)) in ORIENTATIONS.iter().enumerate() {
                            let (au, av) = place(edge - 1, col, edge);
                            let (bu, bv) = place(0, col, edge);
                            down[i] += texel_difference(
                                &here,
                                av * edge + au,
                                &next,
                                bv * edge + bu,
                            );
                        }
                        down_n += 1;
                    }
                }
            }
        }
    }

    if across_n == 0 || down_n == 0 {
        anyhow::bail!("no neighbouring tiles were compared, so this measured nothing");
    }
    println!("{seams_across} seams across and {seams_down} down\n");
    println!("{:<28} {:>10} {:>10} {:>10}", "reading", "across", "down", "sum");
    for (i, (name, _)) in ORIENTATIONS.iter().enumerate() {
        let a = across[i] / across_n as f64;
        let d = down[i] / down_n as f64;
        println!("{name:<28} {a:>10.2} {d:>10.2} {:>10.2}", a + d);
    }
    println!(
        "\nMean absolute channel difference across a seam, 0..255: lower is a picture that \
         joins up.\n**Neither column can settle this alone.** A reading that flips only the \
         down axis walks the\nsame across-seam backwards and scores identically on it; one \
         that flips only across does the\nsame to the down column. It is the pair together \
         that separates all eight."
    );
    Ok(())
}

/// One tile's texels, or nothing if anything about it would not load.
fn minimap_pixels(
    chain: &mut Chain,
    index: &adt::minimap::Translate,
    map: &str,
    x: usize,
    y: usize,
) -> Option<Vec<u8>> {
    let path = index.tile_path(map, x, y)?;
    let bytes = chain.read(&path).ok()?;
    let picture = blp::Blp::parse(&bytes).ok()?;
    let edge = adt::minimap::TILE_TEXELS as u32;
    if picture.level_size(0) != (edge, edge) {
        return None;
    }
    picture.decode_rgba(0)
}

/// Absolute channel difference between two texels, averaged over RGB.
fn texel_difference(a: &[u8], at_a: usize, b: &[u8], at_b: usize) -> f64 {
    let (a, b) = (&a[at_a * 4..at_a * 4 + 3], &b[at_b * 4..at_b * 4 + 3]);
    (0..3)
        .map(|c| (a[c] as f64 - b[c] as f64).abs())
        .sum::<f64>()
        / 3.0
}

/// Draws the minimap's own composition to a PNG.
///
/// Nearest-neighbour, and deliberately: this exists to check *placement*, and
/// a filter that smoothed the seams would hide the thing being looked for.
/// The centre pixel is marked, because "the player is in the middle" is the
/// one claim a picture of a disc cannot make on its own.
fn minimap_stitch(
    chain: &mut Chain,
    map: &str,
    x: f32,
    y: f32,
    range: f32,
    pixels: u32,
    out: Option<PathBuf>,
) -> Result<()> {
    let index = load_minimap_index(chain)?;
    let view = adt::minimap::Viewport::new(x, y, range);
    let pixels = pixels.clamp(16, 4096);
    let edge = adt::minimap::TILE_TEXELS;
    let mut canvas = vec![0u8; (pixels * pixels * 4) as usize];

    let touching = view.tiles_touching();
    let mut drawn = 0usize;
    for (tx, ty) in &touching {
        let Some(art) = minimap_pixels(chain, &index, map, *tx, *ty) else {
            println!("  {map} {tx},{ty}: no art");
            continue;
        };
        drawn += 1;
        let [u0, v0, u1, v1] = view.tile_rect(*tx, *ty);
        println!("  {map} {tx},{ty}: u {u0:.3}..{u1:.3}  v {v0:.3}..{v1:.3}");
        // Walk the output rather than the source, so a tile larger than the
        // picture is cropped instead of writing outside the canvas.
        let (px0, px1) = ((u0 * pixels as f32) as i64, (u1 * pixels as f32) as i64);
        let (py0, py1) = ((v0 * pixels as f32) as i64, (v1 * pixels as f32) as i64);
        for py in py0.max(0)..py1.min(pixels as i64) {
            for px in px0.max(0)..px1.min(pixels as i64) {
                let su = ((px - px0) as f32 / (px1 - px0) as f32 * edge as f32) as usize;
                let sv = ((py - py0) as f32 / (py1 - py0) as f32 * edge as f32) as usize;
                let source = (sv.min(edge - 1) * edge + su.min(edge - 1)) * 4;
                let target = ((py as u32 * pixels + px as u32) * 4) as usize;
                canvas[target..target + 4].copy_from_slice(&art[source..source + 4]);
            }
        }
    }

    // The centre, in a colour no terrain is.
    let middle = pixels / 2;
    for (dx, dy) in [(0i32, 0i32), (1, 0), (-1, 0), (0, 1), (0, -1)] {
        let px = (middle as i32 + dx).clamp(0, pixels as i32 - 1) as u32;
        let py = (middle as i32 + dy).clamp(0, pixels as i32 - 1) as u32;
        let at = ((py * pixels + px) * 4) as usize;
        canvas[at..at + 4].copy_from_slice(&[255, 0, 255, 255]);
    }

    let out = out.unwrap_or_else(|| PathBuf::from("minimap.png"));
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&out)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), pixels, pixels);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&canvas)?;
    println!(
        "{map} at {x:.0},{y:.0}, {range:.0} units across: {drawn} of {} tiles drawn to {}",
        touching.len(),
        out.display()
    );
    Ok(())
}

fn minimap_export(
    chain: &mut Chain,
    map: &str,
    x: usize,
    y: usize,
    out: Option<PathBuf>,
) -> Result<()> {
    let index = load_minimap_index(chain)?;
    let path = index
        .tile_path(map, x, y)
        .with_context(|| format!("{map} has no minimap tile {x},{y}"))?;
    println!("{map} {x},{y} -> {path}");
    blp_export(chain, &path, 0, out)
}

fn map_cmd(chain: &mut Chain, cmd: MapCommand) -> Result<()> {
    match cmd {
        MapCommand::Pages { map, filter } => map_pages(chain, map, filter.as_deref()),
        MapCommand::Locate { map, x, y } => map_locate(chain, map, x, y),
        MapCommand::Canvas { directory } => map_canvas(chain, directory.as_deref()),
        MapCommand::Overlays { directory, verify } => {
            map_overlays(chain, directory.as_deref(), verify)
        }
        MapCommand::Calibrate { map, verbose } => map_calibrate(chain, map.as_deref(), verbose),
    }
}

fn load_atlas(chain: &mut Chain) -> Result<dbc::worldmap::Atlas> {
    use dbc::schema::{WorldMapArea, WorldMapOverlay};
    let table = WorldMapArea::parse(&chain.read(WorldMapArea::PATH)?)?;
    let atlas = dbc::worldmap::Atlas::from_table(&table);
    let overlays = WorldMapOverlay::parse(&chain.read(WorldMapOverlay::PATH)?)?;
    Ok(atlas.with_overlays(&overlays))
}

/// Lists a page's explored-art patches, optionally resolving every tile.
fn map_overlays(chain: &mut Chain, directory: Option<&str>, verify: bool) -> Result<()> {
    use dbc::schema::AreaTable;

    let atlas = load_atlas(chain)?;
    let areas = AreaTable::parse(&chain.read(AreaTable::PATH)?).ok();
    let area_name = |id: u32| -> String {
        areas
            .as_ref()
            .and_then(|t| t.iter().find(|r| r.id() == id))
            .map(|r| r.name().to_string())
            .unwrap_or_default()
    };

    let pages: Vec<_> = atlas
        .pages()
        .iter()
        .filter(|page| directory.is_none_or(|only| page.directory.eq_ignore_ascii_case(only)))
        .cloned()
        .collect();
    if pages.is_empty() {
        anyhow::bail!("no page matched");
    }

    let (mut patches, mut tiles, mut missing) = (0usize, 0usize, Vec::new());
    // Every file the rule *predicts* resolving proves the count is not too
    // high; nothing about it proves the count is not too low, and a patch
    // missing its last tile is a hole in the map that looks like unexplored
    // ground. So the file one past the end is asked for too, and must not be
    // there.
    let mut overshoot = Vec::new();
    for page in &pages {
        let owned: Vec<_> = atlas.overlays(page.id).cloned().collect();
        if owned.is_empty() {
            continue;
        }
        println!("\n{} (page {}), {} patch(es)", page.directory, page.id, owned.len());
        for overlay in &owned {
            let (across, down) = overlay.tile_grid();
            let named: Vec<String> = overlay
                .areas
                .iter()
                .filter(|a| **a != 0)
                .map(|a| format!("{a} {}", area_name(*a)))
                .collect();
            println!(
                "  [{:>4}] {:<24} {:>4}x{:<4} at {:>4},{:<4}  {across}x{down} tile(s)  areas: {}",
                overlay.id,
                overlay.texture,
                overlay.width,
                overlay.height,
                overlay.offset_x,
                overlay.offset_y,
                named.join(", ")
            );
            patches += 1;
            if !verify {
                continue;
            }
            for tile in 1..=overlay.tile_count() {
                let path = overlay.tile_path(&page.directory, tile);
                tiles += 1;
                // **Resolved by path, never looked up in a listing.** An MPQ
                // finds a file by hashing its name, so a file absent from
                // `(listfile)` still reads perfectly -- a coverage check built
                // on `ls` once concluded 0.1% of the baked NPC textures
                // shipped, and forty random reads by path got forty hits.
                if chain.read(&path).is_err() {
                    missing.push(path);
                }
            }
            let past_the_end = overlay.tile_path(&page.directory, overlay.tile_count() + 1);
            if chain.read(&past_the_end).is_ok() {
                overshoot.push(format!(
                    "{past_the_end}  (the table says {}x{}, so {} tile(s))",
                    overlay.width,
                    overlay.height,
                    overlay.tile_count()
                ));
            }
        }
    }

    println!("\n{patches} patch(es) across {} page(s)", pages.len());
    if verify {
        println!(
            "{} of {tiles} tile file(s) resolved",
            tiles - missing.len()
        );
        for path in missing.iter().take(20) {
            println!("  missing: {path}");
        }
        if missing.len() > 20 {
            println!("  ... and {} more", missing.len() - 20);
        }
        // Expected, and not a miscount: the tile grid covers the stated
        // rectangle exactly, so a further file has nowhere to be drawn.
        // `MarshlightLake1` is a whole labelled patch and `MarshlightLake2` is
        // nearly blank -- an offcut of a taller earlier version, left behind
        // when the table row shrank.
        println!(
            "{} patch(es) keep a file past the tile grid, unreadable by any \
             placement of the rectangle the table states",
            overshoot.len()
        );
        for path in overshoot.iter().take(10) {
            println!("  unread: {path}");
        }
        if overshoot.len() > 10 {
            println!("  ... and {} more", overshoot.len() - 10);
        }
    }
    Ok(())
}

fn map_pages(chain: &mut Chain, map: Option<u32>, filter: Option<&str>) -> Result<()> {
    let atlas = load_atlas(chain)?;
    let needle = filter.map(str::to_ascii_lowercase);
    println!(
        "{:>4}  {:>4}  {:>5}  {:<22}  {:>10} {:>10}  {:>10} {:>10}",
        "id", "map", "area", "directory", "x_min", "x_max", "y_min", "y_max"
    );
    let mut shown = 0usize;
    for page in atlas.pages() {
        if map.is_some_and(|m| m != page.map_id) {
            continue;
        }
        if needle
            .as_deref()
            .is_some_and(|n| !page.directory.to_ascii_lowercase().contains(n))
        {
            continue;
        }
        println!(
            "{:>4}  {:>4}  {:>5}  {:<22}  {:>10.1} {:>10.1}  {:>10.1} {:>10.1}",
            page.id,
            page.map_id,
            page.area_id,
            page.directory,
            page.x_min,
            page.x_max,
            page.y_min,
            page.y_max
        );
        shown += 1;
    }
    println!("\n{shown} of {} pages with bounds", atlas.pages().len());
    Ok(())
}

fn map_locate(chain: &mut Chain, map: u32, x: f32, y: f32) -> Result<()> {
    use dbc::schema::AreaTable;

    let atlas = load_atlas(chain)?;
    let areas = AreaTable::parse(&chain.read(AreaTable::PATH)?).ok();
    let area_name = |id: u32| -> String {
        areas
            .as_ref()
            .and_then(|t| t.iter().find(|r| r.id() == id))
            .map(|r| r.name().to_string())
            .unwrap_or_default()
    };

    println!("map {map} at {x:.2}, {y:.2}\n");
    println!("pages containing it, smallest first:");
    let mut containing: Vec<_> = atlas
        .pages()
        .iter()
        .filter(|p| p.map_id == map && p.contains(x, y))
        .collect();
    containing.sort_by(|a, b| {
        a.world_area()
            .partial_cmp(&b.world_area())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if containing.is_empty() {
        println!("  none -- that position is outside every page on this map");
    }
    for page in &containing {
        let kind = if page.area_id == 0 { "continent" } else { "zone" };
        println!(
            "  [{:>4}] {:<22} {kind:<9} {} ({})",
            page.id,
            page.directory,
            page.area_id,
            area_name(page.area_id)
        );
    }

    match atlas.zone_page(map, x, y) {
        Some(page) => {
            let (u, v) = page.project(x, y);
            let (px, py) = page.project_pixels(x, y);
            println!("\nchosen: {} (page {})", page.directory, page.id);
            println!("  fraction  {u:.4}, {v:.4}   (0,0 is the top left)");
            println!(
                "  pixel     {px:.1}, {py:.1}   of {:.0}x{:.0}",
                dbc::worldmap::PAGE_WIDTH,
                dbc::worldmap::PAGE_HEIGHT
            );
            let (col, row) = dbc::worldmap::Page::tile_grid(
                1 + (px / 256.0).floor().max(0.0) as usize
                    + 4 * (py / 256.0).floor().max(0.0) as usize,
            );
            println!("  tile      column {col}, row {row}");
        }
        None => println!("\nno zone page covers it"),
    }
    Ok(())
}

/// Measures the drawn extent of every page's tile grid from the art's alpha.
///
/// A tile that is entirely opaque says nothing about where the picture ends;
/// only the tiles carrying padding do, which is why this reports the furthest
/// opaque column and row across the whole grid rather than per tile.
fn map_canvas(chain: &mut Chain, directory: Option<&str>) -> Result<()> {
    use dbc::worldmap::{Page, TILE_COLUMNS, TILE_ROWS, TILE_TEXELS};

    let atlas = load_atlas(chain)?;
    let mut pages: Vec<_> = atlas.pages().to_vec();
    if let Some(only) = directory {
        pages.retain(|p| p.directory.eq_ignore_ascii_case(only));
        if pages.is_empty() {
            anyhow::bail!("no page with directory {only}");
        }
    }

    println!(
        "{:<24} {:>11}  {:>11}  {}",
        "page", "content", "grid", "tiles read"
    );
    let mut tally: std::collections::BTreeMap<(usize, usize), usize> = Default::default();
    for page in &pages {
        let (mut right, mut bottom, mut read) = (0usize, 0usize, 0usize);
        for tile in 1..=TILE_COLUMNS * TILE_ROWS {
            let path = page.tile_path(tile);
            let Ok(bytes) = chain.read(&path) else {
                continue;
            };
            let Ok(image) = blp::Blp::parse(&bytes) else {
                continue;
            };
            let (w, h) = (image.width() as usize, image.height() as usize);
            let Some(rgba) = image.decode_rgba(0) else {
                continue;
            };
            read += 1;
            let (col, row) = Page::tile_grid(tile);
            for y in 0..h {
                for x in 0..w {
                    // A fully-opaque tile is the common case and its whole
                    // extent counts; padding is what the alpha marks out.
                    if rgba[(y * w + x) * 4 + 3] > 127 {
                        right = right.max(col * TILE_TEXELS + x + 1);
                        bottom = bottom.max(row * TILE_TEXELS + y + 1);
                    }
                }
            }
        }
        if read == 0 {
            continue;
        }
        println!(
            "{:<24} {:>5}x{:<5}  {:>5}x{:<5}  {read}",
            page.directory,
            right,
            bottom,
            TILE_COLUMNS * TILE_TEXELS,
            TILE_ROWS * TILE_TEXELS
        );
        *tally.entry((right, bottom)).or_default() += 1;
    }

    println!("\ncontent rectangles seen:");
    for ((w, h), n) in &tally {
        println!("  {w:>5}x{h:<5}  on {n} page(s)");
    }
    println!(
        "\nthis client uses {:.0}x{:.0}",
        dbc::worldmap::PAGE_WIDTH,
        dbc::worldmap::PAGE_HEIGHT
    );
    Ok(())
}

/// A least-squares fit of `observed = slope * predicted + intercept`.
#[derive(Default, Clone, Copy)]
struct Fit {
    n: usize,
    sx: f64,
    sy: f64,
    sxx: f64,
    sxy: f64,
    syy: f64,
}

impl Fit {
    fn push(&mut self, predicted: f64, observed: f64) {
        self.n += 1;
        self.sx += predicted;
        self.sy += observed;
        self.sxx += predicted * predicted;
        self.sxy += predicted * observed;
        self.syy += observed * observed;
    }

    /// `(slope, intercept, r_squared)`, or `None` when the samples do not vary
    /// enough to fit a line -- which is a real outcome, not an error: a page
    /// whose overlays all sit in one spot says nothing about the projection.
    fn solve(&self) -> Option<(f64, f64, f64)> {
        let n = self.n as f64;
        if self.n < 3 {
            return None;
        }
        let denom = n * self.sxx - self.sx * self.sx;
        if denom.abs() < 1e-9 {
            return None;
        }
        let slope = (n * self.sxy - self.sx * self.sy) / denom;
        let intercept = (self.sy - slope * self.sx) / n;
        let ss_tot = self.syy - self.sy * self.sy / n;
        let ss_res = self.syy - intercept * self.sy - slope * self.sxy;
        let r2 = if ss_tot.abs() < 1e-9 {
            1.0
        } else {
            1.0 - ss_res / ss_tot
        };
        Some((slope, intercept, r2))
    }
}

/// Fits the projection against the terrain, per page and overall.
fn map_calibrate(chain: &mut Chain, map: Option<&str>, verbose: bool) -> Result<()> {
    use dbc::schema::{Map, WorldMapOverlay};
    use std::collections::HashMap;

    let atlas = load_atlas(chain)?;
    let overlays = WorldMapOverlay::parse(&chain.read(WorldMapOverlay::PATH)?)?;
    let map_table = Map::parse(&chain.read(Map::PATH)?)?;

    // Directory names, because a page names a `Map.dbc` id and the terrain
    // files are found by that map's folder.
    let mut directories: Vec<(u32, String)> = map_table
        .iter()
        .map(|m| (m.id(), m.directory().to_string()))
        .filter(|(_, d)| !d.is_empty())
        .collect();
    if let Some(only) = map {
        directories.retain(|(_, d)| d.eq_ignore_ascii_case(only));
        if directories.is_empty() {
            anyhow::bail!("no map directory called {only}");
        }
    }
    // Only maps that actually have pages are worth reading terrain for.
    directories.retain(|(id, _)| atlas.pages().iter().any(|p| p.map_id == *id));

    // Where every area id's terrain is, in world coordinates. Summed rather
    // than stored per chunk: a centroid is all the fit needs and a million
    // samples is not worth holding.
    let mut centroids: HashMap<u32, (f64, f64, u64)> = HashMap::new();
    for (_, directory) in &directories {
        let Ok(wdt) = load_wdt(chain, directory) else {
            continue;
        };
        let (mut tiles, mut chunks) = (0usize, 0u64);
        for ty in 0..adt::TILES_PER_MAP {
            for tx in 0..adt::TILES_PER_MAP {
                if !wdt.has_tile(tx, ty) {
                    continue;
                }
                let path = adt::tile_path(directory, tx, ty);
                let Ok(bytes) = chain.read(&path) else {
                    continue;
                };
                let Ok(parsed) = adt::Adt::parse(&bytes, wdt.big_alpha()) else {
                    continue;
                };
                tiles += 1;
                for chunk in &parsed.chunks {
                    if chunk.area_id == 0 {
                        continue;
                    }
                    // The chunk's stored position is its origin corner and both
                    // axes run inwards from it, so the middle is half a chunk
                    // *down* in each.
                    let e = centroids.entry(chunk.area_id).or_default();
                    e.0 += (chunk.position[0] - adt::CHUNK_SIZE / 2.0) as f64;
                    e.1 += (chunk.position[1] - adt::CHUNK_SIZE / 2.0) as f64;
                    e.2 += 1;
                    chunks += 1;
                }
            }
        }
        println!("{directory}: {tiles} tiles, {chunks} chunks with an area id");
    }
    if centroids.is_empty() {
        anyhow::bail!("no terrain read, so there is nothing to calibrate against");
    }

    // The four readings of the bounds. `project` is the one this client uses;
    // the other three are what it would be with an axis reversed, and they are
    // here so the result can come out the other way.
    let candidates: [(&str, fn(f32, f32) -> (f32, f32)); 4] = [
        ("as written", |u, v| (u, v)),
        ("x flipped", |u, v| (1.0 - u, v)),
        ("y flipped", |u, v| (u, 1.0 - v)),
        ("both flipped", |u, v| (1.0 - u, 1.0 - v)),
    ];
    let mut fits = [[Fit::default(); 2]; 4];
    let mut per_page: HashMap<u32, [[Fit; 2]; 4]> = HashMap::new();
    let mut scored = 0usize;

    for row in overlays.iter() {
        let Some(page) = atlas.page(row.world_map_area_id()) else {
            continue;
        };
        if !directories.iter().any(|(id, _)| *id == page.map_id) {
            continue;
        }
        // Every area this one texture covers, pooled -- the overlay states one
        // rectangle for the lot, so the prediction has to be the pool's
        // centroid rather than any single area's.
        let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0u64);
        for area in [
            row.area_id_0(),
            row.area_id_1(),
            row.area_id_2(),
            row.area_id_3(),
        ] {
            if area == 0 {
                continue;
            }
            if let Some((ax, ay, an)) = centroids.get(&area) {
                sx += ax;
                sy += ay;
                n += an;
            }
        }
        if n == 0 {
            continue;
        }
        let (wx, wy) = (sx / n as f64, sy / n as f64);
        // The clickable box is the honest observation: the texture rectangle
        // is padded to a power of two, so its centre is not the area's centre.
        let obs_x = (row.hit_left() + row.hit_right()) as f64 / 2.0;
        let obs_y = (row.hit_top() + row.hit_bottom()) as f64 / 2.0;
        if row.hit_right() <= row.hit_left() || row.hit_bottom() <= row.hit_top() {
            continue;
        }
        let (u, v) = page.project(wx as f32, wy as f32);
        scored += 1;
        let slot = per_page.entry(page.id).or_default();
        for (i, (_, flip)) in candidates.iter().enumerate() {
            let (fu, fv) = flip(u, v);
            fits[i][0].push(fu as f64, obs_x);
            fits[i][1].push(fv as f64, obs_y);
            slot[i][0].push(fu as f64, obs_x);
            slot[i][1].push(fv as f64, obs_y);
        }
    }

    println!("\n{scored} overlays scored against terrain centroids\n");
    println!(
        "{:<14}  {:>28}  {:>28}",
        "reading", "horizontal: slope/offset/r2", "vertical: slope/offset/r2"
    );
    for (i, (name, _)) in candidates.iter().enumerate() {
        let h = fits[i][0].solve();
        let v = fits[i][1].solve();
        println!("{name:<14}  {:>28}  {:>28}", show_fit(h), show_fit(v));
    }
    println!(
        "\nA slope near +{:.0} horizontally and +{:.0} vertically is this client's\n\
         projection agreeing with the art. A negative slope is an axis read backwards.",
        dbc::worldmap::PAGE_WIDTH,
        dbc::worldmap::PAGE_HEIGHT
    );

    if verbose {
        println!("\nper page, for the reading this client uses:");
        let mut ids: Vec<_> = per_page.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let f = &per_page[&id];
            let name = atlas.page(id).map(|p| p.directory.clone()).unwrap_or_default();
            println!(
                "  [{id:>4}] {name:<22} n={:<4} {:>28}  {:>28}",
                f[0][0].n,
                show_fit(f[0][0].solve()),
                show_fit(f[0][1].solve())
            );
        }
    }
    Ok(())
}

fn show_fit(fit: Option<(f64, f64, f64)>) -> String {
    match fit {
        Some((slope, intercept, r2)) => format!("{slope:>10.1} {intercept:>8.1} {r2:>7.4}"),
        None => "too few samples".to_string(),
    }
}

fn wmo_cmd(chain: &mut Chain, cmd: WmoCommand) -> Result<()> {
    match cmd {
        WmoCommand::Info { path, limit } => wmo_info(chain, &path, limit),
        WmoCommand::Survey { filter, limit } => wmo_survey(chain, filter.as_deref(), limit),
        WmoCommand::Footing { filter, limit } => wmo_footing(chain, filter.as_deref(), limit),
    }
}

/// Asks what a WMO material's `ground_type` column actually holds, and
/// whether the surfaces underfoot inside a building can be told apart by it.
///
/// **The same question `GroundEffectTexture`'s terrain column had, in the same
/// shape.** It is a bare small integer with nothing in the file to confirm it,
/// pointing -- supposedly -- into a twelve-row table. What can confirm it is
/// one step out: a material also names a **texture file**, and those names are
/// authored English. A column whose `Wood` rows are reached by materials
/// painted with files called `..._wood_...` is the terrain column.
///
/// The control that matters is the *other* direction: most materials in the
/// game are walls, roofs and windows, which are not underfoot and have no
/// reason to say anything. A column that is zero nearly everywhere and
/// meaningful on floors is the right shape; one that is uniformly zero says
/// this cannot be done at all.
fn wmo_footing(chain: &mut Chain, filter: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    let terrain = dbc::schema::TerrainType::parse(&chain.read(dbc::schema::TerrainType::PATH)?)?;
    let named: BTreeMap<u32, String> = terrain
        .iter()
        .map(|r| (r.id(), r.name().to_string()))
        .collect();

    let needle = filter.map(str::to_lowercase);
    let roots: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".wmo")
                // Group files repeat the root's materials; counting them would
                // weight every tally by how many pieces a building is cut into.
                && !l
                    .trim_end_matches(".wmo")
                    .rsplit('_')
                    .next()
                    .is_some_and(|tail| tail.len() == 3 && tail.chars().all(|c| c.is_ascii_digit()))
                && needle.as_ref().is_none_or(|f| l.contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    let mut values: BTreeMap<u32, usize> = BTreeMap::new();
    let mut words: BTreeMap<u32, BTreeMap<&str, usize>> = BTreeMap::new();
    let mut examples: BTreeMap<u32, String> = BTreeMap::new();
    let mut floors: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    let (mut roots_ok, mut materials, mut out_of_range) = (0usize, 0usize, 0usize);
    let mut with_any = 0usize;

    const MATERIALS: [&str; 12] = [
        "dirt", "metal", "stone", "snow", "wood", "grass", "leaf", "sand", "water", "rock",
        "brick", "carpet",
    ];

    for path in &roots {
        let Ok(bytes) = chain.read(path) else { continue };
        let Ok(root) = wmo::Root::parse(&bytes) else {
            continue;
        };
        roots_ok += 1;
        let mut any = false;
        for (index, material) in root.materials.iter().enumerate() {
            materials += 1;
            *values.entry(material.ground_type).or_default() += 1;
            // **Not `!= 0`.** Row 0 is `Dirt`, a real answer; the value
            // meaning "this says nothing" is row 10, `None`. Counting it as a
            // label reports 1,981 of 1,985 buildings as labelled when the
            // truth is a small minority, which is the difference between "this
            // works everywhere" and "this works where Blizzard bothered".
            if material.ground_type != 10 {
                any = true;
            }
            if !named.contains_key(&material.ground_type) {
                out_of_range += 1;
                continue;
            }
            let texture = root.texture(material.texture1).to_lowercase();
            for word in MATERIALS {
                if texture.contains(word) {
                    *words
                        .entry(material.ground_type)
                        .or_default()
                        .entry(word)
                        .or_default() += 1;
                }
            }
            // **The strongest signal in the whole table**, and it is one word
            // long. Blizzard file their art by what it is *for*, so a material
            // painted from a texture under a `floor` directory is a surface
            // somebody walks on. If the non-`None` values are floors and
            // `None` is everything else, this column is what a foot lands on
            // rather than a decoration.
            let entry = floors.entry(material.ground_type).or_default();
            entry.1 += 1;
            if texture.contains(r"\floor\") {
                entry.0 += 1;
            }
            if material.ground_type != 10 {
                examples
                    .entry(material.ground_type)
                    .or_insert_with(|| format!("{path} material {index}: {texture}"));
            }
        }
        if any {
            with_any += 1;
        }
    }

    println!("{roots_ok} root WMOs, {materials} materials");
    println!(
        "  {with_any} label at least one surface (a row other than `None`), {out_of_range} materials name a \
         value that is not a TerrainType row"
    );
    // **The discriminating statistic is enrichment, not share.** Most WMO art
    // is walls and roofs, and rock and wood turn up everywhere, so "half the
    // `Wood` materials are painted with something called wood" only means
    // something beside how often `None`'s are. Row 10 is the baseline: it is
    // 91% of the table and is by construction the rows that say nothing.
    let stem = |name: &str| -> Option<&'static str> {
        // The terrain's own name, clipped to the stem a filename would use.
        // `Soggy` has no material word of its own and cannot vote.
        Some(match name {
            "Dirt" => "dirt",
            "Metallic" => "metal",
            "Stone" => "stone",
            "Snow" => "snow",
            "Wood" => "wood",
            "Grass" | "DustyGrass" => "grass",
            "Leaves" => "leaf",
            "Sand" => "sand",
            _ => return None,
        })
    };
    const NONE_ROW: u32 = 10;
    let share = |value: u32, word: &str| -> f32 {
        let hits = words
            .get(&value)
            .and_then(|w| w.get(word))
            .copied()
            .unwrap_or(0);
        let total = values.get(&value).copied().unwrap_or(0);
        100.0 * hits as f32 / total.max(1) as f32
    };
    println!(
        "\n  {:>5} {:<12} {:>9} {:>6} {:>7} {:>8} {:>8}  {}",
        "value", "name", "materials", "word", "share", "in None", "vs None", "textures called"
    );
    for (value, count) in &values {
        let mut tally: Vec<(&&str, &usize)> = words
            .get(value)
            .map(|w| w.iter().collect())
            .unwrap_or_default();
        tally.sort_by(|a, b| b.1.cmp(a.1));
        let listed: Vec<String> = tally
            .iter()
            .take(4)
            .map(|(w, n)| format!("{w} x{n}"))
            .collect();
        let name = named.get(value).map(String::as_str).unwrap_or("?");
        let (word, mine, baseline) = match stem(name) {
            Some(word) => (word, share(*value, word), share(NONE_ROW, word)),
            None => ("--", 0.0, 0.0),
        };
        let ratio = if baseline > 0.0 {
            format!("x{:.1}", mine / baseline)
        } else {
            "--".to_string()
        };
        println!(
            "  {value:>5} {name:<12} {count:>9} {word:>6} {mine:>6.1}% {baseline:>7.1}% \
             {ratio:>8}  {}",
            listed.join(", "),
        );
    }
    // **Reported because it came back flat, not despite it.** Filing art under
    // a `floor` directory looked like it ought to separate a surface from a
    // wall, and does not: `None` is 5% floor art and so is `Metallic`. Most WMO
    // floor art lives under `rock` and `stone` instead. An instrument that
    // answers nothing is worth one line, so nobody builds it a second time.
    println!("\n  share of each value's materials filed under a `floor` directory:");
    let filed: Vec<String> = floors
        .iter()
        .map(|(value, (on_floor, total))| format!("{value}: {}%", 100 * on_floor / total.max(&1)))
        .collect();
    println!("    {}", filed.join("  "));

    println!("\n  one example of each non-zero value:");
    for (value, example) in &examples {
        println!("  {value:>5}  {example}");
    }
    Ok(())
}

fn wmo_info(chain: &mut Chain, path: &str, limit: usize) -> Result<()> {
    if wmo::is_group_path(path) {
        anyhow::bail!("{path} is a group file; pass the root .wmo instead");
    }
    let bytes = chain.read(path)?;
    let root = wmo::Root::parse(&bytes)?;
    // Group names live in the root, so groups need it passed in.
    let names = wmo::Chunks::find(&bytes, b"MOGN").unwrap_or(&[]).to_vec();
    let h = root.header;

    println!("{path}");
    println!(
        "  {} groups, {} materials, {} textures, {} portals, {} lights",
        h.group_count,
        root.materials.len(),
        root.textures().len(),
        h.portal_count,
        h.light_count
    );
    println!(
        "  bounds [{:.1} {:.1} {:.1}] .. [{:.1} {:.1} {:.1}]",
        h.bounding_box.0[0],
        h.bounding_box.0[1],
        h.bounding_box.0[2],
        h.bounding_box.1[0],
        h.bounding_box.1[1],
        h.bounding_box.1[2]
    );
    println!(
        "  ambient {:?}, flags {:#x}, wmo id {}",
        h.ambient_color, h.flags, h.wmo_id
    );

    if !root.doodad_sets.is_empty() {
        println!("\n  doodad sets:");
        for set in &root.doodad_sets {
            let name = if set.name.is_empty() { "<unnamed>" } else { &set.name };
            println!("    {name:<24} {} doodads", set.count);
        }
    }

    println!("\n  textures:");
    for texture in root.textures().iter().take(limit) {
        println!("    {texture}");
    }

    println!("\n  groups:");
    let (mut verts, mut tris, mut collision, mut failures) = (0usize, 0usize, 0usize, 0usize);
    for i in 0..h.group_count as usize {
        let gpath = wmo::group_path(path, i);
        let Ok(gbytes) = chain.read(&gpath) else {
            println!("    {i:>3}: {gpath} MISSING");
            failures += 1;
            continue;
        };
        let group = match wmo::Group::parse(&gbytes, &names) {
            Ok(g) => g,
            Err(e) => {
                println!("    {i:>3}: {e}");
                failures += 1;
                continue;
            }
        };
        verts += group.vertices.len();
        tris += group.triangle_count();
        let hidden = group
            .triangle_materials
            .iter()
            .filter(|t| t.is_collision_only())
            .count();
        collision += hidden;

        if i < limit {
            let name = if group.name.is_empty() { "<unnamed>" } else { &group.name };
            let hidden_note = if hidden > 0 {
                format!("{hidden} collision-only")
            } else {
                String::new()
            };
            println!(
                "    {i:>3}: {name:<26} {:>6} verts {:>6} tris {:>3} batches  {}{}{hidden_note}",
                group.vertices.len(),
                group.triangle_count(),
                group.batches.len(),
                if group.is_interior() { "interior " } else { "exterior " },
                if group.has_vertex_colors() { "vcolors " } else { "" },
            );
        }
        if let Err(e) = group.validate() {
            println!("         INVALID: {e}");
            failures += 1;
        }
    }
    if h.group_count as usize > limit {
        println!("    ... {} more", h.group_count as usize - limit);
    }
    println!(
        "\n  total: {verts} vertices, {tris} triangles, {collision} collision-only, \
         {failures} problems"
    );
    Ok(())
}

fn wmo_survey(chain: &mut Chain, filter: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.map(str::to_lowercase);
    // Group files sit beside their roots and parse as WMOs; treating each as a
    // building would count every wall as its own object.
    let roots: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".wmo")
                && !wmo::is_group_path(&l)
                && needle.as_ref().is_none_or(|f| l.contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut ok, mut groups, mut verts, mut tris) = (0usize, 0usize, 0u64, 0u64);
    let (mut doodads, mut unresolved, mut biggest) = (0u64, 0usize, 0usize);

    for (i, name) in roots.iter().enumerate() {
        let Ok(bytes) = chain.read(name) else {
            unresolved += 1;
            continue;
        };
        let root = match wmo::Root::parse(&bytes) {
            Ok(r) => r,
            Err(e) => {
                let key = e.to_string();
                let key = key.split(" (").next().unwrap_or(&key).to_string();
                failures.entry(key).or_insert((0, name.clone())).0 += 1;
                continue;
            }
        };
        ok += 1;
        doodads += root.doodads.len() as u64;
        let names = wmo::Chunks::find(&bytes, b"MOGN").unwrap_or(&[]).to_vec();

        for gi in 0..root.header.group_count as usize {
            let gpath = wmo::group_path(name, gi);
            let Ok(gbytes) = chain.read(&gpath) else {
                failures
                    .entry("group file missing".into())
                    .or_insert((0, gpath.clone()))
                    .0 += 1;
                continue;
            };
            match wmo::Group::parse(&gbytes, &names) {
                Ok(group) => {
                    groups += 1;
                    verts += group.vertices.len() as u64;
                    tris += group.triangle_count() as u64;
                    biggest = biggest.max(group.vertices.len());
                    if let Err(e) = group.validate() {
                        let key = format!("group invalid: {}", first_clause(&e));
                        failures.entry(key).or_insert((0, gpath.clone())).0 += 1;
                    }
                }
                Err(e) => {
                    let key = format!("group parse: {}", first_clause(&e.to_string()));
                    failures.entry(key).or_insert((0, gpath.clone())).0 += 1;
                }
            }
        }
        if i % 500 == 499 {
            tracing::info!("{}/{} objects", i + 1, roots.len());
        }
    }

    println!("\n{ok}/{} root objects parsed, {groups} groups", roots.len());
    println!("  {verts} vertices, {tris} triangles, {doodads} doodad placements");
    println!("  largest group: {biggest} vertices");
    println!("  {unresolved} listed roots did not resolve (tombstoned or stale)");
    if failures.is_empty() {
        println!("\nno failures");
    } else {
        println!("\nfailures:");
        for (kind, (count, example)) in &failures {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

fn m2_cmd(chain: &mut Chain, cmd: M2Command) -> Result<()> {
    match cmd {
        M2Command::Info { path, lod, limit } => m2_info(chain, &path, lod, limit),
        M2Command::Survey { filter, limit } => m2_survey(chain, filter.as_deref(), limit),
        M2Command::Creature { display_id } => m2_creature(chain, display_id),
        M2Command::Anims { path, limit } => m2_anims(chain, &path, limit),
        M2Command::AttachTrace {
            path,
            anim,
            track,
            limit,
        } => m2_attach_trace(chain, &path, anim, track, limit),
        M2Command::Attachments { path, anim, survey } => {
            if survey {
                m2_attachment_survey(chain, &path)
            } else {
                m2_attachments(chain, &path, anim)
            }
        }
        M2Command::Emitters {
            path,
            survey,
            strides,
        } => {
            if survey || strides {
                m2_emitter_survey(chain, &path, strides)
            } else {
                m2_emitters(chain, &path)
            }
        }
        M2Command::Events {
            path,
            anims,
            survey,
            strides,
            trace,
        } => match trace {
            Some(anim) => m2_event_trace(chain, &path, anim),
            None if survey || strides => m2_event_survey(chain, &path, strides),
            None => m2_events(chain, &path, anims),
        },
    }
}

/// Inspect sounds: what the client can play, and whether the files are there.
#[derive(Subcommand)]
enum SoundCommand {
    /// List sound entries, optionally filtered by name.
    List {
        /// Case-insensitive substring to match against the entry's name.
        filter: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Resolve every sound's files against the archives and report coverage.
    ///
    /// **This is the check that transcribed `SoundEntries` at all.** The
    /// column layout was read off the file's own shape -- ten string columns
    /// of decreasing density, then ten small-integer columns with the same
    /// decreasing density -- and that pattern fits several wrong alignments
    /// just as well. What it cannot fit is the strings naming *files that
    /// exist*: a one-column slip produces paths the archive has never heard
    /// of, where a dump of the same wrong columns looks entirely plausible.
    ///
    /// Resolved **by path**, not by looking in `(listfile)`. An MPQ resolves
    /// by hash, so a file missing from the listfile still reads perfectly --
    /// a coverage check built on `ls` once concluded a whole feature was
    /// impossible.
    Survey {
        /// Stop after this many entries.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Report what each `sound_type` value actually contains.
    ///
    /// The type column runs 1-53 and this client names none of them. Naming
    /// them from memory is the mistake `describe_cast_failure` exists to
    /// refuse, so this asks the data instead: for each value, how many entries
    /// carry it and which folders their files live in. A value whose every
    /// entry sits under `Sound\Music` is the music type, and that is a
    /// measurement rather than a recollection.
    Types,
    /// Check that every zone's music and ambience ids point where they should.
    ///
    /// **Validity is nearly free; the *type* is the discriminator.**
    /// `SoundEntries` ids run 3-18019 over 12,941 rows, so any small integer
    /// in that range lands on a real row and a wrong column would look
    /// perfectly valid. What a wrong column cannot do is land on a row of the
    /// *right kind*: a music id has to resolve to a music entry and an
    /// ambience id to an ambience entry, and those types were themselves
    /// measured rather than assumed. Same reasoning that separated
    /// `Spell.dbc`'s duration column from its plausible neighbours.
    Zones,
    /// Work out what each column of `CreatureSoundData` is for, by reading the
    /// names of the sounds it points at.
    ///
    /// **The identification problem, and the instrument for it.** That table
    /// is 38 columns of sound ids with nothing saying which is the death cry
    /// and which is the footstep. Every column holds ids in the same range, so
    /// validity separates none of them -- and naming them from memory is the
    /// mistake this project keeps refusing.
    ///
    /// `SoundEntries` carries a human label per sound, though, and those
    /// labels are systematic: a column whose entries are called `WolfDeath`,
    /// `BearDeath` and `KoboldDeath` is the death column, and that is a
    /// measurement. This tallies the most common words in each column's names
    /// and prints them, so the answer is read off the data rather than
    /// recalled.
    Creatures {
        /// Only consider this many rows.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Ask the footstep tables to identify their own columns, and the terrain
    /// whether the chain from a patch of ground to a material reaches one.
    Footsteps {
        /// Map directory to walk for the terrain half. Defaults to `Azeroth`.
        #[arg(long)]
        map: Option<String>,
        /// How many tiles to read. Defaults to 64.
        #[arg(long)]
        limit: Option<usize>,
        /// The one tile to resolve end to end, as `x,y`. Defaults to Elwynn's
        /// `32,48`, which carries Northshire's roads, grass and abbey floor.
        #[arg(long)]
        tile: Option<String>,
        /// Creature display id to walk it as. Defaults to 49, the human male.
        #[arg(long, default_value_t = 49)]
        walker: u32,
    },
    /// Extract a sound's files to disk so they can actually be listened to.
    ///
    /// The audio equivalent of `blp export`: a format that has only ever been
    /// parsed is a format nobody has checked. A composed thing needs a way to
    /// be seen -- or here, heard -- as itself.
    Export {
        /// The `SoundEntries` id.
        id: u32,
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
}

/// Asks the footstep tables to identify their own columns, then asks the
/// terrain whether the chain from a patch of ground to a material actually
/// reaches one.
///
/// Three separate questions, and only the first two are about tables:
///
/// 1. **Which reading of `FootstepTerrainLookup`'s terrain column is right.**
///    It is either a [`TerrainType`] row id or that row's `sound_id`, the two
///    are off by one from each other, and both parse. `SoundEntries` names its
///    rows, so the sounds each terrain value reaches can be checked against the
///    material word in their own names -- and one reading agrees with them
///    while the other does not.
/// 2. **Whether both sound columns resolve**, and what *type* of sound each
///    reaches. A splash is a different sound type from a footstep, which is
///    what separates the two columns without recalling their order.
/// 3. **Whether the terrain under a character can be found at all.** A map
///    chunk's texture layer names a `GroundEffectTexture` row, that row names a
///    terrain, and this reports how much of the ground actually gets that far.
///    The honest answer matters more than a high number: ground that names no
///    terrain must fall back to the lookup's own terrain 0 rather than be
///    asserted to be dirt.
fn sound_footsteps(
    chain: &mut Chain,
    map: Option<&str>,
    limit: Option<usize>,
    tile: Option<&str>,
    walker: u32,
) -> Result<()> {
    use dbc::schema::{
        CreatureSoundData, FootstepTerrainLookup, GroundEffectTexture, SoundEntries, TerrainType,
    };
    use std::collections::BTreeMap;

    let terrain = TerrainType::parse(&chain.read(TerrainType::PATH)?)?;
    let lookup = FootstepTerrainLookup::parse(&chain.read(FootstepTerrainLookup::PATH)?)?;
    let sounds = SoundEntries::parse(&chain.read(SoundEntries::PATH)?)?;

    let sound: BTreeMap<u32, (u32, String)> = sounds
        .iter()
        .map(|r| (r.id(), (r.sound_type(), r.name().to_string())))
        .collect();

    println!("TerrainType: {} rows", terrain.len());
    println!("  {:>3}  {:<12} {:>8}  {}", "id", "name", "sound_id", "spray run/walk");
    for row in terrain.iter() {
        println!(
            "  {:>3}  {:<12} {:>8}  {}/{}",
            row.id(),
            row.name(),
            row.sound_id(),
            row.footstep_spray_run(),
            row.footstep_spray_walk(),
        );
    }

    // Which material word each terrain value's sounds carry. Only the sounds
    // whose *name* states a material can vote; the rest ("SpiderAllSurface")
    // say nothing about the ground and are excluded rather than counted as
    // misses.
    const MATERIALS: [&str; 8] = [
        "Dirt", "Metal", "Stone", "Snow", "Wood", "Grass", "Leaves", "Sand",
    ];
    let material_of = |name: &str| -> Option<&'static str> {
        MATERIALS
            .into_iter()
            .find(|m| name.to_lowercase().contains(&m.to_lowercase()))
    };
    // The two readings, each mapping a terrain column value to the names it
    // would claim. `DustyGrass` shares `Grass`'s sound, so a sound id can name
    // more than one row.
    let by_sound_id = |value: u32| -> Vec<String> {
        terrain
            .iter()
            .filter(|r| r.sound_id() == value)
            .map(|r| r.name().to_string())
            .collect()
    };
    let by_row_id = |value: u32| -> Vec<String> {
        terrain
            .iter()
            .filter(|r| r.id() == value)
            .map(|r| r.name().to_string())
            .collect()
    };
    let agrees = |claims: &[String], material: &str| {
        claims.iter().any(|c| {
            let c = c.to_lowercase().replace("metallic", "metal");
            c.contains(&material.to_lowercase()) || material.to_lowercase().contains(&c)
        })
    };

    let (mut votes_sound, mut votes_row, mut voters) = (0usize, 0usize, 0usize);
    let mut per_value: BTreeMap<u32, BTreeMap<&str, usize>> = BTreeMap::new();
    let (mut resolved, mut references) = (0usize, 0usize);
    let mut types: BTreeMap<&str, BTreeMap<u32, usize>> = BTreeMap::new();
    let mut splash_named = 0usize;
    let mut splash_set = 0usize;
    for row in lookup.iter() {
        for (column, id) in [("sound", row.sound()), ("splash", row.sound_splash())] {
            if id == 0 {
                continue;
            }
            references += 1;
            let Some((kind, name)) = sound.get(&id) else {
                continue;
            };
            resolved += 1;
            *types.entry(column).or_default().entry(*kind).or_default() += 1;
            if column == "splash" {
                splash_set += 1;
                if name.to_lowercase().contains("splash") {
                    splash_named += 1;
                }
            }
        }
        let Some((_, name)) = sound.get(&row.sound()) else {
            continue;
        };
        let Some(material) = material_of(name) else {
            continue;
        };
        voters += 1;
        *per_value
            .entry(row.terrain())
            .or_default()
            .entry(material)
            .or_default() += 1;
        if agrees(&by_sound_id(row.terrain()), material) {
            votes_sound += 1;
        }
        if agrees(&by_row_id(row.terrain()), material) {
            votes_row += 1;
        }
    }

    println!("\nFootstepTerrainLookup: {} rows", lookup.len());
    println!("  {resolved} of {references} sound references resolve");
    for (column, kinds) in &types {
        let listed: Vec<String> = kinds.iter().map(|(k, n)| format!("type {k} x{n}")).collect();
        println!("  {column:<7} {}", listed.join(", "));
    }
    println!("  {splash_named} of {splash_set} splash sounds are named `Splash`");

    println!("\n  which reading of the terrain column agrees with the sound names");
    println!(
        "  as a TerrainType.sound_id: {votes_sound} of {voters}  ({:.0}%)",
        100.0 * votes_sound as f32 / voters.max(1) as f32
    );
    println!(
        "  as a TerrainType row id:   {votes_row} of {voters}  ({:.0}%)",
        100.0 * votes_row as f32 / voters.max(1) as f32
    );
    println!("\n  {:>7}  {:<20} {:<20} {}", "terrain", "sound_id says", "row id says", "names seen");
    for (value, materials) in &per_value {
        let seen: Vec<String> = materials.iter().map(|(m, n)| format!("{m} x{n}")).collect();
        println!(
            "  {value:>7}  {:<20} {:<20} {}",
            by_sound_id(*value).join("/"),
            by_row_id(*value).join("/"),
            seen.join(", "),
        );
    }

    // The creature side: which column of `CreatureSoundData` names a group.
    if let Ok(csd) = chain
        .read(CreatureSoundData::PATH)
        .ok()
        .map(|b| CreatureSoundData::parse(&b))
        .unwrap_or(Err(dbc::Error::UnexpectedSchema {
            table: CreatureSoundData::NAME,
            expected: CreatureSoundData::FIELDS,
            got: 0,
        }))
    {
        let groups: std::collections::BTreeSet<u32> =
            lookup.iter().map(|r| r.creature_footstep_id()).collect();
        let (mut set, mut inside) = (0usize, 0usize);
        for row in csd.iter() {
            if row.footstep_group() == 0 {
                continue;
            }
            set += 1;
            if groups.contains(&row.footstep_group()) {
                inside += 1;
            }
        }
        println!(
            "\nCreatureSoundData: {set} rows name a footstep group, {inside} of them name one \
             of the {} groups that exist",
            groups.len()
        );
    }

    // The ground side.
    let textures = GroundEffectTexture::parse(&chain.read(GroundEffectTexture::PATH)?)?;
    let effect: BTreeMap<u32, u32> = textures.iter().map(|r| (r.id(), r.terrain_type())).collect();
    let named: BTreeMap<u32, String> = terrain
        .iter()
        .map(|r| (r.id(), r.name().to_string()))
        .collect();

    let maps: Vec<String> = match map {
        Some(m) => vec![m.to_string()],
        None => vec!["Azeroth".to_string()],
    };
    let (mut chunks, mut layers, mut with_effect, mut effect_resolved) = (0u64, 0u64, 0u64, 0u64);
    let mut per_terrain: BTreeMap<u32, u64> = BTreeMap::new();
    let mut texture_words: BTreeMap<u32, BTreeMap<&str, u64>> = BTreeMap::new();
    let mut tiles = 0usize;
    let mut budget = limit.unwrap_or(64);
    for name in &maps {
        let Ok(wdt) = load_wdt(chain, name) else {
            continue;
        };
        for (x, y) in wdt.tiles() {
            if budget == 0 {
                break;
            }
            let Ok(bytes) = chain.read(&adt::tile_path(name, x, y)) else {
                continue;
            };
            let Ok(tile) = adt::Adt::parse(&bytes, wdt.big_alpha()) else {
                continue;
            };
            budget -= 1;
            tiles += 1;
            for chunk in &tile.chunks {
                chunks += 1;
                for layer in &chunk.layers {
                    layers += 1;
                    if layer.effect_id == 0 {
                        continue;
                    }
                    with_effect += 1;
                    let Some(&terrain_id) = effect.get(&layer.effect_id) else {
                        continue;
                    };
                    effect_resolved += 1;
                    *per_terrain.entry(terrain_id).or_default() += 1;
                    // **The texture's own filename is the check on the whole
                    // chain.** A layer drawing `ElwynnRockBase01` that comes
                    // out as `Stone` is the column identifying itself, the
                    // same way `CreatureSoundData`'s columns did -- and no
                    // wrong column could do it, because a filename is not a
                    // small integer.
                    if let Some(texture) = tile.textures.get(layer.texture_id as usize) {
                        let lower = texture.to_lowercase();
                        for word in [
                            "grass", "dirt", "rock", "stone", "snow", "sand", "wood", "leaf",
                            "leaves", "water", "metal", "mud", "cobble", "brick", "lava",
                        ] {
                            if lower.contains(word) {
                                *texture_words
                                    .entry(terrain_id)
                                    .or_default()
                                    .entry(word)
                                    .or_default() += 1u64;
                            }
                        }
                    }
                }
            }
        }
    }

    println!("\nthe ground: {tiles} tiles, {chunks} chunks, {layers} texture layers");
    println!(
        "  {with_effect} layers name a GroundEffectTexture, {effect_resolved} of those resolve"
    );
    println!("  the terrains they reach:");
    for (id, count) in &per_terrain {
        let mut words: Vec<(&&str, &u64)> = texture_words
            .get(id)
            .map(|w| w.iter().collect())
            .unwrap_or_default();
        words.sort_by(|a, b| b.1.cmp(a.1));
        let listed: Vec<String> = words
            .iter()
            .take(4)
            .map(|(w, n)| format!("{w} x{n}"))
            .collect();
        println!(
            "    {id:>3} {:<12} {count:>8} layers   textures called: {}",
            named.get(id).map(String::as_str).unwrap_or("?"),
            listed.join(", "),
        );
    }

    // **End to end, on ground a person can point at.** Every link above is
    // checked in isolation, and a chain of correct links can still be wired up
    // wrong -- so this walks a real tile, asks what is underfoot at each cell
    // the way the viewer will, and prints the *sound name* that comes out. A
    // composed thing needs a way to be seen as itself.
    let group = chain
        .read(CreatureSoundData::PATH)
        .ok()
        .and_then(|b| CreatureSoundData::parse(&b).ok())
        .and_then(|table| {
            let models = dbc::schema::CreatureModelData::parse(
                &chain.read(dbc::schema::CreatureModelData::PATH).ok()?,
            )
            .ok()?;
            let displays = dbc::schema::CreatureDisplayInfo::parse(
                &chain.read(dbc::schema::CreatureDisplayInfo::PATH).ok()?,
            )
            .ok()?;
            let display = displays.iter().find(|d| d.id() == walker)?;
            let sound = match display.sound_id() {
                0 => models
                    .iter()
                    .find(|m| m.id() == display.model_id())
                    .map(|m| m.sound_id())
                    .unwrap_or(0),
                own => own,
            };
            table
                .iter()
                .find(|r| r.id() == sound)
                .map(|r| r.footstep_group())
        });
    println!("\nwalking {}/{} as display {walker}", maps[0], tile.unwrap_or("32,48"));
    match group {
        None => println!("  that display has no footstep group; it would be silent"),
        Some(group) => {
            let step: BTreeMap<(u32, u32), (u32, u32)> = lookup
                .iter()
                .map(|r| {
                    (
                        (r.creature_footstep_id(), r.terrain()),
                        (r.sound(), r.sound_splash()),
                    )
                })
                .collect();
            let (tx, ty) = tile
                .and_then(|t| t.split_once(','))
                .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
                .unwrap_or((32usize, 48usize));
            let Ok(wdt) = load_wdt(chain, &maps[0]) else {
                return Ok(());
            };
            let Ok(bytes) = chain.read(&adt::tile_path(&maps[0], tx, ty)) else {
                println!("  tile {tx},{ty} is not in this install");
                return Ok(());
            };
            let parsed = adt::Adt::parse(&bytes, wdt.big_alpha())?;
            let mut heard: BTreeMap<String, usize> = BTreeMap::new();
            let mut cells = 0usize;
            for chunk in &parsed.chunks {
                for cell in adt::footing::footing_grid(chunk) {
                    cells += 1;
                    // Exactly the viewer's own chain, and deliberately its
                    // fallbacks too: a layer that names no ground effect and a
                    // ground effect that names no terrain both land on the
                    // lookup's terrain 0.
                    let surface = chunk
                        .layers
                        .get(cell as usize)
                        .map(|l| l.effect_id)
                        .filter(|e| *e != 0)
                        .and_then(|e| effect.get(&e).copied())
                        .and_then(|row| {
                            terrain.iter().find(|t| t.id() == row).map(|t| t.sound_id())
                        })
                        .unwrap_or(0);
                    let id = step
                        .get(&(group, surface))
                        .or_else(|| step.get(&(group, 0)))
                        .map(|s| s.0);
                    let label = id
                        .and_then(|id| sound.get(&id).map(|(_, n)| n.clone()))
                        .unwrap_or_else(|| "(silence)".to_string());
                    *heard.entry(label).or_default() += 1;
                }
            }
            println!("  group {group}, {cells} cells over the tile:");
            let mut by_count: Vec<(&String, &usize)> = heard.iter().collect();
            by_count.sort_by(|a, b| b.1.cmp(a.1));
            for (name, count) in by_count {
                println!("    {count:>6}  {name}");
            }
        }
    }

    Ok(())
}

fn sound_cmd(chain: &mut Chain, cmd: &SoundCommand) -> Result<()> {
    use dbc::schema::SoundEntries;

    let bytes = chain
        .read(SoundEntries::PATH)
        .with_context(|| format!("reading {}", SoundEntries::PATH))?;
    let table = SoundEntries::parse(&bytes)?;

    match cmd {
        SoundCommand::List { filter, limit } => {
            let wanted = filter.as_ref().map(|f| f.to_lowercase());
            let mut shown = 0;
            let mut matched = 0;
            for row in table.iter() {
                if let Some(wanted) = &wanted {
                    if !row.name().to_lowercase().contains(wanted.as_str()) {
                        continue;
                    }
                }
                matched += 1;
                if shown >= *limit {
                    continue;
                }
                shown += 1;
                println!(
                    "  {:>6}  type {:>2}  vol {:.2}  {} .. {}  {}",
                    row.id(),
                    row.sound_type(),
                    row.volume(),
                    row.min_distance(),
                    row.distance_cutoff(),
                    row.name()
                );
                for path in row.paths() {
                    println!("           {path}");
                }
            }
            println!("\n{matched} matched of {} entries", table.len());
        }

        SoundCommand::Survey { limit } => {
            let mut entries = 0usize;
            let mut files = 0usize;
            let mut missing = 0usize;
            let mut no_files = 0usize;
            // Kept so a failure is reportable rather than merely counted: a
            // survey that says "3% missing" and cannot say which is a survey
            // nobody can act on.
            let mut examples: Vec<String> = Vec::new();

            for row in table.iter() {
                if limit.is_some_and(|l| entries >= l) {
                    break;
                }
                entries += 1;
                let paths = row.paths();
                if paths.is_empty() {
                    no_files += 1;
                    continue;
                }
                for path in paths {
                    files += 1;
                    if chain.read(&path).is_err() {
                        missing += 1;
                        if examples.len() < 12 {
                            examples.push(format!("{} ({})", path, row.name()));
                        }
                    }
                }
            }

            let found = files - missing;
            println!("\n{entries} entries surveyed, {files} file references:");
            println!(
                "  {found} resolved ({:.1}%), {missing} missing",
                if files == 0 {
                    0.0
                } else {
                    found as f64 * 100.0 / files as f64
                }
            );
            println!("  {no_files} entries name no file at all");

            if !examples.is_empty() {
                println!("\nfirst few that did not resolve:");
                for example in &examples {
                    println!("  {example}");
                }
            }

            // The whole point of the number. A wrong column alignment does not
            // resolve a little worse -- it resolves essentially not at all.
            println!(
                "\nA correct column layout resolves nearly everything. Anything\n\
                 like a uniform failure means the file and directory columns are\n\
                 not where this schema says they are."
            );
        }

        SoundCommand::Footsteps {
            map,
            limit,
            tile,
            walker,
        } => {
            return sound_footsteps(chain, map.as_deref(), *limit, tile.as_deref(), *walker);
        }

        SoundCommand::Types => {
            use std::collections::BTreeMap;
            // For each type: how many entries, and which top-level folders
            // their files sit in. The folder is the evidence -- a type whose
            // entries all live under `Sound\Music` is the music type.
            let mut by_type: BTreeMap<u32, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
            for row in table.iter() {
                let entry = by_type.entry(row.sound_type()).or_default();
                entry.0 += 1;
                let directory = row.directory();
                // Two levels is enough to separate Music from Ambience from
                // Creature without drowning in per-zone folders.
                let head: String = directory
                    .split('\\')
                    .take(2)
                    .collect::<Vec<_>>()
                    .join("\\");
                if !head.is_empty() {
                    *entry.1.entry(head).or_default() += 1;
                }
            }

            println!("\n{} sound types:", by_type.len());
            for (kind, (count, folders)) in &by_type {
                let mut top: Vec<(&String, &usize)> = folders.iter().collect();
                top.sort_by(|a, b| b.1.cmp(a.1));
                let summary: Vec<String> = top
                    .iter()
                    .take(3)
                    .map(|(folder, n)| format!("{folder} x{n}"))
                    .collect();
                println!("  type {kind:>2}: {count:>5} entries   {}", summary.join(", "));
            }
        }

        SoundCommand::Zones => {
            use dbc::schema::{AreaTable, SoundAmbience, SoundType, ZoneMusic};

            let by_id: std::collections::HashMap<u32, u32> =
                table.iter().map(|row| (row.id(), row.sound_type())).collect();

            // `(label, id)` pairs to check, and the type each must resolve to.
            let mut checks: Vec<(&str, u32, SoundType)> = Vec::new();

            let music_bytes = chain.read(ZoneMusic::PATH)?;
            let music = ZoneMusic::parse(&music_bytes)?;
            for row in music.iter() {
                for id in [row.day_sound(), row.night_sound()] {
                    if id != 0 {
                        checks.push(("ZoneMusic", id, SoundType::Music));
                    }
                }
            }

            let ambience_bytes = chain.read(SoundAmbience::PATH)?;
            let ambience = SoundAmbience::parse(&ambience_bytes)?;
            for row in ambience.iter() {
                for id in [row.day_sound(), row.night_sound()] {
                    if id != 0 {
                        checks.push(("SoundAmbience", id, SoundType::Ambience));
                    }
                }
            }

            let mut unknown = 0usize;
            let mut wrong_type = 0usize;
            let mut examples: Vec<String> = Vec::new();
            for (source, id, expected) in &checks {
                match by_id.get(id) {
                    None => {
                        unknown += 1;
                        if examples.len() < 10 {
                            examples.push(format!("{source} {id}: no such sound entry"));
                        }
                    }
                    Some(raw) => {
                        let actual = SoundType::from_raw(*raw);
                        if actual != *expected {
                            wrong_type += 1;
                            if examples.len() < 10 {
                                examples.push(format!(
                                    "{source} {id}: type {raw} ({actual:?}), expected {expected:?}"
                                ));
                            }
                        }
                    }
                }
            }

            let good = checks.len() - unknown - wrong_type;
            println!("
{} zone sound references checked:", checks.len());
            println!(
                "  {good} resolve to a sound of the right type ({:.1}%)",
                if checks.is_empty() {
                    0.0
                } else {
                    good as f64 * 100.0 / checks.len() as f64
                }
            );
            println!("  {unknown} name no sound entry at all");
            println!("  {wrong_type} resolve to a sound of the wrong type");
            for example in &examples {
                println!("    {example}");
            }

            // And how many zones actually reach any of this, which is the
            // number that says whether wiring it up is worth anything.
            let area_bytes = chain.read(AreaTable::PATH)?;
            let areas = AreaTable::parse(&area_bytes)?;
            let with_music = areas.iter().filter(|a| a.zone_music() != 0).count();
            let with_ambience = areas.iter().filter(|a| a.ambience_id() != 0).count();
            println!(
                "
{} areas: {with_music} name zone music, {with_ambience} name ambience",
                areas.len()
            );
        }

        SoundCommand::Creatures { limit } => {
            use std::collections::BTreeMap;

            let names: std::collections::HashMap<u32, (String, u32)> = table
                .iter()
                .map(|row| (row.id(), (row.name().to_string(), row.sound_type())))
                .collect();

            let raw = chain.read(r"DBFilesClient\CreatureSoundData.dbc")?;
            let sounds = dbc::Dbc::parse(&raw)?;
            println!(
                "
CreatureSoundData: {} rows x {} fields",
                sounds.len(),
                sounds.fields()
            );

            // For each column: how many rows set it, how many of those resolve
            // to a sound at all, how many resolve to a *creature* sound, and
            // the words that keep appearing in their names.
            for field in 0..sounds.fields() as usize {
                let mut set = 0usize;
                let mut resolved = 0usize;
                let mut creature_typed = 0usize;
                let mut words: BTreeMap<String, usize> = BTreeMap::new();

                for (index, row) in sounds.rows().enumerate() {
                    if limit.is_some_and(|l| index >= l) {
                        break;
                    }
                    let id = row.u32(field);
                    if id == 0 {
                        continue;
                    }
                    set += 1;
                    let Some((name, kind)) = names.get(&id) else {
                        continue;
                    };
                    resolved += 1;
                    if dbc::schema::SoundType::from_raw(*kind)
                        == dbc::schema::SoundType::Creature
                    {
                        creature_typed += 1;
                    }
                    // Split a name like `WolfDeath` on its capitals and keep
                    // the trailing word -- that is the part that describes the
                    // *event* rather than the creature.
                    if let Some(word) = split_camel_tail(name) {
                        *words.entry(word).or_default() += 1;
                    }
                }

                if set == 0 {
                    println!("  field {field:>2}: never set");
                    continue;
                }
                let mut top: Vec<(&String, &usize)> = words.iter().collect();
                top.sort_by(|a, b| b.1.cmp(a.1));
                let summary: Vec<String> = top
                    .iter()
                    .take(3)
                    .map(|(word, n)| format!("{word} x{n}"))
                    .collect();
                println!(
                    "  field {field:>2}: {set:>4} set, {resolved:>4} resolve, {creature_typed:>4} are creature sounds   {}",
                    summary.join(", ")
                );
            }
        }

        SoundCommand::Export { id, out } => {
            let row = table
                .iter()
                .find(|row| row.id() == *id)
                .with_context(|| format!("no sound entry {id}"))?;
            let paths = row.paths();
            if paths.is_empty() {
                println!("sound {id} ({}) names no files", row.name());
                return Ok(());
            }
            println!("sound {id}: {} ({} file(s))", row.name(), paths.len());
            let directory = out.clone().unwrap_or_else(|| PathBuf::from("."));
            std::fs::create_dir_all(&directory)?;
            for path in &paths {
                let bytes = match chain.read(path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        println!("  {path}: {error}");
                        continue;
                    }
                };
                let name = path.rsplit('\\').next().unwrap_or("sound.bin");
                let target = directory.join(name);
                std::fs::write(&target, &bytes)?;
                println!("  {} bytes -> {}", bytes.len(), target.display());
            }
        }
    }

    Ok(())
}

#[derive(clap::Subcommand)]
enum SpellCommand {
    /// Tally every `$`-introduced token across every description, split
    /// into resolved and passed-through, so the next column hunt has a
    /// priority order instead of a guess.
    ///
    /// **A counting job, deliberately.** This does not resolve a single new
    /// token or identify a single new column -- see `dbc::spelltext`'s doc
    /// comment for why guessing at a column is the most dangerous work in
    /// this repo. What comes back is only frequencies and one example spell
    /// id per bucket, read straight off `dbc::spelltext::scan`, which shares
    /// its resolved/unresolved judgement with the real substituter rather
    /// than re-deriving the grammar.
    Tokens {
        /// Show only buckets with at least this many occurrences.
        #[arg(long, default_value_t = 1)]
        min_count: usize,
    },
}

fn spell_cmd(chain: &mut Chain, cmd: &SpellCommand) -> Result<()> {
    use dbc::schema::{Spell, SpellDuration, SpellRadius};
    use dbc::spelltext;
    use std::collections::HashMap;

    match cmd {
        SpellCommand::Tokens { min_count } => {
            let spells = Spell::parse(&chain.read(Spell::PATH)?)?;
            let durations: HashMap<u32, i32> = SpellDuration::parse(&chain.read(SpellDuration::PATH)?)?
                .iter()
                .map(|row| (row.id(), row.duration()))
                .collect();
            let radii: HashMap<u32, f32> = SpellRadius::parse(&chain.read(SpellRadius::PATH)?)?
                .iter()
                .map(|row| (row.id(), row.radius()))
                .collect();

            // Every spell's numbers, scoped to the whole table rather than to
            // one character's known spells -- a description can name a
            // *different* spell's row (`$6788d`), and `wow-cli` has no
            // character to scope against in the first place.
            let values: HashMap<u32, spelltext::Values> = spells
                .iter()
                .map(|row| (row.id(), spelltext::values_from_row(&row, &durations, &radii)))
                .collect();

            struct Bucket {
                occurrences: usize,
                resolved: usize,
                example_spell: u32,
                example_raw: String,
            }
            let mut buckets: std::collections::BTreeMap<String, Bucket> = std::collections::BTreeMap::new();
            let mut described = 0usize;
            for row in spells.iter() {
                let description = row.description();
                if description.is_empty() {
                    continue;
                }
                described += 1;
                for hit in spelltext::scan(description, row.id(), &values) {
                    let bucket = buckets.entry(hit.bucket.clone()).or_insert_with(|| Bucket {
                        occurrences: 0,
                        resolved: 0,
                        example_spell: row.id(),
                        example_raw: hit.raw.clone(),
                    });
                    bucket.occurrences += 1;
                    if hit.resolved {
                        bucket.resolved += 1;
                    }
                }
            }

            let mut rows: Vec<(&String, &Bucket)> =
                buckets.iter().filter(|(_, b)| b.occurrences >= *min_count).collect();
            rows.sort_by(|a, b| b.1.occurrences.cmp(&a.1.occurrences));

            println!(
                "{described} non-empty descriptions of {} spells in Spell.dbc\n",
                spells.len()
            );
            println!(
                "{:<12} {:>10} {:>10} {:>8}  {:>8}  example",
                "token", "count", "resolved", "pass", "spell"
            );
            let mut total = 0usize;
            let mut total_resolved = 0usize;
            for (bucket, b) in &rows {
                let pass = b.occurrences - b.resolved;
                total += b.occurrences;
                total_resolved += b.resolved;
                println!(
                    "{:<12} {:>10} {:>10} {:>8}  {:>8}  {}",
                    bucket, b.occurrences, b.resolved, pass, b.example_spell, b.example_raw
                );
            }
            println!(
                "\n{total} total occurrences across {} buckets, {total_resolved} resolved ({:.1}%), {} passed through",
                rows.len(),
                total_resolved as f64 * 100.0 / total.max(1) as f64,
                total - total_resolved
            );
        }
    }

    Ok(())
}

/// The trailing capitalised word of a name like `WolfDeath` -> `Death`.
///
/// The event half of a sound's label. Splitting on capitals is crude and does
/// not need to be better: this is a tally used to *read* what a column
/// contains, not something any behaviour depends on.
fn split_camel_tail(name: &str) -> Option<String> {
    let mut start = 0;
    for (index, ch) in name.char_indices() {
        if ch.is_ascii_uppercase() {
            start = index;
        }
    }
    let tail = name.get(start..)?.trim_end_matches(|c: char| c.is_ascii_digit());
    (!tail.is_empty()).then(|| tail.to_string())
}

fn item_cmd(chain: &mut Chain, cmd: ItemCommand) -> Result<()> {
    match cmd {
        ItemCommand::Display { display_id } => item_display(chain, display_id),
        ItemCommand::Held => item_held(chain),
        ItemCommand::Sheath => item_sheath(chain),
    }
}

/// Cross-tabulates sheathe type against inventory type.
///
/// A weapon's stowed position is not a client constant, it is a column -- so
/// the first question is how many distinct values it takes and whether they
/// partition the held slots the way a set of resting places would. If every
/// two-hander shares one value and every one-hander another, the column means
/// what its name says; if the values scatter across slots, it means something
/// else and nothing should be built on it.
fn item_sheath(chain: &mut Chain) -> Result<()> {
    use dbc::schema::Item;
    use std::collections::{BTreeMap, BTreeSet};

    let items = Item::parse(&chain.read(Item::PATH)?)?;

    // Only the slots that hang geometry can be sheathed at all; the rest paint
    // the body and have nowhere to be stowed from.
    let held: BTreeSet<u32> = [13, 14, 15, 17, 21, 22, 23, 25, 26].into_iter().collect();

    let mut grid: BTreeMap<u32, BTreeMap<u32, usize>> = BTreeMap::new();
    let mut totals: BTreeMap<u32, usize> = BTreeMap::new();
    for item in items.iter() {
        *grid
            .entry(item.inventory_type())
            .or_default()
            .entry(item.sheathe_type())
            .or_default() += 1;
        *totals.entry(item.sheathe_type()).or_default() += 1;
    }

    println!("\nsheathe_type across all {} items: {:?}", items.len(), totals);
    println!("\nheld slots only:");
    println!("  {:>5} {:>8}  distribution of sheathe_type", "slot", "items");
    for (slot, row) in &grid {
        if !held.contains(slot) {
            continue;
        }
        let count: usize = row.values().sum();
        let mut parts: Vec<String> = row
            .iter()
            .map(|(sheathe, n)| {
                format!("{sheathe}: {n} ({:.0}%)", 100.0 * *n as f32 / count as f32)
            })
            .collect();
        parts.sort();
        println!("  {slot:>5} {count:>8}  {}", parts.join(", "));
    }

    // A slot that splits across two sheathe types is the interesting case: it
    // means the resting place depends on something finer than the slot, and
    // `Item.dbc`'s own class/subclass is the only other thing it could be.
    println!("\nheld slots by weapon subclass, for the slots that split:");
    let mut by_subclass: BTreeMap<(u32, u32, u32), BTreeMap<u32, usize>> = BTreeMap::new();
    for item in items.iter() {
        if !held.contains(&item.inventory_type()) {
            continue;
        }
        *by_subclass
            .entry((item.inventory_type(), item.class_id(), item.subclass_id()))
            .or_default()
            .entry(item.sheathe_type())
            .or_default() += 1;
    }
    println!(
        "  {:>5} {:>6} {:>9} {:>8}  sheathe_type",
        "slot", "class", "subclass", "items"
    );
    for ((slot, class, subclass), row) in &by_subclass {
        let count: usize = row.values().sum();
        if count < 20 {
            continue;
        }
        let mut parts: Vec<String> = row
            .iter()
            .filter(|(_, n)| **n * 20 >= count)
            .map(|(sheathe, n)| {
                format!("{sheathe}: {:.0}%", 100.0 * *n as f32 / count as f32)
            })
            .collect();
        parts.sort();
        println!("  {slot:>5} {class:>6} {subclass:>9} {count:>8}  {}", parts.join(", "));
    }

    // The lookup this client actually has to perform. Our own equipment
    // arrives from the character list as *display* ids, and `sheathe_type` is
    // keyed by item entry -- several of which can share one display. So the
    // question is not "what is the sheathe type" but "does a display id
    // determine one", and a display whose items disagree cannot answer.
    let mut per_display: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for item in items.iter() {
        if !held.contains(&item.inventory_type()) || item.display_info_id() == 0 {
            continue;
        }
        per_display
            .entry(item.display_info_id())
            .or_default()
            .insert(item.sheathe_type());
    }
    let ambiguous = per_display.values().filter(|s| s.len() > 1).count();
    println!(
        "\ndisplay id -> sheathe_type: {} displays on held items, {ambiguous} ambiguous ({:.2}%)",
        per_display.len(),
        100.0 * ambiguous as f32 / per_display.len().max(1) as f32
    );
    for (display, states) in per_display.iter().filter(|(_, s)| s.len() > 1).take(5) {
        println!("  display {display} is used at {states:?}");
    }

    println!("\nthe painted slots, as a control:");
    for (slot, row) in &grid {
        if held.contains(slot) {
            continue;
        }
        let count: usize = row.values().sum();
        let dominant = row.iter().max_by_key(|(_, n)| **n);
        if let Some((sheathe, n)) = dominant {
            println!(
                "  {slot:>5} {count:>8}  mostly {sheathe} ({:.0}%), {} distinct",
                100.0 * *n as f32 / count as f32,
                row.len()
            );
        }
    }
    Ok(())
}

/// Shows what one `ItemDisplayInfo` row hangs on the character.
fn item_display(chain: &mut Chain, display_id: u32) -> Result<()> {
    use dbc::schema::ItemDisplayInfo;

    let table = ItemDisplayInfo::parse(&chain.read(ItemDisplayInfo::PATH)?)?;
    let row = table
        .iter()
        .find(|r| r.id() == display_id)
        .with_context(|| format!("no ItemDisplayInfo row {display_id}"))?;

    // The lookup the renderer has to make, run the same way round: from a
    // display id, because that is all the character list gives us.
    let sheathe: Vec<u32> = dbc::schema::Item::parse(&chain.read(dbc::schema::Item::PATH)?)
        .map(|items| {
            let mut seen: Vec<u32> = items
                .iter()
                .filter(|i| i.display_info_id() == display_id)
                .map(|i| i.sheathe_type())
                .collect();
            seen.sort_unstable();
            seen.dedup();
            seen
        })
        .unwrap_or_default();

    println!("item display {display_id}");
    println!("  sheathe_type from the items using this display: {sheathe:?}");
    for (hand, model, texture) in [
        ("right", row.model_right(), row.model_texture_right()),
        ("left", row.model_left(), row.model_texture_left()),
    ] {
        if model.is_empty() {
            println!("  {hand:<5}  (no geometry)");
            continue;
        }
        // The DBC names `.mdx`; the archives hold `.m2`. Same rename the
        // character loader already does.
        let path = format!(r"Item\ObjectComponents\Weapon\{}", m2::model_path(model));
        let readable = chain.read(&path).is_ok();
        println!(
            "  {hand:<5}  {model}  texture {:?}\n         {path} {}",
            texture,
            if readable { "reads" } else { "MISSING" }
        );
        if readable {
            let bytes = chain.read(&path)?;
            let held = m2::Model::parse(&bytes)?;
            let (min, max) = held.bounding_box();
            println!(
                "         {} vertices, {} bones, {} attachments, extent {:.2} x {:.2} x {:.2}",
                held.vertex_count(),
                held.bones().len(),
                held.attachment_count(),
                max[0] - min[0],
                max[1] - min[1],
                max[2] - min[2],
            );
        }
    }
    Ok(())
}

/// Tallies held geometry by inventory slot.
fn item_held(chain: &mut Chain) -> Result<()> {
    use dbc::schema::{Item, ItemDisplayInfo};
    use std::collections::BTreeMap;

    let items = Item::parse(&chain.read(Item::PATH)?)?;
    let displays = ItemDisplayInfo::parse(&chain.read(ItemDisplayInfo::PATH)?)?;

    // One pass to index the displays; the join is 46,000 by 58,000 otherwise.
    let mut geometry: BTreeMap<u32, (bool, bool)> = BTreeMap::new();
    for row in displays.iter() {
        geometry.insert(
            row.id(),
            (!row.model_right().is_empty(), !row.model_left().is_empty()),
        );
    }

    // Which folder an item's geometry sits in is not a column either, so it is
    // measured the same way: try every folder that exists and see which one
    // answers. An MPQ resolves by hash, so a miss costs a lookup, not a scan.
    let folders = [
        "Weapon", "Shield", "Shoulder", "Head", "Cape", "Quiver", "Pouch", "Ammo",
    ];
    let mut resolved: BTreeMap<String, Option<&'static str>> = BTreeMap::new();

    #[derive(Default)]
    struct Slot {
        items: usize,
        right: usize,
        left: usize,
        both: usize,
        example: u32,
        folders: BTreeMap<&'static str, usize>,
        unresolved: usize,
    }
    let mut slots: BTreeMap<u32, Slot> = BTreeMap::new();
    for item in items.iter() {
        let inventory_type = item.inventory_type();
        let (right, left) = geometry
            .get(&item.display_info_id())
            .copied()
            .unwrap_or((false, false));

        // Borrowed separately from the folder probe below, which needs the
        // chain mutably while the slot entry would still be alive.
        {
            let slot = slots.entry(inventory_type).or_default();
            slot.items += 1;
            if right {
                slot.right += 1;
            }
            if left {
                slot.left += 1;
            }
            if right && left {
                slot.both += 1;
            }
            if (right || left) && slot.example == 0 {
                slot.example = item.display_info_id();
            }
        }

        if !(right || left) {
            continue;
        }
        let Some(row) = displays.iter().find(|r| r.id() == item.display_info_id()) else {
            continue;
        };
        let name = if left { row.model_left() } else { row.model_right() };
        let file = m2::model_path(name);
        let found = match resolved.get(&file) {
            Some(found) => *found,
            None => {
                let found = folders.iter().copied().find(|folder| {
                    chain
                        .read(&format!(r"Item\ObjectComponents\{folder}\{file}"))
                        .is_ok()
                });
                resolved.insert(file.clone(), found);
                found
            }
        };
        let slot = slots.entry(inventory_type).or_default();
        match found {
            Some(folder) => *slot.folders.entry(folder).or_default() += 1,
            None => slot.unresolved += 1,
        }
    }

    println!(
        "\n{} items across {} inventory types\n",
        items.len(),
        slots.len()
    );
    println!(
        "  {:>5} {:>8}  {:>16} {:>16} {:>6}  {:>7}  folders",
        "slot", "items", "model_right", "model_left", "both", "example"
    );
    for (slot, t) in &slots {
        let pct = |n: usize| 100.0 * n as f32 / t.items.max(1) as f32;
        let mut folders: Vec<String> = t
            .folders
            .iter()
            .map(|(name, count)| format!("{name} x{count}"))
            .collect();
        if t.unresolved > 0 {
            folders.push(format!("UNRESOLVED x{}", t.unresolved));
        }
        println!(
            "  {slot:>5} {:>8}  {:>9} {:>5.1}% {:>9} {:>5.1}% {:>6}  {:>7}  {}",
            t.items,
            t.right,
            pct(t.right),
            t.left,
            pct(t.left),
            t.both,
            if t.example == 0 {
                "-".to_string()
            } else {
                t.example.to_string()
            },
            folders.join(", ")
        );
    }
    Ok(())
}

/// Traces one attachment through an animation and reports which others it
/// approaches.
///
/// The point is to identify a *destination* from motion rather than from a
/// table. In `Sheath`, the character's hand carries the weapon to its resting
/// place and comes back; whichever static attachment the hand passes closest
/// to is where the weapon ends up. A candidate that is merely nearby all the
/// time -- the elbow, the other hip -- is separated from the real answer by
/// the *contrast* between its closest approach and its typical distance, so
/// both are reported.
fn m2_attach_trace(
    chain: &mut Chain,
    path: &str,
    anim: usize,
    track: u32,
    limit: usize,
) -> Result<()> {
    let path = m2::model_path(path);
    let model = m2::Model::parse(&chain.read(&path)?)?;
    let attachments = model.attachments();
    let sequences = model.sequences();

    let tracked = attachments
        .iter()
        .find(|a| a.id == track)
        .copied()
        .with_context(|| format!("{path} has no attachment {track}"))?;
    let sequence = sequences
        .get(anim)
        .with_context(|| format!("{path} has no sequence {anim}"))?;

    let mut external = std::collections::BTreeMap::new();
    for (i, seq) in sequences.iter().enumerate() {
        if seq.is_inline() {
            continue;
        }
        if let Ok(bytes) = chain.read(&m2::anim::external_anim_path(&path, seq)) {
            external.insert(i, bytes);
        }
    }
    let bones = model.animated_bones_with(&external);

    let names = dbc::schema::AnimationData::parse(&chain.read(dbc::schema::AnimationData::PATH)?)
        .ok();
    let name = names
        .as_ref()
        .and_then(|t| t.iter().find(|r| r.id() == sequence.id as u32))
        .map(|r| r.name().to_string())
        .unwrap_or_else(|| format!("#{}", sequence.id));

    println!("{path}");
    println!(
        "  tracing attachment {track} through sequence {anim} ({name}, {}ms)",
        sequence.duration_ms
    );

    let at = |pose: &[glam::Mat4], a: &m2::Attachment| -> glam::Vec3 {
        pose.get(a.bone as usize)
            .copied()
            .unwrap_or(glam::Mat4::IDENTITY)
            .transform_point3(glam::Vec3::from(a.position))
    };

    // Sample densely: the hand is only at the sheath for an instant, and a
    // coarse sample walks straight past the moment that identifies it.
    const SAMPLES: u32 = 240;
    let mut closest: Vec<(u32, f32, f32, f32)> = Vec::new();
    for candidate in &attachments {
        if candidate.id == track {
            continue;
        }
        let (mut nearest, mut total) = (f32::MAX, 0.0f32);
        for step in 0..=SAMPLES {
            let time = sequence.duration_ms * step / SAMPLES;
            let pose = m2::Model::pose_bones(&bones, anim, time);
            let distance = at(&pose, &tracked).distance(at(&pose, candidate));
            nearest = nearest.min(distance);
            total += distance;
        }
        let mean = total / (SAMPLES + 1) as f32;
        closest.push((candidate.id, nearest, mean, mean - nearest));
    }

    // Ranked by how much closer than usual the tracked point gets, not by
    // absolute distance: the hand is always near the hip and that proves
    // nothing, whereas *approaching and leaving* is the signature of a
    // destination.
    closest.sort_by(|a, b| b.3.total_cmp(&a.3));
    println!(
        "\n  {:>4} {:>10} {:>10} {:>10}   ranked by approach",
        "id", "closest", "mean", "approach"
    );
    for (id, nearest, mean, approach) in closest.iter().take(limit) {
        println!("  {id:>4} {nearest:>10.3} {mean:>10.3} {approach:>10.3}");
    }

    let by_distance = closest
        .iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|c| c.0);
    println!(
        "\n  nearest approach overall: attachment {:?}",
        by_distance
    );
    Ok(())
}

/// Dumps one model's attachment points.
///
/// Prints the bone each names beside the bone's own pivot, because those two
/// are the check: an attachment is drawn through its bone's matrix, and in the
/// bind pose that matrix is the identity, so the offset reads as a model-space
/// point and must land on -- not merely near -- the bone it hangs from.
fn m2_attachments(chain: &mut Chain, path: &str, anim: Option<usize>) -> Result<()> {
    let path = m2::model_path(path);
    let model = m2::Model::parse(&chain.read(&path)?)?;
    let bones = model.bones();
    let attachments = model.attachments();

    // Posed only when asked: decoding every track and loading the external
    // `.anim` files is far more work than reading the hierarchy.
    let posed = anim.map(|sequence| {
        let sequences = model.sequences();
        let mut external = std::collections::BTreeMap::new();
        for (i, seq) in sequences.iter().enumerate() {
            if seq.is_inline() {
                continue;
            }
            if let Ok(bytes) = chain.read(&m2::anim::external_anim_path(&path, seq)) {
                external.insert(i, bytes);
            }
        }
        let animated = model.animated_bones_with(&external);
        // Whether each attachment's bone is *driven* in this sequence at all,
        // as opposed to falling back to its bind orientation. The distinction
        // matters: an attachment that reads as bind pose because its track
        // holds no keys for this cycle looks identical to one deliberately
        // authored that way, and only one of those is a bug.
        println!("\n  bone tracks for each attachment:");
        for a in &model.attachments() {
            let Some(bone) = animated.get(a.bone as usize) else {
                continue;
            };
            let keyed = bone.rotation.sequences.get(sequence).is_some_and(|k| !k.values.is_empty());
            println!(
                "    attachment {:>3}: bone {:>3}  rotation track: {} entries, global {:?}, \
                 keys in this sequence: {}",
                a.id,
                a.bone,
                bone.rotation.sequences.len(),
                bone.rotation.global_sequence,
                keyed,
            );
        }
        let name = sequences
            .get(sequence)
            .map(|s| format!("sequence {sequence} (animation id {})", s.id))
            .unwrap_or_else(|| format!("sequence {sequence} (absent)"));
        (m2::Model::pose_bones(&animated, sequence, 0), name)
    });

    let (min, max) = model.bounding_box();
    println!("{path}");
    println!(
        "  {} attachments, {} bones, bounds [{:.2} {:.2} {:.2}]..[{:.2} {:.2} {:.2}]",
        attachments.len(),
        bones.len(),
        min[0],
        min[1],
        min[2],
        max[0],
        max[1],
        max[2]
    );
    match &posed {
        Some((_, name)) => {
            println!("  posed with {name}\n");
            println!(
                "  {:>4} {:>5}  {:>26}  {:>26}  {:>7}",
                "id", "bone", "bind position", "posed position", "turned"
            );
        }
        None => println!(
            "\n  {:>4} {:>5} {:>4}  {:>26}  {:>26}  {:>7}",
            "id", "bone", "key", "attachment offset", "bone pivot", "apart"
        ),
    }
    for a in &attachments {
        let (key, pivot) = match bones.get(a.bone as usize) {
            Some(b) => (b.key_bone_id.to_string(), b.pivot),
            None => ("-".to_string(), [f32::NAN; 3]),
        };
        if let Some((pose, _)) = &posed {
            let matrix = pose
                .get(a.bone as usize)
                .copied()
                .unwrap_or(glam::Mat4::IDENTITY);
            let at = matrix.transform_point3(glam::Vec3::from(a.position));
            // How far the bone is turned, as an angle. The rotation is what
            // decides the pose of whatever hangs here; the translation only
            // decides where.
            let (_, rotation, _) = matrix.to_scale_rotation_translation();
            let turned = rotation.to_axis_angle().1.to_degrees();
            // Where a weapon hung here would *point*. An item model runs along
            // its own +X (a claymore's blade spans 1.81 units of it), so the
            // bone's rotated +X is the blade direction -- and that is the
            // constraint that separates a resting place from a grip. A stowed
            // sword lies up and back across the spine; a held one points
            // forward out of the fist.
            let blade = (matrix.transform_point3(glam::Vec3::X)
                - matrix.transform_point3(glam::Vec3::ZERO))
            .normalize_or_zero();
            println!(
                "  {:>4} {:>5}  {:>8.3} {:>8.3} {:>8.3}  {:>8.3} {:>8.3} {:>8.3}  \
                 {turned:>6.1}deg  blade {:>6.2} {:>6.2} {:>6.2}",
                a.id,
                a.bone,
                a.position[0],
                a.position[1],
                a.position[2],
                at.x,
                at.y,
                at.z,
                blade.x,
                blade.y,
                blade.z,
            );
            continue;
        }
        let apart = (0..3)
            .map(|i| (a.position[i] - pivot[i]).powi(2))
            .sum::<f32>()
            .sqrt();
        println!(
            "  {:>4} {:>5} {:>4}  {:>8.3} {:>8.3} {:>8.3}  {:>8.3} {:>8.3} {:>8.3}  {apart:>7.3}",
            a.id,
            a.bone,
            key,
            a.position[0],
            a.position[1],
            a.position[2],
            pivot[0],
            pivot[1],
            pivot[2],
        );
    }

    for issue in model.validate() {
        println!("\n  ! {issue}");
    }
    Ok(())
}

/// Tallies every attachment id across the archives.
///
/// The point is the shape of the population, not any single model: a wrong
/// stride reads ids out of the middle of a float and produces thousands of
/// distinct values, where the real vocabulary is small and heavily reused. The
/// side column is the other half -- ids that come in mirrored pairs (the two
/// hands, the two shoulders) sit consistently on one side of the model's plane
/// of symmetry, which is what identifies them without transcribing a table.
/// Dumps one model's emitters, with the tracks that drive them.
///
/// Prints the *values* of every curve rather than only its length, because an
/// emitter cannot be checked any other way: a flame is right when its colour
/// ramp runs orange to pale and its scale ramp grows then shrinks, and those
/// are visible here and nowhere else until something is drawn.
fn m2_emitters(chain: &mut Chain, path: &str) -> Result<()> {
    let path = m2::model_path(path);
    let bytes = chain.read(&path)?;
    let model = m2::Model::parse(&bytes)?;
    let textures = model.textures();
    let bones = model.bones();
    let sequences = model.sequences();

    let texture_name = |i: u16| -> String {
        match textures.get(i as usize) {
            Some(t) if t.is_hardcoded() => t.filename.clone(),
            Some(t) => format!("<supplied at runtime, type {}>", t.kind),
            None => format!("<no texture {i}, model has {}>", textures.len()),
        }
    };

    println!("{path}");
    println!(
        "  {} bones, {} textures, {} sequences",
        bones.len(),
        textures.len(),
        sequences.len()
    );

    let particles = model.particle_emitters();
    let ribbons = model.ribbon_emitters();
    println!(
        "  {} particle emitter(s), {} ribbon emitter(s)",
        particles.len(),
        ribbons.len()
    );

    /// First non-empty sequence of a track, with the sequence it came from.
    fn first_keys<T: m2::Keyframe + std::fmt::Debug>(
        track: &m2::Track<T>,
    ) -> Option<(usize, String)> {
        track.sequences.iter().enumerate().find_map(|(i, k)| {
            (!k.values.is_empty()).then(|| {
                let vals: Vec<String> =
                    k.values.iter().take(6).map(|v| format!("{v:?}")).collect();
                (i, vals.join(", "))
            })
        })
    }

    fn show_track(label: &str, track: &m2::Track<f32>) {
        match first_keys(track) {
            Some((seq, vals)) => println!("      {label:<22} seq {seq}: [{vals}]"),
            None => println!("      {label:<22} (no keys)"),
        }
    }

    fn show_part<T: m2::Keyframe + std::fmt::Debug>(label: &str, track: &m2::PartTrack<T>) {
        if track.values.is_empty() {
            println!("      {label:<22} (no keys)");
            return;
        }
        let pairs: Vec<String> = track
            .times
            .iter()
            .zip(&track.values)
            .map(|(t, v)| format!("{t:.2}->{v:?}"))
            .collect();
        println!("      {label:<22} {}", pairs.join("  "));
    }

    for (i, p) in particles.iter().enumerate() {
        println!(
            "\n  particle {i}: id {} flags {:#x} bone {} ({}) texture {} = {}",
            p.id,
            p.flags,
            p.bone,
            if (p.bone as usize) < bones.len() {
                "in range"
            } else {
                "OUT OF RANGE"
            },
            p.texture,
            texture_name(p.texture),
        );
        println!(
            "    at [{:.3} {:.3} {:.3}]  {} emitter, blend {}, {}x{} cells, \
             colour index {}, type {}, head/tail {}",
            p.position[0],
            p.position[1],
            p.position[2],
            p.emitter_type.name(),
            p.blend,
            p.rows,
            p.columns,
            p.color_index,
            p.particle_type,
            p.head_or_tail,
        );
        println!(
            "    lifespan vary {:.3}, rate vary {:.3}, drag {:.3}, spin {:.3}/{:.3}, \
             tail {:.3}, follow {:?}/{:?}",
            p.lifespan_vary,
            p.emission_rate_vary,
            p.drag,
            p.base_spin,
            p.spin,
            p.tail_length,
            p.follow_speed,
            p.follow_scale,
        );
        println!("    per-sequence tracks (milliseconds into the animation):");
        show_track("emission rate", &p.emission_rate);
        show_track("emission speed", &p.emission_speed);
        show_track("speed variation", &p.speed_variation);
        show_track("vertical range", &p.vertical_range);
        show_track("horizontal range", &p.horizontal_range);
        show_track("gravity", &p.gravity);
        show_track("lifespan", &p.lifespan);
        show_track("area length", &p.emission_area_length);
        show_track("area width", &p.emission_area_width);
        show_track("z source", &p.z_source);
        println!("    per-particle tracks (fraction of one particle's life):");
        show_part("colour (0..255)", &p.color);
        show_part("alpha", &p.alpha);
        show_part("scale", &p.scale);
        show_part("head cell", &p.head_cell);
        show_part("tail cell", &p.tail_cell);
        let enabled: Vec<String> = (0..sequences.len().max(1))
            .map(|s| format!("{}", u8::from(p.enabled(s, 0))))
            .collect();
        println!("      {:<22} {}", "enabled per sequence", enabled.join(""));
    }

    for (i, r) in ribbons.iter().enumerate() {
        println!(
            "\n  ribbon {i}: id {} bone {} ({}) at [{:.3} {:.3} {:.3}]",
            r.id,
            r.bone,
            if (r.bone as usize) < bones.len() {
                "in range"
            } else {
                "OUT OF RANGE"
            },
            r.position[0],
            r.position[1],
            r.position[2],
        );
        println!(
            "    {:.1} edges/s, edge lives {:.3}s, gravity {:.3}, {}x{} cells",
            r.edges_per_second, r.edge_lifetime, r.gravity, r.rows, r.columns,
        );
        for t in &r.textures {
            println!("    texture {t} = {}", texture_name(*t));
        }
        println!("    materials {:?}", r.materials);
        show_track("height above", &r.height_above);
        show_track("height below", &r.height_below);
        show_track("alpha", &r.alpha);
        match first_keys(&r.color) {
            Some((seq, vals)) => println!("      {:<22} seq {seq}: [{vals}]", "colour (0..255)"),
            None => println!("      {:<22} (no keys)", "colour (0..255)"),
        }
    }

    if particles.is_empty() && ribbons.is_empty() {
        println!("\n  nothing to draw: this model burns, trails and sprays nothing.");
    }
    Ok(())
}

/// Where an `(count, offset)` pair sits inside one emitter record.
///
/// Derived here rather than asked of the `m2` crate on purpose. The crate's
/// reading is the thing under test, and a probe that consults it would agree
/// with it however wrong both were -- the same reason the SRP6 tests carry a
/// server written from the protocol rather than from the client.
fn particle_array_positions() -> Vec<usize> {
    let mut out = vec![24, 32, 448];
    // Every `M2Track`: its two outer arrays sit at +4 and +12.
    for base in [52, 72, 92, 112, 132, 152, 176, 200, 220, 240, 456] {
        out.push(base + 4);
        out.push(base + 12);
    }
    // Every `M2PartTrack`: two arrays back to back, no header at all.
    for base in [260, 276, 292, 316, 332] {
        out.push(base);
        out.push(base + 8);
    }
    out
}

fn ribbon_array_positions() -> Vec<usize> {
    let mut out = vec![20, 28];
    for base in [36, 56, 76, 96, 132, 152] {
        out.push(base + 4);
        out.push(base + 12);
    }
    out
}

/// Whether one emitter block accounts for its own bytes at this stride.
///
/// The test that can fail: read every `(count, offset)` pair out of every
/// record, and require that all of them point *past* the end of the block and
/// inside the file, with the nearest one landing within a 16-byte alignment
/// pad of the block's end. A stride that is too small leaves later records'
/// data inside the block; one that is too large overruns into the data. A
/// stride that is wrong by a whole record's worth mangles the fields
/// themselves, which shows up as counts in the millions.
fn block_accounts_for_itself(
    data: &[u8],
    offset: usize,
    count: usize,
    stride: usize,
    positions: &[usize],
) -> bool {
    let word = |at: usize| -> Option<u32> {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let end = match offset.checked_add(count * stride) {
        Some(end) if end <= data.len() => end,
        _ => return false,
    };
    let mut nearest = usize::MAX;
    for record in 0..count {
        let at = offset + record * stride;
        for &position in positions {
            let (Some(n), Some(o)) = (word(at + position), word(at + position + 4)) else {
                return false;
            };
            if n == 0 {
                continue;
            }
            let o = o as usize;
            // A count of tens of thousands inside a record this size is not a
            // count; it is a float being read as one.
            if n > 0x1_0000 || o < end || o >= data.len() {
                return false;
            }
            nearest = nearest.min(o);
        }
    }
    // A block whose records refer to nothing at all cannot vote either way.
    nearest == usize::MAX || nearest - end < 16
}

/// Reads every model and asks whether the emitter blocks parse as themselves.
///
/// Two questions, and they are different. The **stride** question is settled by
/// byte accounting, which can come out the other way -- see
/// [`block_accounts_for_itself`]. The **field** question is settled by
/// properties the values must have: a bone index has to name a bone that
/// exists, a texture index a texture that exists, and an emitter type has to
/// be one of the three the format defines. A wrong offset satisfies none of
/// those for long.
/// Where the events `(count, offset)` pair sits in the M2 header.
///
/// Derived by counting back from the ribbon array at `0x120`, which
/// [`m2_emitter_survey`] already relies on: camera lookup, cameras, lights and
/// events are the four `(count, offset)` pairs before it.
const EVENTS_ARRAY_AT: usize = 0x100;

fn m2_events(chain: &mut Chain, path: &str, anims: bool) -> Result<()> {
    let path = m2::model_path(path);
    let bytes = chain.read(&path).with_context(|| format!("reading {path}"))?;
    let model = m2::Model::parse(&bytes)?;
    let sequences = model.sequences();

    let mut external = std::collections::BTreeMap::new();
    if anims {
        for (i, seq) in sequences.iter().enumerate() {
            if seq.is_inline() {
                continue;
            }
            if let Ok(bytes) = chain.read(&m2::anim::external_anim_path(&path, seq)) {
                external.insert(i, bytes);
            }
        }
    }
    let events = model.events_with(&external);

    let names = dbc::schema::AnimationData::parse(&chain.read(dbc::schema::AnimationData::PATH)?)
        .ok();
    let anim_name = |id: u16| -> String {
        names
            .as_ref()
            .and_then(|t| t.iter().find(|r| r.id() == id as u32))
            .map(|r| r.name().to_string())
            .unwrap_or_else(|| format!("#{id}"))
    };

    println!("{path}");
    println!(
        "  {} sequences, {} events, {} external .anim files loaded",
        sequences.len(),
        events.len(),
        external.len()
    );
    if events.is_empty() {
        println!("  (this model carries no timed events)");
        return Ok(());
    }
    for event in &events {
        let fired: Vec<usize> = (0..sequences.len())
            .filter(|&i| !event.times_in(i).is_empty())
            .collect();
        println!(
            "\n  {:?}  data {}  bone {}  at [{:.2}, {:.2}, {:.2}]  fires in {} of {} sequences",
            event.name(),
            event.data,
            event.bone,
            event.position[0],
            event.position[1],
            event.position[2],
            fired.len(),
            sequences.len(),
        );
        for i in fired.iter().take(12) {
            let seq = &sequences[*i];
            // Printed against the sequence's own duration because that is the
            // property a misread timestamp array breaks: a footfall at 41000ms
            // in a 1200ms walk is not a footfall.
            println!(
                "    [{i:3}] {:<24} {:>6}ms  at {:?}",
                anim_name(seq.id),
                seq.duration_ms,
                event.times_in(*i),
            );
        }
        if fired.len() > 12 {
            println!("    ... and {} more sequences", fired.len() - 12);
        }
    }
    Ok(())
}

/// Reads every model and asks the event block to identify itself.
///
/// **The stride question is settled by the identifier being a name.** Four
/// ASCII bytes cannot be arrived at by a coincidence of small integers, and a
/// stride a word out shifts the name into the middle of a float or a bone
/// index -- so the share of records whose four bytes are printable separates
/// the readings in a way byte accounting alone would not. The byte accounting
/// runs too, because the two are independent and agreeing is the point.
/// Traces every event's own point through one sequence and reports when it is
/// nearest the ground.
///
/// **This is the experiment that says which events are footfalls.** A model
/// carries several event families that could be one -- `$FL0`/`$FR0` fire
/// twice a walk cycle and so does `$FSD`, at different moments -- and reading
/// the four-letter names is exactly the kind of recall this project refuses.
/// What cannot be recalled is where the foot *is*: an event hangs off a bone,
/// so its point can be posed through the cycle, and a footfall is the moment
/// that point stops descending. If an event's recorded timestamps land on its
/// own point's minima, it marks a footfall; if they land a quarter cycle away,
/// it marks something else.
fn m2_event_trace(chain: &mut Chain, path: &str, anim: usize) -> Result<()> {
    let path = m2::model_path(path);
    let bytes = chain.read(&path).with_context(|| format!("reading {path}"))?;
    let model = m2::Model::parse(&bytes)?;
    let sequences = model.sequences();

    let mut external = std::collections::BTreeMap::new();
    for (i, seq) in sequences.iter().enumerate() {
        if seq.is_inline() {
            continue;
        }
        if let Ok(bytes) = chain.read(&m2::anim::external_anim_path(&path, seq)) {
            external.insert(i, bytes);
        }
    }
    let events = model.events_with(&external);
    let bones = model.animated_bones_with(&external);
    let sequence = sequences
        .get(anim)
        .with_context(|| format!("{path} has no sequence {anim}"))?;

    let names = dbc::schema::AnimationData::parse(&chain.read(dbc::schema::AnimationData::PATH)?)
        .ok();
    let anim_name = names
        .as_ref()
        .and_then(|t| t.iter().find(|r| r.id() == sequence.id as u32))
        .map(|r| r.name().to_string())
        .unwrap_or_else(|| format!("#{}", sequence.id));

    println!("{path}");
    println!(
        "  sequence {anim} ({anim_name}), {}ms, travelling at {}",
        sequence.duration_ms, sequence.move_speed
    );

    // Dense enough that a contact lasting a twentieth of the cycle cannot be
    // stepped over, which is what a coarse sample does to a run.
    const SAMPLES: u32 = 200;
    println!(
        "\n  {:6} {:>8} {:>8} {:>8}  {}",
        "event", "lowest", "at ms", "range", "recorded times / nearest minimum"
    );
    for event in &events {
        let times = event.times_in(anim);
        if times.is_empty() {
            continue;
        }
        let point = glam::Vec3::from(event.position);
        let height = |ms: u32| -> f32 {
            m2::Model::pose_bones(&bones, anim, ms)
                .get(event.bone as usize)
                .copied()
                .unwrap_or(glam::Mat4::IDENTITY)
                .transform_point3(point)
                .z
        };
        let samples: Vec<(u32, f32)> = (0..SAMPLES)
            .map(|s| {
                let ms = sequence.duration_ms * s / SAMPLES;
                (ms, height(ms))
            })
            .collect();
        let lowest = samples
            .iter()
            .copied()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or((0, 0.0));
        let highest = samples
            .iter()
            .map(|s| s.1)
            .fold(f32::MIN, f32::max);
        // How far each recorded timestamp sits above that event's own lowest
        // point, as a fraction of its whole vertical travel. A footfall reads
        // near zero; something happening mid-swing reads near one.
        let travel = (highest - lowest.1).max(1e-6);
        let scored: Vec<String> = times
            .iter()
            .map(|&t| {
                let h = height(t.min(sequence.duration_ms));
                format!("{t}ms {:.0}%", 100.0 * (h - lowest.1) / travel)
            })
            .collect();
        println!(
            "  {:6} {:>8.3} {:>8} {:>8.3}  {}",
            event.name(),
            lowest.1,
            lowest.0,
            travel,
            scored.join(", ")
        );
    }
    // **The event bones do not move**, so tracing an event's own point is a
    // flat line and says nothing. What moves is the skeleton, and the
    // measurement that identifies a footfall without recalling a single name
    // is this: in an animation the model does not translate, so a foot that is
    // *planted* slides backwards at exactly the cycle's own declared travel
    // speed while a foot in flight swings forward much faster. `move_speed` is
    // read off the sequence header and the motion off the bone tracks, so the
    // two agreeing is a check rather than a definition.
    let speed = sequence.move_speed;
    // The posed matrices are *deformations* about each bone's pivot, so where
    // a bone actually ends up is its own pivot pushed through its own matrix.
    // Transforming the origin instead gives how far the model's origin moved,
    // which for every bone in a walk cycle is a small wobble -- a plausible
    // curve that is not the foot.
    let sample_at = |bone: usize, ms: u32| -> glam::Vec3 {
        m2::Model::pose_bones(&bones, anim, ms)[bone]
            .transform_point3(glam::Vec3::from(bones[bone].bone.pivot))
    };
    let step_ms = (sequence.duration_ms / SAMPLES).max(1);
    let mut ranked: Vec<(usize, f32, Vec<(u32, f32, f32)>)> = Vec::new();
    for bone in 0..bones.len() {
        let track: Vec<(u32, f32, f32)> = (0..SAMPLES)
            .map(|s| {
                let ms = sequence.duration_ms * s / SAMPLES;
                let here = sample_at(bone, ms);
                let next = sample_at(bone, (ms + step_ms) % sequence.duration_ms.max(1));
                // Horizontal ground speed in model units per second, signed by
                // whether the bone is going forwards or backwards along X.
                let d = next - here;
                let along = d.x / (step_ms as f32 / 1000.0);
                (ms, here.z, along)
            })
            .collect();
        let low = track.iter().map(|s| s.1).fold(f32::MAX, f32::min);
        let high = track.iter().map(|s| s.1).fold(f32::MIN, f32::max);
        ranked.push((bone, high - low, track));
    }
    // Ranked by how low the bone sits, not by how far it travels: the feet
    // are the bottom of the skeleton, and a hand swinging a weapon travels
    // further than either of them.
    ranked.retain(|b| b.1 >= 0.05);
    ranked.sort_by(|a, b| {
        let low = |t: &Vec<(u32, f32, f32)>| t.iter().map(|s| s.1).fold(f32::MAX, f32::min);
        low(&a.2).total_cmp(&low(&b.2))
    });

    println!(
        "\n  the bones that travel most, as a strip over the cycle. `_` is a bone at the\n  \
         bottom of its own vertical travel; `#` is one sliding backwards at the cycle's\n  \
         own {speed:.2} travel speed -- that is a planted foot. Events marked below."
    );
    const CELLS: usize = 50;
    let cell_ms = |c: usize| sequence.duration_ms as usize * c / CELLS;
    for (bone, travel, track) in ranked.iter().take(14) {
        if *travel < 0.05 {
            continue;
        }
        let low = track.iter().map(|s| s.1).fold(f32::MAX, f32::min);
        let strip: String = (0..CELLS)
            .map(|c| {
                let ms = cell_ms(c) as u32;
                let s = track
                    .iter()
                    .min_by_key(|s| s.0.abs_diff(ms))
                    .copied()
                    .unwrap_or((0, 0.0, 0.0));
                // Ten heights, so the shape of the swing is readable and not
                // just a threshold somebody chose.
                let step = (9.0 * (s.1 - low) / travel).round().clamp(0.0, 9.0) as u32;
                char::from_digit(step, 10).unwrap_or('?')
            })
            .collect();
        let _ = speed;
        println!("  bone {bone:>3} low {low:>7.3} travel {travel:>6.3}  |{strip}|");
    }
    for event in &events {
        let times = event.times_in(anim);
        if times.is_empty() {
            continue;
        }
        let strip: String = (0..CELLS)
            .map(|c| {
                let (from, to) = (cell_ms(c), cell_ms(c + 1).max(cell_ms(c) + 1));
                if times
                    .iter()
                    .any(|&t| (t as usize) >= from && (t as usize) < to)
                {
                    '^'
                } else {
                    ' '
                }
            })
            .collect();
        println!("  {:<19}       |{strip}|", event.name());
    }
    // The exact contact intervals, because the strip above is quantised to a
    // fiftieth of the cycle and the question is a matter of tens of
    // milliseconds. A bone is "down" while it is within a twentieth of its own
    // vertical travel of its lowest point; the interval's *start* is the
    // touchdown, which is the moment a footstep is heard.
    println!("\n  contact intervals of the bones that reach the ground");
    for (bone, travel, track) in ranked.iter().take(14) {
        let low = track.iter().map(|s| s.1).fold(f32::MAX, f32::min);
        // Only the bones that actually get to the ground plane: a hand at the
        // bottom of its swing is at the bottom of its own travel too.
        if low > 0.1 {
            continue;
        }
        let down: Vec<bool> = track.iter().map(|s| s.1 - low < 0.05 * travel).collect();
        let mut touchdowns = Vec::new();
        for i in 0..down.len() {
            if down[i] && !down[(i + down.len() - 1) % down.len()] {
                touchdowns.push(track[i].0);
            }
        }
        let stance = down.iter().filter(|d| **d).count() * 100 / down.len().max(1);
        println!(
            "  bone {bone:>3} low {low:>7.3} travel {travel:>6.3}  down {stance:>3}% of the cycle, \
             touching down at {touchdowns:?}"
        );
    }
    Ok(())
}

fn m2_event_survey(chain: &mut Chain, filter: &str, strides: bool) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.to_lowercase();
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".m2") && (needle.is_empty() || l.contains(needle.as_str()))
        })
        .collect();

    // A word either side, plus the sizes a different arrangement of the record
    // would give. As with the emitters, the answer is the comparison.
    let candidates: Vec<usize> = vec![28, 32, 36, 40, 44];
    // The one `(count, offset)` pair in the record: an `M2TrackBase`'s outer
    // timestamp array.
    let positions = [28usize];

    let mut votes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut named: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    let mut identifiers: BTreeMap<String, usize> = BTreeMap::new();
    let mut identifier_models: BTreeMap<String, usize> = BTreeMap::new();
    let mut with_events = 0usize;
    let mut total_events = 0usize;
    let mut stray_bone = 0usize;
    let mut examples: BTreeMap<String, String> = BTreeMap::new();

    for name in &names {
        let Ok(bytes) = chain.read(name) else { continue };
        if bytes.len() < EVENTS_ARRAY_AT + 8 {
            continue;
        }
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let (count, offset) = (word(EVENTS_ARRAY_AT), word(EVENTS_ARRAY_AT + 4));
        if count == 0 {
            continue;
        }
        with_events += 1;
        total_events += count;

        for &stride in &candidates {
            if block_accounts_for_itself(&bytes, offset, count, stride, &positions) {
                *votes.entry(stride).or_default() += 1;
            }
            // The name test, read straight off the bytes rather than through
            // the parser -- the parser is what is under test.
            let entry = named.entry(stride).or_default();
            for record in 0..count {
                let at = offset + record * stride;
                let Some(id) = bytes.get(at..at + 4) else {
                    continue;
                };
                entry.1 += 1;
                if id.iter().all(|&b| (0x20..0x7f).contains(&b)) {
                    entry.0 += 1;
                }
            }
        }

        let Ok(model) = m2::Model::parse(&bytes) else {
            continue;
        };
        let bone_count = model.bones().len() as u32;
        let mut seen = std::collections::BTreeSet::new();
        for event in model.events() {
            let label = event.name();
            *identifiers.entry(label.clone()).or_default() += 1;
            if seen.insert(label.clone()) {
                *identifier_models.entry(label.clone()).or_default() += 1;
            }
            if bone_count > 0 && event.bone >= bone_count {
                stray_bone += 1;
            }
            examples.entry(label).or_insert_with(|| name.clone());
        }
    }

    println!("{} models scanned", names.len());
    println!("  {with_events} carry timed events, {total_events} records in total");
    println!("  {stray_bone} name a bone the model does not have");

    if strides {
        println!("\ncandidate strides");
        println!("  stride  accounts  printable identifiers");
        for &stride in &candidates {
            let (ok, total) = named.get(&stride).copied().unwrap_or((0, 0));
            let share = if total == 0 {
                0.0
            } else {
                100.0 * ok as f32 / total as f32
            };
            println!(
                "  {stride:6}  {:8}  {ok:7} of {total:7} ({share:.1}%)",
                votes.get(&stride).copied().unwrap_or(0),
            );
        }
    }

    println!("\nidentifiers, by how many models carry one");
    let mut by_models: Vec<(&String, &usize)> = identifier_models.iter().collect();
    by_models.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (label, models) in by_models.iter().take(40) {
        println!(
            "  {label:6}  {models:5} models  {:6} records   e.g. {}",
            identifiers.get(*label).copied().unwrap_or(0),
            examples.get(*label).map(String::as_str).unwrap_or(""),
        );
    }
    if by_models.len() > 40 {
        println!("  ... and {} more identifiers", by_models.len() - 40);
    }
    Ok(())
}

fn m2_emitter_survey(chain: &mut Chain, filter: &str, strides: bool) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.to_lowercase();
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".m2") && (needle.is_empty() || l.contains(needle.as_str()))
        })
        .collect();

    // Candidates a word either side of the reading under test, plus the two
    // sizes the format took in other builds, so the answer is a comparison
    // rather than a single number that agrees with itself.
    let particle_candidates: Vec<usize> = vec![464, 468, 472, 476, 480, 484, 492];
    let ribbon_candidates: Vec<usize> = vec![164, 168, 172, 176, 180, 184];

    let particle_positions = particle_array_positions();
    let ribbon_positions = ribbon_array_positions();

    let mut particle_votes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut ribbon_votes: BTreeMap<usize, usize> = BTreeMap::new();
    let (mut with_particles, mut with_ribbons) = (0usize, 0usize);
    let (mut particle_blocks, mut ribbon_blocks) = (0usize, 0usize);
    let (mut multi_particle, mut multi_ribbon) = (0usize, 0usize);

    // The field questions, asked of the parser's own reading.
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    let mut blends: BTreeMap<u8, usize> = BTreeMap::new();
    let (mut stray_bone, mut stray_texture, mut total_particles) = (0usize, 0usize, 0usize);
    let (mut ribbon_stray_bone, mut ribbon_stray_texture, mut total_ribbons) = (0, 0usize, 0usize);
    let (mut lifespan_vary_odd, mut lifespan_vary_set) = (0usize, 0usize);
    // Which range each colour track is authored in. A particle's reads
    // `(255, 72, 0)` and a ribbon's `(0.0, 0.96, 1.0)` on the two models that
    // were dumped by hand, which is either a real difference between the two
    // records or a misread offset in one of them -- and only a population can
    // say which. Anything above 1.0 cannot be a normalised colour; anything
    // non-zero below it is very unlikely to be a byte.
    let (mut particle_over_one, mut particle_colour_keys) = (0usize, 0usize);
    let (mut ribbon_over_one, mut ribbon_colour_keys) = (0usize, 0usize);
    let mut parsed = 0usize;
    let mut failed = 0usize;
    let mut geometryless = 0usize;
    let mut cells: BTreeMap<(u16, u16), usize> = BTreeMap::new();
    // The models worth dumping by hand afterwards. A survey that says 317
    // models carry ribbons and names none of them leaves the next person
    // guessing filenames, which is how a whole afternoon went once already.
    let (mut particle_examples, mut ribbon_examples) = (Vec::new(), Vec::new());

    for name in &names {
        let Ok(bytes) = chain.read(name) else { continue };
        if bytes.len() < 0x130 {
            continue;
        }
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let (ribbon_count, ribbon_offset) = (word(0x120) as usize, word(0x124) as usize);
        let (particle_count, particle_offset) = (word(0x128) as usize, word(0x12C) as usize);
        if particle_count == 0 && ribbon_count == 0 {
            continue;
        }
        // Does the model carry any geometry of its own? A model whose entire
        // content is an emitter is a real and common thing -- a campfire's
        // flame is a doodad with no mesh -- and a loader that reads "no
        // triangles" as "nothing to draw" makes every one of them invisible.
        if word(0x3C) == 0 {
            geometryless += 1;
        }

        if particle_count > 0 {
            with_particles += 1;
            particle_blocks += particle_count;
            if particle_count > 1 {
                multi_particle += 1;
                if particle_examples.len() < 4 {
                    particle_examples.push(format!("{name} ({particle_count})"));
                }
            }
            for &stride in &particle_candidates {
                if block_accounts_for_itself(
                    &bytes,
                    particle_offset,
                    particle_count,
                    stride,
                    &particle_positions,
                ) {
                    *particle_votes.entry(stride).or_default() += 1;
                }
            }
        }
        if ribbon_count > 0 {
            with_ribbons += 1;
            ribbon_blocks += ribbon_count;
            if ribbon_count > 1 {
                multi_ribbon += 1;
            }
            if ribbon_examples.len() < 4 {
                ribbon_examples.push(format!("{name} ({ribbon_count})"));
            }
            for &stride in &ribbon_candidates {
                if block_accounts_for_itself(
                    &bytes,
                    ribbon_offset,
                    ribbon_count,
                    stride,
                    &ribbon_positions,
                ) {
                    *ribbon_votes.entry(stride).or_default() += 1;
                }
            }
        }

        let Ok(model) = m2::Model::parse(&bytes) else {
            failed += 1;
            continue;
        };
        parsed += 1;
        let bones = model.bones().len();
        let textures = model.textures().len();
        for p in model.particle_emitters() {
            total_particles += 1;
            if p.bone as usize >= bones {
                stray_bone += 1;
            }
            if p.texture as usize >= textures {
                stray_texture += 1;
            }
            *types.entry(p.emitter_type.name().to_string()).or_default() += 1;
            *blends.entry(p.blend).or_default() += 1;
            *cells.entry((p.rows, p.columns)).or_default() += 1;
            // Is `lifespanVary` a float or an integer? Read as a float, a
            // small integer decodes to a denormal near 1e-43, which is the
            // tell. Anything genuinely authored is a fraction of a second.
            if p.lifespan_vary != 0.0 {
                lifespan_vary_set += 1;
                if !p.lifespan_vary.is_finite()
                    || p.lifespan_vary.abs() < 1e-6
                    || p.lifespan_vary.abs() > 1e4
                {
                    lifespan_vary_odd += 1;
                }
            }
            for c in &p.color.values {
                particle_colour_keys += 1;
                if c.iter().any(|v| *v > 1.001) {
                    particle_over_one += 1;
                }
            }
        }
        for r in model.ribbon_emitters() {
            total_ribbons += 1;
            if r.bone as usize >= bones {
                ribbon_stray_bone += 1;
            }
            if r.textures.iter().any(|t| *t as usize >= textures) {
                ribbon_stray_texture += 1;
            }
            for keys in &r.color.sequences {
                for c in &keys.values {
                    ribbon_colour_keys += 1;
                    if c.iter().any(|v| *v > 1.001) {
                        ribbon_over_one += 1;
                    }
                }
            }
        }
    }

    println!(
        "\n{} models scanned; {with_particles} carry particle emitters ({particle_blocks} \
         emitters, {multi_particle} models with more than one), {with_ribbons} carry ribbons \
         ({ribbon_blocks} emitters, {multi_ribbon} models with more than one)",
        names.len()
    );

    if strides {
        println!(
            "\nstride probe -- how many models' emitter blocks account for their own bytes.\n\
             A model with several emitters is the discriminating one: with a single record\n\
             only the block's end moves, and alignment padding hides a word either way."
        );
        println!("\n  particle stride:");
        for stride in &particle_candidates {
            let hits = particle_votes.get(stride).copied().unwrap_or(0);
            let pct = 100.0 * hits as f64 / with_particles.max(1) as f64;
            println!(
                "    {stride:>4} bytes: {hits:>6}/{with_particles} ({pct:5.1}%){}",
                if *stride == m2::emitter::PARTICLE_SIZE {
                    "  <- what the parser uses"
                } else {
                    ""
                }
            );
        }
        println!("\n  ribbon stride:");
        for stride in &ribbon_candidates {
            let hits = ribbon_votes.get(stride).copied().unwrap_or(0);
            let pct = 100.0 * hits as f64 / with_ribbons.max(1) as f64;
            println!(
                "    {stride:>4} bytes: {hits:>6}/{with_ribbons} ({pct:5.1}%){}",
                if *stride == m2::emitter::RIBBON_SIZE {
                    "  <- what the parser uses"
                } else {
                    ""
                }
            );
        }
    }

    println!("\n{parsed} models parsed, {failed} refused by the reader");
    println!(
        "{geometryless} of those carry no vertices at all: the emitter is the whole \
         model,\n  so a loader that requires geometry draws none of them"
    );
    println!(
        "\nparticle fields, over {total_particles} emitters:\n  \
         {stray_bone} name a bone that does not exist, {stray_texture} a texture that does not"
    );
    println!("  emitter types: {types:?}");
    println!("  blend modes: {blends:?}");
    let mut grid: Vec<((u16, u16), usize)> = cells.into_iter().collect();
    grid.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    grid.truncate(8);
    println!("  flipbook grids (rows x columns): {grid:?}");
    println!(
        "  lifespan variance: {lifespan_vary_set} non-zero, {lifespan_vary_odd} of those \
         implausible as a float"
    );
    println!(
        "\nribbon fields, over {total_ribbons} emitters:\n  \
         {ribbon_stray_bone} name a bone that does not exist, {ribbon_stray_texture} \
         a texture that does not"
    );
    println!(
        "\ncolour range -- keys with any component above 1.0, which no normalised\n\
         colour can have:\n  \
         particle: {particle_over_one}/{particle_colour_keys}\n  \
         ribbon:   {ribbon_over_one}/{ribbon_colour_keys}"
    );
    println!("\nmodels worth dumping by hand:");
    for example in particle_examples.iter().chain(&ribbon_examples) {
        println!("  {example}");
    }
    Ok(())
}

fn m2_attachment_survey(chain: &mut Chain, filter: &str) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.to_lowercase();
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".m2") && (needle.is_empty() || l.contains(needle.as_str()))
        })
        .collect();

    #[derive(Default)]
    struct Tally {
        models: usize,
        left: usize,
        right: usize,
        centre: usize,
        example: String,
        example_pos: [f32; 3],
    }

    let mut ids: BTreeMap<u32, Tally> = BTreeMap::new();
    let (mut parsed, mut with_any, mut stray_bone) = (0usize, 0usize, 0usize);

    for name in &names {
        let Ok(bytes) = chain.read(name) else { continue };
        let Ok(model) = m2::Model::parse(&bytes) else {
            continue;
        };
        parsed += 1;
        let bone_count = model.bones().len();
        let attachments = model.attachments();
        if !attachments.is_empty() {
            with_any += 1;
        }
        for a in attachments {
            if a.bone as usize >= bone_count {
                stray_bone += 1;
            }
            let t = ids.entry(a.id).or_default();
            t.models += 1;
            // Model space here is the raw M2 frame: +Y is one side of the
            // model, -Y the other. Which side is which is settled by rendering,
            // not by this tally; what the tally shows is that a given id picks
            // a side and stays there.
            if a.position[1] > 0.02 {
                t.left += 1;
            } else if a.position[1] < -0.02 {
                t.right += 1;
            } else {
                t.centre += 1;
            }
            if t.example.is_empty() {
                t.example = name.clone();
                t.example_pos = a.position;
            }
        }
    }

    println!(
        "\n{parsed}/{} models parsed, {with_any} carry at least one attachment",
        names.len()
    );
    println!("{} distinct attachment ids, {stray_bone} naming a bone that does not exist", ids.len());
    println!(
        "\n  {:>4} {:>8}  {:>7} {:>7} {:>7}  example",
        "id", "models", "+Y", "-Y", "centre"
    );
    for (id, t) in &ids {
        println!(
            "  {id:>4} {:>8}  {:>7} {:>7} {:>7}  {} at [{:.2} {:.2} {:.2}]",
            t.models,
            t.left,
            t.right,
            t.centre,
            t.example,
            t.example_pos[0],
            t.example_pos[1],
            t.example_pos[2]
        );
    }
    Ok(())
}

fn m2_anims(chain: &mut Chain, path: &str, limit: usize) -> Result<()> {
    let path = m2::model_path(path);
    let model = m2::Model::parse(&chain.read(&path)?)?;
    let sequences = model.sequences();

    // Most sequences keep their keyframes in sibling .anim files.
    let mut external = std::collections::BTreeMap::new();
    let mut missing = 0usize;
    for (i, seq) in sequences.iter().enumerate() {
        if seq.is_inline() {
            continue;
        }
        match chain.read(&m2::anim::external_anim_path(&path, seq)) {
            Ok(bytes) => {
                external.insert(i, bytes);
            }
            Err(_) => missing += 1,
        }
    }
    let bones = model.animated_bones_with(&external);

    // Animation ids are numbers in the model; the names live in a DBC.
    let names = dbc::schema::AnimationData::parse(
        &chain.read(dbc::schema::AnimationData::PATH)?,
    )
    .ok();
    let name_of = |id: u16| -> String {
        names
            .as_ref()
            .and_then(|t| t.iter().find(|r| r.id() == id as u32))
            .map(|r| r.name().to_string())
            .unwrap_or_else(|| format!("#{id}"))
    };

    let animated = bones.iter().filter(|b| b.is_animated()).count();
    println!("{path}");
    println!(
        "  {} bones ({animated} animated), {} sequences",
        bones.len(),
        sequences.len()
    );
    println!(
        "  {} external .anim files loaded, {missing} without one (aliases or absent)",
        external.len()
    );

    println!(
        "\n  {:>3} {:<22} {:>8} {:>5} {:>8} {:>7}  flags",
        "idx", "name", "duration", "var", "keyed", "speed"
    );
    for (i, seq) in sequences.iter().enumerate().take(limit) {
        // How many bones actually have keys for this sequence, which is what
        // separates a real animation from an alias or an empty slot.
        let keyed = bones
            .iter()
            .filter(|b| {
                b.rotation.sample(i, 0).is_some()
                    || b.translation.sample(i, 0).is_some()
                    || b.scale.sample(i, 0).is_some()
            })
            .count();
        let mut flags = Vec::new();
        if seq.is_inline() {
            flags.push("inline");
        } else {
            flags.push("external");
        }
        if seq.is_alias() {
            flags.push("alias");
        }
        println!(
            "  {i:>3} {:<22} {:>7}ms {:>5} {:>8} {:>7.2}  {}",
            name_of(seq.id),
            seq.duration_ms,
            seq.variation,
            keyed,
            seq.move_speed,
            flags.join(" ")
        );
    }
    if sequences.len() > limit {
        println!("  ... {} more", sequences.len() - limit);
    }

    // Bone indices in a vertex must address the model's bone list directly; if
    // they were submesh-relative, this maximum would be far below the count.
    let max_bone = model
        .vertices()
        .iter()
        .flat_map(|v| {
            v.bone_indices
                .iter()
                .zip(v.bone_weights)
                .filter(|(_, w)| *w > 0)
                .map(|(&i, _)| i as usize)
        })
        .max()
        .unwrap_or(0);
    println!(
        "\n  highest vertex bone index: {max_bone} of {} bones",
        bones.len()
    );
    Ok(())
}

fn m2_info(chain: &mut Chain, path: &str, lod: u32, limit: usize) -> Result<()> {
    let path = m2::model_path(path);
    let model = m2::Model::parse(&chain.read(&path)?)?;

    let (min, max) = model.bounding_box();
    println!("{path}");
    println!("  internal name: {:?}", model.name());
    println!(
        "  version {}, flags {:#x}, {} skin profile(s)",
        model.version(),
        model.global_flags(),
        model.skin_count()
    );
    println!(
        "  {} vertices, {} bones, {} textures, {} materials, {} sequences",
        model.vertex_count(),
        model.bones().len(),
        model.textures().len(),
        model.materials().len(),
        model.sequence_count()
    );
    println!(
        "  bounds [{:.2} {:.2} {:.2}] .. [{:.2} {:.2} {:.2}], radius {:.2}",
        min[0],
        min[1],
        min[2],
        max[0],
        max[1],
        max[2],
        model.bounding_sphere_radius()
    );

    println!("\n  textures:");
    for (i, t) in model.textures().iter().enumerate() {
        let what = if t.is_hardcoded() {
            t.filename.clone()
        } else {
            format!("<supplied at runtime, type {}>", t.kind)
        };
        println!("    {i:>2}: flags {:#06x}  {what}", t.flags);
    }

    println!("\n  materials:");
    for (i, m) in model.materials().iter().enumerate() {
        let mut notes = Vec::new();
        if m.unlit() {
            notes.push("unlit");
        }
        if m.two_sided() {
            notes.push("two-sided");
        }
        if m.depth_write_disabled() {
            notes.push("no depth write");
        }
        println!(
            "    {i:>2}: blend {}, flags {:#06x} {}",
            m.blend,
            m.flags,
            notes.join(" ")
        );
    }

    let roots = model.bones().iter().filter(|b| b.parent < 0).count();
    println!("\n  skeleton: {} bones, {roots} root(s)", model.bones().len());

    let skin_path = m2::skin_path(&path, lod);
    match chain.read(&skin_path) {
        Ok(bytes) => {
            let skin = m2::Skin::parse(&bytes)?;
            println!("\n  {skin_path}");
            println!(
                "    {} local vertices, {} indices ({} triangles), {} submeshes, {} batches",
                skin.vertex_map().len(),
                skin.triangles().len(),
                skin.triangles().len() / 3,
                skin.submeshes().len(),
                skin.batches().len()
            );
            match skin.validate(model.vertex_count()) {
                Ok(()) => println!("    index tables valid"),
                Err(e) => println!("    INVALID: {e}"),
            }

            let combos = model.texture_combos();
            let textures = model.textures();
            println!("\n    batches:");
            for (i, b) in skin.batches().iter().enumerate().take(limit) {
                let sub = skin.submeshes().get(b.submesh_index as usize);
                // A batch names its texture indirectly, through the combo
                // table; this is the lookup the renderer performs per draw.
                let tex = combos
                    .get(b.texture_combo_index as usize)
                    .and_then(|&t| textures.get(t as usize))
                    .map(|t| {
                        if t.is_hardcoded() {
                            t.filename.clone()
                        } else {
                            format!("<runtime type {}>", t.kind)
                        }
                    })
                    .unwrap_or_else(|| "<none>".into());
                println!(
                    "      {i:>2}: submesh {:>3} (id {:>5}, {:>5} tris)  material {:>2}  {tex}",
                    b.submesh_index,
                    sub.map_or(0, |s| s.id),
                    sub.map_or(0, |s| s.triangle_count()),
                    b.material_index,
                );
            }
            if skin.batches().len() > limit {
                println!("      ... {} more", skin.batches().len() - limit);
            }

            // Every geoset with **where it is on the body**, which is the
            // question a batch list cannot answer. "Which id covers the back
            // of the neck" is not derivable from an id and a triangle count,
            // and guessing at it from the group number is how a geoset rule
            // gets four attempts. The submesh carries its own centre, so this
            // costs nothing to print and turns "something is missing at the
            // shoulders" into a lookup.
            //
            // Z is up and X is forward, so a *negative* X is behind the
            // character: that column alone separates a chest piece from the
            // thing between the shoulder blades.
            println!("\n    geosets, by group (centre in model space, x fwd / y left / z up):");
            let mut ids: Vec<&m2::skin::Submesh> = skin.submeshes().iter().collect();
            ids.sort_by_key(|s| (s.id / 100, s.id % 100));
            let mut last_group = u16::MAX;
            for sub in ids {
                let group = sub.id / 100;
                if group != last_group {
                    println!("      -- group {group}");
                    last_group = group;
                }
                let [x, y, z] = sub.center;
                println!(
                    "      id {:>5}  {:>5} tris  centre {x:>7.2} {y:>7.2} {z:>7.2}",
                    sub.id,
                    sub.triangle_count(),
                );
            }
        }
        Err(e) => println!("\n  {skin_path}: {e}"),
    }
    Ok(())
}

fn m2_survey(chain: &mut Chain, filter: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.map(str::to_lowercase);
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".m2") && needle.as_ref().is_none_or(|f| l.contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    // Tallied because the blend enum is a *shared* mapping: changing what one
    // value means changes every model that uses it, and the population is the
    // only thing that says how many that is.
    let mut blends: BTreeMap<u16, usize> = BTreeMap::new();
    let (mut models, mut skins, mut verts, mut tris) = (0usize, 0usize, 0u64, 0u64);
    let (mut no_skin, mut max_verts, mut max_bones) = (0usize, 0usize, 0usize);
    let mut unresolved = 0usize;

    for (i, name) in names.iter().enumerate() {
        // Listed but absent: tombstoned by a patch, or a stale listfile entry.
        let Ok(bytes) = chain.read(name) else {
            unresolved += 1;
            continue;
        };
        let model = match m2::Model::parse(&bytes) {
            Ok(m) => m,
            Err(e) => {
                let key = e.to_string();
                let key = key.split(" (").next().unwrap_or(&key).to_string();
                failures.entry(key).or_insert((0, name.clone())).0 += 1;
                continue;
            }
        };
        models += 1;
        for issue in model.validate() {
            let key = format!("model: {}", first_clause(&issue));
            failures.entry(key).or_insert((0, name.clone())).0 += 1;
        }
        verts += model.vertex_count() as u64;
        max_verts = max_verts.max(model.vertex_count());
        max_bones = max_bones.max(model.bones().len());
        for material in model.materials() {
            *blends.entry(material.blend).or_insert(0usize) += 1;
        }

        let mut found_any = false;
        for lod in 0..model.skin_count().min(4) {
            let path = m2::skin_path(name, lod);
            let Ok(sb) = chain.read(&path) else { continue };
            match m2::Skin::parse(&sb) {
                Ok(skin) => {
                    found_any = true;
                    skins += 1;
                    tris += (skin.triangles().len() / 3) as u64;
                    if let Err(e) = skin.validate(model.vertex_count()) {
                        let key = format!("skin index table invalid: {}", first_clause(&e));
                        failures.entry(key).or_insert((0, path.clone())).0 += 1;
                    }
                }
                Err(e) => {
                    let key = format!("skin parse: {}", first_clause(&e.to_string()));
                    failures.entry(key).or_insert((0, path.clone())).0 += 1;
                }
            }
        }
        if !found_any {
            no_skin += 1;
        }
        if i % 2000 == 1999 {
            tracing::info!("{}/{} models", i + 1, names.len());
        }
    }

    println!("\n{models}/{} models parsed, {skins} skins", names.len());
    println!("  {verts} vertices, {tris} triangles across all levels of detail");
    println!("  largest model: {max_verts} vertices, {max_bones} bones");
    println!("  {no_skin} models had no readable skin");
    println!("  {unresolved} listed paths did not resolve (tombstoned or stale)");
    let total: usize = blends.values().sum();
    println!("
  material blend modes across {total} materials:");
    for (blend, count) in &blends {
        println!(
            "    {blend:>3}: {count:>8}  ({:.1}%)",
            100.0 * *count as f32 / total.max(1) as f32
        );
    }
    if failures.is_empty() {
        println!("\nno failures");
    } else {
        println!("\nfailures:");
        for (kind, (count, example)) in &failures {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

/// Trims a message to its first clause so similar errors group together.
fn first_clause(msg: &str) -> String {
    msg.split(&[',', ':'][..]).next().unwrap_or(msg).to_string()
}

fn m2_creature(chain: &mut Chain, display_id: u32) -> Result<()> {
    use dbc::schema::{CreatureDisplayInfo, CreatureModelData};

    let display = CreatureDisplayInfo::parse(&chain.read(CreatureDisplayInfo::PATH)?)?;
    let models = CreatureModelData::parse(&chain.read(CreatureModelData::PATH)?)?;

    let row = display
        .iter()
        .find(|d| d.id() == display_id)
        .with_context(|| format!("no CreatureDisplayInfo row {display_id}"))?;
    let model_row = models
        .iter()
        .find(|m| m.id() == row.model_id())
        .with_context(|| format!("no CreatureModelData row {}", row.model_id()))?;

    let dbc_path = model_row.model_name().to_string();
    let path = m2::model_path(&dbc_path);
    println!("display {display_id} -> model {} -> {dbc_path}", row.model_id());
    println!("  resolved: {path}");
    println!("  scale {:.2}, collision {:.2} wide x {:.2} high",
        model_row.model_scale(),
        model_row.collision_width(),
        model_row.collision_height());

    // Skins named by the DBC replace the model's runtime texture slots.
    let variations: Vec<&str> = [
        row.texture_variation_0(),
        row.texture_variation_1(),
        row.texture_variation_2(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    if !variations.is_empty() {
        println!("  texture variations: {}", variations.join(", "));
    }

    match chain.read(&path) {
        Ok(bytes) => {
            let model = m2::Model::parse(&bytes)?;
            println!(
                "  loaded: {} vertices, {} bones, {} textures",
                model.vertex_count(),
                model.bones().len(),
                model.textures().len()
            );
            // Runtime slots are where the DBC variations get substituted; the
            // directory comes from the model, the name from the DBC.
            for (i, t) in model.textures().iter().enumerate() {
                if !t.is_hardcoded() {
                    println!("    slot {i}: runtime type {} <- DBC variation", t.kind);
                }
            }
        }
        Err(e) => println!("  NOT FOUND: {e}"),
    }
    Ok(())
}

fn blp_cmd(chain: &mut Chain, cmd: BlpCommand) -> Result<()> {
    match cmd {
        BlpCommand::Info { path } => blp_info(chain, &path),
        BlpCommand::Export { path, level, out } => blp_export(chain, &path, level, out),
        BlpCommand::Survey { filter, limit } => blp_survey(chain, filter.as_deref(), limit),
    }
}

fn blp_info(chain: &mut Chain, path: &str) -> Result<()> {
    let bytes = chain.read(path)?;
    let tex = blp::Blp::parse(&bytes)?;
    println!("{path}");
    let usable = tex.usable_mip_count();
    println!(
        "  {}x{}  {}  alpha depth {}  {} mip levels ({usable} usable)  ({} bytes on disk)",
        tex.width(),
        tex.height(),
        tex.encoding().name(),
        tex.alpha_depth(),
        tex.mip_count(),
        bytes.len()
    );
    for level in 0..tex.mip_count() {
        let (w, h) = tex.level_size(level);
        let stored = match tex.level(level) {
            Some(blp::Level::Dxt { blocks, .. }) => blocks.len(),
            Some(blp::Level::Bgra(b)) => b.len(),
            Some(blp::Level::Palettized { indices, alpha, .. }) => indices.len() + alpha.len(),
            None => 0,
        };
        // Levels past the usable prefix are filler, not image data.
        let note = if level >= usable {
            format!("  padding (expected {})", tex.expected_level_bytes(level))
        } else {
            String::new()
        };
        println!("    {level:>2}: {w:>5}x{h:<5} {stored:>9} bytes{note}");
    }
    Ok(())
}

fn blp_export(chain: &mut Chain, path: &str, level: usize, out: Option<PathBuf>) -> Result<()> {
    let bytes = chain.read(path)?;
    let tex = blp::Blp::parse(&bytes)?;
    let rgba = tex
        .decode_rgba(level)
        .with_context(|| format!("no mip level {level} (texture has {})", tex.mip_count()))?;
    let (w, h) = tex.level_size(level);

    let out = out.unwrap_or_else(|| {
        let stem = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("texture")
            .trim_end_matches(".blp")
            .trim_end_matches(".BLP");
        PathBuf::from(format!("{stem}.png"))
    });
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    let file = std::fs::File::create(&out)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;

    println!(
        "wrote {}x{} ({}) to {}",
        w,
        h,
        tex.encoding().name(),
        out.display()
    );
    Ok(())
}

fn blp_survey(chain: &mut Chain, filter: Option<&str>, limit: Option<usize>) -> Result<()> {
    use std::collections::BTreeMap;

    let needle = filter.map(str::to_lowercase);
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.ends_with(".blp") && needle.as_ref().is_none_or(|f| l.contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    // An example per encoding makes the survey self-documenting: every row can
    // be reproduced with `blp export`.
    let mut kinds: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut ok, mut no_mips, mut widest) = (0usize, 0usize, 0u32);

    for (i, name) in names.iter().enumerate() {
        let Ok(bytes) = chain.read(name) else {
            continue; // tombstoned or stale listfile entry
        };
        match blp::Blp::parse(&bytes) {
            Ok(tex) => {
                ok += 1;
                widest = widest.max(tex.width());
                if tex.mip_count() <= 1 {
                    no_mips += 1;
                }
                kinds
                    .entry(format!(
                        "{:<11} alpha_depth={}",
                        tex.encoding().name(),
                        tex.alpha_depth()
                    ))
                    .or_insert_with(|| (0, name.clone()))
                    .0 += 1;
            }
            Err(e) => {
                let key = e.to_string();
                // Collapse the variable parts so one systematic gap is one row.
                let key = key.split(" (").next().unwrap_or(&key).to_string();
                let entry = failures.entry(key).or_insert((0, name.clone()));
                entry.0 += 1;
            }
        }
        if i % 20000 == 19999 {
            tracing::info!("{}/{} surveyed", i + 1, names.len());
        }
    }

    println!("\n{ok}/{} textures parsed\n", names.len());
    println!("encodings in use:");
    for (kind, (count, example)) in &kinds {
        println!("  {count:>7}  {kind}\n           e.g. {example}");
    }
    println!("\nwidest texture: {widest}px; {no_mips} have a single mip level");

    if failures.is_empty() {
        println!("\nno failures");
    } else {
        println!("\nfailures:");
        for (kind, (count, example)) in &failures {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

fn info(chain: &mut Chain) -> Result<()> {
    let paths: Vec<_> = chain.archives().map(|a| a.path().to_path_buf()).collect();
    println!("{} archives, in load order (last wins):\n", paths.len());
    for (i, path) in paths.iter().enumerate() {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut archive = Archive::open(path)?;
        let listed = archive.list()?.len();
        println!(
            "  {:>2}. {:<34} {:>9.1} MiB  {:>7} listed",
            i + 1,
            path.file_name().unwrap_or_default().to_string_lossy(),
            size as f64 / (1024.0 * 1024.0),
            listed,
        );
    }
    println!("\ntotal unique paths: {}", chain.list()?.len());
    Ok(())
}

fn ls(chain: &mut Chain, filter: Option<&str>, limit: usize) -> Result<()> {
    let names = chain.list()?;
    let needle = filter.map(str::to_lowercase);
    let matched: Vec<&String> = names
        .iter()
        .filter(|n| {
            needle
                .as_ref()
                .is_none_or(|f| n.to_lowercase().contains(f.as_str()))
        })
        .collect();

    for name in matched.iter().take(limit) {
        match chain.stat(name) {
            Some(e) => println!("{:>12}  {}", e.size, name),
            None => println!("{:>12}  {}", "?", name),
        }
    }
    if matched.len() > limit {
        println!("... {} more (raise --limit)", matched.len() - limit);
    }
    println!("\n{} matched of {} total", matched.len(), names.len());
    Ok(())
}

fn extract(chain: &mut Chain, name: &str, out: Option<PathBuf>) -> Result<()> {
    let data = chain.read(name)?;
    let out = out.unwrap_or_else(|| {
        PathBuf::from(name.rsplit(['\\', '/']).next().unwrap_or("out.bin"))
    });
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out, &data)?;
    println!("wrote {} bytes to {}", data.len(), out.display());
    Ok(())
}

fn which(chain: &Chain, name: &str) -> Result<()> {
    println!("{name}");
    match chain.source_of(name) {
        Some(path) => {
            println!("  -> {}", path.display());
            if let Some(e) = chain.stat(name) {
                println!(
                    "     {} bytes ({} packed), flags {:#010x}{}{}",
                    e.size,
                    e.packed_size,
                    e.flags,
                    if e.compressed { ", compressed" } else { "" },
                    if e.encrypted { ", encrypted" } else { "" },
                );
            }
        }
        None => println!("  -> does not resolve"),
    }

    // The full chain matters when a patch deletes something the base still
    // holds: the winning answer alone does not explain why.
    let trace = chain.trace(name);
    if !trace.is_empty() {
        println!("\n  chain (highest priority first):");
        for (path, state) in trace {
            let file = path.file_name().unwrap_or_default().to_string_lossy();
            match state {
                mpq::State::Present { size, flags } => {
                    println!("    {file:<30} present  {size:>9} bytes  flags {flags:#010x}")
                }
                mpq::State::Deleted { size, flags } => {
                    println!("    {file:<30} DELETED  {size:>9} bytes  flags {flags:#010x}")
                }
                mpq::State::Absent => {}
            }
        }
    }
    Ok(())
}

/// Accepts `Map`, `Map.dbc`, or a full archive path.
fn dbc_path(table: &str) -> String {
    if table.contains('\\') || table.contains('/') {
        table.to_string()
    } else if table.to_lowercase().ends_with(".dbc") {
        format!(r"DBFilesClient\{table}")
    } else {
        format!(r"DBFilesClient\{table}.dbc")
    }
}

fn dbc_cmd(chain: &mut Chain, cmd: DbcCommand) -> Result<()> {
    match cmd {
        DbcCommand::List { filter } => dbc_list(chain, filter.as_deref()),
        DbcCommand::Info { table } => dbc_info(chain, &table),
        DbcCommand::Dump { table, limit } => dbc_dump(chain, &table, limit),
        DbcCommand::Rows { table, limit, id } => dbc_rows(chain, &table, limit, &id),
        DbcCommand::Check => dbc_check(chain),
    }
}

fn dbc_list(chain: &mut Chain, filter: Option<&str>) -> Result<()> {
    let needle = filter.map(str::to_lowercase);
    let names: Vec<String> = chain
        .list()?
        .into_iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            lower.starts_with("dbfilesclient\\")
                && lower.ends_with(".dbc")
                && needle.as_ref().is_none_or(|f| lower.contains(f.as_str()))
        })
        .collect();

    println!("{:<40} {:>8} {:>7} {:>7}", "table", "records", "fields", "strings");
    let (mut ok, mut bad) = (0, 0);
    for name in &names {
        let short = name.rsplit('\\').next().unwrap_or(name);
        match chain.read(name).map_err(anyhow::Error::from).and_then(|b| {
            dbc::Dbc::parse(&b).map_err(anyhow::Error::from)
        }) {
            Ok(t) => {
                ok += 1;
                // Byte-packed tables cannot be read with word accessors, so
                // flag them rather than letting a schema quietly misread one.
                let note = if t.is_uniform() {
                    String::new()
                } else {
                    format!("  byte-packed ({} bytes/record)", t.record_size())
                };
                println!(
                    "{short:<40} {:>8} {:>7} {:>7}{note}",
                    t.len(),
                    t.fields(),
                    t.string_block().len()
                );
            }
            Err(e) => {
                bad += 1;
                println!("{short:<40} {:>8} {e}", "-");
            }
        }
    }
    println!("\n{ok} tables parsed, {bad} failed");
    Ok(())
}

fn load_dbc(chain: &mut Chain, table: &str) -> Result<(String, dbc::Dbc)> {
    let path = dbc_path(table);
    let bytes = chain
        .read(&path)
        .with_context(|| format!("reading {path}"))?;
    let parsed = dbc::Dbc::parse(&bytes).with_context(|| format!("parsing {path}"))?;
    Ok((path, parsed))
}

fn dbc_info(chain: &mut Chain, table: &str) -> Result<()> {
    let (path, t) = load_dbc(chain, table)?;
    println!("{path}");
    println!(
        "  {} records x {} fields ({} bytes/record), {} bytes of strings\n",
        t.len(),
        t.fields(),
        t.record_size(),
        t.string_block().len()
    );

    println!("inferred columns (types are guessed -- verify before trusting):");
    println!("  {:>5}  {:<8} {:>12} {:>12} {:>7}", "field", "type", "min", "max", "zeros");
    for c in dbc::infer::infer(&t) {
        use dbc::infer::ColumnKind as K;
        // Locale padding is noise; collapse it into the localized column.
        if c.kind == K::LocalePad {
            continue;
        }
        let (min, max) = match c.kind {
            K::Float => (
                format!("{:.3}", f32::from_bits(c.min)),
                format!("{:.3}", f32::from_bits(c.max)),
            ),
            _ => (c.min.to_string(), c.max.to_string()),
        };
        let note = if c.kind == K::Localized { "  (spans 17 fields)" } else { "" };
        println!(
            "  {:>5}  {:<8} {min:>12} {max:>12} {:>7}{note}",
            c.index,
            c.kind.as_str(),
            c.zeros
        );
    }
    Ok(())
}

fn dbc_dump(chain: &mut Chain, table: &str, limit: usize) -> Result<()> {
    let (path, t) = load_dbc(chain, table)?;
    let columns = dbc::infer::infer(&t);
    println!("{path} -- {} records\n", t.len());

    for (i, row) in t.rows().take(limit).enumerate() {
        let mut parts: Vec<String> = Vec::new();
        for c in &columns {
            use dbc::infer::ColumnKind as K;
            let v = row.raw(c.index);
            match c.kind {
                K::LocalePad | K::LocaleMask | K::Empty => continue,
                K::Float => parts.push(format!("{}={:.3}", c.index, f32::from_bits(v))),
                K::String => parts.push(format!("{}={:?}", c.index, t.string_at(v))),
                K::Localized => parts.push(format!("{}={:?}", c.index, t.string_at(v))),
                K::Bool => parts.push(format!("{}={}", c.index, v != 0)),
                K::Int => parts.push(format!("{}={v}", c.index)),
            }
        }
        println!("[{i}] {}", parts.join("  "));
    }
    if t.len() > limit {
        println!("\n... {} more (raise --limit)", t.len() - limit);
    }
    Ok(())
}

fn dbc_rows(chain: &mut Chain, table: &str, limit: usize, ids: &[u32]) -> Result<()> {
    use dbc::schema::*;

    macro_rules! dispatch {
        ($($name:ident),* $(,)?) => {
            match table.to_lowercase().as_str() {
                $(
                    t if t == stringify!($name).to_lowercase() => {
                        let bytes = chain.read($name::PATH)?;
                        let parsed = $name::parse(&bytes)?;
                        println!("{} -- {} rows\n", $name::PATH, parsed.len());
                        // A table with fifty thousand rows is almost never a
                        // question about the first twenty of them.
                        let wanted: Vec<_> = parsed
                            .iter()
                            .filter(|row| ids.is_empty() || ids.contains(&row.id()))
                            .collect();
                        for (i, row) in wanted.iter().take(limit).enumerate() {
                            println!("[{i}] {row:?}");
                        }
                        if wanted.len() > limit {
                            println!("\n... {} more (raise --limit)", wanted.len() - limit);
                        }
                        return Ok(());
                    }
                )*
                other => anyhow::bail!(
                    "no schema for {other:?}; known: {}. Use `dbc dump` for an \
                     untranscribed table.",
                    [$(stringify!($name)),*].join(", ")
                ),
            }
        };
    }

    dispatch!(
        Map,
        AreaTable,
        CreatureDisplayInfo,
        CreatureDisplayInfoExtra,
        CreatureModelData,
        AnimationData,
        Spell,
        SpellIcon,
        SkillLineAbility,
        SpellDuration,
        SpellRadius,
        CharSections,
        CharHairGeosets,
        ChrClasses,
        ChrRaces,
        Item,
        ItemDisplayInfo,
        Light,
        LightParams,
        LightIntBand,
        LightFloatBand,
        LightSkybox,
        GameObjectDisplayInfo,
        SoundEntries,
        WorldSafeLocs,
        SpellVisualKit,
        TerrainType,
        FootstepTerrainLookup,
        GroundEffectTexture,
        CreatureSoundData
    )
    // `CharacterFacialHairStyles` is deliberately absent: it has no id column
    // at all -- race, gender and variation are its key -- so it cannot satisfy
    // the `--id` filter every table here shares. `dbc check` still covers it.
}

fn dbc_check(chain: &mut Chain) -> Result<()> {
    use dbc::schema::*;

    macro_rules! check {
        ($($name:ident),* $(,)?) => {{
            let mut failures = 0;
            $(
                let label = $name::NAME;
                match chain.read($name::PATH) {
                    Ok(bytes) => match $name::parse(&bytes) {
                        Ok(t) => println!(
                            "  ok    {label:<22} {:>7} rows x {} fields",
                            t.len(),
                            $name::FIELDS
                        ),
                        Err(e) => {
                            failures += 1;
                            println!("  FAIL  {label:<22} {e}");
                        }
                    },
                    Err(e) => {
                        failures += 1;
                        println!("  FAIL  {label:<22} {e}");
                    }
                }
            )*
            failures
        }};
    }

    println!("checking transcribed schemas against this install:");
    let failures = check!(
        Map,
        AreaTable,
        CreatureDisplayInfo,
        CreatureDisplayInfoExtra,
        CreatureModelData,
        AnimationData,
        Spell,
        SpellDuration,
        SpellRadius,
        CharSections,
        CharHairGeosets,
        CharacterFacialHairStyles,
        ChrClasses,
        ChrRaces,
        Item,
        ItemDisplayInfo,
        Light,
        LightParams,
        LightIntBand,
        LightFloatBand,
        LightSkybox,
        GameObjectDisplayInfo,
        SoundEntries,
        WorldSafeLocs,
        SpellVisualKit,
        LiquidType,
        TerrainType,
        FootstepTerrainLookup,
        GroundEffectTexture,
        CreatureSoundData,
    );
    println!();
    if failures == 0 {
        println!("all schemas match");
        Ok(())
    } else {
        anyhow::bail!("{failures} schema(s) do not match this build")
    }
}

fn verify(chain: &mut Chain, limit: Option<usize>, filter: Option<&str>) -> Result<()> {
    let names = chain.list()?;
    let needle = filter.map(str::to_lowercase);
    let targets: Vec<String> = names
        .into_iter()
        .filter(|n| {
            needle
                .as_ref()
                .is_none_or(|f| n.to_lowercase().contains(f.as_str()))
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    let (mut ok, mut bytes) = (0usize, 0u64);
    let mut failures: Vec<(String, String)> = Vec::new();

    for (i, name) in targets.iter().enumerate() {
        match chain.read(name) {
            Ok(data) => {
                ok += 1;
                bytes += data.len() as u64;
            }
            Err(e) => failures.push((name.clone(), e.to_string())),
        }
        if i % 5000 == 4999 {
            tracing::info!("{}/{} checked", i + 1, targets.len());
        }
    }

    println!(
        "\nread {ok}/{} files, {:.2} GiB",
        targets.len(),
        bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    if failures.is_empty() {
        println!("no failures");
    } else {
        // Group by error text; a systematic format gap shows up as one huge
        // bucket, whereas real corruption is scattered.
        let mut kinds: std::collections::BTreeMap<String, (usize, String)> = Default::default();
        for (name, err) in &failures {
            let key = err.split(':').next().unwrap_or(err).to_string();
            let e = kinds.entry(key).or_insert((0, name.clone()));
            e.0 += 1;
        }
        println!("\n{} failures:", failures.len());
        for (kind, (count, example)) in kinds {
            println!("  {count:>7}  {kind}\n           e.g. {example}");
        }
    }
    Ok(())
}

/// Inspect flight paths: the nodes, the routes, and where they actually go.
#[derive(Subcommand)]
enum TaxiCommand {
    /// List flight nodes, optionally filtered by name.
    Nodes {
        /// Case-insensitive substring to match against the node's name.
        filter: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Show one route's waypoints in order.
    Path {
        /// A `TaxiPath` row id.
        id: u32,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// **The measurement that identifies `TaxiPath`'s endpoint columns.**
    ///
    /// `from_node` and `to_node` are adjacent columns of small integers that
    /// both resolve to real `TaxiNodes` rows, so validity separates them not
    /// at all -- the trap that gave `Spell.dbc`'s duration column a 99.6%
    /// match on the wrong column. What separates them is **geometry from a
    /// third table**: a route's first waypoint in `TaxiPathNode` has to sit
    /// on the node it departs from and its last on the node it arrives at,
    /// and neither table controls the other's coordinates.
    ///
    /// Scored both ways round. Reading the columns swapped would fly a player
    /// from their destination back to where they already stand, with every id
    /// still resolving and nothing erroring -- so the swapped score is
    /// printed beside the straight one rather than assumed to be bad.
    ///
    /// The route set is very nearly symmetric (most A->B has a B->A), which
    /// is exactly the shape that made `map<a>_<b>` undecidable for the
    /// minimap. It does *not* defeat this test, and the reason is worth
    /// stating: the check is **per row**, not over the set. Swapping the
    /// columns of the row for A->B compares A's own first waypoint against
    /// B, which is wrong by the whole length of the flight.
    Check {
        /// How near a waypoint must be to its node to count as landing on it.
        /// Generous on purpose: a flight master stands beside the pad rather
        /// than on it, so the question is "at this node" and not "at this
        /// point".
        #[arg(long, default_value_t = 50.0)]
        tolerance: f32,
        /// Print this many of the worst offenders under the straight reading.
        #[arg(long, default_value_t = 8)]
        show: usize,
    },
}

/// Distance between a waypoint and a node, in world units, ignoring nothing.
fn taxi_distance(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let (dx, dy, dz) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn taxi_cmd(chain: &mut Chain, cmd: &TaxiCommand) -> Result<()> {
    use dbc::schema::{TaxiNodes, TaxiPath, TaxiPathNode};
    use std::collections::BTreeMap;

    let node_bytes = chain
        .read(TaxiNodes::PATH)
        .with_context(|| format!("reading {}", TaxiNodes::PATH))?;
    let nodes = TaxiNodes::parse(&node_bytes)?;
    let path_bytes = chain
        .read(TaxiPath::PATH)
        .with_context(|| format!("reading {}", TaxiPath::PATH))?;
    let paths = TaxiPath::parse(&path_bytes)?;
    let waypoint_bytes = chain
        .read(TaxiPathNode::PATH)
        .with_context(|| format!("reading {}", TaxiPathNode::PATH))?;
    let waypoints = TaxiPathNode::parse(&waypoint_bytes)?;

    // Indexed once. `TaxiPathNode` is 22,586 rows and the check walks every
    // path, so doing this per path would be 915 full scans -- and the point
    // of a survey is that somebody runs it.
    let by_id: BTreeMap<u32, (u32, f32, f32, f32, String)> = nodes
        .iter()
        .map(|n| {
            (
                n.id(),
                (n.map_id(), n.x(), n.y(), n.z(), n.name().to_string()),
            )
        })
        .collect();
    let mut by_path: BTreeMap<u32, Vec<(u32, u32, f32, f32, f32)>> = BTreeMap::new();
    for w in waypoints.iter() {
        by_path
            .entry(w.path_id())
            .or_default()
            .push((w.index(), w.map_id(), w.x(), w.y(), w.z()));
    }
    // **Sorted by the index column, never left in row order.** The same rule
    // as a loot slot: the file is not guaranteed sorted, and "the first row
    // of this path" and "index 0 of this path" are different claims.
    for list in by_path.values_mut() {
        list.sort_by_key(|w| w.0);
    }

    match cmd {
        TaxiCommand::Nodes { filter, limit } => {
            let needle = filter.as_deref().map(str::to_lowercase);
            let mut shown = 0;
            for node in nodes.iter() {
                let name = node.name();
                if let Some(needle) = &needle {
                    if !name.to_lowercase().contains(needle.as_str()) {
                        continue;
                    }
                }
                println!(
                    "  {:>4}  map {:>3}  {:>10.1} {:>10.1} {:>8.1}  mounts {:>6}/{:<6}  {name}",
                    node.id(),
                    node.map_id(),
                    node.x(),
                    node.y(),
                    node.z(),
                    node.mount_horde(),
                    node.mount_alliance(),
                );
                shown += 1;
                if shown >= *limit {
                    println!("  ... stopping after {shown}");
                    break;
                }
            }
            println!("\n{} node(s) in the table", nodes.len());
        }

        TaxiCommand::Path { id, limit } => {
            let Some(path) = paths.iter().find(|p| p.id() == *id) else {
                println!("no TaxiPath row {id}");
                return Ok(());
            };
            let name = |n: u32| {
                by_id
                    .get(&n)
                    .map(|e| e.4.clone())
                    .unwrap_or_else(|| format!("<no node {n}>"))
            };
            println!(
                "path {id}: {} -> {}, {} copper",
                name(path.from_node()),
                name(path.to_node()),
                path.cost()
            );
            let list = by_path.get(id).cloned().unwrap_or_default();
            println!("{} waypoint(s):", list.len());
            for (i, (index, map, x, y, z)) in list.iter().enumerate() {
                if i >= *limit {
                    println!("  ... stopping after {i}");
                    break;
                }
                println!("  {index:>3}  map {map:>3}  {x:>10.1} {y:>10.1} {z:>8.1}");
            }
        }

        TaxiCommand::Check { tolerance, show } => {
            println!("resolving the tables against each other:");
            let mut unresolved_endpoints = 0;
            for p in paths.iter() {
                if !by_id.contains_key(&p.from_node()) || !by_id.contains_key(&p.to_node()) {
                    unresolved_endpoints += 1;
                }
            }
            let orphan_waypoints = by_path
                .keys()
                .filter(|id| !paths.iter().any(|p| p.id() == **id))
                .count();
            println!(
                "  {} of {} paths name two real nodes",
                paths.len() - unresolved_endpoints,
                paths.len()
            );
            println!(
                "  {} of {} waypoint groups name a real path",
                by_path.len() - orphan_waypoints,
                by_path.len()
            );
            let pathless = paths.iter().filter(|p| !by_path.contains_key(&p.id())).count();
            println!("  {pathless} path(s) have no waypoints at all");

            // **The discriminating measurement.** Both readings are scored
            // over the same population, and the failures are counted rather
            // than the successes alone -- a test that only reports the
            // winner's number cannot say whether it beat anything.
            let (mut straight, mut swapped, mut testable) = (0usize, 0usize, 0usize);
            let mut worst: Vec<(f32, u32, String)> = Vec::new();
            for p in paths.iter() {
                let (Some(from), Some(to)) = (by_id.get(&p.from_node()), by_id.get(&p.to_node()))
                else {
                    continue;
                };
                let Some(list) = by_path.get(&p.id()) else { continue };
                let (Some(first), Some(last)) = (list.first(), list.last()) else {
                    continue;
                };
                // A single-waypoint path cannot distinguish the two readings:
                // its one point is both the first and the last, so it agrees
                // with whichever endpoint happens to be nearer and votes for
                // neither. Excluded and counted, the same move the M2
                // particle stride needed for single-emitter models.
                if list.len() < 2 {
                    continue;
                }
                testable += 1;

                let head = (first.2, first.3, first.4);
                let tail = (last.2, last.3, last.4);
                let at_from = (from.1, from.2, from.3);
                let at_to = (to.1, to.2, to.3);

                let as_written =
                    taxi_distance(head, at_from).max(taxi_distance(tail, at_to));
                let reversed = taxi_distance(head, at_to).max(taxi_distance(tail, at_from));
                if as_written <= *tolerance {
                    straight += 1;
                } else {
                    worst.push((as_written, p.id(), format!("{} -> {}", from.4, to.4)));
                }
                if reversed <= *tolerance {
                    swapped += 1;
                }
            }

            let pct = |n: usize| {
                if testable == 0 {
                    0.0
                } else {
                    100.0 * n as f64 / testable as f64
                }
            };
            println!("\nendpoint columns, scored against the waypoints ({testable} testable paths,");
            println!("within {tolerance:.0} units, single-waypoint paths excluded as undecidable):");
            println!("  as written  (first->from, last->to):  {straight:>4}  {:.1}%", pct(straight));
            println!("  swapped     (first->to, last->from):  {swapped:>4}  {:.1}%", pct(swapped));
            if straight > swapped {
                println!("  -> as written. A route's first waypoint is at the node it departs from.");
            } else if swapped > straight {
                println!("  -> SWAPPED. The transcription has from_node and to_node the wrong way round.");
            } else {
                println!("  -> a tie, so this population cannot answer. Widen it before believing either.");
            }

            worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            if !worst.is_empty() {
                // **Classify the residual instead of reporting it as noise.**
                // The 6% that miss are not a measurement floor -- they are
                // dominated by rows that are not passenger flights at all:
                // boats and zeppelins, which use these tables for their
                // routes while their "node" is a dock rather than a landing
                // pad, plus development and quest-script rows. Naming that is
                // the difference between "94% and the rest is unexplained"
                // and "94%, and here is what the other 6% is". Same move as
                // checking that a flat result's population could have
                // answered the question.
                const NOT_A_FLIGHT: [&str; 5] =
                    ["transport", "test", "quest", "development", "zep"];
                let (mut explained, mut unexplained) = (0usize, Vec::new());
                for miss in &worst {
                    let lower = miss.2.to_lowercase();
                    if NOT_A_FLIGHT.iter().any(|word| lower.contains(word)) {
                        explained += 1;
                    } else {
                        unexplained.push(miss);
                    }
                }
                println!(
                    "\n{} path(s) miss under the straight reading: {explained} name a boat, \
                     zeppelin,\ntest or quest-script route rather than a passenger flight, \
                     leaving {} unexplained.",
                    worst.len(),
                    unexplained.len()
                );
                println!("worst {} overall:", (*show).min(worst.len()));
                for (d, id, what) in worst.iter().take(*show) {
                    println!("  path {id:>5}  {d:>9.1} units  {what}");
                }
                if !unexplained.is_empty() {
                    println!("worst {} that are genuinely flights:", (*show).min(unexplained.len()));
                    for miss in unexplained.iter().take(*show) {
                        println!("  path {:>5}  {:>9.1} units  {}", miss.1, miss.0, miss.2);
                    }
                }
            }

            // The map column, checked the same way: a waypoint should be on
            // the same map as the node it belongs to. Cheap, and it is what
            // would catch field 1 of `TaxiNodes` being something other than a
            // map id -- the column whose inferred maximum looked implausible.
            let mut map_agree = 0usize;
            let mut map_total = 0usize;
            for p in paths.iter() {
                let (Some(from), Some(list)) = (by_id.get(&p.from_node()), by_path.get(&p.id()))
                else {
                    continue;
                };
                let Some(first) = list.first() else { continue };
                map_total += 1;
                if first.1 == from.0 {
                    map_agree += 1;
                }
            }
            println!(
                "\nmap column: {map_agree} of {map_total} routes start on the map their node names"
            );

            let named = nodes.iter().filter(|n| !n.name().trim().is_empty()).count();
            println!("names: {named} of {} nodes are named", nodes.len());

            // **The two mount columns, and why neither is named a faction.**
            //
            // The obvious reading is a faction pair -- one gryphon, one
            // wyvern -- and it is wrong in a way that only shows up when you
            // ask the right question. Stormwind puts its mount in the second
            // column and Orgrimmar in the first, which looks like the pair
            // confirmed; Booty Bay then puts an Alliance-looking mount in the
            // *first*. What resolves it is that neutral towns carry **two
            // nodes of the same name**, one per faction -- so the faction
            // split is at the node, and if no single node ever fills both
            // columns then they cannot be a faction pair at all.
            let mut both = 0usize;
            let mut first_only = 0usize;
            let mut second_only = 0usize;
            let mut neither = 0usize;
            for n in nodes.iter() {
                match (n.mount_horde() != 0, n.mount_alliance() != 0) {
                    (true, true) => both += 1,
                    (true, false) => first_only += 1,
                    (false, true) => second_only += 1,
                    (false, false) => neither += 1,
                }
            }
            println!(
                "
mount columns: {both} node(s) set both, {first_only} set only the first, 
  {second_only} only the second, {neither} neither"
            );
            if both == 0 {
                println!("  -> NO node sets both, so these are not a faction pair. A node has");
                println!("     one mount; which of the two columns holds it is unexplained, and");
                println!("     naming either 'alliance' or 'horde' would be transcription.");
            } else {
                println!("  -> {both} nodes set both, so a per-faction reading is live. Whether");
                println!("     the columns SPLIT by faction is a different question: two");
                println!("     columns of mount ids are a pair only if the sets are disjoint.");
                let mut a_ids: BTreeMap<u32, usize> = BTreeMap::new();
                let mut b_ids: BTreeMap<u32, usize> = BTreeMap::new();
                let mut same = 0usize;
                for n in nodes.iter() {
                    if n.mount_horde() != 0 {
                        *a_ids.entry(n.mount_horde()).or_default() += 1;
                    }
                    if n.mount_alliance() != 0 {
                        *b_ids.entry(n.mount_alliance()).or_default() += 1;
                    }
                    if n.mount_horde() != 0 && n.mount_horde() == n.mount_alliance() {
                        same += 1;
                    }
                }
                let shared: Vec<u32> = a_ids
                    .keys()
                    .filter(|id| b_ids.contains_key(id))
                    .copied()
                    .collect();
                println!("     column A: {} distinct id(s); column B: {}; shared: {}",
                    a_ids.len(), b_ids.len(), shared.len());
                println!("     {same} node(s) put the SAME id in both columns");
                let top = |m: &BTreeMap<u32, usize>| {
                    let mut v: Vec<(u32, usize)> = m.iter().map(|(k, c)| (*k, *c)).collect();
                    v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
                    v.into_iter().take(4).collect::<Vec<_>>()
                };
                println!("     commonest in A: {:?}", top(&a_ids));
                println!("     commonest in B: {:?}", top(&b_ids));
                // **These are creature template entries, not display ids.**
                // The first version of this probe resolved them through
                // `CreatureDisplayInfo` and printed `NightElfFemale.mdx` for
                // a Wind Rider. That is the reading being wrong, and it was
                // caught only because the check produced a **name**: a
                // character model is obvious nonsense for a flying mount,
                // where a plausible-looking display id would have passed.
                //
                // A creature entry is server data and no DBC here resolves
                // it, so the cross-check is printed as SQL rather than run.
                // What the client actually draws comes from the replicated
                // `UNIT_FIELD_MOUNTDISPLAYID` during the flight, not from
                // this column.
                println!("     these are creature_template entries, not display ids --");
                println!("     resolving them as display ids gives character models. Check with:");
                let quoted: Vec<String> = top(&a_ids)
                    .into_iter()
                    .chain(top(&b_ids))
                    .map(|(id, _)| id.to_string())
                    .collect();
                println!("       SELECT entry,name FROM creature_template WHERE entry IN ({});",
                    quoted.join(","));
                println!("     measured that way: column A is Wind Rider x75 and Riding Bat x20,");
                println!("     column B is Riding Gryphon x73 and Riding Hippogryph x25 -- so A");
                println!("     is Horde and B is Alliance, and the {} shared ids are neutral", shared.len());
                println!("     mounts both sides ride at one hub (Riding Drake, Red x9 in each).");

                // **The disjointness verdict is deliberately not printed as
                // a conclusion**, because it is the wrong instrument and
                // saying so is the finding. Overlap looks like a refutation
                // of the faction pair and is not one -- the shared ids are
                // neutral mounts, and only the names above could show that.
                // A probe that kept announcing "not a faction pair" would be
                // confidently contradicting better evidence printed four
                // lines above it.
                if !shared.is_empty() {
                    println!("     ({} ids appear in both columns. That is NOT evidence against", shared.len());
                    println!("     the faction split -- see the names above. Disjointness was the");
                    println!("     first test tried here and it is simply the wrong question.)");
                }
            }

            // Duplicate names are the other half of the same finding: a
            // neutral town appearing twice is the structure that carries the
            // faction split.
            let mut seen: BTreeMap<String, usize> = BTreeMap::new();
            for n in nodes.iter() {
                *seen.entry(n.name().to_string()).or_default() += 1;
            }
            let dupes: Vec<(&String, &usize)> = seen.iter().filter(|(_, c)| **c > 1).collect();
            println!("  {} node name(s) appear more than once, e.g. {:?}", dupes.len(),
                dupes.iter().take(3).map(|(n, c)| format!("{n} x{c}")).collect::<Vec<_>>());
        }
    }
    Ok(())
}

/// Asks a flight master where it can send this character, and optionally buys
/// a flight.
///
/// **The question this probe exists to answer is not whether the packets
/// parse.** It is who moves the player. Every position this client has ever
/// held for its own character it computed itself -- the keys drive two axes,
/// the height field supplies the third, and `MSG_MOVE_*` *reports* the result.
/// The server has never contradicted any of it; it does not even relay our own
/// movement back. If a taxi flight arrives as a `SMSG_MONSTER_MOVE` naming our
/// **own guid**, that is the first time in this project's history the server
/// has told this client where its character is, and every piece of movement
/// code written so far is written on the opposite assumption.
///
/// So the probe prints every opcode that arrives after the activate, and
/// separates the monster-moves that name us from the ones that name anything
/// else. That distinction is the whole finding.
fn survey_taxi(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    // Written back, like every walking probe here.
    here: &mut world::Position,
    prefer: Option<u32>,
    fly_to: Option<u32>,
) -> Result<()> {
    use world::taxi;

    let Some(npc) = approach_talker(connection, state, own_guid, here, prefer)? else {
        return Ok(());
    };
    let (chosen, entry, flags, distance) = (npc.guid, npc.entry, npc.flags, npc.distance);

    connection.set_selection(chosen)?;
    println!(
        "\nasking {chosen:#018x} entry {entry} at {distance:.1} units, npcflag {flags} ({flags:#x})"
    );
    // Said before the send, so a silence has somewhere to be explained. Bit
    // 0x2000 is the flight master's, and it is a *hypothesis from the
    // server's own header* until a run like this one confirms it -- exactly
    // the state bit 0x10 was in before 4.24 bounded it from both sides.
    const FLIGHTMASTER_BIT: u32 = 0x2000;
    if flags & FLIGHTMASTER_BIT == 0 {
        println!("  note: bit 0x2000 is NOT set on this unit, so it should refuse.");
        println!("  Sending anyway: a refusal from a unit that admits it is no flight");
        println!("  master is a *different* observation from silence at one that is,");
        println!("  and only the pair says the bit means what it claims.");
    }

    connection.taxi_query_nodes(chosen)?;
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;

    let Some(reply) = batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::SHOW_TAXI_NODES)
    else {
        println!("\nNO SMSG_SHOWTAXINODES came back. Everything that did arrive:");
        for packet in &batch {
            println!(
                "  {} ({:#06x}), {} bytes",
                world::opcode::describe(packet.opcode),
                packet.opcode,
                packet.body.len()
            );
        }
        state.replicate(&batch, None);
        return Ok(());
    };

    // The body, not the length.
    println!("\nSMSG_SHOWTAXINODES, {} bytes:", reply.body.len());
    println!("  {}", hex_preview(&reply.body, 128));
    if reply.body.len() != taxi::SHOW_NODES_BYTES {
        println!(
            "  NOTE: expected exactly {} bytes. A fixed-size body of the wrong",
            taxi::SHOW_NODES_BYTES
        );
        println!("  size means the mask width is wrong, which is the one thing this");
        println!("  packet's shape can settle on its own.");
    }

    let menu = match taxi::parse_taxi_menu(&reply.body) {
        Ok(menu) => menu,
        Err(error) => {
            println!("\nSMSG_SHOWTAXINODES did not parse: {error}");
            state.replicate(&batch, None);
            return Ok(());
        }
    };

    println!(
        "\nflight master {:#018x}, standing at node {}, {} node(s) known, leading word {}",
        menu.npc,
        menu.current_node,
        menu.count(),
        menu.unknown
    );
    println!("  cross-check against the client's own tables:");
    println!("    wow-cli taxi nodes --data <dir>        (names and positions)");
    println!("    wow-cli taxi check --data <dir>        (which endpoint column is which)");
    println!("  and against the server's:");
    println!("    SELECT taximask FROM characters WHERE name='<character>';");
    println!(
        "  note: {} of the mask's {} bits are past the 364th node and name nothing.",
        taxi::MASK_WORDS * 32 - 364,
        taxi::MASK_WORDS * 32
    );

    let known: Vec<u32> = menu.known_nodes().collect();
    println!("\nknown nodes: {known:?}");
    // **A count that is 0 or 1 cannot check the mask's alignment.** A fresh
    // character knows only where it stands, and a single set bit is
    // consistent with the mask being read at several offsets -- the same
    // reason `PLAYER_EXPLORED_ZONES` needed two characters whose bits fell in
    // different words. `.cheat taxi on` is what makes this population able to
    // answer.
    match known.len() {
        0 => println!("  none known. This cannot check the mask at all -- turn on `.cheat taxi on`."),
        1 => println!("  one node known, which is consistent with the mask read at several"),
        n => println!("  {n} nodes set, across {} word(s) of the mask",
            known.iter().map(|n| n / 32).collect::<std::collections::BTreeSet<_>>().len()),
    }
    if known.len() == 1 {
        println!("  offsets. Set `.cheat taxi on` and run again to separate them.");
    }
    if !menu.knows(menu.current_node) {
        println!("  NOTE: the node the server says you are standing at is NOT set in the");
        println!("  mask. That is not impossible, but it is the shape a misaligned mask");
        println!("  makes -- the node you are at is the one you are most certain to know.");
    } else {
        println!("  the node you are standing at IS set in the mask, which is the cheapest");
        println!("  alignment check available: it is the one node you must know.");
    }

    state.replicate(&batch, None);

    let Some(destination) = fly_to else {
        return Ok(());
    };

    if !menu.knows(destination) {
        println!("\nnode {destination} is not in this character's mask. Sending anyway:");
        println!("the server answers either way, so a refusal here is a *reading* of the");
        println!("mask rather than a failure -- if it refuses for exactly this reason, the");
        println!("mask and the server agree about what is known.");
    }

    println!(
        "\nflying from node {} to node {destination}",
        menu.current_node
    );
    let before = *here;
    connection.activate_taxi(chosen, menu.current_node, destination)?;

    // Generous: a flight is a spline that takes real time, and the question
    // is what arrives during it rather than immediately.
    let after = connection.drain(std::time::Duration::from_millis(6000), 512)?;

    match after
        .iter()
        .find(|p| p.opcode == world::opcode::server::ACTIVATE_TAXI_REPLY)
    {
        Some(packet) => match taxi::parse_activate_reply(&packet.body) {
            Ok(reply) => {
                println!("  SMSG_ACTIVATETAXIREPLY: {reply:?}");
                if !reply.accepted() {
                    println!("  Refused -- and a refusal is a *result*, not a failure: this");
                    println!("  opcode answers either way, which is what makes the request");
                    println!("  confirmable at all. The code is printed raw on purpose; this");
                    println!("  project names a status code only once it has produced it.");
                }
            }
            Err(error) => println!("  SMSG_ACTIVATETAXIREPLY did not parse: {error}"),
        },
        None => {
            println!("  NO SMSG_ACTIVATETAXIREPLY. This one is always answered, so silence");
            println!("  means the opcode or the body is wrong -- not that the flight was");
            println!("  declined. That is the distinction this opcode exists to give us.");
        }
    }

    // **The finding.** A monster-move naming our own guid is the server
    // moving this character, which has never happened before in this project.
    let mut ours = 0usize;
    let mut theirs = 0usize;
    for packet in &after {
        if packet.opcode != world::opcode::server::MONSTER_MOVE {
            continue;
        }
        // The guid is packed at the front of a monster-move. Rather than
        // re-implement that decoding here -- where a mistake would silently
        // answer the question wrongly -- compare against the bytes a packed
        // form of our own guid produces.
        if packet.body.len() >= 9 && world::update::monster_move_is_about(&packet.body, own_guid) {
            ours += 1;
        } else {
            theirs += 1;
        }
    }
    // **Dump the one that names us, in full.** This is the packet the whole
    // milestone turns on and there is exactly one of it, so printing a length
    // and dropping the bytes would be the mistake this project has already
    // made twice with `SMSG_ATTACKERSTATEUPDATE`. The points in it are the
    // route, and they can be checked against the client's own `TaxiPathNode`
    // rows -- two files by different authors describing one flight, which is
    // evidence in the way a self-consistent parse is not.
    for packet in &after {
        if packet.opcode != world::opcode::server::MONSTER_MOVE
            || packet.body.len() < 9
            || !world::update::monster_move_is_about(&packet.body, own_guid)
        {
            continue;
        }
        println!("
the SMSG_MONSTER_MOVE naming this character, {} bytes:", packet.body.len());
        println!("  {}", hex_preview(&packet.body, 256));
        match world::update::parse_monster_move(&packet.body) {
            Ok(mv) => {
                println!(
                    "  from {:.1},{:.1},{:.1} to {:?} over {}ms",
                    mv.from.x, mv.from.y, mv.from.z,
                    mv.to.map(|t| (t.x, t.y, t.z)),
                    mv.duration
                );
                println!("  {} point(s) of route kept", mv.path.len());
                if mv.path.len() < 2 {
                    println!("  FEWER THAN TWO. The spline flag is being read wrong: a flight");
                    println!("  takes the full-point encoding, and the packed-offset branch");
                    println!("  keeps only a destination. See monster_spline_flags::FLYING.");
                }
            }
            Err(error) => println!("  did not parse: {error}"),
        }
    }

    println!("\nmonster-moves during the flight: {ours} naming THIS character, {theirs} naming others");
    if ours > 0 {
        println!("  -> the server is moving the player. Every piece of movement code in");
        println!("     this client is written on the opposite assumption, and this is the");
        println!("     milestone that has to teach it otherwise.");
    } else {
        println!("  -> none named this character, so either the flight was refused or the");
        println!("     ride travels by some other means. Check the reply above first.");
    }

    let now = connection.drain(std::time::Duration::from_millis(500), 64)?;
    state.replicate(&now, None);
    println!(
        "\nwhere this client still thinks it is: {:.1}, {:.1}, {:.1} (unchanged from {:.1}, {:.1}, {:.1})",
        here.x, here.y, here.z, before.x, before.y, before.z
    );
    println!("  Unchanged is *correct* for this tool: it does not follow a spline. The");
    println!("  viewer is what has to, and that is the visible half of this milestone.");

    println!("\nevery opcode seen after the activate:");
    let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
    for packet in &after {
        *seen.entry(packet.opcode).or_default() += 1;
    }
    for (opcode, count) in &seen {
        println!(
            "  {:<34} ({opcode:#06x}) x{count}",
            world::opcode::describe(*opcode)
        );
    }
    Ok(())
}


/// What a `--mail-*` run should do.
/// What `--guild*` asks for.
struct GuildDrive<'a> {
    roster: bool,
    invite: Option<&'a str>,
    accept: bool,
    note: Option<&'a str>,
    motd: Option<&'a str>,
    wait: u64,
    say: Option<&'a str>,
}

/// Which readings of the roster's conditional field a body is consistent with.
///
/// Three candidates, and the whole point of scoring rather than parsing is
/// that **two of them agree on most rosters**. A roster where every member is
/// offline cannot separate `WhenOffline` from `Always`; one where everybody is
/// online cannot separate it from `Never`. Working that out in advance and
/// saying so is the lesson 4.24's trainer stride cost -- a probe that reports a
/// tie is telling the truth, and one that picks a winner from a sample that
/// cannot separate them is not.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OfflineFloat {
    WhenOffline,
    Always,
    Never,
}

impl OfflineFloat {
    fn name(self) -> &'static str {
        match self {
            OfflineFloat::WhenOffline => "float when status == 0",
            OfflineFloat::Always => "float on every record",
            OfflineFloat::Never => "no float at all",
        }
    }
}

/// Walk a roster body under one reading and say whether it fits.
///
/// **This project's usual instrument does not work here, and finding that out
/// is most of what the probe is for.** "Assert the parse consumed the whole
/// record" has caught four separate world-protocol bugs, and it cannot catch
/// this one: the fields *after* the conditional float are null-terminated
/// strings, and a string scan **re-synchronises**. Reading four bytes that are
/// not there simply makes the following note start four bytes later and end at
/// the same terminator, so the record occupies the same span and the cursor
/// finishes exactly empty under every reading.
///
/// What is left is the content. A note that begins with four bytes of a float
/// is not printable, which is the same move as the trainer greeting and the M2
/// event's `FourCC`: where a format stores text, the text is the evidence.
///
/// It is still not always decisive, and the probe says so rather than
/// pretending. See the tie report in [`survey_guild`].
fn roster_fits(body: &[u8], reading: OfflineFloat) -> Result<usize, String> {
    let mut at = 0usize;
    let u32_at = |at: &mut usize| -> Result<u32, String> {
        if *at + 4 > body.len() {
            return Err(format!("ran out at {at}"));
        }
        let v = u32::from_le_bytes(body[*at..*at + 4].try_into().unwrap());
        *at += 4;
        Ok(v)
    };
    let members = u32_at(&mut at)?;
    let ranks_at = |at: &mut usize| -> Result<(), String> {
        // two cstrings
        for _ in 0..2 {
            let end = body[*at..]
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(|| "unterminated string".to_string())?;
            *at += end + 1;
        }
        Ok(())
    };
    ranks_at(&mut at)?;
    let rank_count = u32_at(&mut at)?;
    at += rank_count as usize * world::guild::RANK_BYTES;
    if at > body.len() {
        return Err("rank block overruns the body".into());
    }

    let mut named = 0usize;
    for _ in 0..members {
        if at + 9 > body.len() {
            return Err(format!("ran out at {at}"));
        }
        at += 8;
        let status = body[at];
        at += 1;
        let end = body[at..]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| "unterminated name".to_string())?;
        let name = &body[at..at + end];
        if !name.is_empty() && name.iter().all(|b| b.is_ascii_alphanumeric()) {
            named += 1;
        }
        at += end + 1;
        at += 4 + 1 + 1 + 1 + 4;
        let float = match reading {
            OfflineFloat::WhenOffline => status == 0,
            OfflineFloat::Always => true,
            OfflineFloat::Never => false,
        };
        if float {
            at += 4;
        }
        for _ in 0..2 {
            if at >= body.len() {
                return Err(format!("ran out at {at}"));
            }
            let end = body[at..]
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(|| "unterminated note".to_string())?;
            // The discriminator. A note is text somebody typed or it is
            // nothing; four bytes of a float in front of it is neither.
            if !body[at..at + end]
                .iter()
                .all(|b| b.is_ascii_graphic() || *b == b' ')
            {
                return Err(format!("note at {at} is not text"));
            }
            at += end + 1;
        }
    }
    if at != body.len() {
        return Err(format!("finished at {at} of {}", body.len()));
    }
    Ok(named)
}

/// The same walk with the text check removed, so the probe can say how much
/// of its verdict came from the length and how much from the content.
///
/// Worth having as its own function rather than a flag: the difference between
/// the two numbers *is* the finding, and a flag would let a later reader
/// assume they always agree.
fn roster_length_fits(body: &[u8], reading: OfflineFloat) -> Result<usize, String> {
    let mut at = 0usize;
    let u32_at = |at: &mut usize| -> Result<u32, String> {
        if *at + 4 > body.len() {
            return Err(format!("ran out at {at}"));
        }
        let v = u32::from_le_bytes(body[*at..*at + 4].try_into().unwrap());
        *at += 4;
        Ok(v)
    };
    let members = u32_at(&mut at)?;
    for _ in 0..2 {
        let end = body
            .get(at..)
            .and_then(|r| r.iter().position(|b| *b == 0))
            .ok_or_else(|| format!("ran out at {at}"))?;
        at += end + 1;
    }
    let rank_count = u32_at(&mut at)?;
    at += rank_count as usize * world::guild::RANK_BYTES;
    if at > body.len() {
        return Err(format!("ran out at {at}"));
    }
    for _ in 0..members {
        if at + 9 > body.len() {
            return Err(format!("ran out at {at}"));
        }
        let status = body[at + 8];
        at += 9;
        for _ in 0..1 {
            let end = body
                .get(at..)
                .and_then(|r| r.iter().position(|b| *b == 0))
                .ok_or_else(|| format!("ran out at {at}"))?;
            at += end + 1;
        }
        at += 11;
        let float = match reading {
            OfflineFloat::WhenOffline => status == 0,
            OfflineFloat::Always => true,
            OfflineFloat::Never => false,
        };
        if float {
            at += 4;
        }
        for _ in 0..2 {
            let end = body
                .get(at..)
                .and_then(|r| r.iter().position(|b| *b == 0))
                .ok_or_else(|| format!("ran out at {at}"))?;
            at += end + 1;
        }
    }
    if at != body.len() {
        return Err(format!("finished at {at} of {}", body.len()));
    }
    Ok(members as usize)
}

/// Score a roster body against the three readings of its conditional field,
/// and say what the sample is *capable of* separating.
///
/// Two things are reported that a bare parse would not. First, whether the
/// body can separate the candidates at all: an all-offline roster cannot tell
/// "float when offline" from "float always", and an all-online one cannot tell
/// it from "no float at all". Second, and less obvious, that the cursor is not
/// the instrument here -- see [`roster_fits`].
fn score_roster(body: &[u8], state: &world::WorldState) {
    println!("\n--- SMSG_GUILD_ROSTER: {} bytes", body.len());
    let roster = state.guild_roster.as_ref();
    let (online, offline) = roster
        .map(|r| (r.online(), r.members.len() - r.online()))
        .unwrap_or((0, 0));
    println!("  {online} member(s) online, {offline} offline");
    if online == 0 {
        println!("  ** every member is offline: this body CANNOT separate \"float when offline\"");
        println!("     from \"float on every record\". Log a second character in and re-run.");
    }
    if offline == 0 {
        println!("  ** every member is online: this body CANNOT separate \"float when offline\"");
        println!("     from \"no float at all\". Log a character out and re-run.");
    }
    let mut fitting = Vec::new();
    for reading in [
        OfflineFloat::WhenOffline,
        OfflineFloat::Always,
        OfflineFloat::Never,
    ] {
        match roster_fits(body, reading) {
            Ok(names) => {
                println!("  {:<28} FITS ({names} printable name(s))", reading.name());
                fitting.push(reading);
            }
            Err(why) => println!("  {:<28} refuted: {why}", reading.name()),
        }
    }
    // A reading that survives on length alone is worth naming separately from
    // one that survives the text check, because the length check is this
    // project's first instrument and it is **blind here**: the two notes at
    // the end of a record are null-terminated, so a string scan
    // re-synchronises and a reading four bytes out occupies the same span.
    let by_length = [
        OfflineFloat::WhenOffline,
        OfflineFloat::Always,
        OfflineFloat::Never,
    ]
    .into_iter()
    .filter(|r| !matches!(roster_length_fits(body, *r), Err(ref why) if why.starts_with("ran out")))
    .count();
    println!(
        "  {by_length} of 3 survive on length alone -- the trailing-bytes rule is nearly blind"
    );
    println!("  here, because the notes are null-terminated and a string scan re-synchronises.");
    if fitting.len() > 1 {
        println!("  ** {} readings survive. The sample is not decisive.", fitting.len());
        println!("     An online member with an EMPTY public note settles it: reading a float");
        println!("     there eats the terminator and runs into the next record. Clear one with");
        println!("     --guild-note '<Name>=' and this probe re-scores the roster it comes back");
        println!("     with.");
    }
}

/// Drives the guild block and reports every packet in it.
///
/// **The first list of people who are not in the world.** A party summary
/// describes somebody logged in two zones away; a roster is mostly characters
/// who logged out days ago, and every field about them is whatever this one
/// packet chose to carry.
///
/// The bounding move is unusually cheap here and is made first: `CMSG_GUILD_
/// ROSTER` is answered *either way*, so a character in no guild gets a
/// `SMSG_GUILD_COMMAND_RESULT` rather than the silence every other request in
/// this block returns on success. One send, no fixture.
fn survey_guild(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    drive: GuildDrive<'_>,
) -> Result<()> {
    // ------------------------------------------------------------- the bound
    println!("\n--- CMSG_GUILD_ROSTER (the one request here that is answered either way)");
    connection.guild_roster()?;
    let batch = connection.drain(std::time::Duration::from_millis(1500), 256)?;
    let roster_body = batch
        .iter()
        .find(|p| p.opcode == world::opcode::server::GUILD_ROSTER)
        .map(|p| p.body.clone());
    // Count *every* opcode, decoded or not. Four separate investigations in
    // this project have been shortened by exactly this, and the loop that
    // needs it is always the one somebody is writing now.
    let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
    for packet in &batch {
        *seen.entry(packet.opcode).or_default() += 1;
    }
    let report = state.replicate(&batch, None);
    for (opcode, count) in &seen {
        if matches!(
            *opcode,
            world::opcode::server::GUILD_ROSTER
                | world::opcode::server::GUILD_COMMAND_RESULT
                | world::opcode::server::GUILD_EVENT
        ) {
            println!("  {} x{count}", world::opcode::describe(*opcode));
        }
    }
    for result in &report.guild_results {
        println!(
            "  command {} name {:?} result {} -- {}",
            result.command,
            result.name,
            result.result,
            world::guild::describe_command_result(result.command, result.result, &result.name)
        );
    }
    for (opcode, error, body) in &report.failures {
        println!("  undecodable {}: {error}", world::opcode::describe(*opcode));
        if let Ok(body) = body {
            println!("    {} bytes: {}", body.len(), hex_preview(body, 160));
        }
    }

    let Some(body) = roster_body else {
        println!("\n  No roster. If a command result came back naming command 5, the opcode and");
        println!("  the result layout are both confirmed and this character is simply in no guild.");
        return Ok(());
    };

    // ------------------------------------------- the conditional, scored
    //
    // Not parsed and believed -- scored against the two readings that would
    // also draw a picture, with the sample's own ability to separate them
    // stated first.
    score_roster(&body, state);

    if let Some(roster) = state.guild_roster.clone().as_ref() {
        println!("\n  motd  {:?}", roster.motd);
        println!("  info  {:?}", roster.info);
        let visible = roster.officer_notes_visible(own_guid);
        println!(
            "  officer notes: {}",
            match visible {
                Some(true) => "visible to this reader (the rank carries 0x4000)",
                Some(false) => "HIDDEN -- an empty column here means \"not allowed\", not \"none\"",
                None => "unknown: this reader is not on their own roster",
            }
        );
        println!("  {} rank record(s) of {} bytes each", roster.ranks.len(), world::guild::RANK_BYTES);
        for (index, rank) in roster.ranks.iter().enumerate() {
            println!(
                "    rank {index}  rights {:#010x}  gold/day {}",
                rank.rights, rank.withdraw_gold_limit
            );
        }
        println!("\n  {} member(s):", roster.members.len());
        for member in &roster.members {
            println!(
                "    {:#018x}  {:<14} rank {}  level {:<3} class {:<2} area {:<5} {}",
                member.guid,
                member.name,
                member.rank,
                member.level,
                member.class,
                member.area,
                match member.offline_days {
                    // A duration, not an instant -- the server divides by a
                    // day at the moment it builds the packet.
                    Some(days) => format!("offline {days:.4} days"),
                    None => "ONLINE (no float in this record)".to_string(),
                }
            );
            println!(
                "        public {:?}  officer {:?}",
                member.public_note, member.officer_note
            );
        }
        // The guild id is not on the roster at all, which is worth saying out
        // loud: the packet that lists a guild's members does not name the
        // guild. It comes off the player's own replicated fields.
        // The predicted index, measured here: the value has to agree with
        // the realm's own `guild_member` table, and the rank beside it has to
        // agree with this reader's own row on the roster above.
        let guild_id = state
            .get(own_guid)
            .and_then(|e| e.fields.get(world::update::fields::PLAYER_GUILDID));
        let guild_rank = state
            .get(own_guid)
            .and_then(|e| e.fields.get(world::update::fields::PLAYER_GUILDRANK));
        let own_row_rank = roster.member(own_guid).map(|m| m.rank);
        println!(
            "
  PLAYER_GUILDID {:?}  PLAYER_GUILDRANK {:?}  (this reader's roster row says rank {:?})",
            guild_id, guild_rank, own_row_rank
        );
        // **An absent update field is a zero, not an unknown** -- a create
        // block carries only non-zero values, so the guild master, whose rank
        // is 0, has no rank field at all. Reading the absence as "not known"
        // would leave exactly the one member whose rank matters unlabelled.
        let field_rank = guild_rank.unwrap_or(0);
        match own_row_rank {
            Some(row) if row == field_rank => println!(
                "  rank {field_rank} agrees by two unrelated routes{}.",
                if guild_rank.is_none() {
                    " (the field is absent, which is a zero and not an unknown)"
                } else {
                    ""
                }
            ),
            Some(row) => println!("  DISAGREE: field says {field_rank}, roster row says {row}."),
            None => println!("  this reader is not on their own roster."),
        }
        match guild_id {
            Some(id) => {
                println!("\n--- CMSG_GUILD_QUERY {id} (the roster does not carry the guild id)");
                connection.guild_query(id)?;
                connection.guild_info()?;
                let batch = connection.drain(std::time::Duration::from_millis(1500), 128)?;
                let query_body = batch
                    .iter()
                    .find(|p| p.opcode == world::opcode::server::GUILD_QUERY_RESPONSE)
                    .map(|p| p.body.clone());
                let info_body = batch
                    .iter()
                    .find(|p| p.opcode == world::opcode::server::GUILD_INFO)
                    .map(|p| p.body.clone());
                let report = state.replicate(&batch, None);
                for (opcode, error, body) in &report.failures {
                    println!("  undecodable {}: {error}", world::opcode::describe(*opcode));
                    if let Ok(body) = body {
                        println!("    {} bytes: {}", body.len(), hex_preview(body, 160));
                    }
                }
                if let Some(info) = state.guilds.get(&id) {
                    println!("  {:?}  id {}", info.name, info.id);
                    println!(
                        "  {} rank name(s): {:?}",
                        info.ranks.len(),
                        info.ranks
                    );
                    println!("  emblem {:?}", info.emblem);
                }
                if let Some(body) = query_body {
                    // **Ten names always travel**, and this is the measurement
                    // that says so rather than the assertion. Reading `n`
                    // names for every plausible `n` and asking which leaves
                    // the cursor exactly empty is the same shape as the
                    // trainer stride probe -- and unlike the roster above, the
                    // cursor *is* decisive here, because what follows the
                    // strings is six fixed words rather than more strings.
                    println!("\n  how many rank names does the body actually hold?");
                    for names in 0..=world::guild::MAX_RANKS + 2 {
                        let mut at = 4usize;
                        let mut ok = true;
                        for _ in 0..names + 1 {
                            match body.get(at..).and_then(|r| r.iter().position(|b| *b == 0)) {
                                Some(end) => at += end + 1,
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        let fits = ok && at + 6 * 4 == body.len();
                        if fits {
                            println!("    {names:>2} names + 6 words = {} bytes: FITS", body.len());
                        }
                    }
                    println!(
                        "    (anything else leaves the cursor short or long; {} bytes total)",
                        body.len()
                    );
                }
                if let Some(body) = info_body {
                    match world::guild::parse_guild_summary(&body) {
                        Ok(summary) => {
                            let d = summary.founded;
                            println!(
                                "\n--- SMSG_GUILD_INFO: {:?}, {} member(s) on {} account(s)",
                                summary.name, summary.members, summary.accounts
                            );
                            println!(
                                "  founded {:04}-{:02}-{:02} {:02}:{:02} (raw {:#010x})",
                                d.year, d.month, d.day, d.hour, d.minute, d.raw
                            );
                            // Three redundant bits, checked. Every plausible
                            // mis-shift moves the date without moving the
                            // weekday.
                            println!(
                                "  weekday field {} {} the date -- packing {}",
                                d.weekday,
                                if d.weekday_agrees() { "agrees with" } else { "DISAGREES with" },
                                if d.weekday_agrees() { "confirmed" } else { "REFUTED" }
                            );
                            println!(
                                "  no seconds and no timezone: this is the server's wall clock, not an instant."
                            );
                        }
                        Err(error) => println!("  SMSG_GUILD_INFO: {error}"),
                    }
                }
            }
            None => println!("\n  PLAYER_GUILDID is not replicated on this character."),
        }
    }

    // ------------------------------------------------- the silent requests
    if let Some(note) = drive.note {
        let (member, text) = note.split_once('=').unwrap_or((note, ""));
        println!("\n--- CMSG_GUILD_SET_PUBLIC_NOTE {member:?} = {text:?} (silent on success)");
        connection.guild_set_public_note(member, text)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Confirmed by effect and by re-asking, never by drawing the
        // intention: a silent send that was declined leaves the picture and
        // the realm disagreeing.
        connection.guild_roster()?;
        let batch = connection.drain(std::time::Duration::from_millis(1500), 256)?;
        state.replicate(&batch, None);
        if let Some(body) = batch
            .iter()
            .find(|p| p.opcode == world::opcode::server::GUILD_ROSTER)
            .map(|p| p.body.clone())
        {
            // Re-scored, not merely re-read: clearing an online member's note
            // is the change that makes the sample decisive, so the whole
            // three-way comparison is worth running again on the body it
            // produced.
            score_roster(&body, state);
        }
        match state
            .guild_roster
            .as_ref()
            .and_then(|r| r.members.iter().find(|m| m.name == member))
        {
            Some(m) if m.public_note == text => {
                println!("  -> the re-asked roster reads {:?}. Confirmed by effect.", m.public_note)
            }
            Some(m) => println!("  -> the roster still reads {:?}. NOT applied.", m.public_note),
            None => println!("  -> no member named {member:?} on the roster."),
        }
    }

    if let Some(text) = drive.motd {
        println!("\n--- CMSG_GUILD_MOTD {text:?}");
        connection.guild_motd(text)?;
        let batch = connection.drain(std::time::Duration::from_millis(1500), 256)?;
        let report = state.replicate(&batch, None);
        for event in &report.guild_events {
            println!("  SMSG_GUILD_EVENT kind {} params {:?} guid {:?}", event.kind, event.params, event.guid);
        }
        println!("  (a motd change is pushed as an event, which is how the roster stays current)");
    }

    if let Some(name) = drive.invite {
        println!("\n--- CMSG_GUILD_INVITE {name:?}");
        println!("  Half of a two-client rig: the invitation arrives at *their* session.");
        connection.guild_invite(name)?;
        let batch = connection.drain(std::time::Duration::from_millis(2000), 256)?;
        let report = state.replicate(&batch, None);
        for result in &report.guild_results {
            println!(
                "  command {} name {:?} result {} -- {}",
                result.command,
                result.name,
                result.result,
                world::guild::describe_command_result(result.command, result.result, &result.name)
            );
        }
    }

    if drive.accept {
        println!("\n--- waiting for SMSG_GUILD_INVITE, then CMSG_GUILD_ACCEPT (empty body)");
        let batch = connection.drain(std::time::Duration::from_secs(30), 512)?;
        state.replicate(&batch, None);
        match state.guild_invitation.clone() {
            Some(invite) => {
                println!(
                    "  {:?} asks this character to join {:?} -- and the packet carries no guid.",
                    invite.inviter, invite.guild
                );
                connection.guild_accept()?;
                let batch = connection.drain(std::time::Duration::from_millis(2000), 256)?;
                let report = state.replicate(&batch, None);
                for event in &report.guild_events {
                    println!("  SMSG_GUILD_EVENT kind {} params {:?} guid {:?}", event.kind, event.params, event.guid);
                }
            }
            None => println!("  nothing arrived."),
        }
    }

    if let Some(text) = drive.say {
        println!("\n--- guild chat: {text:?}");
        println!("  Reaches every member online and nobody else, so only a second session can");
        println!("  confirm it -- a guildless character's line is dropped by the server in");
        println!("  silence, which is what makes a one-client test of this worthless.");
        // **The character's own language, never zero.** This send was
        // written with a hardcoded `0` -- Universal -- which the server
        // refuses with no reply at all, and the first two-client run of it
        // came back with the listening session reporting `0 chat line(s)` and
        // no chat opcode in its tally at all. That is precisely the failure
        // `CLAUDE.md` records against three earlier attempts at chat, walked
        // into again by a probe written after the rule, and it is invisible
        // from the sending end: the request goes out, the session stays up,
        // and nothing comes back either way.
        let language = world::chat::language_for_race(
            state
                .get(own_guid)
                .and_then(|entity| entity.race())
                .unwrap_or(1),
        );
        println!("  sent in language {language} -- zero is Universal and is refused in silence.");
        connection.say(world::ChatType::Guild, language, "", text)?;
        let batch = connection.drain(std::time::Duration::from_millis(1500), 128)?;
        let report = state.replicate(&batch, None);
        for line in &report.chat {
            println!("  heard back: [{:?}] {}", line.channel, line.text);
        }
    }

    if drive.wait > 0 {
        println!("\n--- sitting still for {}s, reporting every SMSG_GUILD_EVENT", drive.wait);
        println!("  Drive it from a second session: log a guild member in or out.");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(drive.wait);
        let mut events = 0usize;
        // **Kept alive, because a long idle wait is exactly what the server
        // drops.** The first run of this loop had no ping in it and died at
        // `failed to fill whole buffer` partway through a 150-second wait --
        // the trap `CLAUDE.md` records for the world connection, walked into
        // again by a loop that sends nothing on purpose. Sent no faster than
        // `PING_INTERVAL`, since pinging too eagerly is punished harder than
        // not pinging at all.
        let mut last_ping = std::time::Instant::now();
        let mut chat_seen = 0usize;
        // **Every opcode seen, decoded or not** -- the same instrument
        // `--mail-wait` carries and for the same reason, which this loop was
        // written without anyway. Zero events could mean the server never
        // pushed one *or* that it pushed one under a number this client does
        // not recognise, and those want opposite investigations. Nothing else
        // in this loop can tell them apart.
        let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
        while std::time::Instant::now() < deadline {
            let batch = connection.drain(std::time::Duration::from_millis(1000), 128)?;
            for packet in &batch {
                *seen.entry(packet.opcode).or_default() += 1;
            }
            let report = state.replicate(&batch, None);
            for event in &report.guild_events {
                events += 1;
                println!(
                    "  kind {:<3} params {:?} guid {:?}",
                    event.kind, event.params, event.guid
                );
            }
            // Guild chat lands here too, and it is the other half of the
            // two-client rig: a line said by the far end and printed here is
            // the only proof the send went out at all.
            for line in &report.chat {
                chat_seen += 1;
                println!("  chat [{:?}] {}", line.channel, line.text);
            }
            if last_ping.elapsed() >= world::client::PING_INTERVAL {
                last_ping = std::time::Instant::now();
                connection.send_ping(0)?;
            }
        }
        println!("  {chat_seen} chat line(s) heard while waiting.");
        println!("  {events} event(s) in {}s.", drive.wait);
        println!("  every opcode seen while waiting:");
        for (opcode, count) in &seen {
            println!(
                "    {:<34} ({opcode:#06x}) x{count}",
                world::opcode::describe(*opcode)
            );
        }
    }

    let _ = drive.roster;
    Ok(())
}

struct MailDrive<'a> {
    list: bool,
    to: Option<&'a str>,
    subject: &'a str,
    text: &'a str,
    money: u32,
    item: Option<u32>,
    take: bool,
    clear: bool,
    wait: u64,
    own_guid: bool,
}

/// Drives the mail block and reports every packet in it.
///
/// **The novelty this probe exists to measure is an absence of a request.**
/// Everything else here sends something and reads the answer; `--mail-wait`
/// sends *nothing at all* and reports what arrives anyway, which is a shape
/// no other probe in this tree has needed.
///
/// The rest is the ordinary bounding move, and mail supplies it where trade
/// could not: `CMSG_SEND_MAIL` is answered either way, and the answer echoes
/// the **action** it was for, so a single send confirms the opcode number, the
/// body and the reply layout together. Everything silent in this block is sent
/// only after that has come back.
fn survey_mail(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    // Written back, like every walking probe here: replicated state holds our
    // login position forever, and four callers have already had to relearn it.
    here: &mut world::Position,
    drive: MailDrive<'_>,
) -> Result<()> {
    // The server logs "maximal 10 is allowed" and checks with its own
    // model-aware box test, so the approach aims comfortably inside -- the
    // same two-distance rule `approach_talker` documents at length.
    const REACH: f32 = 5.0;
    const APPROACH_TO: f32 = 3.0;
    const RUN_SPEED: f32 = 7.0;

    // ---------------------------------------------------------------- mailbox
    //
    // **A display id says how to draw a thing and nothing about what it is.**
    // Game objects have been replicated and drawn since Phase 3 and this is
    // the first feature that has to pick one *kind* out of a field of them, so
    // it is also the first send of `CMSG_GAMEOBJECT_QUERY` this client has
    // ever made.
    let entries: std::collections::BTreeSet<u32> =
        state.game_objects().filter_map(|go| go.entry()).collect();
    println!(
        "\ngame objects in view: {} objects, {} distinct entries",
        state.game_objects().count(),
        entries.len()
    );
    if !entries.is_empty() {
        println!("  asking what each one *is* -- a display id cannot answer that.");
        for entry in &entries {
            connection.ask_gameobject(*entry, 0)?;
        }
        let batch = connection.drain(std::time::Duration::from_millis(1200), 128)?;
        state.replicate(&batch, None);
    }

    let mut mailbox: Option<(u64, world::Position)> = None;
    for object in state.game_objects() {
        let Some(entry) = object.entry() else { continue };
        let info = state.names.gameobject(entry).flatten();
        let (name, kind) = match info {
            Some(info) => (info.name.clone().unwrap_or_default(), info.kind),
            None => (String::new(), u32::MAX),
        };
        let distance = object.position.map(|at| {
            ((at.x - here.x).powi(2) + (at.y - here.y).powi(2) + (at.z - here.z).powi(2)).sqrt()
        });
        let is_mailbox = info.is_some_and(|info| info.is_mailbox());
        println!(
            "  {:#018x}  entry {entry:<7} type {:<6} {:<28} {}{}",
            object.guid,
            if kind == u32::MAX {
                "?".to_string()
            } else {
                kind.to_string()
            },
            name,
            distance
                .map(|d| format!("{d:.1} units"))
                .unwrap_or_else(|| "(no position)".into()),
            if is_mailbox { "   <- MAILBOX" } else { "" }
        );
        if is_mailbox {
            if let Some(at) = object.position {
                let better = mailbox
                    .map(|(_, best): (u64, world::Position)| {
                        let d = |p: world::Position| {
                            ((p.x - here.x).powi(2) + (p.y - here.y).powi(2)).sqrt()
                        };
                        d(at) < d(best)
                    })
                    .unwrap_or(true);
                if better {
                    mailbox = Some((object.guid, at));
                }
            }
        }
    }

    // --------------------------------------------------- the game master trap
    //
    // Measured deliberately in order to be **ruled out**. The server accepts
    // the reader's own guid as a mailbox from anybody at moderator rank or
    // above, and every fixture account on this realm is a game master -- so
    // the cheapest probe available is the one that would ship a client working
    // only for its author. The same shape as "a refusal is a fact about the
    // actor", with the sign reversed: an *acceptance* can be one too.
    if drive.own_guid {
        println!("\n--mail-own-guid: CMSG_GET_MAIL_LIST naming this character's own guid.");
        println!("  The server accepts that from a game master and from nobody else.");
        connection.get_mail_list(own_guid)?;
        let batch = connection.drain(std::time::Duration::from_millis(1500), 128)?;
        let answered = batch
            .iter()
            .any(|p| p.opcode == world::opcode::server::MAIL_LIST_RESULT);
        let report = state.replicate(&batch, None);
        println!(
            "  -> {}",
            if answered {
                "ANSWERED. This works here and would not work for a player."
            } else {
                "no answer, which is what a non-GM would see."
            }
        );
        for (opcode, error, body) in &report.failures {
            println!("  undecodable {}: {error}", world::opcode::describe(*opcode));
            if let Ok(body) = body {
                println!("    {} bytes: {}", body.len(), hex_preview(body, 128));
            }
        }
    }

    let Some((mailbox_guid, at)) = mailbox else {
        println!("\nNo mailbox in view, so nothing below can run.");
        println!("  Northshire has none -- the nearest is 537 units away in Goldshire.");
        println!("  `.gobject add 142075` puts one at the character's feet.");
        return Ok(());
    };

    let distance = ((at.x - here.x).powi(2) + (at.y - here.y).powi(2)).sqrt();
    println!("\nusing mailbox {mailbox_guid:#018x}, {distance:.1} units away");
    if distance > REACH {
        let heading = (at.y - here.y).atan2(at.x - here.x);
        let close = distance - APPROACH_TO;
        println!("  closing {close:.1} units -- the server refuses past about {REACH:.0}");
        let (arrived, _) = connection.walk(own_guid, *here, heading, close, RUN_SPEED)?;
        *here = arrived;
        here.orientation = heading;
        let batch = connection.drain(std::time::Duration::from_millis(400), 128)?;
        state.replicate(&batch, None);
    }

    // ------------------------------------------------------------ the sending
    //
    // **The bounding send.** Answered either way, and the answer names the
    // action it was for -- so this one packet coming back settles the opcode
    // number, the body layout and the reply layout together. Nothing silent in
    // this block is sent before it has.
    if let Some(receiver) = drive.to {
        let mut attached = Vec::new();
        if let Some(entry) = drive.item {
            match world::inventory::carried(state, own_guid)
                .into_iter()
                .find(|carried| carried.item.entry == Some(entry))
            {
                Some(carried) => {
                    println!("\nattaching entry {entry}, item {:#018x}", carried.item.guid);
                    attached.push(carried.item.guid);
                }
                None => println!("\nnothing with entry {entry} is in the bags -- sending without."),
            }
        }
        println!(
            "\nposting to {receiver:?}: subject {:?}, {} copper, {} attachment(s)",
            drive.subject,
            drive.money,
            attached.len()
        );
        println!("  The server charges 30 copper for the stamp on top of the enclosure.");
        connection.send_mail(
            mailbox_guid,
            receiver,
            drive.subject,
            drive.text,
            drive.money,
            0,
            &attached,
        )?;
        let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
        let report = state.replicate(&batch, None);
        report_mail_results(&report);
        if report.mail_results.is_empty() {
            println!("\n  NOTHING came back, and that is the informative failure: this is the");
            println!("  one request in the block that is answered either way, so silence");
            println!("  means the opcode number is wrong rather than that the send was");
            println!("  declined. Everything that did arrive:");
            for packet in &batch {
                println!(
                    "    {} ({:#06x}), {} bytes",
                    world::opcode::describe(packet.opcode),
                    packet.opcode,
                    packet.body.len()
                );
            }
        }
        for (opcode, error, body) in &report.failures {
            println!("  undecodable {}: {error}", world::opcode::describe(*opcode));
            if let Ok(body) = body {
                println!("    {} bytes: {}", body.len(), hex_preview(body, 128));
            }
        }
    }

    // ------------------------------------------------------------- the inbox
    if drive.list || drive.take || drive.clear {
        // Asked from anywhere first, because it is the only mail question that
        // does not need a mailbox and it is what a client has at login.
        println!("\nMSG_QUERY_NEXT_MAIL_TIME -- the one mail question with no mailbox in it.");
        connection.query_next_mail_time()?;
        let batch = connection.drain(std::time::Duration::from_millis(1200), 128)?;
        for packet in &batch {
            if packet.opcode == world::opcode::server::QUERY_NEXT_MAIL_TIME {
                println!(
                    "  {} bytes: {}",
                    packet.body.len(),
                    hex_preview(&packet.body, 96)
                );
                match world::mail::parse_next_mail_time(&packet.body) {
                    Ok(next) => {
                        println!(
                            "  -> marker {:.1} ({}), {} sender(s) named -- the server stops at two",
                            next.marker,
                            if next.has_unread() {
                                "something unread"
                            } else {
                                "nothing unread"
                            },
                            next.pending.len()
                        );
                        for pending in &next.pending {
                            println!(
                                "     player {:#018x} entry {} kind {} in {:.0}s",
                                pending.player, pending.entry, pending.kind, pending.delay
                            );
                        }
                    }
                    Err(error) => println!("  -> WILL NOT PARSE: {error}"),
                }
            }
        }
        state.replicate(&batch, None);

        ask_inbox(connection, state, mailbox_guid)?;
        print_inbox(state);
    }

    // ------------------------------------------------------------- collecting
    if drive.take {
        let target = state
            .mail
            .as_ref()
            .and_then(|inbox| inbox.mails.iter().find(|mail| mail.has_anything()))
            .cloned();
        match target {
            None => println!("\n--mail-take: nothing in the inbox has anything in it."),
            Some(mail) => {
                println!("\ntaking from mail {} ({:?})", mail.id, mail.subject);
                if mail.money > 0 {
                    println!("  {} copper", mail.money);
                    connection.mail_take_money(mailbox_guid, mail.id)?;
                    let batch = connection.drain(std::time::Duration::from_millis(1200), 64)?;
                    report_mail_results(&state.replicate(&batch, None));
                }
                for item in &mail.items {
                    println!("  attachment {} (entry {})", item.guid, item.entry);
                    connection.mail_take_item(mailbox_guid, mail.id, item.guid)?;
                    let batch = connection.drain(std::time::Duration::from_millis(1200), 64)?;
                    report_mail_results(&state.replicate(&batch, None));
                }
                // Marking read is the one silent request here, so it is sent
                // and then *confirmed by re-asking* -- the only confirmation
                // available.
                println!("  marking it read (the one silent request in this block)");
                connection.mail_mark_as_read(mailbox_guid, mail.id)?;
                let batch = connection.drain(std::time::Duration::from_millis(800), 64)?;
                state.replicate(&batch, None);

                println!("\nre-asking the inbox rather than editing the local copy:");
                ask_inbox(connection, state, mailbox_guid)?;
                print_inbox(state);
            }
        }
    }

    // ------------------------------------------------------------- discarding
    if drive.clear {
        let empties: Vec<(u32, u32)> = state
            .mail
            .as_ref()
            .map(|inbox| {
                inbox
                    .mails
                    .iter()
                    .filter(|mail| !mail.has_anything())
                    .map(|mail| (mail.id, mail.template_id))
                    .collect()
            })
            .unwrap_or_default();
        println!("
--mail-clear: {} letter(s) with nothing left in them.", empties.len());
        println!("  Answered like everything else here, so a refusal is legible: a letter");
        println!("  with cash on delivery on it is declined rather than destroyed.");
        for (id, template) in empties {
            connection.mail_delete(mailbox_guid, id, template)?;
            let batch = connection.drain(std::time::Duration::from_millis(900), 64)?;
            report_mail_results(&state.replicate(&batch, None));
        }
        ask_inbox(connection, state, mailbox_guid)?;
        print_inbox(state);
    }

    // --------------------------------------------- the effect with no request
    if drive.wait > 0 {
        println!("\nwaiting {}s and sending NOTHING.", drive.wait);
        println!("  Anything that arrives arrived because somebody else acted, which is");
        println!("  what no other packet this client reads has ever done. Drive it with");
        println!("  `.send mail <name> \"subject\" \"text\"` at the worldserver console, or");
        println!("  from a second session.");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(drive.wait);
        let mut arrivals = 0usize;
        // **Every opcode seen, decoded or not.** The cheapest instrument in
        // this box, and its absence made the first run of this probe
        // ambiguous: zero arrivals could mean the server never sent one *or*
        // that it sent one under a number this client does not recognise, and
        // those want opposite investigations. Three earlier milestones learnt
        // that and this loop was written without it anyway.
        let mut seen: std::collections::BTreeMap<u16, usize> = Default::default();
        while std::time::Instant::now() < deadline {
            let batch = connection.drain(std::time::Duration::from_millis(500), 64)?;
            for packet in &batch {
                *seen.entry(packet.opcode).or_default() += 1;
            }
            let report = state.replicate(&batch, None);
            if report.received_mail > 0 {
                arrivals += report.received_mail;
                println!(
                    "  <- SMSG_RECEIVED_MAIL x{} at {:.0}s. Four bytes, and they are zero:",
                    report.received_mail,
                    (drive.wait as f64)
                        - deadline
                            .saturating_duration_since(std::time::Instant::now())
                            .as_secs_f64()
                );
                for packet in &batch {
                    if packet.opcode == world::opcode::server::RECEIVED_MAIL {
                        println!("     body: {}", hex_preview(&packet.body, 32));
                    }
                }
                println!("     no sender, no subject, no count. The only honest thing to draw");
                println!("     is that something is unread -- and finding out what needs a");
                println!("     mailbox, which may be a continent away.");
            }
            report_mail_results(&report);
        }
        println!(
            "\n  {arrivals} arrival(s) in {}s. mail_waiting is now {}.",
            drive.wait, state.mail_waiting
        );
        println!("  every opcode seen while sending nothing:");
        for (opcode, count) in &seen {
            println!(
                "    {:<34} ({opcode:#06x}) x{count}",
                world::opcode::describe(*opcode)
            );
        }
        if arrivals > 0 {
            println!("  Asking the inbox now, to see what the arrival would not say:");
            ask_inbox(connection, state, mailbox_guid)?;
            print_inbox(state);
        }
    }

    Ok(())
}

/// Asks a mailbox for the inbox and folds the answer in.
fn ask_inbox(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    mailbox: u64,
) -> Result<()> {
    connection.get_mail_list(mailbox)?;
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    let report = state.replicate(&batch, None);
    for (opcode, error, body) in &report.failures {
        println!("  undecodable {}: {error}", world::opcode::describe(*opcode));
        if let Ok(body) = body {
            println!("    {} bytes: {}", body.len(), hex_preview(body, 256));
        }
    }
    // Two counts, not one. "the request was answered" and "the inbox is
    // empty" draw the same picture and are different facts -- the same
    // reason the trade probe printed two halves rather than one.
    println!(
        "  {} inbox packet(s) answered CMSG_GET_MAIL_LIST",
        report.inboxes
    );
    Ok(())
}

/// Prints the inbox, **with each record's announced length beside its real
/// one**.
///
/// That pair is the measurement: a `u16` at the head of every record says how
/// long the record is, and this server's number is four too many on every one
/// of them. Printing both is what turns a reading of the server's source into
/// an observation.
fn print_inbox(state: &world::WorldState) {
    let Some(inbox) = state.mail.as_ref() else {
        println!("  no inbox has been described.");
        return;
    };
    println!(
        "\ninbox: server counted {}, sent {}{}",
        inbox.total,
        inbox.mails.len(),
        if inbox.withheld() > 0 {
            format!(" -- {} withheld and named nowhere", inbox.withheld())
        } else {
            String::new()
        }
    );
    for mail in &inbox.mails {
        println!(
            "  #{:<7} {:<24} from {:<28} {}c  cod {}  flags {:#04x}{}  {:.1} days",
            mail.id,
            format!("{:?}", mail.subject),
            match mail.sender {
                world::MailSender::Player(guid) => format!("player {guid:#018x}"),
                other => format!("{other:?}"),
            },
            mail.money,
            mail.cod,
            mail.flags,
            if mail.is_read() { " read" } else { "" },
            mail.days_left,
        );
        println!(
            "          announced {} bytes, parsed {} -- a {} overcount",
            mail.announced_bytes,
            mail.actual_bytes,
            mail.announced_bytes as i64 - mail.actual_bytes as i64
        );
        if !mail.body.is_empty() {
            println!("          body: {:?}", mail.body);
        }
        for item in &mail.items {
            println!(
                "          attachment {}: low guid {} entry {} x{} durability {}/{}",
                item.index, item.guid, item.entry, item.count, item.durability, item.max_durability
            );
        }
    }
}

/// Prints every `SMSG_SEND_MAIL_RESULT` in a batch.
///
/// The action is printed as well as the result because **the action is what
/// ties a reply to a request**: several mail requests can be in flight and the
/// echo is the only thing that says which answer belongs to which.
fn report_mail_results(report: &world::state::Replication) {
    for result in &report.mail_results {
        println!(
            "  <- SMSG_SEND_MAIL_RESULT: mail {} {:?} -> {}{}{}",
            result.id,
            result.action,
            if result.result == world::mail::MAIL_OK {
                "OK".to_string()
            } else {
                format!("refused, code {}", result.result)
            },
            result
                .equip_error
                .map(|e| format!(" (inventory error {e})"))
                .unwrap_or_default(),
            result
                .taken
                .map(|(guid, count)| format!(" (took item {guid} x{count})"))
                .unwrap_or_default(),
        );
    }
}

/// What a `--trade` run should do.
///
/// A struct rather than eight arguments, because both halves of the two-client
/// rig run the *same* function with different flags -- the initiator and the
/// responder differ only in who sends first, and writing them as two functions
/// would have produced two drifting copies of the same state machine.
struct TradeDrive<'a> {
    partner: Option<&'a str>,
    wait: bool,
    decline: bool,
    nobody: bool,
    item: Option<u32>,
    gold: Option<u32>,
    accept: bool,
    seconds: u64,
}

/// Drives a player-to-player trade and reports every packet in it.
///
/// **The first probe in this tree where one client cannot produce the
/// observation.** Everything before this asked the server for something and
/// the server answered; a trade needs a second person's client to send a
/// packet of its own before anything at all comes back here. So this function
/// is half a rig: one session runs `--trade <name>` and another runs
/// `--trade-wait`, and neither on its own proves the opcode is right.
///
/// Except for one part, which is why `--trade-nobody` exists and runs first.
/// `CMSG_INITIATE_TRADE` is answered on *failure*, so aiming it at a guid that
/// is not a player produces a reply immediately, from one client, with nobody
/// else in the world -- confirming the opcode number, the eight-byte body and
/// the reply layout together. That is the same bounding move
/// `CMSG_LIST_INVENTORY` made for `CMSG_BUY_ITEM`, with the novelty that the
/// bounding case is this request's own refusal rather than a neighbour's
/// success.
///
/// Everything printed is read back off the wire. In particular the offer this
/// client staged is reported from the server's **reflection** of it and never
/// from what was sent: `CMSG_SET_TRADE_ITEM` is silent, so a client that drew
/// its own intentions would show an item the server refused as though it were
/// on the table.
fn survey_trade(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
    // Written back, like every other walking probe here: replicated state
    // holds our login position forever, and three callers have already had to
    // relearn that the hard way.
    here: &mut world::Position,
    drive: TradeDrive<'_>,
) -> Result<()> {
    use world::TradeStatus;

    // The server's own limit is ten units. Approached to comfortably inside
    // it for the reason `approach_talker` documents at length: aiming at the
    // threshold produces a loop that asymptotically fails to cross it.
    const TRADE_RANGE: f32 = 10.0;
    const APPROACH_TO: f32 = 4.0;
    const RUN_SPEED: f32 = 7.0;
    // A burst of identical requests does not get refused, it gets the socket
    // closed -- `10053`, observed on the party loot control in 4.20. Every
    // repeated send here is spaced.
    const RESEND_EVERY: std::time::Duration = std::time::Duration::from_millis(1200);

    if drive.nobody {
        // **The bounding send.** A guid shaped like a creature's and occupied
        // by nothing: the server looks it up as a player, finds nothing, and
        // says so. What matters is not the refusal but that it *arrives* -- a
        // wrong opcode number produces silence here, and silence is what the
        // rest of this block is made of.
        let nobody = 0xF130_0000_0000_0001u64;
        println!("\n--trade-nobody: CMSG_INITIATE_TRADE at {nobody:#018x}, which is not a player.");
        println!("  A reply at all confirms the opcode, the body and the status layout,");
        println!("  from one client, with nobody else logged in.");
        connection.initiate_trade(nobody)?;
        let batch = connection.drain(std::time::Duration::from_millis(1500), 128)?;
        let mut answered = false;
        for packet in &batch {
            if packet.opcode == world::opcode::server::TRADE_STATUS {
                answered = true;
                println!(
                    "\n  SMSG_TRADE_STATUS, {} bytes: {}",
                    packet.body.len(),
                    hex_preview(&packet.body, 64)
                );
                match world::trade::parse_trade_status(&packet.body) {
                    Ok(status) => println!("  -> {status:?}"),
                    // Printed rather than counted: a body that will not decode
                    // here is the one thing that could have answered the
                    // question, and two tools in this tree have already logged
                    // a length and dropped the bytes.
                    Err(error) => println!("  -> WILL NOT PARSE: {error}"),
                }
            }
        }
        if !answered {
            println!("\n  NOTHING came back. Everything that did arrive:");
            for packet in &batch {
                println!(
                    "    {} ({:#06x}), {} bytes",
                    world::opcode::describe(packet.opcode),
                    packet.opcode,
                    packet.body.len()
                );
            }
            println!("\n  This is the informative failure: the one request in this block that");
            println!("  is answered did not answer, so the opcode number is wrong -- there is");
            println!("  no 'declined' reading of a refusal that never came.");
        }
        state.replicate(&batch, None);
        if drive.partner.is_none() && !drive.wait {
            return Ok(());
        }
    }

    // Names first: a player is anonymous until a query comes back, so a
    // partner named on the command line cannot be matched without them.
    if drive.partner.is_some() {
        resolve_names(connection, state, 2)?;
    }

    if let Some(wanted) = drive.partner {
        let found = state
            .players()
            .filter(|entity| entity.guid != own_guid)
            .find(|entity| {
                state
                    .names
                    .player(entity.guid)
                    .flatten()
                    .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
            })
            .map(|entity| (entity.guid, entity.position));

        let Some((partner, at)) = found else {
            println!("\n--trade: no replicated player is called {wanted:?}.");
            println!("  Players in view:");
            for entity in state.players().filter(|e| e.guid != own_guid) {
                println!(
                    "    {:#018x}  {}",
                    entity.guid,
                    state
                        .names
                        .player(entity.guid)
                        .flatten()
                        .unwrap_or("(unnamed)")
                );
            }
            println!("\n  A trade partner has to be *replicated*, which needs them in");
            println!("  visibility range -- and they must be anyway, since the server");
            println!("  refuses past {TRADE_RANGE:.0} units. Two clients in different starting");
            println!("  zones cannot see each other and prove nothing.");
            return Ok(());
        };

        // Walk into range if need be. Measured from `here`, never from
        // replicated state: the server does not relay our own movement back,
        // so our replicated position is wherever we logged in.
        if let Some(there) = at {
            let d = ((there.x - here.x).powi(2) + (there.y - here.y).powi(2)).sqrt();
            println!("\n{wanted} is {partner:#018x}, {d:.1} units away");
            if d > TRADE_RANGE {
                let heading = (there.y - here.y).atan2(there.x - here.x);
                let close = d - APPROACH_TO;
                println!(
                    "  closing {close:.1} units first -- the server refuses past {TRADE_RANGE:.0}"
                );
                let (arrived, _) = connection.walk(own_guid, *here, heading, close, RUN_SPEED)?;
                *here = arrived;
                here.orientation = heading;
                let batch = connection.drain(std::time::Duration::from_millis(400), 128)?;
                state.replicate(&batch, None);
            }
        }

        println!("\nasking {wanted} to trade. This send is SILENT when it works:");
        println!("  the server answers it at *their* client, not at this one.");
        connection.initiate_trade(partner)?;
        // Recorded locally because it is nowhere on the wire -- the initiator
        // is never told who it asked. See `WorldState::note_trade_request`.
        state.note_trade_request(partner);
    }

    // The driving loop. Both roles run it; what differs is only who sent
    // first.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(drive.seconds);
    let mut answered_offer = false;
    let mut staged = false;
    let mut staged_at: Option<std::time::Instant> = None;
    let mut last_accept: Option<std::time::Instant> = None;
    let mut last_theirs: Option<world::TradeOffer> = None;
    let mut last_ours: [Option<u64>; world::trade::TRADE_SLOTS] = Default::default();
    // Counted rather than assumed. The **own-half** form of
    // `SMSG_TRADE_STATUS_EXTENDED` was expected to arrive and does not: the
    // server sends an offer to the partner and never back to its author. That
    // is a claim about an absence, so the run reports the two counts side by
    // side -- "zero of N" is a measurement and "it never printed" is not.
    let mut halves = (0usize, 0usize);
    let mut finished = false;

    println!(
        "\ndriving the trade for {}s. Every SMSG_TRADE_STATUS is printed as it lands.",
        drive.seconds
    );

    while std::time::Instant::now() < deadline && !finished {
        // A small batch, for the reason `hold_connection` documents: a
        // populated zone is never quiet, so a large limit means few long
        // iterations and a state machine that reacts in coarse jumps.
        let batch = connection.drain(std::time::Duration::from_millis(300), 48)?;
        let report = state.replicate(&batch, None);
        halves.0 += report.trade_offers;
        halves.1 += report.trade_own_offers;

        for status in &report.trade_statuses {
            println!("  <- {status:?}");
            match status {
                // Terminal, and the run is over: printing the reason is the
                // whole point, because every *request* in this block is
                // silent and these are the only things that ever explain one.
                TradeStatus::Complete => {
                    println!("     the exchange went through. Check the bags on both clients.");
                    finished = true;
                }
                TradeStatus::Cancelled => {
                    println!("     called off.");
                    finished = true;
                }
                TradeStatus::NoTarget | TradeStatus::TooFar | TradeStatus::Busy => {
                    println!("     refused. Note this arrived at the *sender*, which is what");
                    println!("     makes the initiate request confirmable at all.");
                }
                _ => {}
            }
        }
        for (opcode, error, body) in &report.failures {
            println!("  undecodable {}: {error}", world::opcode::describe(*opcode));
            if let Ok(body) = body {
                println!("    {} bytes: {}", body.len(), hex_preview(body, 96));
            }
        }

        let Some(session) = state.trade.clone() else {
            if finished {
                break;
            }
            continue;
        };

        // Answer an offer, once. Which of the three answers goes out is the
        // whole of the responder's decision, and the server closes the trade
        // on either refusal -- so exactly one is sent per offer.
        if drive.wait && !session.open && !answered_offer {
            // Ask who this is before saying it. The responder never ran the
            // name sweep the initiator does -- it has no name to look up until
            // somebody asks it to trade -- so without this the one line that
            // says a trade was offered says it about a bare guid.
            if let Some(guid) = session.partner {
                if state.names.player(guid).is_none() {
                    connection.ask_player_name(guid)?;
                    let batch = connection.drain(std::time::Duration::from_millis(700), 64)?;
                    state.replicate(&batch, None);
                }
            }
            let who = session
                .partner
                .map(|guid| unit_label_for(state, guid))
                .unwrap_or_else(|| "somebody".into());
            if drive.decline {
                println!("\n  {who} offered a trade -- answering BUSY.");
                connection.busy_trade()?;
            } else {
                println!("\n  {who} offered a trade -- opening the window.");
                connection.begin_trade()?;
            }
            answered_offer = true;
        }

        // Report each half of the window whenever it changes. Both halves are
        // printed, and which is which is the leading byte of the body: a
        // client that filed them together would draw a completely plausible
        // window describing one person's goods twice.
        if session.theirs != last_theirs {
            if let Some(offer) = &session.theirs {
                println!("  <- their half: {}", describe_trade_offer(offer));
            }
            last_theirs = session.theirs.clone();
        }
        // Our own half is **local**: the server does not send it back, so this
        // prints what this client asked for rather than what anything
        // confirmed. See `world::trade`.
        if session.ours != last_ours {
            let held: Vec<String> = session
                .ours
                .iter()
                .enumerate()
                .filter_map(|(slot, item)| item.map(|guid| format!("slot {slot} = item {guid:#x}")))
                .collect();
            println!(
                "  -- our half (local): {} [{}c]",
                if held.is_empty() {
                    "nothing".to_string()
                } else {
                    held.join(", ")
                },
                session.our_money
            );
            last_ours = session.ours;
        }

        if session.open && !staged {
            if let Some(entry) = drive.item {
                let found = world::inventory::carried(state, own_guid)
                    .into_iter()
                    .find(|carried| carried.item.entry == Some(entry));
                match found {
                    Some(carried) => {
                        let (bag, slot) = carried.at.address();
                        println!(
                            "\n  putting entry {entry} (bag {bag}, slot {slot}) into trade slot 0."
                        );
                        println!("  Silent: the proof is the offer coming back with it in.");
                        connection.set_trade_item(0, bag, slot)?;
                        // Recorded beside the send, because nothing will
                        // record it for us -- our own half of the window has
                        // no packet behind it.
                        state.note_trade_item(0, carried.item.guid);
                    }
                    None => {
                        println!("\n  nothing with entry {entry} is in the bags -- not staged.")
                    }
                }
            }
            if let Some(copper) = drive.gold {
                println!("  putting {copper} copper on the table.");
                connection.set_trade_gold(copper)?;
                state.note_trade_gold(copper);
            }
            staged = true;
            staged_at = Some(std::time::Instant::now());
        }

        // Accept, and re-accept if the server takes it back. Changing an
        // offer resets *both* accepts, and two scripted clients staging a
        // second apart hit that every run -- so this is a loop condition
        // rather than a one-shot, with a cooldown for the reason above.
        if drive.accept && session.open && staged && !session.accepted {
            let settled = staged_at.is_some_and(|at| at.elapsed() >= RESEND_EVERY);
            let cooled = last_accept.is_none_or(|at| at.elapsed() >= RESEND_EVERY);
            if settled && cooled {
                println!("  -> accepting (token {})", session.token);
                connection.accept_trade(session.token)?;
                // Local: the server never reports a client's own accept back
                // to it, only the consequences.
                state.note_trade_accept();
                last_accept = Some(std::time::Instant::now());
            }
        }
    }

    println!(
        "
SMSG_TRADE_STATUS_EXTENDED: {} describing the partner's half, {} describing our own.",
        halves.0, halves.1
    );
    println!("  The second number is the claim: an offer is sent to the *other* person");
    println!("  and never back to whoever made it, so a client's own half of the window");
    println!("  is the one thing in this block it has to remember for itself.");

    match &state.trade {
        Some(session) => {
            println!("\ntrade still open at the end of the run:");
            println!(
                "  partner {:?}, token {}, accepted {} / theirs {}",
                session.partner, session.token, session.accepted, session.partner_accepted
            );
        }
        None => println!("\nno trade open at the end of the run."),
    }
    Ok(())
}

/// One line describing half a trade window.
fn describe_trade_offer(offer: &world::TradeOffer) -> String {
    let mut parts: Vec<String> = offer
        .items
        .iter()
        .map(|item| {
            format!(
                "slot {} = entry {} x{} (display {})",
                item.slot, item.entry, item.count, item.display_id
            )
        })
        .collect();
    if offer.money != 0 {
        let (g, s, c) = world::inventory::purse(offer.money);
        parts.push(format!("{g}g {s}s {c}c"));
    }
    if parts.is_empty() {
        parts.push("nothing".into());
    }
    format!(
        "{} [token {}, slots {}/{}]",
        parts.join(", "),
        offer.token,
        offer.slot_count,
        offer.slot_count_again
    )
}
