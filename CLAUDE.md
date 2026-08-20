# MeoWoW

*The open source WoW client for cats.*

Open-source reimplementation of the WoW 3.3.5a (build 12340) client in Rust.
Client only — no server, no bundled assets.

**The name is the product's, not the tree's.** The directory, the crates and
the binaries are still `open-wow-client`, `wow-viewer` and `wow-cli`, and the
layout still lives in `%APPDATA%\open-wow\ui.toml` — renaming that last one
would orphan every existing arrangement of bars and frames for nothing. What
changed is what a person sees: the window, the README, this file.

**This file holds the rules, the setup and the current state. The reasoning
behind every milestone — what was measured, what refuted what, what is
deliberately not done — lives in `docs/ROADMAP.md`, one section per rung.**
When a row below says "done", the *why* is there and nowhere else. Keep it
that way: this file grows past its usefulness if each milestone narrates
itself here too.

## Where the project is

Phases 1, 2 and 3 are complete: every data format reads, the world renders and
streams, and the protocol reaches a live realm.

**Phase 4's six-part city-services block is finished** — flight paths,
trainers, mail, auction, guild, trade — the services a city offers, taken as
six milestones in an order set by what each one introduces that this client has
never done: trainers (4.24, *nothing* new, which is what makes it the bound on
the rest), flight paths (4.25, the **server moves the player**), trade (4.26,
**both ends must act**), mail (4.27, an **effect with no request**), guild
(4.28, **a list of people who are not in the world**), auction (4.30, **the
first list this client cannot bound**).

**4.29 stepped out of that order to pay off Phase 2's last debt**: shadows, a
skybox, clouds and stars. Phase 2 named all four as deliberately deferred until
there was a character standing in the world; there has been one since 3.5.

Roughly two thirds of the way to something a person could test by playing, and
**4.31 is the rung that makes the playing legible** — a tracker, questgiver
pins and difficulty colouring, all of it a second view of things already on the
wire.

**4.31 is a native Questie, and it is built** — the destination the ladder has
pointed at since NPC interaction. That ladder was a dependency chain rather
than a preference: NPC interaction (4.15) → quests (4.16) → map (4.17) →
minimap (4.21) → Questie. The rung *numbers* kept moving as things landed in
between; the *order* never did, which is the point worth reading.

Two scoping facts that were decided before it started, and held:

* **Most of Questie's bulk is a workaround for a restriction this client does
  not have.** An addon cannot ask the server about a quest it has not been
  offered, so it ships a hand-collected database of the whole game. This client
  can send the query. **Quest data comes from the server and Questie's database
  is not ported**, so quests are never stale and are correct on the custom
  realm this client is developed against. 4.31 is therefore a *presentation*
  milestone.
* **`CMSG_QUEST_POI_QUERY` `0x1E3` is what makes that work.** WotLK shipped its
  own tracker, so the server already holds objective markers for 8,953 of 9,464
  quests and hands over a map id, an area id and a polygon per objective. It
  answers only for quests **in the log**, 25 ids per request, and there is **no
  enumerate-all opcode** — so no bulk prefetch at login, which would repeat the
  37-second burst. What the wire cannot answer is where things are before you
  have seen them; that is solved by recording what the client already streams,
  keyed by realm, starting empty.

**Reading the realm's MySQL is a verification oracle, not a client
capability** — a player on someone else's server has no DB access, so anything
built on it works only for realm operators.

Reference addons live in the **gitignored `addons-to-port/`**, are read rather
than vendored, and each one's licence is checked and recorded *before* a port
starts; see `docs/REUSE-POLICY.md`.

## State

Every row is "what works now". The evidence is in `docs/ROADMAP.md`.

| | State |
|---|---|
| Data formats | MPQ, DBC, BLP, M2 (+animation, timed events, particles/ribbons), WMO, ADT/WDT, MH2O — all done |
| Renderer | Textures, skinned models, buildings, blended terrain, streaming, liquids, M2 emitters, sun shadows — done. **`--screenshot` renders one frame headless and draws NO HUD** (see the instrument rule below). Model files, skeletons and the creature tables are cached **per file** as well as per display id, so a zone of humanoids loads one `HumanMale.m2` rather than one per NPC; every load prints its own cost breakdown |
| Protocol | 3.1–3.5 done against a live realm, two clients at once. Replicated creatures interpolate, turn and animate; **other players do not** — see the defect below |
| World | Day/night from `Light.dbc`, a real sky gradient, sun and moon, weather that falls, game objects drawn. **A star dome, a cloud band and the zone skybox `LightSkybox` names** — which on Azeroth and Kalimdor is none, measured. No moon texture, one cloud layer |
| Shadows | **A directional shadow map from the sun**, cast by terrain, models and alpha-keyed foliage, received by everything but liquid. One cascade around the camera; `--no-shadows` and `--shadow-dump` are the instruments |
| Appearance | NPCs, other players and the viewer's own character are all dressed from their replicated fields. Weapons draw and sheathe. No shoulders, helms or ranged weapons on others |
| Interface | Native, fully customisable, **no addons** — see the decision below. Player/target/party frames, click-to-target, chat, spellbook, action bars per character, `F1` to rearrange, saved to `ui.toml` |
| Sign-in | **The viewer opens a login screen with no arguments**, so double-clicking it is the ordinary way to start: account, password, server, a folder picker for the `Data` directory, then a realm list and a character list. Remembers everything but the password, in `%APPDATA%\open-wow\login.toml`. **Creates no characters** — the original client does that. Four themes (`slate`, `neko`, `void`, `calico`) that *write their colours into `ui.toml`* rather than sitting under it. Its own cat-head icon, drawn in code, on the window and on the executable |
| Game | Melee, spells with real tooltips and a cast bar, cooldowns, combat log, corpse and loot end to end, inventory with slot moves, character panel, quests taken and handed in, quest log with progress counters |
| Map | `M` opens the zone page with the character and quest objectives on it; fills in as explored. **Minimap** in the corner with party dots and objective rings. **Questgiver pins** as diamonds — `!` and `?` told apart, and a *remembered* one drawn faded, because it is a fact about the past. No zoom, panning, continent view or rotation |
| Tracker | **Always on, no key**, top right under the minimap: five quests of however many, by distance with the finished ones first, each with its objective counters and yards to the nearest marker. States the count in every state. A quest the realm gave no markers for sorts **last, not as zero**, and shows no distance at all |
| Sound | Zone music and ambience by area and hour, creature voices, weapon impacts, **footsteps that know what they are standing on** — terrain and building floors both. No attenuation, no spell sounds |
| NPCs | Gossip, vendors (buy and sell), quests, questgiver `!`/`?` marks, trainers, auctioneers |
| City services | **All six done and confirmed at the window: trainers, flight paths, trade, mail, guilds and the auction house.** Browsing, paging, bidding and cancelling; no sell window, no search box, no sort control |
| Collision | Walls stop you, floors and stairs hold you up, M2 collision meshes are obstacles. Tiles are selected by the **bounds of what they hold**, not by where the character is — Stormwind is one placement covering nine tiles. Transitions cut rather than blend; a stair stutter is instrumented, not solved |

### At the window

**`wow-viewer` with no arguments opens the sign-in screen**, which asks for
everything below and remembers all of it but the password. `--data` is
therefore optional now. A command line that already says what to draw
(`--texture`, `--model`, `--creature`, `--wmo`, `--map`) or what to connect to
(`--realm-host` **and** `--user` **and** `--character`) skips the screen
entirely, which is what keeps every probe here reproducible; anything less
opens it with those parts filled in. See `Args::is_self_contained`.

`wow-viewer --realm-host <host> --user <account> --character <name>` logs in,
enters the world and streams the map around where the server says the character
is. **W/S** walk, **A/D** turn, **Q/E** strafe, **Space** jumps, **Num Lock**
is autorun; **right-drag** steers the character, **left-drag** swings the
camera, the **wheel** zooms. Each is a real `MSG_MOVE_*` stream, with the
opcode naming the axis that changed. **Left-click selects, right-click selects
and attacks** — hostility is not yet known (`FactionTemplate.dbc` is
untranscribed), so the client rules out only what is never a fight and lets the
server refuse the rest.

