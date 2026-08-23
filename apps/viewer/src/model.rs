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
use std::time::{Duration, Instant};

/// Where the time went loading one model.
///
/// Every field is measured rather than attributed: a first-time creature load
/// on the render thread is the whole of a visible stutter, and three separate
/// guesses at *which part* of it was expensive were wrong before anything was
/// timed. The rule this pays for is `CLAUDE.md`'s "measure the thing, not the
/// thing next to it" -- the confident diagnosis of the thirty-seven-second
/// login named a `Spell.dbc` read that turned out to take 185ms.
///
/// Carried on the loaded model rather than logged here so the caller can print
/// one line naming the display id, which is the thing that identifies *which*
/// creature cost the frame.
#[derive(Default, Clone, Copy)]
pub struct LoadTimings {
    /// Reading the `.m2` out of the archives -- I/O and decompression.
    pub model_read: Duration,
    /// Parsing those bytes.
    pub model_parse: Duration,
    /// Finding and parsing a readable `.skin`, including the LOD walk.
    pub skin: Duration,
    /// Building the vertex pool and walking the batches into one index
    /// buffer, including the geoset filter a dressed model applies.
    pub geometry: Duration,
    /// Resolving, decoding and uploading every texture slot.
    pub textures: Duration,
    /// Reading the `.anim` files whose keyframes are not inline. A character
    /// model has over a hundred sequences and most of them are external, so
    /// this is one archive read and one decompression *each*.
    pub external_anims: Duration,
    /// How many of those files were actually read.
    pub external_anim_files: usize,
    /// Re-reading and re-parsing `AnimationData.dbc`, which happens once per
    /// model load and holds nothing but the human-readable sequence names.
    pub sequence_names: Duration,
    /// Resolving the bone tracks against those files, and the timed events
    /// the footfall list is read from.
    pub skeleton: Duration,
    /// Uploading the vertex and index buffers.
    pub upload: Duration,
    /// Attachments, emitters and the collision mesh -- everything read off
    /// the parsed model after the drawable part is finished.
    pub extras: Duration,
    /// **Not the sum of the fields above.** They are measured, not
    /// attributed, so a gap between this and their total is unaccounted work
    /// rather than a rounding error -- which is a finding, and the reason
    /// this is recorded separately instead of being computed by the reader.
    pub total: Duration,
}

impl LoadTimings {
    /// The breakdown as one line, in descending order of what has actually
    /// been seen to matter.
    ///
    /// Ends with what none of the phases claimed. **Printing that number is
    /// the point**: a breakdown whose parts sum to less than its total looks
    /// complete and is not, and the missing third is exactly where the next
    /// wrong guess would go.
    pub fn summary(&self) -> String {
        let named = self.model_read
            + self.model_parse
            + self.skin
            + self.geometry
            + self.textures
            + self.external_anims
            + self.skeleton
            + self.sequence_names
            + self.upload
            + self.extras;
        format!(
            "m2 read {:?} + parse {:?}, skin {:?}, geometry {:?}, \
             textures {:?}, {} anim file(s) {:?}, skeleton {:?}, names {:?}, \
             upload {:?}, extras {:?}, unaccounted {:?}",
            self.model_read,
            self.model_parse,
            self.skin,
            self.geometry,
            self.textures,
            self.external_anim_files,
            self.external_anims,
            self.skeleton,
            self.sequence_names,
            self.upload,
            self.extras,
            self.total.saturating_sub(named),
        )
    }
}

/// One draw call: a slice of the index buffer with the state to draw it.
pub struct Draw {
    pub first_index: u32,
    pub index_count: u32,
    pub state: RenderState,
    /// Index into [`LoadedModel::textures`].
    pub texture: usize,
    pub texture_transform: Option<usize>,
    pub submesh_id: u16,
}

pub struct TextureAnimation {
    transforms: std::rc::Rc<Vec<m2::AnimatedTextureTransform>>,
    global_sequences: std::rc::Rc<Vec<u32>>,
    gpu: render::mesh::TextureTransformBuffer,
}

