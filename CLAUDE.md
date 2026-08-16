# open-wow-client

Open-source reimplementation of the WoW 3.3.5a (build 12340) client in Rust.
Client only — no server, no bundled assets.

## Where the project is

Phases 1, 2 and 3 are complete: every data format reads, the world renders and
streams, and the protocol reaches a live realm. Phase 4 has started.

**4.15 has begun and it changes the method.** NPC interaction — gossip, quests,
shops — is a *protocol* milestone, not a format one: quest text, menu options
and vendor stock live in the server's world database and reach the client only
when asked for, so "transcribe a table and check a property it must have" mostly
does not apply. Send, watch, confirm by effect. On a test realm the world
database *is* readable and is a source the client is never sent, which makes it
the same class of evidence as `Item.dbc`, and it is what every gossip field was
checked against.

| | State |
|---|---|
| Data formats | MPQ, DBC, BLP, M2 (+animation), WMO, ADT/WDT — all done |
| Renderer | Textures, skinned models, buildings, blended terrain, streaming — done |
| Protocol | **3.1–3.5 done**, all confirmed against a live realm including one client watching another move. Replicated *creatures* slide along their actual path, turn to face it, and play the model's own walk/stand cycles. **Other players do not** — see the known defect below |
| Interface | **4.1 and 4.2 done.** Native, fully customisable, no addons — see the decision below. Player and target unit frames, click-to-target with an in-world bracket, a chat window you can type in, real names, a spellbook you arrange the bars from, `F1` to rearrange, saved to `ui.toml` |
| World | Lighting and the day/night cycle come from `Light.dbc`'s curves and the realm's own clock. **The sky is a real gradient**: bands 2–6 are the sky from zenith to horizon, identified by the one hour that could refute it — at dawn the warm/cool crossing lands on the horizon side, once. Fog is *derived* from the horizon band rather than named, so distant terrain meets the sky it is drawn against. **The sun and the moon are drawn**: band 9 is the one band that stays bright while the sky goes black, so it is the disc rather than a light — cool white all night, warm at dawn — and one band serves both because only one is ever up. **Weather works and now falls**: `SMSG_WEATHER` blends towards the stormy curves *and* rain or snow comes down, as camera-relative billboards with no particle buffer at all, and a storm puts the sun out. Still no skybox (Elwynn names none), no clouds, no stars, no M2 emitters. Game objects — doors, benches, chests, ships — are drawn |
| Appearance | Humanoid NPCs wear their baked `CreatureDisplayInfoExtra` texture and other players are dressed from their replicated appearance fields, so nothing in a zone renders as a white ghost. The player's own armour is painted on from `ItemDisplayInfo`'s eight body components. **Other players are dressed too now (`foss-wow#23`)**: their visible-item fields carry item *entries*, not display ids, so `Item.dbc` bridges the two — `PLAYER_VISIBLE_ITEM_ENTRY_HEAD + 2 * slot`, measured the same way `PLAYER_BYTES` was, against two characters wearing five and seventeen items respectively, and confirmed live: a second account's distinctly-dressed character renders in a different outfit from the viewer's own. **The player's weapon is drawn**, and now so is another player's, off the same path: the M2 attachment table parses, and a sword or shield hangs off the hand's animated bone and swings with it. Shoulders, helms and ranged weapons are not, and there is no sheathed state for anyone but the viewer's own character — see below |
| Game | **4.3 done**: three action bars with real icons, keys `1`-`=` with Shift/Ctrl, click-to-cast, the player's own character drawn in third person with its chosen face, beard, skin and haircut, hover tooltips reading real numbers (82% of `Spell.dbc`'s description templates resolve), a cooldown sweep, and a cast bar off `SMSG_SPELL_START`/`SMSG_SPELL_GO`. **4.4 melee done**: swing at a target and be swung at, a named combat log (`You hit Kobold Vermin for 6. Killing blow.`), and a dead unit dimmed in the frames. **A spellbook panel** (`P`) now lists what the character can do and puts it on a bar by click, auto-attack included -- see the note below on why the seeding filter had to reject it. Threat and the corpse *interface* remain (the corpse protocol is done). Quests follow |
| Loot | **Works end to end.** Right-click a body to open it, click a row to take money or an item, and the corpse releases itself once empty -- a client that never releases leaves the body locked to it for everyone else. `CMSG_LOOT` `0x15D`, `CMSG_LOOT_MONEY` `0x15E`, `CMSG_AUTOSTORE_LOOT_ITEM` `0x108`, `SMSG_LOOT_RESPONSE` `0x160`, `SMSG_LOOT_REMOVED` `0x162`, `SMSG_LOOT_CLEAR_MONEY` `0x165` -- every one confirmed by content or by effect. A loot slot is the **server's** index and never a row position: the numbers do not close up when one is taken |
| Sound | **4.14 done.** Zone music and ambience by area and hour, creature attack/wound/death/aggro voices, and weapon impacts -- all from the tables. `SoundEntries`' layout is checked by its filenames resolving in the archive (93%); the zone tables by their ids landing on a sound of the **right type** (99.1%), which validity alone cannot show. `CreatureSoundData`'s 38 columns identified themselves through the *names* of the sounds they reach. No distance attenuation, no crossfade, no spell or footstep sounds |
| NPCs | **4.15 started: they answer.** `CMSG_GOSSIP_HELLO` `0x017B` → `SMSG_GOSSIP_MESSAGE` `0x017D`, parsed whole — menu id, greeting text id, a list of clickable options and a list of quests offered. Confirmed on **three** NPCs picked so the two counts differ (3 options/0 quests, 0/0, 0 options/1 quest), because one sample is nearly all zeroes and any reading survives it. Every field then agreed with the server's own database independently: menu ids equal `creature_template.gossip_menu_id`, the options match `gossip_menu_option` in text *and* icon, quest 783 came with title `A Threat Within`, level 1 and flags 524296. **An option index is the server's id, never a row position** — menu 1291 has four rows and three arrived, numbered 1,2,3 with 0 filtered out and the numbering *not* closed up, exactly like a loot slot. **A menu can now be chosen and a vendor lists its stock**: `CMSG_GOSSIP_SELECT_OPTION` `0x017C` (`{guid, menu id, option index}`) is confirmed by effect -- choosing an innkeeper's `I want to browse your goods.` produces `SMSG_LIST_INVENTORY` `0x019F`, a different opcode carrying stock, which no misunderstood request would cause. Twelve rows of 32 bytes, `8 + 1 + 12*32 = 393` exactly, matching `npc_vendor` in order, each entry paired with the display id `Item.dbc` gives it -- and one of those pairs (2070/6353) was independently confirmed by `SMSG_LOOT_RESPONSE` in 4.13. **The price on the wire is not `Item.dbc`'s `BuyPrice`**: the server applies the buyer's reputation discount first (25→23, 500→475, 2000→1900, i.e. `*0.95` truncated), so a client showing the table's number is silently wrong for everyone not at neutral. The wire is authoritative for price. **Buying and selling work**: `CMSG_LIST_INVENTORY` `0x019E`, `CMSG_BUY_ITEM` `0x01A2` (`{u64 vendor, u32 entry, u32 slot, u32 count, u8 bag}` -- entry *before* slot, counts are `u32`), `CMSG_SELL_ITEM` `0x01A0` (item named by **guid**, not slot, so a request racing an inventory change refuses rather than selling the wrong thing). Both confirmed by effect, and the buy checks the stock list too: a row quoted at 23 took exactly 23 from `PLAYER_FIELD_COINAGE` where the table says 25, and delivered a stack of exactly `buy_count`. Nothing vendor-related is drawn yet and the `UNIT_NPC_FLAGS` bits are still unnamed |
| Quests | **4.16: a quest can be taken, finished and paid for.** The whole loop runs against the live realm on a character created from nothing. **Accepting is confirmed**: `CMSG_QUESTGIVER_ACCEPT_QUEST` `0x0189` put quest 333 into field `0x00a3` — `PLAYER_QUEST_LOG + 1 * QUEST_LOG_STRIDE`, exactly where the measured base and stride say slot 1 goes. **Turning in works**: `CMSG_QUESTGIVER_COMPLETE_QUEST` `0x018A` (`{u64 npc, u32 quest}`) is *answered* — `SMSG_QUESTGIVER_OFFER_REWARD` `0x018D`, 525 bytes of real completion text — and so it bounds the silent `CMSG_QUESTGIVER_CHOOSE_REWARD` `0x018E` (`{u64 npc, u32 quest, u32 reward}`) that follows, the same move that bounded `CMSG_BUY_ITEM`. Which reply arrives is the diagnosis: `SMSG_QUESTGIVER_REQUEST_ITEMS` `0x018B` says the send was understood and the quest is not finished, silence says the opcode, the body or an NPC that does not end it. **The verdict is the log, never a packet**: 783 left `PLAYER_QUEST_LOG` and quest 7 — McBride's follow-up, `PrevQuestID` 783 — appeared in its place, a chain step no misread packet could fake; `character_queststatus_rewarded` and `xp` 0→40 agree independently. **The trap: asking to *read* a quest's scroll can accept it.** `CMSG_QUESTGIVER_QUERY_QUEST` adds a quest carrying `QUEST_FLAGS_AUTO_ACCEPT` `0x80000` to the log before the accept is ever sent, and `quest_template_addon.SpecialFlags & 0x4` ORs that flag in at load time, which is how the entire Northshire chain has it. Only **179 of 9,464** quests do — rare enough to look like a bug, common enough to cover the zone a first end-to-end test reaches for. Nothing is drawn; `SMSG_QUEST_QUERY_RESPONSE` and the two questgiver text packets arrive whole and are reported as lengths, not parsed |
| Inventory | **4.13 done bar looting; a bag square can now be picked up.** A **single combined bag window** (`B`) covering the backpack *and every equipped bag's contents* -- deliberately unlike the original's one frame per bag -- with real icons, stack counts and money; a separate **character panel** (`C`) with the nineteen worn slots, all nineteen named. The slot array, coinage, stack count, container capacity, container contents and the owner/contained pair were all measured against the live realm. Right-click **auto-equips** a backpack item, sending the already-confirmed `CMSG_AUTOEQUIP_ITEM`. Left-click picks a square up and drops it on another -- the same gesture the spellbook uses -- but the drop does not yet send anything, and `HudResponse::move_item` is currently read by nobody. `foss-wow#55` is **not** blocked on finding the right opcode, contrary to the first reading: `0x010B` with a `{dst_bag, dst_slot, src_bag, src_slot}` body is understood and *declined*. The refusal names a real item guid -- the one the leading `(bag, slot)` pair points at, and reversing the slots reverses which guid comes back -- so the body parses. It is also **not** state-independent: two occupied slots get the refusal, a real item against an empty destination gets silence. What is unknown is what result code **59** means. See `SwapItemCandidate`'s doc comment in `crates/world/src/opcode.rs`. |

