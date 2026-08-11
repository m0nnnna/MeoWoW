# DBC

Implementation notes for `crates/dbc`. Records what the format actually does
and where it bit us, rather than restating
[wowdev.wiki/DBC](https://wowdev.wiki/DBC).

## Shape

A 20-byte header (`WDBC`, record count, field count, record size, string block
size), a block of fixed-size records, then a block of NUL-terminated strings.
String-typed columns store a byte offset into that block. Offset 0 is the empty
string — the block opens with a NUL specifically so an unset string is
representable.

A stock 12340 install has 246 tables, and all 245 that actually exist parse.
The 246th, `CharVariations.dbc`, is present in `locale-enUS.MPQ` but tombstoned
by `patch-enUS.MPQ`, so it correctly resolves to nothing.

## Columns are usually 4 bytes. Usually.

`field_count` and `record_size` are independent header values, and the obvious
invariant `record_size == field_count * 4` does **not** hold everywhere. Five
tables in a stock install byte-pack:

| Table | Fields | Record size |
|-------|--------|-------------|
| `CharBaseInfo.dbc` | 2 | 2 |
| `PowerDisplay.dbc` | 6 | 15 |
| `SpellItemEnchantmentCondition.dbc` | 31 | 64 |
| `SpellChainEffects.dbc` | 48 | 177 |
| `CharStartOutfit.dbc` | 77 | 296 |

`SpellChainEffects` is not even 4-aligned. Treating the invariant as a
validation rule rejects real files; `record_size` is authoritative for
striding, and `Dbc::is_uniform` reports the discrepancy so the typed layer can
refuse tables its word accessors cannot address.

## Trailing slack is normal

A file may be **longer** than its header accounts for. This install's
`Spell.dbc` — supplied by a third-party `patch-V.MPQ`, not by Blizzard — has 21
bytes past the declared string block, holding a mojibake'd fragment of Chinese
text that some patch tool failed to account for.

Nothing indexes past the declared block, so slack is harmless and tolerated;
the parser truncates the string block to the declared size so the extra bytes
can never become addressable. A file *shorter* than declared is a different
matter and is rejected.

## The file does not describe its own types

This is the central difficulty. A `u32`, an `i32`, an `f32`, and a string
offset are four bytes each and indistinguishable on disk. Reading a table means
knowing its layout in advance, per build.

Localized strings occupy **17 columns**: 16 locale slots followed by a bitmask.
Only the locale the client was downloaded for is populated; the other 15 point
at the empty string. This is why `Map.dbc` has 66 fields for what is
conceptually about 15 columns.

Schemas are declared with the `dbc_table!` macro and may be sparse — `Spell`
has 234 columns and we name eleven. Each schema asserts its field count, which
is the only cheap defence against applying a 3.3.5a layout to another build:
the wrong layout parses perfectly and returns plausible nonsense.

## Type inference

`wow-cli dbc info <table>` guesses column types by testing every value against
what each type would have to look like. It is the tool for transcribing a table
that has no schema yet.

- **String**: every non-zero value must land immediately after a NUL — i.e. at
  a string boundary, not inside one — and produce printable text, with at least
  two *distinct* offsets. The boundary test is what rejects small integers: in
  `Map.dbc` the block starts `\0Azeroth\0`, so a column containing `2` would be
  pointing at `"zeroth"` and fails.
- **Float**: every non-zero value must decode to a finite magnitude in
  `1e-6..1e12`. Integers cannot fake this — as a float, `42` is a denormal near
  `6e-44` and `0xFFFFFFFF` is `NaN`.
- **Bool**: tested *before* String, and this ordering matters. Offset 1 is a
  legitimate pointer to the first string, so a `{0,1}` flag column looks
  exactly like a string column naming that one string. Ranking String first
  misread `Map.pvp` as text, which shifted the localized-block detector one
  column left and swallowed `MapName_lang` — a single bad guess corrupting
  every column after it.

Inference is a starting point for a schema, not a substitute for one. It cannot
distinguish a foreign key from any other integer, and it guesses on sparse
columns.

## Verification

`wow-cli dbc check` validates every transcribed schema against the install;
`dbc list` shows the shape of all 246 tables and flags the byte-packed ones.
Field counts catch a wrong build, but only content assertions catch a wrong
*index* — the integration tests pin values like `Map(0).directory == "Azeroth"`
and `Spell(5).name == "Death Touch"` for that reason.
