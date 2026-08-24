//! A directional shadow map: the world's depth, rendered from the sun.
//!
//! Phase 2 left shadows out on purpose -- `docs/ROADMAP.md` filed them beside
//! liquid and portal culling as "easier to judge once there is a character
//! standing in the world". There is one now, and standing in an open field at
//! four in the afternoon with no shadow under it is the single loudest thing
//! left in an outdoor render.
//!
//! **Everything here casts and nothing here is clever.** One orthographic
//! frustum follows the camera, and the whole resident world is drawn into it
//! every frame. That is deliberately the naive arrangement: cascades, caster
//! culling and a tighter fit are all real improvements and all of them are
//! optimisations of something that has to be correct first. What it does buy
//! is that the shadow of a building spanning nine tiles cannot go missing,
//! which the obvious tile-based culling would have arranged -- a world object
//! is filed under the tile containing its *origin*, so Stormwind belongs to
//! one tile and reaches into eight others.
//!
//! Casters are the opaque and alpha-keyed batches only. A blended batch is
//! glass, a glow or a spray, and giving a torch flame a solid shadow is worse
//! than giving it none.

use bytemuck::{Pod, Zeroable};

use crate::mesh::{Instance, MeshVertex};
use crate::Gpu;

/// Depth format for the map. Matches the scene's, so nothing has to think
/// about two depth conventions at once.
pub const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightUniform {
    view_proj: [[f32; 4]; 4],
}

