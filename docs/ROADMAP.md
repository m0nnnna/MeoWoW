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

This milestone also changed what "careful" means. Every earlier parser was
memoryless, so a mistake produced one wrong answer and vanished. Replicated
state keeps mistakes: a dropped update is permanent and a bad merge erases
fields nothing will resend. The defence that worked was **accounting** rather
than parsing — count every change, tally updates that name unknown objects
instead of inventing them, and check that `created - removed` equals the number
of objects held. Those counters would have caught every replication bug this
project could plausibly have written, and none of them are assertions about
packet layout.

## Phase 4 — Game

Chat, inventory, spellcasting, combat, loot, quests — sequenced by whatever is
most visibly missing at the time.

The open question is the UI. WoW's interface is Lua 5.1 driving an XML-defined
frame tree (`FrameXML`), and addon compatibility means reimplementing that
whole widget system faithfully. The alternative is a native UI that abandons
addons. This decision does not need making until Phase 4, and the world is a
better place to spend effort first.

## Non-goals

- Server implementation. TrinityCore and MaNGOS exist and are excellent.
- Any expansion other than 3.3.5a until 3.3.5a is genuinely playable.
- Distributing game content, in any form.