impl TextureAnimation {
    pub fn new(gpu: &Gpu, meshes: &render::mesh::MeshRenderer, model: &m2::Model, draws: &[Draw]) -> Self {
        let transforms = std::rc::Rc::new(model.animated_texture_transforms());
        let global_sequences = std::rc::Rc::new(model.global_sequence_durations());
        let indices = draws
            .iter()
            .map(|draw| draw.texture_transform.map_or(0, |index| index.saturating_add(1)))
            .collect::<Vec<_>>();
        let gpu = meshes.create_texture_transforms(gpu, transforms.len().saturating_add(1), &indices);
        Self { transforms, global_sequences, gpu }
    }

    pub fn empty(gpu: &Gpu, meshes: &render::mesh::MeshRenderer, draws: usize) -> Self {
        let gpu = meshes.create_texture_transforms(gpu, 1, &vec![0; draws]);
        Self {
            transforms: std::rc::Rc::new(Vec::new()),
            global_sequences: std::rc::Rc::new(Vec::new()),
            gpu,
        }
    }

    pub fn update(&self, gpu: &Gpu, meshes: &render::mesh::MeshRenderer, sequence: usize, time_ms: u32) {
        let mut matrices = Vec::with_capacity(self.transforms.len().saturating_add(1));
        matrices.push(glam::Mat4::IDENTITY.to_cols_array_2d());
        matrices.extend(self
            .transforms
            .iter()
            .map(|transform| {
                transform
                    .matrix(sequence, time_ms, &self.global_sequences)
                    .to_cols_array_2d()
            })
        );
        meshes.update_texture_transforms(gpu, &self.gpu, &matrices);
    }

    pub fn is_animated(&self) -> bool {
        self.transforms.iter().any(m2::AnimatedTextureTransform::is_animated)
    }

    pub fn global_sequences(&self) -> &[u32] {
        &self.global_sequences
    }

    pub fn bind(&self, draw: usize) -> Option<&wgpu::BindGroup> {
        self.gpu.binds.get(draw)
    }
}

