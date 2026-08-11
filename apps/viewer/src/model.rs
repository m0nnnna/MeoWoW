//! Turning an archive path into something drawable.
//!
//! This is where the layers meet: `mpq` finds the files, `m2` decodes geometry,
//! `dbc` supplies the textures the model deliberately does not name, and
//! `render` uploads the result. It lives in the viewer rather than a library
//! because it is the only consumer so far; when a second one appears it should
//! move.

use anyhow::{Context, Result};
use glam::Vec3;
use mpq::Chain;
use render::mesh::{BlendMode, GpuMesh, MeshVertex, RenderState, Winding};
use render::{texture::upload_blp, Gpu, UploadedTexture};

/// One draw call: a slice of the index buffer with the state to draw it.
pub struct Draw {
    pub first_index: u32,
    pub index_count: u32,
    pub state: RenderState,
    /// Index into [`LoadedModel::textures`].
    pub texture: usize,
    pub submesh_id: u16,
}

pub struct LoadedModel {
    pub mesh: GpuMesh,
    pub draws: Vec<Draw>,
    pub textures: Vec<UploadedTexture>,
    /// Skeleton with animation tracks, kept so poses can be evaluated per
    /// frame rather than baked at load.
    pub bones: Vec<m2::AnimatedBone>,
    pub sequences: Vec<m2::Sequence>,
    /// Human-readable name per sequence, from `AnimationData.dbc`.
    pub sequence_names: Vec<String>,
    pub min: Vec3,
    pub max: Vec3,
    pub path: String,
    pub vertex_count: usize,
    pub triangle_count: usize,
    /// Textures that could not be resolved, for the overlay to report.
    pub missing_textures: Vec<String>,
}

/// Texture names supplied from outside the model, as `CreatureDisplayInfo`
/// provides them.
#[derive(Default, Clone)]
pub struct Variations(pub Vec<String>);

impl Variations {
    /// Looks up a runtime texture slot.
    ///
    /// Creature skins are types 11 to 13, mapping to the three
    /// `texture_variation` columns in order.
    fn for_kind(&self, kind: u32) -> Option<&str> {
        let slot = match kind {
            11 => 0,
            12 => 1,
            13 => 2,
            // Character body/object skins use the first variation when the
            // caller supplied one; better than a blank texture.
            1 | 2 => 0,
            _ => return None,
        };
        self.0.get(slot).map(String::as_str).filter(|s| !s.is_empty())
    }
}

/// Resolves a bare variation name against the model's own directory.
///
/// `CreatureDisplayInfo` stores `ShadowHideGnollFighterSkin`, and the file is
/// `Creature\GnollMelee\ShadowHideGnollFighterSkin.blp` -- the directory comes
/// from the model, never from the DBC.
fn variation_path(model_path: &str, name: &str) -> String {
    let dir = model_path
        .rsplit_once(['\\', '/'])
        .map(|(dir, _)| dir)
        .unwrap_or("");
    if dir.is_empty() {
        format!("{name}.blp")
    } else {
        format!("{dir}\\{name}.blp")
    }
}

