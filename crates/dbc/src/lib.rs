//! Reader for WDBC client database tables.
//!
//! Written from the public format documentation at <https://wowdev.wiki/DBC>.
//!
//! A DBC is a fixed-width table: a 20-byte header, a block of equal-sized
//! records, and a block of NUL-terminated strings that string-typed columns
//! index into.
//!
//! **The file does not describe its own column types.** A `u32`, an `i32`, a
//! `f32`, and an offset into the string block are indistinguishable on disk, so
//! reading a table means knowing its layout in advance -- see [`schema`] for
//! the ones we have transcribed, and [`infer`] for working out an unknown one.
//!
//! Two things the format documentation understates, both of which occur in a
//! stock 3.3.5a install:
//!
//! - **Columns are usually four bytes, but not always.** `record_size` and
//!   `field_count` are independent header values, and a few tables byte-pack:
//!   `SpellItemEnchantmentCondition.dbc` declares 31 fields in 64 bytes, and
//!   `SpellChainEffects.dbc` uses a 177-byte record that is not even 4-aligned.
//!   `record_size` is authoritative for striding; see [`Dbc::is_uniform`].
//! - **A file may be longer than its header accounts for.** Trailing slack is
//!   harmless because nothing indexes into it, so it is tolerated; a file
//!   *shorter* than declared is truncated and rejected.

pub mod infer;
pub mod light;
pub mod schema;
pub mod spelltext;

use std::fmt;

/// Bytes in the header preceding the first record.
const HEADER_SIZE: usize = 20;
/// Every column is one 32-bit word.
pub const FIELD_SIZE: usize = 4;

/// Locale slots in a localized string column, in their on-disk order.
///
/// A localized column is 16 string offsets followed by a bitmask, so it
/// occupies 17 columns. Only the slots a given client build actually ships are
/// populated; the rest point at the empty string.
pub const LOCALE_COUNT: usize = 16;
/// Columns spanned by one localized string: the locales plus the mask.
pub const LOCALIZED_WIDTH: usize = LOCALE_COUNT + 1;

/// Index of a locale within a localized string column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(usize)]
pub enum Locale {
    #[default]
    EnUs = 0,
    KoKr = 1,
    FrFr = 2,
    DeDe = 3,
    ZhCn = 4,
    ZhTw = 5,
    EsEs = 6,
    EsMx = 7,
    RuRu = 8,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a WDBC file (magic {0:?})")]
    BadMagic([u8; 4]),
    #[error("file is {got} bytes, too short for a {HEADER_SIZE}-byte header")]
    TooShort { got: usize },
    #[error(
        "truncated: header describes {expected} bytes ({records} records x {record_size} + \
         {strings} string bytes + header) but the file is only {got}"
    )]
    Truncated {
        expected: usize,
        got: usize,
        records: u32,
        record_size: u32,
        strings: u32,
    },
    #[error("{table}: expected {expected} fields for build 12340, file has {got}")]
    UnexpectedSchema {
        table: &'static str,
        expected: u32,
        got: u32,
    },
    #[error(
        "{table}: record size {record_size} is not {fields} x 4 bytes -- this table is \
         byte-packed and needs a bespoke layout rather than word accessors"
    )]
    NonUniform {
        table: &'static str,
        record_size: usize,
        fields: u32,
    },
}

/// A parsed DBC table.
pub struct Dbc {
    fields: u32,
    record_size: usize,
    records: Vec<u8>,
    strings: Vec<u8>,
}

