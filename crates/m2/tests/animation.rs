//! Animation tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory.

use std::collections::BTreeMap;

use glam::Vec3;
use m2::{Model, Sequence};
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

/// Loads a model together with whatever external keyframe files it needs.
fn load(chain: &mut Chain, path: &str) -> (Model, Vec<Sequence>, Vec<m2::AnimatedBone>) {
    let model = Model::parse(&chain.read(path).expect(path)).expect(path);
    let sequences = model.sequences();
    let external: BTreeMap<usize, Vec<u8>> = sequences
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.is_inline())
        .filter_map(|(i, s)| {
            chain
                .read(&m2::anim::external_anim_path(path, s))
                .ok()
                .map(|b| (i, b))
        })
        .collect();
    let bones = model.animated_bones_with(&external);
    (model, sequences, bones)
}

const MODELS: &[&str] = &[
    r"Creature\BogBeast\BogBeast.m2",
    r"Creature\GnollMelee\GnollMelee.m2",
    r"Character\Human\Male\HumanMale.m2",
];

/// Rotation keys are unit quaternions by construction.
///
/// This is the sharpest available probe on the whole animation pipeline: it
/// catches a wrong component decode, a wrong stride, and -- how the external
/// `.anim` files were discovered -- keyframes read out of a file that does not
/// contain them. Garbage bytes essentially never decode to a unit quaternion.
#[test]
fn every_rotation_key_is_a_unit_quaternion() {
    let mut chain = require_data!();
    for path in MODELS {
        let (_, _, bones) = load(&mut chain, path);
        let (mut n, mut worst) = (0usize, 0.0f32);
        for bone in &bones {
            for keys in &bone.rotation.sequences {
                for q in &keys.values {
                    worst = worst.max((q.length() - 1.0).abs());
                    n += 1;
                }
            }
        }
        assert!(n > 1000, "{path}: only {n} rotation keys");
        assert!(
            worst < 1e-3,
            "{path}: worst |q|-1 is {worst} across {n} keys"
        );
    }
}

/// A sequence with no data must sample to nothing rather than to noise, so the
/// model falls back to bind pose instead of exploding.
#[test]
fn sequences_without_data_sample_to_nothing() {
    let mut chain = require_data!();
    let path = r"Creature\GnollMelee\GnollMelee.m2";
    let model = Model::parse(&chain.read(path).unwrap()).unwrap();
    let sequences = model.sequences();

    // Deliberately load *no* external files.
    let bones = model.animated_bones();
    let external: Vec<usize> = sequences
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.is_inline())
        .map(|(i, _)| i)
        .collect();
    assert!(!external.is_empty(), "fixture has no external sequences");

    for i in external {
        for bone in &bones {
            assert!(
                bone.rotation.sample(i, 0).is_none(),
                "sequence {i} has no data here but sampled anyway"
            );
        }
    }
}

/// Posing must not fling geometry away from the model.
///
/// Skinning is applied on the GPU, so this repeats it on the CPU and checks the
/// result stays near the bind-pose bounds. A wrong pivot, a mis-ordered matrix
/// product, or a bad parent walk all show up as vertices thousands of units out.
#[test]
fn posed_vertices_stay_near_the_model() {
    let mut chain = require_data!();
    for path in MODELS {
        let (model, sequences, bones) = load(&mut chain, path);
        let vertices = model.vertices();
        let radius = vertices
            .iter()
            .map(|v| Vec3::from(v.position).length())
            .fold(0.0f32, f32::max)
            .max(1.0);

        for (si, seq) in sequences.iter().enumerate().take(12) {
            for t in [0, seq.duration_ms / 3, seq.duration_ms.saturating_sub(1)] {
                let pose = Model::pose_bones(&bones, si, t);
                if pose.is_empty() {
                    continue;
                }
                for (vi, v) in vertices.iter().enumerate().step_by(17) {
                    let total: u32 = v.bone_weights.iter().map(|&w| w as u32).sum();
                    if total == 0 {
                        continue;
                    }
                    let mut p = Vec3::ZERO;
                    for (&bi, &w) in v.bone_indices.iter().zip(&v.bone_weights) {
                        if w == 0 {
                            continue;
                        }
                        if let Some(m) = pose.get(bi as usize) {
                            p += m.transform_point3(Vec3::from(v.position))
                                * (w as f32 / total as f32);
                        }
                    }
                    assert!(
                        p.is_finite() && p.length() < radius * 4.0,
                        "{path} seq {si} t{t}: vertex {vi} posed to {p:?} \
                         (bind radius {radius:.2})"
                    );
                }
            }
        }
    }
}

