//! What is drawn on the sky, and how bright each of it is this hour.
//!
//! Three things, in three different relationships to the data:
//!
//! * **The star dome is a transcription.** `Environments\Stars\Stars.mdx` is
//!   row 4 of `LightSkybox.dbc`, in the same table as every zone's painted
//!   backdrop, and it is found here by *name* rather than by id -- see
//!   `dbc::light::Lighting::star_dome`.
//! * **The zone skybox is a lookup.** `LightParams.light_skybox_id` names one
//!   for 158 of 850 rows, and for **none of the 158 rows an outdoor light on
//!   Azeroth or Kalimdor uses**. So this is a feature that draws nothing in
//!   every zone this client is usually standing in, and that is the data's
//!   answer rather than a gap.
//! * **The cloud band is a construction**, and the only one. There is no cloud
//!   model and no cloud colour this client has identified, so the geometry is
//!   generated and the colour is *derived* from two bands that are measured --
//!   the disc and the ambient. See [`cloud_tint`].
//!
//! The brightnesses are the interesting part and they are all in free
//! functions at the bottom, tested without a GPU. Every one of them can be
//! wrong in a way that renders perfectly.

use anyhow::{Context, Result};
use glam::{Mat4, Vec3};
use mpq::Chain;
use render::celestial::{CelestialRenderer, Placement};
use render::mesh::{BlendMode, GpuMesh};
use render::Gpu;

use crate::model;

/// How far out the sky geometry is drawn, in world units.
///
/// **Any value renders the same picture and this one still matters.** Nothing
/// on the sky writes or tests depth, so the radius changes no occlusion --
/// but the geometry still goes through the projection, so a dome outside the
/// far plane is clipped into a hole. The `Fly` camera's far plane is 12,000.
const SKY_RADIUS: f32 = 3_000.0;

/// The cloud band, in radians above the horizon.
///
/// **Chosen, like the sun's arc, and for the same reason**: no table says
/// where clouds sit. What is *not* chosen is that they sit in a band at all --
/// see `render::celestial::cloud_band` for the measurement of
/// `StarsAndClouds.blp` that settles it.
const CLOUD_LOW: f32 = 0.03;
const CLOUD_HIGH: f32 = 0.62;
/// How many times the panorama goes round. Its 512x256 shape covers twice as
/// much azimuth as elevation, so four copies over 360 degrees put the band at
/// roughly the 34 degrees of sky `CLOUD_LOW`..`CLOUD_HIGH` spans.
const CLOUD_REPEAT: f32 = 4.0;
/// How fast the band drifts, in texture widths per second. One full copy of
/// the panorama crosses in eight minutes.
const CLOUD_DRIFT: f32 = 0.002;

const CLOUD_TEXTURE: &str = r"Environments\Stars\StarsAndClouds.blp";

/// A model drawn on the sky: its geometry, one binding per texture, and the
/// uniform that places it.
struct SkyModel {
    model: model::LoadedModel,
    binds: Vec<wgpu::BindGroup>,
    placement: Placement,
}

impl SkyModel {
    fn load(
        gpu: &Gpu,
        chain: &mut Chain,
        celestial: &mut CelestialRenderer,
        path: &str,
    ) -> Result<Self> {
        let model = model::load(gpu, chain, path, &model::Variations::default(), 0)
            .with_context(|| format!("loading sky model {path}"))?;
        let binds = model
            .textures
            .iter()
            .map(|t| celestial.material_bind_group(gpu, &t.view))
            .collect();
        celestial.prepare(gpu, model.draws.iter().map(|d| d.state.blend));
        Ok(Self {
            model,
            binds,
            placement: celestial.placement(gpu),
        })
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, celestial: &CelestialRenderer) {
        pass.set_bind_group(0, self.placement.bind_group(), &[]);
        pass.set_vertex_buffer(0, self.model.mesh.vertices.slice(..));
        pass.set_index_buffer(
            self.model.mesh.indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        for draw in &self.model.draws {
            let (Some(pipeline), Some(bind)) =
                (celestial.get(draw.state.blend), self.binds.get(draw.texture))
            else {
                continue;
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, bind, &[]);
            pass.draw_indexed(
                draw.first_index..draw.first_index + draw.index_count,
                0,
                0..1,
            );
        }
    }
}

/// The generated cloud band and the one texture it wears.
struct Clouds {
    mesh: GpuMesh,
    indices: u32,
    bind: wgpu::BindGroup,
    placement: Placement,
}

/// Everything drawn on the sky, and which skybox is currently loaded.
#[derive(Default)]
pub struct SkyScene {
    stars: Option<SkyModel>,
    clouds: Option<Clouds>,
    /// The zone backdrop and the `LightSkybox` id it was loaded for.
    ///
    /// **The id is kept even when the model failed to load**, so a skybox this
    /// client cannot read is attempted once rather than once a frame. "Not
    /// asked yet" and "asked and there is nothing" are different states, which
    /// is the same three-state discipline the game-object cache follows.
    skybox: Option<SkyModel>,
    skybox_id: u32,
}

impl SkyScene {
    /// Loads the star dome and the cloud band. Neither is fatal: a client that
    /// cannot read them draws the gradient it drew before, which is a sky.
    pub fn load(
        gpu: &Gpu,
        chain: &mut Chain,
        celestial: &mut CelestialRenderer,
        lighting: Option<&dbc::light::Lighting>,
    ) -> Self {
        let stars = lighting
            .and_then(|l| l.star_dome())
            .and_then(|path| match SkyModel::load(gpu, chain, celestial, &path) {
                Ok(model) => Some(model),
                Err(e) => {
                    tracing::warn!("no star dome: {e:#}");
                    None
                }
            });
        let clouds = match load_clouds(gpu, chain, celestial) {
            Ok(clouds) => Some(clouds),
            Err(e) => {
                tracing::warn!("no cloud band: {e:#}");
                None
            }
        };
        tracing::info!(
            stars = stars.is_some(),
            clouds = clouds.is_some(),
            "sky loaded"
        );
        Self {
            stars,
            clouds,
            skybox: None,
            skybox_id: 0,
        }
    }