const SHADER: &str = r#"
struct Light {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> light: Light;
@group(1) @binding(0) var<storage, read> bones: array<mat4x4<f32>>;
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(2) @binding(1) var samp: sampler;

struct MeshIn {
    @location(0) position: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) bone_indices: vec4<u32>,
    @location(4) bone_weights: vec4<f32>,
    @location(5) model_0: vec4<f32>,
    @location(6) model_1: vec4<f32>,
    @location(7) model_2: vec4<f32>,
    @location(8) model_3: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// The same skinning the mesh shader does, and it has to be: a shadow computed
// from the bind pose while the model is drawn from its current one is a
// creature standing beside its own silhouette.
@vertex
fn vs_mesh(in: MeshIn) -> VsOut {
    var out: VsOut;
    let w = in.bone_weights;
    let total = w.x + w.y + w.z + w.w;
    var position = vec4<f32>(in.position, 1.0);
    if (total > 0.001) {
        let p = vec4<f32>(in.position, 1.0);
        var skinned = vec4<f32>(0.0);
        skinned = skinned + (bones[in.bone_indices.x] * p) * w.x;
        skinned = skinned + (bones[in.bone_indices.y] * p) * w.y;
        skinned = skinned + (bones[in.bone_indices.z] * p) * w.z;
        skinned = skinned + (bones[in.bone_indices.w] * p) * w.w;
        position = skinned / total;
    }
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    out.clip = light.view_proj * model * position;
    out.uv = in.uv;
    return out;
}

struct TerrainIn {
    @location(0) position: vec3<f32>,
};

@vertex
fn vs_terrain(in: TerrainIn) -> VsOut {
    var out: VsOut;
    out.clip = light.view_proj * vec4<f32>(in.position, 1.0);
    out.uv = vec2<f32>(0.0);
    return out;
}

// **The one fragment stage here, and it exists for leaves.** A tree is a
// handful of alpha-keyed planes; drawn without the cutout its shadow is a set
// of solid rectangles on the grass, which is not a subtle artefact. Opaque
// batches use a pipeline with no fragment stage at all, so they pay nothing
// for it.
@fragment
fn fs_alpha_key(in: VsOut) {
    if (textureSample(tex, samp, in.uv).a < 0.5) {
        discard;
    }
}
"#;

/// The depth map, the matrix it was drawn with, and the pipelines that fill
/// it.
pub struct ShadowMap {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: u32,
    light_buffer: wgpu::Buffer,
    light_bind: wgpu::BindGroup,
    /// A one-matrix pose and a one-pixel texture, bound the moment the pass
    /// opens.
    ///
    /// **Every pipeline here shares one three-group layout, so every group has
    /// to be set before any draw** -- terrain skins nothing and tests no
    /// alpha, and the validator does not care. Binding defaults centrally is
    /// what stops that being a rule every caller has to remember: the first
    /// version of this left it to the caller, and the failure was not a
    /// missing shadow but a panic in the middle of a frame.
    default_bones: wgpu::BindGroup,
    default_material: wgpu::BindGroup,
    terrain_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    mesh_alpha_pipeline: wgpu::RenderPipeline,
}

impl ShadowMap {
    /// `bone_layout` and `material_layout` must be the *same* layouts the mesh
    /// renderer built its bind groups with, so the shadow pass can bind a
    /// model's existing pose and texture rather than a second copy of each.
    pub fn new(
        gpu: &Gpu,
        size: u32,
        bone_layout: &wgpu::BindGroupLayout,
        material_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shadow"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });

        let light_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("shadow light"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        let light_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow light"),
            size: std::mem::size_of::<LightUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow light"),
            layout: &light_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: light_buffer.as_entire_binding(),
            }],
        });

        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadow"),
                bind_group_layouts: &[
                    Some(&light_layout),
                    Some(bone_layout),
                    Some(material_layout),
                ],
                immediate_size: 0,
            });

        let mesh_buffers = [
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<MeshVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3,
                    1 => Float32x3,
                    2 => Float32x2,
                    3 => Uint8x4,
                    4 => Unorm8x4
                ],
            },
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Instance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    5 => Float32x4,
                    6 => Float32x4,
                    7 => Float32x4,
                    8 => Float32x4
                ],
            },
        ];
        let terrain_buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3],
        }];

        let build = |label: &str,
                     entry: &str,
                     buffers: &[wgpu::VertexBufferLayout<'_>],
                     fragment: Option<&str>| {
            gpu.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        buffers: &buffers.iter().cloned().map(Some).collect::<Vec<_>>(),
                    },
                    fragment: fragment.map(|entry| wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        targets: &[],
                    }),
                    primitive: wgpu::PrimitiveState {
                        // **Nothing is culled while casting, and that is not
                        // laziness.** M2 and WMO wind opposite ways, terrain a
                        // third; one shadow pipeline per winding would be
                        // three pipelines to keep in step with a fact that has
                        // already been got wrong twice in this project. A
                        // two-sided caster costs a few more fragments and
                        // cannot produce a building that fails to shade its
                        // own courtyard.
                        cull_mode: None,
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: SHADOW_FORMAT,
                        depth_write_enabled: Some(true),
                        depth_compare: Some(wgpu::CompareFunction::LessEqual),
                        stencil: Default::default(),
                        // A small constant bias on top of the normal offset the
                        // receiver applies. The slope term is what handles a
                        // hillside seen edge-on by a low sun, where the depth
                        // across one texel is enormous and no constant is
                        // large enough.
                        bias: wgpu::DepthBiasState {
                            constant: 2,
                            slope_scale: 2.0,
                            clamp: 0.0,
                        },
                    }),
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
        };

        let terrain_pipeline = build("shadow terrain", "vs_terrain", &terrain_buffers, None);
        let mesh_pipeline = build("shadow mesh", "vs_mesh", &mesh_buffers, None);
        let mesh_alpha_pipeline = build(
            "shadow mesh alpha",
            "vs_mesh",
            &mesh_buffers,
            Some("fs_alpha_key"),
        );

        // Identity, not zero: a zero matrix collapses every vertex it touches
        // to the origin, which this project has already lost a milestone to
        // once. Nothing should read this, and if something does it should draw
        // the bind pose rather than a point.
        let identity = glam::Mat4::IDENTITY.to_cols_array_2d();
        let default_bone_buffer = {
            use wgpu::util::DeviceExt;
            gpu.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("shadow default bones"),
                    contents: bytemuck::cast_slice(&[identity]),
                    usage: wgpu::BufferUsages::STORAGE,
                })
        };
        let default_bones = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow default bones"),
            layout: bone_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: default_bone_buffer.as_entire_binding(),
            }],
        });
        // Opaque white, so the alpha test passes if anything ever reaches it
        // with this bound. A transparent default would silently discard.
        let white = crate::texture::upload_rgba(gpu, 1, 1, &[255, 255, 255, 255], "shadow default");
        let white_sampler = crate::texture::default_sampler(gpu);
        let default_material = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow default material"),
            layout: material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&white.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&white_sampler),
                },
            ],
        });

        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow map"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            // `COPY_SRC` for [`ShadowMap::read_depth`] alone. It costs
            // nothing to have and it is the difference between being able to
            // look at this buffer and reasoning about it -- which this
            // milestone spent an hour doing before dumping it.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            texture,
            view,
            size,
            light_buffer,
            light_bind,
            default_bones,
            default_material,
            terrain_pipeline,
            mesh_pipeline,
            mesh_alpha_pipeline,
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    /// The texture, for a debug dump. Nothing in the frame loop needs it --
    /// this is here because a shadow map is the one buffer in this renderer
    /// nobody can see, and "the map is empty" and "the matrix is wrong"
    /// produce the same unshadowed world.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Reads the map back as one float per texel, 0 nearest the sun and 1 at
    /// the far plane.
    ///
    /// **The instrument this project's own rules asked for and this milestone
    /// wrote late.** A shadow map is the only buffer in the renderer that is
    /// never displayed, and every way of getting it wrong -- an empty map, a
    /// map of the sky, a map with the depth axis reversed -- produces a world
    /// that is uniformly lit or uniformly dark. Those are two pictures for at
    /// least six causes. Looking at the map separates them in one glance.
    pub fn read_depth(&self, gpu: &Gpu) -> Result<Vec<f32>, crate::Error> {
        let row = self.size * 4;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow readback"),
            size: (padded * self.size) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("shadow readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.size),
                },
            },
            wgpu::Extent3d {
                width: self.size,
                height: self.size,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| crate::Error::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| crate::Error::Readback(e.to_string()))?
            .map_err(|e| crate::Error::Readback(e.to_string()))?;
        let view = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| crate::Error::Readback(e.to_string()))?;
        let mut out = Vec::with_capacity((self.size * self.size) as usize);
        for line in view.chunks_exact(padded as usize) {
            for texel in line[..row as usize].chunks_exact(4) {
                out.push(f32::from_le_bytes([texel[0], texel[1], texel[2], texel[3]]));
            }
        }
        drop(view);
        buffer.unmap();
        Ok(out)
    }

    /// Uploads the matrix the pass will be recorded with.
    pub fn set_matrix(&self, gpu: &Gpu, view_proj: glam::Mat4) {
        gpu.write_buffer(
            &self.light_buffer,
            0,
            bytemuck::bytes_of(&LightUniform {
                view_proj: view_proj.to_cols_array_2d(),
            }),
        );
    }

    /// Opens the depth-only pass, cleared and with the light bound.
    pub fn begin<'a>(&'a self, encoder: &'a mut wgpu::CommandEncoder) -> wgpu::RenderPass<'a> {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            multiview_mask: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_bind_group(0, &self.light_bind, &[]);
        pass.set_bind_group(1, &self.default_bones, &[]);
        pass.set_bind_group(2, &self.default_material, &[]);
        pass
    }

    pub fn terrain_pipeline(&self) -> &wgpu::RenderPipeline {
        &self.terrain_pipeline
    }

    pub fn mesh_pipeline(&self) -> &wgpu::RenderPipeline {
        &self.mesh_pipeline
    }

    pub fn mesh_alpha_pipeline(&self) -> &wgpu::RenderPipeline {
        &self.mesh_alpha_pipeline
    }
}