pub struct LoadedModel {
    pub mesh: GpuMesh,
    pub draws: Vec<Draw>,
    pub textures: Vec<UploadedTexture>,
    pub texture_animation: TextureAnimation,
    /// Skeleton with animation tracks, kept so poses can be evaluated per
    /// frame rather than baked at load.
    pub bones: std::rc::Rc<Vec<m2::AnimatedBone>>,
    pub sequences: Vec<m2::Sequence>,
    /// Points other models hang from. Cheap to carry -- a character model has
    /// thirty-nine and a tree none -- and the only way to hang a weapon on a
    /// hand after the file has been dropped.
    pub attachments: Vec<m2::Attachment>,
    /// What this model burns or sprays, and what it trails. Empty for the
    /// great majority: 6,429 models of 22,844 carry a particle emitter and 317
    /// carry a ribbon. Carried alongside the geometry for the same reason the
    /// attachments are -- the file is gone by the time anything wants to emit,
    /// and re-reading it per placement would parse a torch once per torch.
    pub particles: Vec<m2::ParticleEmitter>,
    pub ribbons: Vec<m2::RibbonEmitter>,
    /// When this model's feet hit the ground, one sorted list of
    /// milliseconds per sequence.
    ///
    /// Read from the model's own timed events -- see [`m2::event`], which
    /// carries the measurement that identified them. Empty for everything with
    /// no legs, which is most models, and empty *per sequence* for the cycles
    /// a creature does not walk in.
    pub footfalls: std::rc::Rc<Vec<Vec<u32>>>,
    /// Human-readable name per sequence, from `AnimationData.dbc`.
    pub sequence_names: Vec<String>,
    pub min: Vec3,
    pub max: Vec3,
    pub path: String,
    pub vertex_count: usize,
    pub triangle_count: usize,
    /// Textures that could not be resolved, for the overlay to report.
    pub missing_textures: Vec<String>,
    /// The model's own collision mesh, in model space -- a far coarser thing
    /// than the drawn geometry, and empty for anything meant to be walked
    /// through. See `m2::Model::collision_triangles`.
    pub collision: Vec<[[f32; 3]; 3]>,
    /// Where the time went. See [`LoadTimings`].
    pub timings: LoadTimings,
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

/// The parts of a model that do not depend on how it is dressed, kept once
/// per file instead of once per display id.
///
/// **This exists because of a measurement, and the measurement is the whole
/// argument.** Creature models are cached by display id -- they have to be,
/// since a display id supplies the skins -- but every humanoid NPC in the game
/// is `Character\Human\Male\HumanMale.m2` and its forty-seven `.anim` files.
/// Loading Marshal McBride and then Deputy Willem read the identical bytes out
/// of the archives twice and decoded the identical bone tracks twice:
///
/// ```text
/// display 2072 (HumanMale.m2) loaded in 28.9ms
///   (m2 read 10.2ms + parse 0.4ms, 47 anim file(s) 6.5ms, skeleton 9.3ms, ...)
/// ```
///
/// Two thirds of that is the archive reads and the track decode, and both are
/// a pure function of the path. A kobold, whose model is its own, costs 3.8ms
/// -- so the stutter was never "loading a creature is expensive", it was
/// "loading the *same* creature once per costume".
///
/// Deliberately a parameter rather than a global: it is a cache with a budget
/// and the thing that owns the other caches should own this one too. See
/// `CLAUDE.md` -- a parameter that can be passed wrong is still better than
/// one nobody can see.
/// Everything one model's `.anim` files are decoded into, shared by every
/// costume that model wears.
#[derive(Clone)]
struct Skeleton {
    bones: std::rc::Rc<Vec<m2::AnimatedBone>>,
    footfalls: std::rc::Rc<Vec<Vec<u32>>>,
    /// How many external files went into it, kept only so the load line can
    /// still report the number on a cache hit -- where none were read.
    files: usize,
}

#[derive(Default)]
pub struct Sources {
    /// Raw archive bytes, keyed by path. Holds `.m2` and `.anim` files only:
    /// everything else is either read once (the DBCs) or already cached at a
    /// higher level (a doodad's model, by path).
    files: std::collections::HashMap<String, std::rc::Rc<Vec<u8>>>,
    /// Paths the archives do not hold.
    ///
    /// **A negative is an answer and is cached like one.** Five of a character
    /// model's `.anim` paths do not exist -- they are alias sequences, which is
    /// ordinary -- and without this the question is asked again for every
    /// costume, exactly the retry-the-failure shape `World::cache` already
    /// stores a `None` to avoid. Archives do not change while the client runs,
    /// so a miss is permanent.
    absent: std::collections::HashSet<String>,
    /// Decoded bone tracks and footfall events, keyed by model path. Shared
    /// rather than cloned -- a character's skeleton is a hundred and fifty
    /// sequences of keyframes per bone, and copying it per display id would
    /// trade a decode for a memcpy rather than removing the work.
    ///
    /// The two are cached *together* because they are decoded from the same
    /// `.anim` files. Caching only the bones would keep reading those files
    /// for the events, which is the bigger half of the read.
    skeletons: std::collections::HashMap<String, Skeleton>,
    /// `AnimationData.dbc`, which supplies nothing but human-readable sequence
    /// names and was re-read and re-parsed on every single model load.
    anim_names: Option<Option<dbc::schema::AnimationData>>,
    /// The two tables that turn a display id into a model path and its skins.
    ///
    /// Read once. They were read and parsed *per creature* -- 24,262 rows of
    /// `CreatureDisplayInfo` and the whole of `CreatureModelData`, before a
    /// single byte of the model itself -- which is where the twenty-odd
    /// milliseconds left over after the model cache went. `None` means the
    /// read was attempted and failed, so a broken installation is reported
    /// once rather than retried per creature.
    creature_tables: Option<
        Option<(dbc::schema::CreatureDisplayInfo, dbc::schema::CreatureModelData)>,
    >,
    bytes_held: usize,
    hits: usize,
    misses: usize,
}

/// How much raw archive data [`Sources`] will hold before dropping the lot.
///
/// Crude on purpose. An LRU here would be machinery in service of a case
/// nobody has hit: the set being cached is *character and creature models in
/// view*, which is tens of files, and the cap exists so a long session
/// wandering through every zone in the game cannot grow without bound. It
/// says so in the log when it fires, which is the part that matters -- a cache
/// that silently started missing would look exactly like the fix regressing.
const SOURCE_BYTE_BUDGET: usize = 64 * 1024 * 1024;

impl Sources {
    /// Reads a file, from memory if it has been read before.
    fn read(&mut self, chain: &mut Chain, path: &str) -> Result<std::rc::Rc<Vec<u8>>> {
        if let Some(bytes) = self.files.get(path) {
            self.hits += 1;
            return Ok(bytes.clone());
        }
        if self.absent.contains(path) {
            self.hits += 1;
            anyhow::bail!("{path} is not in the archives (remembered from an earlier look)");
        }
        self.misses += 1;
        let bytes = match chain.read(path) {
            Ok(bytes) => std::rc::Rc::new(bytes),
            Err(e) => {
                self.absent.insert(path.to_string());
                return Err(e.into());
            }
        };
        if self.bytes_held + bytes.len() > SOURCE_BYTE_BUDGET {
            tracing::info!(
                "model source cache full at {} file(s), {:.1} MiB -- clearing",
                self.files.len(),
                self.bytes_held as f64 / (1024.0 * 1024.0),
            );
            self.files.clear();
            self.skeletons.clear();
            // Not `absent` -- it holds no bytes, and a path that was missing
            // before the flush is still missing after it.
            self.bytes_held = 0;
        }
        self.bytes_held += bytes.len();
        self.files.insert(path.to_string(), bytes.clone());
        Ok(bytes)
    }

