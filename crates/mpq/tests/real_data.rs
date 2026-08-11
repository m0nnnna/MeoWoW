//! Integration tests against a real 3.3.5a installation.
//!
//! These skip unless `WOW_DATA` points at a `Data` directory, because the
//! project never ships game assets. Run them with:
//!
//! ```console
//! WOW_DATA="D:/Games/World of Warcraft 3.3.5a/Data" cargo test -p mpq
//! ```

use mpq::{Chain, State};

fn chain() -> Option<Chain> {
    let data = std::env::var_os("WOW_DATA")?;
    Some(Chain::open_wow_data(data, "enUS").expect("opening archives"))
}

macro_rules! require_data {
    () => {
        match chain() {
            Some(c) => c,
            None => {
                eprintln!("skipping: WOW_DATA not set");
                return;
            }
        }
    };
}

/// A stock install is 17 archives; the count is a canary for the load order
/// silently dropping one.
#[test]
fn opens_the_full_stock_chain() {
    let mut chain = require_data!();
    assert_eq!(chain.archives().count(), 17);
    assert!(chain.list().unwrap().len() > 200_000);
}

/// Patches must shadow base content. `Map.dbc` exists in four archives and the
/// highest-priority locale patch has to win, or the engine gets stale tables.
#[test]
fn patch_chain_prefers_the_highest_priority_archive() {
    let chain = require_data!();
    let source = chain
        .source_of(r"DBFilesClient\Map.dbc")
        .expect("Map.dbc resolves");
    assert!(
        source.ends_with("patch-enUS-3.MPQ"),
        "expected the top locale patch to win, got {}",
        source.display()
    );
}

/// A delete marker in a patch must mask the copy still present in a lower
/// archive. Regression test: resolving through the tombstone resurrected 5,121
/// files that the real client does not see.
#[test]
fn delete_markers_mask_lower_archives() {
    let chain = require_data!();
    let name = r"Creature\KodobeastPack\KodoBeastPack.m2";

    let trace = chain.trace(name);
    let deleted_above_present = trace
        .iter()
        .position(|(_, s)| matches!(s, State::Deleted { .. }))
        .zip(
            trace
                .iter()
                .position(|(_, s)| matches!(s, State::Present { .. })),
        )
        .is_some_and(|(del, present)| del < present);

    assert!(
        deleted_above_present,
        "test fixture no longer has a tombstone shadowing a real file: {trace:?}"
    );
    assert!(
        !chain.contains(name),
        "tombstoned file must not resolve, but it did"
    );

    // A tombstone carries no payload; that is what distinguishes it from a
    // legitimate zero-length file.
    if let Some((_, State::Deleted { size, flags })) = trace.first() {
        assert_eq!(*size, 0);
        assert_eq!(flags & 0x0200_0000, 0x0200_0000);
    }
}

/// Decompression correctness, checked through content rather than a size
/// field: a wrong byte anywhere corrupts the string block and the map names
/// stop being readable.
#[test]
fn decompresses_map_dbc_to_valid_content() {
    let mut chain = require_data!();
    let data = chain.read(r"DBFilesClient\Map.dbc").unwrap();

    assert_eq!(&data[..4], b"WDBC");
    let u32at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let (records, fields, rsize, sblock) = (u32at(4), u32at(8), u32at(12), u32at(16));

    assert_eq!(fields * 4, rsize, "field count and record size disagree");
    assert_eq!(
        data.len() as u32,
        20 + records * rsize + sblock,
        "header does not account for the file length"
    );

    // Record 0 is Azeroth, whose directory string is a stable fixture.
    let strings = &data[(20 + records * rsize) as usize..];
    let offset = u32at(20 + 4) as usize;
    let end = offset + strings[offset..].iter().position(|&b| b == 0).unwrap();
    assert_eq!(&strings[offset..end], b"Azeroth");
}
