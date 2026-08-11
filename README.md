# open-wow-client

An open-source reimplementation of the World of Warcraft 3.3.5a (build 12340)
client, written from scratch in Rust.

This is a *client only*. It ships no game content: you point it at a copy of
the 3.3.5a data files you already own, exactly as
[OpenMW](https://openmw.org/) does for Morrowind. It talks to existing
3.3.5a-compatible servers.

> **Status: very early.** The archive layer reads real game data. Nothing
> renders yet. See [docs/ROADMAP.md](docs/ROADMAP.md).

## What works today

- **MPQ archives** — format versions 0 and 1, zlib and bzip2 sectors,
  encrypted files, single-unit and sectored storage, sparse decoding.
- **Patch chains** — resolves a path across all 17 archives of a stock
  installation in the client's load order, including delete markers, so
  patches correctly shadow *and remove* base content.

Verified against a stock build 12340 install: 203,949 paths, of which 198,827
read and decompress cleanly (21.4 GiB) and 5,121 are correctly masked by patch
tombstones. The one remaining unresolved path is a stale entry in Blizzard's
own listfile.

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
```

## Getting started

You need a 3.3.5a installation (verify `Wow.exe` reports file version
`3, 3, 5, 12340`) and a recent stable Rust toolchain.

```console
cargo build --release
cargo run -p wow-cli -- --data "<path>/Data" info
```

Set `WOW_DATA` to avoid passing `--data` every time. If your source tree lives
on a network share, see [docs/BUILDING.md](docs/BUILDING.md).

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