    /// Loads the backdrop a place names, if it has changed since the last one.
    ///
    /// Called per frame with whatever `Sample::skybox_id` says; on Azeroth and
    /// Kalimdor that is always zero and this does nothing at all.
    pub fn set_skybox(
        &mut self,
        gpu: &Gpu,
        chain: &mut Chain,
        celestial: &mut CelestialRenderer,
        lighting: Option<&dbc::light::Lighting>,
        id: u32,
    ) {
        if id == self.skybox_id {
            return;
        }
        self.skybox_id = id;
        self.skybox = lighting
            .and_then(|l| l.skybox_model(id))
            .and_then(|path| match SkyModel::load(gpu, chain, celestial, &path) {
                Ok(model) => {
                    tracing::info!(id, %path, "skybox loaded");
                    Some(model)
                }
                Err(e) => {
                    tracing::warn!(id, "skybox {id} would not load: {e:#}");
                    None
                }
            });
    }

    /// Records the sky's geometry into a pass that has already been given the
    /// gradient.
    ///
    /// **The order is stars, then backdrop, then clouds**, which is the order
    /// they are at: a zone skybox is a painted wall in front of the stars and
    /// hides them where it is opaque, and clouds are the only one of the three
    /// that is actually weather. None of them writes depth, so the order here
    /// is the whole of it.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        gpu: &Gpu,
        pass: &mut wgpu::RenderPass<'_>,
        celestial: &CelestialRenderer,
        view_proj: Mat4,
        eye: Vec3,
        sun: Vec3,
        sample: Option<&dbc::light::Sample>,
        hour: f32,
        storm: f32,
        seconds: f32,
    ) {
        if let Some(stars) = &self.stars {
            let opacity = star_opacity(sun.z, storm);
            if opacity > 0.0 {
                celestial.set(
                    gpu,
                    &stars.placement,
                    view_proj,
                    // **Turned by the clock, so the night sky moves.** The
                    // rate is the game's own day, not the sky's: one turn per
                    // game day is what a player sitting still actually sees.
                    Mat4::from_translation(eye)
                        * Mat4::from_rotation_z(hour / 24.0 * std::f32::consts::TAU)
                        * Mat4::from_scale(Vec3::splat(SKY_RADIUS / 25.0)),
                    [1.0, 1.0, 1.0, opacity],
                    [0.0, 0.0],
                );
                stars.draw(pass, celestial);
            }
        }
        if let Some(skybox) = &self.skybox {
            celestial.set(
                gpu,
                &skybox.placement,
                view_proj,
                Mat4::from_translation(eye) * Mat4::from_scale(Vec3::splat(SKY_RADIUS / 25.0)),
                [1.0; 4],
                [0.0, 0.0],
            );
            skybox.draw(pass, celestial);
        }
        if let Some(clouds) = &self.clouds {
            let Some(pipeline) = celestial.get(BlendMode::Blend) else {
                return;
            };
            celestial.set(
                gpu,
                &clouds.placement,
                view_proj,
                Mat4::from_translation(eye) * Mat4::from_scale(Vec3::splat(SKY_RADIUS)),
                cloud_tint(sample, sun.z, storm),
                [seconds * CLOUD_DRIFT, 0.0],
            );
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, clouds.placement.bind_group(), &[]);
            pass.set_bind_group(1, &clouds.bind, &[]);
            pass.set_vertex_buffer(0, clouds.mesh.vertices.slice(..));
            pass.set_index_buffer(clouds.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..clouds.indices, 0, 0..1);
        }
    }

    /// Whether anything at all was loaded, for the overlay to report. "The sky
    /// is empty" and "the sky failed to load" are the same picture.
    pub fn describe(&self) -> String {
        format!(
            "stars {}, clouds {}, skybox {}",
            if self.stars.is_some() { "yes" } else { "no" },
            if self.clouds.is_some() { "yes" } else { "no" },
            match (&self.skybox, self.skybox_id) {
                (Some(_), id) => format!("{id}"),
                (None, 0) => "none".into(),
                (None, id) => format!("{id} (unreadable)"),
            }
        )
    }
}

