//! Model tests against a real 3.3.5a installation.
//!
//! Skipped unless `WOW_DATA` points at a `Data` directory.

use m2::{model_path, skin_path, Model, Skin};
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

fn load(chain: &mut Chain, path: &str) -> Model {
    Model::parse(&chain.read(path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn load_skin(chain: &mut Chain, path: &str) -> Skin {
    Skin::parse(&chain.read(path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn parses_a_known_model() {
    let mut chain = require_data!();
    let path = r"Creature\BogBeast\BogBeast.m2";
    let model = load(&mut chain, path);

    assert_eq!(model.version(), m2::VERSION_WOTLK);
    assert_eq!(model.name(), "BogBeast");
    assert_eq!(model.vertex_count(), 985);
    assert_eq!(model.bones().len(), 70);
    assert_eq!(model.textures().len(), 2);
    assert_eq!(model.materials().len(), 2);

    // Creature skins are supplied by CreatureDisplayInfo, not named in-model.
    assert!(model.textures().iter().all(|t| !t.is_hardcoded()));
    assert_eq!(model.textures()[0].kind, 11);
}

/// Normals are unit vectors by construction, so their lengths are the sharpest
/// available probe on the vertex layout: a wrong stride or field offset breaks
/// them immediately while everything still parses.
#[test]
fn vertex_layout_produces_unit_normals() {
    let mut chain = require_data!();
    let model = load(&mut chain, r"Creature\BogBeast\BogBeast.m2");

    for (i, v) in model.vertices().iter().enumerate() {
        let len: f32 = v.normal.iter().map(|c| c * c).sum::<f32>().sqrt();
        assert!(
            (0.98..=1.02).contains(&len),
            "vertex {i} normal has length {len}"
        );
        let weight: u32 = v.bone_weights.iter().map(|&w| w as u32).sum();
        assert!(
            weight == 0 || (250..=260).contains(&weight),
            "vertex {i} bone weights sum to {weight}"
        );
    }
}

/// Skin indices are two levels deep -- triangles index the vertex map, which
/// indexes the model. Resolving them wrongly yields plausible but scrambled
/// geometry, so this checks the resolution explicitly.
#[test]
fn resolves_submesh_geometry() {
    let mut chain = require_data!();
    let model = load(&mut chain, r"Creature\BogBeast\BogBeast.m2");
    let skin = load_skin(&mut chain, r"Creature\BogBeast\BogBeast00.skin");

    skin.validate(model.vertex_count()).expect("index tables");
    assert_eq!(skin.submeshes().len(), 2);
    assert_eq!(skin.batches().len(), 2);

    let total: u32 = skin.submeshes().iter().map(|s| s.index_count).sum();
    assert_eq!(
        total as usize,
        skin.triangles().len(),
        "submeshes should tile the index array"
    );

    for sub in skin.submeshes() {
        let indices = skin.submesh_indices(sub).expect("resolves");
        assert_eq!(indices.len(), sub.index_count as usize);
        assert!(indices.iter().all(|&i| (i as usize) < model.vertex_count()));
        assert_eq!(indices.len() % 3, 0);
    }
}

/// Every batch must resolve through the texture combo table to a real texture
/// slot; that indirection is what the renderer performs per draw call.
#[test]
fn batches_resolve_to_texture_slots() {
    let mut chain = require_data!();
    let model = load(&mut chain, r"Creature\BogBeast\BogBeast.m2");
    let skin = load_skin(&mut chain, r"Creature\BogBeast\BogBeast00.skin");

    let combos = model.texture_combos();
    let textures = model.textures();
    for batch in skin.batches() {
        let combo = combos
            .get(batch.texture_combo_index as usize)
            .expect("combo index in range");
        assert!(
            (*combo as usize) < textures.len(),
            "combo points outside the texture list"
        );
        assert!((batch.material_index as usize) < model.materials().len());
    }
}

/// `indexCount` is a 16-bit field and a few models overflow it. The true span
/// is recovered from the next submesh's start.
#[test]
fn repairs_sixteen_bit_count_overflow() {
    let mut chain = require_data!();
    let path = r"World\DUNGEON\Sunwell\Sunwell_Bushes.m2";
    let model = load(&mut chain, path);
    let skin = load_skin(&mut chain, &skin_path(path, 0));

    assert_eq!(skin.triangles().len(), 93_456);
    let sub = &skin.submeshes()[0];
    assert!(sub.counts_repaired, "this submesh should have been repaired");
    // Stored as 27,920, which is 93,456 wrapped at 65,536.
    assert_eq!(sub.index_count, 93_456);
    skin.validate(model.vertex_count()).expect("index tables");
}

/// The repair must not fire on a submesh that is already correct.
///
/// This skin has a submesh legitimately declaring 1,728 indices whose distance
/// to the next start is 67,264 -- congruent modulo 65,536, and therefore a trap
/// for a repair rule that checks congruence alone.
#[test]
fn does_not_repair_a_valid_submesh() {
    let mut chain = require_data!();
    let path = r"Environments\Stars\NexusRaid_SkyA.m2";
    let model = load(&mut chain, path);
    let skin = load_skin(&mut chain, &skin_path(path, 0));

    skin.validate(model.vertex_count()).expect("index tables");
    let sub = &skin.submeshes()[43];
    assert!(!sub.counts_repaired);
    assert_eq!(sub.index_count, 1_728);
}

/// `CreatureModelData` stores `.mdx` paths that no longer exist as files. The
/// rewrite to `.m2` has to hold for the whole table, not just the examples.
#[test]
fn every_creature_model_path_resolves() {
    let mut chain = require_data!();
    let table = dbc::schema::CreatureModelData::parse(
        &chain
            .read(dbc::schema::CreatureModelData::PATH)
            .expect("CreatureModelData"),
    )
    .expect("parse");

    let paths: Vec<String> = table
        .iter()
        .map(|row| model_path(row.model_name()))
        .filter(|p| !p.is_empty())
        .collect();
    assert!(paths.len() > 1000);

    let missing: Vec<&String> = paths.iter().filter(|p| !chain.contains(p)).collect();
    let ratio = missing.len() as f32 / paths.len() as f32;
    assert!(
        ratio < 0.02,
        "{}/{} creature models did not resolve, e.g. {:?}",
        missing.len(),
        paths.len(),
        &missing[..missing.len().min(5)]
    );
}

/// Broad structural sweep. `wow-cli m2 survey` covers all 22k; this samples so
/// the suite stays quick.
/// The attachment stride, checked by a property the format guarantees rather
/// than by the byte count.
///
/// A wrong stride still parses: it reads an id out of the middle of a float and
/// a bone index out of its low half. What it cannot do is keep producing bone
/// indices that are in range, land those bones' pivots exactly on the positions
/// it reads, and do it on every playable race at once.
#[test]
fn attachments_land_on_the_bones_they_name() {
    let mut chain = require_data!();
    let races = [
        r"Character\Human\Male\HumanMale.m2",
        r"Character\Orc\Male\OrcMale.m2",
        r"Character\NightElf\Female\NightElfFemale.m2",
        r"Character\Dwarf\Male\DwarfMale.m2",
        r"Character\Tauren\Female\TaurenFemale.m2",
    ];

    for path in races {
        let model = load(&mut chain, path);
        let bones = model.bones();
        let attachments = model.attachments();
        assert!(
            attachments.len() > 20,
            "{path}: only {} attachments",
            attachments.len()
        );

        for a in &attachments {
            let bone = bones
                .get(a.bone as usize)
                .unwrap_or_else(|| panic!("{path}: attachment {} names bone {}", a.id, a.bone));
            // Model space, not a delta from the bone -- see `Attachment`.
            for axis in 0..3 {
                assert!(
                    (a.position[axis] - bone.pivot[axis]).abs() < 1e-3,
                    "{path}: attachment {} at {:?} but bone {} pivots at {:?}",
                    a.id,
                    a.position,
                    a.bone,
                    bone.pivot
                );
            }
        }
    }
}

/// A bone on a global sequence holds its orientation in *every* animation.
///
/// The attachment points that carry stowed weapons are authored this way -- one
/// keyframe list on a shared timeline rather than one per sequence -- and
/// sampling them as `sequences[sequence]` finds data only when the sequence
/// index happens to be zero. The symptom was precise and easy to misread: a
/// sheathed sword sat correctly on the back while the character stood, and swung
/// out to point forward from his shoulder the moment he walked, because the
/// bone had silently fallen back to its bind orientation.
///
/// Asserted against the standing pose rather than against a constant: the point
/// is that the cycles *agree*, not what they agree on.
#[test]
fn a_global_sequence_bone_keeps_its_pose_across_animations() {
    let mut chain = require_data!();
    let path = r"Character\Human\Male\HumanMale.m2";
    let model = load(&mut chain, path);
    let bones = model.animated_bones();

    // The back and hip resting places, both `global_sequence` tracks.
    let carried: Vec<m2::Attachment> = model
        .attachments()
        .into_iter()
        .filter(|a| [26, 27, 32, 33].contains(&a.id))
        .collect();
    assert_eq!(carried.len(), 4, "{path}: the resting places moved");

    // Stand, Walk, Run: the first three sequences of every character model.
    let pose_at = |sequence: usize| m2::Model::pose_bones(&bones, sequence, 0);
    let standing = pose_at(0);

    for attachment in &carried {
        let bone = attachment.bone as usize;
        let track = &bones[bone].rotation;
        assert!(
            track.global_sequence.is_some(),
            "attachment {} is no longer on a global sequence, so this test \
             has stopped covering what it was written for",
            attachment.id
        );

        // Where a weapon hung here would point, in each cycle.
        let aim = |pose: &[glam::Mat4]| {
            let m = pose[bone];
            (m.transform_point3(glam::Vec3::X) - m.transform_point3(glam::Vec3::ZERO)).normalize()
        };
        let stood = aim(&standing);
        for sequence in [1, 2] {
            let moved = aim(&pose_at(sequence));
            // Generous: the torso genuinely sways as it walks and runs, and
            // the resting place sways with it. What this rules out is the
            // ninety-degree snap back to bind pose.
            let apart = stood.dot(moved).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(
                apart < 45.0,
                "attachment {} aims {apart:.0} degrees apart between standing \
                 and sequence {sequence}: the global-sequence track is being \
                 read as a per-sequence one",
                attachment.id
            );
        }
    }
}

/// The hands are a mirrored pair, and the right one is on -Y.
///
/// This is the fact the renderer hangs a weapon on, and it is worth asserting
/// separately from the stride: a client that puts every sword in the wrong hand
/// is wrong in a way no parse error will ever report. The sign convention comes
/// from the renderer's live-confirmed "an M2's forward is +X" -- drawn Z-up and
/// right-handed, the model's left is +Y.
#[test]
fn hand_attachments_mirror_across_the_body() {
    let mut chain = require_data!();
    let races = [
        r"Character\Human\Male\HumanMale.m2",
        r"Character\Orc\Male\OrcMale.m2",
        r"Character\NightElf\Female\NightElfFemale.m2",
        r"Character\Dwarf\Male\DwarfMale.m2",
        r"Character\Tauren\Female\TaurenFemale.m2",
    ];

    for path in races {
        let model = load(&mut chain, path);
        let right = model
            .attachment(m2::Attachment::HAND_RIGHT)
            .unwrap_or_else(|| panic!("{path}: no right hand"));
        let left = model
            .attachment(m2::Attachment::HAND_LEFT)
            .unwrap_or_else(|| panic!("{path}: no left hand"));

        assert!(
            right.position[1] < -0.05,
            "{path}: right hand at {:?} is not on -Y",
            right.position
        );
        assert!(
            left.position[1] > 0.05,
            "{path}: left hand at {:?} is not on +Y",
            left.position
        );
        // Mirrored: the same point on either side of the plane of symmetry.
        assert!(
            (right.position[1] + left.position[1]).abs() < 0.05,
            "{path}: hands are not mirrored: {:?} and {:?}",
            right.position,
            left.position
        );
        // Looser than the Y check on purpose. The plane of symmetry is exact,
        // but the bind pose is hand-authored and need not be: the female tauren
        // rests one arm 6cm lower than the other. A stride error misses by
        // metres, so this still separates the two.
        assert!(
            (right.position[2] - left.position[2]).abs() < 0.15,
            "{path}: hands are at different heights: {:?} and {:?}",
            right.position,
            left.position
        );
        assert_ne!(right.bone, left.bone, "{path}: both hands on one bone");
    }
}

#[test]
fn sampled_models_parse_and_validate() {
    let mut chain = require_data!();
    let names: Vec<String> = chain
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".m2"))
        .step_by(29)
        .take(700)
        .collect();

    let mut failures = Vec::new();
    let mut checked = 0;
    for name in &names {
        let Ok(bytes) = chain.read(name) else { continue };
        let model = match Model::parse(&bytes) {
            Ok(m) => m,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        checked += 1;
        for issue in model.validate() {
            failures.push(format!("{name}: {issue}"));
        }

        let path = skin_path(name, 0);
        if let Ok(sb) = chain.read(&path) {
            match Skin::parse(&sb) {
                Ok(skin) => {
                    if let Err(e) = skin.validate(model.vertex_count()) {
                        failures.push(format!("{path}: {e}"));
                    }
                }
                Err(e) => failures.push(format!("{path}: {e}")),
            }
        }
    }
    assert!(checked > 500, "sample too small: {checked}");
    assert!(
        failures.is_empty(),
        "{} failures: {:#?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

/// The torch's flame, field by field.
///
/// Pinned against one real record because every constant here was *measured*
/// off it -- the record stride by byte accounting, the colour range by the
/// ramp running orange to pale, the `M2PartTrack` timestamps by reading
/// `0, 16384, 32767`. A regression in any of those still parses cleanly and
/// produces a plausible-looking wrong flame, which is precisely the failure
/// this project's rules are about.
#[test]
fn a_torch_carries_a_flame_that_reads_as_a_flame() {
    let mut chain = require_data!();
    let path = r"ITEM\OBJECTCOMPONENTS\WEAPON\Club_1H_Torch_A_01.m2";
    let model = load(&mut chain, path);

    let emitters = model.particle_emitters();
    assert_eq!(emitters.len(), 1, "the torch has exactly one emitter");
    let flame = &emitters[0];

    // Placement: the tip of the handle, on a bone that exists.
    assert!((flame.bone as usize) < model.bones().len());
    assert!(flame.position[0] > 0.5, "at {:?}", flame.position);

    // The texture is a direct index into the model's own list, not through the
    // combo table -- read through the combos this lands on the handle's
    // runtime slot instead of the flame sheet.
    let texture = &model.textures()[flame.texture as usize];
    assert!(
        texture.filename.to_uppercase().contains("FLAMELICK"),
        "the emitter's texture is {:?}",
        texture.filename
    );

    assert_eq!(flame.emitter_type, m2::EmitterType::Plane);
    assert_eq!(flame.blend, 4, "a flame is additive");
    assert_eq!((flame.rows, flame.columns), (4, 4));

    // Per-sequence tracks, in the units they are documented in.
    let rate = flame.emission_rate.sample(0, 0).expect("an emission rate");
    assert!((rate - 20.0).abs() < 0.01, "{rate} particles a second");
    let life = flame.lifespan.sample(0, 0).expect("a lifespan");
    assert!((life - 0.8).abs() < 0.01, "{life} seconds");
    // Exactly one full turn of azimuth, which is what makes a torch's flame
    // symmetric -- and a number no misread offset produces by accident.
    let spread = flame.horizontal_range.sample(0, 0).expect("a spread");
    assert!(
        (spread - std::f32::consts::TAU).abs() < 1e-4,
        "horizontal range is {spread}, not a full turn"
    );

    // **Per-particle tracks run over a lifetime fraction, not milliseconds.**
    // The first and last keys must be at 0 and 1; read as milliseconds they
    // would be 0 and 32767.
    assert_eq!(flame.color.times.first().copied(), Some(0.0));
    assert!((flame.color.times.last().copied().unwrap() - 1.0).abs() < 1e-4);

    // And the colour is 0..255, running orange to pale. Divided by 255 this is
    // a plausible dark ramp and nothing says which was meant.
    let young = flame.color.sample(0.0).expect("a colour at birth");
    let old = flame.color.sample(1.0).expect("a colour at death");
    assert!(young[0] > 200.0 && young[2] < 20.0, "young is {young:?}");
    assert!(old[1] > 200.0 && old[2] > 150.0, "old is {old:?}");

    // Alpha fades to nothing, and scale grows then shrinks -- a flame.
    assert!(flame.alpha.sample(1.0).unwrap() < 0.01);
    let mid = flame.scale.sample(0.5).unwrap()[0];
    assert!(
        mid > flame.scale.sample(0.0).unwrap()[0] && mid > flame.scale.sample(1.0).unwrap()[0],
        "the flame does not grow and shrink: {:?}",
        flame.scale.values
    );

    assert!(model.ribbon_emitters().is_empty(), "a torch trails nothing");
}

/// A ribbon, and the one place it disagrees with a particle.
#[test]
fn a_ribbon_is_a_trail_with_a_normalised_colour() {
    let mut chain = require_data!();
    let path = r"Creature\CelestialDragonWyrm\CelestialDragonWyrm.M2";
    let model = load(&mut chain, path);

    let ribbons = model.ribbon_emitters();
    assert_eq!(ribbons.len(), 3);
    let trail = &ribbons[0];
    assert!((trail.bone as usize) < model.bones().len());
    assert!(trail.edges_per_second > 0.0 && trail.edge_lifetime > 0.0);
    assert!(!trail.textures.is_empty());
    assert!(trail
        .textures
        .iter()
        .all(|t| (*t as usize) < model.textures().len()));

    // A ribbon's colour is normalised where a particle's is 0..255. Both
    // halves, because asserting only that the ribbon is under 1.0 passes just
    // as well if every colour in the file were being read as zero.
    let colour = trail.color.sample(0, 0).expect("a colour");
    assert!(
        colour.iter().all(|c| *c <= 1.001),
        "a ribbon's colour is not normalised: {colour:?}"
    );
    assert!(
        colour.iter().any(|c| *c > 0.1),
        "the ribbon's colour read as black: {colour:?}"
    );
    let particle = &model.particle_emitters()[0];
    assert!(
        particle.color.values.iter().flatten().any(|c| *c > 1.0),
        "the same model's particle colour is not in 0..255"
    );
}

/// Every emitter in the archives names a bone and a texture that exist, and an
/// emitter type the format defines.
///
/// The survey does this over all 22,844 models; this is the sampled version
/// that runs with the suite. A wrong record stride shows up here as strays in
/// the hundreds rather than as a parse error, because nothing about an emitter
/// block is self-describing.
#[test]
fn sampled_emitters_name_things_that_exist() {
    let mut chain = require_data!();
    let names: Vec<String> = chain
        .list()
        .expect("listing")
        .into_iter()
        .filter(|n| n.to_lowercase().ends_with(".m2"))
        .collect();

    let (mut checked, mut emitters, mut strays) = (0usize, 0usize, 0usize);
    for name in names.iter().step_by(23) {
        let Ok(bytes) = chain.read(name) else { continue };
        let Ok(model) = Model::parse(&bytes) else {
            continue;
        };
        let (bones, textures) = (model.bones().len(), model.textures().len());
        let mut any = false;
        for p in model.particle_emitters() {
            any = true;
            emitters += 1;
            if p.bone as usize >= bones || p.texture as usize >= textures {
                strays += 1;
            }
            if matches!(p.emitter_type, m2::EmitterType::Unknown(_)) {
                strays += 1;
            }
        }
        for r in model.ribbon_emitters() {
            any = true;
            emitters += 1;
            if r.bone as usize >= bones
                || r.textures.iter().any(|t| *t as usize >= textures)
            {
                strays += 1;
            }
        }
        if any {
            checked += 1;
        }
    }

    // Both numbers. A sample that found no emitters at all would satisfy
    // "no strays" perfectly, which is the shape of assertion this project has
    // been caught by before.
    assert!(
        checked > 100 && emitters > 300,
        "the sample carried almost nothing to check: {checked} models, {emitters} emitters"
    );
    assert_eq!(strays, 0, "{strays} of {emitters} emitters name nothing real");
}
