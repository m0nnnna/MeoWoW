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
| 3.4 | **Movement** | Move, and be seen moving by another client |
| 3.5 | **Entity replication** | Other players and creatures visible and animating |

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