Panels: **B** bags, **C** character, **P** spellbook, **L** quest log, **M**
map, **G** guild, **T** trade, **Z** sheathe, **Enter** chat, **F1** to drag
the layout around and save it. **The auction window has no key** — it opens by
right-clicking an auctioneer and closes when you walk out of range, because
every request in that block resolves its NPC through the server's five-unit
check and fails in silence past it. **The objective tracker has no key
either**, for the opposite reason: it is never opened and never closed, because
a log is a thing you open and a tracker is a thing that is simply there.

**Chat commands take `/` and never reach the wire; `.` is the *server's*
prefix**, which is how a GM command travels as ordinary chat.

**`--log-file <path>` tees every log line, and any panic with its backtrace,
to a file** — the console scrollback dies with the window, which is why no
crash report so far has reached the moment of death. A panic hook forces the
backtrace, so `RUST_BACKTRACE` does not have to have been remembered on the run
that crashed. A log ending in a panic and a log ending mid-frame are different
findings: the second one is not a Rust fault at all.

`wow-cli world <host> --enter <character>` is the CLI-driven equivalent —
**the host is positional and the character is `--enter`**, where the viewer
takes `--character`. `--walk 20`, `--say`, `--units` and the per-milestone
probes hang off it.

**Known defect: replicated *players* do not interpolate.** A creature moves by
`SMSG_MONSTER_MOVE`, which carries a start, an end and a duration; a player
moves by relayed `MSG_MOVE_*`, which carries a position and no path, so
`update_movement` stores it and clears any prediction. The player snaps
between packets and, having no duration, reads as `speed: 0.0` and never leaves
the stand cycle. **Why 3.5's two-client test missed it is the useful half**:
both clients were *this* client, which heartbeats every 100ms, and a hundred
milliseconds of snap between nearby points reads as movement. A real client
sends every ~500ms. See `foss-wow#22`.

**The UI question is answered: this client draws its own interface and does not
run addons.** Reimplementing `FrameXML` faithfully enough for third-party
addons means reproducing a whole Lua/XML widget system before the first health
bar appears. Instead the interface *is* the customisation surface: every
position, size and colour lives in `%APPDATA%\open-wow\ui.toml`, editable by
hand or by dragging frames in-game. egui is the drawing substrate only. See
`docs/UI.md`.

**Frames draw in an explicit stacking order and it decides which window a click
reaches**: map at the bottom, then panels, then the always-there frames (no
window may eat a click meant for an action bar), then the windows that want an
answer — loot, questgiver, release.

## Orientation

- `crates/` — one library per concern: `chunk` (shared chunked container),
  `mpq`, `dbc`, `blp`, `m2`, `wmo`, `adt`, `render`, `auth`, `world`, `ui`
  (the player's interface; depends on neither `world` nor `render`, so it is
  testable without a connection or a GPU) and `collision` (pure geometry,
  likewise testable with a hand-built box)
- `tools/wow-cli` — inspection CLI. **Every format gets a dump command here
  before it is wired into the renderer**, and a `survey` command that parses the
  whole archive set. Those surveys have caught every systematic parser bug so
  far.
- `apps/viewer` — windowed viewer. `--screenshot` renders one frame headless to
  a PNG.
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
- Remote test realm: **`wow1.nekos.farm`** (auth 3724, world 8085), realm
  `NekoCore` at `108.174.48.199:8085`, realm id 1. Accounts `TESTER`,
  `ACCOUNT33`, `ACCOUNT34`. **Passwords are deliberately not recorded here** —
  this file is committed. Ask the user, and pass the password via
  `WOW_PASSWORD` rather than an argument. A wrong password and a missing
  account are hard to tell apart, so guessing wastes real time.
- Two accounts exist so **two clients can be online at once**, which is the
  only way to test anything about one player observing another. `ACCOUNT33` has
  `Testwolf` (human warrior), `Testdruid` (night elf druid) and `Facetest`;
  `ACCOUNT34` has `Watcher`, deliberately human so it spawns within view of
  `Testwolf`.
- **`Facetest` was created with five different non-zero appearance values** —
  skin 3, face 5, hairstyle 7, hair colour 2, facial hair 4. Every other
  character here is the all-zero default, and an all-zero appearance makes any
  search for it match every zero field in the object. Keep it: it is the only
  character that can distinguish an appearance field from its neighbours.

### Local AzerothCore realm (127.0.0.1)

A second realm runs entirely on this machine, from `C:\azerothcore-wotlk`
(`docker compose up -d`). **Prefer this over `wow1.nekos.farm` for anything
needing a specific game state** — remotely every death cost a five-minute fight
and permanently consumed a character's state; locally a death and a
resurrection are each one GM command, so the same scenario runs twenty times.

- Auth `3724`, world `8085`, MySQL `3306`, SOAP `7878`. Realm `AzerothCore`.
- Accounts, all GM level 3: `OWC33`/`owc33` (`Testwolf`, `Facetest`,
  `Questtest`, `Questtwo`), `OWC34`/`owc34` (`Watcher`, `Huntertest`),
  `OWCADMIN`/`owcadmin`. **Unlike the remote realm above, these passwords are
  fine to commit** — the server is local, disposable and reachable only from
  this machine.
- **Every fixture account here is a game master**, which changes what some
  requests do. See the "an acceptance is a fact about the actor" rule below.
- **GM commands travel as ordinary chat**: `--say ".die"` works from our own
  client, and `.die` needs a target, hence `--select-self`. **SOAP on `7878`**
  drives the server with no game session at all.
- Reading the AzerothCore source in that tree is authorised (rule 2 permits
  it): source makes a hypothesis cheap, observation still has to confirm it.

**The fixtures, and why each one exists:**

- **Three NPCs stand at `Testwolf`'s login spot**: Innkeeper Farley (295,
  npcflag 66179), Marshal McBride (197, flag 3) whose quest chain is gated
  behind a prerequisite, and Deputy Willem (823, flag 3) whose is not. That
  combination is what made `SMSG_GOSSIP_MESSAGE`'s two variable blocks
  separable — one NPC with options and no quests, one with neither, one with a
  quest and no options. `.npc add <entry>` rebuilds it.
- **Quest 333 "Harlan Needs a Resupply" (questgiver 1427) is the accept
  fixture**, because its `Flags` and `SpecialFlags` are both zero, so nothing
  but `CMSG_QUESTGIVER_ACCEPT_QUEST` can put it in the log.
- **`Questtwo` is virgin and `Questtest` is spent.** A quest test needs a
  character that has never held the quest and one is not reusable: `.quest
  remove` clears the log but *not* `character_queststatus_rewarded`. Creating
  another is one `--create` and is the right move.
- **`Huntertest` (dwarf hunter on `OWC34`) is the most valuable character in
  the project — do not delete it.** A dwarf hunter is *created wearing* a gun
  and an ammo pouch with shot in it, which named equipment slot 17 and produced
  the first non-empty container this project had ever seen. Both gaps had
  survived a whole milestone of protocol cleverness.
- **Guild id 1, "Cat Herders"**, leader `Testwolf`, six members at ranks
  0/1/2/4/4/4 with both note columns varying independently and a distinct
  emblem. `Testwolf`'s **public note is deliberately empty** — that is the
  record that makes the roster self-refuting. `.guild create <player> "<name>"`
  needs the player online; `.guild invite` and `.guild rank` work offline over
  SOAP.
- Two setup failures worth not re-diagnosing: a stale cached `:master` image
  expecting `VMAP_4.7` against `VMAP_4.8` data (fix: `docker compose pull`),
  and an old database missing the RBAC tables (fix: a fresh volume —
  `AC_UPDATES_ENABLE_DATABASES=0` is baked in on purpose, because
  `ac-db-import` owns migrations and the two must not race).
