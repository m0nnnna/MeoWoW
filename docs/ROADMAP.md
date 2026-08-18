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

**Not done at the time this was written:** spell damage
(`SMSG_SPELLNONMELEEDAMAGELOG`) and the corpse and release flow -- both are
covered in their own sections below. The two swing-refusal opcodes stayed
unseparated even after a dedicated attempt: `foss-wow#32` varied range and
facing one at a time and found that a swing wrong on exactly one axis gets no
reply at all, not either named refusal -- see the doc comment on
`ATTACK_SWING_REFUSED_A`/`B` in `opcode.rs` for the full result. Combat has
also not been *watched* in the viewer yet: it is verified headlessly and
through the CLI.

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

### 4.8 continued: a sword that flew off the back, and a sprint in reverse

Both reported from one look, and only one of them was about weapons.

#### The global-sequence bug, four milestones old

The sword sat correctly on the back while the character stood and **swung out
to point forward from his shoulder the moment he moved**. The obvious reading
is that the sheath attachment was wrong. Measuring said something stranger: the
attachment bone's orientation was 108 degrees in `Stand` and 8 degrees in
`Walk`, and the same was true of the hip. A resting place cannot do that, and
for a while the conclusion was that the whole identification was wrong -- the
points chosen were, by measurement, the *least* stable of all thirty-nine.

They were right all along and the renderer was lying. The bones that carry
stowed weapons are authored on **global sequences**: one keyframe list on a
timeline shared by every animation, rather than one list per sequence. And
`Track::sample` indexed `sequences[sequence]` unconditionally, so a global
track resolved only when the sequence index happened to be zero -- which is
`Stand`. Every other cycle fell back to the bind orientation, silently.

`Track::global_sequence` was parsed, and documented, and then ignored by the
only function that could act on it. The field's own doc comment says it "runs
on a shared global timer rather than the current sequence".

This was never a weapon bug. It has been wrong since animation was written and
affects every bone authored that way in every model; weapons are simply the
first thing whose *position* made it visible, because a body bone snapping to
bind pose in one cycle out of a hundred and fifty is not something anyone was
going to notice.

The fix reads a global track from its single entry. The timeline's true period
lives in the model's `global_loops` array, which this reader still does not
parse -- and for a track with one key, which is the overwhelming case and every
case here, a constant samples the same at any time. A genuinely looping global
track now runs on the current animation's clock rather than its own. Stated
rather than hidden.

The regression test asserts that the resting places *agree* across standing,
walking and running rather than asserting what they agree on, with a tolerance
loose enough for real torso sway (45 degrees) and far tighter than the failure
(91).

#### Retreating at a sprint

`Motion::from_speed` took a magnitude, and the viewer handed it the run speed
whenever any movement key was held. So backing up played the forward run cycle
at 7.0 units per second: a character sprinting while facing the wrong way.

The speed is now **signed**, which is the smallest thing that can carry the
distinction, and negative resolves to `Walkbackwards` (row 13, whose own
fallback column names `Walk`). A single `live_pace` decides both the number
that moves the character and the number that picks its cycle, because those two
disagreeing is exactly what produced the bug.

The backpedal speed is 4.5, hardcoded beside the run speed and for the same
reason: the authoritative figures are the nine speeds in the object-create
movement block, which this client does not parse. A character with a speed buff
still moves at the default. That is the real fix and it is not this one.

### 4.9: weather

Weather was genuinely absent -- three mentions in the whole tree, all comments
saying it did not exist. The hook was already there: every `Light.dbc` row
carries eight `LightParams` columns, one per weather condition, and the
renderer had always taken the first.

`SMSG_WEATHER` (0x2F4) is nine bytes: a `u32` state, an `f32` intensity, and a
`u8` saying whether the change was abrupt. It arrives on entering a zone and
whenever that zone's weather turns, so a client that ignores it stands in
permanent sunshine. **Weather is a zone property the server owns** -- the exact
opposite of the sheath state next door, which the client decides and the server
merely republishes.

The state numbers are sparse (2 is absent; 8 jumps to 22 to 41) and only the
named ones are named. An unrecognised state is carried through with its raw
number rather than guessed at, and is lit as clear weather, which is the
conservative direction: ordinary daylight where it should be dim is
unremarkable, the reverse looks broken.

#### The storm column, and a statistic that nearly refuted it

The schema calls column 9 the stormy one, which was a reading rather than a
measurement -- and a wrong one would be invisible, since the renderer would
pick a perfectly valid `LightParams` row that simply is not the weather it
claims. So the question was asked as a property: **a storm must be dimmer,
greyer and foggier than clear weather at the same place and hour.**

Over 200 outdoor lights it came back flat. Storm was darker 55% of the time,
greyer 54%, and pulled the fog *in* only 47% of the time -- a coin flip, and it
read as a refutation.

It was the sloppy version of the question. Most positioned lights are
decorative -- a glowing crater, a haunted wood -- and their weather columns are
authored for effect rather than for weather. The row that matters is the one
that actually lights a zone, and asking about *that* is unambiguous: map 0's
default light names clear params 12 and storm params 10, and **row 10 is a flat
neutral 0.32/0.33/0.32 at every hour of the day**, with fog ending at 10,000
against clear's 18,000. No dawn orange, no midday white, no sunset. Map 1's
default names the same row 10. That is not a lighting preset that happens to
look moody; it is a sky with the sun taken out of it.

Same lesson as the `Spell.dbc` duration column, from the other end: a property
test is only as good as the population you run it against, and 200 rows where
the question is meaningless will bury two rows where it is decisive.

#### Blended, not switched

The two sets of curves are interpolated by the reported intensity rather than
chosen between, because the server eases weather in and out and a client that
switched would turn the sky grey between one frame and the next. Fog distances
blend with the colours: the horizon coming in from 18,000 to 10,000 is most of
what makes rain feel like rain, and it is the part a screenshot shows most
clearly.

#### Confirmed against the realm

`.wchange 1 1` in Elwynn, then a screenshot: the sky goes from blue to grey,
mean frame brightness drops from 90/101/25 to 71/75/17, and the abbey two
hundred yards away goes hazy. The parse logged `weather: HeavyRain at 1.00`
from the live packet.

The test asserts the three properties separately and at the hour each is real
-- **greyness is checked at dawn, not at noon**, because clear midday light is
already a perfectly neutral 0.71/0.71/0.71 and nothing can be greyer than grey.
The first version asserted it at noon and failed, which was the test being
wrong rather than the data.

#### What weather still is not

- **There is no precipitation.** No rain, no snow, no sand: the weather changes
  the light and nothing falls out of the sky. That needs a particle system this
  renderer does not have, and it is the obvious next piece.
- **There is no skybox**, weather or otherwise. `LightParams.light_skybox_id` is
  read and unused; the sky is a cleared colour.
- Weather sounds, and the `abrupt` flag, are parsed and ignored.

### 4.9 continued: the camera was half an orbit

Reported after the weather work: swinging the camera left and right kept the
character centred, and dragging up and down did not -- *"they go everywhere"*.

The cause is in the shape of the two halves rather than in either one. The
follow code placed the eye at a **fixed height behind** the character and
recomputed that placement every frame from the character's heading, so a
horizontal drag really did carry the eye around them. Pitch was deliberately
left alone for the mouse to write, with a comment saying so -- and a camera
that stays put and re-aims does not keep anything centred. It swings its aim
off the subject, which is precisely what was seen.

So one axis orbited and the other tilted, and the half that worked made the
half that did not look like a tuning problem.

The camera now places the eye on a sphere around a point at the character's
chest, from both angles, every frame. `FOLLOW_HEIGHT` changed meaning with it:
it used to be how high the *eye* sat and is now what the view orbits **around**,
so it dropped from 4.0 to 2.2 -- chest height rather than head-and-a-half above
the ground. Both drags feed the same two offsets; neither writes the camera
directly any more, which was the earlier bug on the yaw axis reappearing on the
pitch axis for the same reason: two places writing one field, and the one that
runs later wins.

`live_camera` and the per-frame follow now share one `orbit_around`, because a
headless screenshot exists to be evidence about what the window shows and two
copies of the arithmetic would make it evidence about itself -- the same rule
that unprojects the picking ray from the matrix the scene was drawn with.

The test projects the character's own focus point through the very view matrix
the scene is drawn with and asserts it lands within a thousandth of the centre,
across six pitches, four headings and three distances. A second test asserts
the eye is genuinely `distance` away and *moves* as the pitch changes, which is
the half the first would miss: a camera welded to the character's own position
also keeps them dead centre, and shows the inside of their head.

#### And it was loose, and it went through the ground

Two more from the same look: *"it needs to be tighter, right now it's pretty
flowy"*, and the camera passed straight through the terrain.

**The looseness was a constant that had drifted from its own comment.** The
drag rate was `0.008` radians per pixel, annotated "roughly half a turn across
the window". On a 1920-wide window that is 15.4 radians -- **two and a half
full turns**, five times what it claimed, and worse on a larger monitor. A
hand-sized movement threw the view most of the way round, which reads as a
camera that will not sit still.

It is now expressed as what the comment always meant: half a turn across the
window *width*, whatever that width is. Both axes use the width rather than
each their own dimension, so a diagonal drag does not curve on a non-square
window.

**The ground collision pulls the eye in along its own ray.** Lifting it instead
would keep the distance and break the framing -- the subject would slide off
centre, which is the bug fixed immediately before this -- and pushing it
sideways would swing the world. Shortening the distance is the one move that
leaves the picture pointing exactly where it was, and it is what the game this
models does.

The ray is marched from the subject outwards rather than solved at the
destination, because the ground under a straight line is not a straight line: a
camera that only checked where it wanted to end up would tunnel through a ridge
in between and look back through it. There is a test for exactly that, and one
for the opposite half -- open ground must leave the camera where it asked to
be, or a collision check would pass for a camera that simply always sits close.

A tile that has not streamed in yet counts as clear rather than as blocking.
The other direction would yank the camera into the character whenever the world
was catching up, which is the same failure direction the rest of the streaming
code already chooses.

#### And then the feel became a setting

*"We should add a slider for the camera to tighten up and loosen up in the
settings."* -- which is the right answer to the previous fix, because "tighter"
is a preference and hard-coding a number only moves the argument.

`ui.toml` gains a `[camera]` section and the `F1` window gains three controls:
the turn a full-window drag is worth, the starting distance, and whether the
vertical axis is inverted. They live in `crates/ui` because `Profile` is what
gets written to disk, not because the camera is a frame -- stated in `docs/UI.md`
so the next reader does not have to guess.

The viewer's two constants went with it. `FOLLOW_NEAR` and `FOLLOW_FAR` are now
aliases of the ui crate's exports rather than copies, because the wheel's range
and the slider's range are the same claim, and two copies of a claim agree only
until somebody edits one. The starting distance is seeded from the saved profile
at construction and then owned by the wheel: reading it from the profile every
frame would have been simpler and would have made every scroll rewrite a saved
setting.

`Camera::radians_per_pixel` clamps on every call rather than sanitising at load,
which is the opposite of what `Style` does and deliberate. A style value is read
once when a frame is drawn; this one is read from a hand-editable file and fed
straight into a per-frame rate, where a zero freezes the camera and a negative
inverts it. The guard belongs where it cannot be skipped by a caller that built
the struct some other way.

### 4.10: the sky is five bands, and the world was too bright

The sky was one flat colour cleared behind the world. `Light.dbc` describes it
as five, and this is where they were identified and drawn.

**Bands 2 to 6 are the sky, zenith first and horizon last**, and the
identification is the point rather than the render. Noon and midnight both look
like plausible gradients under almost any ordering of five bands, so agreeing
with one proves nothing. **Dawn is the hour that discriminates**, because a
sunrise is not a ramp: at 06:00 red minus blue runs `-49, -60, +138, +191, +179`
across the five, crossing zero exactly once with the warm side at the horizon.
Sunset does the same. That crossing has a *side*, and a side is something an
ordering can get wrong -- which is the whole difference between a test that can
fail and one that cannot. It also settles the byte order for good: read
red-first, the sunrise would be directly overhead.

The full write-up is in `docs/RENDERING.md` under "Lighting, part three". What
came out of it that was not the sky:

- **Fog was pointing at the zenith**, so distant ground faded into the colour of
  the sky directly overhead -- black at midnight. Not unconfirmed; wrong. It is
  now *derived* from the horizon band rather than named, so distant terrain
  meets the sky it is drawn against by construction. Three bands remain
  plausible as a separate fog colour and none is named, because nothing has
  separated them.
- **The diffuse light is the horizon band.** It stays, on the render evidence
  that put it there, but as a documented borrowing rather than an open
  question: the colour a low sun arrives in is the colour it painted the
  horizon. Whether it should instead be band 9 -- the sun and moon *disc*,
  whose brightness is flat across the day because its contribution depends on
  elevation -- modulated by that elevation is now the open question.
