//! Texture tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory.

use blp::{Blp, Encoding, Level};
use mpq::Chain;

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

fn load(chain: &mut Chain, path: &str) -> Blp {
    let bytes = chain.read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    Blp::parse(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// One texture per encoding, so every decode path stays exercised.
#[test]
fn decodes_every_encoding() {
    let mut chain = require_data!();
    let cases: &[(&str, Encoding, u32, u32)] = &[
        (
            r"CHARACTER\Vrykul\Male\TEXTURES\VYKRULMALEGREY.BLP",
            Encoding::Dxt(blp::DxtFormat::Dxt1),
            256,
            256,
        ),
        (
            r"Character\Goblin\Female\GoblinFemale01.blp",
            Encoding::Dxt(blp::DxtFormat::Dxt3),
            256,
            256,
        ),
        (
            r"CHARACTER\BloodElf\Male\BLOODELFFEMALEEYEGLOWGREEN.BLP",
            Encoding::Dxt(blp::DxtFormat::Dxt5),
            32,
            32,
        ),
        (
            r"CHARACTER\Taunka\Male\TAUNKAMALEFACELOWER00_00.BLP",
            Encoding::Palettized,
            256,
            128,
        ),
        (r"textures\SunGlare.blp", Encoding::Bgra, 256, 256),
    ];

    for &(path, encoding, w, h) in cases {
        let tex = load(&mut chain, path);
        assert_eq!(tex.encoding(), encoding, "{path}");
        assert_eq!((tex.width(), tex.height()), (w, h), "{path}");

        let rgba = tex.decode_rgba(0).expect("mip 0");
        assert_eq!(rgba.len(), (w * h * 4) as usize, "{path}");
        // A decode that silently produced nothing would still be the right
        // length, so require some variation.
        let first = &rgba[..4];
        assert!(
            rgba.chunks_exact(4).any(|px| px != first),
            "{path} decoded to a single flat colour"
        );
    }
}

/// A texture with no alpha channel must come out fully opaque; getting this
/// wrong makes every DXT1 surface invisible.
#[test]
fn dxt1_without_alpha_is_opaque() {
    let mut chain = require_data!();
    let tex = load(&mut chain, r"Interface\Icons\Spell_Fire_Fireball02.blp");
    assert_eq!(tex.alpha_depth(), 0);
    let rgba = tex.decode_rgba(0).unwrap();
    assert!(rgba.chunks_exact(4).all(|px| px[3] == 255));
}

/// The mip chain halves each level and clamps at 1x1, so a 64x64 texture has
/// seven levels rather than six.
#[test]
fn mip_chain_runs_down_to_one_pixel() {
    let mut chain = require_data!();
    let tex = load(&mut chain, r"Interface\Icons\Spell_Fire_Fireball02.blp");
    assert_eq!(tex.mip_count(), 7);
    assert_eq!(tex.level_size(0), (64, 64));
    assert_eq!(tex.level_size(6), (1, 1));
}

/// Cross-checks our block arithmetic against Blizzard's stored sizes across a
/// broad sample. If `level_bytes` were wrong -- forgetting that a 2x2 mip still
/// occupies a whole 4x4 block, say -- the usable levels would disagree.
///
/// Only the usable prefix is checked, because the tail is padding rather than
/// image data; `padded_mip_tails_are_a_single_block` pins that separately.
#[test]
fn usable_mip_sizes_match_computed_block_sizes() {
    let mut chain = require_data!();
    let names: Vec<String> = chain
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".blp"))
        .step_by(97) // spread the sample across the archive set
        .take(1500)
        .collect();

    let (mut checked, mut mismatches) = (0usize, Vec::new());
    for name in &names {
        let Ok(bytes) = chain.read(name) else { continue };
        let Ok(tex) = Blp::parse(&bytes) else { continue };

        for level in 0..tex.usable_mip_count() {
            let (w, h) = tex.level_size(level);
            if let Some(Level::Dxt { format, blocks }) = tex.level(level) {
                let want = format.level_bytes(w, h);
                if blocks.len() != want {
                    mismatches.push(format!(
                        "{name} mip {level} ({w}x{h} {}): stored {}, computed {want}",
                        format.name(),
                        blocks.len()
                    ));
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 1000, "sample too small: {checked} levels");
    assert!(
        mismatches.is_empty(),
        "{} mip sizes disagree: {:#?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(5)]
    );
}

/// Where the mip chain is padded, every padded level is exactly one block.
///
/// This is what makes `usable_mip_count` safe to rely on: the tail is a known,
/// uniform shape rather than arbitrary truncation.
#[test]
fn padded_mip_tails_are_a_single_block() {
    let mut chain = require_data!();
    let names: Vec<String> = chain
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".blp"))
        .step_by(53)
        .take(2500)
        .collect();

    let (mut padded, mut odd) = (0usize, Vec::new());
    for name in &names {
        let Ok(bytes) = chain.read(name) else { continue };
        let Ok(tex) = Blp::parse(&bytes) else { continue };
        let usable = tex.usable_mip_count();
        if usable == tex.mip_count() {
            continue;
        }
        padded += 1;

        for level in usable..tex.mip_count() {
            if let Some(Level::Dxt { format, blocks }) = tex.level(level) {
                if blocks.len() != format.block_bytes() {
                    odd.push(format!(
                        "{name} mip {level}: {} bytes, expected one {} block",
                        blocks.len(),
                        format.name()
                    ));
                }
            }
        }
    }
    assert!(padded > 0, "sample contained no padded chains");
    assert!(odd.is_empty(), "{:#?}", &odd[..odd.len().min(5)]);
}

/// The specific texture that exposed the padding behaviour.
#[test]
fn padded_chain_reports_only_its_real_levels() {
    let mut chain = require_data!();
    let tex = load(
        &mut chain,
        r"Interface\AchievementFrame\UI-Achievement-MetalBorder-Top.blp",
    );
    assert_eq!((tex.width(), tex.height()), (512, 16));
    assert_eq!(tex.mip_count(), 10, "declares a full chain");
    assert_eq!(
        tex.usable_mip_count(),
        3,
        "only levels down to 128x4 hold real data"
    );
}

/// Palettized levels store one index per pixel followed by a packed alpha
/// plane, so the stored size is a function of dimensions and alpha depth.
#[test]
fn palettized_levels_are_sized_by_alpha_depth() {
    let mut chain = require_data!();
    for path in [
        r"CHARACTER\Taunka\Male\TAUNKAMALEFACELOWER00_00.BLP",
        r"CHARACTER\Taunka\Male\TaunkaMaleSkin00_00_Extra.blp",
        r"Character\Dwarf\Hair00_10.blp",
    ] {
        let tex = load(&mut chain, path);
        let depth = tex.alpha_depth() as usize;
        for level in 0..tex.mip_count() {
            let (w, h) = tex.level_size(level);
            let pixels = (w * h) as usize;
            if let Some(Level::Palettized { indices, alpha, .. }) = tex.level(level) {
                assert_eq!(indices.len(), pixels, "{path} mip {level} index plane");
                // The alpha plane rounds up to whole bytes.
                let want = (pixels * depth).div_ceil(8);
                assert_eq!(alpha.len(), want, "{path} mip {level} alpha plane");
            }
        }
    }
}

/// Every texture in the install must parse. Sampled rather than exhaustive so
/// the suite stays fast; `wow-cli blp survey` covers all 112k.
#[test]
fn sampled_textures_all_parse() {
    let mut chain = require_data!();
    let names: Vec<String> = chain
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".blp"))
        .step_by(37)
        .take(3000)
        .collect();

    let mut failures = Vec::new();
    for name in &names {
        let Ok(bytes) = chain.read(name) else { continue };
        match Blp::parse(&bytes) {
            Ok(tex) => {
                // Decoding the smallest level is cheap and still exercises the
                // partial-block path.
                let last = tex.mip_count().saturating_sub(1);
                if tex.decode_rgba(last).is_none() && tex.mip_count() > 0 {
                    failures.push(format!("{name}: mip {last} did not decode"));
                }
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
