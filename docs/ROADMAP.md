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