Roughly 60% of the way to something a person could test by playing. See
`docs/ROADMAP.md` for the milestone ladder and what is deliberately deferred.

**The destination for the next few milestones is a native Questie**, and the
ladder to it is a dependency chain rather than a preference: NPC interaction
(4.15) → quests (4.16) → map (4.17) → minimap (4.18) → Questie's features
(4.19). A quest tracker is a map feature — its whole value is "the thing you
need is over there", and there is no *there* until a map exists. The scoping
fact that matters: **most of Questie's bulk is a workaround for a restriction
this client does not have.** An addon cannot ask the server about a quest it has
not been offered, so it ships a hand-collected database of the entire game. This
client can send the query. **Decided: quest data comes from the server and
Questie's database is not ported** — so quests are never out of date, and are
correct on a private realm with custom content, which is what this client is
developed against. 4.19 is therefore a *presentation* milestone — pins, tracker,
availability colouring — with facts off the wire wherever the wire will answer.
The cost lands on **4.16**, which must build the query layer (`CMSG_QUEST_QUERY`
`0x05C`, the questgiver-status queries, and a cache that treats a missing answer
as *unknown* rather than as *nothing*) because the map has nothing to draw
without it. Doing only what a quest log needs there means writing it twice.

**`CMSG_QUEST_POI_QUERY` `0x1E3` is the find that makes this work.** WotLK
shipped its own quest tracker, so the server already holds the objective map
markers — **8,953 of 9,464 quests (94.6%)** on this realm — and hands over a map
id, an area id and a polygon of points per objective. That is the thing Questie
exists to draw, on the wire, always matching the realm played on. Its constraint
shapes the design: it answers only for quests **in the player's log**, 25 ids per
request. **Reading the realm's MySQL is a verification oracle, not a client
capability** — a player on someone else's server has no DB access, so anything
built on it works only for realm operators. What the wire genuinely cannot
answer is where things are before you have seen them; that is solved by
*recording what the client already streams* (every creature in range, with entry
and position), keyed by realm, which starts empty and fills as you explore. There
is **no enumerate-all opcode**, so no bulk prefetch at launch — and a mass query
at login would repeat the 37-second burst. See `docs/ROADMAP.md`. Reference addons live in the **gitignored `addons-to-port/`**, are read
rather than vendored, and each one's licence is checked and recorded *before* a
port starts; see `docs/REUSE-POLICY.md`'s addon section.

**Buildings are solid.** Walls stop you, floors and stairs hold you up, and
doodads with a collision mesh are obstacles rather than scenery — from the WMO
triangles (including the invisible collision-only ones) and the M2 collision
mesh, which is a separate and far coarser thing than the drawn geometry.
Collision is entirely the client's job: a character walked through the abbey
wall and a *second* client drew it happening, so nothing server-side corrects
it. Known gaps: animation transitions cut rather than blend, and a residual
stutter on stairs that is instrumented but not yet solved.

**Weapons are drawn, and they sheathe.** `Z` draws and stows, attacking draws
automatically, and a stowed weapon goes where `Item.dbc`'s `sheathe_type` says
— a greatsword on the back, a one-hander at the hip. **The server never draws a
weapon for you**: sheathing is a client decision reported with
`CMSG_SET_SHEATHED`, and a whole fight passes without the state moving on its
own. With a weapon out the character holds the matching ready stance rather
than the at-ease idle.

**The UI question is answered: this client draws its own interface and does not
run addons.** Reimplementing `FrameXML` faithfully enough for third-party addons
means reproducing a whole Lua/XML widget system before the first health bar
appears. Instead the interface *is* the customisation surface: every position,
size and colour lives in `%APPDATA%\open-wow\ui.toml`, editable by hand or by
dragging frames in-game. egui is the drawing substrate only — frames are painted
from explicit geometry, so `scale` multiplies every dimension and the appearance
is a function of our `Style` alone. See `docs/UI.md`.

**The two halves have met, the viewer drives movement, and it draws the
replicated world moving.** `wow-viewer --realm-host <host> --user <account>
--character <name>` logs in, enters the world, and streams the map the server
chose around the position it reported. W/S walk, A/D turn, **Q/E strafe, Space
jumps, right-drag steers the character while left-drag swings the camera, the
wheel zooms and Num Lock is autorun** — each sent as a real `MSG_MOVE_*` stream,
with the opcode naming the axis that changed and the flags carrying the whole
state. The camera follows behind rather than flying freely. **Left-click
selects; right-click selects and attacks** — hostility is not yet known
(`FactionTemplate.dbc` is untranscribed), so the client rules out only what is
never a fight and lets the server refuse the rest. Altitude follows the terrain — the keys drive the
two horizontal axes and Z is read back out of the height field the ground is
drawn from, so the character walks over hills rather than into them. `LiveWorld` keeps a `world::WorldState` alongside
the connection and folds every drained packet into it, so creatures slide along
their actual path instead of jumping between snapshots or standing wherever they
were at login — turning to face the way they're moving, playing the model's own
walk cycle in motion and its stand cycle at rest, all re-evaluated every frame.

**Known defect: this is true of creatures and not of other players.** A creature
moves by `SMSG_MONSTER_MOVE`, which carries a start, an end and a duration, and
that is what `interpolated_position` was built for. A player moves by relayed
`MSG_MOVE_*`, which carries a position and no path at all, so
`update_movement` stores it and clears any prediction — the player snaps from
packet to packet and, having no duration, reads as `speed: 0.0` and never leaves
the stand cycle. Two symptoms, one cause, and a live report was what surfaced
it.