- A cold snapshot of the previous database lives at
  `C:\azerothcore-wotlk\var\db-snapshot\ac-database-cold-2026-08-13.tar.gz`.
  **Do not delete it** — the live database was deliberately recreated fresh and
  that file is the only remaining copy.

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

The same shapes keep recurring. Each rule names the instance that produced it;
the full account is in `docs/ROADMAP.md`.

### Measuring a format

- **A wrong field offset parses perfectly and returns nonsense.** Check
  properties the data *must* have, not just that it decoded: M2 normals are
  unit vectors, SRP6 rotation keys are unit quaternions, terrain chunks meet at
  their edges.
- **"Could this column mean X" is the wrong question; "is it set *because* of
  X" is the right one.** Any column of small integers points somewhere inside a
  small table, so validity is nearly free. Which of `Spell.dbc`'s 234 columns
  holds a duration matched 99.6% — on the wrong column. Comparing spells whose
  description says `$d` against those that do not separated it immediately,
  98.5% against 39.0%.
- **Validity is nearly free; *variation* is the discriminator.** Two update
  fields both resolved 100% to real `GameObjectDisplayInfo` rows; one was the
  constant 33. Ask whether the candidate *varies the way the thing it names
  varies* — the same move that separated `ITEM_FIELD_OWNER` from
  `ITEM_FIELD_CONTAINED`, which hold the identical guid on every item a
  starting character carries.
- **A property test is only as good as the population.** `Light.dbc`'s storm
  column came back a coin flip because most positioned lights are decorative;
  the row that matters is the one lighting a zone, where it is unambiguous. A
  loot capture came back empty three times because a GM `.die` generates no
  loot at all. Before believing a flat result, check the population can answer
  the question.
- **Ask the question that can refute you.** Five sky bands look plausible at
  noon under almost any ordering; at dawn the warm/cool crossing has a *side*.
  A test that cannot come out the other way is not evidence.
- **Assert a property at the point where it exists.** "A storm is greyer than
  clear weather" fails at noon, where clear light is already neutral grey. The
  property is real at dawn.
- **An error that scales with the value hides in the half of the range you look
  at.** `Light.dbc`'s colour bands were double-encoded to sRGB for as long as
  lighting existed and no daylight render showed it, because the curve is
  nearly flat near the top. Midnight over Elwynn came out afternoon blue.
- **Correcting half of a matched pair is worse than correcting neither.** The
  same double-encode applied to sky, fog and diffuse light; fixing only the
  certain half left the world bright under a dusk sky, and the report that came
  back complained about the half that was *correct*.
- **A name is the one thing in a binary format that cannot be a coincidence.**
  The M2 event stride fit 4,265 of 4,265 models by byte accounting and every
  neighbouring stride fit 1,343; the identifier being four printable ASCII
  characters fit **25,498 of 25,500 records at 36 and 22–24% everywhere else**.
  Same move: `CreatureSoundData`'s columns naming themselves through
  `SoundEntries` labels, `GroundEffectTexture`'s terrain column naming itself
  through the *filenames* reaching it, the trainer's greeting at the end of a
  body, and the guard test checking hardcoded animation ids against the **name**
  of the row they claim.
- **Work out in advance which samples are *incapable* of separating your
  candidates.** 476 fit 6,429 of 6,429 M2 particle records and every neighbour
  fit 1,739 — exactly the number of single-emitter models, where padding hides
  a word. 86,222 full-chunk ocean sheets transpose to themselves and had to be
  excluded before the `MH2O` axis vote meant anything. A six-row trainer packet
  cannot tell stride 38 from 42, because the overshoot lands *inside* the
  greeting; a 133-row one can. **When the probe reports a tie, that is the
  honest answer** — go and find a better sample.
- **When a population cannot separate two readings, change the population.**
  The guild roster's conditional float had two survivors after every static
  check; clearing an *online* member's public note made a wrong reading run off
  the end. The probe manufactured the decisive sample in the same run.
- **A colour that is always exactly grey is not a colour.** `Light.dbc`'s band
  8 is neutral on **3,744 of 3,744 outdoor samples**, where every other band is
  1-6%; it is a scalar stored in a colour column. The contrast is the evidence,
  and it stops there: a storm lowers it on 79% of the samples that move, which
  is the shape of a shadow strength and of several other things, so it is
  **not named**. `wow-cli light --band-survey`.
- **Two fields of the same shape in two records need not be in the same
  units.** A particle emitter stores colour in 0..255 and a ribbon in 0..1, in
  the same file, twenty bytes apart. Counted rather than guessed: 77,410 of
  78,377 particle keys exceed 1.0 and **0 of 1,572** ribbon keys do.
- **A column can be named correctly and still not mean what its name says
  here.** Every held item stores geometry in `ItemDisplayInfo.model_left` and
  leaves `model_right` empty — but shoulders fill both with `LShoulder_` and
  `RShoulder_`, which proves the names. The pair is "first model, second
  model". Find the rows where the name is unambiguous and let those define it.
- **A column that is an override is not the column most rows use.**
  `CreatureDisplayInfo.sound_id` found voices for 1,205 displays of 24,262; the
  real one lives on the *model*, and every creature in a starting zone was in
  the silent majority. When a lookup works for a minority, ask whether the
  field is a default or an exception.
- **A field set on every row and resolving most of the time can still not be
  the id.** `CreatureSoundData` column 0 resolves 935 of 1,306 — and only 102
  to *creature* sounds, where every genuine column is near 100%.
- **Two tables can mean the same thing and disagree about which value means
  "nothing".** `GroundEffectTexture`'s terrain column defaults to row **0**
  (also `Dirt`); a WMO material's `ground_type` defaults to row **10**
  (`None`). Carrying the outdoor reading indoors makes every wall claim to be
  dirt. Ask each table separately what its default is — and note the survey's
  own headline counted "not zero" as "labelled" and reported 1,981 of 1,985
  buildings as saying something where the real number is **622**.
- **A schema is a fact about storage, not about a wire.** The
  `SMSG_TRAINER_LIST` stride was predicted at 30 from the realm's own modern
  trainer tables and is **38**: the schema was modernised and the packet
  builder was not, inside one running server. Sibling of "a grep that finds the
  field names is not a grep that finds the structure" — reading
  AzerothCore for the jump block's field order turned up a *different packet's*
  handler, and "fixing" the correct one would have been silent.
- **A number copied out of a tool's output carries that tool's frame of
  reference.** Two animation constants were transcribed from `wow-cli m2
  anims`' *index* column as if it were the `AnimationData` id. Both were silent
  and silent in opposite ways: one named a cycle no character model carries, so
  it fell through to `Stand`; the other named `EmoteTalkQuestion`, which plays.
- **Listing a directory and reading a path are different questions.** An MPQ
  resolves by hash, so a file absent from `(listfile)` still reads perfectly —
  a coverage check built on `wow-cli ls` said 0.1% of the baked NPC textures
  shipped, and forty random names by path got forty hits. **A tool's own
  default limit is the same trap**: `m2 anims` lists thirty of 156 sequences,
  and the sidestep cycles are rows 38 and 39.

### Reading a wire

- **Assert the parse consumed the whole record.** Four separate
  world-protocol bugs were invisible field by field and obvious the moment a
  cursor reported leftovers. Parse through a cursor; make running out of input
  *and* having input left over both errors.
- **…and know where that instrument is blind.** It cannot see a conditional
  field followed by null-terminated strings, because **a string scan
  re-synchronises**: all three readings of the guild roster consumed 703 bytes
  exactly. What refuted one was a note that is *not text*.
