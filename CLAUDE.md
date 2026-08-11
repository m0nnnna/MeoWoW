# open-wow-client

Open-source reimplementation of the WoW 3.3.5a (build 12340) client in Rust.
Client only — no server, no bundled assets.

## Where the project is

Phases 1 and 2 are complete: every data format reads, and the world renders and
streams. Phase 3 has started.

| | State |
|---|---|
| Data formats | MPQ, DBC, BLP, M2 (+animation), WMO, ADT/WDT — all done |
| Renderer | Textures, skinned models, buildings, blended terrain, streaming — done |
| Protocol | **3.1 logon, 3.2 world handshake, 3.3 enter world done**, all confirmed against a live realm; 3.4 movement is next |
| Game + UI | Not started. The largest remaining chunk. |

Roughly 45% of the way to something a person could test by playing. See
`docs/ROADMAP.md` for the milestone ladder and what is deliberately deferred.

The two halves still have not met in code. The client can log in, enter the
world and parse everything around it — position, creatures, game objects — and
it can render terrain and models at those same coordinates, but nothing yet
feeds one into the other. Joining them is the obvious next visible win and does
not depend on 3.4.

## Orientation

- `crates/` — one library per concern: `chunk` (shared chunked container),
  `mpq`, `dbc`, `blp`, `m2`, `wmo`, `adt`, `render`, `auth`, `world`
- `tools/wow-cli` — inspection CLI. **Every format gets a dump command here
  before it is wired into the renderer**, and a `survey` command that parses the
  whole archive set. Those surveys have caught every systematic parser bug so
  far.
- `apps/viewer` — windowed viewer. `--screenshot` renders one frame headless to
  a PNG, which is how render output is checked without a display.
- `docs/` — `ROADMAP.md`, `RENDERING.md`, `PROTOCOL.md`, `REUSE-POLICY.md`, and
  `formats/*.md` recording what each format actually does and where it bit us.

## Local setup

- Source lives on an SMB share (`N:`), which cannot execute binaries. The
  gitignored `.cargo/config.toml` redirects `target-dir` to local disk; without
  it every build fails with `Access is denied (os error 5)`.
- Reference installation: `D:\Games\World of Warcraft 3.3.5a` (verified 12340,
  enUS, 17 archives, 203,949 paths). 1.12.1 and 2.4.3 are also on disk for
  format-evolution comparison.
- `WOW_DATA` supplies `--data` to `wow-cli` and gates the integration tests.
- Test realm: **`wow1.nekos.farm`** (auth 3724, world 8085), realm `NekoCore`
  at `108.174.48.199:8085`, realm id 1. Accounts `TESTER` and `ACCOUNT33`
  exist. **Passwords are deliberately not recorded here** — this file is
  committed. Ask the user, and pass the password via `WOW_PASSWORD` rather than
  an argument. A wrong password and a missing account are hard to tell apart,
  so guessing wastes real time.
- `ACCOUNT33` has two characters, `Testwolf` (human warrior) and `Testdruid`
  (night elf druid), created to give `SMSG_CHAR_ENUM` real data to parse. An
  account with no characters exercises none of that packet's field offsets.

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
- **Not every failure is a bug.** The world connection dropping after three
  keepalives was the server enforcing a *minimum* ping interval — pinging too
  eagerly is punished harder than not pinging. It surfaced as an unexpected end
  of stream, which is indistinguishable from a desynchronised cipher. Before
  suspecting corruption, ask whether a rate limit or anti-abuse rule was tripped.
- **Compare against something derived independently.** The SRP6 tests carry a
  server written from the protocol, not from the client. Agreement between two
  separate derivations is evidence; a thing checked against itself is not.
- **When geometry is missing rather than wrong, suspect culling before data.**
  WMO winds counter-clockwise, M2 and terrain clockwise. Guessing from a
  neighbouring format culled a roof and looked like a hole in the mesh.
- **An odd-looking render is often the camera.** A gnoll looked scrambled and a
  building looked misplaced; both were framing, not geometry. Render canonical
  angles before doubting the parser.

## Traps already hit

- **Never rewrite a file with a script that can throw mid-write.** A Python
  `write_text` containing a character the console codec could not encode
  truncated `docs/ROADMAP.md` to zero bytes. Prefer the editing tools; if a
  script must write, write UTF-8 explicitly.
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