**Why 3.5's two-client test missed it is the more useful half.** Both clients in
that test were *this* client, which heartbeats every 100ms; a hundred
milliseconds of snap between two nearby points reads as movement. A real client
sends roughly every 500ms, and at that spacing the same bug is unmistakable —
which is exactly how it was reported. Two copies of our own client agreeing is
the weakest form of the two-client rig, and this is the first time that has cost
anything: the rig proves a *format* travels both ways, and proves nothing about
timing that both copies share. See `foss-wow#22`. Verified with two clients, one walking while the
other, running the real viewer, drew it happen; four real bugs in that
drawing path (an animation that never went idle, a whole species animating
because one instance of it moved, entities facing a constant wrong direction,
motion that stuttered once animation ran faster than position updates) were
only found by watching it live — see `docs/ROADMAP.md`'s 3.5 section.
`wow-cli world --enter X --walk 20` remains the CLI-driven equivalent, useful
when no window is available.

On top of that there is now an interface: a player frame and a target frame
drawn from replicated fields, a left click that casts a ray through the cursor
and sends `CMSG_SET_SELECTION` for whatever it hits, and `F1` to drag the whole
layout around and save it. The fields those frames read are confirmed against
the live realm via `wow-cli world --units` — `Testwolf` reads as a rage user
with `0/1000`, not the `0/0` mana a mis-indexed power array would give, and a
second account's player replicates the same way. Watched live too: overlapping
creatures each select deliberately, a bracket of corner ticks marks the
selection out in the world, and left-drag swings the camera around the
character. Two bugs came out of that look and out of nothing else — no
in-world selection marker at all, and a camera whose yaw was written by a drag
and overwritten by the follow code a millisecond later.

4.2 added chat and names on top: `Enter` opens a line to type in (and takes the
keyboard away from movement while it is open), the scrollback colours by kind,
and frames say `Young Wolf` rather than `Creature 299`. Verified across two
clients — `Watcher` on `ACCOUNT34` whispered `Testwolf` on `ACCOUNT33` and it
arrived — plus 50 names resolved from 50 queries with none unanswered. Watched
live, and for the first time in this phase the look found **nothing**: the
typed line went out and came back exactly once, typing did not walk the
character, and a whisper from a player who was never in visibility range
resolved from a bare guid to their name retroactively. That is not extra care;
it is that 4.1's live bugs had been converted into headless checks (a paint
assertion, and received chat logged as well as drawn), and the one bug 4.2 did
have was caught by reading the viewer's own log rather than by looking at it.

## Orientation

**All nineteen equipment slots are named, and a bag's contents are readable.**
Both were open gaps and both closed the same way — see the note below on
changing the character rather than the technique. Wearing one item of each kind
and recording where the *server* put it named slots 0–16 and 18, two of which
independently agree with the starting-gear prediction that identified the array's
base. Slot 17 held out because every ranged item is refused to a warrior; a
**dwarf hunter** is created wearing a gun *and* an ammo pouch with shot in it,
which named it `Ranged` and produced the first non-empty container this project
had ever seen. A bag's contents live at `CONTAINER_FIELD_SLOT_1 = 0x42`, guid
pairs at stride two, and `ITEM_FIELD_CONTAINED = 0x08` distinguishes an item in
a bag from one held directly.

- `crates/` — one library per concern: `chunk` (shared chunked container),
  `mpq`, `dbc`, `blp`, `m2`, `wmo`, `adt`, `render`, `auth`, `world`, `ui`
  (the player's interface; depends on neither `world` nor `render`, so it is
  testable without a connection or a GPU) and `collision` (solid-world
  queries; pure geometry, so likewise testable with a hand-built box)
- `tools/wow-cli` — inspection CLI. **Every format gets a dump command here
  before it is wired into the renderer**, and a `survey` command that parses the
  whole archive set. Those surveys have caught every systematic parser bug so
  far.
- `apps/viewer` — windowed viewer. `--screenshot` renders one frame headless to
  a PNG, which is how render output is checked without a display.
- `docs/` — `ROADMAP.md`, `RENDERING.md`, `PROTOCOL.md`, `UI.md`,
  `REUSE-POLICY.md`, and `formats/*.md` recording what each format actually
  does and where it bit us.

## Local setup

- Source lives on an SMB share (`N:`), which cannot execute binaries. The
  gitignored `.cargo/config.toml` redirects `target-dir` to local disk; without
  it every build fails with `Access is denied (os error 5)`.
- Reference installation: `D:\Games\World of Warcraft 3.3.5a` (verified 12340,
  enUS, 17 archives, 203,949 paths). 1.12.1 and 2.4.3 are also on disk for
  format-evolution comparison.
- `WOW_DATA` supplies `--data` to `wow-cli` and gates the integration tests.
- Test realm: **`wow1.nekos.farm`** (auth 3724, world 8085), realm `NekoCore`
  at `108.174.48.199:8085`, realm id 1. Accounts `TESTER`, `ACCOUNT33` and
  `ACCOUNT34` exist. **Passwords are deliberately not recorded here** — this
  file is committed. Ask the user, and pass the password via `WOW_PASSWORD`
  rather than an argument. A wrong password and a missing account are hard to
  tell apart, so guessing wastes real time.
- **Three NPCs are deliberately left standing at `Testwolf`'s login spot** on
  the local realm: an Innkeeper Farley (entry 295, npcflag 66179), a Marshal
  McBride (197, flag 3) whose quest chain is gated behind a prerequisite, and a
  Deputy Willem (823, flag 3) whose is not. That combination is what made the
  gossip packet's two variable blocks separable — one NPC with options and no
  quests, one with neither, one with a quest and no options — and it is the
  fixture the rest of 4.15 reads from. `.npc add <entry>` rebuilds it.
- Two accounts exist so that **two clients can be online at once**, which is the
  only way to test anything about one player observing another — relayed
  movement, entity replication. A single account cannot prove any of it.
- `ACCOUNT33` also has `Facetest` (human warrior), created deliberately with
  **five different non-zero appearance values** — skin 3, face 5, hairstyle 7,
  hair colour 2, facial hair 4. Every other character here was made with the
  all-zero default, and an all-zero appearance makes any search for it match
  every zero field in the object, which is how two attempts at locating
  `PLAYER_BYTES` settled nothing. Keep it: it is the only character on either
  account that can distinguish a field from its neighbours, or show hair at all.
- `ACCOUNT33` has two characters, `Testwolf` (human warrior) and `Testdruid`
  (night elf druid), created to give `SMSG_CHAR_ENUM` real data to parse. An
  account with no characters exercises none of that packet's field offsets.
  `ACCOUNT34` has `Watcher` (human warrior), deliberately a human so it spawns
  in Northshire within view range of `Testwolf` — two clients in different
  starting zones cannot see each other and prove nothing.

### Local AzerothCore realm (127.0.0.1)

A second realm runs entirely on this machine, from `C:\azerothcore-wotlk`
(`docker compose up -d`). **Prefer this over `wow1.nekos.farm` for anything
needing a specific game state** — against the remote realm every death cost a
five-minute fight and permanently consumed a character's state; locally a
death and a resurrection are each one GM command, so the same scenario runs
twenty times.

- Auth `3724`, world `8085`, MySQL `3306`, SOAP `7878`. Realm `AzerothCore` at
  `127.0.0.1`.
- Accounts, all GM level 3: `OWC33`/`owc33` (`Testwolf`, `Facetest`,
  `Questtest`, `Questtwo`), `OWC34`/`owc34` (`Watcher`, `Huntertest`),
  `OWCADMIN`/`owcadmin`.
- **A quest test needs a character that has never held the quest, and one is
  not reusable.** `Questtest` has completed 783 and cannot retake it —
  `.quest remove` clears the log but *not*
  `character_queststatus_rewarded`, so the server keeps declining to offer it.
  `Questtwo` is virgin and shows the auto-accept path (log `[]` at login,
  `[783]` after the scroll alone). Creating another is one `--create` and is
  the right move rather than trying to reset one; the same lesson as the dwarf
  hunter that closed two gaps a whole milestone of cleverness could not.
- **Quest 333 "Harlan Needs a Resupply" (questgiver entry 1427) is the
  accept fixture**, because its `Flags` and `SpecialFlags` are both zero, so
  nothing but `CMSG_QUESTGIVER_ACCEPT_QUEST` can put it in the log.
  `.npc add 1427` puts the questgiver at your feet.
- **Unlike `wow1.nekos.farm` above, these passwords are fine to commit.** The
  server is local, disposable, and reachable only from this machine, which is
  the opposite of the remote realm's rule two paragraphs up — a reader needs
  to know which one applies before typing a password into a file.
- **GM commands travel as ordinary chat.** `--say ".die"` works from our own
  client for a GM account — `ChatHandler.cpp` parses any message starting
  with `.`. `.die` additionally needs a target, hence `--select-self`.