- **A record that states its own length can be lying, so check it and never
  seek by it.** Every `SMSG_MAIL_LIST_RESULT` record announces a size four
  bytes too large, on every record, because the server counts eight words where
  the writer writes seven. Seeking lands inside the next record; ignoring
  throws away a per-record check; **checking** localises the mistake. The test
  asserts the odd half — a record whose announced size *equals* its real one is
  refused.
- **One sample of a variable-length packet is nearly free; a sample where the
  *counts differ* is the evidence.** `SMSG_GOSSIP_MESSAGE` has two
  variable blocks back to back, and a wrong reading parses an innkeeper's
  three-option/no-quest packet perfectly. Three NPCs were greeted so the counts
  would disagree, and the test asserts *both* shapes.
- **A count in a reply is not always the length of the thing it counts.**
  Every list this client read for four phases arrived whole, so "store the
  reply" was the same as "store the truth". `SMSG_AUCTION_LIST_RESULT` carries
  two counts -- **50 rows in the packet, 130 matched in the house** -- and on a
  populated realm the second is tens of thousands with no opcode that returns
  them. Nothing errors: the rows are real, the prices are real, and a client
  that merges successive pages builds a union of snapshots that was never true
  at any instant. Ask whether the count at the front of a packet is the
  subject's length or the packet's.
- **The difference of two lengths over the difference of two counts is a
  stride, and it is the only measurement that does not assume the header and
  the footer.** `(16440 - 7412) / (111 - 50) = 148`, and neither the four-byte
  header nor the eight-byte footer appears in it. One packet can only say
  which candidate accounts for the body, which is a unique answer *given* a
  footer width -- the thing being tested. Header and footer then came free off
  an **empty** page: twelve bytes.
- **A trailing block absorbs a stride error instead of exposing one.** With the
  count at the front and nothing after the records, a wrong stride runs off the
  end and the cursor says so. With a total and a delay *after* them, a stride
  wrong by a word eats the footer as record data and reads the last record's
  tail as the total -- the parse succeeds and the number shown to the player is
  garbage.
- **The strongest bound is a reply that cannot vary.**
  `CMSG_AUCTION_LIST_PENDING_SALES` checks nothing at all -- not the NPC, not
  the range, not the level, not the body it just read -- and always answers a
  `u32` zero, because the server's own loop over the records is commented out.
  `CMSG_GUILD_ROSTER` is answered without a guild but still *describes state*,
  so a reply that looked wrong could be a wrong parse or a strange guild. Sent
  before looking for an auctioneer, this separates "the opcode block is
  unreachable" from "the request was declined" with no fixture at all.
- **The value that disables a filter is not always zero.**
  `CMSG_AUCTION_LIST_ITEMS` turns a filter off with `0xFFFFFFFF`, because zero
  is a real item class -- so a zeroed request is not "search everything", it is
  a real and very narrow search that returns almost nothing and looks like a
  broken parser. `AuctionSearch` has no `Default` for that reason; the only
  constructor is `any()`.
- **A filtered list is the cheapest proof that an index is an id.** Menu 1291
  has four options and three arrived, numbered 1,2,3 with the numbering **not
  closing up**. Same for loot slots, trainer rows and gossip options: **an
  index is the server's own id, never a row position.**
- **Print the body, not the length, of anything you refuse.** A parser that
  declines an unconfirmed shape is only useful if the shape survives the
  refusal. `SMSG_ATTACKERSTATEUPDATE` arrived four bytes long, and two separate
  tools logged the *length* and dropped the bytes.
- **Print every opcode seen, decoded or not.** "The server never sent it" and
  "it arrived and we could not read it" are the same observation until
  something separates them, and they want opposite investigations. This is the
  cheapest instrument in the box and it has been needed **five** times: three
  failed attempts at chat, an equip sweep reporting `nothing moved`, a mail
  arrival loop that reported zero while the database showed the letter
  delivered, and a guild-chat probe sending in **language `0`** — Universal,
  refused with no reply at all — where zero chat lines *and* no chat opcode of
  any number is what separated "never sent" from "arrived unreadable". **The
  loop that needs it is always the one somebody is writing now**, and four of
  those five were written after the rule.
- **"Nothing happened" is two findings wearing one sentence.** An opcode the
  server never understood and a correct opcode deliberately declined have
  identical printouts and opposite investigations.
- **A diagnosis that names one cause for two situations sends the reader in a
  circle.** "Already in the log, clear it first" is right for a stale character
  and useless for an auto-accept quest. The tell is advice that would not
  change if the other cause were true.
- **Bound a silent send with an answered one from the same block.** A write
  nothing acknowledges fails identically whether the opcode, the body or the
  request was wrong. `CMSG_LIST_INVENTORY` bounded `CMSG_BUY_ITEM`;
  `CMSG_GROUP_INVITE` bounded the party block; `CMSG_TRAINER_LIST` bounded the
  city-services block; `CMSG_GUILD_ROSTER` is answered even for a character in
  no guild, so it needs no fixture at all.
- **When every request in a block is silent, the bounding instrument is a
  refusal.** All ten of trade's client opcodes are silent on success and the
  reply to a successful initiate arrives at *somebody else's* client. Aimed at
  a guid that is not a player it answers immediately, at the sender, with
  nobody else logged in. **A refusal that names a reason is a reply.**
- **A refusal that resolves a guid is proof the body parsed.** The strongest
  evidence in a rejection is what the server had to *understand* to produce it.
  `foss-wow#55`'s refusal carried a real item guid, and reversing the two slots
  reversed which guid came back — the opcode and body were right the whole
  time.
- **A reply you cannot get is not the same as a reply you did not earn.**
  `CMSG_ATTACKSWING` from out of range produced two empty refusals, which looks
  exactly like a wrong opcode. The proof was not that a reply came but that it
  **changed when the conditions did**. When nothing confirms a send directly,
  find the input you can vary and check the output varies with it.
- **A design premise can be wrong in a way that fails as an empty rectangle.**
  `SMSG_TRADE_STATUS_EXTENDED` describes one side of a trade and is sent to the
  *partner*, never to its author — so the obvious design draws its own column
  permanently empty, which reads as half-finished rather than as false. What
  caught it was the probe printing **two counts**: "3 describing the partner's
  half, 0 describing our own" is a measurement; "our half never printed" is
  not.
- **A silent request may be believed only when every refusal is loud.** This
  client draws its own half of a trade window from memory, which breaks the
  standing rule — the licence is that *every* way `CMSG_SET_TRADE_ITEM` can
  fail cancels the trade outright, so an open window and a disputed item is not
  a state that exists. Before trusting a local record, enumerate the refusals
  and check none of them is quiet.
- **Two things can share one number.** Trade's `BUSY` means "already trading"
  before the window opens and "not enough money" after it;
  `ERR_GUILD_PERMISSIONS` and `ERR_GUILD_LEADER_LEAVE` are both `8`. Leave such
  a value as a number rather than naming one meaning and being confidently
  wrong half the time.
- **Do not transcribe a table you have not verified — especially one that only
  produces text.** A wrong offset eventually fails loudly; a wrong *name* for a
  status code never does. `describe_cast_failure` names exactly one reason, the
  one observed, and returns the raw number for everything else.
- **A number nobody can check is worse than a blank.** The description
  substituter resolves only tokens whose columns were confirmed and passes the
  rest through with the `$` intact: a visible `$s1` says "not implemented", a
  fabricated `47` says nothing and is believed.
- **A packet is a statement made once; a field is a statement that stays
  true.** Creature facing driven off `SMSG_MONSTER_MOVE`'s facing block turned
  only when the player moved. `UNIT_FIELD_TARGET` says who a unit is fighting
  for as long as it is fighting them.
- **An absent update field is a zero, not an unknown.** A create block carries
  only non-zero values, so a player with the default appearance has no
  `PLAYER_BYTES` at all and the guild master has no `PLAYER_GUILDRANK`. Only a
  dropped *object* means unknown.
