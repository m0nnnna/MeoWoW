# Roadmap

The ordering principle: **every milestone ends in something you can look at or
interact with.** A reimplementation of this size dies if it spends six months
in library code with no feedback, so each step below produces either a CLI
dump, a window, or a connection.

The two halves — reading the data files and speaking the network protocol — are
independent. Rendering comes first because it is the part that fails silently
if the parsers are subtly wrong, and because a world you can fly through is the
best possible test harness for everything that follows.

## Phase 1 — Data

| # | Milestone | Ends with |
|---|-----------|-----------|
| 1.1 | **MPQ archives** ✅ | `wow-cli info` / `ls` / `extract` over the real patch chain |
| 1.2 | **DBC tables** ✅ | Typed `Map`, `AreaTable`, `Spell`, `CreatureDisplayInfo`, `CreatureModelData`; `wow-cli dbc list/info/dump/rows/check` |
| 1.3 | **BLP textures** ✅ | DXT1/3/5, palettized, BGRA; `wow-cli blp info/export/survey` |
| 1.4 | **M2 models** ✅ | Geometry, submeshes, texture units, bone hierarchy, `.skin` LODs; `wow-cli m2 info/survey/creature` |
| 1.5 | **WMO objects** ✅ | Root + group files, materials, batches, doodad sets; `wow-cli wmo info/survey` |
| 1.6 | **ADT terrain** ✅ | WDT tile maps, height fields, alpha layers, placements; `wow-cli adt map/tile/survey` |

DBC comes before textures because nearly everything else is indexed by it — you
cannot place a model in the world without the tables that say which model goes
where.

## Phase 2 — Renderer

| # | Milestone | Ends with |
|---|-----------|-----------|
| 2.1 | **Window + wgpu device** ✅ | `apps/viewer`: textures on screen, egui overlay, headless `--screenshot` |
| 2.2 | **Static M2 rendering** ✅ | Textured creatures and doodads, orbit camera, depth-sorted batches |
| 2.3 | **M2 animation** ✅ | Keyframe tracks, external `.anim` files, GPU skinning, animation picker |
| 2.4 | **WMO rendering** ✅ | Buildings on screen, per-group isolation; portals and interior lighting still to come |
| 2.5 | **Terrain rendering** ✅ | Four-layer alpha blending on its own pipeline, correct tileset scale |
| 2.6 | **World streaming** ✅ | Fly across a continent; tiles load and evict around the camera, models cached across them |

Phase 2 is complete: you can fly across Azeroth with terrain, buildings and
doodads streaming in around you.

