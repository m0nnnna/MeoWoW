//! Column type inference for undocumented tables.
//!
//! A DBC records no type information, so transcribing a new table means
//! staring at columns until they confess. This automates the staring: it
//! classifies each column by testing every value in it against what each type
//! would have to look like.
//!
//! Inference is a starting point for a schema, not a substitute for one. It is
//! reliable on strings and floats, guesses on sparse columns, and cannot tell a
//! foreign key from any other integer.

use crate::{Dbc, FIELD_SIZE, LOCALIZED_WIDTH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    /// Every value is zero -- unused padding, or a field this build never set.
    Empty,
    Bool,
    Int,
    Float,
    String,
    /// First column of a 17-wide localized string block.
    Localized,
    /// One of the 15 non-English locale slots following a [`Localized`].
    LocalePad,
    /// The bitmask terminating a localized block.
    LocaleMask,
}

impl ColumnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::String => "string",
            Self::Localized => "loc",
            Self::LocalePad => "  ·",
            Self::LocaleMask => "  mask",
        }
    }
}

/// What a column looks like, with the evidence behind the guess.
#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub index: usize,
    pub kind: ColumnKind,
    pub min: u32,
    pub max: u32,
    /// Rows whose value is zero -- a mostly-empty column is a weak guess.
    pub zeros: usize,
}

/// Classifies every column in a table.
pub fn infer(dbc: &Dbc) -> Vec<Column> {
    let field_count = dbc.record_size() / FIELD_SIZE;
    let mut columns: Vec<Column> = (0..field_count)
        .map(|index| classify(dbc, index))
        .collect();
    mark_localized(&mut columns);
    columns
}

fn classify(dbc: &Dbc, index: usize) -> Column {
    let values: Vec<u32> = dbc.rows().map(|r| r.raw(index)).collect();
    let zeros = values.iter().filter(|&&v| v == 0).count();
    let min = values.iter().copied().min().unwrap_or(0);
    let max = values.iter().copied().max().unwrap_or(0);

    let kind = if values.is_empty() || max == 0 {
        ColumnKind::Empty
    } else if max == 1 {
        // Ordered ahead of the string test deliberately. Offset 1 is a valid
        // pointer to the string block's first entry, so a flag column is
        // indistinguishable from a string column where every row names that
        // one string -- and a flag is overwhelmingly the more likely of the
        // two. Guessing wrong here does not just mislabel one column: it
        // shifts the localized-block detector and corrupts everything after
        // it.
        ColumnKind::Bool
    } else if looks_like_strings(dbc, &values) {
        ColumnKind::String
    } else if looks_like_floats(&values) {
        ColumnKind::Float
    } else {
        ColumnKind::Int
    };

    Column {
        index,
        kind,
        min,
        max,
        zeros,
    }
}

/// A string column holds offsets into the string block, and every non-zero
/// offset must land immediately after a NUL -- that is, at the start of a
/// string rather than in the middle of one. Requiring the text to be printable
/// as well makes a false positive on a small-integer column very unlikely.
fn looks_like_strings(dbc: &Dbc, values: &[u32]) -> bool {
    let block = dbc.string_block();
    if block.len() <= 1 {
        return false;
    }

    let mut distinct = std::collections::BTreeSet::new();
    for &v in values {
        if v == 0 {
            continue;
        }
        let offset = v as usize;
        // A real offset lands at the start of a string, i.e. just past a NUL.
        // This is what rejects small-integer columns: in `Map.dbc` the block
        // opens with `\0Azeroth\0`, so offset 2 would point mid-word and fail.
        if offset >= block.len() || block[offset - 1] != 0 {
            return false;
        }
        let end = block[offset..]
            .iter()
            .position(|&b| b == 0)
            .map_or(block.len(), |e| offset + e);
        // Tabs and newlines appear in quest and spell description text.
        if block[offset..end]
            .iter()
            .any(|&b| b < 0x09 || (b > 0x0D && b < 0x20))
        {
            return false;
        }
        distinct.insert(offset);
    }
    // A column that names the same single string in every row carries no
    // information as text and is more likely an integer that happens to be a
    // valid offset.
    distinct.len() > 1
}

/// Float bit patterns cluster in a range that ordinary small integers cannot
/// reach: as a float, the integer 42 is a denormal around 6e-44, and 0xFFFFFFFF
/// is NaN. Requiring every value to decode to a plausible magnitude separates
/// them cleanly.
fn looks_like_floats(values: &[u32]) -> bool {
    let mut real = 0usize;
    for &v in values {
        if v == 0 {
            continue;
        }
        let f = f32::from_bits(v);
        if !f.is_finite() {
            return false;
        }
        let m = f.abs();
        if !(1e-6..=1e12).contains(&m) {
            return false;
        }
        real += 1;
    }
    real > 1
}

/// Collapses runs of 17 columns into a single localized string.
///
/// The shape is unmistakable: a string column, 15 more string-or-empty columns
/// (blank in a single-locale install), then a small bitmask saying which
/// locales were populated.
fn mark_localized(columns: &mut [Column]) {
    let n = columns.len();
    let mut i = 0;
    while i + LOCALIZED_WIDTH <= n {
        let head_is_text = columns[i].kind == ColumnKind::String;
        let tail_is_text = columns[i + 1..i + 16]
            .iter()
            .all(|c| matches!(c.kind, ColumnKind::String | ColumnKind::Empty));
        let mask = columns[i + 16];
        let mask_ok = matches!(mask.kind, ColumnKind::Int | ColumnKind::Empty | ColumnKind::Bool);

        if head_is_text && tail_is_text && mask_ok {
            columns[i].kind = ColumnKind::Localized;
            for c in &mut columns[i + 1..i + 16] {
                c.kind = ColumnKind::LocalePad;
            }
            columns[i + 16].kind = ColumnKind::LocaleMask;
            i += LOCALIZED_WIDTH;
        } else {
            i += 1;
        }
    }
}