- **…but a missing *answer* is unknown, never nothing.** There is deliberately
  no `quest_cache::get() -> Option<&QuestInfo>`: "no such quest" and "the reply
  was lost" are indistinguishable and only one is permanent. The cache holds
  packet **bodies, not parsed structs**, so a parser that learns a new field
  upgrades every cached quest with no migration. Nothing POI is cached at all,
  because the query gives the same empty list for "not in your log" as for "no
  markers".
- **A cache that records only the answers worth acting on cannot say why it is
  silent.** The remembered questgivers store `Some(QuestgiverMark::None)` --
  "the server said there is nothing here" -- as carefully as they store an
  exclamation, because the alternative makes an NPC nobody has got round to
  querying indistinguishable from one with nothing left to give. Same trap as
  the two rules above, in the one module whose entire job is remembering, which
  is where it is hardest to see: **the interesting answers are not the only
  answers.**
- **A pin is a claim about a place, so it is keyed by the spawn and not the
  kind.** An entry names a *sort* of creature -- Innkeeper Farley's stands in a
  dozen inns -- and a guid names one of them. The probe demonstrated it without
  being asked to: two guids came back both holding entry 197, Marshal McBride,
  at two different positions, and an entry-keyed cache would have had one
  overwrite the other. The cost is that a **temporary summon's guid comes off a
  counter** and can later name something else, so the record carries its entry
  too and is dropped when the two disagree.
- **The absence of a field and the absence of a feature look identical.** A
  container announces its capacity and nothing naming its contents — which
  proves nothing, because every bag observed was empty, a create block omits
  zeros, and an empty slot *is* a zero. Check whether the sample could have
  shown it.
- **The step before the one under test can already have done it.** Accepting a
  quest looked confirmed for a milestone: the request went out and the quest
  was in the log. The *scroll request* had taken it — `CMSG_QUESTGIVER_QUERY_
  QUEST` adds an auto-accept quest server-side. Only 179 of 9,464 quests behave
  that way and the starting-zone chain is all of them. **Before believing a
  positive result, check nothing else in the run could have produced it.**
- **State that persists needs accounting, not just parsing.** Every parser here
  is memoryless; replicated world state is not. A dropped update is permanent,
  a merge that overwrites erases fields nothing will resend, a missed removal
  leaves a ghost. None of it errors and all of it compounds. Count every
  change, tally updates naming unknown objects, check `created - removed ==
  held`.
- **When the wire and the database disagree, the wire is the client's
  business.** A bag hand-placed into an equipped slot by editing
  `character_inventory` came back in the backpack: the server's loader silently
  relocates a bag it declines to equip, and a session that ends by closing the
  socket never saves. A fixture built behind the server's back is not a
  fixture; `.additem` needs `.save` for the same reason.
- **When the data will not produce a case, change the *character*, not the
  technique.** Equipment slot 17 could not be named because every ranged weapon
  offered to a warrior was refused, and no non-empty bag had ever been
  observed. Both were attacked as *protocol* problems, with better items and
  cleverer fixtures, for a whole milestone. A dwarf hunter is created wearing a
  gun and an ammo pouch with shot in it — a fixture the server builds itself —
  and one login answered both. **Creating a test character is cheap and was not
  tried for far too long.**
- **A refusal is a fact about the actor — and so is an acceptance.** When a
  request keeps being declined, ask who is allowed to make it (the dwarf
  hunter). The mirror costs more: `CanOpenMailBox` accepts the reader's **own
  guid** from anybody at moderator rank, and every account on this realm is a
  game master, so the cheapest possible probe works here and for no player at
  all. A refusal sends somebody looking; an acceptance sends them home.
  **Anything that works suspiciously easily on a GM account gets measured in
  order to be ruled out.**
- **A corpse keeps every flag it had in life.** `UNIT_NPC_FLAGS` is unchanged
  on a dead NPC, so "will it talk" and "will it answer" are different
  questions. And the server has **six** interaction preconditions that each
  fail in silence — alive, the flag, not charmed, reaction above unfriendly,
  within five units, class matching for a class trainer — so a fixture standing
  in a field of kobolds is not a fixture.
- **A display id says how to draw a thing and nothing about what it is.** A
  mailbox and a bench are both a model at a position; `CMSG_GAMEOBJECT_QUERY`'s
  type is the only thing on the wire that separates them. The cache keeps
  **three** states — not asked, answered "no such thing", answered — because
  reading "not asked yet" as "not a mailbox" refuses the object the player is
  standing in front of, and only on the first click.
- **Give the other end time to act before concluding it ignored you.** A single
  packet sent immediately before disconnecting is often never processed.
- **Not every failure is a bug, and a rate limit is not a refusal.** Three
  keepalives dropped the world connection because the server enforces a
  *minimum* ping interval. Clicking the loot-method control repeatedly
  disconnected the socket outright (`10053`) rather than producing refusals.
  Before shipping anything a player can click repeatedly, ask what happens to
  the **connection** under a burst.
- **A limit that bounds packets does not bound time.** The login burst drained
  until quiet *or* 512 packets; Northshire emits a monster move fourteen times
  a second and is never quiet, so it ran **thirty-seven seconds** before the
  first frame. The same bug reappeared in the next loop somebody wrote, and was
  worse for being **silent** — with no progress output a stuck run and a slow
  one look identical. Any loop draining a live stream needs a wall clock *and*
  something printed per round. A 150-second guild wait then died at `failed to
  fill whole buffer` for the mirror reason: **no keepalive**. A loop that sends
  nothing on purpose is exactly the one the server drops. **4.30's auction
  probe made it four**, with `(2s quiet, 256 packets)` in an Elwynn that never
  goes quiet for two seconds — five minutes of no output and no CPU, which
  reads as a hang. The rule predicted its own next instance and was right
  again.
- **Measure the thing, not the thing next to it.** That same delay presented
  as the action bar filling half a minute after login, and the confident
  diagnosis was a slow `Spell.dbc` read blocking the render thread — with a
  plausible argument attached, since two runs agreed to the second. The DBC
  load takes 185ms. One timing log around the suspected culprit settled in one
  run what reasoning had got backwards.
- **A marker that a slow thing finished looks exactly like its cause.** Every
  visible stutter carried a `drew with N placeholder texture(s)` warning, and
  the correlation is perfect -- because the warning is printed at the *end* of
  the load that cost the frame. Two guesses were spent on it. A log line
  emitted by the expensive thing is evidence about *when*, never about *what*.
- **A breakdown whose parts do not sum to its total is not a breakdown.** The
  first load timings named six phases summing to 36ms of a 58ms load, and the
  missing third was where the next wrong guess would have gone. Every phase is
  measured rather than attributed now and the line prints what none of them
  claimed -- which is how `model::creature` was caught re-parsing 24,262 rows
  of `CreatureDisplayInfo` *per creature*, outside everything being timed.
- **Split a cost before choosing its fix, because the halves have different
  ones.** Reading the `.m2` and parsing it were one number until they were
  two, and they came out 10.2ms against 0.4ms: a byte cache removes nearly all
  of it and a parsed-object cache removes nothing. Timed together, either fix
  looked equally reasonable.
- **A cache keyed by what varies will re-load what does not.** Creature models
  are keyed by display id, correctly -- a display id supplies the skins. But
  every humanoid in the game is one `.m2` and forty-seven `.anim` files, so the
  key that distinguishes costumes re-read the *costume-independent* file once
  per NPC. The fix is a second cache keyed by file, not a wider key. **The tell
  is a per-item cost that does not fall as more items are loaded.**
- **A negative is an answer and gets cached like one.** Five of a character
  model's `.anim` paths do not exist -- alias sequences, entirely ordinary --
  and only the file cache's own hit/miss counters said they were being asked
  for again on every costume. The regression test asserts a *count of archive
  reads* rather than elapsed time, and that is what caught it: a timing
  assertion would have passed and a timing assertion is flaky anyway.
- **Writing a format is riskier than reading it.** A bad read fails loudly at a
  known offset; a bad write is accepted as some other valid message. Where a
  structure travels both ways, define it once and round-trip it.
