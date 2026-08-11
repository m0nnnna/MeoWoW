# Building

## Requirements

- Rust stable (1.85+). Install via [rustup](https://rustup.rs/).
- On Windows: Visual Studio Build Tools with the **Desktop development with
  C++** workload, which supplies the MSVC linker and Windows SDK that the
  `x86_64-pc-windows-msvc` target links against. A full Visual Studio
  installation also satisfies this.
- A World of Warcraft 3.3.5a installation for anything that touches real data.

```console
cargo build --release
cargo test
```

## Pointing at game data

Most commands need the `Data` directory of a 3.3.5a install:

```console
cargo run -p wow-cli -- --data "D:/Games/World of Warcraft 3.3.5a/Data" info
```

Set `WOW_DATA` once instead:

```powershell
[Environment]::SetEnvironmentVariable('WOW_DATA', 'D:\Games\World of Warcraft 3.3.5a\Data', 'User')
```

Verify the build first — `Wow.exe`'s file version must read `3, 3, 5, 12340`.
Other builds have different structure layouts and will mis-parse.

## Building from a network drive

Windows refuses to execute binaries from SMB shares, so if the source tree
lives on a mapped network drive, cargo's build scripts fail with
`Access is denied. (os error 5)` before compiling anything.

Redirect the build directory to local disk. Create `.cargo/config.toml` in the
repo root — it is gitignored, since the path is machine-specific:

```toml
[build]
target-dir = "C:/Users/you/.cargo-targets/open-wow-client"
```

This is also substantially faster than writing gigabytes of intermediate
artifacts over SMB.

## Windows will not run a test binary named like an installer

Windows applies an installer-detection heuristic to executable *filenames*: a
binary whose name contains `install`, `setup`, `update`, or `patch` triggers a
UAC elevation prompt. Cargo names a test binary after its source file, so
`tests/real_install.rs` produces `real_install-<hash>.exe` and the test run
dies with:

```
The requested operation requires elevation. (os error 740)
```

Name integration tests around the data they use, not the installation —
`tests/real_data.rs`. This is worth remembering given how much of this project
deals with patches and installs.
