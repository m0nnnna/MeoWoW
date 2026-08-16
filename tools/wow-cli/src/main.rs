//! Inspection tool for a WoW 3.3.5a installation's data files.
//!
//! Points at a `Data` directory and reads through the patch chain exactly as
//! the client would, so what it prints is what the engine would see.

use std::path::PathBuf;

use anyhow::{Context, Result};

mod light;
use clap::{Parser, Subcommand};
use mpq::{Archive, Chain};

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
    /// Inspect spell description templates.
    #[command(subcommand)]
    Spell(SpellCommand),
    /// Inspect world objects: buildings, dungeons, bridges.
    #[command(subcommand)]
    Wmo(WmoCommand),
    /// Inspect terrain.
    #[command(subcommand)]
    Adt(AdtCommand),
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
        /// **An experimental probe, not a confirmed feature.** Sends
        /// `ClientOpcode::SwapItemCandidate` (`0x010B`), which `foss-wow#55`
        /// has not yet confirmed against a live realm, and diffs the slot
        /// array the same way `--equip` does. If nothing moves, prints every
        /// opcode that arrived so a wrong guess and a refusal are not
        /// mistaken for each other.
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
        /// Ask where the objectives of every quest in the log are.
        ///
        /// The check the whole native-tracker plan rests on: WotLK shipped its
        /// own quest tracker, so the server already holds the map markers.
        /// Answers **only for quests in the player's own log**, so an empty
        /// log makes this vacuous rather than negative -- which is why the log
        /// is printed alongside.
        #[arg(long)]
        quest_poi: bool,
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
        /// CharSections, CharHairGeosets, Item, ItemDisplayInfo, Light,
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
            items,
            equip,
            swap,
            loot,
            gossip,
            gossip_select,
            buy,
            sell_back,
            quest,
            quest_accept,
            quest_poi,
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
                items: *items,
                equip,
                swap: swap.as_deref(),
                loot: *loot,
                gossip: *gossip,
                gossip_select: *gossip_select,
                buy: *buy,
                sell_back: *sell_back,
                quest: *quest,
                quest_accept: *quest_accept,
                quest_poi: *quest_poi,

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
        Command::Spell(cmd) => spell_cmd(&mut chain, &cmd),
        Command::Wmo(cmd) => wmo_cmd(&mut chain, cmd),
        Command::Adt(cmd) => adt_cmd(&mut chain, cmd),
        Command::Light {
            map,
            x,
            y,
            hour,
            weather_check,
        } => {
            if weather_check {
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
    items: bool,
    equip: &'a [u16],
    swap: Option<&'a str>,
    loot: bool,
    gossip: Option<u32>,
    gossip_select: Option<u32>,
    buy: Option<u32>,
    sell_back: bool,
    quest: Option<u32>,
    quest_accept: Option<u32>,
    quest_poi: bool,
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
    locale: &'a str,
    timeout: u64,
    /// Only touched by `--visible-items`, which is a game-file question
    /// wearing a network command's clothes -- see the comment in `main`
    /// about why `World` otherwise never demands a data directory.
    data: Option<&'a std::path::Path>,
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
        items,
        equip,
        swap,
        loot,
        gossip,
        gossip_select,
        buy,
        sell_back,
        quest,
        quest_accept,
        quest_poi,
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

        if let Some(spell_id) = cast {
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

        if stay > 0 {
            hold_connection(
                &mut connection,
                std::time::Duration::from_secs(stay),
                &mut state,
                character.guid,
                capture.as_mut(),
            )?;
        }

        if let (Some(capture), Some(path)) = (capture, capture_path) {
            capture.finish(path)?;
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

        if let Some(entry) = quest {
            let prefer = (entry != 0).then_some(entry);
            survey_quests(
                &mut connection,
                &mut state,
                character.guid,
                &mut here,
                prefer,
                quest_accept,
            )?;
        }

        if quest_poi {
            survey_quest_poi(&mut connection, &mut state, character.guid)?;
        }

        if unit_fields {
            report_unit_fields(&state, character.guid);
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

        if own_fields {
            report_own_fields(&state, character.guid, "after");
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

/// Prints everything a fold *returns* rather than stores: chat and swings.
///
/// One function because there are now three places that drain packets, and
/// the standing hazard in this crate is a caller that folds a batch and drops
/// the events it hands back. The count that made this worth extracting was a
/// summary reading "0 attacks started, 2 stopped" -- impossible, and caused by
/// the approach loop folding the starts and discarding the report.
fn print_events(report: &world::Replication, state: &world::WorldState, own_guid: u64) {
    for message in &report.chat {
        println!("  {}", describe_chat(message, state));
    }
    for swing in &report.swings {
        println!(
            "  {}",
            world::combat::describe_swing(swing, own_guid, |guid| combat_name(state, guid))
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
    println!("\nbodies of everything unexpected:");
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

/// Asks the server where the objectives of every quest in the log are.
///
/// **The whole native-quest-tracker plan rests on this working.** WotLK
/// shipped its own tracker, so the server already holds the map markers and
/// will hand them over -- which is why this client does not need to ship
/// anybody's quest database. If it comes back empty, that plan needs
/// rethinking, so the command is deliberately loud about which of the two
/// reasons an empty answer has.
fn survey_quest_poi(
    connection: &mut world::Connection,
    state: &mut world::WorldState,
    own_guid: u64,
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
) -> Result<()> {
    let Some(npc) = approach_talker(connection, state, own_guid, here, prefer)? else {
        return Ok(());
    };

    connection.set_selection(npc.guid)?;
    println!(
        "\ngreeting questgiver {:#018x} entry {} at {:.1} units, npcflag {} ({:#x})",
        npc.guid, npc.entry, npc.distance, npc.flags, npc.flags
    );
    connection.questgiver_hello(npc.guid)?;
    let batch = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    state.replicate(&batch, None);

    dump_unexpected(&batch, "after CMSG_QUESTGIVER_HELLO");

    let Some(wanted) = accept else {
        return Ok(());
    };

    // Ask for the scroll before accepting, in that order, because that is the
    // order a real client uses and the server checks the NPC actually offers
    // the quest at each step. Skipping straight to the accept would work or
    // not for reasons this survey could not tell apart.
    println!("\nasking for quest {wanted}'s details");
    connection.query_quest(npc.guid, wanted)?;
    let details = connection.drain(std::time::Duration::from_millis(2000), 128)?;
    state.replicate(&details, None);
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
    println!("quest log before: {log_before:?}");

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
        None if log_after.contains(&wanted) => {
            println!("\n-- quest {wanted} was ALREADY in the log before this ran, so");
            println!("   nothing here tests the accept. Clear it first:");
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
fn dump_unexpected(batch: &[world::client::Packet], what: &str) {
    const NOISE: [u16; 6] = [0x00DD, 0x00A9, 0x01F6, 0x0390, 0x0085, 0x0086];
    println!("\nbodies of everything unexpected {what}:");
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
    println!("\nbodies of everything unexpected:");
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

/// Sends the unconfirmed `SwapItemCandidate` between two of the player's own
/// slots and reports what moved -- the same diff `equip_and_report` uses,
/// copied rather than shared because this one is testing the *opcode itself*
/// and has no business looking confirmed until it is.
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
        println!("both slots are empty; a run against two empty slots cannot tell");
        println!("a working opcode from a broken one");
        return Ok(());
    }

    // **Both body shapes, one run, and the comparison is the point.** The
    // four-byte form was tried alone and refused identically whatever the
    // destination held -- but the refusal was
    // `SMSG_INVENTORY_CHANGE_FAILURE`, which the server only sends after
    // routing the request to an inventory handler. An opcode it did not
    // recognise is dropped in silence, not answered, so that reply is
    // evidence *for* the number and against the body. `CMSG_AUTOEQUIP_ITEM`
    // next door at 0x010A takes two bytes; a request naming two slots of the
    // same array has no use for two bag bytes.
    //
    // Sending both in one session against the same pair is what makes the
    // answer readable: the two attempts differ in the body alone, so a
    // difference in what comes back cannot be about the character, the
    // slots, or the state of the world.
    let attempts: [(&str, Box<dyn Fn(&mut world::Connection) -> Result<(), world::client::Error>>); 2] = [
        (
            "four-byte {dst_bag, dst_slot, src_bag, src_slot}",
            Box::new(move |c: &mut world::Connection| {
                c.swap_item_candidate(
                    inventory::OWN_SLOT_ARRAY,
                    to.index() as u8,
                    inventory::OWN_SLOT_ARRAY,
                    from.index() as u8,
                )
            }),
        ),
        (
            "two-byte {src_slot, dst_slot}",
            Box::new(move |c: &mut world::Connection| {
                c.swap_own_slots(from.index() as u8, to.index() as u8)
            }),
        ),
    ];

    let mut moved = Vec::new();
    let mut after: std::collections::BTreeMap<u16, u64>;

    for (shape, send) in &attempts {
        println!("\n--- body: {shape}");
        send(connection)?;
        let batch = connection.drain(std::time::Duration::from_millis(1200), 128)?;
        state.replicate(&batch, None);

        after = inventory::held(state, own_guid)
            .into_iter()
            .map(|item| (item.slot.index(), item.guid))
            .collect();

        moved.clear();
        for index in 0..inventory::SLOT_COUNT {
            let (was, now) = (before.get(&index), after.get(&index));
            if was != now {
                moved.push((index, was.copied(), now.copied()));
            }
        }

        // The bodies of whatever came back, always -- a refusal that is seen
        // and dropped is the one packet that could have answered the
        // question. The failure's own result code is the informative byte and
        // it is printed raw rather than named: naming a status code from
        // memory is what `describe_cast_failure` exists to refuse.
        println!("    what arrived (body in full, not just its length):");
        let mut said_something = false;
        for packet in &batch {
            // The constant traffic would bury the answer; it is counted in
            // the histogram the caller already prints.
            const NOISE: [u16; 6] = [0x00DD, 0x00A9, 0x01F6, 0x0390, 0x0085, 0x0086];
            if NOISE.contains(&packet.opcode) {
                continue;
            }
            let hex: Vec<String> = packet.body.iter().map(|b| format!("{b:02x}")).collect();
            println!(
                "      {:<34} {:>3} bytes: {}",
                world::opcode::describe(packet.opcode),
                packet.body.len(),
                hex.join(" ")
            );
            said_something = true;

            // **Split the refusal up, because its shape is the finding.**
            // An 18-byte body divides exactly as `{u8 code, u64 guid, u64
            // guid, u8}`, and the first guid is a *real item* -- the one
            // named by the leading (bag, slot) pair of the request. A server
            // that had misparsed the body could not have resolved it, so
            // this is a considered refusal rather than a rejected shape,
            // which is the opposite of what the four-byte attempt was
            // originally read as meaning.
            //
            // The code is printed raw and **not named**. Only one value has
            // ever been observed and naming a status code from memory is
            // exactly what `describe_cast_failure` exists to refuse.
            if packet.opcode == 0x0112 && packet.body.len() == 18 {
                let code = packet.body[0];
                let guid = |at: usize| {
                    u64::from_le_bytes(packet.body[at..at + 8].try_into().unwrap())
                };
                let (first, second) = (guid(1), guid(9));
                println!("        result code {code} ({code:#04x}), not named -- only this value seen");
                println!("        item {first:#018x}{}", match before.iter().find(|(_, g)| **g == first) {
                    Some((slot, _)) => format!("  = the item in slot {slot}"),
                    None => "  -- not a slot this run knows about".into(),
                });
                println!("        item {second:#018x}, trailing byte {}", packet.body[17]);
                println!("        a resolved guid means the body PARSED; this is a refusal,");
                println!("        not a shape the server failed to read.");
            }
        }
        if !said_something {
            println!("      nothing but the usual traffic -- complete silence,");
            println!("      which is a different answer from a refusal.");
        }

        if moved.is_empty() {
            println!("    nothing moved.");
        } else {
            println!("    {} slot(s) changed -- see below.", moved.len());
            break;
        }
    }

    if moved.is_empty() {
        println!("\nneither body shape moved anything.");
        println!("**Read the two attempts against each other rather than alone.** A");
        println!("refusal that names a real item guid says the server parsed the body");
        println!("and declined the request; silence says it did not get that far. If");
        println!("one shape is answered and the other is not, that difference is about");
        println!("the body -- and if the answer changes with what the destination slot");
        println!("holds, the handler is evaluating the request, which is the opposite");
        println!("of an unrecognised opcode. Run this against a real/real pair AND a");
        println!("real/empty pair before concluding anything: they do not agree.");
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
            "-- something moved, but not the two-way swap this was testing for."
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

        let report = state.replicate(&batch, None);
        totals.object_updates += report.object_updates;
        totals.monster_moves += report.monster_moves;
        totals.relayed_moves += report.relayed_moves;
        totals.names += report.names;
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

fn wmo_cmd(chain: &mut Chain, cmd: WmoCommand) -> Result<()> {
    match cmd {
        WmoCommand::Info { path, limit } => wmo_info(chain, &path, limit),
        WmoCommand::Survey { filter, limit } => wmo_survey(chain, filter.as_deref(), limit),
    }
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
        Item,
        ItemDisplayInfo,
        Light,
        LightParams,
        LightIntBand,
        LightFloatBand,
        GameObjectDisplayInfo,
        SoundEntries,
        WorldSafeLocs,
        SpellVisualKit
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
        Item,
        ItemDisplayInfo,
        Light,
        LightParams,
        LightIntBand,
        LightFloatBand,
        GameObjectDisplayInfo,
        SoundEntries,
        WorldSafeLocs,
        SpellVisualKit,
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