fn load_clouds(
    gpu: &Gpu,
    chain: &mut Chain,
    celestial: &mut CelestialRenderer,
) -> Result<Clouds> {
    let bytes = chain
        .read(CLOUD_TEXTURE)
        .with_context(|| format!("reading {CLOUD_TEXTURE}"))?;
    let parsed = blp::Blp::parse(&bytes).with_context(|| format!("parsing {CLOUD_TEXTURE}"))?;
    let texture = render::texture::upload_blp(gpu, &parsed, CLOUD_TEXTURE);
    let (vertices, indices) =
        render::celestial::cloud_band(96, CLOUD_LOW, CLOUD_HIGH, CLOUD_REPEAT);
    celestial.prepare(gpu, [BlendMode::Blend]);
    Ok(Clouds {
        mesh: GpuMesh::upload(gpu, &vertices, &indices),
        indices: indices.len() as u32,
        bind: celestial.material_bind_group(gpu, &texture.view),
        placement: celestial.placement(gpu),
    })
}

/// A clamped ramp, which is `smoothstep` without the curve.
///
/// `from` may be greater than `to`, which is how a fade *out* is written.
fn ramp(from: f32, to: f32, at: f32) -> f32 {
    if (to - from).abs() < f32::EPSILON {
        return if at >= to { 1.0 } else { 0.0 };
    }
    ((at - from) / (to - from)).clamp(0.0, 1.0)
}

/// How much of the star dome to draw, from the sun's height and the weather.
///
/// **Derived from the arc, which is itself chosen** -- see the viewer's
/// `sun_direction`. Nothing in `Light.dbc` fades the stars: of its six scalar
/// curves, four are constant across the day on every outdoor row and the two
/// that move are the fog distances. So this is the sun's own elevation, and it
/// is written down as a choice rather than dressed up as a transcription.
///
/// The zenith colour is deliberately *not* the driver, though it is the
/// obvious one. It is measured, and it refutes itself: Azeroth's zenith reads
/// (0,31,73) at noon and (35,74,84) at dawn, so a fade driven by how dark the
/// sky is overhead would put more stars out at midday than at sunrise.
pub fn star_opacity(sun_z: f32, storm: f32) -> f32 {
    // Full stars once the sun is a little below the horizon, none while it is
    // a little above -- which is roughly what twilight is.
    let night = ramp(0.10, -0.12, sun_z);
    night * (1.0 - storm.clamp(0.0, 1.0))
}