impl Dbc {
    /// Parses a table.
    ///
    /// The length check is the cheapest guard against a mis-decompressed file:
    /// the three sizes in the header are independent, so corrupt data almost
    /// never produces a self-consistent one.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::TooShort { got: bytes.len() });
        }
        if &bytes[..4] != b"WDBC" {
            return Err(Error::BadMagic([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }
        let word = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let (records, fields, record_size, strings) = (word(4), word(8), word(12), word(16));

        let body = records as usize * record_size as usize;
        let declared = HEADER_SIZE + body + strings as usize;
        if declared > bytes.len() {
            return Err(Error::Truncated {
                expected: declared,
                got: bytes.len(),
                records,
                record_size,
                strings,
            });
        }
        if declared < bytes.len() {
            // Third-party patch tooling emits this; the client ignores it
            // because nothing indexes past the declared string block.
            tracing::debug!(
                slack = bytes.len() - declared,
                "file is longer than its header declares"
            );
        }

        Ok(Self {
            fields,
            record_size: record_size as usize,
            records: bytes[HEADER_SIZE..HEADER_SIZE + body].to_vec(),
            // Bounded by the declared size, so trailing slack is dropped
            // rather than becoming addressable string data.
            strings: bytes[HEADER_SIZE + body..declared].to_vec(),
        })
    }

    /// Whether `record_size` is exactly `field_count * 4`.
    ///
    /// False for the handful of byte-packed tables, whose columns cannot be
    /// read as uniform 32-bit words and need a bespoke layout.
    pub fn is_uniform(&self) -> bool {
        self.record_size == self.fields as usize * FIELD_SIZE
    }

    /// Number of whole 32-bit words in a record, which is what the word-based
    /// accessors can actually reach.
    pub fn word_count(&self) -> usize {
        self.record_size / FIELD_SIZE
    }

    pub fn len(&self) -> usize {
        if self.record_size == 0 {
            0
        } else {
            self.records.len() / self.record_size
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn fields(&self) -> u32 {
        self.fields
    }

    pub fn record_size(&self) -> usize {
        self.record_size
    }

    pub fn string_block(&self) -> &[u8] {
        &self.strings
    }

    pub fn row(&self, index: usize) -> Option<Row<'_>> {
        let start = index.checked_mul(self.record_size)?;
        let data = self.records.get(start..start + self.record_size)?;
        Some(Row { data, dbc: self })
    }

    pub fn rows(&self) -> impl ExactSizeIterator<Item = Row<'_>> {
        (0..self.len()).map(|i| self.row(i).expect("index within len"))
    }

    /// Resolves an offset into the string block.
    ///
    /// Offset 0 is the empty string by convention -- the block starts with a
    /// NUL for exactly this purpose -- so an unset string column reads as `""`
    /// rather than failing.
    pub fn string_at(&self, offset: u32) -> &str {
        let offset = offset as usize;
        let Some(tail) = self.strings.get(offset..) else {
            return "";
        };
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        std::str::from_utf8(&tail[..end]).unwrap_or("")
    }
}

impl fmt::Debug for Dbc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dbc")
            .field("records", &self.len())
            .field("fields", &self.fields)
            .field("record_size", &self.record_size)
            .field("string_block", &self.strings.len())
            .finish()
    }
}

/// One record. Column accessors return a default rather than panicking on an
/// out-of-range index, so a partially-transcribed schema degrades quietly.
#[derive(Clone, Copy)]
pub struct Row<'a> {
    data: &'a [u8],
    dbc: &'a Dbc,
}

impl<'a> Row<'a> {
    #[inline]
    pub fn raw(&self, field: usize) -> u32 {
        let start = field * FIELD_SIZE;
        self.data
            .get(start..start + FIELD_SIZE)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .unwrap_or(0)
    }

    #[inline]
    pub fn u32(&self, field: usize) -> u32 {
        self.raw(field)
    }

    #[inline]
    pub fn i32(&self, field: usize) -> i32 {
        self.raw(field) as i32
    }

    #[inline]
    pub fn f32(&self, field: usize) -> f32 {
        f32::from_bits(self.raw(field))
    }

    #[inline]
    pub fn bool(&self, field: usize) -> bool {
        self.raw(field) != 0
    }