/// Where the shadow frustum sits, and the matrix that renders it.
///
/// Split out as a plain function of plain numbers so the geometry can be
/// tested without a GPU -- which matters more here than usual, because every
/// way of getting this wrong produces *no shadows at all* and they all look
/// identical from the window.
///
/// `sun` points **towards** the sun, as everywhere else in this client.
/// `centre` is where the box is aimed; `radius` is half its width in world
/// units. Returns `None` when the sun is too near the horizon for a shadow to
/// mean anything -- at which point the box would be kilometres long and every
/// texel of it useless.
pub fn light_view_proj(
    centre: glam::Vec3,
    sun: glam::Vec3,
    radius: f32,
    texels: u32,
) -> Option<glam::Mat4> {
    let sun = sun.normalize_or_zero();
    // Below this the shadows are longer than the box could ever be, and the
    // direct light is nearly gone anyway -- `sun_direction` puts the sun at
    // 0.087 about five degrees above the horizon.
    if sun.z < 0.05 {
        return None;
    }
    // The world is Z-up and `look_at` needs an up vector that is not the view
    // direction; overhead is the one case where Z is.
    let up = if sun.z.abs() > 0.99 {
        glam::Vec3::Y
    } else {
        glam::Vec3::Z
    };
    // The same constructors the camera builds its own matrices with, from the
    // same `glam::camera::rh` family -- a shadow map projected by a different
    // convention from the scene is a depth comparison between two different
    // ideas of depth.
    let view = glam::camera::rh::view::look_at_mat4(centre + sun * radius * 2.0, centre, up);
    let mut proj = glam::camera::rh::proj::directx::orthographic(
        -radius,
        radius,
        -radius,
        radius,
        0.0,
        radius * 4.0,
    );

    // **Snapped to whole texels, or the edges crawl.** A shadow map whose
    // frustum slides continuously with the camera resamples the same edge at a
    // different sub-texel offset every frame, and the result is a shimmer
    // along every shadow boundary that is far more obvious in motion than the
    // shadows themselves. Rounding a fixed world point onto the texel grid
    // makes the whole map move in steps.
    let per_texel = texels as f32 / 2.0;
    let origin = (proj * view).project_point3(glam::Vec3::ZERO);
    let dx = (origin.x * per_texel).round() / per_texel - origin.x;
    let dy = (origin.y * per_texel).round() / per_texel - origin.y;
    proj = glam::Mat4::from_translation(glam::vec3(dx, dy, 0.0)) * proj;
    Some(proj * view)
}