- **Everything was too bright, and had been since lighting existed.** The band
  bytes are display values and an sRGB target re-encodes what a shader writes,
  so 49 became 123. The error grows the darker the colour is, which is why every
  daylight render looked right and the thing that finally showed it was midnight
  over Elwynn arriving as a bright afternoon blue.

That last one is the one worth remembering, because the fix for the *sky* was
obvious and the fix for the *lighting* was not. The sky is written straight to
the target, so there is one right answer. The diffuse and ambient multiply
textures already decoded to linear, so what space they belong in is a genuine
question -- and it took a report ("the sky looks like night even if it is
bright") to notice that correcting one and not the other left the world and its
sky disagreeing about the hour. The derivation then settled it: the original
client multiplied a texture byte by a light byte into an 8-bit framebuffer, so
matching it needs a factor of `(L/255)^2.2`, which is exactly the linearisation.
Faithful and physically-right turned out to be the same answer.

**The heights of the five bands are chosen, not measured**, and that is stated
everywhere it matters. Nothing in the data says how far up each sits -- the
original client keeps it in a dome mesh. Evenly spaced was tried first and a
render refused it: the blue bands landed above 60 degrees, where a third-person
camera never looks, and midday Elwynn came back nearly white.

There is still no skybox, and that turns out not to be a gap for the outdoor
world: `LightParams.light_skybox_id` is 0 on the row that lights Elwynn, and
`LightSkybox.dbc`'s 124 rows are named things like `StratholmeSkybox`. No sun
disc, no clouds, no stars.

#### And two instruments that were not working

Neither was the task, and both cost time before they were fixed.

**`--hour` did nothing offline.** The flag parsed, the help text promised, and
an offline screenshot silently got the fallback gradient -- which is a perfectly
plausible sky, so nothing announced that the lighting tables had not been
consulted at all. The first render of this milestone was studied before that
landed. It now resolves `--map` through `Map.dbc` and lights a world with no
realm behind it.

**`cargo test` was not green at `HEAD`.** `unanimated_bones_pose_to_identity`
had been failing since 4.8's global-sequence fix, which invalidated the test's
premise: it asserted that a sequence index past the end has no keys anywhere,
and a global track has no sequence to be outside of. The behaviour was right and
the test was describing the old bug. Rewritten to assert *both* halves -- bones
that inherit no global track pose to identity, bones that do must not -- and
walking the parent chain rather than the bone, because a bone with no track of
its own still inherits an ancestor's global scale. Asserting only the first half
would have passed while quietly tolerating a regression to the original bug.

### 4.10 continued: weather that falls

Rain and snow, and the first particles this renderer has drawn.

**No vertex buffer, no instance buffer, and no CPU-side particle list.** A
raindrop needs a position and a speed, both pure functions of its index, so the
draw is `draw(0..6, 0..count)` and every drop hashes its own seed in the vertex
shader. The field is a box that follows the camera by *wrapping*: each drop's
offset from the eye is taken modulo the box, so walking forward brings drops
round from behind and nothing is ever created or destroyed.

It draws last in the world pass and **tests depth without writing it** --
testing is what keeps rain out of the abbey's interior, not writing is what lets
thousands of unsorted drops blend without occluding each other.

Two of the constants exist only because a render demanded them. **The near
fade**: a drop is a fixed size in the world, so one an arm's length from the eye
covers a third of the screen, and the first render was a field of white bars.
**The slant**: rain falling exactly along the world's up axis draws every streak
parallel to every other and to the window's edges, and reads as a picket fence.
And snow needed a third, `roundness`, because a raindrop's ends are hard -- the
streak is the exposure, not the drop -- and a snowflake's are not; without it,
snow drew as tiny vertical bars.

**What falls is a different question from what storms.** `Weather::is_storm`
counts fog as a storm, which is right for choosing between two sets of light
curves and would have rained on a misty morning here. This is the shape of trap
this project keeps paying for -- a predicate that is right for the caller it was
written for and quietly wrong for the next one -- so precipitation got its own
accessor and its own test, which asserts that fog storms *and stays dry*.
Thunderstorms and the three sandstorms are deliberately dry: neither has been
seen from a realm here, and a missing effect is visible and fixable where a
wrong one just looks odd for ever.

`--weather <state>` overrides the realm's, for the same reason `--hour`
overrides its clock, and it is what makes the effect checkable headlessly. The
GPU tests render it against a black background and read the pixels back: it
falls, it falls harder with intensity, it stops at zero, it moves between two
times, and **an hour in it is still falling** -- that last because a drop falls
by `speed * seconds` and an `f32` holding an hour of that has lost enough
precision to quantise a drop below its own width, so rain would slowly become a
flickering grid on a client left running. The clock is wrapped inside `draw`
rather than trusted from the caller, the same reasoning that puts
`Camera::radians_per_pixel`'s clamp where it cannot be skipped.

What is still missing: M2 particle emitters -- torches, spell effects, a fire
elemental. Those are per-model and per-bone, with emitter types, lifespans,
gravity, and colour and alpha tracks over a particle's life, and the header
offsets for them are recorded but the block is unparsed. This milestone is what
gives them a billboard path to arrive into. There is also no splash where rain
lands, no sound, and no lightning.

### 4.11: a sun to look at, and legs that sidestep

Two small reports from play, and both turned out to have a measurement problem
in front of the fix.

#### The sky was empty

The gradient was right and there was nothing in it. **Band 9 is the sun and the
moon**, identified by the one property no other band has: it is the brightest
band at *every* hour and its brightness barely moves across a whole day -- 728,
615, 724, 675 summed over the channels at midnight, dawn, noon and dusk, against
389 for the next brightest, while every band that lights the world drops to a
fraction of itself at night. A curve that stays bright while the sky goes black
is not lighting anything; it is a thing you look at.

The hue then says which thing it is: cool white (232, 241, 255) right through
the night, warm (255, 210, 150) at sunrise, warm white (255, 247, 222) at noon.
A moon that becomes a sun. One band serves both because only one is ever up.

**The handover is where the bug was.** Drawing "whichever body is up" means the
direction handed to the shader jumps clean across the sky the instant the sun
sets, and a disc that teleports is worse than no disc. Fading at the horizon
fixes it -- the sun dims out as it goes down, the moon comes up dim on the other
side, and both are at zero when they cross. The first version of the fade was
written against the *unflipped* direction and was therefore dead code, which the
test caught by measuring a "setting" sun and getting a hard zero: the sun had
already become the moon and was behind the camera. The test now looks behind the
camera as well as in front, which is the only way to see the handover at all.

The glow around it is derived from band 9 rather than named. Band 10 behaves
plausibly like a halo, and so do 11 and 13; nothing separates them, and a halo
is the disc's own light scattered, so fading the disc's colour is derivation
instead of a guess. The same reasoning that gave fog the horizon band.

Clouds, stars and lightning are still absent.

#### Strafing ran forwards

`Motion::from_speed` took a signed scalar, so a character sidestepping was
indistinguishable from one running forward and drew a full forward sprint while
crab-walking. The comment above `live_pace` said so in as many words -- "strafing
sideways uses the run" -- which is the useful kind of admission: the gap was
recorded at the point it was made, and finding it was a grep rather than an
investigation.

It is now `Motion::from_pace(forward, lateral)`, and **travelling outranks
sidestepping**. Running diagonally plays the run; only a pure strafe gets the
shuffle. That precedence is a judgement about the original client rather than
something the data states, and it is flagged as the one line to A/B at a window
if diagonal movement ever looks wrong again. The test asserts both halves,
because making pure strafing shuffle is easy and would also make a diagonal
sprint draw as a sidestep -- a worse picture than the one it replaced, and one
a test that only checked pure strafing would pass for.

**The cycles were nearly declared absent.** `ShuffleLeft` and `ShuffleRight` are
`AnimationData` rows 11 and 12 and sequences 38 and 39 on the human male, and the
first search for them came back empty -- from `m2 anims`, which defaults to
listing thirty of a hundred and fifty-six. A truncated listing answers a
different question from the one asked, and the conclusion drawn from it was that
no character model had a sidestep at all and the existing behaviour was correct.
It is the same shape as the `wow-cli ls` check that once said 0.1% of the baked
NPC textures shipped.

Both cycles advance the character by 0.00, which is right rather than suspicious:
they are stepping motions played in place while the movement system does the
travelling, exactly like `Walkbackwards`. Their fallback deviates from
`AnimationData`'s own column, which sends both to Stand -- a model sliding
sideways on the spot is the bug this whole family exists to avoid, so they fall
back to the travelling cycles instead. Nearly unreachable either way: only the
player supplies a lateral component, because a monster move gives a path and a
duration and says nothing about how the body is turned relative to it.

### 4.12: buildings are solid

Reported from play, with the strongest evidence this project has: a character
driven by this client walked through the wall of Northshire Abbey, and a
*second* client watching drew it happening. So the server neither corrects nor
objects, and collision is entirely this client's job. That is a measurement
rather than an assumption, and it is written at the top of the new crate.

The data turned out to be almost all in hand already:

- **WMO groups already parsed `MOVT`, `MOVI` and `MOPY`**, including the 0xFF
  material that marks a collision-only triangle. The loader *counted* those and
  threw the indices away. Now kept -- and all triangles are kept, not only the
  collision-only ones, because a wall you can see is as solid as one you cannot.
  The invisible ones matter most: they are the barriers deliberately placed
  across a doorway or under a stair.
- **M2 headers already located the collision mesh** and dropped all three arrays
  with `let _`. It is a genuinely separate and far coarser mesh -- tens of
  triangles where the render mesh has thousands -- and that distinction is the
  whole point: using the drawn geometry would make a tree's *foliage* solid.

`crates/collision` is pure geometry: no MPQ, no DBC, no protocol, no GPU, so the
whole of it is testable from a unit test with a hand-built box. Same reasoning as
the `ui` crate depending on neither `world` nor `render`. Northshire's tile comes
out at 62,756 solid triangles, indexed into an eight-unit grid so a query touches
a few dozen.

**Sliding falls out of resolving by push-out rather than by sweeping.** A swept
test finds the first surface a path crosses and stops there, which is exact and
sticks at a shallow angle. Pushing the destination back out of whatever it ended
up inside keeps the component of the move along the wall and removes only the
component into it.

Two things the tests caught immediately, both written down because they are the
same shape:

- `slide`'s own doc comment conceded that push-out can tunnel through a thin
  wall on a large step, and the test asserted "the one thing that must never
  happen is ending up on the far side of a wall". It failed at once. A
  Moller-Trumbore check on the *path* is now the final refusal, so the guarantee
  is real rather than conditional on the caller taking small steps.
- A floor test expected `None` under a box and got `Some(0.0)`. The test was
  wrong about its own fixture: a closed box has a bottom, and that bottom is the
  floor under the character's feet.

The M2 parse is checked against something that does not come from the parser --
each model's own declared collision box. That matters more than usual here,
because collision geometry read at a wrong offset produces an invisible wall
somewhere else, with no visual symptom at all until somebody walks into nothing.

#### Stairs, and the fix that is a way of breaking walls

Reported straight back: the abbey's steps and every small bump stopped the
character dead. A stair riser is a vertical face, so it is a wall by every test
in the crate and pushes the character off it -- while `floor_under` sat there
ready to stand them on the tread they could not reach. Both halves worked; they
could not reach each other.

The fix lifts the bottom of the collision cylinder by a step height, so anything
whose top is below the feet plus that height stops blocking horizontally and the
ground query then lifts the character onto it. `step` is therefore exactly "how
tall a thing may be and still be walked over" -- and raise it far enough and
every wall in the game is a kerb. The test holds something on each side of that
line: a 0.3 step is walked over, a 5.0 wall of identical footprint is not, and
with the allowance set to zero the step blocks again, so it fails if the riser
ever stops blocking for some reason other than the step.

The same lift had to be applied to the path check's sample heights. It samples at
ankle, middle and head, and an ankle sample finds every stair riser -- it would
have refused the exact move the band above it had just allowed. The same bug one
layer up, which is the recurring shape here.

`STEP_HEIGHT` came down from 1.2 to 0.8 in the same pass. At a body height of 2.0
the original was waist-high: it climbs stairs and also strolls over fences. One
constant serves both `floor_under`'s reach and `slide`'s threshold, because two
would let a character be stopped by a step it was simultaneously tall enough to
stand on.

#### And a camera welded to a discrete decision

Standing is discrete -- the feet are on this triangle or that one -- so walking
up steps moves Z in jumps of a riser, and where a floor meets the terrain the
answer can flip between two values a hair apart on consecutive frames. Rigidly
attached, the camera reproduced every one of those as a shake.

The camera now eases *vertically* toward the character, closing a fraction of the
remaining error per second rather than moving at a maximum rate -- the same trap
the creature-turning code documents, where a rate cap looks right in every frame
and then falls arbitrarily far behind. Only the vertical: smoothing the
horizontal would trade a shake for a camera that trails a running character.

**It treated a symptom, and the follow-up said so.** The next report was that the
*character* stutters on stairs and the camera wobbles on flat ground too -- which
is one cause with two faces, because the camera eases towards a height that is
itself oscillating. Rather than guess a third time between the three candidate
explanations (two surfaces alternating, a floor and the terrain trading places,
or a horizontal move being refused and retried), there is now a debug line naming
both candidate heights whenever the standing height changes with a building
involved. Gated on a building because on a hillside the terrain answers
differently every frame by design.

#### Jumping had no animation at all

Not collision, found beside it. `Motion` had no airborne state, so a character
mid-flight resolved to Run or Stand. The models carry `Jump` and always did;
nothing ever asked for it.

Worth recording how nearly the id went in wrong. On the human male, `JumpStart`
and `Fall` are **sequence indices** 15 and 17, while their `AnimationData` ids
are 37 and 40. Reading the id off a model listing looks entirely plausible and
lands on `HandsClosed`.

#### What is still wrong

Both known, both reported, neither fixed:

- **Transitions cut rather than blend.** The jump animation appears the instant
  the state changes, because nothing here interpolates between two cycles. That
  is a general animation-system gap, not a jump one -- every state change in the
  client snaps.
- **The stair stutter is better and not gone.** The instrumentation above is in
  place to find the rest of it.

Nothing collides with creatures or other players: those are moved by the server
and are a different feature from a solid world.

### 4.13: what the character is carrying

Bags, a character panel, and the first write this client makes that *moves*
something. Loot and corpse release are deliberately not here -- they are the
last item on the list and they start with a survey, not with code.

The design was specified up front and is a deliberate departure: **all bags
combine into one window**, where the original client gives each bag its own
frame. That is not a simplification. A character with four bags has five
draggable windows to arrange in the original, and this interface is already
customisable -- so the thing separate frames buy you, putting them where you
like, is provided by moving the one window. The character panel *is* separate,
because it answers a different question and there is nothing to combine.

#### Five fields, and only one of them was hard

Four of the five fell out of the technique this project already had: change
something on a live realm, diff the player's own field block, keep what moved.
`PLAYER_FIELD_INV_SLOT_HEAD` is `0x0144` with two fields per slot;
`PLAYER_FIELD_COINAGE` is `0x0492`; a container's capacity is `0x40`.

The base of the slot array is worth recording as a *shape* rather than a number,
because a near-miss is not blank. Read two fields early and the array still
yields perfectly plausible guids -- they are the high word of one guid beside
the low word of the next. So the check was not "does this produce guids" but
"does it put them where something else says they should be": with this base, a
starting human warrior's four items land on slots 3, 6, 7 and 15, which is
shirt, legs, feet and main hand. The identification predicts which four slots
are occupied, and that is a claim it could have failed.

**The stack count is the one that needed the other rule.** An item object
carries eight fields and *three* of them hold the constant 1 on every item a
starting character owns, so "contains a plausible stack size" costs nothing and
proves nothing -- the same trap as a column of small integers landing inside a
130-row table. What settled it was asking for numbers nobody holds by accident.
`.additem 2589 3`, `.additem 2592 5` and `.additem 4306 17` produced items
reading 3, 5 and 17 in field `0x0E` and 1 everywhere else, with every other
field constant across all three. Two values could be a coincidence between
neighbouring columns; a field that follows an arbitrary number we chose, three
times, is reporting that number.

#### The equip write, and why it could be confirmed at all

`CMSG_AUTOEQUIP_ITEM` is `0x010A`, and nothing acknowledges it. That is the
`CMSG_ATTACKSWING` situation again: an outgoing number that is wrong is not
refused, it is read as some *other* valid request, and the silence is identical
either way.

What makes this one checkable is that a correct send has a loud consequence. The
item's guid leaves its slot in `PLAYER_FIELD_INV_SLOT_HEAD` and reappears at an
equipment index, and both halves arrive in the next object update. So the
instrument snapshots the whole slot array, sends, waits, and prints every slot
whose occupant changed. Twelve items moved; a wrong opcode moves nothing.

The *auto* form was chosen over one naming a destination, and that choice paid
twice. It is the simplest possible write -- two bytes, a bag and a slot -- and
the server's choice of destination is a fact about the item that this client
would otherwise have to guess at.

#### Eighteen slots named, and the nineteenth left alone

Wearing one item of each kind and recording where it landed named slots 0-16 and
18. Two of those overlap the starting-gear prediction and agree with it: boots
arrived at slot 7 and a sword at slot 15, which that earlier and entirely
unrelated reasoning had already called Feet and Main Hand. Independent
derivations agreeing is the evidence; either alone is not.

**Slot 17 is unnamed, and the test asserts that it is.** It is the single gap in
an otherwise contiguous run of nineteen, which makes "it must be ranged"
overwhelmingly tempting -- and every ranged item tried came back refused,
because the character doing the testing is a warrior with no proficiency for a
bow, a rifle or a fishing pole. "Presumably" is not a measurement, and a slot
label is exactly the kind of wrong answer that never fails loudly: it draws a
helm in the boots square and reads as a rendering bug. A hunter would settle it
in one run.

#### An instrument that could not tell "ignored" from "refused"

The first equip sweep reported three failures as `nothing moved`, which is two
completely different findings wearing one sentence: an opcode the server never
understood, and a correct opcode it deliberately declined. The fix is the one
this project keeps relearning -- print every opcode that arrived, decoded or
not. With that in place the two remaining failures each showed a single `0x0112`
and the twelve successes showed none, which says the send was understood and the
answer was no.

#### A bag's contents are still unaddressable, and that is a measurement too

A container announces its capacity the moment it is replicated, including one
merely sitting in the backpack. It carries no field naming what is *inside* it
-- which is consistent rather than surprising, because every bag observed was
empty, a create block omits zero fields, and an empty slot is a zero. An empty
bag and a bag whose contents array we cannot find look identical.

Getting a non-empty equipped bag turned out to be the obstacle, and not for
protocol reasons. `.additem` places a bag in the backpack and never in a bag
slot. Editing `character_inventory` directly does not survive: the server's
loader declines a hand-placed bag and relocates it *and its contents* into the
backpack, which the wire then reports faithfully. Three runs showed the database
and the wire disagreeing in exactly that way, with the database unchanged
afterwards -- because a session that ends by closing the socket never saves, and
that is also why `.additem` alone had appeared not to persist. `.save` is now
part of the exchange, and `--say` is repeatable so it can be.

So the bag window shows the backpack's sixteen slots and says so. The legitimate
route to an equipped bag is a client asking for it, which is another write, and
moving items between slots is its own feature.

#### What is still wrong

- **Nothing can be moved yet.** The equip write exists and is confirmed, but the
  interface does not send it -- clicking a bag slot does nothing. Dragging
  between slots needs the swap request as well.
- **Items have no names.** `Item.dbc` does not carry them; they come from the
  server, and this client does not speak the item query yet. A slot shows its
  icon and its entry, which is honest and checkable, rather than an invented
  name.
- **Slot 17, and a bag's contents**, both above.

#### The loot survey, and what it does and does not establish

Started, not finished. Recorded here so the next attempt begins from measured
facts rather than repeating the run.

**Confirmed against the realm:**

- `CMSG_LOOT` is `0x15D`, body an *unpacked* eight-byte guid. Confirmed by
  reply -- which makes it far cheaper to establish than the equip write, where
  nothing acknowledges the send and a field had to be watched instead.
- `SMSG_LOOT_RELEASE_RESPONSE` is `0x161`: nine bytes, a guid and one byte.
  Notably it arrives *in answer to* `CMSG_LOOT` when a corpse has nothing on
  it -- the server closes the window rather than sending an empty one.
- `SMSG_LOOT_RESPONSE` is `0x160`, seen at ten bytes for an empty corpse: guid,
  a type byte, and one more.

**Not established:** the shape of a loot response that actually carries items,
which is the only shape worth writing a parser against. A GM `.die` kill
generates no loot at all, and the corpses reached with `.damage` came back
empty too, so no populated response has been captured yet. Nothing in
`crates/world` parses loot, and nothing should until one is.

Two instrument faults were found on the way and both are fixed, because each
would have made a future run's silence unreadable:

- The corpse's distance was measured from replicated state, which holds this
  client's *login* position forever. It reported fifteen units and refused a
  request that succeeded at 1.8 once the walked position was passed in. Third
  time this fact has been rediscovered by a new caller; it is a parameter now
  rather than another comment.
- A `nothing came back` printout could not distinguish an ignored opcode from a
  refusal. It now prints every opcode and body that arrived.

Still open: `--select --target <name>` and `--attack` choose their targets by
different rules, so a run can select one creature and walk to another.

### 4.13 continued: both deliberate gaps closed, by changing the character

Two things were left explicitly unmeasured at the end of 4.13, each with a test
or a comment asserting the silence. Both closed in a single login, and the way
they closed is more useful than either answer.

**They were being treated as protocol problems and they were not.** Equipment
slot 17 could not be named because every ranged weapon offered to the test
character came back refused with a single `0x0112` -- a bow, a rifle, a fishing
pole. A bag's contents could not be located because no non-empty bag had ever
been seen: `.additem` never places a bag in a bag slot, and a bag placed there
by editing `character_inventory` is relocated into the backpack by the server's
own loader. The response to both was to look for better items and cleverer
fixtures, and neither moved.

A **dwarf hunter is created wearing an Old Blunderbuss and a Small Ammo Pouch
with two hundred Light Shot already in it.** That is a fixture the server builds
itself, and one `--create` answered both questions. The generalisation is worth
keeping: a refusal is a fact about the *actor*, not about the thing being asked
for, so when a request keeps being declined the question is who is allowed to
make it. Creating a character costs one command and was not tried for far too
long.

#### Slot 17 is Ranged, and two structures say so

The blunderbuss lands at index 17, and two unrelated parsers agree: the
character list reports the character wearing inventory type 26 at index 17, and
the update-field slot array puts that item's guid at slot 17. Those share no
code. The test that asserted slot 17 must stay unnamed has been rewritten to
assert it is `Ranged` -- retired by a measurement rather than by someone
deciding the inference was probably fine, which is the only acceptable way for
that particular assertion to die.

#### A bag's contents, and a field pair that could not be told apart

`CONTAINER_FIELD_SLOT_1` is `0x42`: guid pairs at stride two, the same shape as
the player's own slot array. One contained item locates the base and says
nothing about the stride, so two more stacks went into the pouch and produced
guids at `0x44` and `0x46`.

The better find was next to it. `ITEM_FIELD_OWNER` (`0x06`) and
`ITEM_FIELD_CONTAINED` (`0x08`) hold **the same value on every item a starting
character carries** -- the player's own guid, `1` on one test character and `4`
on another, each matching that character. Two fields holding one correct-looking
constant confirm either reading equally well, which is to say neither. The
hunter's pouch separates them in a single dump: of ten items, the seven held
directly have both fields equal to the player, and the three inside the pouch
have `0x08` holding the *pouch's* guid while `0x06` still holds the player's.
The field that changes when the containment changes is the containment field.

That also explains why a contained item is invisible to the player's slot array
and needs the containment field at all: it is replicated as an ordinary item
object that no slot in the player's own array mentions.

#### What the window does now

The bag grid is the backpack's sixteen squares followed by each equipped bag's,
in bag-slot order, every square filled by its slot index rather than by packing
items in. The bags themselves are not drawn as squares -- a bag is a container
rather than a thing you carry, and drawing it beside its own contents would show
the same items twice.

#### And the loot response, once the population could answer

`SMSG_LOOT_RESPONSE` parses. What had been blocking it was never the protocol:
three earlier attempts came back with the *empty* form, and an empty response
says almost nothing about where an item list lives.

The mistake was hoping. A GM `.die` kill generates no loot at all, and an
ordinary low-level creature killed with `.damage` usually rolls nothing, so
every run was sampling a population that could not exhibit the thing being
looked for -- the same error as believing a flat property-test result without
checking the rows could answer the question. `creature_loot_template` names a
handful of creatures with a 100% entry, and `.npc add 103` put one within
reach. It dropped on the first try.

The layout:

```
u64  guid            u8   loot type        u32  money, in copper
u8   item count
  per item: u8 loot slot, u32 entry, u32 count, u32 display id,
            u32 random property, u32 random suffix, u8 slot type
```

**Confirmed by a relationship the packet does not control.** Each item block
carries an entry *and* a display id, and those are bound together by
`Item.dbc`, which the server never sends. The first capture held entry 2070 and
display 6353; `Item.dbc` and the server's own `item_template` independently
agree that item 2070 -- Darnassian Bleu -- has display 6353. A second run
dropped entry 1374 with display 16659, which pairs the same way. Shift the item
block by one byte and that agreement breaks, where a length check would still
pass.

**The short form is a second message wearing the same opcode**, and the two are
told apart by length alone -- there is no discriminator in the body. An empty
corpse is ten bytes: guid, loot type, one status byte, and nothing else, where
the full form's header alone is fourteen. That matters twice over: reading the
money and count fields anyway would invent four bytes the server never sent,
and refusing the packet would turn the commonest case into a parse error. The
status byte is returned raw, since only `0` has been seen.

Also worth recording: `SMSG_LOOT_RELEASE_RESPONSE` arrives **in answer to
`CMSG_LOOT`** when a corpse is empty. The server closes the window rather than
sending an empty one, so a client that treats it purely as an acknowledgement
of its own release request will be surprised.

Still to do: nothing sends `CMSG_LOOT` from the interface, `CMSG_LOOT_MONEY`
and the take-an-item request are unexercised, and corpse release is unwired.

#### And taking it

`CMSG_LOOT_MONEY` (`0x15E`) and `CMSG_AUTOSTORE_LOOT_ITEM` (`0x108`) both work.
Neither is acknowledged, so both were confirmed the way the equip write was --
by a consequence that could have failed to appear. Opening a corpse holding two
copper and two items, then sending both requests, moved the money from 0 to 2
in `PLAYER_FIELD_COINAGE` and put two new guids in the player's own slot array
whose entries are exactly the two that were on the corpse.

`CMSG_AUTOSTORE_LOOT_ITEM` carries one byte, and the byte is the **server's own
loot slot index** rather than a position in whatever list a client has built.
The distinction is not academic: a corpse whose first slot has already been
taken still numbers the rest from where they were, so a client that filtered
its list and re-indexed would ask for the wrong item. Both requests act on the
loot *currently open* and name no corpse at all, so they are only meaningful
after `CMSG_LOOT`.

What is left is interface rather than protocol: nothing sends any of this from
the client, so there is no loot window yet.

#### The loot window, and three bugs that only a person at a window could find

The protocol was done; the interface took three live attempts, and each failure
was a different shape. All three are worth recording because none of them would
have shown up in a test written before the fact.

**The corpse was released a frame after being asked for.** The release logic
was "if replicated state holds no loot and we asked for some, release" — which
is true on every frame between the request going out and the answer coming
back. The window never appeared, and the log said `asked to loot` ten times,
which reads exactly like a server ignoring the request. What it needed was to
distinguish *asked* from *open*: they look identical from replicated state,
since neither has any loot in it. Two states, and only the second may be
released.

**The window sensed hover, not clicks.** Frames opt into `Sense::click()` by
appearing in one `matches!` in `Hud::show`, and a frame left out of it draws
correctly, hit-tests correctly, and simply never reports a click — so the arm
handling the click is dead code that looks alive. The symptom was a window that
opened and did nothing. There is now a headless test that clicks a loot row and
asserts what comes back, which fails if the frame ever drops out of that list.

**And the camera kept the mouse after the button was up**, reported straight
back: *"the camera should never capture the mouse if left or right click are
NOT held down"*. The drag flags were cleared in the branch that handles a
release — which sits *after* the check that hands the event to egui first and
returns if egui consumed it. The loot window opens *on* a right-click and
appears under the cursor, so it swallowed the very release that ends the
gesture, and the flag stayed set for good.

That one generalises past loot: **state mirroring a physical input has to be
corrected from the input's end, not from the path that usually handles it.** The
flags are now cleared before anything can consume the event, and on focus loss
as well, since alt-tabbing with a button down never delivers a release at all.

#### Taking loot needs the packets that say it is gone

A fourth bug, and the one that took a survey rather than a fix. Taking an item
worked on the server and the window went on showing it, because nothing parsed
the messages that say a slot is gone. The corpse therefore never emptied, never
closed, and was never released — a body left locked to this client for everyone
else on the realm.

Both were identified by content rather than by number:

- `SMSG_LOOT_REMOVED` (`0x162`) — one byte, the loot slot. Taking slot `0`
  produced a one-byte body holding `0`.
- `SMSG_LOOT_CLEAR_MONEY` (`0x165`) — empty. A zero-length body is itself the
  identification; nothing else arriving then carries no payload at all.

Removal is **by slot, not by position**: the numbers do not close up when one
goes, so the rows that remain keep the numbers the server still uses for them.

Two others arrive and are deliberately not parsed: `0x163` carries the money
amount, which `0x165` already covers, and `0x166` is the "you received an item"
popup, which this client has no use for yet.

Finding all of this needed the survey command fixed first: it released the
corpse and *then* tried to take things off it, so the money moved and the item
did not, and the printout said `0 new item(s)` as though the opcode were wrong.
Anything acting on open loot has to happen before the release.

### 4.14: sound

The client makes a noise. Zone music, ambience, creature voices and weapon
impacts, all driven by the tables rather than by anything hardcoded.

`SoundEntries` is the table everything points at -- 12,941 rows, ten filenames
and ten weights apiece, because a sound is a *set* of files and a footstep or a
sword hit picks one at random each time. Its column layout was read off the
file's own shape and then checked against something the data cannot fake: the
strings have to name **files that exist**. 93% of file references resolve, and
the 7% that miss are clustered in character voice lines rather than scattered,
which is a different finding and visible as one. A one-column slip resolves
essentially nothing.

The type column was measured rather than remembered. It runs 1-53 over 26
values, and instead of writing down which number means music, `sound types`
tallies which folders each value's entries live in: type 28 is 629 of 632 under
`Sound\Music`, type 50 is 273 of 273 under `Sound\Ambience`, type 10 is 6,153 of
6,380 under `Sound\Creature`. Only those three are named. Type 1 is 69%
`Sound\Spells` and not clean enough to put a name on.

#### The zone chain, and a check stronger than validity

Position to ADT chunk to `AreaTable` to `ZoneMusic` or `SoundAmbience` to
`SoundEntries` to a file. Every link but the last already existed.

`ZoneMusic` ids run into a table of 12,941 rows spanning ids 3-18019, so any
small integer lands on a real row -- validity separates nothing. What a wrong
column cannot do is land on a row of the right *kind*: **99.1% of 1,144 zone
sound references resolve to a sound whose type matches what named it**, and
those types were themselves measured. The ten that miss are five dangling ids
counted twice, in Blizzard's own data.

Day and night being two columns rather than one written twice is measured the
same way. `Zone-Forest` has 2523 in both and proves nothing; `Zone-EvilForest`
has 2524 and 2534. A column that differs from its neighbour on some rows and not
others is two things.

Area is stored per *chunk*, not per tile. A tile is a third of a mile square and
Elwynn's carry Goldshire, the abbey and open forest at once, so keying off the
tile would change the music a third of a mile from where the zone does. A tile
still streaming in answers `None`, and that is held rather than treated as
silence -- otherwise the music cuts every time the player outruns the loader.

#### Creature voices, and a column that was an override

`CreatureSoundData` is 38 columns of sound ids with nothing saying which is the
death cry. They identified themselves through the *names* of the sounds they
reach: a column whose entries are called `WolfDeath`, `BearDeath` and
`KoboldDeath` is the death column. Five came back overwhelming -- attack
(`Attack` x787 of 818), wound (`Wound` x826 of 836), critical wound
(`Crit`/`Critical` x749), death (`Death` x634) and aggro (`Aggro` x459) -- and
the rest stay unnamed.

Field 0 needed proving too, and is the nice one: it is set on all 1,306 rows and
935 of those "resolve" to a real sound, which looks like a populated sound
column until you notice only 102 of them are *creature* sounds. It is the id.
Ids overlapping a table's id range is a coincidence of magnitude, not a
reference.

**Then combat was silent, and the log said why.** `CreatureDisplayInfo`'s
`sound_id` is an *override*, and most creatures do not use it: the Diseased
Young Wolf's display carries zero, while its **model** carries 43, which is the
row holding a wolf's growls. Reading only the display found voices for 1,205
displays of 24,262 -- and every creature anyone fights in a starting zone was in
the silent majority, which presented as the feature simply not working. Falling
back to the model took it to 24,220.

#### Weapon impacts, and a sound that arrived before the sword

`WeaponImpactSounds` is thirty rows, one per weapon subclass, each naming ten
sounds for the ten things a weapon can hit and ten more for the criticals. The
columns named themselves again: row 1's first three ids are `Axe1H_ArmorFlesh`,
`Axe1H_ArmorChain` and `Axe1H_ArmorPlate`, and the second block's first is
`Axe1H_ArmorFleshCritical`.

Only flesh is transcribed. Chain and plate need the target's armour, which this
client cannot see, and guessing between them is exactly what it refuses to do.

The last problem was timing, and it came back from the window as *"the audio
plays -> sword makes contact"*. The server reports a swing when it **resolves**
it and the client only then *starts* the attack animation, so an impact played
on arrival lands before the blade. The right number is the time from the
animation's first frame to the frame the weapon connects -- which is in the
model's own timed events, and this client parses none of them. So it is a
constant found the only way available: adjustable in play with `[` and `]`, and
dialled in by a person watching a sword. **625ms.** It stays adjustable because
that is what it is.

#### What sound is not

No distance attenuation, no volume curve beyond a flat multiply, no crossfade
between zones, and `ZoneMusic`'s four silence-interval columns are transcribed
and ignored -- this loops where the original pauses between tracks. Other
players' weapons make no sound, because their equipment arrives as visible-item
fields this client does not read. Spell sounds, footsteps and interface clicks
are all untouched.

### 4.15: NPCs answer

The first send of the NPC-interaction milestone, and the first reply. Greeting
an NPC produces a menu, and every field of it is parsed and confirmed.

**This milestone is a protocol one, not a format one, and that changes the
method.** Everything through 4.14 lived in DBC tables that ship with the client,
so the technique was: transcribe a table, then find a property the data must
have and check it. Gossip text, menu options, vendor stock and quest text are in
the *server's* world database and arrive only when asked for. Nothing on disk
can be consulted. What replaces it is the other half of this project's toolkit --
send, watch, and confirm by effect -- plus, on a test realm, the fact that the
world database is *readable* and is a source the client is never sent. That
makes it the same class of evidence as `Item.dbc` pairing a loot entry with its
display id, and it is what every claim below rests on.

#### Why gossip went first

`CMSG_GOSSIP_HELLO` is **answered**, which is the cheapest confirmation
available here. Nothing acknowledges an opcode as such, and a wrong outgoing
number is read as some other valid request rather than refused -- so
`CMSG_AUTOEQUIP_ITEM` had to be confirmed by watching a guid move between two
fields, and `CMSG_ATTACKSWING` by varying the range and seeing refusals turn
into swings. A reply arriving at all says the number was understood. `0x017B`
was right on the first send, and `0x017D` came back carrying a menu.

#### The layout, and three packets that had to disagree

```
u64 npc guid
u32 menu id
u32 greeting text id
u32 option count
  u32 index, u8 icon, u8 coded, u32 money, cstring message, cstring box message
u32 quest count
  u32 quest id, u32 icon, i32 level, u32 flags, u8 repeatable, cstring title
```

One packet cannot establish this and it is worth saying why: most of a gossip
menu is zeroes, so almost any reading of the two variable blocks survives a
single sample. Three NPCs were greeted, chosen so that the counts differ:

| greeted | bytes | shape |
|---|---|---|
| Innkeeper Farley (295) | 136 | menu 1291, **3 options**, 0 quests |
| Marshal McBride (197) | 24 | menu 4048, 0 options, 0 quests |
| Deputy Willem (823) | 57 | menu 57020, 0 options, **1 quest** |

A layout with the quest block in the wrong place parses Farley's packet
perfectly, because Farley offers no quests. It takes Willem's to break it. All
three consume their bodies exactly.

Every field then agreed with the world database independently: the menu ids are
`creature_template.gossip_menu_id` for each of the three NPCs, 1291's greeting
text id is 820 in `gossip_menu`, Farley's three options match
`gossip_menu_option` in text *and* icon, and quest 783 arrived with title
`A Threat Within`, level 1 and flags 524296 -- the last being the load-bearing
one, since a title could conceivably be matched by luck at a nearby offset and
524296 could not.

#### The option index is the server's id, and a filtered menu proved it

`gossip_menu_option` has **four** rows for menu 1291. Three arrived. The missing
one is `Trick or Treat!`, a Hallowe'en seasonal line the server filters out --
and the three that came carried indices 1, 2 and 3, with **0 absent**. The
numbering does not close up.

So an option index is the server's own id and never a row position, exactly like
a loot slot. A client that replied with a row number would ask for the wrong
thing, and would do it only when talking to an NPC whose menu happens to be
conditional -- a bug that hides until it is expensive. It has its own test.

#### An empty quest list from a questgiver, which was correct

Greeting Marshal McBride -- npcflag 3, a questgiver -- returned zero quests,
first with a level-5 character and then with one that had never taken a quest at
all. That looks exactly like the quest block being in the wrong place.

It was right. Every quest McBride starts is gated behind `A Threat Within`, and
somebody else gives that one out. The population could not exhibit the thing
being looked for, which is this project's most frequently repaid lesson --
the same shape as three empty loot responses from creatures that roll nothing,
and as `Light.dbc`'s storm column coming back a coin flip across 200 decorative
lights. Deputy Willem starts 783 with no prerequisite, and one greeting produced
the quest block.

#### What the instrument refuses to do

`wow-cli world --gossip [entry]` picks its target by `UNIT_NPC_FLAGS` rather
than by proximity, walks into range, prints the flags before sending, and
**refuses to send from out of reach**. That last part is the design: a greeting
that produces nothing is equally what a wrong opcode, an NPC with no gossip bit,
and an NPC across the field look like -- three investigations behind one
printout. Keeping them apart cost `--loot` three runs, and the lesson was
applied rather than re-learned.

It also takes an optional creature entry, because `.npc add` puts every spawn at
the caller's feet and "the nearest talker" then picks arbitrarily between them --
and comparing what *different* NPCs answer is the whole method for naming the
flag bits.

#### The flag bits are still not named

`UNIT_NPC_FLAGS` reads 3 on both questgivers and 66179 on the innkeeper. 66179
is `0x10283`, so five bits are set, and the two samples together are consistent
with `0x1` being gossip and `0x2` questgiver -- which is a hypothesis, not a
finding, and neither is written down as a name yet. They will be confirmed the
way the loot and equip writes were: send the request a bit is supposed to gate
and see whether it is answered. Farley carries `0x80` and offers
`I want to browse your goods.`; a vendor request that he answers and McBride
refuses is what would name it.

#### What is not done

Nothing can be *chosen* yet -- `CMSG_GOSSIP_SELECT_OPTION` is unsent, so the
menu is readable and not clickable. The greeting text id resolves to nothing,
because `npc_text` is a server table with its own query. Vendors, buying,
selling and the whole quest flow past the one-line summary all remain, in that
order: the vendor list is checkable against `Item.dbc` the way the loot response
was, and quests are last because they have the most fields and the least
external check.

The three NPCs spawned for this are deliberately left standing at `Testwolf`'s
login spot on the local realm. An innkeeper, a questgiver whose chain is gated
and a questgiver whose is not, all within greeting range, is the fixture the
rest of this milestone needs -- and building one by changing the *character* or
the *cast* rather than the technique is what closed both of 4.13's gaps.

### 4.15 continued: a menu can be chosen, and a vendor lists its stock

Two opcodes, one run, and the second confirms the first.

#### `CMSG_GOSSIP_SELECT_OPTION` (`0x017C`)

Body is `{u64 npc guid, u32 menu id, u32 option index}` and a trailing string
for *coded* options -- empty for everything observed. **The index is the
server's own option id**, taken from the reply rather than from anything the
caller typed, because menu 1291 is the standing proof that the numbering has
holes in it.

Nothing acknowledges a selection as such, so it is confirmed the way the equip
and loot writes were -- by an effect that could not have happened otherwise.
Choosing Innkeeper Farley's `I want to browse your goods.` produced
`SMSG_LIST_INVENTORY`: a *different opcode carrying a stock list*, which no
misunderstood request would have caused.

#### `SMSG_LIST_INVENTORY` (`0x019F`)

```
u64 vendor guid
u8  row count
  u32 slot, u32 entry, u32 display id, i32 remaining (-1 = unlimited),
  u32 price, u32 unknown, u32 buy count, u32 extended cost
```

Thirty-two bytes a row, and Farley's reply was 393 -- which is `8 + 1 + 12 * 32`
exactly. The twelve rows are the twelve in the server's `npc_vendor` table for
that creature, in the same order, and each pairs an item entry with the display
id `Item.dbc` independently gives it: 159 with 18084, 414 with 21904, 422 with
6352.

**One of those pairs was already confirmed by a different packet.** Item 2070,
Darnassian Bleu, display 6353 -- the exact pair that verified
`SMSG_LOOT_RESPONSE`'s layout in 4.13. Two unrelated packets, parsed at
different times against different tables, agreeing on the same two numbers is
about as good as corroboration gets here.

#### The price is not the price in the table

The field worth reading this milestone for. The wire does **not** carry
`Item.dbc`'s `BuyPrice`; the server applies the buyer's reputation discount
first, and the arithmetic is unmistakable across three very different values:

| item | `BuyPrice` | on the wire |
|---|---|---|
| Refreshing Spring Water | 25 | 23 |
| Dwarven Mild | 500 | 475 |
| Moonberry Juice | 2000 | 1900 |

`BuyPrice * 0.95`, truncated. A client that displayed the table's number would
be wrong for every player at any standing but neutral, and nothing about the
result would look wrong -- the numbers are all plausible. This is the same
class of hazard as a fabricated tooltip value, and the rule falls out the same
way: **the wire is authoritative for price and the table is not.** The test
asserts the relationship rather than three constants, and asserts the table's
value is *not* what arrives, so nobody "fixes" it back.

#### A bug in the instrument, found by the fixture moving

`--gossip` walked to *exactly* its interaction reach and then asked whether it
was within it. With the NPCs spawned at the caller's feet that never mattered;
with `Facetest` standing 4.04 units from an innkeeper and the reach set to 4.0,
it produced three rounds of "closing 0.0 units" and then refused to send -- a
loop asymptotically approaching the threshold it was waiting to cross.

The fix is two numbers instead of one: the range the *server* will talk from,
which is what the send is tested against, and a closer distance the walk aims
for. Walking past the line rather than up to it. Collapsing a limit and a
target into one constant is the same mistake as a smoothing constant that is a
maximum speed -- it looks right until the input sits exactly on it.

#### Buying and selling, and a silence that had to be bounded

`CMSG_BUY_ITEM` (`0x01A2`) is `{u64 vendor, u32 item entry, u32 vendor slot,
u32 count, u8 bag}` -- twenty-one bytes. `CMSG_SELL_ITEM` (`0x01A0`) is
`{u64 vendor, u64 item guid, u32 count}`, naming the item by **guid** rather
than by slot so a request that races an inventory change refuses instead of
selling whatever moved into that index.

**The first attempt at the buy body got total silence**, which is the least
informative failure available: three bytes short, the two `u32`s transposed,
and the count sent as a `u8`. Nothing came back at all, and that is equally
what a wrong opcode, a wrong body and a declined request look like.

What bounded the search was sending `CMSG_LIST_INVENTORY` (`0x019E`) first --
an opcode four below in the same block that is *answered*, and whose reply
layout was already established. It came back with 393 bytes of stock, which
said the numbering was right and moved the whole question onto the body. **One
cheap answered request to bound a silent one** is the same move that turned
three failed attempts at chat into a one-run answer, and it is worth reaching
for before improving any guess.

#### Two fields confirmed by consequence, not by agreement

Buying one row is a better test than it looks, because it checks the *stock
list's* reading as well as the purchase:

- Vendor slot 1 quoted **23** copper. Exactly **23** left the purse, where
  `item_template.BuyPrice` says 25. A price field read from the wrong offset
  could not have predicted the charge, so the discounted-price finding is now
  confirmed by an effect rather than by a table lookup.
- The row's `buy_count` is **5**, and the item that arrived carried a **stack
  of 5**. Every row of this vendor holds 5 in that field, so agreeing with
  `item_template.BuyCount` had proved nothing -- a constant agreeing with a
  constant. One purchase settled it.

Selling the item straight back returned 5 copper and removed it from the bags.
The probe does both in one run deliberately: it is self-cleaning, so it can be
run twice, and the sell names a guid the purchase had just produced, which is
exactly how a real shop window works.

#### What is still missing

Quests. The stock list, the purchase and the sale all work; nothing is drawn in
the interface yet, and the vendor window is still a CLI printout.


## The road to a native Questie

A destination worth writing down, because it changes what "done" means for the
four milestones in front of it. The plan is to reach **Questie's features,
implemented natively**, and the ladder is deliberate rather than arbitrary —
each rung is a prerequisite for the next, not a preference.

```
4.15  NPC interaction     gossip [done], vendors, buying and selling
4.16  Quests              accept, track, turn in
4.17  Map                 the world map, and where things are on it
4.18  Minimap             the same, small, and following the player
4.19  Questie, natively   what to do, where, and whether you can yet
```

### Why this order and not another

A quest tracker is a map feature. Questie's whole value is *"the thing you need
is over there"*, and there is no "there" to point at until a map exists — so
building the tracker first would produce a list of objectives with nowhere to
put them. Equally, a map with no quest state on it is a picture. The two are
one feature separated by a dependency, and the dependency runs one way.

Vendors come before quests because a quest reward is an item and a turn-in is
an inventory write, and the vendor work confirms both paths against a server
that answers loudly. Quests then reuse them rather than debugging them.

### Decided: quest data comes from the server, not from a shipped database

**This is a settled decision, not a preference.** Quest data is asked for over
the wire and rendered on the map and minimap; Questie's hard-coded database is
not ported.

**Most of Questie's bulk is a workaround for a restriction this project does
not have.** An addon cannot ask the server about a quest it has not already
been offered, so Questie ships a hand-collected database of every quest, NPC,
object and spawn point in the game — hundreds of thousands of rows, gathered by
observation because the API forbade the question.

This client *is* the client. It can send `CMSG_QUEST_QUERY` and the questgiver
status queries and be told, by the server, what a quest wants and who wants it.
Server-supplied data needs no licence, cannot drift from the realm being played
on, and is correct on a private server with custom content where a shipped
database is simply wrong.

So 4.19 is a **presentation** milestone, not a data one: the pins, the tracker,
the availability colouring, the "you are too low level for this" state. Facts
come off the wire wherever the wire will answer, and only what the server
genuinely will not volunteer — static world facts an addon collected by looking
rather than by asking — is a candidate for reimplementation. That judgement gets
made per feature, with the wire tried first.

**The payoff is that quests are never out of date.** A shipped database is a
snapshot of one version of one server's content: it drifts as the game is
patched, and it is simply wrong on a private realm with custom quests, which is
exactly what this client is developed against. Asking the server cannot drift,
because the answer *is* whatever the realm being played on believes. It is also
strictly less code — no rows to maintain, no import pipeline, no staleness
policy.

**What that decision costs, and where the cost lands.** The query layer becomes
a **4.16 requirement rather than a 4.19 one**, because the map has nothing to
draw without it. `SMSG_GOSSIP_MESSAGE`'s quest block is a title and a level and
nothing else — no objectives, no locations, no turn-in — so 4.16 has to build:

- `CMSG_QUEST_QUERY` and its response, for what a quest actually asks of you.
- The questgiver status queries, for the `!`/`?` state over an NPC's head — the
  single most Questie-ish thing on the screen, and the one that decides whether
  a pin is drawn at all.
- Somewhere to keep the answers, since a query is a round trip and a map redraw
  is not. Cache by quest id, and treat a missing answer as *unknown* rather than
  as *nothing* — an absent reply must not render as "no objectives", which is
  the same absent-versus-default trap `PLAYER_BYTES` and the loot short form
  both cost this project once already.

The tempting shortcut is to have 4.16 parse only what the quest log needs and
leave the rest to 4.19. That would mean writing the query layer twice, and the
second time against a map that is already drawing wrong pins.

**What stays open.** Some things the server genuinely will not volunteer —
where a quest's objective *is* in the world, when no NPC involved is in
visibility range. Those are the only candidates for reimplementation, and the
judgement gets made per feature with the wire tried first. It may turn out that
`SMSG_QUESTGIVER_STATUS_MULTIPLE` plus creature spawn data the client already
streams covers more of it than expected; that is a question for 4.17, not an
assumption to make now.

### Where quest data actually comes from, measured

The decision above raised a fair question: we run the realm, so we can read
every quest table in MySQL — should the client just pull the lot at launch and
top up whatever is missing?

**Two things have to be separated first, and conflating them is the trap.**
Reading `acore_world` over MySQL is a *development* capability. It works
because we happen to own this realm. A player connecting to somebody else's
server has no database access at all, so **anything built on DB reads works
only for realm operators** — which defeats the point of not shipping a
database. The DB stays what it has been all through 4.15: a verification
oracle, the independent answer a wire reading gets checked against. It is not a
data source for the client.

That leaves the protocol, and it covers more than expected.

#### What the wire will answer

| feature | opcode | coverage |
|---|---|---|
| a quest's text and objectives | `CMSG_QUEST_QUERY` `0x05C` | all |
| what is in the quest log | update fields | all |
| **map markers for objectives** | `CMSG_QUEST_POI_QUERY` `0x1E3` | **8,953 of 9,464 quests (94.6%)** |
| `!` / `?` over nearby NPCs | `CMSG_QUESTGIVER_STATUS_MULTIPLE_QUERY` `0x417` | NPCs in range |

**The POI query is the find of this investigation.** WotLK shipped its own quest
tracker, so the server already stores the map markers — 18,771 POI areas and
57,162 points on this realm — and hands them over on request: per objective, a
map id, an area id and a polygon of points. That is precisely the thing Questie
exists to draw, available over the wire, always matching the realm being played
on.

**Its one constraint shapes the design.** The handler only answers for quests
*in the player's log* (`GetQuestSlotQuestId(questSlot) == questId`), and takes
at most 25 ids per request. So POI covers "where do I go for the quest I am
on" completely, and says nothing about a quest not yet accepted.

#### What the wire will not answer

Where things *are* when you have not seen them: the questgiver you have not met,
the mobs for a quest you have not taken. The server streams creatures in
visibility range and nothing more. This realm's `creature` table holds 149,996
spawns and `gameobject` 96,628 — none of it reachable by a client.

**Answer it by observation, not by import.** This client already replicates
every creature in range with its entry and position, and already throws that
away. Recording it — keyed by realm — builds a spawn cache that is correct for
the server actually being played on, custom content included, needs no licence,
and cannot go stale. It starts empty and fills as the player explores, which is
the honest version of "launch empty and pull from the server".

#### So: no bulk prefetch at launch

The instinct is right and the mechanism is not. There is **no enumerate-all
opcode** — `CMSG_QUEST_QUERY` takes an id, so "fetch everything" presupposes a
list of every quest id, which is the database being avoided. POI is capped at 25
per request and log-only besides.

And a mass query at login is a mistake this project has already paid for once:
the login burst that took **thirty-seven seconds** because a drain loop had a
packet bound and no clock. Thousands of queries fired at a realm that punishes
eager pinging harder than no pinging is the same shape.

**Demand-driven, cached, persistent.** Ask when an id is first seen — from the
gossip quest block, a questgiver list, the quest log. Write the answer to a
per-realm cache. Later launches warm instantly and only genuinely new ids cost a
round trip, which is exactly "check if there are any missing entries" without
ever needing the bulk half. A missing answer must cache as **unknown**, never as
*nothing*: an absent reply rendering as "no objectives" is the absent-versus-
default trap that `PLAYER_BYTES` and the loot short form have each cost this
project once already.

Seeding that cache from a realm's own database, or from a public source, stays
possible as an **optional developer command** — never a dependency, never
committed, and marked as unverified wherever it is displayed, because a number
nobody can check is worse than a blank.

See `docs/REUSE-POLICY.md`'s addon section: reference copies live in the
gitignored `addons-to-port/`, are read rather than vendored, and each one's
licence gets checked and recorded before any port begins rather than during.

### 4.16: a quest taken, finished and paid for

The end-to-end run this milestone was waiting for: a character created from
nothing, a quest in its log, the quest handed in, and the reward collected.
Quest **783 "A Threat Within"** was chosen for it because nothing in the middle
could fail for a reason unrelated to the protocol — no kills, no items to
collect, and `RewardChoiceItemID1 = 0`, so the reward index is unambiguously
`0`.

**The turn-in is two sends and only the first of them talks back.**
`CMSG_QUESTGIVER_COMPLETE_QUEST` (`0x018A`) is `{u64 npc, u32 quest}` and is
answered; `CMSG_QUESTGIVER_CHOOSE_REWARD` (`0x018E`) is
`{u64 npc, u32 quest, u32 reward}` and is not. Both had existed in `client.rs`
since the milestone opened and neither had ever been fired, which is exactly
the situation this project's notes say costs the most: a write nothing
acknowledges fails identically whether the opcode is wrong, the body is wrong,
or the request was declined.

What made it cheap was that the answered half comes *first*, so it bounds the
silent half the way `CMSG_LIST_INVENTORY` bounded `CMSG_BUY_ITEM` in 4.15.
Offering the quest to Marshal McBride produced **`SMSG_QUESTGIVER_OFFER_REWARD`
(`0x018D`), 525 bytes**, carrying the quest's real completion text — so the
opcode, the body and the choice of NPC were all right before the second send
was attempted at all.

**Which of two replies arrives is itself the diagnosis**, and the probe reports
them as different outcomes rather than as one "no reward screen":
`SMSG_QUESTGIVER_REQUEST_ITEMS` (`0x018B`) means the send was understood and
the quest is simply not finished, which is a statement about the character;
silence means the opcode, the body, or an NPC that does not end this quest.

#### Confirmed by an effect nobody had to interpret

`CMSG_QUESTGIVER_CHOOSE_REWARD` went out with index `0` and
`SMSG_QUESTGIVER_QUEST_COMPLETE` (`0x0191`, 24 bytes) came back — but the
verdict rests on none of that. **The quest log went from `[783]` to `[7]`**,
read out of `PLAYER_QUEST_LOG`, a field this project measured rather than
transcribed. 783 left, and quest **7 "Kobold Camp Cleanup"** — McBride's next
quest, whose `PrevQuestID` is 783 — appeared in its place because the server
advances an auto-accept follow-up when a reward is taken. A misread of any of
those packets could not have produced a chain step.

The realm's own database agrees independently: `character_queststatus_rewarded`
holds 783 for the character, and its `xp` moved from 0 to 40.

#### The trap the run walked into: a scroll request that accepts

The same run showed the accept step doing nothing at all — 0 fields changed,
and the log already held 783 before `CMSG_QUESTGIVER_ACCEPT_QUEST` was sent.
The reason is not a bug and not a stale character:

**A quest carrying `QUEST_FLAGS_AUTO_ACCEPT` (`0x80000`) is added to the log by
the server when `CMSG_QUESTGIVER_QUERY_QUEST` arrives.** Asking to *read* the
scroll takes the quest. Quest 783's flags are `524296 = 0x80008`, and the flag
also arrives indirectly — `quest_template_addon.SpecialFlags & 0x4` is ORed
into it at load time, which is how the whole Northshire chain acquires it.

So the accept had never actually been proven. It had been *observed working*
only on 783, where the query alone accounts for every effect, and the quest-log
field's identification survives only because three further ids were added by
`.quest add` and landed at a constant stride.

**Proving it needed a quest the flag does not touch.** Only **179 of 9,464**
quests on this realm auto-accept — rare enough to look like a bug, common
enough to cover the starting zone a first end-to-end test naturally reaches
for. Quest **333 "Harlan Needs a Resupply"** has `Flags = 0` and
`SpecialFlags = 0`; its questgiver was spawned at the character's feet with
`.npc add 1427`. The scroll came back (`SMSG_QUESTGIVER_QUEST_DETAILS`, 695
bytes) and the log was **unchanged**. The accept then put `333` into field
`0x00a3` — `PLAYER_QUEST_LOG + 1 * QUEST_LOG_STRIDE`, the second slot, exactly
where the measured base and stride say the second quest goes.

That is `CMSG_QUESTGIVER_ACCEPT_QUEST` confirmed for the first time, and it is
confirmed by a number nothing else in a player object has a reason to hold.

#### The instrument was giving the wrong advice, and that is the durable fix

`--quest-accept` reported "quest 783 was ALREADY in the log, clear it first" —
sound-looking advice that sends the reader round the same loop forever, because
the very next run's scroll request re-accepts it. Two different situations, one
sentence: the same shape as an equip sweep reporting `nothing moved` for both
an unknown opcode and a deliberate refusal.

Separating them costs one extra read. The quest log is now sampled **before the
greeting** as well as after the scroll, so "the character already held this"
and "this run's own scroll request took it" are distinguishable — and they want
opposite next steps, since clearing the quest fixes the first and cannot fix
the second at all.

#### What is still missing

Nothing is drawn. `SMSG_QUESTGIVER_QUEST_DETAILS` (695 bytes),
`SMSG_QUESTGIVER_OFFER_REWARD` (525 bytes) and `SMSG_QUEST_QUERY_RESPONSE`
arrive whole and are reported as lengths, not parsed — and
`SMSG_QUEST_QUERY_RESPONSE` is the one the map and the tracker are built on.
The per-realm cache, in which a missing answer must be *unknown* rather than
*nothing*, is unwritten. A quest offering an actual choice of reward has not
been turned in, so index `0` is confirmed only where there was nothing to
choose.

### 4.16 continued: what a quest actually is, and a log to read it in

`SMSG_QUEST_QUERY_RESPONSE` (`0x005D`) parses. It is the packet the whole
feature rests on, because unlike a questgiver's scroll it answers for **any**
quest id with no NPC in front of the player and no entry in the log — which is
exactly what a tracker and a map need, and exactly why this client does not
have to ship anybody's database.

#### Measured, then confirmed, in that order

The body is a **260-byte fixed head, five strings, and then two more arrays** —
so nothing past byte 260 sits at a fixed offset and the whole thing has to be
read through a cursor.

The layout was worked out from eight captures chosen so that every array count
disagrees between them, because a quest with empty arrays parses perfectly
under several wrong readings:

| quest | what it can refute |
|---|---|
| 783 *A Threat Within* | every array empty — the control |
| 152 *The Coast Isn't Clear* | four **creature** objectives, seven each |
| 38 *Westfall Stew* | four item objectives **and** four reward items |
| 18 *Brotherhood of Thieves* | five reward **choices** |
| 498 *The Rescue* | **game object** targets, item drops, objective texts |
| 61 *Shipment to Stormwind* | a non-zero inline point of interest |
| 31 *Aquatic Form* | a start item |
| 28 *Trial of the Lake* | an item drop with **no creature to kill** |

Every non-zero value in every one of them is a value in the realm's own
`quest_template`, which no client is ever sent. Only after that did reading
AzerothCore's `Quest::InitializeQueryData` confirm the same order — and it
supplied two facts the captures could not have, both of which are now handled.

#### Two things the wire does that a table does not

**A game object target arrives with the top bit set.** The server stores it as
a *negative* creature id and sends `|id| | 0x80000000`. Reading it as an `i32`
and negating — which is the obvious thing, since that is what the table holds —
gives a perfectly valid *creature* id pointing at something else entirely.
Quest 498 is the fixture: its two objectives are game objects 1721 and 1722, and
the test asserts both that those read as game objects **and** that quest 152's
four creature targets do not.

**`Flags` arrives with only its low sixteen bits.** The server sends
`Flags & 0xFFFF`, so `QUEST_FLAGS_AUTO_ACCEPT` (`0x80000`) cannot travel: quest
783 arrives here as `8` where the realm's table says `524296`. That was measured
before it was explained, and it matters — a client that tested this field to
decide whether an accept is needed would wait forever for a quest the server had
already taken on its behalf.

#### The sweep, which is the evidence that matters

Eight hand-picked captures show a layout is *possible*. Asking the realm about
**every quest id from 1 to 26,034** shows it is right:

```
9464 answered, 9464 parsed whole
  100.0% -- bodies from 381 to 1438 bytes
```

A body that ends anywhere but its last byte is an error here, so that is the
reading confirmed across the whole table rather than across the quests somebody
chose to look at. Same instrument as `dbc check` and `m2 survey`, and the same
reason for it.

**The first version of that sweep took twenty minutes and had not finished.**
It drained with a 4,096-packet bound and no clock, against a zone that emits a
monster move fourteen times a second and is therefore *never* quiet — so every
block collected four thousand packets of background traffic to find two hundred
answers. That is the login-burst bug written up two milestones ago, walked into
again by the next loop that had a limit and no deadline. It now has a wall-clock
budget per block *and* prints progress, because a silent twenty-minute run is
indistinguishable from a hung one.

#### A cache that knows the difference between empty and unknown

`world::quest_cache` keeps what the server has said, keyed by realm. Two
decisions carry the weight:

**It holds packet bodies, not parsed structs.** A cache of parsed structs
freezes a *parse*; a cache of bodies freezes an *observation*. When the parser
learns to read a field it currently skips, every already-cached quest gains it
with no version stamp and no migration — where a struct cache would have to be
discarded, or worse, read back with the new field silently defaulted, which is a
fabricated number.

**A missing answer is `unknown`, never `nothing`.** There is deliberately no
`get() -> Option<&QuestInfo>`, because `None` would mean both "still waiting"
and "genuinely empty" and every call site would have to remember which. The
three states reach the screen intact: a quest still being asked about draws
`asking the server...`, one the realm would not describe draws `no answer from
the realm`, and neither is the same as a quest with no objectives. An id asked
about and never answered is **not** written to disk, because "there is no such
quest" and "the reply was lost" are indistinguishable and only one of them is
permanent.

#### The log itself

`L` opens it. Titles, levels and objective lines come off the wire and out of
the cache; nothing is looked up in a shipped table.

**Whether a quest is finished is read from the log's own state field**, which
had to be measured rather than assumed — every field of a log entry holds a
small integer, so validity separates none of them. The first two attempts at a
sample could not have answered the question: quest 783 has no objectives and so
is complete the instant it is taken, and quest 333's `StartItem` *is* its own
`RequiredItemId1`, so accepting it hands you the item and completes it too. Both
read `1`. Putting quest 38 — which wants twelve items the character does not
have — beside 783 gave `0` against `1` with all three remaining fields zero on
both, which names the column and nothing more. Only bit zero is read, because
that is the only bit anything here has seen move.

### 4.16 continued: quests can be taken and handed in from the window

Right-click a questgiver, read what it says, press Accept. Walk to the ender,
right-click, press Complete Quest. That is the loop, and it runs inside the
viewer rather than through the CLI.

**Right-clicking a talker greets it, and that is a fix as much as a feature.**
`is_attack_candidate` rules out only what is *never* a fight — yourself, a
corpse, a bench — and lets the server arbitrate the rest, which was the right
call when hostility could not be judged locally. But an innkeeper is not on that
list, so right-clicking one used to send a swing the server refused, in silence.
`UNIT_NPC_FLAGS` is replicated and non-zero means "this unit offers something",
so this is the one interaction test the client *can* make locally, and it now
runs before the attack branch.

#### The window shows the cache's text, not the questgiver's

`SMSG_QUESTGIVER_QUEST_DETAILS` (`0x0188`) and `SMSG_QUESTGIVER_OFFER_REWARD`
(`0x018D`) carry the quest's title, text, objectives and rewards — the same
content `SMSG_QUEST_QUERY_RESPONSE` carries, in a different layout. Parsing them
a second time would give this client two independently-derived copies of the
same strings, and two copies drift. So the details packet is read for **twenty
bytes** — the NPC guid and the quest id — and the rest is taken and discarded
deliberately rather than left unread, and the reward packet is not parsed at all:
its *arrival* is the server saying the hand-in is legal, which is the one thing
the query cannot say.

The offset was confirmed on two captures that disagree in both fields: quest 333
from Harlan Bagley and quest 783 from Deputy Willem, id at byte 16 in each.

**`SMSG_QUESTGIVER_QUEST_LIST` (`0x0185`) is still unparsed, and deliberately.**
Nothing has captured one. It is what a questgiver with no gossip menu and two or
more available quests sends; the two candidates tried both declined to offer
anything to a level-one human (a skill requirement in one case, a consumed chain
in the other). Every NPC in the fixture has a gossip menu, and
`SMSG_GOSSIP_MESSAGE`'s quest block — parsed since 4.15 — is what actually
arrives. A layout nobody has captured does not get written down.

#### What the window refuses to do

**No Accept button over a body nobody has read.** A quest whose description has
not arrived draws `Asking the server what this quest is...` and no button, which
is a fourth state beside Accept, Complete and Unfinished. The alternative is
asking the player to agree to something blank.

**No invented reward names.** Item names need `CMSG_ITEM_QUERY_SINGLE`, which
this client does not send yet, so a reward line reads `item 2224 x1`. An id is
checkable; a plausible-looking name would not be, and this project treats a
number nobody can check as worse than a blank.

**No reward choice.** `choose_quest_reward` goes out with index `0`, which is
correct only where there is nothing to choose. A quest offering alternatives
would hand over the first — a wrong answer rather than a missing feature — and
that is recorded here rather than hidden.

**The button's meaning is recomputed, not remembered.** Which of Accept and
Complete a press means comes from the same function that drew the label, on the
frame the click lands. A flag stored when the window opened would go stale
exactly when it matters: between the frame that drew `Accept` and the click that
pressed it, the quest may already be in the log.

### 4.17: the map, and the objectives on it

`M` opens the page for the zone the character is standing in, with the
character's own arrow on it and a marker for every objective the realm says
belongs to that page. It is the rung the tracker needs: until now this client
could say *what* to do and had nowhere to say *where*.

#### The projection, and why choosing it by eye was not allowed

`WorldMapArea.dbc` states, per page, the rectangle of world it draws. Turning a
position into a place on that picture has **four plausible readings** — the
horizontal axis can run either way and so can the vertical — and every one of
them produces a map with things on it. This project has already paid full price
for settling a question of that shape by looking: the ADT placement rotation
shipped at `-90`, was "fixed" to `+90` because a render of the abbey looked
better, and both were ninety degrees wrong, because a building has four sides
and every rotation shows a door to somebody.

So the reading was fitted against something that could refute it.
`WorldMapOverlay` states, in page pixels, where a named area's art sits; the
terrain files state, in world coordinates, which area every chunk of ground
belongs to. Those come from different files authored by different tools, so
agreement between them is evidence rather than a restatement.
`wow-cli map calibrate` regresses one against the other and presupposes neither
answer: a reversed axis fits a **negative** slope, and the page's pixel size is
whatever the slope's magnitude comes out as rather than a number decided in
advance.

```
Azeroth: 687 tiles, 173413 chunks with an area id
296 overlays scored against terrain centroids

reading          horizontal: slope/offset/r2     vertical: slope/offset/r2
as written            984.2      9.2  0.9864        633.3     18.3  0.9717
x flipped            -984.2    993.4  0.9864        633.3     18.3  0.9717
y flipped             984.2      9.2  0.9864       -633.3    651.6  0.9717
```

A slope of +984 against a page 1002 pixels wide, and +633 against one 668
high, at r² 0.986 and 0.972 over 296 overlays. The flips fit the same data with
the sign reversed, which is exactly what a wrong reading looks like and exactly
what this experiment was built to be able to say.

Two hand checks with margins no measurement error covers came first, and are
kept as unit tests: the `Stormwind` page's own world box, projected onto the
`Elwynn` page, lands where Elwynn's `STORMWIND` overlay art sits, and every
other orientation puts it more than five hundred pixels away on a canvas a
thousand wide; and the Northshire spawn `(-8950, -132)` — the position this
project has logged in at more than any other — projects inside Elwynn's
`NORTHSHIREVALLEY` box.

#### The page is bigger than the picture, and the art said so itself

Twelve 256x256 tiles in four columns and three rows make a 1024x768 image, but
the map stops short of its right and bottom edges. **The tiles announced it**:
on every page, the right column and the bottom row carry an alpha channel and
the rest do not — padding is transparent, content is not. Measuring the
furthest opaque pixel across all 91 pages whose art loads gives **1002x668,
unanimously**.

That 2% horizontally and 15% vertically is the kind of error that never fails.
A client that drew the twelve tiles to fit its frame would look entirely
correct and put every marker in the wrong place, worst at the bottom of the
page. So the grid is drawn *larger* than the frame and clipped to it.

#### The markers are the server's, and they name their own page

`CMSG_QUEST_POI_QUERY` answers with a map id, a `WorldMapArea` id and a polygon
per objective — the data Questie exists to ship, on the wire, always matching
the realm being played on. Two decisions about it are worth recording.

**A marker names the page it belongs to, so nothing guesses.**
`quest_poi.WorldMapAreaId` is a row in the same table the pages are keyed by,
which makes "does this marker go on the page being drawn" an equality rather
than a containment test. Testing containment instead would put a Westfall
objective on the Elwynn page wherever the two rectangles overlap, and it would
look entirely reasonable. `Testwolf`'s log is the case that shows it: quests 16,
85 and 783 mark Elwynn and quest 106 marks Westfall, and standing in Northshire
the Westfall marker is correctly absent.

**A third of the markers are regions, so a region is drawn as a region.** Of
the 18,768 markers in this realm's `quest_poi_points`, 12,794 carry a single
point and 5,974 carry between three and dozens — a valley to search rather than
a door to walk to. Those are drawn as the ring the server sent, with the pin at
its middle only as somewhere to hang the label. Collapsing a polygon to its
centroid would be claiming a precision the server never offered.

Live, against the local realm, `Testwolf`'s four quests answered with 18
markers and 126 points, matching `quest_poi_points` exactly, and **all 126
project inside their own page's rectangle**. That is a check on the page
assignment and the scale rather than on the orientation — a flipped axis lands
inside the rectangle too, which is why the calibration above is the experiment
that settles the flips and this one is not.

#### What the window will not do

**No invented names.** A marker's label is the quest's title where the cache
has one and `quest 106` where it does not — the same rule that leaves a reward
reading `item 2224 x1`.

**No pin without an answer.** A quest with no POI reply draws nothing rather
than a marker at the questgiver, because "the server did not say" and "it is
here" are different statements.

**Nothing is cached to disk.** Unlike `SMSG_QUEST_QUERY_RESPONSE`, a POI answer
is given only for quests in the player's own log, and a quest that is not in
the log gets the same empty list as a quest with no markers. Writing that down
would turn "you did not have it then" into "it has no markers", permanently. So
the store is memory-only and forgets a quest the moment it leaves the log.

**The map covers windows, never the frames you did not open.** It is the
largest frame in the interface and the only one anchored dead centre, so it is
excluded from the layout test that forbids overlapping defaults — and given a
narrower test of its own instead: it may sit over the loot window or the
release prompt, and it may not touch the unit frames, the chat log, the cast
bar or the action bars. An exemption with nothing asserted in its place is how
a layout rule quietly stops meaning anything.

### 4.17 continued: the map fills in as it is explored

The first version drew the twelve base tiles and nothing else, and the report
back was that the abbey the character had walked through was not on the map.
That is the correct complaint about a wrong assumption: **the base tiles are
the *unexplored* picture.** A zone page on its own is a coastline and some
water. Every road, building, mine and name is a separate `WorldMapOverlay`
patch, blitted on at a stated pixel offset, and drawn only where the player has
been.

#### One patch per area, and the files say how they are cut

A patch is stored the way a page is: 256-pixel tiles, `<TEXTURE><n>.blp`,
across before down. The counts on disk are what confirm it rather than
anything transcribed — `Interface\WorldMap\Elwynn` holds exactly
`NorthshireValley1`, `ForestsEdge1..2` (the patch is 256x341, so one column and
two rows), `RidgepointTower1..2` (306x233, two columns and one row) and
`Stormwind1..4` (485x405). Swept over the whole game with
`wow-cli map overlays --verify`: **886 patches on 105 pages, 1,524 of 1,524
tile files resolved**, by path rather than through a listing, because an MPQ
finds files by hash and a listing has answered this question wrongly here
before.

**A tile is stored larger than the piece of map it carries.** `FORESTSEDGE` is
341 pixels tall and its second tile is a 256x**128** file holding the remaining
85 rows. So the drawn rectangle *and* the texture coordinates are cropped
together against the patch's stated size: crop one without the other and the
art stretches or squashes instead of stopping, which reads as "the map is
slightly off" rather than as anything checkable.

Asking for the file one past the computed count turns one up for **76 of the
886** patches, which looks like a miscount and is not: `ceil(w/256) *
ceil(h/256)` covers the stated rectangle exactly, so a further file has nowhere
to be placed. Exporting a pair settles what they are — `MarshlightLake1` is the
whole labelled patch and `MarshlightLake2` is nearly blank, an offcut of a
taller earlier version left in the archive when the table row shrank.

#### The explored bitfield, measured against two characters

Which patches to draw comes from `PLAYER_EXPLORED_ZONES`, 128 update fields
holding one bit per `AreaTable` row's area bit. **A bitmask is a poor search
target on its own** — a single set word is just a power of two, and a player
object is full of flags — so it was measured against two characters whose set
bits sit in *different words*:

| character | explored | area bit | word | field |
|---|---|---|---|---|
| `Watcher` | Northshire Valley | 125 | 3 | `0x0414` |
| `Huntertest` | one Dun Morogh area | 212 | 6 | `0x0417` |

Two fields three apart for two bits three words apart, giving the same base
`0x0411` from either character. The server's own `characters.exploredZones` —
128 words per character, a source no client is ever sent — agrees with both.

An absent field is a **zero**, not an unknown, which is the same rule
`PLAYER_BYTES` established: an update block carries only non-zero fields, so a
character who has explored nothing in a word simply has no field there.
Reading that as "not known" would draw the whole map explored for exactly the
characters who have explored least.

So `Testwolf`, having walked Northshire Valley, the Abbey and Echo Ridge Mine,
gets Elwynn's `NORTHSHIREVALLEY` patch and eleven others hidden — and a page
with nothing revealed says `nothing here explored yet`, which is a third state
beside "no art for this page" and an ordinary map.

#### What is still missing

No zoom or panning, and no way to open a page other than the one you are
standing on. No continent view, no minimap (4.18), no `!` or `?` over
questgivers, and nothing on the map for anything but quest objectives — no
vendors, no flight points, no trainers. Three of the 108 pages state no
rectangle at all (`Dalaran`, `TheNexus`, `UtgardeKeep`) and a character
standing in one gets a window that says so rather than a blank picture.

### 4.17 continued: what the log says you have done

The map showed where objectives are before the log could say how many of them
were done. Four things came out of one live test, and three of them were bugs
in things that already worked.

#### The objective counters, and the sample that could tell three readings apart

A quest-log entry is five update fields: the id, the state, **two fields of
counters**, and a timer. The counters are the part that had never been read,
and the three plausible readings of those bytes -- four counters of eight bits
in one field, two of sixteen across two fields, or one `u32` each -- all
display a small number for the first objective, which is the only objective
most quests have.

So the sample had to be a quest with **four** objectives, all counted at once.
Quest 837 `Encroachment` wants four kills of each of four creatures; taken and
completed on the live realm, its entry reads:

```
   quest          +1         +2         +3         +4
     837           1     262148     262148          0
```

`262148` is `0x0004_0004`. Two fields each holding two fours is the sixteen-bit
reading and nothing else: eight-bit quarters would have put `0x04040404` in
`+2` and left `+3` zero, and a `u32` per counter needs three fields for four
counters and leaves nowhere for the timer in `+4`. Quest 7, with one objective,
agrees independently -- four kobolds killed read `4` in the low half of `+2`
while the server's own `character_queststatus.mobcount1` said `4`.

**Two objectives of the same quest are counted from two different places.** A
kill or a use is counted by the server in those fields. An item objective is
not there at all -- `.additem` moves nothing in them, because the original
client counts the items in the bags itself and the server only checks at
hand-in. So this client counts kills from the wire and items from its own
replicated inventory, which is the same split the original makes.

**The objective's wire slot indexes the counter, never its position in the
parsed list.** `SMSG_QUEST_QUERY_RESPONSE`'s objectives are pruned on the way
in -- a quest with one objective yields one, not four with three blanks -- so a
quest whose only objective sits in slot 2 would otherwise read counter 0, which
is a permanent zero or somebody else's progress. The slot travels with the
objective now.

#### Action bars belong to a character

They were kept once, for everybody. A rogue logging in held a warrior's bar:
every icon drew, every key pressed, and every cast was refused by the server --
which reads as "the bars do not work" rather than as "those are somebody else's
spells".

`ui.toml` now keeps a set per character. The migration rule is the interesting
part: a leftover bar is **adopted** by the first character who can cast all of
it, and thrown away for one who cannot. On the only evidence available -- a
spellbook -- a bar whose every spell is in this character's book is this
character's, and a warrior's `Heroic Strike` never passes that test for a
rogue. One arrangement survives the change; nobody inherits spells they do not
know.

#### The pointer is held for the duration of a drag

Turning the camera ran out of desk: the pointer reached the edge of the window
and the camera stopped, or left the window and the next click landed in another
application. It is now hidden, confined, and warped back to where the gesture
started after every movement, so a turn is unlimited.

That broke the click test and had to be replaced rather than patched. A click
was a press and release *in the same place*, which is exactly what a pinned
pointer produces for every drag. It is now distance **travelled** since the
press, which is also the more honest question: a gesture that swings the camera
around and comes back is a drag by any reading.

### 4.17 continued: the marks over questgivers, and a window nobody could press

#### A window sealed under another one

The Accept button stopped working, and it was not the button. Every frame is an
egui area of the same order, so the one built last is on top -- and the map is
760 by 520 in the middle of the screen, exactly where a questgiver's scroll
grows down into. With both open the button was drawn, hit-tested, and
unreachable, which reads as a window that has stopped working rather than as
one that is behind something.

Frames now draw in an explicit order, ranked by **how much answering them
matters**: the map at the bottom, the panels you open and close above it, then
the frames that are simply always there -- health, target, chat, the bars,
which no window may ever eat a click meant for -- and on top the windows that
appeared because something happened and want an answer: a corpse, a
questgiver, a death. The live bug is now a headless test that asserts the two
windows really do overlap before it asserts the click gets through, because a
test whose premise has quietly stopped holding passes for the wrong reason.

#### `!` and `?`, and an enum that was measured rather than remembered

`CMSG_QUESTGIVER_STATUS_QUERY` (`0x0182`) asks what mark belongs over one NPC's
head and `SMSG_QUESTGIVER_STATUS` (`0x0183`) answers in nine bytes: the guid,
and one status byte. **The guid coming back is the confirmation** -- nothing
acknowledges an outgoing opcode, and twenty-nine requests in one run each came
back naming the NPC they were about.

The byte is an enum with more values than this client has ever seen, and
writing the rest down from memory is the urge that produced `CHAT_MSG_SAY =
0x00`. So a character was created from nothing and the same four questgivers
were asked about after each change to its state:

| byte | the state that produced it | mark |
|---|---|---|
| 0 | an innkeeper with no quests; a questgiver whose quests are gated behind one not yet done | none |
| 2 | the same available quest, asked after `.levelup 25` | grey `!` |
| 5 | quest 7 in the log with none of its eight kobolds killed, asked at its ender | grey `?` |
| 8 | quest 783 on offer to a fresh level-one human | yellow `!` |
| 10 | quest 783 in the log and finished, asked at its ender | yellow `?` |

Anything else parses to `Unknown(n)`, draws nothing, and says so in the log
once -- a mark invented for a value nobody has produced would send a player
somewhere for no reason.

**Why ask at all, rather than work it out from the quest tables?** Because
whether a quest can be taken depends on level, race, class, reputation, every
prerequisite in its chain and whatever the realm has been scripted to check. A
client deciding that for itself would be reimplementing the server's
eligibility rules, and would be wrong on exactly the realms this client is
developed against.

**Every mark is thrown away when the quest log changes.** Taking a quest turns
its giver's exclamation into nothing and its ender's nothing into a question
mark, and the server volunteers neither. Asking once and keeping the answer
would leave an exclamation over an NPC with nothing left to give, which is
worse than no mark at all.

`SMSG_QUESTGIVER_STATUS_MULTIPLE` is **not** used: the request went out in the
same runs and nothing ever came back, so the one-guid form is what this client
has evidence for. Per-guid asking is capped at six a frame with a retry window,
the same shape every other query here uses.

### 4.18 Liquids: water, lava and slime

There was no liquid in this client at all. Elwynn's rivers, Northshire's pond
and the whole coastline were absent, the terrain simply ended, and a character
walked along the riverbed. This milestone reads the format, draws it, and swims
in it.

#### `MH2O`, and three sizes the public documentation gets wrong

One `MH2O` chunk serves a whole tile: 256 fixed headers naming where each map
chunk's liquid *layers* live, then 24-byte instances, an exists bitmap and a
vertex grid. Every offset is measured from the start of the chunk's **payload**,
which is the sort of thing that still lands inside the file when read from the
wrong base.

Three sizes were measured against `Azeroth_32_48` rather than transcribed,
because the wiki's readings do not fit it:

| | wiki | measured |
|---|---|---|
| attribute block | 8 bytes | **16** |
| exists bitmap, 3x6 rectangle | 6 bytes, one per row | **3**, `ceil(w*h/8)` packed |
| exists bitmap, 5x8 rectangle | 8 bytes | **5** |

The two rectangles disagree in opposite directions, which is what makes that a
measurement rather than a coincidence -- a full 8x8 sheet is the one case where
both readings agree, and so is exactly the sample that cannot settle it. What
proved the whole layout is that the sizes account for every byte: chunk 13's
sheet ends precisely where chunk 14's attributes begin, and chunk 14's ends
precisely where chunk 15's do.

`MCLQ`, the pre-Wrath per-chunk form, is **not** a fallback this reader declines
to implement. The tile has 256 map chunks and not one of them carries it, while
its `MH2O` describes 42 chunks of water.

#### The axis reading was fitted, and the first attempt could not answer

An instance covers a sub-rectangle of its chunk and nothing in the file says
which axis `x_offset` indexes. Both readings parse every byte of every file, and
both draw an entirely convincing pond -- one of them a quarter of a chunk from
where the pond is. So `wow-cli adt liquid` measures the thing that can refute
it: **water lies in low ground.**

Over all 5,660,498 liquid cells in Azeroth the two readings come out **99.2%
against 98.4%** -- indistinguishable, and it would have been read as
confirmation. The population was the problem. 86,222 of the 92,219 sheets are
open ocean covering an entire chunk, and **transposing a full 8x8 rectangle
produces the identical footprint**, so those sheets agree with both readings
whatever the seabed does. Restricted to cells in a rectangle that transposing
actually *moves*, over ground that differs between the two sample points:

| reading | liquid at or above the ground, of 90,346 cells |
|---|---|
| as read | 66,299 (**73.4%**) |
| axes swapped | 33,223 (**36.8%**) |

A factor of two, where the whole-population figure was a rounding error. It is
not 100% because a shoreline genuinely has cells where the bank rises through
the water plane; what matters is the ratio.

#### `LiquidType.dbc`, and a row whose name lies

26 rows, 45 fields. Field 3 is the category, identified by the names its rows
carry *and* independently by the art each one reaches -- every row the column
calls magma resolves to a file under `lava` or `LavaGreen`, every row it calls
slime to one under `slime`. Two different columns agreeing is evidence; a column
agreeing with its own name is not.

**And the one row where the two disagree is why this is read as a column rather
than matched as a name.** Row 181 is called `Orange Slime`, draws `LavaOrange`,
and its category is **0 -- water**. A client deciding what burns you by looking
for "slime" in a name would set a player alight in a harmless pond, and would do
it silently.

#### The damage is the server's, and that is the whole design

Lava hurting you is not something a client can implement. `Map::GetLiquidData`
computes liquid state from the server's own copy of the same terrain, runs a
`FIRE_TIMER`, and applies `DAMAGE_LAVA` itself; health is a replicated field. A
client that also subtracted hit points would be inventing a number that
disagrees with the server's the moment either is looked at.

So `crates/world/src/environment.rs` reads four packets and writes none:
`SMSG_START_MIRROR_TIMER` (`0x01D9`), `PAUSE` (`0x01DA`), `STOP` (`0x01DB`) and
`SMSG_ENVIRONMENTAL_DAMAGE_LOG` (`0x01FC`). The timers are *state* on
`WorldState` -- stated once and then silent, like the weather -- and the damage
is an *event*, one line in the combat log.

#### Three faults, each hiding the next, and none found by a test

The renderer was verified headlessly, declared working, and was wrong twice.
Both were found by a person at the window, and each report split the problem
exactly in half.

**"It isn't glitched, it's just not there."** The liquid art is cached per type
and shared across tiles, and there were two caches: one on `World`, built by
`load_tile`, and one on the renderer for the offline scenes. `draw_streaming`
was handed the renderer's, which is empty in streaming mode, so every sheet
resolved to no art and hit a bare `continue`. 2,398 triangles of Northshire's
stream were parsed, meshed and uploaded every time that tile loaded, and none of
it was ever submitted. The fix is that `draw_streaming` no longer *takes* a
cache -- it reads `world.liquid_types()` -- because a parameter that can be
passed wrong is worse than no parameter.

**"I see where water should be but nothing blue."** `river\lake_a` and
`ocean\ocean_h` average RGB **3.6** and **4.1** of 255 across their whole
256x256: they store no colour at all, and their entire ripple pattern is in the
alpha channel. `lava` and `slime` are ordinary opaque colour textures.
`LiquidType.material_id` says which -- 1 is the alpha-keyed pair, 2 the colour
pair -- and the shader was running the colour rule over both, multiplying the
tint by about 0.014. A black river, perfectly placed, perfectly animated,
perfectly depth-faded and perfectly lit.

**And the first "fix confirmed" was not a confirmation.** The skip was checked by
the *absence* of a warning, which is equally what zero iterated sheets produces
-- the same ambiguous silence the parser rules warn about, walked into while
fixing an instance of it. Both counters now print every frame.

#### Swimming, and an ordering bug caught by reading

`MSG_MOVE_START_SWIM` (`0x00CA`) and `STOP_SWIM` (`0x00CB`) were two of the nine
opcodes `MOVE_RELAYED` recorded as unconfirmed, on the grounds that they "need
water" -- which this client did not have. The character floats with its head
clear, dives along the camera pitch, rises on the jump key, and carries
`movement_flags::SWIMMING`, which brings a **pitch float** with it in every
packet. Another player's swim state arrives free with their relayed movement.
The cycles are `AnimationData` 41, 42 and 45, read from the table.

The ground-planting assignment ran unconditionally **before** the buoyancy read
its own altitude, so every frame reset a swimmer to the riverbed and then rose
three per cent of the way. That draws as a character walking along the bottom
with the stroke cycle playing: the feature apparently half-working, rather than
an ordering mistake. Found by re-reading the code before the test rather than
after it, which is the one time in this milestone that was cheaper.

#### What this deliberately does not do

No underwater view: `LiquidType` carries `MaxDarkenDepth`, `FogDarkenIntensity`,
`AmbDarkenIntensity` and `DirDarkenIntensity` and none of them is read, so
submerging looks like air with a blue ceiling. No breath or fatigue bar drawn,
though both packets are parsed and stored. No WMO liquid, so fountains and
indoor pools are dry. No reflection, no refraction, no procedural water
(material 3).

**There is deliberately no lava bar, because there is no lava timer on the
wire.** Breath and fatigue call `SendMirrorTimer`; the fire timer counts down
server-side and sends only the damage. A client waiting for a lava bar before
believing itself correct would wait forever.

#### Confirmed live

Water and swimming in Elwynn; lava in Searing Gorge. The lava run wanted a
character that survives it, and the useful trick is `.cheat god` rather than
`.gm on`: the damage packet is written at `Player.cpp:804` and `Unit::DealDamage`
-- where `CHEAT_GOD` returns 0 -- is not reached until 806, so the log arrives
with its real amount while health never moves. `.gm on` instead satisfies
`IsImmuneToEnvironmentalDamage` and suppresses the packet entirely, which would
have read as the client failing to parse it. With the cheat off, the same tick
killed the character, which is the other half of the same claim.
