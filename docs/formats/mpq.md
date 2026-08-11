# MPQ

Implementation notes for `crates/mpq`. These record what the format actually
does and where it surprised us, rather than restating
[wowdev.wiki/MPQ](https://wowdev.wiki/MPQ).

## Shape

An archive is a header, a hash table, a block table, and a heap of file data.
Both tables are encrypted with keys derived from the fixed strings
`(hash table)` and `(block table)`.

The header is located by scanning 512-byte boundaries for `MPQ\x1a`, because an
archive can be appended to another file. **All offsets in the tables are
relative to wherever the header was found**, not to the start of the file.
Getting this wrong reads plausible-looking garbage from a stock install only
when the header is at offset 0 — which it is for every WoW archive, so the bug
hides until someone opens a self-extracting archive.

## Filenames do not exist

The archive stores no paths. A path becomes three independent 32-bit hashes:
one picks a slot in a power-of-two table, the other two verify it. Enumeration
is only possible because archives conventionally contain a `(listfile)` member
holding the real names — but it is *only* a convention. Files absent from the
listfile are still readable if you know the path, and WoW ships some.

Probing is open-addressed. An empty slot (`0xFFFFFFFF`) terminates a search;
a deleted slot (`0xFFFFFFFE`) must **not**, since a later insertion may have
probed past it.

## Storage modes

A file is stored one of three ways, selected by its block flags:

- **Uncompressed** — contiguous bytes.
- **Single unit** — one compressed blob, used for files smaller than a sector.
- **Sectored** — split into `512 << sector_shift` chunks (4 KiB in WoW), each
  compressed independently, preceded by a table of `count + 1` offsets.

The sector offset table gains one extra entry when `SECTOR_CRC` is set, and the
per-sector checksums land in a trailing pseudo-sector.

**A sector that did not compress is stored verbatim, with no compression mask
byte in front of it.** The only way to tell is to compare the stored length
against the expected decompressed length: equal means raw. Feeding such a
sector to the decompressor reads its first byte as a mask and produces
confident nonsense.

## Encryption

Files can be encrypted with a key derived from their *base name only* — the
directory is not part of the key. `FIX_KEY` additionally folds in the file's
offset and size, and the offset used is the one **relative to the archive
base**, which matters for the appended-archive case above.

The sector offset table of an encrypted file is keyed at `key - 1`, and sector
*i* at `key + i`.

The cipher works on 32-bit words, so a trailing partial word is left in the
clear. Every encrypted structure in the format is word-aligned to make this a
non-issue.

## Compression

The mask byte can name several algorithms; the packer applied them in a fixed
order, so unpacking reverses it. In a stock 3.3.5a install the only masks that
actually occur are zlib (`0x02`) and bzip2 (`0x10`).

Implemented: zlib, bzip2, sparse, stored.
Not implemented: PKWARE implode, Huffman, ADPCM — all pre-WotLK or audio-only,
and all reported as explicit errors rather than silently mis-decoded.

## Patch chains

A stock 3.3.5a install is 17 archives, and the same path frequently exists in
several. Resolution order matters: `DBFilesClient\Map.dbc` lives in
`common.MPQ` but the version the client actually uses comes from
`enUS/patch-enUS-3.MPQ`. Locale archives outrank base archives, and lettered
patches (`patch-U.MPQ`, the slot private servers use) outrank numbered ones.

Filesystem case is not consistent even within one install — a stock directory
holds both `patch-3.MPQ` and `Patch-U.mpq` — so archive files are resolved
case-insensitively.

## Verification

`wow-cli verify` reads and decompresses every listed file. On a stock install
that is ~204k files and ~16 GiB, and it is the cheapest way to catch a parser
regression: a systematic error shows up as one large bucket in the failure
summary rather than as scattered noise.