/// How far a shadow-map texel is in world units, for the receiver's normal
/// offset.
pub fn texel_size(radius: f32, texels: u32) -> f32 {
    2.0 * radius / texels.max(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn noon() -> Vec3 {
        Vec3::new(0.0, 0.0, 1.0)
    }

    #[test]
    fn the_centre_of_the_box_lands_in_the_middle_of_the_map() {
        let centre = Vec3::new(1234.0, -567.0, 89.0);
        let m = light_view_proj(centre, Vec3::new(0.2, -0.3, 0.9), 128.0, 2048).unwrap();
        let ndc = m.project_point3(centre);
        // Not exactly zero: the texel snap moves the whole frustum by up to
        // half a texel, which is the point of it.
        let slack = 2.0 / 2048.0;
        assert!(
            ndc.x.abs() < slack && ndc.y.abs() < slack,
            "the aim point should be at the centre of the map, got {ndc:?}"
        );
        assert!(
            ndc.z > 0.0 && ndc.z < 1.0,
            "the aim point must be inside the depth range, got {}",
            ndc.z
        );
    }

    #[test]
    fn something_above_the_centre_is_nearer_the_light_than_the_centre() {
        // The sign of the depth axis is the single easiest thing to get
        // backwards here, and getting it backwards does not fail: it produces
        // a world where everything shadows nothing, which is exactly what a
        // missing feature looks like.
        let centre = Vec3::ZERO;
        let m = light_view_proj(centre, noon(), 100.0, 1024).unwrap();
        let above = m.project_point3(Vec3::new(0.0, 0.0, 40.0));
        let below = m.project_point3(Vec3::new(0.0, 0.0, -40.0));
        assert!(
            above.z < below.z,
            "a caster overhead must be nearer the sun: {} against {}",
            above.z,
            below.z
        );
    }

    #[test]
    fn the_box_covers_its_radius_and_stops() {
        let m = light_view_proj(Vec3::ZERO, noon(), 100.0, 1024).unwrap();
        let inside = m.project_point3(Vec3::new(90.0, 0.0, 0.0));
        let outside = m.project_point3(Vec3::new(140.0, 0.0, 0.0));
        assert!(inside.x.abs() < 1.0, "90 of 100 units should be inside");
        assert!(outside.x.abs() > 1.0, "140 of 100 units should be outside");
    }

    #[test]
    fn a_sun_on_the_horizon_gets_no_matrix_at_all() {
        // Rather than a matrix that technically exists and wastes a whole
        // frame's shadow pass on a box tens of kilometres long. The caller
        // reads `None` as "no shadows this hour", which is what a sunset is.
        assert!(light_view_proj(Vec3::ZERO, Vec3::new(0.0, -1.0, 0.02), 100.0, 1024).is_none());
        assert!(light_view_proj(Vec3::ZERO, Vec3::new(0.0, -1.0, -0.5), 100.0, 1024).is_none());
        assert!(light_view_proj(Vec3::ZERO, noon(), 100.0, 1024).is_some());
    }

    #[test]
    fn the_frustum_moves_in_whole_texels() {
        // Two camera positions a fraction of a texel apart must produce the
        // same matrix, because that is what stops the edges shimmering. A
        // whole texel apart must produce a different one, or the box would
        // never follow the camera at all -- which is the opposite failure and
        // looks like shadows that stop existing when you walk.
        let sun = Vec3::new(0.3, -0.4, 0.86);
        let radius = 128.0;
        let texels = 1024;
        let world = texel_size(radius, texels);
        let a = light_view_proj(Vec3::ZERO, sun, radius, texels).unwrap();
        let nudged =
            light_view_proj(Vec3::new(world * 0.05, 0.0, 0.0), sun, radius, texels).unwrap();
        let moved = light_view_proj(Vec3::new(world * 8.0, 0.0, 0.0), sun, radius, texels).unwrap();
        let at = |m: glam::Mat4| m.project_point3(Vec3::new(20.0, 20.0, 0.0));
        let (x, y) = (at(a), at(nudged));
        assert!(
            (x.x - y.x).abs() < 1e-4 && (x.y - y.y).abs() < 1e-4,
            "a sub-texel step must not move the grid: {x:?} against {y:?}"
        );
        let z = at(moved);
        assert!(
            (x.x - z.x).abs() > 1e-3,
            "eight texels must move it: {x:?} against {z:?}"
        );
    }

    #[test]
    fn a_texel_is_the_box_divided_by_the_map() {
        assert_eq!(texel_size(128.0, 1024), 0.25);
        // Never a division by zero, whatever a caller passes.
        assert!(texel_size(128.0, 0).is_finite());
    }
}