/// What colour a cloud is at this hour, and how solid.
///
/// **Both bands here are measured; the shape between them is not.** The disc
/// (`bands::DISC`) is the only band that stays bright at every hour, which is
/// what identified it, and the ambient (`bands::AMBIENT`) is the sky-coloured
/// fill. A cloud is a white diffuse thing lit by exactly those two, so taking
/// them is derivation -- the same move that made fog the horizon's own colour
/// instead of a band somebody had to guess.
///
/// What is chosen is the weighting, and it has one job: **the sun lights the
/// underside of a cloud when it is low and the top of it when it is high**,
/// which is why sunsets are the colourful ones and midday clouds are white.
/// So the direct term is strongest at grazing angles and nearly gone overhead.
pub fn cloud_tint(sample: Option<&dbc::light::Sample>, sun_z: f32, storm: f32) -> [f32; 4] {
    let (ambient, disc) = match sample {
        Some(s) => (s.ambient, s.disc),
        // No light data: a plain grey cloud, which is what the fixed-gradient
        // fallback sky wants standing next to it.
        None => ([0.55; 3], [0.85; 3]),
    };
    // Nothing of the sun once it is down: at night the cloud is whatever the
    // sky is lending it, which is the ambient.
    let up = ramp(-0.10, 0.10, sun_z);
    let grazing = 1.0 - sun_z.clamp(0.0, 1.0);
    let direct = (0.25 + 0.75 * grazing) * up;
    let mut rgb = [0.0f32; 3];
    for (out, (a, d)) in rgb.iter_mut().zip(ambient.iter().zip(disc.iter())) {
        *out = (a + d * direct).clamp(0.0, 1.0);
    }
    // Chosen, and the only honest thing to say about it: `SMSG_WEATHER` sends
    // a state and an intensity, and cloud cover is not on the wire at all.
    let opacity = 0.50 + 0.45 * storm.clamp(0.0, 1.0);
    [rgb[0], rgb[1], rgb[2], opacity]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ambient: [f32; 3], disc: [f32; 3]) -> dbc::light::Sample {
        dbc::light::Sample {
            diffuse: [1.0; 3],
            ambient,
            sky: dbc::light::DEFAULT_SKY,
            disc,
            fog_end: 1000.0,
            fog_start: 100.0,
            params_id: 0,
            skybox_id: 0,
        }
    }

    #[test]
    fn stars_come_out_after_the_sun_goes_down_and_not_before() {
        // Midnight, dusk, noon -- `sun_direction`'s z at those hours.
        assert_eq!(star_opacity(-1.0, 0.0), 1.0);
        assert_eq!(star_opacity(1.0, 0.0), 0.0);
        // The sun is exactly on the horizon at six and eighteen, which is
        // inside the fade rather than at either end of it: a starfield that
        // switched on at the instant of sunset would be a visible pop.
        let dusk = star_opacity(0.0, 0.0);
        assert!(dusk > 0.0 && dusk < 1.0, "half-lit at the horizon: {dusk}");
    }

    #[test]
    fn a_storm_hides_the_stars_it_does_not_dim_them_a_little() {
        assert_eq!(star_opacity(-1.0, 1.0), 0.0);
        assert!(star_opacity(-1.0, 0.5) < star_opacity(-1.0, 0.0));
    }

    #[test]
    fn a_low_sun_paints_the_clouds_and_a_high_one_does_not() {
        // The property this whole function exists for, and the one that would
        // be lost by any simpler weighting: the sun's *colour* reaches the
        // clouds most when it is low. A sunset is orange because of the angle,
        // not because band 9 is more orange than it is at noon -- it is, but
        // this must hold even when it is not.
        let warm = sample([0.1, 0.1, 0.2], [1.0, 0.5, 0.2]);
        let low = cloud_tint(Some(&warm), 0.10, 0.0);
        let high = cloud_tint(Some(&warm), 1.0, 0.0);
        let redness = |c: [f32; 4]| c[0] - c[2];
        assert!(
            redness(low) > redness(high),
            "a low sun must colour the clouds more: {low:?} against {high:?}"
        );
    }

    #[test]
    fn a_cloud_at_night_is_the_ambient_and_nothing_else() {
        // Otherwise the moon's own band -- which is the *brightest* band at
        // every hour, that being how it was identified -- would light the
        // clouds as hard as the sun does, and midnight would come out with a
        // bright overcast.
        let night = sample([0.11, 0.12, 0.2], [0.9, 0.94, 1.0]);
        let tint = cloud_tint(Some(&night), -1.0, 0.0);
        assert!((tint[0] - 0.11).abs() < 1e-5, "{tint:?}");
        assert!((tint[1] - 0.12).abs() < 1e-5, "{tint:?}");
        assert!((tint[2] - 0.20).abs() < 1e-5, "{tint:?}");
    }

    #[test]
    fn a_storm_thickens_the_cloud_and_clear_weather_does_not_hide_it() {
        let s = sample([0.3; 3], [0.9; 3]);
        let clear = cloud_tint(Some(&s), 0.5, 0.0)[3];
        let storm = cloud_tint(Some(&s), 0.5, 1.0)[3];
        assert!(storm > clear, "{storm} against {clear}");
        assert!(clear > 0.0, "clouds must be visible in clear weather too");
        assert!(storm <= 1.0);
    }

    #[test]
    fn with_no_light_data_the_clouds_are_still_drawn() {
        // The fixed-gradient fallback sky is a sky; clouds that vanish with it
        // would make an offline render look like a different feature set.
        let tint = cloud_tint(None, 0.5, 0.0);
        assert!(tint[3] > 0.0);
        assert!(tint.iter().take(3).all(|c| *c > 0.0));
    }

    #[test]
    fn the_ramp_runs_both_ways_and_clamps() {
        assert_eq!(ramp(0.0, 1.0, -5.0), 0.0);
        assert_eq!(ramp(0.0, 1.0, 5.0), 1.0);
        assert!((ramp(0.0, 1.0, 0.25) - 0.25).abs() < 1e-6);
        // Backwards, which is how every fade-out here is written. Getting this
        // wrong would put the stars out at noon.
        assert_eq!(ramp(1.0, 0.0, -5.0), 1.0);
        assert_eq!(ramp(1.0, 0.0, 5.0), 0.0);
        // Degenerate rather than a division by zero.
        assert!(ramp(1.0, 1.0, 2.0).is_finite());
    }
}