- **The hard-looking part is rarely the expensive one.** SRP6, the RC4 header
  cipher and the update-field bit-packing all worked close to first time: they
  are precisely specified and fail loudly. Every hour actually lost went to
  ordinary struct layout, where a wrong guess parses perfectly.

### Looking at a picture

- **A buffer nothing displays needs a dump command more than the others, not
  less.** A shadow map that is empty, aimed at the sky, depth-reversed or
  perfectly correct all produce a world that is uniformly lit or uniformly
  dark. `--shadow-dump` writes it as a PNG, stretched to the range present --
  raw, a 440-unit box holding a 40-unit landscape prints as a white square --
  and prints texels drawn *and* texels left at the far plane.
- **A composite needs a way to be seen as itself.** A dressed character looked
  bare-chested at walking distance; dumping the composed 512x512 skin showed
  all ten regions correct and a white shirt that reads as skin at three hundred
  pixels. Anything assembled in memory from a dozen files gets a dump command.
- **Measure the asset, not the channel you expected it to use.**
  `lake_a.blp` averages **RGB 3.6 of 255** and keeps its whole ripple pattern
  in alpha, while `lava` and `slime` are ordinary opaque colour textures
  (`material_id` 1 against 2). A shader multiplying `texel.rgb * tint` over
  both draws a river that looks like a shadow. Dump and *measure* the input
  before doubting the output.
- **Comparing two candidates tells you which is nicer, not which is right.**
  The ADT placement offset shipped at `-90`, was "fixed" to `+90` because a
  render looked better, and both were 90 degrees wrong. A building has four
  sides and every rotation shows a door to somebody. **A movable thing checked
  against another movable thing proves nothing** — what settled it was fence
  *runs*, which give a direction from positions alone.
- **When looking cannot settle it, find the thing that moves.** A greatsword on
  a back has two mirror images and both look right. What was asymmetric was the
  `Sheath` **animation**: the hand travels two to three times closer to one
  candidate, on three races.
- **A value with nothing to compare it against is not verified by looking at
  it.** Entity facing was applied backwards for four milestones under a comment
  claiming an M2's forward is +X. Watching it live could not catch it — the
  only heading this client *knows* is the player's own, and the player's body
  was not drawn.
- **When geometry is missing rather than wrong, suspect culling before data.**
  WMO winds counter-clockwise, M2 and terrain clockwise.
- **Geometry drawn at zero size looks exactly like geometry never drawn.** A
  fresh bone palette is zeroed and a zero matrix collapses a model to the
  origin, silently; a bone index past the end reads zero on the GPU and does
  the same. `--screenshot` never called `update_animations`, so a zone with
  ninety-five creatures rendered as empty grass and had since the feature was
  written. **Anything that must be written before it is read should start as
  something you can see** — the palette initialises to identity now, so the
  same mistake draws a bind pose.
- **An absent capability and an absent thing produce the same picture.** The
  model loader had refused `produced no drawable geometry` since it was
  written, and 653 models are *nothing but* an emitter. **A guard meaning
  "nothing to draw" has to be re-asked every time the renderer learns to draw a
  new kind of thing.**
- **A still frame is the wrong instrument for anything with a history.** A
  headless render of a particle system draws an emitter just switched on; a
  ribbon at one frozen instant lays every edge in the same place and draws
  nothing. The headless path warms up sixty steps *with the clock running*.
- **An instrument that fails quietly costs more than one that fails.**
  `--hour` parsed, was promised in the help text, and did nothing without a
  realm — an offline screenshot silently got the fallback gradient, which is a
  perfectly plausible sky, and several minutes went into studying a picture
  that had never consulted the tables. A flag that cannot act should say so.
- **When every measurement says it is right, stop measuring and move.** A
  weapon that would not appear produced four clean diagnostics; the camera sits
  behind the character and a blade held forward at hip height is entirely
  behind its owner. One render from the side settled it. **The tell is the
  pattern**: diagnostics that keep coming back correct are evidence about the
  observer.
- **When a measurement contradicts a conclusion you trust, suspect the
  instrument.** The stowed-weapon attachment points came back the *least*
  stable of thirty-nine, which looked like proof the identification was wrong.
  The sampler was broken.
- **An instrument that cannot move cannot measure, and it says so with a
  straight line rather than an error.** Two attempts to find the footfall event
  produced perfectly readable output that meant nothing: the first traced a
  bone with no animation track (a flat line for all thirty-seven events), the
  second read `matrix.transform_point3(ZERO)` when a posed matrix is a
  deformation about the bone's *pivot*. Only pivot-through-matrix showed two
  feet planted in antiphase, which is what a walk is. **Before reading a
  measurement, ask what it would look like if the probe were aimed at
  nothing.**
- **An odd-looking render is often the camera.** Render canonical angles before
  doubting the parser.
- **Some rules can only be found by looking.** Geoset selection took four
  attempts and each wrong one was a *reasonable* reading of the same table —
  drawing only what the character's numbers name takes the forearms, hands and
  legs off, because variant one of an equipment group is the bare body part.
  When a rule is about what a *model file* contains rather than what a table
  says, render it.

### Building an interface

- **`--screenshot` does not draw the HUD, so a clean headless render says
  nothing about the interface.** 4.24 shipped on "1,004 tests green plus a
  clean live render", a white-screen report came back, and the reproduction
  produced a perfect picture of Elwynn — because it could not have produced
  anything else. The tell was in the image: **no player frame, no action bar,
  no minimap.** Every headless UI test also drives a fresh `Hud::default()` in
  a synthetic egui context, so a saved `ui.toml`, egui's cross-frame layer
  ordering and window-only input routing are outside what any of it covers.
  **When claiming a milestone is verified, say which instrument saw which
  half.**
- **Convert every live-only bug into a check that runs without a window.** 3.5
  and 4.1 each cost a handful of bugs no test could have caught; 4.2 cost none,
  and the difference was not care — 4.1's failures had become a headless egui
  pass asserting a frame painted where the layout put it, and received chat
  logged as well as drawn.
- **A modal gesture with nothing on screen naming it is not a gesture.** Trade
  offering is a right-click in the bag window meaning something different while
  a trade is open, and the first live test came back *"I couldn't give him an
  item"* with every line correct. **The first live report of a new interaction
  tests discoverability as much as correctness.** And all four refusal paths in
  `offer_item` returned silently, so three causes shared one sentence — they
  each log now.
- **A headless click test needs more passes than a headless draw test.** egui
  matches a press against the widget rectangles from the pass *before* it, so
  on a fresh context the first press lands on nothing. Two passes is right for
  asserting what was painted and wrong for asserting what was clicked: the
  sign-in panel's click test, written with two, reported the panel's *initial*
  state for every control and read exactly like a hit test that was simply
  wrong. Four. The HUD's own harness already carried the split and said why.
- **A frame that never receives clicks looks exactly like one whose handler is
  broken.** Frames opt into `Sense::click()` by appearing in one `matches!`,
  and one left out draws correctly, hit-tests correctly, and never reports a
  click. Anything reading `response.clicked()` has to appear in that list.
- **A window onto a longer list has to say that it is a window.** The auction
  window draws `49-60 of 1284 -- page 5 of 107` as one sentence, in every
  state, including the ones where it is uninteresting. The failure is not a
  rendering bug: the fifty rows are real and their prices are real, and the
  person reading them believes they are the market. A line that appeared only
  when there was a surplus is a line nobody has learned to read.
- **Sorting an absence as zero states a fact nobody supplied.** The tracker
  orders quests by distance to the nearest objective marker, and a quest the
  realm gave no markers for has no distance. Sorted as zero it goes to the
  *top* of a nearest-first list, which reads as "you are standing on it" --
  a different sentence from "the realm did not say". It sorts last and shows no
  number at all. **A default value inside a comparison is an assertion**, and
  the comparison is where nobody looks for one.