/// Bone matrices stay finite and bounded.
///
/// Deliberately *not* asserting invertibility: M2 collapses bones to zero scale
/// to hide geometry, so a singular matrix is a feature. `HumanMale` bone 24 has
/// sixteen explicit scale keys that sample to exactly `(0, 0, 0)` during its
/// first sequence.
#[test]
fn bone_matrices_are_finite_and_bounded() {
    let mut chain = require_data!();
    let mut collapsed = 0usize;

    for path in MODELS {
        let (_, sequences, bones) = load(&mut chain, path);
        for (si, seq) in sequences.iter().enumerate().take(8) {
            let pose = Model::pose_bones(&bones, si, seq.duration_ms / 2);
            for (bi, m) in pose.iter().enumerate() {
                assert!(
                    m.to_cols_array().iter().all(|f| f.is_finite()),
                    "{path} seq {si} bone {bi}: non-finite matrix"
                );
                let det = m.determinant();
                assert!(
                    det.abs() < 1e4,
                    "{path} seq {si} bone {bi}: determinant {det} is an explosion"
                );
                if det.abs() < 1e-6 {
                    collapsed += 1;
                }
            }
        }
    }
    // The zero-scale trick is real and should keep showing up; if it stops,
    // scale tracks have probably stopped being read.
    assert!(collapsed > 0, "expected some deliberately collapsed bones");
}

/// A bone with no keys in a sequence must resolve to identity, so an
/// unanimated model renders exactly as the unskinned renderer drew it -- and a
/// bone on a *global* sequence must not, because it has no sequence to be
/// outside of.
///
/// **Both halves, because the second one broke the first.** This asserted
/// plain identity for every bone until global tracks began resolving, on the
/// premise that "a sequence index past the end has no keys anywhere". A global
/// track holds one keyframe list on a timeline shared by every animation, so
/// the premise stopped being true and BogBeast's bone 44 -- carrying a constant
/// scale of 1.45 -- started failing a test that was describing the old bug.
/// Asserting only identity-for-the-rest would pass again while quietly
/// tolerating a regression back to that bug, so the global bones are asserted
/// to be *different* from identity in the same breath.
#[test]
fn unanimated_bones_pose_to_identity_unless_they_are_global() {
    let mut chain = require_data!();
    let (_, _, bones) = load(&mut chain, r"Creature\BogBeast\BogBeast.m2");
    // Sequence index far past the end: nothing keyed per-sequence can answer.
    let pose = Model::pose_bones(&bones, 9999, 0);
    let has_global_track = |b: &m2::AnimatedBone| {
        b.translation.global_sequence.is_some()
            || b.rotation.global_sequence.is_some()
            || b.scale.global_sequence.is_some()
    };
    // **Up the parent chain, not just the bone.** A pose is composed with its
    // parent's, so a bone with no track of its own still inherits an ancestor's
    // global scale -- BogBeast's bone 62 carries a 1.5 it never asked for.
    // Checking only the bone itself made this test fail on a model that was
    // behaving exactly as it should.
    let is_global = |mut index: usize| loop {
        let Some(bone) = bones.get(index) else {
            return false;
        };
        if has_global_track(bone) {
            return true;
        }
        match usize::try_from(bone.bone.parent) {
            Ok(parent) if parent != index => index = parent,
            _ => return false,
        }
    };
    let worst_of = |m: &glam::Mat4| {
        (*m - glam::Mat4::IDENTITY)
            .to_cols_array()
            .iter()
            .fold(0.0f32, |a, b| a.max(b.abs()))
    };

    let mut globals = 0;
    for (i, m) in pose.iter().enumerate() {
        if is_global(i) {
            globals += 1;
            continue;
        }
        assert!(
            worst_of(m) < 1e-5,
            "bone {i} inherits no global track and is not identity: {m:?}"
        );
    }
    // The model was chosen because it has them. If it stops having any, this
    // test has quietly become the weaker one it used to be.
    assert!(
        globals > 0,
        "BogBeast has no global-sequence bones, so this proves only half of it"
    );
    assert!(
        pose.iter()
            .enumerate()
            .any(|(i, m)| is_global(i) && worst_of(m) > 1e-5),
        "every global bone posed to identity, which is the bug this guards"
    );
}
