//! The WGSL every surface in the world shades itself with: the camera
//! uniform, the shadow map, and the three functions that read them.
//!
//! **This exists because a comment claiming two shaders were identical was not
//! a check that they were.** `terrain.rs` carried "the same two functions the
//! mesh shader uses, kept identical on purpose -- terrain lit one way and the
//! buildings standing on it lit another is the seam a player notices first",
//! above a copy whose unlit fallback read `0.45 + 0.55 * ndl` against the mesh
//! shader's `0.38 + 0.62 * ndl`. They had drifted, in the exact way the
//! comment was written to prevent, and nothing could have said so: the
//! fallback only runs where there is no light data, and the difference is one
//! shade of grey on an offline model view.
//!
//! So the text is now one string, prepended to both shaders. The two cannot
//! disagree because there is no longer a second copy to edit.

/// The camera binding, the shadow binding, and `shadow_factor`, `sky_light`
/// and `fogged`.
///
/// Prepended to a shader that then declares its own group 1 and up. Group 0 is
/// entirely spoken for here.
pub const COMMON: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    light: vec4<f32>,
    sun: vec4<f32>,
    ambient: vec4<f32>,
    fog: vec4<f32>,
    fog_range: vec4<f32>,
    // The matrix the shadow map was rendered with.
    light_view_proj: mat4x4<f32>,
    // `x` how dark a shadow is, and zero means there is no shadow map to read.
    // `y` one shadow texel, in texture coordinates, which is the PCF step.
    // `z` how far along its own normal a surface moves before it asks.
    shadow: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
// Bound to a 1x1 texture when there is no shadow map, rather than being
// optional: a binding that sometimes is not there would need a second pipeline
// for every state, and `shadow.x` of zero already says "do not ask".
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;

// How much of the direct light reaches a point: 1 in the open, less in shadow.
//
// **The offset is along the surface normal rather than in depth, and that is
// the whole difference between a shadow and a rash.** A depth bias big enough
// to stop a chunk of terrain shadowing itself is big enough to detach a
// character's shadow from its feet; moving the *sample point* out of the
// surface instead scales with how obliquely the surface faces the sun, which
// is exactly where acne appears.
fn shadow_factor(world: vec3<f32>, normal: vec3<f32>) -> f32 {
    if (camera.shadow.x <= 0.0) {
        return 1.0;
    }
    let n = normalize(normal);
    // A surface already facing away from the sun receives no direct light, so
    // asking whether it is shadowed is asking a question with no consequence
    // -- and it is the case where a shadow map is least reliable, because the
    // surface is its own occluder.
    if (dot(n, normalize(camera.light.xyz)) <= 0.0) {
        return 1.0;
    }
    let p = camera.light_view_proj * vec4<f32>(world + n * camera.shadow.z, 1.0);
    let ndc = p.xyz / p.w;
    if (ndc.z <= 0.0 || ndc.z >= 1.0) {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    var sum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let at = uv + vec2<f32>(f32(x), f32(y)) * camera.shadow.y;
            // `...Level` rather than `textureSampleCompare`: the plain form
            // requires uniform control flow, which the early returns above
            // have already given up.
            sum = sum + textureSampleCompareLevel(shadow_map, shadow_sampler, at, ndc.z);
        }
    }
    // **The box has an edge and the world does not.** Without this the shadow
    // map's boundary draws a hard line across the ground wherever the camera
    // happens to be standing, which reads as a rendering fault rather than as
    // a budget. Fading the *shadow* out rather than clamping the lookup means
    // the far side of the line is lit, which is what it would have been
    // anyway.
    let edge = 1.0 - smoothstep(0.82, 1.0, max(abs(ndc.x), abs(ndc.y)));
    return 1.0 - (1.0 - sum / 9.0) * camera.shadow.x * edge;
}

// Lighting from the world's own tables when there is any, and the old fixed
// headlight when there is not -- an offline model view has no hour and no
// place, and must still read as shape rather than going black. `sun.w` is the
// switch: zero means no data.
//
// **The shadow multiplies the direct term only.** Ambient is light arriving
// from the whole sky, and the sky is not what the tree is standing in front
// of; darkening it too turns every shadow into a hole.
fn sky_light(normal: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
    let n = normalize(normal);
    let ndl = max(dot(n, normalize(camera.light.xyz)), 0.0);
    if (camera.sun.w <= 0.0) {
        return vec3<f32>(0.38 + 0.62 * ndl);
    }
    return camera.ambient.rgb
        + camera.sun.rgb * ndl * camera.sun.w * shadow_factor(world, normal);
}

// Distance fog, applied after lighting. `fog_range.y` of zero disables it.
fn fogged(colour: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
    if (camera.fog_range.y <= 0.0) {
        return colour;
    }
    let distance = length(world - camera.eye.xyz);
    let t = clamp((distance - camera.fog_range.x) / max(camera.fog_range.y - camera.fog_range.x, 1.0), 0.0, 1.0);
    return mix(colour, camera.fog.rgb, t);
}
"#;