- **SOAP on `7878`** drives the server from a script with no game session at
  all, for setup that should not depend on a client being logged in.
- Reading the AzerothCore source in that tree is authorised, and rule 2 below
  already permits it — source makes a hypothesis about a packet or a table
  cheap to form, but observation still has to confirm it.
- Two setup failures already cost time and are worth not re-diagnosing: a
  stale cached `:master` image expecting `VMAP_4.7` against `VMAP_4.8` data
  (fix: `docker compose pull`), and an old database missing the RBAC tables
  (fix: a fresh volume — `AC_UPDATES_ENABLE_DATABASES=0` is baked into the
  image on purpose, because `ac-db-import` owns migrations and the two must
  not race).
- A cold snapshot of the previous database lives at
  `C:\azerothcore-wotlk\var\db-snapshot\ac-database-cold-2026-08-13.tar.gz`.
  **Do not delete it** — the live database was deliberately recreated fresh
  and that file is the only remaining copy of what was in it.

## Rules that matter

1. **Never commit game assets** — not as fixtures, not as test data. Tests
   needing real data read `WOW_DATA` and skip when unset.
2. **No GPL code in the tree.** TrinityCore/MaNGOS may be read to understand a
   field's meaning; implementations are written from public documentation.
3. **WoW-specific formats are implemented in-tree.** Generic plumbing (codecs,
   GPU, windowing, math, crypto primitives) comes from crates.io. The test:
   would this dependency exist if WoW had never been written?
4. **Surveys are the regression net.** `wow-cli verify`, `dbc check`,
   `blp survey`, `m2 survey`, `wmo survey`, `adt survey` each parse everything
   of their kind; a systematic error shows up as one large bucket rather than
   scattered noise.

## How this project finds bugs

Worth reading before debugging anything, because the same shapes keep recurring.

- **A wrong field offset parses perfectly and returns nonsense.** Check
  properties the data must have, not just that it decoded: M2 normals are unit
  vectors, SRP6 rotation keys are unit quaternions, terrain chunks must meet at
  their edges. Each of those caught a real bug that size checks missed.
- **"Could this column mean X" is the wrong question; "is it set *because* of
  X" is the right one.** Finding which of `Spell.dbc`'s 234 columns holds a
  duration by asking which one contains a valid `SpellDuration` id gave a
  99.6% match — on the wrong column. Any column of small integers points
  somewhere inside a 130-row table, so validity is nearly free and proves
  nothing. Comparing the spells whose description says `$d` against those that
  do not immediately separated the real column: non-zero 98.5% of the time
  versus 39.0%. The same reframing found every other column here, and the one
  test that came back flat had been asked the sloppy version of its question
  (every description *mentioning* `$m1`, rather than only those quoting a
  range, which is when a die is actually needed). A property test is only as
  good as the population you run it against.
- **A number nobody can check is worse than a blank.** A wrong field offset
  eventually fails loudly; a wrong *number* on a tooltip never does. So the
  description substituter resolves only the tokens whose columns were
  confirmed against the data and passes everything else through with its `$`
  intact — a visible `$s1` says "not implemented", a fabricated `47` says
  nothing and is believed. Same rule as `describe_cast_failure` naming one
  status code, one layer up.
- **Print the body, not the length, of anything you refuse.** A parser that
  declines an unconfirmed shape is only useful if the shape survives the
  refusal. `SMSG_ATTACKERSTATEUPDATE` arrived four bytes longer than any packet
  seen before, the cursor caught it as trailing bytes -- and the tool logged
  the *length* and dropped the bytes, so the one packet that could have
  answered the question was seen and lost. Two separate tools here had the same
  hole. If a parser's own doc comment says "a capture would settle this", then
  something has to be keeping captures.
- **A reply you cannot get is not the same as a reply you did not earn.**
  `CMSG_ATTACKSWING` could not be read off a capture -- nothing acknowledges an
  opcode, and an outgoing number that is wrong gets read as some *other* valid
  request rather than refused. Sent from out of range and facing the wrong way
  it produced two empty-bodied refusals and no damage, which looks exactly like
  a wrong opcode. The proof was not that a reply came, but that the reply
  *changed when the conditions did*: closing to melee and turning to face
  turned those same refusals into an attack-start and fifteen swings. When
  nothing can confirm a send directly, find the input you can vary and check
  the output varies with it.
- **State that persists needs accounting, not just parsing.** Every parser here
  is memoryless: a bad packet gives one wrong answer and the next is unaffected.
  Replicated world state is not — a dropped update is permanent, a merge that
  overwrites erases fields nothing will resend, a missed removal leaves a ghost.
  None of it errors and all of it compounds. Count every change, tally updates
  naming unknown objects instead of inventing them, and check the books balance
  (`created - removed == held`). Those counters catch replication bugs long
  before the world looks wrong, and none of them assert anything about layout.
- **Assert the parse consumed the whole record.** The corollary to the above,
  and cheaper than any of it. Four separate world-protocol bugs — a packet
  sixteen bytes longer than expected, three missing equipment slots, a
  result-code enum off by one, a position block read as nine floats instead of
  eight — were invisible field by field and obvious the moment a cursor reported
  leftovers. Parse through a cursor and make running out of input *and* having
  input left over both errors.
- **The hard-looking part is rarely the expensive one.** SRP6, the RC4 header
  cipher and the update-field bit-packing all worked close to first time: they
  are precisely specified and fail loudly. Every hour actually lost went to
  ordinary struct layout, where a wrong guess parses perfectly. Budget for the
  boring parts.
- **Check that your check is current.** The first walk was declared a failure
  because the character list still showed the old position — but the character
  list reports the last *saved* position, which lands tens of seconds after a
  disconnect, while `SMSG_LOGIN_VERIFY_WORLD` reports the live one. The movement
  had worked all along. When a change appears not to have taken, confirm the
  thing being read is the thing being written.
- **Give the other end time to act before concluding it ignored you.** A single
  packet sent immediately before disconnecting is often never processed, and the
  result is indistinguishable from having sent the wrong thing. A facing opcode
  was briefly written off as wrong on exactly this evidence; half a second of
  waiting made it work every time.
- **Writing a format is riskier than reading it.** A bad read fails loudly at a
  known offset; a bad write is accepted as some other valid message and shows up
  as wrong behaviour far away. Where a structure travels both ways, define it
  once and round-trip it — two copies of a conditional layout can drift, and the
  outgoing copy has nothing to announce the drift.
- **When a send produces no reply at all, inventory what *did* arrive before
  improving your guess.** Sending chat failed silently three times and looked
  identical every time: packet out, session alive, nothing back. The causes were
  a language an ordinary account may not speak, an enum where `0` is `SYSTEM`
  and `1` is `SAY`, and — twice — our own tooling receiving the reply and
  discarding it. Each round of guessing at the layout was wasted; the moment
  `wow-cli world --stay` printed every opcode seen, decoded or not, the answer
  took one run. "The server never sent it" and "it arrived and we could not read
  it" are the same observation until something separates them, and they want
  opposite investigations.
- **Convert every live-only bug into a check that runs without a window.**
  3.5 and 4.1 each cost a handful of bugs that no test could have caught. 4.2
  cost none, and the difference was not care: 4.1's failures had been turned
  into a headless egui pass asserting a frame painted where the layout put it,
  and into logging received chat as well as drawing it. 4.2's one real bug —
  a chat line stamped with a guid before its name resolved — was then found by
  *reading the viewer's log*, a step earlier than looking. Live testing does not
  stop mattering; each live bug just stops recurring for free.
- **A limit that bounds packets does not bound time.** The login burst drained
  until the stream went quiet *or* 512 packets arrived. Northshire emits a
  monster move fourteen times a second and is never quiet, so the drain ran
  until it had its 512 -- **thirty-seven seconds**, before the client drew a
  single frame. Nothing was wrong with the drain; its contract simply had no
  clock in it. Any "read until N or until idle" loop against a live stream
  wants a wall-clock budget too, and the chunk size then sets how far past that
  budget it can overshoot.
- **Measure the thing, not the thing next to it.** That same delay presented as
  the action bar filling half a minute after login, and the confident diagnosis
  was a slow `Spell.dbc` read blocking the render thread -- with a plausible
  argument attached (two runs agreed to the second, so it must be a fixed cost
  rather than network jitter). It was wrong. The DBC load takes 185ms; the
  spellbook had been sitting at the end of a burst that took 37 seconds to
  finish collecting. One timing log around the suspected culprit settled in one
  run what reasoning had got backwards.
