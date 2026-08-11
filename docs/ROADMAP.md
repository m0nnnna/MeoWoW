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
| 1.5 | **WMO objects** | Root + group files, portals, materials, doodad sets |
| 1.6 | **ADT terrain** | Height maps, alpha layers, texture assignment, doodad/WMO placement |

DBC comes before textures because nearly everything else is indexed by it — you
cannot place a model in the world without the tables that say which model goes
where.

## Phase 2 — Renderer

| # | Milestone | Ends with |
|---|-----------|-----------|
| 2.1 | **Window + wgpu device** ✅ | `apps/viewer`: textures on screen, egui overlay, headless `--screenshot` |
| 2.2 | **Static M2 rendering** ✅ | Textured creatures and doodads, orbit camera, depth-sorted batches |
| 2.3 | **M2 animation** | Bone transforms, keyframe interpolation, animation picker |
| 2.4 | **WMO rendering** | Walk inside a building with correct lighting groups |
| 2.5 | **Terrain rendering** | One ADT tile, correct texture blending |
| 2.6 | **World streaming** | Free-fly across Azeroth, tiles loading and evicting |

Milestone 2.6 is the first point where the project feels real. It is also the
natural place to stop and harden, because everything after it depends on the
world being trustworthy.

## Phase 3 — Protocol

Independent of Phases 1–2; can proceed in parallel once there is appetite.
Target is a stock TrinityCore or MaNGOS 3.3.5a server.

| # | Milestone | Ends with |
|---|-----------|-----------|
| 3.1 | **Auth server** | SRP6 login, realm list printed by `wow-cli` |
| 3.2 | **World handshake** | RC4 header crypt, `SMSG_AUTH_CHALLENGE` → character list |
| 3.3 | **Enter world** | Login to a character, receive the initial object update |
| 3.4 | **Movement** | Move, and be seen moving by another client |
| 3.5 | **Entity replication** | Other players and creatures visible and animating |

3.2 is the single hardest protocol step: the header cipher and the
object-update-field packing are both unforgiving, and a one-bit error produces
a desync with no useful error message. Budget accordingly.

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