- **A control that sorts what is on screen answers a different question from
  the one it appears to answer.** Sorting fifty rows of 1,284 by price gives
  the cheapest of *those fifty*, in price order, looking exactly like the
  cheapest fifty in the house -- with the actual cheapest on page nine. The
  auction window therefore has **no clickable column headers at all**, because
  the sort belongs in the request and the request does not carry one yet. An
  honest absence beats a plausible wrong answer, the same call
  `describe_cast_failure` makes about naming a status code.
- **Page arithmetic has a boundary nobody has to be careless to reach.**
  Narrowing a search while on page nine leaves the offset where it was, and the
  server *answers* an offset past the end -- an empty page and the true total.
  `offset / 50 + 1` against `ceil(39 / 50)` then reads **"page 3 of 1"**, which
  the probe printed before anything asserted otherwise. It is a named state
  now, and the window says "past the end, go back" rather than lying about the
  search.
- **A hit test that answers for every row is indistinguishable from a correct
  one until somebody clicks the wrong one.** `row_at` answers only for rows
  that can act — learnable trainer spells, unemptied letters, online guild
  members — because the alternative ships a request the server declines in
  **silence**.
- **The rows are not all the same height**, so the hit test walks the same
  accumulating heights the drawing does. An averaged division targets the wrong
  member, silently.
- **State geometry once and read it from both the drawing and the hit test.**
  The invite prompt has two opposite answers a few pixels apart; a press
  between the buttons answers nothing.
- **State mirroring a physical input must be corrected from the input's end.**
  The camera's drag flags were cleared in the mouse-release branch, which sits
  *after* the check that offers the event to egui — and the loot window opens
  *on* a right-click, so it swallowed the release that ends the gesture. Any
  frame appearing mid-gesture strands the flag. Cleared before anything can
  consume the event now, and on focus loss too.
- **A line that renders as a plausible *different* line never errors.** Guild
  chat parsed correctly the whole time and both maps from the wire type to the
  interface's own `ChatKind` had no arm for it, so it drew with no tag in
  `Other`'s grey — against `Say`'s near-white, a difference nobody can see. The
  report that came back named the *sticky channel*, which is the wrong half of
  the system. Assert the tag and the colour separately: either alone passes
  with the other broken.
- **"The number changed on the wire and in the log" is not "the control
  works".** The party loot control had a debug notice proving the send, another
  client's log proving the broadcast, and a number on the sender's own screen
  that silently refused to move past one value. A live capture that never puts
  a person in front of the exact frame the report is about will declare victory
  one layer short of it.
- **A drag holds the pointer** — hidden, confined, warped back to the press
  position — which broke the click test, since press-and-release-in-the-same-
  place is what a pinned pointer produces for *every* drag. It is distance
  travelled now.
- **Copying a mechanism is the cheapest way to audit it.** Right-click's
  press/release distance test was written by mirroring left-click's, and
  mirroring it surfaced a four-milestone-old bug: the left button cleared
  `last_cursor` on release, so a second click at the same pixel was discarded.
  A selection is not a gesture anyone repeats; right-click-to-attack is.
- **The wheel had two claimants** — the camera's handler never asks where the
  pointer is, so the minimap answers first and returns.
- **Per character, not per client.** One shared action-bar set meant a rogue
  logged in holding a warrior's bar: every icon drew, every key pressed, every
  cast was refused, and it read as "the bars are broken".

### Writing the code around it

- **A trap documented at one call site does not protect the next one.** That
  replicated state holds our *login* position forever is written up at length
  in the function that **draws** the player, and was walked into immediately by
  one that **aims at** the player — and then a third time by a loot command
  that measured a corpse's distance from it. **When a fact keeps being
  rediscovered, making it a *parameter* beats documenting it again**: a caller
  cannot forget to pass an argument. Same shape: `Entity::will_talk` knew
  nothing about dead NPCs while the viewer's `is_talk_candidate` already did.
- **A comment describing a check is not a check.** `/g`'s doc comment claimed
  guild membership was re-checked in `send_on_channel` the way `/p` is, from
  the day it was written. It was not.
- **An in-tree comment is a claim with the same evidentiary weight as any
  other.** `SMSG_GM_MESSAGECHAT` carried a comment saying it shares the
  ordinary body and exists only for styling. It does not — a GM line names its
  sender inline, whatever `ChatType` it carries — and nothing had ever
  exercised the path, because every earlier chat test ran on the non-GM remote
  realm.
- **A rule written for the exceptional case can silently swallow the ordinary
  one.** `App::target` was cleared whenever its guid held no replicated object,
  correctly for a target that died or walked away. Parties made "no replicated
  object" the *common* case for a good selection. The fix is a positive
  exemption (`still_targetable`), not a narrower negative condition — **the
  milestone that changes which case is common is the one that has to teach the
  old rule about the new one.**
- **A rule can be right and still exclude the one thing you need — and then the
  test has to assert both halves.** `Auto Attack` sits on `SkillLineAbility`'s
  generic line 183 with a class mask of zero, exactly like the junk the seeding
  filter correctly rejects. Widening the rule readmits the junk; naming the
  single spell does not. **Test the exception *and* the thing it is
  indistinguishable from.**
- **A fallback can invalidate the rule written beside it.** Combat animations
  fall back to `Stand` on a wolf, which broke "plays once, so hold the last
  frame" — a Stand frozen at its final frame is a statue — and broke it the
  other way for `Dead`, which resolves to the fall and must hold. Holding had
  to become a property of the animation that *resolved*, not of the state that
  asked. **When a fallback is added, re-ask every question that was answered in
  terms of the thing being fallen back from.**
- **A field can be parsed, documented, and then ignored by the one function
  that had to act on it.** `Track::global_sequence` had a doc comment saying it
  runs on a shared timer, and `sample` indexed `sequences[sequence]` anyway —
  so global tracks resolved only when the index happened to be zero, which is
  `Stand`. Invisible for four milestones, until a sheathed sword flew off a
  character's back.
- **A per-frame assignment silently destroys state that has to survive the
  frame.** `position.z = ground` was correct for four milestones; swimming
  *accumulates*, and the two were written next to each other with the
  assignment first, so every frame reset the swimmer to the riverbed and rose
  3% of the way. That draws as a character walking along the bottom with the
  stroke cycle playing — the feature apparently half-implemented rather than
  mis-ordered.
- **An early return skips more than it was written to skip.** A taxi flight
  replaces `drive_live_movement` wholesale, which is right for writes that must
  all be skipped together — but the *camera placement* lived in the same
  function's tail, so a flying character was followed by nothing and, because
  streaming follows the camera, the world stopped loading. One cause, three
  symptoms. Placing the camera is its own unconditionally-called method now.
- **A region of interest centred on the camera is centred in the air.** The
  shadow frustum was aimed at the eye, the camera was three hundred units above
  Elwynn, and a box 220 units deep therefore held no landscape at all. Nothing
  failed: the pass ran, the map filled with the depth of empty air, and every
  surface was told it was lit. **What said so was the A/B** -- 162 pixels of
  576,000 differed with the feature switched off -- and the fix was to aim at
  the ground under the point ahead of the camera.
- **A fix that changes nothing at all refutes the diagnosis, not the size of
  the fix.** "Everything is shadowed" looked like acne; multiplying the
  receiver's normal offset by **fifty** changed the picture by exactly zero
  pixels, which is not what a bias problem does. What settled it was removing
  the suspects: `--max-doodads 0` made the shadowed and unshadowed renders
  *identical*, so terrain was not shadowing itself, and the 09:00 result was a
  45-degree sun behind a forest of forty-unit trees being right.
- **A rate limit and a lag are different failure modes, and only one is
  bounded.** Easing creature turns at a fixed maximum rate looks right every
  frame — until a player circling at melee range exceeds the cap, past which
  the error grows without limit. Closing a *fraction* of the remaining error
  bounds the lag at `omega * tau` for any `omega`.