- **Do not transcribe a table you have not verified — especially one that only
  produces text.** A wrong field offset eventually fails loudly; a wrong *name*
  for a status code never does. It confidently misexplains what happened and
  sends the next reader somewhere else. `describe_cast_failure` therefore names
  exactly one reason, the one actually observed against the realm, and returns
  the raw number for everything else. The urge to fill in the whole enum from
  memory is the same urge that produced `CHAT_MSG_SAY = 0x00`.
- **One dispatch table does not save a caller from ignoring what it produces.**
  `WorldState::replicate` is deliberately the only place opcodes are dispatched,
  and that is still right — but chat is *returned* rather than stored, and three
  separate callers quietly dropped it. A two-client test then showed chat never
  being delivered when it had arrived and been thrown away. Centralising the
  producer does not centralise the consumers.
- **Not every failure is a bug.** The world connection dropping after three
  keepalives was the server enforcing a *minimum* ping interval — pinging too
  eagerly is punished harder than not pinging. It surfaced as an unexpected end
  of stream, which is indistinguishable from a desynchronised cipher. Before
  suspecting corruption, ask whether a rate limit or anti-abuse rule was tripped.
- **But derive from the *same* source when two things must agree exactly.**
  The opposite-sounding rule, and both are right about different situations.
  Independence is evidence when you are checking whether something is
  *correct*. It is a liability when two things must *stay* consistent: the
  picking ray is unprojected from the very matrix the scene is drawn with, not
  rebuilt from the camera's angles, because those two agree only until someone
  changes the projection — and a ray that is off by a little lands clicks on
  the creature *beside* the one under the cursor, which reads as the server
  disagreeing about positions rather than as a stale copy of a matrix. Same
  reasoning as defining a both-ways structure once and round-tripping it.
- **Two copies of your own client are not two independent derivations.** The
  two-client rig is this project's strongest shape *for formats*: a structure
  goes out through one client and back in through another, so the write and read
  halves are confirmed via a third party. It proves nothing about behaviour the
  two copies share. 3.5 declared replicated players smooth on exactly that
  evidence, and they were not — both clients heartbeat every 100ms, and a
  hundred milliseconds of snap between nearby points reads as movement. A real
  client sends every ~500ms and the same missing interpolation is obvious. When
  the thing under test is *timing* rather than layout, one of the two ends has
  to be something you did not write.
- **Compare against something derived independently.** The SRP6 tests carry a
  server written from the protocol, not from the client. Agreement between two
  separate derivations is evidence; a thing checked against itself is not.
  The strongest version of this available here is the two-client movement test:
  the structure goes out through one client, through the server, and back in
  through another, so the write and read halves are confirmed against each other
  *via a third party* that had to understand both. Reach for that shape whenever
  a format travels in both directions.
- **Two bugs can share one symptom, and you will fix the innocent one.**
  M2 geometry drawn with the wrong winding culls front faces, which does not
  look like missing geometry -- it looks like a model *facing away from you*,
  because what survives is the interior of its far surface. On that reading a
  half turn was added to entity facing, then the same wrong reasoning was
  propagated to doodads. Neither rotation was ever wrong. What separated them
  was fixing the winding first and then A/B-ing the rotation live, one
  variable at a time, with the person at the window pressing the key. When a
  symptom persists across a fix that should have worked, suspect that it has
  two causes rather than that the fix was too small.
- **And one bug can produce several unrelated-looking reports.** The mirror of
  the rule above, and it costs the same way. A character sinking into the
  ground, a click marker landing off-centre, hills that could not be walked up,
  and another client seeing this one twitch were four separate complaints, none
  of which said "altitude" — and they were one missing feature. The click
  marker in particular reads as a picking-ray bug, because the ray starts at
  the eye and the eye is a fixed offset above a position whose Z was wrong.
  Before opening the second investigation, check whether the first cause
  reaches it.
- **A composite needs a way to be seen as itself.** A dressed character looked
  bare-chested at walking distance and the obvious diagnosis was that the torso
  region was wrong. Dumping the composed 512x512 skin to a PNG showed all ten
  regions correct and the torso wearing a white shirt that simply reads as skin
  at three hundred pixels. The render was right and the *look* at it was wrong,
  which is the inverse of the usual failure here and just as expensive. Anything
  assembled in memory from a dozen files gets a dump command.
- **A trap documented at one call site does not protect the next one.** That
  the server never relays our own movement back — so replicated state holds
  our *login* position forever — is written up at length in
  `live::drawable_entities`, which is the function that **draws** the player.
  It was then walked into immediately by a function that **aims at** the
  player: resolving "face this guid" through replicated state made every
  creature attack the spot the character logged in at, drifting further wrong
  the further they walked, until the player could stand behind a creature
  supposedly fighting them. Same fact, different consumer, and the comment was
  in the wrong place to help. When a fact about the data is surprising enough
  to document, put it on the *data* — an accessor or a type — not on the first
  caller that tripped over it.
- **A packet is a statement made once; a field is a statement that stays
  true.** Creature facing was first driven off `SMSG_MONSTER_MOVE`'s facing
  block, which the server sends only when it decides a creature has turned.
  The result was a wolf that turned *only when the player moved*, because that
  is what prompts the server to re-issue one. `UNIT_FIELD_TARGET` says who a
  unit is fighting for as long as it is fighting them, so deriving the heading
  from it tracks continuously and for free. When behaviour should be
  continuous, prefer the replicated field over the event that last changed it.
- **A rate limit and a lag are different failure modes, and only one of them
  is bounded.** Easing creature turns at a fixed maximum rate looks right in
  every single frame, and is fine while the target's angular speed stays under
  the cap. Angular speed is `v / r`, so a player circling at melee range
  exceeds any cap chosen to look unhurried — and past that point the error does
  not settle at "somewhat behind", it grows without limit until the creature
  faces nowhere near its victim. Closing a *fraction* of the remaining error
  instead bounds the lag at `omega * tau` for any `omega` at all. Whenever a
  smoothing constant is a maximum speed, ask what happens when the input
  exceeds it.
- **A fallback can invalidate the rule that was written beside it.** Combat
  animations fall back to plain `Stand` on a model with no attack or ready
  cycle — which a wolf genuinely lacks. That fallback then broke the
  *clamping* rule sitting next to it: "plays once, so hold the last frame" was
  keyed on the state that asked, and a Stand frozen at its final frame is a
  statue. The same fallback broke it the other way too — `Dead` resolves to
  the *fall* on those models, which must hold rather than loop, while carrying
  no start time to clamp against. Holding had to become a property of the
  animation that resolved rather than of the state that requested it. When a
  fallback is added, re-ask every question that was answered in terms of the
  thing being fallen back from.
- **Copying a mechanism is the cheapest way to audit it.** Right-click needed
  the same press/release distance test the left button had used since 4.1, so
  it was written by mirroring it — and mirroring it surfaced a bug four
  milestones old. The left button cleared `last_cursor` on release, and the
  *next* press reads its own start position out of that same field, so a
  second click at the same pixel was silently discarded. It survived because a
  selection is not a gesture anyone repeats; right-click-to-attack is, and
  exactly when it appears not to have worked. Reusing a mechanism in a second
  place asks questions of it that the first place never did.
- **A grep that finds the field names is not a grep that finds the
  structure.** Reading AzerothCore for the jump block's field order turned up
  `sinAngle, cosAngle, xyspeed, zspeed` in `MovementHandler.cpp` — a different
  order from this project's `Falling`, and it looked exactly like a bug worth
  fixing. It was a different packet. The canonical `MovementInfo` codec in
  `WorldSession.cpp` reads `zspeed, sinAngle, cosAngle, xyspeed`, which is what
  we already had. "Fixing" the correct one would have been silent, and the
  source would have been blamed for it. When source makes a hypothesis cheap,
  confirm you are reading the *definition* and not one of its users.
