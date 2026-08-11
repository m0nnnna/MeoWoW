# Reuse policy

**We implement every WoW-specific format ourselves. We use the ecosystem for
everything that is not WoW-specific.**

## Why the line sits there

Three reasons, in order of importance.

**Licensing.** The best existing references — TrinityCore, MaNGOS — are GPL.
Reading their source to learn a packet layout is fine; deriving our code from
it is not, and the distinction gets blurry fast when the source is open in the
next window. Implementing from format documentation keeps the provenance of
every line unambiguous and lets this project stay MIT/Apache-2.0.

**Debuggability.** When a model renders inside-out, the bug is in a parser we
wrote and can single-step. Wrapping someone else's decoder turns that into an
investigation of a foreign codebase with different conventions.

**The formats are the interesting part.** Delegating them would leave a project
that is mostly glue.

## In practice

| We write | We depend on |
|----------|--------------|
| MPQ archives, patch chains | `flate2`, `bzip2-rs` (generic codecs) |
| DBC tables | `winit` (windowing/input) |
| BLP textures | `wgpu` (GPU abstraction) |
| M2 models and animation | `glam` (vector math) |
| WMO objects | `egui` (debug UI) |
| ADT/WDT terrain | `tracing` (logging) |
| SRP6 authentication | `sha1`, `hmac`, `rc4`, `num-bigint` (crypto primitives) |
| The 3.3.5a opcode protocol | `clap`, `anyhow`, `thiserror` |

The test is simple: *would this dependency exist if WoW had never been
written?* If yes, use it. If no, write it.

Cryptographic primitives sit on the "depend" side deliberately. Hand-rolling
SHA-1 or RC4 is a way to introduce subtle bugs, not a way to learn something
about WoW. The *protocol* built on them — SRP6 parameter choices, the header
cipher's key derivation — is ours.

## Acceptable sources

- **[wowdev.wiki](https://wowdev.wiki/)** — community format documentation, the
  primary reference for everything in Phase 1.
- **Observation of our own client installation** — hexdumps, structural
  inference, checking a parse against 200k real files.
- **Protocol documentation and packet captures** against a server we control.
- **Reading** GPL projects to understand *what* a field means, while writing
  our own implementation of *how* to read it.

## Unacceptable

- Copying code from any GPL project into this tree.
- Committing game assets, including as test fixtures.
- Vendoring a decoder for a WoW format and calling it done.
