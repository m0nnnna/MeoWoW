# MeoWoW

*The open source WoW client for cats.*

An open-source reimplementation of the World of Warcraft 3.3.5a (build 12340)
client, written from scratch in Rust.

This is a *client only*. It ships no game content: you point it at a copy of
the 3.3.5a data files you already own, exactly as
[OpenMW](https://openmw.org/) does for Morrowind. It talks to existing
3.3.5a-compatible servers.

> **Status: it plays.** Every data format reads, the world renders and streams,
> and the protocol reaches a live realm: log in, walk around, see other
> players move, cast spells, swing a sword. A native interface -- unit frames,
> chat, a spellbook, action bars -- draws over it, with no `FrameXML` and no
> addons. Roughly 58% of the way to something a person could sit down and
> play. See [docs/ROADMAP.md](docs/ROADMAP.md) for the milestone ladder and
> what's still missing (inventory, quests, most spell effects).

## What works today

- **MPQ archives** — format versions 0 and 1, zlib and bzip2 sectors,
  encrypted files, single-unit and sectored storage, sparse decoding.
- **Patch chains** — resolves a path across all 17 archives of a stock
  installation in the client's load order, including delete markers, so
  patches correctly shadow *and remove* base content.

- **DBC tables** — the client's database files. 18 tables have typed schemas
  (`Map`, `Spell` and its duration/radius siblings, the creature and
  character-customisation tables, `Item`/`ItemDisplayInfo`, the lighting
  tables, and more), verified with `wow-cli dbc check`; column-type inference
  transcribes the rest of the 245 tables in the install on demand.
- **BLP textures** — DXT1/3/5, palettized at every alpha depth, and raw BGRA,
  with PNG export. Compressed blocks are also exposed unmodified, so the
  renderer can hand them to the GPU without a CPU decode.

- **M2 models** — header, vertex pool, bone hierarchy, materials, and the
  `.skin` files holding the actual triangles, plus the DBC walk that turns a
  creature display id into a model on disk.
- **Animation** — keyframe tracks, the external `.anim` files most sequences
  actually live in, alias resolution, and GPU skinning.
- **WMO objects** — buildings and dungeon interiors: root plus group files,
  materials, render batches, collision separation, and doodad sets.
- **ADT terrain** — WDT tile maps, height fields, texture layers with their
  alpha maps, and the doodad and world-object placements that fill the world.

- **Protocol** — SRP6 login against the auth server, the realm list, the world
  handshake and its RC4 header cipher, movement (walk, strafe, jump, fall),
  chat, spellcasting, melee combat, and a replicated `WorldState` that folds
  every packet into the world other players and creatures actually see:
  creatures slide along their real path and play the model's own walk/stand
  cycles. Confirmed against a live realm with two clients at once, including
  one watching the other move.
- **Interface** — a native, from-scratch UI with no addon support: player and
  target unit frames, click-to-target, a chat window, three action bars with
  real spell icons, hover tooltips, a cooldown sweep, and a spellbook you build
  the bars from. Every position, size and colour is a plain number in
  `ui.toml`, editable by hand or by dragging frames in-game. See
  [docs/UI.md](docs/UI.md).
- **Game** — auto-attack and swing-timer melee with a named combat log, spell
  casting off real `Spell.dbc` data (82% of its description templates
  resolve), and a player character drawn in third person with its own chosen
  face, skin, hair and gear -- weapon included, drawn on the correct attachment
  bone and sheathed to the position `Item.dbc` names for it.

Verified against a stock build 12340 install: 203,949 paths, of which 198,827
read and decompress cleanly (21.4 GiB) and 5,121 are correctly masked by patch
tombstones. The one remaining unresolved path is a stale entry in Blizzard's
own listfile. All 245 DBC tables present in the install parse, as do all
107,927 readable textures, all 22,779 models with their 24,626 skins
(9.9M vertices, 15.9M triangles), and all 1,985 world objects with their 9,346
groups (29.4M vertices, 31.6M triangles). All 5,744 terrain tiles across 106
maps parse with every chunk edge meeting its neighbour, carrying 1,023,338
doodad and 11,182 world-object placements.

```console
$ wow-cli --data "D:/Games/World of Warcraft 3.3.5a/Data" info
17 archives, in load order (last wins):
   1. common.MPQ           2723.8 MiB    83670 listed
   ...
total unique paths: 203949

$ wow-cli --data ... which 'DBFilesClient\Map.dbc'
DBFilesClient\Map.dbc
  -> .../enUS/patch-enUS-3.MPQ
     43226 bytes (7746 packed), compressed

$ wow-cli dbc rows Map --limit 1
MapRow { id: 0, directory: "Azeroth", instance_type: 0, name: "Eastern Kingdoms", ... }
```

## Viewer

`apps/viewer` puts assets on screen. It also renders headless, which is how the
GPU path is checked without a display:

```console
cargo run -p wow-viewer -- --creature 1216            # a gnoll, skinned and animated
cargo run -p wow-viewer -- --map Azeroth --tile 31,48 --stream --radius 2  # fly over Stormwind
cargo run -p wow-viewer -- --map Azeroth --tile 32,48 --world  # one tile, populated
cargo run -p wow-viewer -- --wmo 'World\wmo\Azeroth\Buildings\Human_Farm\Farm.wmo'
cargo run -p wow-viewer -- --model 'World\...\HumanGuardTower.m2'
cargo run -p wow-viewer -- --texture 'Interface\Icons\Spell_Fire_Fireball02.blp'
cargo run -p wow-viewer -- --screenshot frame.png --creature 1216 --yaw 0
```

Drag to orbit, scroll to zoom. DXT textures are handed to the GPU as `Bc1/2/3`
blocks with no CPU decode, and the overlay reports which path each texture took
and why. See [docs/RENDERING.md](docs/RENDERING.md).

## Playing on a realm

Run `wow-viewer` with no arguments -- double-clicking it counts -- and it opens
a sign-in screen: account, password, realm server, and a cat-headed button that
asks where your `Data` folder is. Sign in and it lists the characters on the
account for you to pick one. It remembers everything but the password, which it
never writes anywhere.

**It does not create characters.** Make those in the original client; this one
plays them.

The command line still says the whole thing, which is what every probe and
screenshot in `docs/` relies on:

```console
cargo run -p wow-viewer -- --realm-host <host> --user <account> --character <name>
```

Given all three it connects straight away and never shows the screen. Given
some of them it shows the screen with those parts filled in.

`W`/`S` walk, `A`/`D` turn, `Q`/`E` strafe, `Space` jumps, right-drag steers
while left-drag swings the camera, left-click selects and right-click
attacks. `Enter` opens a chat line, `P` opens the spellbook, `F1` unlocks the
interface for dragging. `wow-cli world` and `wow-cli auth` do the login half
headlessly, for scripting or for checking a capture without a window. See
[docs/PROTOCOL.md](docs/PROTOCOL.md).

The interface comes in four palettes -- `slate`, `neko`, `void` and `calico` --
picked from the sign-in screen's settings. Choosing one **writes the colours
into `ui.toml`**, so a theme is a starting point you then edit rather than a
layer hiding under the file. See [docs/UI.md](docs/UI.md).

## Getting started

You need a 3.3.5a installation (verify `Wow.exe` reports file version
`3, 3, 5, 12340`) and a recent stable Rust toolchain.

```console
cargo build --release
cargo run -p wow-cli -- --data "<path>/Data" info
```

Set `WOW_DATA` to avoid passing `--data` every time. If your source tree lives
on a network share, see [docs/BUILDING.md](docs/BUILDING.md).

## Releases

Prebuilt Windows binaries (`wow-viewer.exe`, `wow-cli.exe`) are attached to
each [GitHub Release](https://github.com/m0nnnna/MeoWoW/releases) as a zip --
no build toolchain required, and, per the design commitments above, no game
assets inside it either. Unzip it, point `wow-viewer.exe` at your own `Data`
folder from its sign-in screen (or pass `--data`), and go.

**Cutting a new release** (maintainers): push a tag matching `v*.*.*`, e.g.

```console
git tag v0.1.0
git push origin v0.1.0
```

`.github/workflows/release.yml` picks that up, runs the same zero-warning
build and full test suite this project holds itself to, packages both
binaries plus the README into a zip, and publishes it as a GitHub Release with
auto-generated notes. A tag is the only trigger that publishes; the workflow
can also be run by hand from the Actions tab (`workflow_dispatch`) to sanity
check the build without cutting a real release.

## Design commitments

These are the constraints the project holds itself to; they are the reason it
is structured the way it is.

1. **No game assets, ever.** Not in the repo, not in releases, not in test
   fixtures. Tests that need real data are gated behind `WOW_DATA` and skip
   when it is unset.
2. **Formats are implemented in-tree from public documentation.** Every WoW
   format parser is ours. Generic plumbing (GPU, windowing, zlib) comes from
   the ecosystem. See [docs/REUSE-POLICY.md](docs/REUSE-POLICY.md).
3. **No GPL code in the tree.** The reference server implementations are GPL;
   we read the public protocol documentation instead, so this project can stay
   MIT/Apache-2.0.
4. **Every layer is inspectable from the CLI** before it is wired into the
   engine. A format is not done until `wow-cli` can dump it.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.

World of Warcraft is a trademark of Blizzard Entertainment, Inc. This project
is not affiliated with or endorsed by Blizzard Entertainment.