- **A rule can be right and still exclude the one thing you need — and then
  the test has to assert both halves.** 4.3 established that a spell earns a
  bar slot by belonging to the character's own skill line, because every
  internal effect (`Opening`, `Duel`, `Honorless Target`) sits on
  `SkillLineAbility`'s generic line 183 with a class mask of zero. `Auto
  Attack` sits on line 183 with a class mask of zero. So the mechanism that
  correctly keeps the junk off a bar necessarily hid the one ability every
  character uses, and the rule was not wrong — merely complete. Two fixes
  present themselves and only one is right: widening the rule to admit line
  183 readmits all the junk, where naming the single spell admits exactly what
  was checked. The trap is in the *test*: asserting only that auto-attack is
  admitted passes just as well under the wrong fix, so the check has to assert
  the junk beside it is still refused. Whenever an exception is carved into a
  filter, test the exception **and** the thing it is indistinguishable from.
- **A column can be named correctly and still not mean what its name says
  here.** Every held item in the game -- main-hand swords included -- stores
  its geometry in `ItemDisplayInfo.model_left`, and `model_right` is empty.
  The obvious conclusion is that the two columns are swapped, and it is wrong:
  shoulders fill both and put `LShoulder_...` in one and `RShoulder_...` in
  the other, which proves the names. The pair is really "first model, second
  model", and only a genuinely *paired* item uses both, so a single-model item
  sits in the first column whichever hand it belongs in. Reading the column as
  the hand would have put every weapon in the game in the wrong one, silently.
  When a column's name suggests an answer, find the rows where the name is
  unambiguous -- the pairs, the extremes -- and let those define it.
- **An error that scales with the value hides in the half of the range you
  look at.** `Light.dbc`'s colour bands are *display* bytes and an sRGB render
  target re-encodes whatever a shader writes to it, so every one of them was
  brightened on the way to the screen — 49 arriving as 123. It had been true
  since lighting existed and no daylight render ever showed it, because near
  the top of the range the curve is nearly flat. What showed it was **midnight
  over Elwynn coming out a bright afternoon blue**. When a transform's error
  vanishes at one end, testing at that end proves nothing; go to the other.
- **Correcting half of a matched pair is worse than correcting neither.** The
  same double-encode applied to the sky, the fog and the diffuse light. The
  sky's fix was unarguable — it is written straight to the target — and the
  diffuse's was not, because it multiplies textures already decoded to linear,
  so what space it belongs in is a real question rather than a slip. Fixing
  only the certain half left the world bright under a dusk sky, and the report
  that came back was not "the lighting is wrong" but *"the sky looks like
  night even if it is bright"* — a complaint about the half that was correct.
  The derivation then settled it and both answers agreed. If two values are
  compared by eye, they must be converted together or not at all.
- **Ask the hour that can refute you.** Five sky bands look like a plausible
  gradient at noon under almost any ordering, so agreeing with one is nearly
  free. At dawn a sky is not a ramp: the warm half arrives at a definite height
  and the crossing has a *side*. That is what identified the bands, and it is
  the same move as comparing `$d` descriptions against the rest rather than
  asking whether a column contains valid ids. A test that cannot come out the
  other way is not evidence.
- **An instrument that fails quietly costs more than one that fails.**
  `--hour` parsed, promised in its help text, and did nothing without a realm —
  an offline screenshot silently got the fallback gradient, which is a
  perfectly plausible sky. Several minutes went into studying a picture that
  had never consulted the tables. A flag that cannot act should say so, not
  render something believable.
- **A green suite is a claim with a date on it.** `cargo test` was not green at
  `HEAD` despite a handoff saying so: 4.8's global-sequence fix had invalidated
  a test's premise ("a sequence index past the end has no keys anywhere" — a
  global track has no sequence to be outside of). The behaviour was right and
  the test was describing the old bug. Run it before believing it, and when a
  fix invalidates a test, the rewrite has to assert **both** halves — that the
  ordinary case still holds *and* that the exception genuinely differs — or it
  passes just as well after a regression to the original bug.
- **And the suite can fail without any test failing.** The sequel, one
  milestone later, and it cost sixteen minutes before it was even recognised as
  a hang. `cargo test --release` stalled at `HEAD` on a tree whose handoff again
  said it was green: the eleven GPU tests each built their own `wgpu` device,
  the harness runs them on separate threads, and eleven concurrent DX12 device
  creations deadlocked — thirty-seven threads with six seconds of CPU between
  them, indefinitely. Single-threaded the same eleven pass in under six seconds.
  A shared `OnceLock` device fixed it *and* made them eight times faster. Two
  things worth keeping: it is a **race**, so the previous green run was luck
  rather than evidence — and the tempting fix, `RUST_TEST_THREADS=1`, is a
  workaround living in an environment variable, which is precisely where nobody
  applies it on the run that matters. When a test needs a process-wide
  resource, build it once for the binary. Also: a hang is not a slow run, and
  the tell is CPU time — six seconds across thirty-seven threads in sixteen
  minutes is a deadlock, not work.
- **A property test is only as good as the population, and 200 irrelevant rows
  will bury two decisive ones.** Asking whether `Light.dbc`'s storm column is
  really the storm column across every outdoor light came back flat: darker 55%
  of the time, foggier 47%, a coin flip that read as a refutation. Most
  positioned lights are decorative — a glowing crater, a haunted wood — and
  their weather columns are authored for effect. The row that *matters* is the
  one that lights a zone, and there the answer is unambiguous: map 0's default
  storm row is a flat neutral grey at every hour of the day with the fog
  distance nearly halved. Before believing a flat result, check that the
  population can answer the question.
- **Assert a property at the point where it exists.** The same weather work
  asserted that a storm is *greyer* than clear weather and failed — at noon,
  where clear light is already a perfectly neutral 0.71/0.71/0.71 and nothing
  can be greyer than grey. The property is real at dawn, where the sun has a
  colour for the storm to take away. The test was wrong, not the data.
- **A field can be parsed, documented, and then ignored by the one function
  that had to act on it.** `Track::global_sequence` was read off the wire and
  carried a doc comment saying it "runs on a shared global timer rather than
  the current sequence" — and `sample` indexed `sequences[sequence]` anyway.
  Global tracks hold one keyframe list, so they resolved only when the sequence
  index happened to be zero, which is `Stand`; every other cycle silently fell
  back to bind pose. Invisible for four milestones, because a body bone
  snapping to bind in one cycle is nothing anyone would spot. It took a
  *sheathed sword flying off a character's back* to make a bone's orientation
  something you could see. When a struct field exists to change behaviour,
  check that something reads it.
- **When a measurement contradicts a conclusion you trust, suspect the
  instrument.** The attachment points chosen for stowed weapons came back as
  the *least* stable of all thirty-nine across animations — which looked like
  proof the identification was wrong, and nearly caused it to be redone. The
  identification was right; the sampler was broken. Two independent arguments
  had already agreed on those points, and a third measurement disagreeing with
  both is evidence about the third.
- **When looking cannot settle it, find the thing that moves.** A greatsword
  slung across a back has two mirror images, and *both* look exactly like a
  greatsword slung across a back — the placement-rotation trap over again, and
  a render was never going to break the tie. What was asymmetric was not the
  picture but the **animation**: character models carry a cycle named `Sheath`
  in which the hand travels to wherever the weapon is stowed, and tracing that
  hand showed it passing two to three times closer to one candidate than the
  other, on three races. When two static candidates look equally right, ask
  what *moves* between them.
- **A wrong constant is right for whatever it was written against.** The
  attachment sanity check capped offsets at 100 units, which is generous for a
  character and absurd for a hundred-and-fifty-unit falling tree whose
  perfectly good attachment sits at Z=127. The check was not too strict or too
  loose; it was measured against the wrong thing. A threshold that scales with
  its subject — here the model's own declared extent — cannot make that
  mistake, and this is the second time a fixed limit has been the bug (see the
  turn-rate cap above).
- **When every measurement says it is right, stop measuring and move.** A
  weapon that would not appear on screen produced four clean diagnostics in a
  row: the item resolved, the group was built, the transform put it exactly at
  the hand, the model rendered fine alone. Nothing was wrong. The camera sits
  behind the character and a blade held forward at hip height is entirely
  behind its owner from there. One render from the side settled it. The
  sibling of "a composite needs a way to be seen as itself", and the tell is
  the *pattern*: diagnostics that keep coming back correct are evidence about
  the observer, not the code.
- **Validity is nearly free; *variation* is the discriminator.** Two update
  fields both resolved 100% to real `GameObjectDisplayInfo` rows, because the
  table is 39% dense and any small integer lands in it. One was the constant 33
  -- the type mask -- and would have drawn thirty-two identical powder kegs;
  the other took seven values that came out as inn benches in the abbey the
  player was standing in. When a candidate column and a control both look
  valid, ask whether the candidate *varies the way the thing it names varies*.
- **Listing a directory and reading a path are different questions.** An MPQ
  resolves by hash, so a file absent from `(listfile)` still reads perfectly.
  A coverage check for the baked NPC textures built on `wow-cli ls` concluded
  0.1% of them shipped and would have sunk the approach; resolving forty random
  names by path got forty hits. When a cheap check says a whole feature is
  impossible, confirm it answered the question you asked. **A tool's own
  default limit is the same trap wearing a different hat**: `m2 anims` lists
  thirty of a hundred and fifty-six sequences, and a search for the sidestep
  cycles came back empty from a list that stopped at index 29 — concluding that
  no character model had one and that the bug was not a bug. They are rows 38
  and 39.
- **An absent update field is a zero, not an unknown.** An object-create block
  carries only non-zero values, so a player with the default appearance has no
  `PLAYER_BYTES` field at all. Treating absence as "not known" left exactly the
  plainest-looking players white -- the bug the field had just been added to
  fix. The rule generalises: for a sparse field set, missing and default are the
  same statement, and only a dropped *object* means unknown.
- **When geometry is missing rather than wrong, suspect culling before data.**
  WMO winds counter-clockwise, M2 and terrain clockwise. Guessing from a
  neighbouring format culled a roof and looked like a hole in the mesh.
- **Geometry drawn at zero size looks exactly like geometry never drawn.**
  This one recurred, in a second place, years of commits later. A bone palette
  is a fresh GPU buffer, and a fresh GPU buffer is zeroed, and a zero matrix
  multiplies every vertex to the origin -- so a palette created and never posed
  collapses its model to a point in total silence. `--screenshot` placed every
  replicated creature and never called `update_animations`, so a headless
  render of a zone with ninety-five creatures in it came back as empty grass,
  and had done since the feature was written. Nobody noticed because 3.5 was
  verified by watching a *window*, where the frame loop does pose them. The
  buffer now initialises to identity, so the same mistake draws a bind pose --
  visibly wrong instead of invisibly absent. **Anything that must be written
  before it is read should start as something you can see.** A
  bone index past the end of the palette reads zero on the GPU, collapsing the
  model to the origin with no error anywhere. Creatures were invisible while
  doodads rendered, and the obvious reading — that the entities were never
  placed — sent the search to the protocol instead of the renderer. When
  something is missing, confirm whether it was *submitted* before asking whether
  it was produced.
- **Comparing two candidates tells you which is nicer, not which is right.**
  The ADT placement offset shipped at `-90`, was "fixed" to `+90` because a
  render of Northshire Abbey looked better that way, and both were 90 degrees
  wrong -- every fence in Elwynn lay across its own line the whole time. A
  building has four sides and every rotation shows a door to somebody, so the
  test could never fail. What settled it was measuring something that could
  not move: fence *runs* give a direction from positions alone with no
  rotation involved, and the lamp pillars beside the abbey steps are doodads
  whose world positions are fixed however the building is turned. **A movable
  thing checked against another movable thing proves nothing.** And when a
  user says a second thing is still wrong, that is data about the *first* fix.
- **A value with nothing to compare it against is not verified by looking at
  it.** Entity facing was applied raw for four milestones under a comment
  claiming an M2's forward is +X, and every creature in the world was turned
  exactly backwards the whole time. Watching it live could not catch it: the
  only heading this client *knows* is the player's own, and the player's body
  was not drawn, while a creature's heading comes from the server with nothing
  to check it against. It fell out the moment the player appeared on screen --
  turn the character to a heading the server confirms, put the camera at the
  matching yaw, and whether you see a face or a back is no longer a matter of
  opinion. Before trusting a value because it "looks right", ask what it is
  being compared *to*.
- **Some rules can only be found by looking.** Geoset selection -- which of a
  character model's seventeen haircuts and six beards to draw -- took four
  attempts, and each wrong one was a *reasonable* reading of the same table.
  Drawing everything gave every haircut at once; drawing only what the
  character's own numbers name took the forearms, hands and legs off with the
  phantom cloak, because variant one of an equipment group is the bare body
  part. No amount of staring at `CharHairGeosets` distinguishes those. One
  screenshot each did. When a rule is about what a *model file* contains rather
  than what a table says, render it.
- **An odd-looking render is often the camera.** A gnoll looked scrambled and a
  building looked misplaced; both were framing, not geometry. Render canonical
  angles before doubting the parser.
- **Bound a silent send with an answered one from the same block.** A write
  nothing acknowledges fails identically whether the opcode is wrong, the body
  is wrong, or the request was declined — three investigations behind one
  silence. `CMSG_BUY_ITEM` produced exactly that. What collapsed it in one run
  was sending `CMSG_LIST_INVENTORY`, four opcodes below it, *first*: that one
  is answered, and its reply layout was already known, so 393 bytes of stock
  coming back said the numbering was right and moved the whole question onto
  the body — which was indeed wrong three separate ways. Before improving a
  guess at a silent request, look for a neighbouring one that talks back.
- **A refusal that resolves a guid is proof the body parsed.** The strongest
  evidence in a rejection is usually not the rejection — it is whatever the
  server had to *understand* in order to produce it. `foss-wow#55` was written
  off as "the wrong opcode or body shape" because a slot swap kept being
  refused; but the refusal is eighteen bytes carrying a **real item guid**, and
  reversing the two slots in the request reverses which guid comes back. A
  server that could not parse the body could not have resolved bytes 0 and 1 to
  an item, let alone tracked the argument order. The opcode and the body were
  right the whole time and the open question was the status code. Before
  concluding a send is malformed, ask what the reply proves the far end already
  worked out.