What is deliberately still missing from the renderer, in rough order of how much
it matters: liquid (`MH2O`, so Stormwind's harbour is a dry basin), WMO portal
culling and interior lighting, frustum culling, and shadows. None of these block
Phase 3, and all are easier to judge once there is a character standing in the
world.

## Phase 3 — Protocol

Independent of Phases 1–2; can proceed in parallel once there is appetite.
Target is a stock TrinityCore or MaNGOS 3.3.5a server.

| # | Milestone | Ends with |
|---|-----------|-----------|
| 3.1 | **Auth server** ✅ | SRP6 login, realm list, stage-aware refusals; `wow-cli auth <host>` |
| 3.2 | **World handshake** ✅ | RC4 header crypt, `SMSG_AUTH_CHALLENGE` → character list; `wow-cli world <host>` |
| 3.3 | **Enter world** ✅ | Login to a character, parse the initial object update; `wow-cli world --enter <name>` |
| 3.4 | **Movement** ✅ | Move, and be seen moving by a second client; `wow-cli world --enter <name> --walk <n>`, or W/S/A/D in the viewer |
| 3.5 | **Entity replication** ✅ | Live world state, and the viewer draws it: creatures and other players walk and turn to face the way they're actually moving, interpolated along each `SMSG_MONSTER_MOVE` path and animated with the model's own walk/stand cycles, updated every frame |

3.2 was expected to be the single hardest protocol step, and the header cipher
was indeed unforgiving — but not in the way budgeted for. The cipher itself
worked on the first attempt against a live server. What failed three times was
ordinary packet layout: a challenge sixteen bytes longer than expected, three
missing equipment slots, and a result-code enum offset by one.

3.3 went the same way. The object-update-field packing — the other half that was
budgeted as unforgiving — took one bug: a position block read as nine floats
instead of eight. Everything else parsed first time.

The pattern across both milestones is worth stating, because it should change
how the remaining protocol work is approached. **The hard-looking parts were not
the expensive ones.** Cryptography and bit-packing are precisely specified and
either work or fail loudly. What cost the time every single time was ordinary
struct layout, where a wrong guess parses perfectly and returns nonsense. The
cheap defence — parse through a cursor, assert the packet was consumed exactly —
caught all four bugs, and no amount of per-field validation would have caught
any of them. See `docs/PROTOCOL.md`.

The other lesson is that some failures are not bugs at all: the connection
dropping after three keepalives was the server enforcing a *minimum* ping
interval, not a parser losing its place. Rate limits and anti-abuse rules are
part of the protocol, and they fail in ways that mimic corruption.

### The renderer and the protocol have joined

Not a numbered milestone — it needed no new format work and no new protocol
work — but it is the point the project stops being two halves.
`wow-viewer --realm-host <host> --character <name>` logs in, enters the world,
and streams the map the server chose around the position it reported, with the
creatures it reported standing in it.

It was cheap, which is the interesting part: a `Map.dbc` lookup for the map id,
a reuse of the existing `--creature` model path for display ids, and no
coordinate conversion at all. The single bug was in the renderer and not the
join — a shared bone palette sized for one matrix, which silently collapsed
every skinned model to the origin. See `docs/RENDERING.md`.

### 3.4, and what closed it

The client moves, the server accepts it, and the position persists — verified by
walking a closed square and confirming each leg against the server's own reading
of where the character was.

The second half of the milestone — being *seen* moving — sat unproven for a
while, because it needs two accounts online at once. With a second account it
closed cleanly: one client walked 40 units while another watched, and the
watcher's independently decoded path matched the walker's own record exactly,
over 60 relayed packets.

That test is worth more than the milestone it closed. The movement structure
went **out** through one client, through the server, and **back in** through
another — so the write half and the read half of `MovementInfo` were confirmed
against each other through a third party rather than against themselves. This
project's standing rule is that a thing checked against itself is not evidence;
this is the strongest form of the opposite available without a reference client.

### 3.5: the renderer now draws what the protocol replicates

`crates/world/src/state.rs` maintains a live view of everything in range —
creates, field merges, movement, removals and creature paths — and the same
two-client rig verified it: the observer's replicated position for another
player matched that player's own record exactly, over 376 applied updates with
none undecodable and none orphaned.

The viewer used to draw only the entities from the login burst — a frozen
snapshot — because nothing folded later packets into a `WorldState` the
renderer could read from. It now does: `LiveWorld` keeps a `WorldState`
alongside the connection, every drained batch is folded into it the same way
`wow-cli`'s `replicate` does, and the instanced draws are rebuilt from it on a
timer rather than once at login. Verified with the same two-client rig used to
close 3.4 — one client walked while the other, running the actual `wow-viewer`
binary, drew it moving.

### 3.5 closes: interpolation, animation, and four bugs only live testing found

Getting a replicated entity from "jumps between positions" to "visibly and
animating" needed two pieces: `Entity::interpolated_position` lerps between a
monster move's `position` and `destination` over `move_duration`, keyed off
when the move was received (`move_started`); `World::update_animations` plays
the model's own walk or stand sequence, chosen by `AnimationData.dbc` id and
sampled every frame from a free-running clock.

None of the four bugs below showed up in a headless run or in `cargo test` —
a healthy live session never sends malformed input, and a screenshot cannot
show whether something *moves* smoothly. All four were found by literally
watching the window while a second client walked:

- **A creature that had ever moved never stopped "moving."** `destination`
  stays set to a move's endpoint until a fresher update replaces it, which for
  a creature at rest might be a long time — so gating the walk animation on
  `destination.is_some()` played it forever, with no idle state, for anything
  that had made even one move. `Entity::is_moving` checks whether the move's
  *duration* has actually elapsed instead.
- **A whole species animated if any one instance of it was moving.** Grouping
  and animating by display id alone means a zone with several wolves — almost
  always at least one of them wandering — showed every wolf, including the
  standing ones, playing the walk cycle continuously. Splitting each display
  id into a moving bucket and a standing bucket, each with its own instance
  buffer and its own bone buffer, fixed it; the model is already cached by
  display id, so drawing two buckets of one species costs a second cache hit,
  not a second load.
- **Entities faced one direction regardless of which way they walked.** The
  entity placement formula carried the doodad path's quarter-turn offset,
  which corrects for ADT rotations being measured from a different axis than
  an M2's forward — a fact about *that* data source. Network orientation
  already uses this codebase's own `(cos θ, sin θ)` convention directly
  against an M2's own +X-forward axis, so it needs no correction at all.
  `docs/RENDERING.md` had flagged entity facing as never checked against a
  reference client; it hadn't been, and the copied offset was wrong.
- **Motion looked stuttery, not merely coarse.** Position was rebuilt on a
  100ms timer while animation, once decoupled from it, ran every frame: legs
  mid-stride over a body that only advanced ten times a second reads as
  jankiness, not as a frame-rate problem. The timer existed to bound a
  rebuild cost that had never actually been measured against this project's
  entity counts (tens to a couple hundred); once removed, `set_entities` runs
  every frame too, and the stutter was gone. If a much larger population ever
  makes that measurably expensive, the fix is an in-place transform update,
  not the same timer again.

A second look at that facing fix found it was incomplete, and found something
worse alongside it: a claim in both this fix's own commit and in
`docs/RENDERING.md` that `SMSG_MONSTER_MOVE` "never reports" a facing was
false. Three of its five move types carry one — `FACING_ANGLE` as a bare
angle, `FACING_SPOT` as a point, `FACING_TARGET` as a guid — and the parser
skipped all three past the bug that made the claim look true: a stopped
creature has no destination to derive a heading from, so it fell through to
the raw wire position, whose orientation the parser always hands back as
zero, and snapped to face east regardless of which way it had been walking.
`FACING_ANGLE` was parsed into a new `MonsterMove::facing` field in response.
`FACING_SPOT` and `FACING_TARGET` remain unparsed — the former is a short
further step, the latter needs another entity's live position, which the
packet parser has no access to.

A third look found that parsing it was not the same as using it: the value
reached `Entity` but not the screen. `apply_monster_move` stored it into
`entity.position.orientation`, and `interpolated_position` never reads that
field while a path is in flight — it computes a fresh heading from direction
of travel unconditionally, at every `t` including `t == 1.0`, and the field
that held the parsed angle was really only consulted after a *later* update
overwrote it with something else. `destination` living well past its move's
actual duration (the exact fact `is_moving` exists to work around) meant
that "later update" might not arrive for a long time, so the parsed value
was live in memory and dead in practice — asserted only by the tests that
checked the parse boundary itself, not by anything that checked an entity
ended up facing it. Facing now has two regimes computed in one place,
`interpolated_position` itself: direction of travel while `t < 1.0`, and a
new `Entity::arrival_facing` — set from `MonsterMove::facing`, consulted only
once `t >= 1.0` — otherwise. That same fix closed a second, smaller gap in
the same function: a duration of exactly zero used to return the destination
verbatim, endpoint orientation and all, bypassing both the direction-of-travel
computation and the arrival-facing fallback in the one branch that skipped
both.

That second review also named a real deferral this milestone had made
without writing down: one bone buffer per `(display id, moving)` bucket means
every instance sharing a bucket — several wolves walking together, say —
animates in exact lockstep, identical phase, all at once. The bucket split
fixed standing creatures playing a walk cycle at all; it did not give each
instance its own, and per-instance phase would need either a bone buffer per
instance or CPU-side pose sampling packed by instance index — real
architecture work. Deferring it is still the right call. See
`docs/RENDERING.md` for where that call is now on record.

This milestone also changed what "careful" means. Every earlier parser was
memoryless, so a mistake produced one wrong answer and vanished. Replicated
state keeps mistakes: a dropped update is permanent and a bad merge erases
fields nothing will resend. The defence that worked was **accounting** rather
than parsing — count every change, tally updates that name unknown objects
instead of inventing them, and check that `created - removed` equals the number
of objects held. Those counters would have caught every replication bug this
project could plausibly have written, and none of them are assertions about
packet layout.

## Phase 4 — Game and interface

### The UI question is answered: native, and fully customisable

**This client draws its own interface. It does not reimplement `FrameXML`, and
it does not run addons.**

The alternative was reproducing Lua 5.1 driving an XML frame tree faithfully
enough that third-party code written against it keeps working — the hierarchy,
the event names and their argument order, the templates, taint, secure frames,
and a Lua runtime to host it. That is a large subsystem that draws no health
bar until most of it exists.

The trade taken instead: addons are given up, and what is paid back is that
**the interface itself is the customisation surface**. Every position, size,
colour and dimension lives in one text file the user owns, editable by hand or
by dragging frames around inside the running client. There is no fixed
appearance to patch around, because there is no appearance that is not a value
in that file. See `docs/UI.md`.

| # | Milestone | Ends with |
|---|-----------|-----------|
| 4.1 | **Interface layer + unit frames** ✅ | A player frame and a target frame on screen, click-to-target, `F1` to rearrange it all, saved to `ui.toml` |
| 4.2 | **Chat and names** ✅ | A chat window that talks, and frames that say `Young Wolf` instead of `Creature 299` |
| 4.3 | **Action bars and casting** ✅ | A spellbook off `SMSG_INITIAL_SPELLS`, three bars of twelve slots with real icons, keys `1`-`=` plus Shift/Ctrl, click-to-cast, hover tooltips, a cooldown sweep, and a cast bar off `SMSG_SPELL_START`/`SMSG_SPELL_GO` |
| 4.4 | **Combat** ◑ | Melee done: swinging, being swung at, a named combat log, and a dead unit in the frames. Spell damage, threat and the corpse/release flow remain |
| 4.5 | **Inventory, equipment, loot** | Bags, a paper doll, and picking up what a corpse dropped |
| 4.6 | **Quests** | The quest log and the give/turn-in conversation |

Sequenced by what is most visibly missing at the time, with one deliberate
exception: 4.1 came first because the interface is the frame everything else in
this phase is displayed in, and because it needed almost no new protocol —
health, power and level were already replicated. A milestone about the UI that
could not fail for protocol reasons was the right way to make the UI decision
real rather than theoretical.

### 4.1: the interface, and the two directions of one formula

`crates/ui` owns a retained layout: one `Element` (anchor, offset, scale,
visibility) per thing that can be drawn, plus the `Style` they all draw with,
serialised to `%APPDATA%\open-wow\ui.toml`. egui is the drawing and input
substrate only — frames are painted from explicit geometry rather than
assembled from egui widgets, which is what makes `scale` genuinely multiply
every dimension and the appearance a function of `Style` alone.

The care went where this project's own notes said it would. Anchoring runs in
both directions — anchor plus offset becomes a rectangle for drawing, a
rectangle becomes an offset for dragging — and that is the same shape as
"writing a format is riskier than reading it". Two separately written
conversions drift, and the symptom is a frame that creeps a few pixels every
time it is held, slow enough to blame on the pointer. They are one formula and
its inverse, round-tripped under all nine anchors.

Two things were nearly got wrong in ways that parse perfectly:

- **A unit's power is not `UNIT_FIELD_POWER1`.** The seven power fields are a
  parallel array indexed by the unit's own power type, from byte 3 of
  `UNIT_FIELD_BYTES_0`. Reading `POWER1` regardless is correct for every caster
  and reports zero for every rogue and warrior — which looks like replication
  failing rather than the wrong field being read. The index is bounds-checked,
  and the test states the case in the sharpest terms available: `UNIT_POWER1 +
  29` lands *exactly* on `UNIT_LEVEL`, so an unguarded read of an unfamiliar
  power type reports a unit's level as its mana. Plausible number, right range,
  wrong.
- **The picking ray is unprojected from the matrix the scene is drawn with**,
  not rebuilt from the camera's angles. The two agree only until someone
  changes the projection, and a ray that is off by a little is far harder to
  notice than one off by a lot: clicks land on the creature *beside* the one
  under the cursor, which reads as the server disagreeing about positions. The
  test projects a world point to a pixel and casts a ray back through it.

One bug was written and caught by its own test rather than by reasoning: the
compact slab test for ray-versus-box divides by the ray direction and lets an
infinite slope stand in for "parallel to these planes" — which is fine until
the ray also lies *in* one of them, where `0 * inf` is `NaN`, every comparison
involving it is false, and the box silently stops being clickable. A camera
looking along a world axis produces that case routinely. The parallel case is
now written out.

The fields half **is** confirmed against the live realm, through a dump command
that should have existed before any of it was wired into a renderer —
`CLAUDE.md`'s own rule, skipped once and repaid as `wow-cli world --units`:

- `Testwolf` reads race 1, class 1, gender 0, **power type 1**, with power
  `0/1000`. The character list independently calls it a Human Warrior, and
  1000 is rage's known scaling (100 rage stored times ten). Had the power array
  been indexed from `POWER1` regardless, the same warrior would have read
  `0/0` — the mana a warrior does not have. This is the check that would have
  caught the bug, run against a fact derived without reference to this code.
- Creatures read power type 0 with a maximum of 0, so they get the shorter
  frame with no power bar, which is what `UnitView::has_power` exists for.
- With `ACCOUNT34`'s `Watcher` online, the *other* player replicates the same
  way — race 1, class 1, gender 0, rage `0/1000` — so the fields the target
  frame reads survive the trip out through one client, through the server, and
  back in through another. Same shape of evidence as 3.4's movement test.
- `CMSG_SET_SELECTION` was sent for a real creature guid and the session
  survived it, held open half a second afterwards rather than disconnecting
  immediately — the trap that once got a facing opcode written off as wrong.

### And the live look, which found two more

The window was then actually watched, and it earned its keep exactly as 3.5
predicted. The frames, the picking, the drag/click split, `F1`, and saving all
worked first time — overlapping wolves and kobolds could each be selected
deliberately. Two things the tests could not have asked:

- **Nothing showed what was selected.** The target frame filled in, but out in
  the world there was no sign of which creature it belonged to. The fix is a
  bracket of four corner ticks — corners, not a closed box, which would hide
  the silhouette you were looking at when you clicked. It is built from *the
  same `hit_box` the ray was tested against* rather than a second measurement,
  so picking and its marker cannot drift apart; if they ever disagree, the
  marker is what shows it. It is painted onto a layer rather than into an
  `egui::Area`, because an Area claims the pointer over its own rectangle — and
  that rectangle surrounds a creature, so it would have made the thing you just
  selected the one thing you could no longer click.
- **Horizontal camera drag did nothing.** Vertical worked. The cause was two
  things writing one field: `drive_live_movement` rebuilds `fly.yaw` from the
  character's facing every frame so the camera follows, overwriting the yaw a
  drag had just written, while pitch survived because it is explicitly left
  alone. It presented as half a broken camera rather than as a conflict.
  Dragging now accumulates a `camera_yaw_offset` that is *added* to the
  character's facing instead of competing with it, so the camera swings around
  a character that keeps facing where it was.

Neither is exotic, and neither would have failed a test that was ever going to
be written. Both were obvious within a minute of looking. That is the third
milestone running where the live look found what the suite could not, and it
should be assumed for 4.2 as well rather than rediscovered.

## Non-goals

- Server implementation. TrinityCore and MaNGOS exist and are excellent.
- Any expansion other than 3.3.5a until 3.3.5a is genuinely playable.
- Distributing game content, in any form.

### 4.2: chat and names, and three ways to be ignored

`crates/world` gained `query` (the two name queries and their very differently
shaped responses), `names` (the cache that decides *what to ask and when*), and
`chat` (the most layout-dependent packet in the client). `crates/ui` gained a
chat frame that wraps and colours by kind, and the viewer gained an Enter-to-type
line that takes the keyboard away from movement while it is open.

The parsing went in without drama — the cursor-and-`finish` discipline is by now
routine, and the tests walk each variant of `SMSG_MESSAGECHAT` including the
ones that differ by a single field. What cost the time was **sending**, and it
cost it three times over, each failure looking exactly like the others: the
packet went out, the session survived, and nothing whatsoever came back.

1. `LANG_UNIVERSAL` is a GM's language. An ordinary account speaking it is
   refused with no reply.
2. **`CHAT_MSG_SAY` is `0x01`; `0x00` is `CHAT_MSG_SYSTEM`.** A client claiming
   to be the server is ignored. This project had already recorded a result-code
   enum off by one; this was the same mistake in a different table, and worse,
   because the symptom was nothing at all rather than a wrong value.
3. The line *was* being received, and thrown away by our own tooling — a drain
   done for another reason whose `Replication` was discarded. `replicate` has
   one dispatch table by design, but one table does not save a caller from
   ignoring a category it produces. The same bug existed in the viewer and was
   fixed before it could be found there.

The generalisable lesson, now in `CLAUDE.md`: **when a send produces no reply,
build an inventory of what did arrive before improving the guess.** `wow-cli
world --stay` now prints every opcode seen, decoded or not, because "never sent"
and "arrived and unread" are the same observation until something separates
them, and they want opposite investigations. The moment that histogram existed,
the answer took one run.

Two smaller things worth keeping:

- **Creature names are keyed by entry, not guid.** 131 replicated objects
  needed 50 queries; a zone of forty wolves costs one.
- **A chat line is rendered fresh every frame from the message that arrived,**
  not stored as the text it rendered to. A whisper comes from someone who may
  be nowhere near, so they were never in replicated state to be name-queried —
  the query only goes out *because* the line arrived. Rendering once on arrival
  stamped the guid in permanently and the name that turned up a moment later
  had nowhere to go. This was caught by watching the viewer's own log, not by a
  test.

Verified live: 50 names resolved with 50 queries and none unanswered; `--say`
echoed back through the server attributed by name; and `Watcher` on `ACCOUNT34`
whispered `Testwolf` on `ACCOUNT33`, arriving with `SMSG_MESSAGECHAT x1` in the
histogram — out through one client, through the server, back in through another.
A *yell* between the same two, 154 units apart, did not arrive at all while the
sender's own echo did, which is a fact about this realm's listen range rather
than about the parser, and exactly the sort of thing that would have been
blamed on the parser without the histogram.

### The live look, which found nothing

Worth recording precisely because the previous three milestones each had at
least one bug that appeared only on screen, and this one did not. Watched live:
the chat box drew where it should, a typed line went out and came back exactly
once (the client deliberately does not echo locally, relying on the server's
relay), typing did not walk the character, `Escape` cancelled, the target frame
named creatures properly, and an incoming whisper from a player 150 units away
— never in visibility range, never name-queried until the line itself arrived —
resolved from a bare guid to `Watcher` retroactively.

The interesting question is why this one came out clean, since the difference is
not care. It is that 4.1's bugs had already been converted into checks that run
without a window: a headless egui pass that asserts a frame painted at the
rectangle the layout chose, and received chat logged at debug as well as drawn.
The retroactive-naming bug was found by *reading the viewer's own log* before
anyone looked at the screen — which is the same class of bug as 4.1's missing
selection marker, caught a step earlier. The lesson is not that live testing
stopped mattering; it is that each live bug is worth converting into something
that would have caught it headlessly, because the next one of its kind then
shows up for free.

### 4.3, started: the protocol half of spellcasting

Done ahead of the interface, deliberately: the parts that can be verified
against a live realm without anyone at the screen are worth banking first, and
the action bar is the part that needs eyes.

`crates/world/src/spell.rs` parses `SMSG_INITIAL_SPELLS` — two counted lists
back to back, which is the whole risk: a wrong width in the first does not fail,
it reads the second from the middle of the first, and only the cursor running
out or having bytes left says so. It also writes `CMSG_CAST_SPELL` and reads
`SMSG_CAST_FAILED`.

The spellbook arrives **unprompted in the login burst and is never resent**, so
`WorldState::replicate` is the one place it can be caught. A caller folding
packets anywhere else would see a character who knows nothing, which reads as a
missing feature rather than a dropped packet.

Verified live: `wow-cli world --enter Testwolf --spells` returned 48 spells for
a level-one human warrior, and the ids are exactly right — `6603` Attack, `78`
Heroic Strike, `2457` Battle Stance, `196`–`203` the weapon skills, `20597`–
`20599` the human racials, and `668` Language: Common, which independently
confirms the language id 4.2 had to get right to speak at all. `--cast 78
--cast-self` was refused with a specific reason for the right spell id, which
is the strongest available proof that a fire-and-forget packet was understood:
a *specific* refusal means the server parsed it.

One deliberate non-feature. `describe_cast_failure` names exactly **one** reason
— the one actually observed — and returns the number for everything else. The
obvious move is to transcribe the whole `SpellCastResult` table, and this
milestone had just finished paying for precisely that habit with
`CHAT_MSG_SAY`. A wrong name here would be worse than a number: it does not
error, it confidently misexplains why a cast failed and sends the next reader
somewhere else. Reasons get words when they are seen, one at a time.

### 4.3 continued: the bars themselves

Three bars of twelve slots, reached by no modifier, Shift and Ctrl, matching the
number row `1`-`0`, `-`, `=`. Icons come from `Spell.dbc` -> `SpellIcon.dbc` ->
BLP, and **all of it is optional**: without a game installation a slot draws the
spell's abbreviated name instead, because an interface that requires assets
would be the one place "never bundle game data" turned into "does not run
without it".

Slot geometry lives in one function used by both the drawing and the hit test.
Two copies would agree until one changed, and the failure -- a click that casts
the spell beside the one you pressed -- reads as a targeting bug rather than a
layout one. Same rule as the picking ray.

**Deciding what belongs on a bar took three attempts, and the first two were
guesses.** Dropping passives removed the weapon skills and languages but left
`Opening`, `Closing`, `Duel` and `Honorless Target`. Membership in
`SkillLineAbility` changed *nothing* -- all the junk is in that table too. What
finally separated them was which line and whose: real abilities sit on the
class's own skill line with a matching `class_mask`, while every internal
effect sits on line 183, the generic catch-all, with a mask of zero.

The part worth keeping is what the data said about the obvious approach: **no
`Spell.dbc` attribute bit distinguishes them at all.** Heroic Strike is
`0x50014`, `Opening` is `0x190`, `Auto Attack` is `0x10`, and there is no bit
the wanted ones share that the junk lacks. No amount of care with those flags
would ever have worked, and only dumping the rows showed it -- which is why
`dbc rows` grew an `--id` filter.

### The 37-second login nobody had measured

Chasing why the bar filled half a minute after login found a bug that had been
there since 3.5 and had been silently paid on every viewer session since.

`Connection::drain` stops when the stream goes quiet *or* when a packet limit is
reached. Northshire emits a monster move about fourteen times a second and is
therefore never quiet, so the login burst ran until it had collected all 512 of
its packets -- **thirty-seven seconds, before the client drew a single frame**.
Nothing was wrong with the drain; its contract simply had no clock in it. The
burst is now bounded by wall time as well, and startup went from 37 seconds to
0.66 while still capturing 128 objects and all 48 spells.

Worth recording separately: the first diagnosis was confidently wrong. The
symptom was blamed on a slow `Spell.dbc` read blocking the render thread, with
an argument attached -- two runs agreed to the second, so it looked like a fixed
cost rather than network jitter. The load takes 185ms. One timing log around the
suspected culprit settled in a single run what the reasoning had got backwards.
Measure the thing, not the thing next to it.

**Not started:** cast bars, drag-to-assign, and `SMSG_SPELL_GO`/`SPELL_START`
(recognised by name, not parsed). See "4.3 finished: cast bars" below for how
that changed.

## The player is on screen

`wow-viewer --realm-host ... --entities true` now draws the character's own
body, in third person, with the world's creatures around it.

Two separate bugs, and the second was much bigger than the first.

**The body was excluded on purpose, by a comment that had gone stale.**
`drawable_entities` skipped the player's own guid because "the player's own
body is where the camera is; drawing it would fill the view from inside the
mesh" -- true when the camera flew freely, and untrue since the camera started
following nine units behind. The body is now built by `live::own_entity`,
which is deliberately *not* part of `drawable_entities`: the two have
different sources of truth. What the body looks like comes from replicated
state, which is authoritative and never changes. Where it *is* comes from the
viewer's own movement simulation, because the server never echoes our own
movement back -- the replicated position is frozen at login, and drawing from
it would leave the character standing at the login spot while the camera
walked away. The same trap had already cost a wrong diagnosis in `wow-cli`,
where an attack command measured its approach from that frozen position.

**And then: every replicated creature had been invisible in every headless
render since the feature was written.** Not the player -- all of them. A bone
palette is a fresh GPU buffer, a fresh GPU buffer is zeroed, and a zero matrix
multiplies every vertex to the origin. `--screenshot` places entities and then
renders a single frame; the only thing that ever writes a pose is
`World::update_animations`, which is called from the windowed frame loop and
nowhere else. So ninety-five creatures collapsed to the world origin, silently,
and a screenshot of Northshire came back as empty grass.

This is `CLAUDE.md`'s own "geometry drawn at zero size looks exactly like
geometry never drawn", recurring in a second place. It survived because 3.5 was
verified by *watching a window*, where the loop does pose them -- the one
observation that could not have caught it. Fixed twice over: the screenshot
path poses before it renders, and `create_bones` now fills the palette with
identities, so the same mistake made anywhere else draws a bind pose instead of
nothing. Anything that must be written before it is read should start as
something a person can see.

**The character is untextured**, and that is the honest current state rather
than a bug: a player's skin, hair and face come from `CharSections.dbc`, and
display id 49's `CreatureDisplayInfo` texture columns are empty because players
do not get their appearance that way. Every hairstyle geoset draws at once for
the same reason. Both are appearance, not placement, and both are next.

### The character is dressed

`apps/viewer/src/character.rs` turns the five numbers a player picked at
character creation into textures and geosets. The character now renders as a
recognisable human rather than a white mannequin wearing every haircut at once.

**The five numbers come from the character list**, which this client already
parses and has confirmed against a live realm. They also live in the player
object's update fields, at an index nothing here has verified -- and reading a
field whose offset was guessed is the failure this project keeps paying for.
The source that is already known to be right wins.

Column meanings in `CharSections` were read off the data, not transcribed: for
race 1 / sex 0, section type 0 yields `HumanMaleSkin00_00`, type 1
`HumanMaleFaceLower00_00` beside `...FaceUpper00_00`, type 2
`FacialLowerHair00_00`, type 4 `HumanMaleNakedPelvisSkin00_00`. Names that say
what they are is about as unambiguous as a column gets.

**Geoset selection took three renders and got it wrong twice, which is the
point of having a headless render.** A character model ships every hairstyle
and every beard in one file and expects the client to choose exactly one per
group, where a geoset id is `group * 100 + variant`.

- *Draw everything*: seventeen haircuts on one head.
- *Draw only what the character's numbers name*: the haircut was right and the
  character was wearing a large white sheet -- geoset 1501, equipment geometry
  for a cloak nobody owns.
- *Hide every equipment group*: the sheet went, and so did the forearms, hands,
  pelvis and legs. A floating torso with its hands and feet lying on the grass
  nearby. **Variant one of an equipment group is the bare body part**, not a
  piece of gear.
- *Show variant one of equipment groups, hide the rest, except group 15 which
  has no bare variant*: a complete human.

None of those four steps could have been distinguished by reasoning about the
table. Each took one screenshot. The two tests that then failed were the tests
being wrong, not the rule: geoset zero is the body and always draws, and group
8 ships `802` and `803` with no variant one at all, so its default is to draw
nothing. **A group's default is whatever the model actually contains**, not a
number this code assumes every group has.

**What is still missing, and it is one feature rather than several.** The face
is blank and the character is nude, because the face, facial hair and underwear
are *layers* meant to be composed onto the base skin -- `CharSections` hands
back `FaceLower`/`FaceUpper` and `NakedPelvisSkin` as separate textures, and
turning them into one skin needs a texture blit this client does not do yet.
The base skin is used alone until it does. Hair renders geometrically but
untextured for the same class of reason.

### The skin is composed, and every entity was facing backwards

The character's face, beard and underwear are now blended into its skin, and
the render found a bug that had been in the tree since 3.5.

**The composition regions were derived, not transcribed.** 3.3.5a ships no
`CharComponentTextureSections` table -- the layout lives inside the real
client -- so the regions are the classic 256-unit character layout doubled to
this build's 512x512 base skin. What makes that a measurement is that the
overlay files are *exactly* the sizes it predicts, three times over: face
upper 256x64, face lower 256x128, pelvis 256x128. A wrong layout would have to
be wrong by zero pixels in three independent places. Facial hair ships at half
resolution and is stretched over the same face regions, and is the one layer
with real alpha -- a beard blended as an opaque rectangle would be a box on
the chin.

**Then the screenshot showed the character's face, and the camera is behind
it.**

Entity headings were being applied raw, under a comment asserting that "an
M2's forward is +X" -- a comment which also admitted, in the same breath, that
facing had never been checked against a live reference. It could not have
been. The only entity whose heading this client *knows* is the player's own,
and until this milestone the player's body was not drawn. Creature headings
come from the server and there is nothing to check them against; a wolf's
silhouette does not advertise which end is the head at nine yards.

So **every creature has been facing backwards since 3.5**, and the milestone
that introduced facing was verified by watching exactly the thing that could
not show it.

What settled it, with two constants chosen independently: `wow-cli world
--face 0` turned the character to heading zero and the server confirmed
`facing 0.00 rad`; a screenshot with the camera at yaw zero -- which puts it
directly behind a character facing +X -- then showed the model's face. With a
half turn applied it shows its back. The server picked one number, the camera
picked the other, and they agree that an M2's forward is -X.

The lesson is not "check facing". It is that **a value with nothing to compare
it against is not verified by looking at it**, however carefully. Drawing the
player supplied the missing reference, which is why a rendering feature found
a protocol-adjacent bug that four milestones of watching had not.

### Placement rotation, settled against a real client

Reported from playing: "the church isn't facing the right direction", and
later, decisively: "the fences were like that before but now also 180". Both
were right, and the second is what broke the problem open.

The offset in `scene::placement_rotation` had shipped as `-90`. It was changed
to `+90` on the strength of a render of Northshire Abbey looking better. Both
are **90 degrees wrong**: they lay every fence in Elwynn across its own line
instead of along it. The correct value is `+180`, for doodads and world
objects alike.

**Why a building could not settle it.** A church has four sides and every
rotation shows a door to somebody. Comparing two candidate offsets against
each other says which is nicer, never whether the truth is outside the pair --
and it was. This is the same error as a test asserting a convention it also
defines, one level up, and it was made here twice in a row.

**What did settle it, in three independent measurements:**

- **Fence runs.** A fence is copies of one model laid end to end, so the run's
  direction comes from the placements' own *positions*, with no rotation
  involved at all. Across three runs at different angles, `direction - yaw` is
  one constant and `direction + yaw` is not: the yaw is not mirrored, and the
  offset is zero modulo a half turn. The model is 4.3 units long in X against
  0.3 in Y, so its long axis is local X, which is what makes that comparison
  meaningful. Now pinned as `a_fence_run_lies_along_its_stored_yaw`, built from
  the real placements.
- **A screenshot of the real client**, side by side with ours from comparable
  ground: the real one shows the abbey's portal where ours showed a windowed
  side wall. A 90 degree error, not the 180 that had been "fixed".
- **The lamp pillars.** The remaining half turn was decided by two things that
  cannot move: the lamps flanking the abbey steps are *doodads*, so their world
  positions are fixed whatever any rotation does, and the cobbled path is
  painted into the terrain. Both say the entrance faces the path. Only `+180`
  puts it there -- and the render then matches the reference photograph step
  for step, arch, doors, steps and lamps.

The lesson, which is now in `CLAUDE.md`: a movable thing checked against
another movable thing proves nothing. Find something nailed down.

## 4.4: melee combat

`wow-cli world --enter Testwolf --attack --stay 40 --capture <file>` walks a
level-one warrior into the nearest creature, swings, and writes every packet
that comes back. What it produces now:

```
You hit Kobold Vermin for 4.
Kobold Vermin hits you for 1.
You hit Kobold Vermin for 6.
Kobold Vermin hits you for 1.
You hit Kobold Vermin for 6. Killing blow.
```

Three packets are parsed in `crates/world/src/combat.rs`, all confirmed
against that capture, all with their regression tests built from its literal
bytes. `SMSG_ATTACKERSTATEUPDATE` carries a swing; `SMSG_ATTACKSTART` and
`SMSG_ATTACKSTOP` bracket the exchange and disagree with each other about guid
encoding -- the start uses raw guids, the stop packed ones -- which has its own
test, because sharing one parser between them would read a packed body as two
plausible wrong guids rather than failing.

**The outgoing opcode was confirmed by reaction.** `CMSG_ATTACKSWING` is the
only number in this milestone that could not be read off a capture, because
nothing acknowledges an opcode as such and an outgoing message can always be
misread as some *other* valid request. The first attempt swung from five units
away without turning to face the target, and the server answered with two
different empty-bodied refusals, three times each, and no damage at all.
Closing to melee and facing first turned exactly those into `SMSG_ATTACKSTART`
and fifteen swings. Silence would have proved nothing; a specific, repeatable
reaction that changes when the *conditions* change is the test.

**A layout that one capture cannot settle, and is not guessed.** Every swing
carried exactly one damage block, which means the block's width and the
packet's trailing fields are not separable: a 12-byte block with nine bytes of
tail and a 20-byte block with one byte of tail describe the same 42 bytes.
The 12-byte reading was chosen on evidence -- the field that separates a hit
from a miss reads `1` on all eleven landed swings and `0` on both misses,
which is a victim state; under the other reading the same bytes would be the
*absorbed* amount, and an absorb of exactly one on every landed hit is not
what absorption means. A single capture carrying two damage blocks would
settle it by its length alone, so `parse_melee_swing` errors on any count but
one rather than reading the wrong width in silence.

**The overkill field named itself.** It read zero for fourteen swings and `7`
for the fifteenth -- an 8-damage critical against a kobold with 1 health left,
and the fight ended there. No other reading of that field is consistent with
the fight it came from.

Three bugs, none of them in the parser:

- The test rig measured distance from *replicated* state, and the server never
  relays this client's own movement back to it. So after walking six units the
  client still believed it was at the login position, computed the next
  approach from there, and arrived out of range -- which looked exactly like
  the attack opcode being wrong. Creatures also wander, so one approach is not
  enough; it re-measures and repeats.
- `WorldState::casts` was keyed by caster guid and never pruned, justified in
  its own doc comment by analogy to `cooldowns` -- which is keyed by *spell
  id* and bounded at a few dozen by construction. Nothing visible broke,
  because a stale entry reads as a finished cast, which is precisely why it
  would never have been noticed. Both maps are now cleared when a unit leaves
  the world.
- And the one worth the most: the summary printed **"0 attacks started, 2
  stopped"**, which is impossible. The cause was this milestone's own approach
  loop folding a batch and discarding the `Replication` it returns. That is
  the *fourth* time a caller in this project has dropped a returned category,
  after chat, cast failures, and received chat in `wow-cli`. Extracting one
  `print_events` was the fix; the lesson stands where `CLAUDE.md` already puts
  it.

**Death is in the frames, and the subtle part is not the death.** A dead unit
is one with no health left -- but a unit whose fields have not arrived yet
reads zero too, because `hud::unit_view` renders an absent field as zero on
purpose. Testing `health == 0` alone would grey out every creature in range
for the moment after login, a hundred at once, and look like the feature
rather than the bug. `UnitView::is_dead` therefore requires a known maximum,
and the live run confirms it reads correctly: `Kobold Vermin  unit  2  0/55`
against three neighbours at full health.

### 4.4 continued: threat, power, and a packet that was seen and thrown away

Three more of the fight's opcodes now parse, two of them decided by data that
was already on disk.

**`SMSG_POWER_UPDATE` (`0x0480`) was confirmed from outside itself.** Read as
`{packed guid, u8 power type, u32 value}`, all thirty in one fight named this
client and carried power type `1`, and the last read **500** -- and `--units`,
whose numbers come from the entirely separate object-update path, reported that
character at `500/1000` rage at the end of the same run. Two parsers with no
code in common agreeing on a number neither could have taken from the other is
the strongest check this project has, and it is why the value is folded into
the entity's own field set through the new `Fields::set` rather than kept
beside it. One store means the two paths cannot drift.

**`SMSG_THREAT_UPDATE` (`0x0483`) had to be confirmed structurally**, because
nothing outside the packet knows what a threat number should be. Across a
fight every one named the kobold as the list's owner, carried exactly one entry
naming this client, climbed monotonically from 1360 to 4240, and consumed its
body exactly. A layout wrong about where the count sat would not produce a
clean tail on every packet, let alone a rising series.

**And the one worth the most: a 46-byte swing.** `SMSG_ATTACKERSTATEUPDATE`
turned up four bytes longer than every packet seen so far, and the cursor
discipline caught it as `4 trailing bytes left unread` rather than reading a
wrong field. Then it was lost -- `hold_connection` printed the refusal's
*length* and discarded its body, so the single packet that could answer the
question was seen and thrown away. This is the same shape of mistake as the
viewer counting decode failures without their bodies, one tool over, and it was
fixed the same way. Printing refused bodies caught four more in three fights.

All four carry `hit_info = 0x2002`, and all 35 shorter packets have that bit
clear -- a perfect correlation across 39 swings, which is enough to gate the
extra `u32` on the bit rather than on how many bytes happen to remain. What the
number *means* is not settled: it read `2` on two swings that dealt no damage
and `1` on two that dealt 5 and 4, which is consistent with an amount blocked
and with several other things. So the flag is named `CARRIES_EXTRA_AMOUNT`, for
what it does to the layout, and the field stays a number.

The test rig also stopped being a coin flip. A creature wanders while the
approach walk runs, so a swing sent at where it *was* is refused for range and
nothing happens -- which reads as the command being broken rather than the
target having moved two yards. `--attack` now re-swings until swings actually
come back, and says so when they do not.

**Not done:** spell damage (`SMSG_SPELLNONMELEEDAMAGELOG`), the corpse and
release flow, and telling the two swing-refusal opcodes apart -- no experiment
has separated them, because neither carries a payload and both conditions were
violated at once. Combat has also not been *watched* in the viewer yet: it is
verified headlessly and through the CLI.

### 4.4 continued: floating combat text

Every swing in `Replication::swings` now spawns a number above whoever it hit
-- a landed hit, a critical (larger, its own colour), or `Miss`, rising and
fading over `Style::combat_text_lifetime_ms`. Drawn the way the target marker
is, not through an `Element`: a number belongs to a swing, not to a fixed spot
on screen, so `apps/viewer/src/hud.rs`'s `combat_text_anchor` re-projects one
fixed world point every frame exactly the way `marker_rect` re-projects the
selection box, and for the same reason -- a world position measured once
and a screen position recomputed as the camera moves cannot drift apart the
way two independently-tracked ones could.

The position is captured **at the swing**, from the victim's replicated
position, and never re-read afterwards. A killing blow's number has to keep
rising after the corpse it came from can no longer answer
`interpolated_position` -- tracking the entity live would make the last
number of every fight disappear early, which is exactly the swing worth
seeing land.

**`MeleeSwing::extra_amount` names nothing here either.** The number drawn is
always `swing.damage` or the literal text `Miss`; `extra_amount` is not
substituted into the label, the colour, or the count of numbers spawned. The
same reasoning as `describe_cast_failure` and the field's own doc comment in
`crates/world/src/combat.rs`: four captures are not a confirmation, and a
combat log -- or a floating number -- that guesses "blocked" would misexplain
a fight to whoever is reading the screen, not just whoever is reading the log.

Verified headlessly, the same way the rest of 4.4 has been: `crates/ui`'s
`combat_text_paints_something` and `an_aged_combat_number_rises` pin that an
entry actually reaches the screen and that age moves it upward, and
`apps/viewer/src/hud.rs`'s `combat_text_anchor_follows_marker_rects_own_rule_about_the_camera`
pins the same behind-the-camera refusal `marker_rect` already has. Not yet
watched live -- see the paragraph above.

### 4.4 continued: dying, and the two states that look like one

The corpse flow starts by being killed, and **a death cannot be captured without
dying**, so `wow-cli world --until-death` exists to pick fights until the
character loses. Getting it to work cost five separate bugs, none of which
errored and all of which presented identically -- as combat not happening:

- **Distance measured from replicated state**, which is frozen at login because
  the server never relays this client's own movement back to it. Every approach
  was computed from the login position, so the walk arrived out of range. This
  is the *same* bug 4.4 already paid for once in `--attack`, reintroduced in a
  new function; `nearest_ordered_from` now takes the origin as a parameter and
  the doc comment says why.
- **The rig walked away from the thing killing it.** At four health it picked a
  fresh wolf thirty units off, which broke combat, and health regenerated on the
  way. It now prefers whatever is already attacking us, wherever it is in the
  list.
- **Altitude.** `Connection::walk` advances x and y and carries the starting Z
  along, so after a ninety-unit walk over Northshire's hills the client was
  *telling the server* it was six yards in the air. The server measures melee
  range in three dimensions, so a swing from two yards away and six above is
  refused exactly like one from eight yards away -- printed as `dz 5.1` beside a
  flat distance of 2.2. The rig now takes the target's Z as ground truth, on the
  grounds that creatures stand on the ground. That is the fifth report tracing
  back to the same missing feature.
- **A ghost has one health.** Four runs were spent swinging as a ghost, because
  `1/79` reads as a warrior about to fall over. Every attack came back
  `SMSG_ATTACKSTOP` with no `SMSG_ATTACKSTART`, no swings and *no refusals* --
  indistinguishable from the attack opcode having stopped working. The character
  list had said `ghost` on every one of those runs and nothing read it.
- **Auto-attack wins fights.** `CMSG_ATTACKSWING` starts a *repeating* attack,
  so a command whose entire purpose is to be killed was efficiently killing each
  attacker and regenerating between them; two runs ended with more health than
  they started. It now swings once to be noticed and then sends
  `CMSG_ATTACKSTOP` and soaks. Health fell 54 to 0 in three rounds.

**Dead and ghost are two states, and one snapshot said otherwise.** Diffing a
living character's update fields against a dead one's -- the technique that
settled `PLAYER_BYTES` and the game object display id -- showed a field
appearing exactly when that character stopped being alive. Complete, plausible,
and wrong: that character had already *released*. Killing a second character and
looking again showed it dead with health `0` and that field **absent**. Written
down after one observation it would have been labelled "dead", and would have
been silent for the entire window between dying and releasing -- which is the
window a corpse run happens in.

Six snapshots, three characters, two accounts, two zones, a warrior and a druid:

| state | health | `0x04AD` | `0x0096` | corpse object |
|---|---|---|---|---|
| alive (3 characters) | > 1 | absent | absent | none |
| dead, not yet released | `0` | `0x08` | absent | **none** |
| ghost, released (2 characters) | `1` | `0x08` | `0x10` | present |

`0x04AD` is the *only* field whose presence separates alive from not-alive with
no exceptions. Absence is meaningful rather than unknown, by the rule
`PLAYER_BYTES` established: a create block carries only non-zero values, so a
field missing from a living player's set is a zero.

Two independently-derived structures agree with the split. The character list's
`flags & 0x2000` is parsed out of `SMSG_CHAR_ENUM` with no code in common with
the update-field path, and it reads `ghost` for the two released characters and
not for the freshly killed one. And the corpse *object* only exists after
releasing -- a player lying dead has none in view, which is also why the corpse
run needs the distinction rather than merely liking it.

**Neither flag is named beyond what was measured.** The constants are
`PLAYER_NOT_ALIVE` and `PLAYER_GHOST`, one observed bit each; what the rest of
either field holds is not established and is not guessed.

**What the death itself sends** is a differential rather than a reading: the
same character, same zone, same creatures, one run that died and one that did
not. Nine opcodes appear only in the run that died. Two are worth naming as
observations, and neither is yet parsed -- `0x0269` carries a single `30000`,
which is a thirty-second timer arriving at the moment of death, and `0x0484`
carries `{creature guid, our guid}` twice, which is the shape of a unit's threat
list losing us. The remaining seven are recorded in the capture and left alone.

### 4.4 finished: the corpse run, and a local realm to find it with

A character can now die, release, run back to its body and take it. Every step
was confirmed against a **local AzerothCore** rather than the shared test realm,
and the change of venue is most of the story: on the remote realm each death
cost a five-minute fight and consumed a character's state permanently, so a
wrong guess was expensive and a *second* observation of anything was rare.
Locally a death is a GM command, a resurrection is another, and the same
scenario can be run twenty times.

The two writes and what they cost:

- **`CMSG_REPOP_REQUEST`** carries one byte the server reads and discards.
  Sending an empty body is not the same thing -- the read happens before any
  check, so a zero-length body is a short read rather than a request.
- **`CMSG_RECLAIM_CORPSE`** carries the corpse's guid *unpacked*, eight plain
  bytes, unlike almost every other guid this protocol sends.

Both are refused in silence -- seven separate conditions between them, none of
which says which -- so `--release` and `--reclaim` report a before-and-after of
the things that must change rather than claiming success.

**Three bugs, each of which made the previous one look finished.**

The first attempt released before the character was dead, because the flags ran
in declaration order and the kill is a GM command sent as chat. It produced
nothing at all, which is what a wrong opcode produces -- and the reclaim-delay
packet arriving afterwards, proving the death had happened, is what separated
them.

The second was found by a number that did not add up. The reclaim *succeeded*
from 58 yards away when the server's limit is 39. The cause was
`MSG_MOVE_TELEPORT_ACK`: the server moves a released ghost to the graveyard and
waits to be told the client noticed, **and discards every movement packet until
it is** -- so the ghost had never left the corpse, the range check passed at
nought yards, and the whole feature looked complete. Acknowledging the teleport
made a passing test fail, correctly, and turned the corpse run into a real one.
The same obligation applies to *any* teleport, so a viewer that ignored it would
freeze server-side while walking its camera around; that is fixed in the same
change.

The third was picking the wrong body. Corpse-shaped objects include the bones of
bodies already reclaimed, they all carry their owner's guid, and a graveyard
accumulates them -- one run saw nine, five of them ours. Choosing by owner sent
the run back fifty-eight yards to a previous death site. `MSG_CORPSE_QUERY` asks
the server which body is current, which is both simpler and correct; the
replicated objects then only supply the guid, chosen as the one nearest the
answer.

**The graveyard needs no `WorldSafeLocs.dbc`.** `SMSG_DEATH_RELEASE_LOC` carries
the map and position of the grave the server picked. The obvious design is the
opposite -- look up the nearest row and walk there -- and building it that way
would have put a table on the critical path of a feature that needs none. The
table is wanted later only to put a *name* on a place we are already given the
coordinates of.

**The reclaim delay is not a constant.** Observed at 30s on a first death, 60s
on the second and 120s on the third: it stacks. Anything that hardcoded thirty
seconds would have worked exactly once per character, which is precisely how
often it would have been tested.

### An entire population can still be one sample

The sharpest lesson of the milestone, and it invalidated a conclusion this
document previously recorded as solid.

Field `0x04AD` bit `0x08` was identified as "not alive" from six live snapshots
-- three characters, two accounts, two zones, a warrior and a druid -- where it
was the *only* field separating alive from not-alive with no exceptions. That is
a broad population by every measure this project usually applies.

It is the release-timer display flag. Every one of those six came from the same
*path*: an ordinary death. A GM resurrection produces a living character at full
health with the bit still set, and no amount of watching people die naturally
could have shown it. **Breadth in characters, accounts, zones and classes reads
exactly like rigour, and none of those was the dimension that mattered.** The
question to ask of a population is not how large it is but how many different
ways the value could have been arrived at.

What survived the correction: `PLAYER_FLAGS` bit `0x10` really is the ghost
flag, and dead-versus-ghost really are two states. `Entity::is_dead_or_ghost`
now reads the two things that do mean it, and the rewritten test immediately
caught a second bug in the replacement -- it checked `health == Some(0)`, but a
dead player's health *is* zero so the create block omits the field and it reads
`None`. That is this project's own "an absent field is a zero" rule, which the
live capture had shown plainly and the code still got wrong.

### 4.3 continued: tooltips, a cooldown sweep, and a width that was never 16

Hovering a slot now shows the spell's name, rank and description, straight off
the four `Spell.dbc` columns `spells.rs` already loaded and had nowhere to put.
The immediate reason this was worth doing rather than deferring further:
`Activate Primary Spec` and `Activate Secondary Spec` genuinely share
`spell_icon_id` 2970, so two slots on a level-one warrior's own seeded bar are
pixel-identical, and the name was the only thing that could tell them apart.
Confirmed live -- hovering both slots on `Watcher`'s bar read as distinct.

**The descriptions were still templates, and that live look could not have
noticed.** `Spell.dbc` stores them with substitution tokens: `Heroic Strike`
reads "increases melee damage by `$s1`" and `Battle Shout` reads "within `$a1`
yards by `$s1`. Lasts `$d`". The two spells the live check hovered --
`Activate Primary Spec` and `Activate Secondary Spec` -- are the only ones on
that bar with no tokens at all, so the one observation made was the one that
could not have shown this. Slot 1 on the same character is `Heroic Strike`,
and it had read `$s1` the whole time. Now substituted; see below.

### 4.3 continued: the numbers behind the tokens

`apps/viewer/src/spelltext.rs` fills the tokens in. 82% of the 22,633
token-bearing descriptions in build 12340 now resolve completely, and the
remainder keep every character they had.

**No column here was transcribed.** A wrong offset in a name column produces a
visible mess; a wrong offset in a *damage* column produces a confident number
that gets believed, which makes this the worst possible place to write down a
layout from memory. So each one was located by a property of the whole table
and the test is recorded on the column in `dbc::schema::Spell`:

- **`$s1` -> column 80, stored one below what it displays.** Of the 3,775
  descriptions reading `$s1%`, that column plus one is a multiple of five for
  69.3% of them -- and without the plus one, for 5.2%. Game percentages are
  round numbers, and a thirteen-fold split settles the column and the offset
  together. `$s2` and `$s3` then separate 81 and 82 at 78% and 80%, which is
  what makes it an array rather than one lucky column.
- **`$d` -> column 40.** Non-zero in 98.5% of descriptions saying `$d` against
  39.0% of those that do not, resolving to 98.7% whole seconds. The naive test
  -- "which column holds a valid `SpellDuration` id" -- picked a *different*
  column at 99.6% and was pure coincidence: any column of small integers hits
  somewhere in a 130-row table. Asking whether a column is non-zero *because*
  the spell has a duration is the question that discriminates.
- **`$a1`/`$t1` -> columns 92 and 98**, by the same lift test at gaps of 79 and
  90 points, with their `2`/`3` variants separating their own columns.
- **`$M1` -> column 74**, which first tested *flat*: counted over every
  description mentioning `$m1` it looked no different from background. That
  was the wrong question -- a single quoted value needs no die. Restricted to
  the 88 descriptions quoting both `$m1` and `$M1`, a genuine range, it
  exceeds one in 96.6% against 24.9%.

**What is deliberately not resolved is as much of the design as what is.**
`${$m1+0.15*$SPH}` arithmetic, `$<mult>` variables, `$?[a][b]` conditionals,
`$g`/`$l` gender and plural forms, and every token whose column was never
confirmed (`$h`, `$n`, `$x`, `$i`, `$u`, `$o`) pass through untouched, `$`
included. A visible `$s1` tells the next reader a feature is missing; a
number substituted from a guessed column is indistinguishable from a correct
one. The same reasoning that keeps `describe_cast_failure` down to the one
status code actually observed.

Two bugs, both found by tests rather than by looking. Brace expressions were
*documented* as skipped and not actually skipped, so the scanner walked into
`${$m1+0.15*$SPH}` and resolved the token inside it -- half-substituted, which
reads as a finished sentence that is wrong about the other half. And the fix
for it dropped the leading `$`. Neither would have been obvious on screen.

The nicest confirmation was not planned. `Rejuvenation`'s duration resolved to
15 seconds where the test expected 12; its tick-period column reads 3000ms,
and the description's own hand-written `*5` is exactly the number of ticks
that implies. Two columns found by separate statistical tests agreeing with a
literal a Blizzard designer typed is a stronger statement than either test
made alone -- the same "compare against something derived independently" that
the SRP6 tests are built on, arriving for free.

Also converted to a headless check, per the rule that live-only tests do not
stay live-only: `a_hovered_slot_explains_itself` drives a real pointer over
slot 1 and asserts the spell's *full* name reaches the painted shapes, which
the slot itself never draws -- it shows an icon or the abbreviation `HS`. It
needs four egui passes rather than the two the other frame tests use: hovering
is not known until a pass has registered where the widgets are, and a tooltip's
first pass is egui's invisible sizing pass. Read at two passes it reports a
tooltip that is genuinely on screen as missing, which is the same
paints-versus-computes gap this crate's other tests already watch for, one
layer further out.

The cooldown sweep darkens a slot by the remaining fraction of its cooldown,
timed from when this client learned about it rather than from the server's own
clock, which it never sees. `SMSG_SPELL_COOLDOWN` (`0x0134`) is parsed for
this; `WorldState::replicate` folds it in next to `INITIAL_SPELLS`, the same
dispatch discipline as everything else this crate replicates.

**Verifying it found a real bug in code this milestone did not otherwise
touch.** `SpellCooldown` -- the *other* cooldown list, the one already inside
`SMSG_INITIAL_SPELLS` -- had sat at a 16-byte-per-entry guess (`item_id: u16,
category: u16, cooldown_ms: u32, category_cooldown_ms: u32`) since 4.3 began,
transcribed from public documentation and never actually exercised: every
prior login had zero cooldowns, which tests a list's *count* field but nothing
about its *entry width*. The first login that carried a real one -- a
level-one warrior on `wow1.nekos.farm` who had just cast racial `59752`,
"Every Man for Himself" -- failed to parse at all: `needed 4 bytes at offset
325, packet holds 325`. The packet's own count field said 4 entries in exactly
32 remaining bytes, which divides evenly at 8 bytes each and not at 16, and
the first entry's first word decoded to `59752` itself -- the exact spell just
cast. The width was wrong, confirmed by the one packet that had ever actually
tested it, exactly the shape `CLAUDE.md` already names: *a wrong width in the
first list does not fail, it reads the second list from the middle of the
first* -- here it was the *second* list whose own internal width was wrong,
caught by the same cursor discipline one level down. Fixed to the confirmed 8
bytes (`spell_id: u32`, and a second word), re-verified against a fresh login
carrying the same character's cooldown, and pinned as a regression test built
from the literal captured bytes so the width cannot silently drift back.

What the second word actually *measures* stays open, and says so in the code
rather than guessing: it read `0` for a cooldown that was demonstrably active,
which rules out "milliseconds remaining" taken at face value, and a second
capture returned three different not-quite-spell-looking numbers for the same
racial. This project's rule against transcribing an unverified table applies
exactly as much to a field's *meaning* as to a status code's *name* --
`describe_cast_failure` already lives by it, and this is the same shape of
carefulness one struct over. The field is named `second`, not `cooldown_ms`,
until an observation actually pins it down.

**`SMSG_SPELL_COOLDOWN` itself was never seen.** Two live casts of the same
racial, one held open six seconds afterward, both genuinely started a
cooldown -- confirmed by the *next* login's spellbook carrying it, and by an
immediate recast being refused -- but neither capture contained opcode
`0x0134` at all, only `SMSG_SPELL_START`/`SMSG_SPELL_GO` and an unrecognised
`0x0496` twice. Either this realm does not send it for this ability, or the
opcode number itself is wrong. The parser and its fold into `WorldState` are
therefore exercised only by unit tests today, and say so in their own doc
comments -- the next person chasing a cooldown sweep that never appears should
start there, not assume the sweep's arithmetic is at fault.

### 4.3 finished: cast bars

Closing the one thing 4.3 was still missing meant answering the question the
`SMSG_SPELL_COOLDOWN` work above left open: `SMSG_SPELL_START` and
`SMSG_SPELL_GO` are two opcodes this crate had recognised by name since 4.3
began and never parsed, and public documentation for their layout exists but
had never been checked against this realm -- exactly the situation `CLAUDE.md`
warns is the most dangerous kind, a wrong offset that parses without failing.

**So it was captured before it was written.** `wow-cli world --cast`'s drain
was temporarily widened (900ms to 4000ms, so a multi-second cast had time to
land) and given a one-line hex dump for opcodes `0x0131`/`0x0132`, cast
`5185` (Healing Touch) at self as `Testdruid` on `wow1.nekos.farm`, and reverted
the instant the bytes were in hand -- the same discipline as capturing the
`SMSG_SPELL_COOLDOWN` regression bytes, just done *before* guessing a layout
rather than after one broke.

The capture decoded cleanly, with **zero leftover bytes on either packet**:
two packed guids, a cast-count byte, and the spell id are byte-identical
between `SMSG_SPELL_START` and `SMSG_SPELL_GO` -- both `5185` -- which is what
let `parse_spell_go` share `read_cast_header` with `parse_spell_start` rather
than re-deriving it. `SMSG_SPELL_START` then carries an opaque flags word, a
cast time (`1500`ms, a plausible Healing Touch cast time), and a target;
`SMSG_SPELL_GO` carries its own flags word, a hit list, a miss list, and a
server timestamp, before the same target shape. Both end in one more `u32`
that lines up with a predicted-power guess (`100` for `START`, `81` for `GO`)
but is kept as a plainly-named `trailing` field rather than asserted to be
that, on the same reasoning `SpellCooldown::second` already lives by.

**The target was never `target_flags::SELF`, even for `--cast-self`.** Both
packets named the caster's own guid through an explicit `target_flags::UNIT`.
No capture of an actual `SELF` target, an item target, or a positional target
exists, so both parsers refuse anything but `UNIT` with a specific error
rather than silently misreading whatever would follow a shape nobody has
observed -- the same restraint `describe_cast_failure` applies to status
codes, one struct over. `SMSG_SPELL_GO`'s miss list gets the identical
treatment: the one capture on hand had a miss count of zero, so a nonzero one
is refused rather than parsed against a guessed per-entry width.

**A cast bar must not survive a packet it cannot read.** Because
`SMSG_SPELL_GO` is refused for any shape this parser has not confirmed, a real
miss on a real cast could in principle leave `SMSG_SPELL_START`'s entry
un-cleared forever. Rather than accept that, `WorldState::casts` follows
`cooldowns`' own precedent: entries are never pruned by the caller, and
`WorldState::active_cast` instead reads `None` once `progress_fraction` would
reach `1.0`, whether or not `SMSG_SPELL_GO` ever arrived to say so explicitly.
A stuck cast bar is therefore not a failure mode this client has, independent
of how much of the wire format around misses ever gets filled in.

The bar itself (`crates/ui/src/frames/cast_bar.rs`) fills left-to-right as the
cast completes -- the opposite direction of the action bar's cooldown sweep,
which darkens as a spell becomes ready again, because the two are measuring
opposite things. It is absent unless a cast is in progress, the same
absent-unless-relevant rule the target frame already follows, with the same
edit-mode placeholder so it can be positioned without waiting for a live cast.
Two headless tests follow this crate's existing shape: one pins the
appears-only-while-relevant behaviour the same way
`unit_frames_appear_without_data_only_while_editing` does, and
`a_cast_bar_fills_as_the_cast_progresses` pins the fill the same way
`a_cooldown_darkens_the_slot` pins the sweep -- painting more shapes at
`0.6` progress than at `0.0` is proof the fill reached the screen, not only
that the arithmetic behind it was right.

### The character stands on the ground, and four reports were one bug

Altitude had been left at whatever the server last reported since 3.4 -- an
explicit deferral, written down in `docs/RENDERING.md` as expected until
height-following existed. What made it worth doing now was not the deferral
coming due. It was that **four separate things reported as wrong turned out to
be the same missing feature**, and not one of them said "altitude":

- the character sinking into the ground;
- the click marker landing off-centre -- the picking ray starts at the eye, and
  the eye is a fixed offset from a position whose Z was wrong;
- walking *into* hills rather than up them;
- another client watching this one twitch, as the server corrected an altitude
  that had been drifting for a while.

That is the mirror of a rule already in `CLAUDE.md` -- two bugs can share one
symptom, so a symptom that survives a fix may have a second cause -- and the
reverse is just as expensive: four symptoms with one cause invite four separate
investigations, three of which are into things that were never broken. The
click marker in particular reads as a picking-ray bug, which is where the
search would have gone.

`World::height_at` resolves a position to its tile, its chunk and the surface
inside it. **The interpolation lives in `crates/adt`, not the viewer**, because
the awkward part is the lattice convention that crate already documents: 145
samples per chunk, a 9x9 outer grid interleaved with an 8x8 inner one at the
cell centres, both axes running inwards from the chunk's corner. Sampling is
the *drawn* surface -- four triangles fanning from each cell's inner sample,
matching `emit_chunk_indices` -- rather than a bilinear patch across the outer
four, which would ignore the inner sample and flatten every ridge running
through a cell centre. A character standing a little above or below ground it
can see reads as the terrain being wrong, not as two different surfaces.

**Holes return `None`, and so does a tile that is not resident.** Both mean the
same thing to the caller and get the same treatment: leave the altitude alone.
The server's Z is stale, but it is a real place; a guess is not. A hole is a
doorway or a cave mouth, where the floor is WMO geometry nothing here can yet
be asked about.

**What confirmed it was not a screenshot.** Three checks, none of which needs a
window, and the strongest of them needs nothing but the map files:

- 37,080 of a real tile's 37,120 height samples resolve to their own recorded
  vertex position -- the remaining 40 fall in genuine holes. That pins the
  index convention against `vertex_position`, which is what the renderer builds
  its mesh from.
- Northshire's doodads were placed by an artist standing them on this surface,
  and they arrive through a different chunk of the file in a different
  coordinate convention. 706 of 759 sit within a unit of the interpolated
  ground; the median offset is zero. An axis swap does not shift that median by
  a metre, it destroys the relationship.
- The realm reports the human starting position as `-8950.0, -132.5, 83.5`.
  `wow-cli adt height Azeroth --x=-8950.0 --y=-132.5` answers **83.528** from
  the map files alone. Two derivations sharing nothing -- one a value stored on
  a server, one an interpolation over an ADT -- agreeing to three centimetres.

The CLI command exists for the same reason every other format got one first:
"the height is wrong" and "the position landed on the wrong tile" are the same
observation until something separates them, and `adt height` prints the tile
and the chunk it went through, not just the answer.

### Running, walking and standing

The same rebuild now picks between three cycles instead of two. `moving` as a
flag could not choose: a wolf on patrol and a wolf charging are both moving,
and committing to one cycle for both is wrong either way round -- walk, and the
charge is dragged along by its own legs; run, and the patrol skates ahead of
them.

**The speed is not on the wire.** `SMSG_MONSTER_MOVE` carries two endpoints and
a duration; the speed fields in a unit's update block describe what it is
*capable* of, which is not what it is doing -- a creature ambling home moves at
a fraction of its run speed without either number changing.
`Entity::move_speed` divides the path by its duration, which is the only
statement about the move actually in flight, and is the same pair of numbers
the position being drawn was interpolated from. `Motion::from_speed` splits at
4.75 units per second, the midpoint of 3.3.5a's 2.5 walk and 7.0 run, which
also leaves the 4.5 backing-up speed on the walking side.

A model with no run sequence falls back to its walk one. Nothing falls back as
far as standing: a unit sliding along in its stand cycle is the 3.5 bug
inverted, and worth failing loudly-looking rather than quietly. `sequence_for`
resolves that fallback once and both `set_entities` and `update_animations`
consult it, because a disagreement between the buffer that gets created and the
pose written into it would not error anywhere -- it would pose one cycle into a
buffer drawn as another.

The player's own body took the same change for free, and revealed a small lie
in the process: `LIVE_WALK_SPEED` was 7.0, which is the *run* speed. The
character has been running since 3.4 while playing a walk cycle. Renamed, and
now it runs.

### Everyone in the world has a skin

Reported as one thing -- "NPCs are blank white ghosts" -- and it was two,
with different causes and different fixes. The report also named "player hair"
as missing, which turned out not to be a defect at all.

**The NPCs.** `docs/RENDERING.md` had said for several milestones that humanoid
NPCs render white because character models composite their skin at runtime and
no compositor existed. That is true of a *player* and false of an NPC:
`CreatureDisplayInfoExtra.bake_name` names a texture of the finished character,
armour and all, already composed by an artist and shipped in the archives. The
deferral was real; the reason recorded for it was wrong, and a wrong reason for
a deferral is worse than none, because it makes the work look bigger than it is.

15,446 of 24,262 display ids -- 64% of every creature appearance in build 12340
-- have an extended row and no texture variation of their own. That is the
population that was white.

The near-miss worth recording: a coverage check said only 0.1% of the named
bakes ship, which would have sunk the approach. It was built on `wow-cli ls`,
and an MPQ resolves by hash rather than by listing -- the baked textures are in
the archives and absent from the listfile. Forty of forty randomly sampled names
read back fine. **Listing a directory and reading a path are different
questions**, and the cheap one answered the wrong one convincingly.

**The other players.** Once the NPCs were fixed, exactly one white figure
remained in the Northshire scene, and it was not an NPC: display 49, which is
every human male alive. A player's appearance is five numbers in their update
fields, and this client had only ever read its *own* from the character list.

Rather than transcribe the documented field index, it was searched for: the same
five numbers arrive by two unrelated routes, so `wow-cli world --appearance`
packs the character list's answer and asks which field holds it. The first two
runs proved nothing, because every character this project has ever created was
made with an all-zero appearance and a search for zero matches every zero field.
`--create` grew appearance flags; a character made with five *different* values
matched exactly one field, pinning the byte order as well as the index.

**And the same tool then found the bug in the fix.** A stranger was still white,
and `--appearance` showed `PLAYER_BYTES` unset while `PLAYER_BYTES_2` was
present. **An absent update field is a zero, not an unknown** -- a create block
carries only non-zero values -- so refusing on absence left exactly the
default-looking players white. Both directions were then observed: the field
appears when non-zero, and is omitted when zero.

Verified through the two-client rig. A character was created on one account with
skin 3, face 5, hair 7, colour 2, facial hair 4; a viewer on the *other* account
read those five numbers back off the wire and resolved them to
`HumanMaleSkin00_03.blp` and `Hair02_02.blp`. Out through creation, through the
server, back in through a different client -- the strongest shape available here.

**The number that says it worked**, and the reason it exists: on one Northshire
scene of 17 drawn entities, no entity has an unfilled body texture, where before
exactly one did. `load_dressed` had always collected the list of textures it
could not resolve, and every caller had always dropped it -- so a white creature
was invisible in the logs, which is the same failure as the packet body this
project once refused and threw away. `World::entity_model` now warns with the
display id and the slot, and that warning is what turned "one thing looks wrong
in a screenshot" into "one entity, display 49, missing slot type 1".

**Player hair was never broken.** Human-male hairstyle 0 is bald:
`CharHairGeosets` maps only variation 0 to geoset 0, and all thirteen colours of
that variation in `CharSections` have an empty texture. The character being
looked at had chosen it. Worth the twenty minutes it took to establish, because
the alternative was "fixing" a lookup that was already right.

**Not done:** equipment. Slot type 2 -- the object/item skin -- is still
unfilled on twelve of those seventeen entities, which is why NPCs wear their
armour as a painted-on texture with no sleeves or boots to it, and why the
player is in underwear. `CreatureDisplayInfoExtra` already carries eleven item
display ids per NPC, read and named here but unused. Lighting and the day/night
cycle are untouched, and game objects are still not drawn at all.

### Equipment, the texture half

Clothes before geometry, because that is where the visible difference is: the
player was standing in Northshire in its underwear, and a character's armour is
in the first instance eight texture patches blended onto the same composed skin
the face and underwear already use.

`ItemDisplayInfo` and `Item` are transcribed, and both were verified rather than
trusted:

- The eight component columns **name themselves**. Every value carries its
  component as a suffix (`..._Chest_TU`, `..._Pant_LL`), and across 57,986 rows
  each column is 98.9%-100% dominated by exactly its own suffix in order. That
  is a stronger check than any external documentation, because it is the data
  agreeing with itself.
- The regions those components land on were pinned by aspect ratio -- hand,
  torso-lower and foot are 4:1, the rest 2:1 -- and by the fact that all ten
  regions **tile the 512x512 skin exactly**, which a layout guessed one region
  at a time would not do. There is a test for it now.
- `Item.display_info_id` was picked out **against a control**: 100.0% of its
  46,096 values are real `ItemDisplayInfo` ids, where the item id in column 0 --
  a number of the same magnitude from an overlapping range -- manages 89.6%.
  This project has already been burned by a column that looked valid because any
  small integer points somewhere inside a big table, so the gap is the argument,
  not the hit rate.

**No slot enum was transcribed.** Items are painted in the order the character
list sends them, and that order is already inner-to-outer wherever two items
share a component -- shirt before chest, bracer before glove, trouser before
boot -- so the layering falls out of the wire order.

**The best thing to come out of this was `--skin-out`.** At walking distance the
dressed character looked bare-chested, and the obvious diagnosis was that the
torso regions were wrong. Dumping the composed atlas to a PNG showed every one
of the ten regions painted correctly, with the torso wearing a white shirt and
brown braces that simply read as skin at that size. **The render was right and
the look at it was wrong** -- the inverse of the usual failure here, and it would
have cost an afternoon of moving correct regions around. A composite assembled
from a dozen files in memory needs a way to be seen as itself, not only as three
hundred pixels of character on a screen.

**Not done, and each is its own piece of work:**

- **Geometry.** Sleeves, boot tops and glove cuffs that stand off the body are
  geosets, switched on by `ItemDisplayInfo.geoset_group_*` against the item's
  inventory type. Which group each applies to is client logic rather than a
  column, so it wants rendering to settle, exactly like the four attempts
  hairstyles took.
- **Weapons, shields and shoulders**, which are separate M2s on attachment
  points -- `model_left` and `model_right` are read and named but nothing draws
  them.
- **Other players' equipment.** Their visible-item fields carry item *entry*
  ids, which is why `Item.dbc` is transcribed here at all; the field indices
  want the same search treatment `PLAYER_BYTES` got, against a character whose
  gear is known from the character list.

### Fixed: other players jump rather than walk

Reported from live play and not caught by any test here: another player vanishes
from one spot and reappears further along their path, with no animation playing.

The cause is traced and recorded in `foss-wow#22`. Briefly: a creature moves by
`SMSG_MONSTER_MOVE`, which carries a start, an end and a duration, and that is
the packet `Entity::interpolated_position` was built around. A player moves by
relayed `MSG_MOVE_*`, which carries a position and no path, so
`WorldState::update_movement` stores it and calls `clear_predicted_move`. With
no duration there is nothing to interpolate along and nothing for
`Entity::move_speed` to divide, so the player snaps between points and
`Motion::from_speed` picks the stand cycle. Two symptoms, one cause, again.

**The interesting part is why 3.5 passed.** That milestone was verified with two
clients, one walking while the other drew it -- and both of them were *this*
client, which sends a movement heartbeat every 100ms. A snap of a hundred
milliseconds between two nearby points reads as movement. A real client sends
roughly every 500ms, and at that spacing the missing interpolation is
unmistakable, which is exactly how it came in.

So the two-client rig has a limit worth naming, because it has been described
here as the strongest evidence available and mostly is. It confirms a *format*
travels both ways, through a third party that had to understand both halves. It
confirms nothing about behaviour the two copies share -- and identical timing is
the most obvious thing two copies of one binary share. When what is under test
is timing rather than layout, one end has to be software this project did not
write.

**The fix builds a path out of two samples rather than predicting one.**
`WorldState::apply_relayed_movement_at` keeps the previous sample as the start,
takes the new one as the destination, and uses the interval between them as the
duration -- which is all `interpolated_position` and `move_speed` ever needed.
Nothing is extrapolated forward. Every position drawn is one the server actually
reported, and the price is that a mover is drawn one sample-interval behind.
That trade is deliberate: dead-reckoning ahead from the movement flags and the
speed fields would remove the lag and start inventing positions, and a player
invented into a wall is a bug nothing here can check, where a player drawn half
a second late is merely late.

**The interval comes from the mover's own clock**, `MovementInfo::time`, not
from when the two packets reached us -- that is the one measurement of how the
sender actually spaced its samples, where arrival times also measure the network
and our own scheduler. Three things fall back to the old snap: an interval too
short to be a sample, one long enough that the mover was standing still or out
of view, and a distance no legitimate movement covers in the time, which is a
teleport and must not be drawn as a walk. The fallback being *exactly the
previous behaviour* is the point -- a mover whose clock this client cannot make
sense of is no worse off than before the fix.

`PathFacing` came out of the same work, and is the half that a monster-move
mental model gets wrong. `SMSG_MONSTER_MOVE` reports no orientation at either
end, so facing has to be inferred from the direction of travel. A relayed
`MSG_MOVE_*` carries the mover's *own* orientation at both ends, which is a
different thing: a player strafing or walking backwards faces somewhere other
than the way they are going, and inferring it from the path would spin them
round. The enum makes `interpolated_position` ask which kind of path it is
holding rather than assume.

**And this time the live-only bug became six headless tests**, per the standing
rule: two samples become a walked path with a real speed (the assertion that
would have caught the stand-cycle half), a reported facing beating the direction
of travel, a turn across zero going the short way, a teleport snapping, a long
gap snapping, and a relayed path superseding a stale monster move.

**What the live check then measured, and what it found instead.** A run of
`wow-cli world --enter Watcher --stay` beside a real 3.3.5a client reported
`92 sample(s), 63 walked (mean interval 795ms)` -- and the mover was a level-one
human *rogue*, class 4 with energy, which is no character this project has ever
created. That is the independent end the two-client rig could never supply.

But the same run's opcode census is the more useful half, and it corrects that
number completely. This client folded exactly **three** relayed movement
opcodes. One real client walking about produced **five more** carrying the
identical body, every one discarded -- including `0x00DA`, which on its own was
**93% of that client's entire movement stream**. So 795ms was never how often a
real client samples itself; it was the gap between the 7% this client bothered
to read.

`wow-cli moves <capture>` settled which, and the method matters more than the
answer. It asks which opcodes carry `{packed guid, MovementInfo}` *structurally*
-- the body parses, it consumes to the byte, and the guids it names are few and
repeating -- using the shipped parser rather than a second copy written for the
tool. Across one 180-second capture:

| opcode | packets | parse as movement | bytes left over | movers |
|---|---|---|---|---|
| `0x00B5`, `0x00B7`, `0x00EE` (folded) | 68 | 68 | 0 | 1 |
| `0x00B6`, `0x00B8`, `0x00B9`, `0x00BA`, `0x00DA` (dropped) | 1,202 | 1,202 | 0 | 1 |
| `SMSG_MONSTER_MOVE` (control) | 944 | 75 | 358 | 23 |

The control is what makes it evidence rather than a hit rate. Any body of about
the right length parses as *something* -- the same trap as a column of small
integers landing inside a 130-row table -- so the argument is the gap between a
layout that consumes exactly and names one mover, and one that does neither.
**None of the five is given a name**, because nothing here established which
movement each is; they render as `MSG_MOVE_* relayed (0x00da)`, which says the
packet is understood and its name is not. A fabricated `MSG_MOVE_START_STRAFE_LEFT`
would say neither.

**And measuring the rate immediately falsified a constant written from
intuition.** `MIN_INTERVAL_MS` was 40ms, on the reasoning that nothing samples
itself faster. The heartbeat this client already reads has a *minimum* of 500ms
and a median of 1,140ms, which is where "roughly every 500ms" came from and is
correct as far as it goes. `0x00DA`'s median interval is **21ms**. The floor
would have rejected most of the real stream and snapped it -- reinstating the
defect the surrounding code exists to remove, while guarding against something
that never happens. It is now 1ms, and there is a test holding the measurement.

**Three wrong readings of the capture file, none of which errored.** The tool
that produced the table above got the answer wrong twice first, and both times
confidently. `opcode::describe` renders an unknown opcode as `opcode 0x00da` --
two tokens -- so splitting the line on whitespace and taking the third field
landed on the length for exactly the opcodes under investigation, shifting every
body by a byte and reporting all of them as "not movement". Reading the body as
the trailing run of two-hex-digit tokens then failed differently: a 32-byte
packet's length is `32`, which is itself two hex digits. The fix anchors on the
length by making it *agree with what follows it*, scanning left to right --
because a body ending in `00` also satisfies "parses as zero, and zero tokens
follow", so a right-to-left scan finds a byte instead. A tool whose own output
format defeats its own reader is the same failure as printing the length of a
packet you refused, one layer up.

### 4.4 continued: a spellbook, and the ability the filter had to reject

Combat could be driven from `wow-cli` and not from the client, for a reason
that had nothing to do with the protocol. Auto-attack is spell 6603, `Testwolf`
has known it since the character was made, and `Connection::attack_swing` has
worked since 4.4 opened -- but there was no way to put it on a bar. The bars
were filled once at login by `App::seed_action_bars` and never touched again,
so whatever that one filter rejected was unreachable from inside the client
however much the character knew.

**And it rejected auto-attack necessarily, not by oversight.** 4.3 established
that a spell earns a slot by belonging to the character's own skill line --
`Opening`, `Closing`, `Duel` and `Honorless Target` are not passive and belong
on no bar, no attribute bit separates them from `Heroic Strike`, and what does
separate them is that the junk all sits on `SkillLineAbility`'s generic line
183 with a class mask of zero. `Auto Attack` sits on line 183 with a class mask
of zero:

```
SkillLineAbilityRow { id: 3999, skill_line: 183, spell_id: 6603,
                      race_mask: 0, class_mask: 0 }
```

So the mechanism that correctly keeps a warrior's bar free of junk is the same
mechanism that hides the one ability every character in the game uses. That is
a nicer shape of problem than it looks: the rule is not wrong, it is merely
complete, and the fix is either to widen it or to name the exception. Widening
it to admit line 183 readmits `Honorless Target` with it. Naming the spell
admits exactly what was checked -- so `spells::AUTO_ATTACK` is a hardcoded
6603 with its evidence beside it (row named `Auto Attack`, attributes `0x10`
so not passive, description *"Automatically attacks a target in melee with an
equipped weapon until cancelled"*), and the test asserts **both** halves
against the real archives: the one spell is admitted, and the five junk spells
it is structurally indistinguishable from are still refused. A test of the
first half alone passes just as well under the wrong fix, which is the whole
reason the second half is in it.

**The general answer, though, is the book.** A per-spell exception fixes one
spell; a list you can drag from fixes the category, and the category is real --
a seeded bar is a guess about what a player wants, and no filter is going to
get that right for every class. `crates/ui/src/frames/spellbook.rs` opens with
`P`, a left click picks a spell up, a left click on a slot puts it down, and a
right click on a slot empties it.

**A slot still stores a plain `u32`.** The obvious shape was to give a slot an
action *kind* -- `Spell(u32) | AutoAttack` -- and that would have been a
serialisation change to a file users already have. Which message a slot sends
is a fact about the *spell*, not about the slot, so it is derived at the point
of sending instead: `ui.toml` is untouched, every existing layout still loads,
and a bar arranged by hand in the file behaves exactly like one arranged
in-game.

**Auto-attack is a state, so its slot toggles**, and whether it is on is read
out of `WorldState::attacking` rather than kept in a local flag. This is the
same reasoning that put teleports on `WorldState` instead of returning them:
the server ends an attack on its own when the target dies or walks out of
range, and a local flag would be inverted from that instant. The next press
would send a stop for a fight that was already over -- and since a refused
attack is silent on the wire, that reads as the key having failed.

#### The tests that would have caught it, written before it happened

4.2 shipped with no live bugs because 4.1's failures had been converted into a
headless egui pass. The same conversion is done here up front, because every
part of an assignment gesture is invisible from outside: `drive` delivers real
egui events across the passes a click actually takes (a press lands on the
rectangles the *previous* pass registered, and `clicked()` reports on the
release), and the checks are that clicking a spell then a slot puts it on the
bar and reports the layout changed, that a slot clicked with nothing held is
still a cast, that right-click empties, that closing the book drops what was
held -- and that a **scrolled** book picks up the spell under the cursor.

That last one earned its place immediately. The row index and the entry index
are deliberately different things, and the test that pins them apart caught a
different bug on its first run: the scroll offset is a `usize`, and applying a
wheel delta by casting it to `i32` and adding turns a large offset into `-1`.
The offset is clamped every frame so it can never legitimately *be* large --
but a cast that is only safe because of an invariant somewhere else is exactly
the kind of thing that stops being safe quietly. It is a saturating add and
subtract now.

#### What was checked live, and what was left to be

Against the local AzerothCore realm, with `APPDATA` pointed at an empty
directory so the layout was genuinely fresh and the seeder actually ran:

```
in world as Testwolf on map 0 at -8935.3, -188.6, 80.4
login burst: 71 packets, 84 objects, 54 spells
action bar seeded from 54 known spells (5 castable):
    Auto Attack, Heroic Strike, Battle Stance, Activate Secondary Spec,
    Activate Primary Spec
```

Auto-attack leading the bar of a real character, from the real archives, over a
real connection -- the whole data path from `SMSG_INITIAL_SPELLS` through the
filter to a slot, confirmed without a person at the window. What that run
cannot confirm is a keystroke and a click, so the swing itself is still a live
look: target a creature, press its key, and watch `SMSG_ATTACKSTART` and the
combat log. `--screenshot` renders no egui, so this is the boundary of what is
automatable here today.

### 4.5: the controls — strafing, jumping, steering and a camera that zooms

The client could walk forward, walk backward, and turn. That is genuinely all
it could do: `Q`, `E` and `Space` were fly-camera leftovers that did nothing at
all while connected, so there was no strafing, no jumping, and no way to point
the character with the mouse. Getting combat to a state worth testing made the
gap obvious -- circling a target is how melee is actually played.

**Movement stopped being a heading and became two axes.** The old state was
`Option<LiveMove>` with `Forward | Backward`, which cannot express running
forward *and* sidestepping -- a thing a player does constantly. `Motion` holds
both axes, and the wire agrees: there is a start/stop pair per axis, and
`MSG_MOVE_STOP` ends only the longitudinal one. A character that stops running
while still holding a strafe key keeps strafing.

**The opcode names the axis that changed; the flags carry the whole state.**
Beginning to strafe while already running sends `MSG_MOVE_START_STRAFE_LEFT`
with *both* bits set. Sending only the bit matching the opcode would tell the
server the character had stopped running the instant it started strafing --
and, movement being unacknowledged, the symptom would be a drift with no
error anywhere.

#### Where the numbers came from, which is the part worth being careful about

Five opcodes and two flags were needed and none of them were in the tree. This
is the situation `CHAT_MSG_SAY = 0x00` came out of, so nothing was transcribed
from memory.

The AzerothCore source is on disk and reading it is authorised, so it made the
hypothesis cheap: `Opcodes.h` gives the strafe block `0x0B8`/`0x0B9`/`0x0BA`,
`MSG_MOVE_JUMP` `0x0BB` and `MSG_MOVE_FALL_LAND` `0x0C9`, and `UnitDefines.h`
gives `MOVEMENTFLAG_STRAFE_LEFT` `0x4` and `_RIGHT` `0x8`. Two things then made
that more than a transcription from a different source:

- **Every constant this project already had agrees with the same enum.**
  `FORWARD` `0x1` through `SPLINE_ENABLED` `0x8000000` were established
  earlier and independently, and all nine match. Reading a bit off the end of a
  confirmed run is a different act from guessing at it.
- **The capture already said which opcodes were movement.** `wow-cli moves`
  had found five unnamed opcodes in a real client's stream whose bodies parsed
  as `{packed guid, MovementInfo}` and consumed to the byte -- `0x00B6`,
  `0x00B8`, `0x00B9`, `0x00BA`, `0x00DA`. Three of those are exactly the strafe
  block. The capture could say *these are movement* and not *which* movement;
  the source names them; the two agree.

And one reading was checked and came back *against* the obvious answer.
`MovementHandler.cpp` writes a jump block as `sinAngle, cosAngle, xyspeed,
zspeed` -- a different order from this project's `Falling`, which looked like a
bug worth fixing. It is not: that is a different packet. The canonical
`MovementInfo` codec in `WorldSession.cpp` reads `zspeed, sinAngle, cosAngle,
xyspeed`, which is exactly what `world::movement::Falling` already had. The
near-miss is the point -- a grep that finds the field names is not the same as
a grep that finds the *structure*, and "fixing" the correct one would have been
silent.

#### Confirmed by relay, because an outgoing opcode cannot be confirmed any
other way

Nothing acknowledges movement, and a wrong outgoing number is read as some
*other* valid request rather than refused -- the same problem `CMSG_ATTACKSWING`
had. So the check is the two-client rig, which is the one shape here where the
write half is confirmed through a third party that had to understand both.

`wow-cli` gained `--strafe left|right` and `--jump` for exactly this, and they
drive the *same* `Motion` the viewer does rather than a second copy -- which is
why `Motion` lives in `crates/world` despite being fed by a keyboard. Two
mappings from movement state to flags and opcodes would agree until one of them
changed, and this rig exists to check the other.

One session moved while a second, on a different account, logged what the
server relayed:

```text
--jump                     --strafe right --walk 15
0x00bb  x1  (jump)         0x00b9  x1  (start strafe right)
0x00c9  x1  (fall land)    0x00ba  x1  (stop strafe)
```

Exactly one of each, in order. A body the server could not parse as a jump
would not have been relayed *as a jump* to somebody else.

The strafe also has a confirmation involving no opcodes at all, which is
better still because it tests the *direction* rather than the framing. Facing
4.61 rad, strafing left 20 units moved the server's own position from
`-8939.3, -197.5` to `-8919.4, -199.5` -- +19.9 in x, -2.0 in y, **orientation
unchanged**. Sideways without turning is something a forward walk cannot
produce at any heading. Strafing right moved it back -14.9 in x, the mirror.

#### The rest of the controls

- **Right-drag steers the character**, left-drag still swings the camera around
  it. Deliberately two verbs, as in the game this is modelled on: collapsing
  them would lose the ability to look sideways while running straight. While
  steering, `A`/`D` become strafe keys -- and are then *stopped* from also
  turning, because a key that turned the character and pushed it sideways at
  once would send it round in a circle, and the cause reads as a mouse problem.
- **The wheel zooms** from 2.5 to 30 units back while following a character,
  and still trims fly speed when there is no character to follow. The near
  limit is not zero on purpose: this client draws the player's own head, and a
  true first-person view is a screenful of the inside of a face.
- **Autorun** on `Num Lock`, cleared by pressing a key that means stop.

#### Two bugs found by writing the tests, before either could be seen

The arithmetic went into `crates/world/src/motion.rs` with ten tests precisely
because a movement bug is invisible until a server disagrees with you, and both
of the mistakes made here were in the packet-ordering rather than the maths:

- **The landing was written as an `else` of the key-transition branch.**
  Releasing a key on the very frame the ground was reached would have sent the
  key change and swallowed the `MSG_MOVE_FALL_LAND`, leaving the server holding
  a character it believed was still in the air. They are two different facts
  about one frame and both have to travel; the landing is unconditional now.
- **`A`/`D` did double duty while steering**, turning the character *and*
  strafing it.

And the jump arc integrates with the midpoint term rather than stepping the
height by an already-updated velocity, with a test asserting the peak is the
same at 20fps as at 240. A jump whose height depends on the frame rate is a
fault that only ever appears on someone else's machine.

#### Right-click, which is two gestures and three jobs

The controls above shipped with right-drag steering and nothing else on the
right button, which is half of what that button does in the game. Right-click
is *select-and-do-the-obvious-thing*: it targets what is under it, and then
performs the default action -- attack a creature, talk to a vendor, loot a
corpse. Left-click only ever selects.

Telling the two apart needs no new machinery, because the left button already
had this exact problem: a press and a release in the same place is a click, the
same two events with movement between them is a drag, and nothing but the
distance separates them. So the right button mirrors that structure rather than
inventing a second one.

**Mirroring it turned up a bug in the original.** The left button cleared
`last_cursor` on release -- and the *next* press reads `press_at` from that
same field. Two clicks at the same pixel with no movement between them
therefore had the second one silently discarded: `press_at` was `None`, so the
release had nothing to measure against and no click was ever reported. With a
left click that is a selection nobody repeats, which is why it survived four
milestones unnoticed. Right-click-to-attack is a gesture people *do* repeat on
the same pixel, precisely when it seems not to have worked -- which is exactly
when it would have gone on not working. Nothing needed the field cleared;
`CursorMoved` fires whether or not a button is down.

**Attack is offered on a rule that is deliberately not a hostility test**, and
the distinction matters. A unit's faction arrives as `UNIT_FACTION`, but
turning that into "hostile *to me*" needs `FactionTemplate.dbc`, which is not
transcribed. Inventing the judgement here is the fabricated-number problem one
layer up: a client that decides a guard is hostile does not display a wrong
number, it attacks the guard. So `is_attack_candidate` rules out only what is
never a fight on any reading -- yourself, a corpse, a bench, anything already
dead -- and lets the server arbitrate the rest, which it does anyway and is the
only party that actually knows. The cost is that right-clicking a friendly NPC
sends a swing the server refuses. That is the honest failure; the alternative
was an unprovoked attack.

`FactionTemplate.dbc` is the follow-up, and it is the right shape of work for
this project: a table whose meaning has to be confirmed against live data
rather than transcribed, with a ready-made control group -- the guards and the
innkeeper in Northshire against the wolves and kobolds outside it.

Right-click also had to *start* a fight rather than toggle one, so
`toggle_auto_attack` split into `start_auto_attack` and `stop_auto_attack`.
Right-clicking the creature you are already fighting must not call the fight
off, which is what a toggle bound to the same gesture would have done.

### 4.6: things die, and a fight looks like a fight

Reported from playing it, and the first two are one problem: **the player
stands still while the fight happens**, and **mobs reach zero health and go on
standing**. The animation system knew about exactly one input -- ground speed
-- so a creature that stopped moving was standing, whether it had stopped
because it was idle or because it was dead.

**The architecture had a shape that made this awkward, and the shape was
right.** Bone poses are shared: every instance in a `(display id, motion)`
bucket is drawn from one buffer, which is what keeps ninety-five creatures
affordable. That works for loops because everything standing is in step
anyway. It breaks for a death, because two wolves die at different moments and
cannot share a pose unless they are at the same point of the same cycle.

The fix is to put **when the cycle started** in the key. Units that died
together share a bucket and units that died a second apart do not, and the
count of live buckets is bounded by how many things died in the last few
seconds. The stamp is *absolute* rather than an age, which is the whole trick:
an age changes every frame, so it would rebuild the bone buffer every frame and
quietly turn the cache into a cost. There is a test pinning that specifically,
because the wrong version would have looked perfect.

**Four states now, not three.** `Dying` plays the fall from the moment it was
seen; `Dead` is settled; `Attacking` plays a swing from the moment it landed;
`Ready` is the guard-up stance that a fight is mostly *made of* -- a swing is
an instant and a fight is a minute, so the swing animation alone would still
have left the character at ease for nine tenths of it. Precedence is death,
then the swing, then the guard, then speed; and a swing only interrupts
standing, because a creature charging you swings as it runs and the swing
played over a run reads as a stumble.

#### The table already knew the fallbacks

`AnimationData.dbc` has a `fallback` column, and it encodes exactly the chains
this needed: `Dead` (6) falls back to `Death` (1), `Attack1H` (17) to
`AttackUnarmed` (16), `Ready1H` (26) to `ReadyUnarmed` (25). Nothing was
guessed; the ids were read out of the table.

**But the table's fallbacks are not sufficient, and only the models say so.**
`wow-cli m2 anims Creature\Wolf\Wolf.m2` lists `AttackUnarmed` and `Death` and
**no ready stance at all**. So a wolf entering combat resolved to no sequence,
which draws the bind pose -- stiff and T-posed, worse than the idle it
replaced. Both combat stances now end their fallback list at plain `Stand`.
That is this project's own rule about rendering coming back around: when a rule
is about what a *model file* contains rather than what a table says, look at
the model.

#### And that fallback forced the clamping rule to be rewritten

A cycle that plays once has to hold its last frame; a loop has to wrap. The
obvious place to decide is the state that asked -- and it is wrong in both
directions at once:

- `Attacking` on a wolf falls back to `Stand`, and a Stand frozen at its final
  frame is a statue.
- `Dead` on that same wolf resolves to the *fall*, and looping a fall is a
  creature dying over and over -- while carrying no start time to clamp
  against, because a settled corpse's pose is the same however long ago it
  died.

So holding is a property of the **animation that resolved**, not of the motion
that requested it. `plays_once` takes an id.

#### One bug caught by a screenshot, which no test would have found

The first version asked `is_dead_or_ghost()`. That is the right question for a
health bar and the wrong one for a renderer: a released player is a *ghost*,
which walks to its corpse on its own feet. Laying every ghost flat would have
been a fine piece of logic and a nonsense picture. `is_corpse()` -- dead and
not a ghost -- is the rendering question, and for creatures the two coincide,
which is exactly why it took a player corpse run to notice.

#### Verified by reading the renderer's own log

The one-shot states each log the sequence they resolved to, and whether they
resolved to anything -- because "playing the death animation" and "having a
death animation to play" are different claims and only the second is checkable
from inside. Against the local realm with corpses about:

```text
display 31048: Dead -> sequence 7 (animation id 1), 1 instance(s)
display 49:    Dead -> sequence 7 (animation id 1), 1 instance(s)
```

Animation id 1 is `Death`, reached by falling back from `Dead` exactly as the
table prescribes. And `wow-cli m2 anims Character\Human\Male\HumanMale.m2`
independently lists sequence **7** as `Death`, 2000ms -- a dump of the file
agreeing with what the renderer picked at runtime, which is the two-independent-
derivations shape applied to a renderer rather than to a protocol.

Still outstanding from the same report, and both larger: **weapons are not
drawn** (needs the M2 attachment table, `foss-wow#25`/`#26`), and **loot and
inventory do not exist** -- a new protocol surface and a new frame, not an
animation problem.

### 4.6 continued: making a creature look at you, in four goes

The animation work above was confirmed at a window and left one complaint:
*the mob did not turn to face the player*. Fixing it took four passes, and
each one was wrong in an instructive way rather than merely incomplete. All
four were found by a person watching a wolf, and none of them by a test that
existed at the time.

**1. The facing was being parsed and thrown away.** `SMSG_MONSTER_MOVE` has
four facing modes and this client kept one:

```rust
monster_move_type::FACING_SPOT   => reader.skip(12)?,
monster_move_type::FACING_TARGET => reader.skip(8)?,
monster_move_type::FACING_ANGLE  => facing = Some(reader.f32()?),
```

`FACING_TARGET` is exactly how a creature in melee turns to face what it is
hitting -- its body is the victim's guid. What makes this one nasty is that
**the packet parsed perfectly the whole time**. A skip of the correct *length*
keeps the rest of the packet in step, so the cursor discipline that catches
every other layout error here -- running out of input, or having input left
over -- cannot see a field that is correctly sized and discarded. It shows up
only as behaviour, and only to somebody looking.

The reason it was skipped is sound: a guid is not an angle until something
knows where that unit is, and a parser has no world to ask. So `MoveFacing`
carries it out intact and `WorldState::facing_of` resolves it.

**2. That made the creature turn only when the player moved.** The server
sends a facing packet when it decides a creature has turned, which is prompted
by the victim moving -- so between packets the wolf held the heading it walked
in on. A packet is a statement made once. `UNIT_FIELD_TARGET` is a replicated
field saying who a unit is fighting for as long as it is fighting them, so the
heading now comes from that first and tracks continuously, with the facing
packet as the fallback for a creature looking at something it is not
targeting.

**3. Then it could not keep up with a player circling it.** Turning was eased
at a fixed maximum rate, which looks correct in every individual frame. But
angular speed is `v / r`, and a player orbiting at melee range exceeds any cap
chosen to look unhurried -- at which point the error does not settle at
"somewhat behind", it *grows without bound* and the creature ends up facing
nowhere near its victim. Reported, precisely, across three wolves at three
different circling speeds: one that worked because the player stood still, one
that "couldn't update fast enough", and one that "did attempt to face the
player". Closing a fixed *fraction* of the remaining error per second has no
such mode: the lag is `omega * tau` for any `omega`, under ten degrees at any
achievable orbit. The test drives a six-radian-per-second orbit -- faster than
a player can run -- and asserts the worst lag stays small; the fixed-rate
version fails it outright.

**4. And then it aimed at the player's login spot.** The last one is the most
embarrassing and the most useful. **The server never relays a client's own
movement back**, so this client's entry for *itself* in replicated state is
frozen at the login position forever. That is documented at length --
in `live::drawable_entities`, the function that *draws* the player. Resolving
"face this guid" through replicated state then walked straight into it from a
function that *aims at* the player: the creature faced the login spot, which
is right at first, drifts as the player walks, and eventually lets them stand
behind a creature that is supposedly attacking them. Exactly the progression
reported.

The lesson is not "remember the trap" -- the trap *was* written down. It is
that a surprising fact about the data belongs on the data, not on the first
caller that tripped over it, because the second caller will not be reading
that comment.

`facing_of` now takes the caller's own guid and real position, since the
caller is the only thing that knows it, and there is a test pinning a creature
told to face a player whose replicated position says north while they have
actually walked east.

### 4.7: weapons in hand

Reported at a window after 4.6, in four words: *"there is no item in his
hand"*. Tickets `foss-wow#25` (parse the M2 attachment table) and `#26` (draw
what a character carries).

**The attachment table.** An `M2Attachment` is 40 bytes -- an id, a bone index,
two bytes of padding, a position, and a 20-byte visibility track. The stride is
the part worth proving, because a wrong one still parses: it reads an id out of
the middle of a float and a bone index out of its low half. Two properties
settle it, and neither is a size check. Across all 22,779 models the ids come
out as a *contiguous vocabulary of 50*, 0 to 49, with **zero** out-of-range
bone indices -- a wrong stride gives thousands of distinct ids and strays
everywhere. And every attachment's position lands bit-for-bit on the pivot of
the bone it names.

That second property is not decoration, it is the drawing rule. An attachment
position is a **model-space point, not a delta from the bone**, which follows
from how a bone matrix is built here: `translate(pivot) * transform *
translate(-pivot)`, so the bind pose is the identity rather than a
translation. Treating it as a delta adds the pivot in twice and hangs the sword
out at arm's length from the fist -- which renders perfectly and is never an
error. `held_transform` is a named free function for exactly that reason, with
tests pinning both the composition order and the rotation.

**Which id is which hand, derived rather than transcribed.** Ids 1 and 2 are a
mirrored pair at the ends of the two arm chains, and each keeps to its own side
of the plane of symmetry: id 1 sits on -Y in 684 models against 103 on +Y, id 2
the reverse. Which side *is* the left then follows from the one fact the
renderer had already confirmed live -- an M2's forward is +X, drawn Z-up and
right-handed, so the model's left is +Y and id 1 is the right hand.
Corroborated independently by id 0, which sits just outboard of the +Y hand:
that is the shield, and a shield is worn in the off hand. Two derivations, one
answer, and then a person at a window.

#### Two things in the item data were not what they looked like

`wow-cli item held` joins all 46,096 rows of `Item.dbc` to `ItemDisplayInfo`
and asks which inventory types name geometry at all. The separation is total:
the held slots fill their model column for 99.2% or more of their items, where
a chest or a belt manages under 2%. But the column they fill is
**`model_left` -- for every one of them, main-hand swords included.**

The names are not wrong. Shoulders (type 3) fill *both* columns and put
`LShoulder_Leather_A_01` in one and `RShoulder_...` in the other, which is what
proves it. The pair is really "first model, second model", and only a genuinely
paired item uses both. A sword is a single model, so it sits in the first
column and goes in whichever hand its **slot** names. Reading the column as the
hand would have put every weapon in the game in the wrong one, and it is the
reading anybody would reach for first.

The folder is not a column either, so it was measured the same way: resolve
each name against every `Item\ObjectComponents` directory and see which
answers. Weapons land in `Weapon` and shields in `Shield`, each at 100%.

**Bows, guns, thrown weapons and shoulders are deliberately left out.** Their
data resolves perfectly -- ranged weapons are 100% in `Weapon` -- but the
second half of the rule is a claim about *which attachment point*, and that has
been confirmed for the two hands and nothing else. A guess there puts a rifle
through a character's palm. Same precedent as `geoset_rule` leaving out belts,
and a test asserts the omission is a decision rather than a gap.

The test that matters asserts both halves: the held slots choose a hand **and**
the painted ones choose neither, even though 16% of gloves do name a model.
Asserting only that a sword lands in the right hand passes under the wrong rule
too -- the same trap the auto-attack filter had in 4.4.

#### The renderer needed no new draw path

A held item is an ordinary `Group` whose transforms happen to come from
somewhere else: `wielder_placement * hand_matrix * translate(attachment)`,
rewritten every frame in `update_animations` from the very pose the wielder was
drawn with. Deriving it from the same matrices rather than recomputing it is
deliberate -- two things that must agree exactly should have one source, and a
hand that disagreed with its own model by a frame is a sword that trails the
arm. Its own `animation` stays `None`: a weapon's skeleton is rigid and draws
in bind pose against the identity palette, and all the movement comes from the
hand. `InstanceBuffer` gained `COPY_DST` and a `write`, and the buffer is
seeded with the bind-pose answer rather than zeroes, per the rule that anything
written before it is read should start as something you can *see*.

#### The bug that was not one

The first live screenshot showed no sword, and the diagnostics all came back
clean: the item resolved, the group was built with three draws, the transform
put it 0.9 up and 0.36 out from the wielder -- exactly a right hand. The model
rendered fine on its own. Nothing was wrong. The camera sits directly behind
the character, and a blade held forward at hip height is entirely behind its
owner's body from there. One render from the side showed it held correctly.

That is the trap already written up in `CLAUDE.md` about a composite needing a
way to be seen as itself, walked into anyway, and it cost a full round of
diagnostics that all reported "correct". The tell was there the whole time:
when every measurement says the thing is where it should be, stop measuring and
change where you are standing.

Confirmed at a window: the sword is in the hand, textured, and swings with the
combat animation when killing a wolf.

#### What is deliberately still missing

- **There is no sheathed state at all**, which was the first thing noticed
  live. `Item.dbc`'s `sheathe_type` column is transcribed and never read, and
  this client has no drawn/undrawn concept to hang it on -- so every weapon is
  always in hand, including while standing in town. That is a feature, not a
  bug in this one: `foss-wow#42`.
- **Other players' weapons** need their visible-item fields, which arrive as
  item *entry* ids rather than display ids -- `foss-wow#23`, unchanged.
- **NPCs hold nothing**, and not for want of trying:
  `CreatureDisplayInfoExtra`'s eleven item columns are the eleven that paint
  the *body*, measured column by column in 4.4. A guard's sword is simply not
  in that table.
- Shoulders, helms and ranged weapons, for the reason above.

### 4.8: sheathing, and a black blade that was never the blade

`foss-wow#42`, opened by the first live look at weapons: *"its still in hand
there is no sheath state."*

#### The server does not draw your weapon

The starting assumption was that entering combat would set a sheath state to
read. It does not. A whole fight was driven against the realm -- selection,
swings landing both ways, real damage -- while every replicated field was
watched: `UNIT_FLAGS` gained its in-combat bit and **byte 0 of
`UNIT_FIELD_BYTES_2` never moved off zero**. Drawing a weapon is a decision the
*client* makes and reports with `CMSG_SET_SHEATHED`; the server only
republishes it so other players can draw it too. A client that never sends is a
character who never unsheathes, which is exactly what this one was.

Both the opcode and the field were confirmed by varying the input, the
`CMSG_ATTACKSWING` shape: nothing acknowledges the send, but asking for each
state in turn moved that byte to match -- unarmed, melee, ranged, and back.
Each run also *started* in the state the last one left, so it persists
server-side.

The field index was not taken on trust either. `UNIT_FIELD_BYTES_2` is
`OBJECT_END + 0x74`, and the same expression gives `0x36` for the level and
`0x3B` for the unit flags -- both of which this client already reads, and both
of which matched a live character (level 5, and the in-combat bit appearing
exactly when a fight started).

#### `sheathe_type` is a real column, and the control says so

`wow-cli item sheath` cross-tabulates it against inventory type over all 46,096
items. Every slot that only *paints* the body is 100% type zero with a single
distinct value; the slots that hang geometry spread across five. So the column
is filled in because an item can be sheathed, not incidentally. Item by item: a
claymore is 1, a stave 2, a short sword 3, a shield 4, and bows, guns and
thrown weapons are 0 -- meaning **no resting place**, not "unknown". Those stay
in the hand.

One wrinkle: the column is keyed by item *entry* and the character list only
gives display ids, so the real question is whether a display id determines a
sheath type. 98.9% do. The remaining 1.1% is resolved toward the non-zero
value, because zero means nowhere to rest and preferring it would leave a sword
in the hand of somebody who should have stowed it.

#### The attachment points, and why looking could not settle them

The same posed-skeleton dump that found the hands narrows the resting places to
four families, each making physical sense: **32/33** at hip height with the
blade trailing backward, **26/27** high on the shoulder blades with the blade
pointing down, **30/31** on the upper back pointing up, and **28**, the only
centred point behind the torso. Consistent across human, orc, night elf and
tauren.

Rendering then produced two candidates that both look completely correct,
because a greatsword slung across a back has two mirror images and *both* look
like a greatsword slung across a back. That is the placement-rotation trap
again: a movable thing checked against another movable thing proves nothing.

What is asymmetric is not the picture but the **animation**. Character models
carry cycles named `Sheath` and `HipSheath`, and during them the hand travels
to wherever the weapon is stowed. `wow-cli m2 attach-trace` plays a sequence
and reports which static attachment the moving hand approaches: over `Sheath`
the right hand gets two to three times closer to 26 than to 27, on human, orc
and dwarf alike. So a right-hand weapon rests on the right of the back, and the
mirror follows.

The equivalent trace for `HipSheath` does **not** separate 32 from 33 -- three
races give three different answers -- so the hip follows the rule the back
established rather than a measurement of its own. Recorded as the weaker half.

#### Two things the first live look found immediately

**The blade was black, and it was never the blade.** A weapon draws its
geometry *twice*: once with the item texture `ItemDisplayInfo` names, and again
over the very same submesh with a hardcoded reflection map. That second pass is
material blend 4, which this renderer collapsed into plain alpha blending --
and the reflection is a DXT1 with no alpha channel, so it was fully opaque and
covered the first pass completely. Blend 4 is additive. It is **17.2% of the
58,479 materials in the archives**, so this was never a weapon bug; weapons
were just the first place anyone looked closely.

The argument is structural rather than aesthetic, which matters because "it
looks better now" is not evidence: if blend 4 painted over, then
`model_texture_left` -- a column filled in for 19,702 items -- would be
invisible on every weapon with a reflect layer. A column exists to be seen.

**The hands were open.** With the weapon drawn, a standing character resolved
to the plain idle, whose hands are open, so the grip floated through the
fingers: *"the hand is open holding the sword like he was precombat."* The
`Ready` stance already existed and was gated on `fighting`, which was
indistinguishable from correct for as long as nothing was ever drawn outside a
fight. A drawn weapon is now enough on its own, and the stance carries which
grip it is -- Testwolf's claymore resolves `Ready2H` rather than `Ready1H`.

The fallback chains come from `AnimationData.dbc`'s own fallback column rather
than an invented order: row 18 (`Attack2H`) names 17, rows 26 and 27 both name
25 (`ReadyUnarmed`). Reading the table beats guessing, and it happens to agree
with what a model lacking two-handed cycles should do.

#### A validation that was right about the wrong thing

The attachment sanity check shipped with a fixed 100-unit ceiling on how far an
attachment may sit from the origin, which is a sensible number for a character
and nonsense for `Creature\TREE\AshenvaleTreeFalling01.m2` -- a hundred and
fifty units of falling tree whose perfectly good attachment sits at Z=127. It
now scales with the model's own declared extent. A threshold that scales with
its subject cannot make that mistake.

#### Still missing

- **`Ready2HL`, `ReadyBow`, `ReadyRifle` and `ReadyThrown` are not used.** The
  cycles exist; which items want the long two-handed hold has not been
  measured, and this client does not draw a bow at all.
- **Blend modes 5 and 6** (modulate and modulate-2x) are still folded into
  plain alpha. Together they are 3.8% of materials and neither has been looked
  at.
- Sheathing applies to the player only, because only the player has held
  geometry -- other players' weapons are still `foss-wow#23`.