    pub fn string(&self, field: usize) -> &'a str {
        self.dbc.string_at(self.raw(field))
    }

    /// Reads one locale out of a localized string column.
    pub fn localized(&self, field: usize, locale: Locale) -> &'a str {
        self.dbc.string_at(self.raw(field + locale as usize))
    }

    /// Reads a localized column, falling back to `enUS` when the requested
    /// locale is empty. Stock installs ship only the locale they were
    /// downloaded for, so every other slot is blank.
    pub fn localized_or_english(&self, field: usize, locale: Locale) -> &'a str {
        match self.localized(field, locale) {
            "" => self.localized(field, Locale::EnUs),
            s => s,
        }
    }

    pub fn fields(&self) -> usize {
        self.data.len() / FIELD_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic table so the edge cases can be tested without game
    /// data.
    fn build(rows: &[&[u32]], strings: &[u8]) -> Vec<u8> {
        let fields = rows.first().map_or(0, |r| r.len());
        let mut out = Vec::new();
        out.extend_from_slice(b"WDBC");
        out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        out.extend_from_slice(&(fields as u32).to_le_bytes());
        out.extend_from_slice(&((fields * FIELD_SIZE) as u32).to_le_bytes());
        out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        for row in rows {
            for v in *row {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.extend_from_slice(strings);
        out
    }

    /// Offset 0 must read as empty, and offsets must resolve to whole strings.
    #[test]
    fn resolves_strings_including_the_empty_one() {
        let strings = b"\0hello\0world\0";
        let dbc = Dbc::parse(&build(&[&[0, 1, 7]], strings)).unwrap();
        let row = dbc.row(0).unwrap();
        assert_eq!(row.string(0), "");
        assert_eq!(row.string(1), "hello");
        assert_eq!(row.string(2), "world");
    }

    /// A file longer than its header declares is tolerated, and the slack must
    /// not become addressable string data.
    #[test]
    fn tolerates_trailing_slack() {
        let mut bytes = build(&[&[1]], b"\0abc\0");
        bytes.extend_from_slice(b"junk-past-the-end");
        let dbc = Dbc::parse(&bytes).unwrap();
        assert_eq!(dbc.string_block().len(), 5);
        assert_eq!(dbc.row(0).unwrap().string(0), "abc");
    }

    /// Truncation is the dangerous direction and must fail loudly.
    #[test]
    fn rejects_truncation() {
        let bytes = build(&[&[1, 2], &[3, 4]], b"\0xy\0");
        let short = &bytes[..bytes.len() - 6];
        assert!(matches!(Dbc::parse(short), Err(Error::Truncated { .. })));
    }

    /// `record_size` is authoritative, and a byte-packed table is reported
    /// rather than rejected.
    #[test]
    fn detects_byte_packed_records() {
        // 3 fields declared, but only 7 bytes per record.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WDBC");
        bytes.extend_from_slice(&2u32.to_le_bytes()); // records
        bytes.extend_from_slice(&3u32.to_le_bytes()); // fields
        bytes.extend_from_slice(&7u32.to_le_bytes()); // record_size
        bytes.extend_from_slice(&1u32.to_le_bytes()); // strings
        bytes.extend_from_slice(&[0u8; 14]);
        bytes.push(0);

        let dbc = Dbc::parse(&bytes).unwrap();
        assert!(!dbc.is_uniform());
        assert_eq!(dbc.len(), 2);
        assert_eq!(dbc.word_count(), 1, "only one whole word fits in 7 bytes");
    }

    #[test]
    fn reads_numeric_columns() {
        let dbc = Dbc::parse(&build(
            &[&[7, (-3i32) as u32, 1.5f32.to_bits(), 0, 1]],
            b"\0",
        ))
        .unwrap();
        let row = dbc.row(0).unwrap();
        assert_eq!(row.u32(0), 7);
        assert_eq!(row.i32(1), -3);
        assert_eq!(row.f32(2), 1.5);
        assert!(!row.bool(3));
        assert!(row.bool(4));
        // Out-of-range columns degrade to zero rather than panicking.
        assert_eq!(row.u32(99), 0);
    }

    #[test]
    fn rejects_non_wdbc() {
        assert!(matches!(
            Dbc::parse(b"WDB2\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0"),
            Err(Error::BadMagic(_))
        ));
    }
}