- **"It fails identically under every condition" is a claim to re-run, not to
  build on.** The same ticket rested on the failure being state-independent —
  the argument being that a real handler would treat an empty destination
  differently from an occupied one, so identical behaviour meant no handler.
  The premise did not reproduce: two occupied slots get the refusal and an
  empty destination gets **silence**. The conclusion was sound reasoning from
  an observation that was wrong, which is the expensive combination, because
  the reasoning is what gets scrutinised and the observation is what gets
  believed. A negative result that closes off a line of work earns one
  reproduction before it is written down.
- **"Nothing happened" is two findings wearing one sentence.** An equip sweep
  reported three items as `nothing moved`, which conflates an opcode the server
  never understood with a correct opcode it deliberately declined -- opposite
  investigations, identical printout. Printing every opcode that arrived,
  decoded or not, separated them in one run: the failures each carried a single
  `0x0112` and the twelve successes carried none. This is the same move that
  turned three failed attempts at chat into a one-run answer, and it keeps
  being the cheapest instrument in the box.
- **When the wire and the database disagree, the wire is the client's
  business.** A bag hand-placed into an equipped slot by editing
  `character_inventory` came back on the wire sitting in the backpack, and the
  database still said otherwise afterwards. Both statements were true: the
  server's loader silently relocates a bag it declines to equip, and a session
  that ends by closing the socket never saves, so nothing wrote the correction
  back. Time went into "why is my edit being reverted" when the answer was that
  it never took. A test fixture built behind the server's back is not a
  fixture; and `.additem` needs `.save` for the same reason.
- **A substring filter in a test rig will eventually match a person.**
  `--target Wolf` matched `Testwolf` -- a character belonging to the person
  running the test, logged in on the other account at that moment -- and the
  `.die` behind it killed them. Nothing malfunctioned: the selection registered
  correctly, on exactly what was asked for. This is the mirror of the
  documented `.die`-falls-back-to-self trap and is worse, because it looks like
  it worked. It cannot be fixed by choosing better search words, since a
  creature's name being a substring of somebody's character name is *how people
  name characters*. The selection helpers now refuse players outright, which is
  also just correct: everything they exist to find is something to walk to,
  swing at, or loot.
- **The same trap in a new caller, for the third time.** That replicated state
  holds our *login* position forever is documented on the data and in two
  previous incident write-ups -- and a new loot command still measured a
  corpse's distance from it, reported 15 units, and refused a request that
  would have worked at 1.8. Anything downstream of a walk must be handed the
  walked position, not look one up. When a fact keeps being rediscovered,
  making it a *parameter* beats documenting it again: a caller cannot forget to
  pass an argument.
- **The absence of a field and the absence of a feature look identical.** A
  container announces its capacity but carries nothing naming its contents --
  which proves nothing either way, because every bag observed was empty, a
  create block omits zero fields, and an empty slot *is* a zero. An empty bag
  and a bag whose contents array we cannot find are the same bytes. Before
  concluding a structure does not exist, check whether the sample could have
  shown it.