    /// The same, for a file whose absence is ordinary -- a missing `.anim` is
    /// an alias sequence, not a failure. See [`load_external_anims`].
    fn read_optional(&mut self, chain: &mut Chain, path: &str) -> Option<std::rc::Rc<Vec<u8>>> {
        self.read(chain, path).ok()
    }

    /// `CreatureDisplayInfo` and `CreatureModelData`, read on first use.
    fn creature_tables(
        &mut self,
        chain: &mut Chain,
    ) -> Result<&(dbc::schema::CreatureDisplayInfo, dbc::schema::CreatureModelData)> {
        use dbc::schema::{CreatureDisplayInfo, CreatureModelData};

        self.creature_tables
            .get_or_insert_with(|| {
                let display = chain
                    .read(CreatureDisplayInfo::PATH)
                    .ok()
                    .and_then(|b| CreatureDisplayInfo::parse(&b).ok())?;
                let models = chain
                    .read(CreatureModelData::PATH)
                    .ok()
                    .and_then(|b| CreatureModelData::parse(&b).ok())?;
                Some((display, models))
            })
            .as_ref()
            .context("CreatureDisplayInfo or CreatureModelData could not be read")
    }

    /// Hits and misses since the last call, for a caller that wants to say
    /// whether the cache is doing anything.
    ///
    /// **Both numbers, always.** A hit count alone cannot tell "everything was
    /// already loaded" from "nothing is ever loaded twice", and those want
    /// opposite investigations.
    pub fn counts(&self) -> (usize, usize) {
        (self.hits, self.misses)
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
///
/// Takes its own [`Sources`], because every caller of this one loads a
/// distinct path exactly once -- a doodad is already cached by path a level
/// up, so there is nothing for a shared cache to hit. The dressed path is the
/// one that reads a file per *costume*; see [`load_dressed_with`].
pub fn load(
    gpu: &Gpu,
    meshes: &render::mesh::MeshRenderer,
    chain: &mut Chain,
    path: &str,
    variations: &Variations,
    lod: u32,
) -> Result<LoadedModel> {
    load_dressed(gpu, meshes, chain, path, variations, lod, None)
}

/// The same as [`load_dressed_with`], with a cache that lives for one call.
pub fn load_dressed(
    gpu: &Gpu,
    meshes: &render::mesh::MeshRenderer,
    chain: &mut Chain,
    path: &str,
    variations: &Variations,
    lod: u32,
    look: Option<&crate::character::Look>,
) -> Result<LoadedModel> {
    let mut sources = Sources::default();
    load_dressed_with(gpu, meshes, chain, &mut sources, path, variations, lod, look)
}

/// The same, for a model whose textures and geosets come from a character's
/// own appearance rather than from `CreatureDisplayInfo`.
///
/// Split as an extra parameter rather than a second loader because everything
/// else -- the LOD fallback, the batch walk, the bone palette -- is identical,
/// and two copies of that would drift. See [`crate::character`] for why a
/// player needs it at all.
pub fn load_dressed_with(
    gpu: &Gpu,
    meshes: &render::mesh::MeshRenderer,
    chain: &mut Chain,
    sources: &mut Sources,
    path: &str,
    variations: &Variations,
    lod: u32,
    look: Option<&crate::character::Look>,
) -> Result<LoadedModel> {
    let began = Instant::now();
    let mut timings = LoadTimings::default();

    let path = m2::model_path(path);
    // Split because the two have completely different fixes: an archive read
    // is I/O and decompression, which a byte cache removes, and a parse is CPU
    // over bytes already in hand, which only a parsed-object cache removes.
    // Timing them together would have picked one of those at random -- and the
    // answer was 10.2ms against 0.4ms, which is not a tie.
    let bytes = sources.read(chain, &path)?;
    timings.model_read = began.elapsed();
    let phase = Instant::now();
    let model = m2::Model::parse(&bytes).with_context(|| format!("parsing {path}"))?;
    timings.model_parse = phase.elapsed();

    // Fall back down the LOD chain: not every model ships every level.
    let phase = Instant::now();
    let (skin, used_lod) = (lod..4)
        .chain(0..lod)
        .find_map(|l| {
            let sp = m2::skin_path(&path, l);
            let bytes = sources.read_optional(chain, &sp)?;
            m2::Skin::parse(&bytes).ok().map(|s| (s, l))
        })
        .with_context(|| format!("no readable .skin for {path}"))?;
    timings.skin = phase.elapsed();

    skin.validate(model.vertex_count())
        .map_err(|e| anyhow::anyhow!("{path} lod {used_lod}: {e}"))?;

    // The model's whole vertex pool goes to the GPU once; batches index into
    // it, so there is no reason to split or duplicate.
    let phase = Instant::now();
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

    timings.geometry = phase.elapsed();

    let combos = model.texture_combos();
    let texture_transform_combos = model.texture_transform_combos();
    let defs = model.textures();
    let materials = model.materials();

    // One texture per model slot, resolved once and shared by every batch.
    let phase = Instant::now();
    let mut textures = Vec::new();
    let mut missing_textures = Vec::new();
    for def in &defs {
        let file = if def.is_hardcoded() {
            Some(def.filename.clone())
        } else {
            // A character's own textures are full archive paths already --
            // `CharSections` stores `Character\Human\Male\HumanMaleSkin00_00`
            // -- where a creature's are bare names resolved against the
            // model's directory. Resolving one like the other produces a path
            // that does not exist and a silently untextured model.
            let dressed = look.and_then(|look| match def.kind {
                1 => look.body.clone(),
                6 => look.hair.clone(),
                _ => None,
            });
            dressed.or_else(|| {
                variations
                    .for_kind(def.kind)
                    .map(|name| variation_path(&path, name))
            })
        };

        // A character's body texture is composed in memory from several
        // layers and has no file behind it, so it is uploaded from pixels
        // before any path is considered.
        let composed = look
            .filter(|_| def.kind == 1)
            .and_then(|look| look.skin.as_ref())
            .map(|skin| {
                render::texture::upload_rgba(
                    gpu,
                    skin.width,
                    skin.height,
                    &skin.rgba,
                    "character skin",
                )
            });
        let uploaded = composed.or_else(|| {
            file.as_ref().and_then(|f| {
                let bytes = chain.read(f).ok()?;
                let parsed = blp::Blp::parse(&bytes).ok()?;
                Some(upload_blp(gpu, &parsed, f))
            })
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
    timings.textures = phase.elapsed();

    // Build one index buffer holding every batch back to back, so drawing is a
    // range per batch with no buffer rebinding.
    let phase = Instant::now();
    let mut indices: Vec<u32> = Vec::new();
    let mut draws: Vec<Draw> = Vec::new();
    for batch in skin.batches() {
        let Some(submesh) = skin.submeshes().get(batch.submesh_index as usize) else {
            continue;
        };
        // A character model ships every hairstyle and beard at once and
        // expects the client to pick. Skipped here rather than drawn with a
        // transparent material: an unwanted geoset costs a draw call and
        // overlapping geometry either way.
        if look.is_some_and(|look| !look.shows(u32::from(submesh.id))) {
            continue;
        }
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
                // `OWC_NO_CULL=1` draws every triangle regardless of which
                // way it faces. Kept as a diagnostic: it is how the winding
                // below was identified, by making "the front of the pillar is
                // missing and I can see inside it" disappear -- which proved
                // the geometry was there all along and only the facing test
                // was rejecting it.
                two_sided: material.two_sided()
                    || std::env::var_os("OWC_NO_CULL").is_some(),
                // Transparent geometry must not occlude what is behind it, and
                // the format says so per material as well.
                depth_write: !blend.is_transparent() && !material.depth_write_disabled(),
                // **Counter-clockwise, despite what the rest of this
                // project long assumed.** `docs/RENDERING.md` said M2 winds
                // clockwise and WMO counter-clockwise; for M2 that was wrong,
                // and it culled every front-facing triangle. The symptom is
                // not a missing model -- it is a model you can see *into*,
                // because what survives culling is the far side's interior.
                //
                // It hid for as long as it did because an inside-out model
                // still has a silhouette, a texture and a size, and reads at
                // a glance as a model facing away from you. That is almost
                // certainly why entity facing looked wrong at the same time:
                // two bugs producing one symptom.
                winding: Winding::CounterClockwise,
            },
            texture,
            texture_transform: texture_transform_combos
                .get(batch.texture_transform_combo_index as usize)
                .copied()
                .filter(|&index| index != u16::MAX)
                .map(usize::from),
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
    // **A model with no triangles is not necessarily an empty model.** 653 of
    // the 6,500 emitter-carrying models in the archives have no vertices at
    // all: their entire content is a flame or a spray, and a campfire's fire
    // is exactly that -- a doodad with a particle emitter and no mesh. This
    // check refused every one of them, silently, for as long as it has
    // existed, and nothing said so because "the model failed to load" and "the
    // model has nothing in it" produce the same empty patch of ground.
    //
    // It still refuses a model that genuinely draws nothing, which is the case
    // it was written for: a parse that produced no batches is a bug worth
    // hearing about.
    let emits = !model.particle_emitters().is_empty() || !model.ribbon_emitters().is_empty();
    if (vertices.is_empty() || indices.is_empty()) && !emits {
        anyhow::bail!("{path} produced no drawable geometry");
    }

    let (min, max) = if vertices.is_empty() {
        let (a, b) = model.bounding_box();
        (Vec3::from(a), Vec3::from(b))
    } else {
        (min, max)
    };

    let triangle_count = indices.len() / 3;
    timings.geometry += phase.elapsed();

    let texture_animation = TextureAnimation::new(gpu, meshes, &model, &draws);

    let sequences = model.sequences();
    // The whole skeleton -- the `.anim` reads, the bone tracks and the timed
    // events -- resolved once per *path*. On a hit none of the files below are
    // opened at all, which is the point: they are keyframes, and keyframes do
    // not depend on what the model is wearing.
    let phase = Instant::now();
    let skeleton = match sources.skeletons.get(&path) {
        Some(skeleton) => skeleton.clone(),
        None => {
            let read = Instant::now();
            let external = load_external_anims(sources, chain, &path, &sequences);
            timings.external_anims = read.elapsed();
            // Read here rather than at the struct literal because that runs
            // after `sequences` has moved into it, and the outer index of a
            // footfall list *is* a sequence index.
            let skeleton = Skeleton {
                footfalls: std::rc::Rc::new(m2::event::footfalls(
                    &model.events_with(&external),
                    sequences.len(),
                )),
                bones: std::rc::Rc::new(model.animated_bones_with(&external)),
                files: external.len(),
            };
            sources.skeletons.insert(path.clone(), skeleton.clone());
            skeleton
        }
    };
    timings.external_anim_files = skeleton.files;
    let (bones, footfalls) = (skeleton.bones, skeleton.footfalls);
    // Net of the reads already attributed above, so the two do not double-count
    // on a miss and `skeleton` reads as ~0 on a hit.
    timings.skeleton = phase.elapsed().saturating_sub(timings.external_anims);
    let phase = Instant::now();
    let sequence_names = sequence_names(sources, chain, &sequences);
    timings.sequence_names = phase.elapsed();

    // Hoisted out of the struct literal below purely so it can be timed: a
    // buffer upload is the one part of this that touches the GPU, and telling
    // it apart from the archive reads is the difference between "load this off
    // the render thread" and "there is nothing to move".
    let phase = Instant::now();
    let mesh = if vertices.is_empty() || indices.is_empty() {
        // A zero-length GPU buffer is not something wgpu will make, and an
        // emitter-only model has exactly that. One degenerate vertex costs
        // nothing and keeps every downstream `set_vertex_buffer` working
        // without a branch: `draws` is empty, so it is never drawn.
        GpuMesh::upload(
            gpu,
            &[MeshVertex {
                position: [0.0; 3],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0; 2],
                bone_indices: [0; 4],
                bone_weights: [0; 4],
            }],
            &[0, 0, 0],
        )
    } else {
        GpuMesh::upload(gpu, &vertices, &indices)
    };
    timings.upload = phase.elapsed();

    // Hoisted for the same reason the upload was. `collision_triangles` walks
    // a second mesh nothing draws, and an emitter list is parsed per model --
    // both invisible in a breakdown that stopped at the struct literal.
    let phase = Instant::now();
    let attachments = model.attachments();
    let particles = model.particle_emitters();
    let ribbons = model.ribbon_emitters();
    let collision = model.collision_triangles();
    timings.extras = phase.elapsed();
    timings.total = began.elapsed();

    Ok(LoadedModel {
        mesh,
        draws,
        textures,
        texture_animation,
        bones,
        sequences,
        attachments,
        particles,
        ribbons,
        footfalls,
        collision,
        sequence_names,
        min,
        max,
        path,
        vertex_count: vertices.len(),
        triangle_count,
        missing_textures,
        timings,
    })
}

/// Loads the `.anim` files holding keyframes that are not inline in the `.m2`.
///
/// A sequence without `is_inline` has no usable data in the model; its offsets
/// address the external file. Missing files are skipped rather than fatal --
/// aliases legitimately have none, and the loader falls back to bind pose.
fn load_external_anims(
    sources: &mut Sources,
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
            // Copied out of the cache rather than shared, because
            // `animated_bones_with` wants a map it owns -- and that copy is
            // paid only when the bones are actually decoded, which the cache
            // above makes a once-per-path event.
            sources.read_optional(chain, &path).map(|bytes| (i, (*bytes).clone()))
        })
        .collect()
}

/// Resolves each sequence's numeric animation id to a name.
///
/// Names are per-id, and models routinely ship several variations of the same
/// animation, so the variation index is appended to keep entries distinct in a
/// picker.
fn sequence_names(
    sources: &mut Sources,
    chain: &mut Chain,
    sequences: &[m2::Sequence],
) -> Vec<String> {
    // Held across loads rather than re-read per model. It supplies nothing but
    // display strings, and it was being read and parsed from the archives once
    // for every creature that came into view.
    let table = sources.anim_names.get_or_insert_with(|| {
        chain
            .read(dbc::schema::AnimationData::PATH)
            .ok()
            .and_then(|b| dbc::schema::AnimationData::parse(&b).ok())
    });

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
pub fn creature(
    sources: &mut Sources,
    chain: &mut Chain,
    display_id: u32,
) -> Result<(String, Variations)> {
    let (display, models) = sources.creature_tables(chain)?;

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

    /// Two humanoid display ids must not read the same model twice.
    ///
    /// **This is the stutter, as a test.** Every humanoid NPC in the game is
    /// `HumanMale.m2` and its forty-seven `.anim` files, the entity cache is
    /// keyed by display id because a display id supplies the *skins*, and the
    /// two facts together had the client re-reading and re-decoding an
    /// identical skeleton once per costume -- 25 to 30ms on the render thread
    /// for each new NPC that walked into view.
    ///
    /// Asserted as a *count of archive reads* rather than as elapsed time: a
    /// timing assertion on a machine under load is a flaky test, and the thing
    /// that was actually wrong was the repetition, not the duration.
    ///
    /// Both display ids are Elwynn humans -- Marshal McBride and Deputy
    /// Willem, the same two fixtures the gossip work used. Skipped without
    /// `WOW_DATA`.
    #[test]
    fn two_costumes_of_one_model_read_the_archives_once() {
        let Some(data) = std::env::var_os("WOW_DATA") else {
            eprintln!("skipping: WOW_DATA not set");
            return;
        };
        let mut chain = Chain::open_wow_data(data, "enUS").expect("opening archives");
        let mut sources = Sources::default();

        let (first, _) = creature(&mut sources, &mut chain, 1859).expect("Marshal McBride");
        let (_, misses_after_first) = sources.counts();
        assert_eq!(
            sources.counts().0,
            0,
            "nothing can have been a cache hit before anything was cached"
        );

        // The skeleton is only decoded by a full load, which needs a GPU. The
        // reads are not: resolving the path and pulling the model and its
        // `.anim` files is the whole of the archive traffic, and that is what
        // is being counted.
        let sequences = {
            let bytes = sources.read(&mut chain, &first).expect("HumanMale.m2");
            m2::Model::parse(&bytes).expect("parsing").sequences()
        };
        load_external_anims(&mut sources, &mut chain, &first, &sequences);
        let (_, misses_after_reads) = sources.counts();
        assert!(
            misses_after_reads > misses_after_first,
            "the first load should have read files it had never seen"
        );

        let (second, _) = creature(&mut sources, &mut chain, 2072).expect("Deputy Willem");
        assert_eq!(
            first, second,
            "these two NPCs are the same model wearing different skins; \
             if that stopped being true the test no longer tests anything"
        );
        let before = sources.counts();
        let bytes = sources.read(&mut chain, &second).expect("HumanMale.m2 again");
        m2::Model::parse(&bytes).expect("parsing");
        load_external_anims(&mut sources, &mut chain, &second, &sequences);
        let after = sources.counts();

        assert_eq!(
            after.1, before.1,
            "the second costume read {} file(s) that the first had already read",
            after.1 - before.1
        );
        assert!(
            after.0 > before.0,
            "the second costume should have hit the cache and did not"
        );
    }
}