- **A wrong constant is right for whatever it was written against.** The
  attachment sanity check capped offsets at 100 units, generous for a character
  and absurd for a 150-unit falling tree whose attachment sits at Z=127. A
  threshold that scales with its subject cannot make that mistake.
- **A rate below one per frame rounds to a feature that never runs.** A torch
  emits twenty particles a second; at 60fps that is a third per frame, and
  truncating each frame's share independently emits *nothing, ever*. Any
  per-frame budget derived from a per-second rate needs its remainder kept.
- **A streaming radius of zero is not a small world, it is a broken one.** It
  survived four milestones because `EVICT_MARGIN` retains a 3x3, so walking
  back the way you came looked fine. It is a **floor** in the constructor now
  rather than a default.
- **A thing's *owner* and a thing's *extent* are different.** A world object is
  filed under the tile containing its **origin**, correctly, so it is neither
  drawn twice nor left behind. Collision then chose tiles by where the
  *character* was — and Stormwind is one WMO placement 1,058 units across
  against a 533-unit tile, so standing over eight of its nine tiles the floor
  query asked tiles holding nothing. **The bug was not in the new feature that
  exposed it; what exposed it was new *ground*.** Tiles are selected by the
  bounds of what they hold now.
- **When one thing resolves another's ids, they have to come from the same
  owner.** Liquid frames resolved against the renderer's cache while the
  streaming draw needed the world's, so 2,398 triangles per tile were parsed,
  meshed, uploaded and never submitted — nothing, indistinguishable from the
  feature not existing. **A parameter that can be passed wrong is worse than no
  parameter**; the draw reads the cache off the world it is drawing.
- **Derive from the *same* source when two things must agree exactly.** The
  picking ray is unprojected from the very matrix the scene is drawn with, not
  rebuilt from the camera's angles — a ray off by a little lands clicks on the
  creature *beside* the one under the cursor, which reads as the server
  disagreeing about positions.
- **…but compare against something derived *independently* when checking
  whether something is correct.** The SRP6 tests carry a server written from
  the protocol, not from the client. The strongest version available here is
  the two-client rig: a structure goes out through one client and back in
  through another, confirmed via a third party that had to understand both.
- **Two copies of your own client are not two independent derivations.** That
  rig proves a *format* travels both ways and proves nothing about behaviour
  the two copies share. 3.5 declared replicated players smooth on exactly that
  evidence. **When the thing under test is timing rather than layout, one end
  has to be something you did not write.**
- **One dispatch table does not save a caller from ignoring what it produces.**
  `WorldState::replicate` is deliberately the only place opcodes are
  dispatched, and chat is *returned* rather than stored — three separate
  callers quietly dropped it.
- **A substring filter in a test rig will eventually match a person.**
  `--target Wolf` matched `Testwolf`, a character belonging to the person
  running the test, and the `.die` behind it killed them. Nothing
  malfunctioned. It cannot be fixed by choosing better search words, since a
  creature's name being a substring of somebody's character name is *how people
  name characters* — the selection helpers refuse players outright.
- **A green suite is a claim with a date on it.** `cargo test` was not green at
  `HEAD` despite a handoff saying so. And **the suite can fail without any test
  failing**: eleven GPU tests each built their own `wgpu` device and eleven
  concurrent DX12 creations deadlocked — thirty-seven threads with six seconds
  of CPU between them. It is a **race**, so the previous green run was luck; a
  shared `OnceLock` device fixed it and made them eight times faster.
  `RUST_TEST_THREADS=1` is a workaround living in an environment variable,
  which is precisely where nobody applies it on the run that matters. **A hang
  is not a slow run, and the tell is CPU time.**
- **`cargo build --release` does not compile test targets**, so "zero warnings"
  has never covered them. `cargo test --release --no-run` is what sees those,
  and a milestone-end check wants both.
- **Check that your check is current.** The first walk was declared a failure
  against the character list, which reports the last *saved* position; the
  movement had worked all along. **The same rule reaches the binary**: 4.22's
  three animation fixes came back reported as still broken, accurately, against
  a `wow-viewer.exe` built four hours before any of them existed. One `ls` on
  the timestamp settled it.
- **"It fails identically under every condition" is a claim to re-run, not to
  build on.** `foss-wow#55` rested on the failure being state-independent, and
  the premise did not reproduce: two occupied slots get a refusal and an empty
  destination gets **silence**. Sound reasoning from a wrong observation is the
  expensive combination, because the reasoning is what gets scrutinised and the
  observation is what gets believed. **A negative result that closes off a line
  of work earns one reproduction before it is written down.**
- **The absence of a warning is not the presence of success.** A counter that
  only speaks on failure cannot distinguish "none were wrong" from "there were
  none". Print both numbers, always.
- **Two bugs can share one symptom, and you will fix the innocent one.** M2
  geometry with the wrong winding culls front faces, which looks like a model
  *facing away from you* — so a half turn was added to entity facing and then
  propagated to doodads. Neither rotation was ever wrong. What separated them
  was fixing the winding first and then A/B-ing the rotation live, one variable
  at a time. **When a symptom persists across a fix that should have worked,
  suspect two causes rather than too small a fix.**
- **And one bug can produce several unrelated-looking reports.** A character
  sinking into the ground, a click marker landing off-centre, hills that could
  not be walked up, and another client seeing this one twitch were four
  complaints, none of which said "altitude", and one missing feature. **Before
  opening the second investigation, check whether the first cause reaches it.**
- **A live report closes the loop and names the next thing.** "Grass dirt roads
  all sound different but the steps don't respect buildings" is confirmation
  and the next ticket in one sentence. So is *"I honestly can't remember if you
  can hear anyone else's footsteps, it's so minor I never listened"* — a
  scoping decision no measurement could have produced. Take the second half as
  seriously as the first.

## Traps already hit

- **`mem::zeroed` on a struct holding a `String` takes the whole test binary
  down.** A zeroed `String` is a null pointer wearing a `String`'s shape, and
  the `Vec` behind it must be non-null: `wow-viewer`'s test binary died with
  `STATUS_STACK_BUFFER_OVERRUN` and no failing test name, which reads as a
  compiler or harness fault rather than as one line in one fixture. The
  compiler warns (`invalid_value`) and the warning was in the same output as
  the crash. Wire structs here have no `Default` on purpose; a fixture that
  wants one writes the fields out.
- **Never rewrite a file with a script that can throw mid-write.** A Python
  `write_text` containing a character the console codec could not encode
  truncated `docs/ROADMAP.md` to zero bytes. Prefer the editing tools; if a
  script must write, write UTF-8 explicitly.
- **And a script that writes cleanly can still rewrite every line.** This tree
  is LF; Python's `io.open(p, 'w')` on Windows is *text* mode and translates
  every `\n` to `\r\n`. Five files edited that way came back byte-different on
  every line, turning a 1,400-line diff into a 26,000-line one. The build and
  the tests are entirely happy, so nothing catches it but `git show --stat`.
  Open in **binary** mode, and read the stat before committing.
- **Do not run `cargo fmt` on this tree.** It is not rustfmt-clean and never
  has been: much of it is hand-wrapped in ways rustfmt disagrees with. One run
  turned a 190-line change into a 1,500-line one across thirteen files nobody
  had touched — the same shape as the CRLF trap, caught the same way. Format
  the lines you write to match their neighbours.
- `wgpu`/`egui`/`egui-wgpu`/`egui-winit` versions are coupled, and the
  `windows` crate needs a pin to build the DX12 backend at all — see
  `docs/RENDERING.md` before touching any of them.
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
- Commit messages explain the reasoning and record dead ends, so a later
  session does not repeat them. `git log` is part of the documentation, and
  **it is where the milestone narration that used to live in this file went**.
- Every milestone ends with a clean `cargo build --release` (zero warnings), a
  clean `cargo test --release --no-run`, and a full `cargo test --release` run.