- **State mirroring a physical input must be corrected from the input's end,
  not from the path that usually handles it.** The camera's drag flags were
  cleared in the branch handling a mouse release -- which sits *after* the
  check that offers the event to egui first and returns if egui consumed it.
  The loot window opens *on* a right-click and appears under the cursor, so it
  swallowed the very release that ends the gesture, and the camera then turned
  with every mouse movement with no button held and no way to stop it. Not a
  rare race: any frame that appears mid-gesture strands the flag. Clearing
  happens before anything can consume the event now, and on focus loss too,
  since alt-tabbing with a button down never delivers a release at all.
- **A frame that never receives clicks looks exactly like one whose handler is
  broken.** Frames opt into `Sense::click()` by appearing in one `matches!`,
  and a frame left out of it draws correctly, hit-tests correctly, and never
  reports a click -- so the arm handling that click is dead code that reads as
  live. The loot window opened and did nothing. Anything that reads
  `response.clicked()` has to appear in that list, and there is now a headless
  test that clicks a row and asserts what comes back.
- **A column that is an override is not the column most rows use.** Combat was
  silent because `CreatureDisplayInfo.sound_id` was read as *the* creature's
  sound, and it is an override that most displays leave at zero -- the real one
  lives on the **model**. It found voices for 1,205 displays of 24,262, and
  every creature in a starting zone was in the silent majority, so the feature
  looked broken rather than partial. Falling back to the model took it to
  24,220. When a lookup works for a minority, check whether the field is a
  default or an exception.
- **Names are data too, and they identify columns nothing else can.**
  `CreatureSoundData` is 38 columns of sound ids with nothing saying which is
  the death cry, and every one holds ids from the same range -- so validity
  separates none of them. But `SoundEntries` carries a *label* per sound, and a
  column whose entries are called `WolfDeath`, `BearDeath` and `KoboldDeath` is
  the death column. The same move named `WeaponImpactSounds`' flesh, chain and
  plate columns. Where a table points at labelled rows, read the labels.
- **A field that is set on every row and resolves most of the time can still be
  the id.** `CreatureSoundData`'s column 0 is set on all 1,306 rows and 935 of
  those land on a real sound, which reads as a well-populated sound column --
  until you notice only 102 of them are *creature* sounds where every genuine
  column is near 100%. Ids overlapping a table's id range is a coincidence of
  magnitude.
- **A sample that cannot exhibit the thing you are looking for is not
  evidence.** Three attempts to capture a loot response came back empty and
  each looked like a protocol problem. A GM `.die` kill generates no loot at
  all, and an ordinary creature killed with `.damage` usually rolls nothing --
  so every run was sampling a population that could not answer the question.
  `creature_loot_template` names creatures with a 100% drop, and spawning one
  with `.npc add` produced a populated packet on the first try. Same rule as
  the `Light.dbc` storm column coming back a coin flip because most positioned
  lights are decorative: **check the population before believing the result.**
- **When the data will not produce a case, change the *character*, not the
  technique.** Two gaps survived a whole milestone and looked unrelated:
  equipment slot 17 could not be named because every ranged weapon offered to
  a warrior came back refused, and a bag's contents could not be located
  because no non-empty bag had ever been observed — `.additem` never puts a bag
  in a bag slot and a hand-edited database does not survive the server's
  loader. Both were attacked as *protocol* problems, with better items and
  cleverer fixtures, and neither moved. A **dwarf hunter is created wearing a
  gun and an ammo pouch with shot already in it** — a fixture the server builds
  itself — and one login answered both. The generalisation: a refusal is a fact
  about the actor, not about the thing being asked for, so when a request keeps
  being declined, ask who is allowed to make it. Creating a test character is
  cheap and was not tried for far too long.
- **One sample of a variable-length packet is nearly free; a sample where the
  *counts differ* is the evidence.** `SMSG_GOSSIP_MESSAGE` carries two
  variable-length blocks back to back, and most of a real menu is zeroes — so a
  reading with the quest block in the wrong place parses an innkeeper's
  three-option, no-quest packet perfectly. What breaks it is a questgiver whose
  packet has no options and one quest. Three NPCs were greeted specifically so
  the two counts would disagree, and the test asserts *both* shapes rather than
  either. The same move as testing an exception beside the thing it is
  indistinguishable from, and as asking the one hour that could refute the sky.
- **A filtered list is the cheapest proof that an index is an id.** Menu 1291
  has four options in the database and three arrived — the missing one is a
  seasonal line the server declines to send — and the three carried indices 1,
  2 and 3, with the numbering *not* closing up. That single observation
  converts "the index is probably the server's own id" from a guess into a
  finding, and the failure it prevents is nasty: a client that replied with a
  row position would work at every NPC except the conditional ones. Whenever a
  list arrives with per-item indices, look for a sample where something was
  filtered out.
- **The step before the one under test can already have done it.** Accepting a
  quest looked confirmed for a whole milestone: the request went out and the
  quest was in the log afterwards. It was the *scroll request* that took it —
  `CMSG_QUESTGIVER_QUERY_QUEST` adds an auto-accept quest server-side — so the
  accept was a no-op against a log that already held it, and every effect
  measured had happened one send earlier. Nothing about the observation was
  wrong; the population was. Only 179 of 9,464 quests behave that way, and the
  starting-zone chain a first end-to-end test naturally picks is all of them.
  Same rule as the loot capture that could not roll a drop and the storm column
  drowned in decorative lights: **before believing a positive result, check
  that something else in the run could not have produced it.** The fix is a
  sample where only the step under test can be the cause — a quest without the
  flag, which put the id in the log for the first time.
- **A diagnosis that names one cause for two situations sends the reader in a
  circle.** The probe reported "already in the log, clear it first", which is
  correct advice for a stale character and useless for an auto-accept quest,
  since the next run's scroll request re-takes it. Distinguishing them cost one
  extra read — sample the log *before* the greeting as well as after the scroll
  — and the two halves then want opposite next steps. Third instance of
  "nothing happened is two findings wearing one sentence", and the tell is
  always the same: advice that would not change if the other cause were true.
- **Two fields holding the same constant cannot be told apart, and the fix is
  a sample where they differ.** `ITEM_FIELD_OWNER` and `ITEM_FIELD_CONTAINED`
  are both the player's guid on every item a starting character carries — `1`
  on one character, `4` on another, matching each one's own guid, which looks
  like solid confirmation of *either* reading. The hunter's pouch separates
  them in a single dump: of ten items, the seven held directly have both fields
  equal to the player and the three inside the bag have one field holding the
  *bag's* guid. The field that changes when the containment changes is the
  containment field. Same move as the storm column and the game-object display
  id: ask which candidate varies the way the thing it names varies.

## Traps already hit

- **Never rewrite a file with a script that can throw mid-write.** A Python
  `write_text` containing a character the console codec could not encode
  truncated `docs/ROADMAP.md` to zero bytes. Prefer the editing tools; if a
  script must write, write UTF-8 explicitly.
- **And a script that writes cleanly can still rewrite every line.** This tree
  is LF; Python's `io.open(p, 'w')` on Windows is *text* mode, which translates
  every `\n` to `\r\n` on the way out. Five files edited that way came back
  byte-different on every line, and the commit went from a 1,400-line diff to a
  26,000-line one that no reviewer could read. The build and the tests are
  entirely happy, so nothing catches it but `git show --stat`. Use the editing
  tools; if a script must write, open in **binary** mode. Check the stat before
  committing — a file you changed three lines of has no business showing
  thousands.
- `wgpu`/`egui`/`egui-wgpu`/`egui-winit` versions are coupled, and the `windows`
  crate needs a pin to build the DX12 backend at all — see `docs/RENDERING.md`
  before touching any of them.
- Windows refuses to execute a test binary whose filename looks like an
  installer, so integration tests are `tests/real_data.rs`, never
  `real_install.rs`.
- Clap eats a bare negative number as a flag: pass `--pitch=-20`, not
  `--pitch -20`.

## Conventions

- Errors are typed per crate with `thiserror`; `anyhow` only in `tools/` and
  `apps/`.
- Comments explain *why*, especially where the format is counterintuitive —
  those are the notes that stop a bug being reintroduced.
- Byte-level parsing gets a unit test with a known-good constant wherever one
  exists (e.g. the MPQ crypt table keys).
- Commit messages explain the reasoning and record dead ends, so a later session
  does not repeat them. `git log` is part of the documentation.
- Every milestone ends with a clean `cargo build --release` (zero warnings) and
  a full `cargo test --release` run.
