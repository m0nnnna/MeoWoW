# open-wow-client

Open-source reimplementation of the WoW 3.3.5a (build 12340) client in Rust.
Client only — no server, no bundled assets.

## Orientation

- `crates/` — libraries, one per concern (`mpq`, `dbc`, `blp`, `render`, …)
- `tools/wow-cli` — inspection CLI; every format gets a dump command here
  *before* it is wired into the renderer
- `apps/viewer` — windowed asset viewer; `--screenshot` renders one frame
  headless to a PNG, which is how render output is verified without a display
- `docs/RENDERING.md` — GPU layering, the compressed-upload path, and the
  dependency pinning that the graphics stack requires
- `docs/ROADMAP.md` — milestone ladder and why it is ordered that way
- `docs/REUSE-POLICY.md` — what we implement vs. depend on; read before adding
  any dependency
- `docs/formats/` — implementation notes per format, recording the parts that
  bite

## Local setup

- Source lives on an SMB share (`N:`), which cannot execute binaries. The
  gitignored `.cargo/config.toml` redirects `target-dir` to local disk; without
  it every build fails with `Access is denied (os error 5)`.
- Reference installation: `D:\Games\World of Warcraft 3.3.5a` (verified
  12340, enUS, 17 archives, 203,949 unique paths). Also on disk for
  format-evolution comparison: 1.12.1 and 2.4.3 clients.
- `WOW_DATA` supplies `--data` to `wow-cli`.

## Rules that matter

1. **Never commit game assets** — not as fixtures, not as test data. Tests
   needing real data read `WOW_DATA` and skip when unset.
2. **No GPL code in the tree.** TrinityCore/MaNGOS may be read to understand a
   field's meaning; implementations are written from public documentation.
3. **WoW-specific formats are implemented in-tree.** Generic plumbing (codecs,
   GPU, windowing, math, crypto primitives) comes from crates.io. The test:
   would this dependency exist if WoW had never been written?
4. **`wow-cli verify` is the regression net** for the data layer — it reads all
   ~204k files, and systematic parser errors surface as one large bucket in the
   failure summary.

## Traps already hit

- **Never rewrite a file with a script that can throw mid-write.** A Python
  `write_text` containing a character the console codec could not encode
  truncated `docs/ROADMAP.md` to zero bytes. Prefer the editing tools; if a
  script must write, write UTF-8 explicitly and to a temporary file first.
- `wgpu`/`egui`/`egui-wgpu`/`egui-winit` versions are coupled, and the
  `windows` crate needs a pin to build the DX12 backend at all — see
  `docs/RENDERING.md` before touching any of them.
- Windows refuses to execute a test binary whose filename looks like an
  installer, so integration tests are `tests/real_data.rs`, never
  `real_install.rs`.

## Conventions

- Errors are typed per crate with `thiserror`; `anyhow` only in `tools/`.
- Comments explain *why*, especially where the format is counterintuitive —
  those are the notes that stop a bug being reintroduced.
- Byte-level parsing gets a unit test with a known-good constant wherever one
  exists (e.g. the MPQ crypt table keys).