/// A 1x1 white texture, so a model with unresolved slots still renders as
/// shaded geometry instead of failing to draw.
pub fn placeholder(gpu: &Gpu) -> UploadedTexture {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("placeholder"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[220, 220, 220, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    UploadedTexture {
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        texture,
        width: 1,
        height: 1,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        mip_levels: 1,
        compressed: false,
        fallback_reason: Some("placeholder"),
        bytes_uploaded: 4,
    }
}

/// Loads a model and everything it needs to draw.
pub fn load(
    gpu: &Gpu,
    chain: &mut Chain,
    path: &str,
    variations: &Variations,
    lod: u32,
) -> Result<LoadedModel> {
    let path = m2::model_path(path);
    let model = m2::Model::parse(&chain.read(&path)?)
        .with_context(|| format!("parsing {path}"))?;

    // Fall back down the LOD chain: not every model ships every level.
    let (skin, used_lod) = (lod..4)
        .chain(0..lod)
        .find_map(|l| {
            let sp = m2::skin_path(&path, l);
            let bytes = chain.read(&sp).ok()?;
            m2::Skin::parse(&bytes).ok().map(|s| (s, l))
        })
        .with_context(|| format!("no readable .skin for {path}"))?;

    skin.validate(model.vertex_count())
        .map_err(|e| anyhow::anyhow!("{path} lod {used_lod}: {e}"))?;

    // The model's whole vertex pool goes to the GPU once; batches index into
    // it, so there is no reason to split or duplicate.
    let vertices: Vec<MeshVertex> = model
        .vertices()
        .iter()
        .map(|v| MeshVertex {
            position: v.position,
            normal: v.normal,
            uv: v.uv[0],
            bone_indices: v.bone_indices,
            bone_weights: v.bone_weights,
        })
        .collect();

    let combos = model.texture_combos();
    let defs = model.textures();
    let materials = model.materials();

    // One texture per model slot, resolved once and shared by every batch.
    let mut textures = Vec::new();
    let mut missing_textures = Vec::new();
    for def in &defs {
        let file = if def.is_hardcoded() {
            Some(def.filename.clone())
        } else {
            variations
                .for_kind(def.kind)
                .map(|name| variation_path(&path, name))
        };

        let uploaded = file.as_ref().and_then(|f| {
            let bytes = chain.read(f).ok()?;
            let parsed = blp::Blp::parse(&bytes).ok()?;
            Some(upload_blp(gpu, &parsed, f))
        });

        match uploaded {
            Some(t) => textures.push(t),
            None => {
                missing_textures.push(
                    file.unwrap_or_else(|| format!("<runtime slot type {}>", def.kind)),
                );
                textures.push(placeholder(gpu));
            }
        }
    }
    if textures.is_empty() {
        textures.push(placeholder(gpu));
    }

    // Build one index buffer holding every batch back to back, so drawing is a
    // range per batch with no buffer rebinding.
    let mut indices: Vec<u32> = Vec::new();
    let mut draws: Vec<Draw> = Vec::new();
    for batch in skin.batches() {
        let Some(submesh) = skin.submeshes().get(batch.submesh_index as usize) else {
            continue;
        };
        let Some(resolved) = skin.submesh_indices(submesh) else {
            continue;
        };

        let material = materials
            .get(batch.material_index as usize)
            .copied()
            .unwrap_or(m2::Material { flags: 0, blend: 0 });
        let blend = BlendMode::from_m2(material.blend);

        let texture = combos
            .get(batch.texture_combo_index as usize)
            .map(|&t| t as usize)
            .filter(|&t| t < textures.len())
            .unwrap_or(0);

        draws.push(Draw {
            first_index: indices.len() as u32,
            index_count: resolved.len() as u32,
            state: RenderState {
                blend,
                two_sided: material.two_sided(),
                // Transparent geometry must not occlude what is behind it, and
                // the format says so per material as well.
                depth_write: !blend.is_transparent() && !material.depth_write_disabled(),
                winding: Winding::Clockwise,
            },
            texture,
            submesh_id: submesh.id,
        });
        indices.extend_from_slice(&resolved);
    }

    // Opaque first so the depth buffer is populated before anything blends
    // against it. Within each group the authored order is kept: M2 batches are
    // ordered deliberately, and priority_plane refines it.
    draws.sort_by_key(|d| {
        (
            d.state.blend.is_transparent(),
            d.state.blend == BlendMode::Additive,
        )
    });

    // Bounds from the vertices actually drawn, not the header's box. The
    // header box is a culling volume that also covers animation extents, so
    // framing against it leaves a static pose small and off-centre.
    let (min, max) = vertices.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(min, max), v| {
            let p = Vec3::from(v.position);
            (min.min(p), max.max(p))
        },
    );
    if vertices.is_empty() || indices.is_empty() {
        anyhow::bail!("{path} produced no drawable geometry");
    }

    let (min, max) = if vertices.is_empty() {
        let (a, b) = model.bounding_box();
        (Vec3::from(a), Vec3::from(b))
    } else {
        (min, max)
    };

    let triangle_count = indices.len() / 3;
    let sequences = model.sequences();
    let external = load_external_anims(chain, &path, &sequences);
    let bones = model.animated_bones_with(&external);
    let sequence_names = sequence_names(chain, &sequences);

    Ok(LoadedModel {
        mesh: GpuMesh::upload(gpu, &vertices, &indices),
        draws,
        textures,
        bones,
        sequences,
        sequence_names,
        min,
        max,
        path,
        vertex_count: vertices.len(),
        triangle_count,
        missing_textures,
    })
}

/// Loads the `.anim` files holding keyframes that are not inline in the `.m2`.
///
/// A sequence without `is_inline` has no usable data in the model; its offsets
/// address the external file. Missing files are skipped rather than fatal --
/// aliases legitimately have none, and the loader falls back to bind pose.
fn load_external_anims(
    chain: &mut Chain,
    model_path: &str,
    sequences: &[m2::Sequence],
) -> std::collections::BTreeMap<usize, Vec<u8>> {
    sequences
        .iter()
        .enumerate()
        .filter(|(_, seq)| !seq.is_inline())
        .filter_map(|(i, seq)| {
            let path = m2::anim::external_anim_path(model_path, seq);
            chain.read(&path).ok().map(|bytes| (i, bytes))
        })
        .collect()
}

/// Resolves each sequence's numeric animation id to a name.
///
/// Names are per-id, and models routinely ship several variations of the same
/// animation, so the variation index is appended to keep entries distinct in a
/// picker.
fn sequence_names(chain: &mut Chain, sequences: &[m2::Sequence]) -> Vec<String> {
    let table = chain
        .read(dbc::schema::AnimationData::PATH)
        .ok()
        .and_then(|b| dbc::schema::AnimationData::parse(&b).ok());

    sequences
        .iter()
        .map(|seq| {
            let name = table
                .as_ref()
                .and_then(|t| t.iter().find(|r| r.id() == seq.id as u32))
                .map(|r| r.name().to_string())
                .unwrap_or_else(|| format!("#{}", seq.id));
            if seq.variation == 0 {
                name
            } else {
                format!("{name} ({})", seq.variation)
            }
        })
        .collect()
}

/// Looks up the model and skins for a creature display id.
pub fn creature(chain: &mut Chain, display_id: u32) -> Result<(String, Variations)> {
    use dbc::schema::{CreatureDisplayInfo, CreatureModelData};

    let display = CreatureDisplayInfo::parse(&chain.read(CreatureDisplayInfo::PATH)?)?;
    let models = CreatureModelData::parse(&chain.read(CreatureModelData::PATH)?)?;

    let row = display
        .iter()
        .find(|d| d.id() == display_id)
        .with_context(|| format!("no CreatureDisplayInfo row {display_id}"))?;
    let model_row = models
        .iter()
        .find(|m| m.id() == row.model_id())
        .with_context(|| format!("no CreatureModelData row {}", row.model_id()))?;

    let variations = Variations(vec![
        row.texture_variation_0().to_string(),
        row.texture_variation_1().to_string(),
        row.texture_variation_2().to_string(),
    ]);
    Ok((m2::model_path(model_row.model_name()), variations))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_variations_against_the_model_directory() {
        assert_eq!(
            variation_path(
                r"Creature\GnollMelee\GnollMelee.m2",
                "ShadowHideGnollFighterSkin"
            ),
            r"Creature\GnollMelee\ShadowHideGnollFighterSkin.blp"
        );
        assert_eq!(variation_path("Loose.m2", "Skin"), "Skin.blp");
    }

    #[test]
    fn maps_creature_texture_slots_in_order() {
        let v = Variations(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(v.for_kind(11), Some("a"));
        assert_eq!(v.for_kind(12), Some("b"));
        assert_eq!(v.for_kind(13), Some("c"));
        assert_eq!(v.for_kind(7), None);
    }

    #[test]
    fn empty_variations_do_not_resolve() {
        let v = Variations(vec![String::new()]);
        assert_eq!(v.for_kind(11), None);
    }
}
