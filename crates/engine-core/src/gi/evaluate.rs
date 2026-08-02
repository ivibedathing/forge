//! Folding baked transfer against the live sky, into the field the GPU reads.
//!
//! This is M21's architecture rather than M16's: the *model* is a pure CPU
//! function of (bake file, sky, ambient) and the GPU only ever reads its
//! output. No new pass, no compute — the per-frame cost is a small upload.
//!
//! # The fold
//!
//! A probe stores transfer: how much light reaches it per unit emitted by each
//! basis source. Transfer is linear in source radiance, so evaluation is a
//! scaled sum, per SH coefficient and per channel:
//!
//! ```text
//! F[c] = ambient ⊙ (zenith ⊙ T_zenith[c] + ground ⊙ T_ground[c]) / mean
//! ```
//!
//! Everything that is constant over the volume — the authored `AmbientLight`,
//! the two live sky bands, and `sky_ambient`'s per-channel normalization — folds
//! in here, which leaves the shader with nothing to do but a trilinear fetch and
//! four multiply-adds. It is also what makes the guarantee in
//! [`LINEAR_GAIN`] checkable on the CPU, where a test can read the numbers.

use crate::components::LightProbeVolume;
use crate::math::Vec3;
use crate::scene::{EnvironmentSettings, ResolvedLights};

use super::{BakedGi, BAND_GROUND, BAND_ZENITH, CHANNELS, SH_L1_COEFFS};

/// What the three linear SH coefficients are multiplied by when the field is
/// reconstructed for a normal.
///
/// Not a tuning knob — it is the one value that makes design §3.1 exact, and it
/// is derived rather than fitted. An unoccluded probe integrates
/// `weight_band(d) · [1, d.x, d.y, d.z]` over a sphere of directions, and
/// `weight_zenith(d)` is `d.y · 0.5 + 0.5`, so:
///
/// ```text
/// T_zenith[0] = ⟨0.5·d.y + 0.5⟩            = 0.5
/// T_zenith[2] = ⟨(0.5·d.y + 0.5)·d.y⟩      = 0.5·⟨d.y²⟩ = 1/6
/// T_zenith[1] = T_zenith[3]                = 0
/// ```
///
/// and the ground band is its mirror. Reconstruction must give back
/// `mix(ground, zenith, n.y · 0.5 + 0.5)`, whose zenith half is
/// `0.5 + 0.5·n.y`. Solving `1·0.5 + g·(1/6)·n.y = 0.5 + 0.5·n.y` gives
/// `g = 3`.
///
/// So an unoccluded probe reconstructs **`sky_ambient(n)` exactly**, which is
/// the property the whole milestone rests on: turning GI on cannot change the
/// brightness of an open scene, only redistribute it, and every visible
/// difference is therefore attributable to geometry.
pub const LINEAR_GAIN: f32 = 3.0;

/// The floor `sky_ambient` puts under its per-channel mean, mirrored here so a
/// black sky divides by the same number the shader would have divided by.
const MEAN_FLOOR: f32 = 1.0e-4;

/// A volume's baked transfer folded against one frame's lighting.
///
/// Four planes of RGBA, one per SH-L1 coefficient, in the probe order the bake
/// file uses (x fastest, then y, then z) — which is also the layout a 3D texture
/// wants, so the upload is a memcpy per plane rather than a shuffle.
#[derive(Debug, Clone, PartialEq)]
pub struct IrradianceField {
    pub grid: [u32; 3],
    /// World position of probe `[0, 0, 0]`.
    pub origin: Vec3,
    pub spacing: f32,
    /// How far the effect is mixed in, from the component. `0.0` reproduces the
    /// pre-M35 look exactly, which is what makes it a one-field A/B.
    pub intensity: f32,
    /// Metres of fade at the volume's boundary, from the component.
    pub blend: f32,
    /// `[coefficient][probe]`, RGB per channel. The alpha of coefficient 0
    /// carries [`openness`](Self::openness); the other three alphas are unused
    /// and written as zero, because `Rgba16Float` is the filterable four-channel
    /// format and a three-channel one is not guaranteed.
    pub planes: [Vec<[f32; 4]>; SH_L1_COEFFS],
}

impl IrradianceField {
    /// The volume's far corner — the position of the last probe.
    pub fn max_corner(&self) -> Vec3 {
        self.origin
            + Vec3::new(
                (self.grid[0].saturating_sub(1)) as f32,
                (self.grid[1].saturating_sub(1)) as f32,
                (self.grid[2].saturating_sub(1)) as f32,
            ) * self.spacing
    }

    /// Index of the probe at a grid coordinate, x fastest.
    fn index(&self, x: u32, y: u32, z: u32) -> usize {
        (z * self.grid[1] * self.grid[0] + y * self.grid[0] + x) as usize
    }

    /// Trilinearly interpolate one coefficient plane at a world position.
    ///
    /// Clamped at the edges, matching the sampler the renderer binds: outside
    /// the grid a fetch returns the border probe rather than wrapping to the
    /// far side of the volume.
    fn fetch(&self, coefficient: usize, at: Vec3) -> [f32; 4] {
        let cell = (at - self.origin) / self.spacing.max(f32::MIN_POSITIVE);
        let plane = &self.planes[coefficient];

        // The negation is load-bearing, not a style slip: `!(v > 0.0)` is not
        // `v <= 0.0` when `v` is NaN, and only the negated form keeps a NaN
        // position out of `as u32`. `grid_counts` carries the same allow for the
        // same reason; see CLAUDE.md's Traps before "cleaning up" either.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        let axis = |v: f32, n: u32| -> (u32, u32, f32) {
            let last = n.saturating_sub(1);
            if !(v > 0.0) {
                return (0, 0, 0.0);
            }
            let floor = v.floor();
            let i = (floor as u32).min(last);
            ((i), (i + 1).min(last), (v - floor).clamp(0.0, 1.0))
        };

        let (x0, x1, fx) = axis(cell.x, self.grid[0]);
        let (y0, y1, fy) = axis(cell.y, self.grid[1]);
        let (z0, z1, fz) = axis(cell.z, self.grid[2]);

        let mut out = [0.0f32; 4];
        for (zi, wz) in [(z0, 1.0 - fz), (z1, fz)] {
            for (yi, wy) in [(y0, 1.0 - fy), (y1, fy)] {
                for (xi, wx) in [(x0, 1.0 - fx), (x1, fx)] {
                    let w = wx * wy * wz;
                    if w == 0.0 {
                        continue;
                    }
                    let corner = plane[self.index(xi, yi, zi)];
                    for (o, c) in out.iter_mut().zip(corner) {
                        *o += c * w;
                    }
                }
            }
        }
        out
    }

    /// The irradiance the renderer would use at a point and normal.
    ///
    /// Mirrors `gi.wgsl`'s `gi_irradiance` statement for statement — the M41
    /// arrangement, where a CPU evaluator and a shader compute the same thing
    /// and a test holds them together. This one answers `engine gi-probe`.
    pub fn sample(&self, at: Vec3, normal: Vec3) -> Vec3 {
        let n = normal.normalize_or_zero();
        let c0 = self.fetch(0, at);
        let c1 = self.fetch(1, at);
        let c2 = self.fetch(2, at);
        let c3 = self.fetch(3, at);
        let mut out = Vec3::ZERO;
        for channel in 0..CHANNELS {
            let linear = c1[channel] * n.x + c2[channel] * n.y + c3[channel] * n.z;
            out[channel] = c0[channel] + LINEAR_GAIN * linear;
        }
        out
    }

    /// How much of the sky this point can see, `1.0` in the open and `0.0`
    /// buried. Carried in the alpha of coefficient 0 rather than computed from
    /// the coefficients, because it is a scalar a shader wants without three
    /// extra fetches.
    pub fn openness(&self, at: Vec3) -> f32 {
        self.fetch(0, at)[3]
    }

    /// How strongly GI applies here: `intensity`, faded to zero over `blend`
    /// metres at the boundary and zero outside.
    ///
    /// A fade rather than a step, and it costs nothing in an open scene: an
    /// unoccluded probe reconstructs the same value the fallback would have
    /// given, so mixing between them is invisible where there is nothing to
    /// occlude. The fade only ever shows where GI is actually doing something.
    // Both negations are deliberate, `fetch`'s reason: a NaN position must fall
    // out of the volume rather than through the fade, and a NaN `blend` must not
    // divide. `partial_cmp` here would be three lines that say less.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    pub fn weight(&self, at: Vec3) -> f32 {
        let max = self.max_corner();
        let inside = (at - self.origin).min(max - at);
        let distance = inside.x.min(inside.y).min(inside.z);
        if !(distance > 0.0) {
            return 0.0;
        }
        if !(self.blend > 0.0) {
            return self.intensity;
        }
        self.intensity * (distance / self.blend).clamp(0.0, 1.0)
    }
}

/// Fold a bake against one frame's lighting.
///
/// The two live sky bands come from `environment` when the scene draws a sky.
/// When it does not, both bands are unit and the normalization is unit, which
/// makes the fold collapse to the flat `AmbientLight` the pre-M35 engine used —
/// still occluded by geometry, because that is what the transfer holds, but no
/// longer tinted by a sky that is not being drawn. That is the honest answer for
/// a scene with `sky: false`: there is no sky colour to redistribute, only a
/// uniform fill to block.
pub fn evaluate(
    baked: &BakedGi,
    volume: &LightProbeVolume,
    lights: &ResolvedLights,
    environment: &EnvironmentSettings,
) -> IrradianceField {
    let (zenith, ground, mean) = if environment.sky {
        let mean =
            ((environment.sky_zenith + environment.sky_ground) * 0.5).max(Vec3::splat(MEAN_FLOOR));
        (environment.sky_zenith, environment.sky_ground, mean)
    } else {
        (Vec3::ONE, Vec3::ONE, Vec3::ONE)
    };
    // Per channel, exactly as `sky_ambient` normalizes: against the mean of the
    // two bands and not against mean luminance. M16 fought for that choice and
    // the reason is in `mesh.wgsl` — luminance normalization turns every
    // up-facing surface blue-gray under a saturated sky.
    let gain = lights.ambient / mean;

    let mut planes = std::array::from_fn(|_| Vec::with_capacity(baked.probes.len()));
    for probe in &baked.probes {
        let z = &probe.sky[BAND_ZENITH];
        let g = &probe.sky[BAND_GROUND];
        for (coefficient, plane) in planes.iter_mut().enumerate() {
            let mut rgba = [0.0f32; 4];
            for channel in 0..CHANNELS {
                let i = coefficient * CHANNELS + channel;
                rgba[channel] = gain[channel] * (zenith[channel] * z[i] + ground[channel] * g[i]);
            }
            if coefficient == 0 {
                // The constant band summed over both bands is the fraction of
                // the sphere that reached this probe, tint and all; averaged
                // over channels it is the scalar a grass root or an edge fade
                // wants. Unoccluded it is 1.0, because the two bands' weights
                // partition every direction.
                let openness = (0..CHANNELS).map(|c| z[c] + g[c]).sum::<f32>() / CHANNELS as f32;
                rgba[3] = openness.clamp(0.0, 1.0);
            }
            plane.push(rgba);
        }
    }

    IrradianceField {
        grid: baked.header.grid,
        origin: Vec3::from_array(baked.header.origin),
        spacing: baked.header.spacing,
        intensity: volume.intensity,
        blend: volume.blend,
        planes,
    }
}

/// The volume a scene renders with, and where its bake sits.
///
/// **A scene has at most one**, which is `multiple_light_probe_volumes` at
/// `validate` rather than a rule here. The shader holds a single field — four 3D
/// textures and one placement in the frame uniform — and a second volume would
/// mean either four more bindings per volume or giving up the hardware trilinear
/// filtering that is the whole reason the field is a texture. Making that an
/// error rather than a silent pick follows `DirectionalLight` and `AmbientLight`,
/// which are at-most-one for the same reason and were errors already.
///
/// The name sort is therefore not a resolution rule; it is what keeps a scene
/// that reached here **unvalidated** — the editor's live view, mid-edit —
/// picking the same volume every frame instead of flickering between two.
pub fn rendered_volume(scene: &crate::scene::Scene) -> Option<(String, LightProbeVolume, Vec3)> {
    let mut names: Vec<&str> = scene
        .names()
        .filter(|name| {
            scene
                .entity(name)
                .is_some_and(|entity| scene.world.get::<&LightProbeVolume>(entity).is_ok())
        })
        .collect();
    names.sort();

    let name = names.first()?;
    let entity = scene.entity(name).expect("filtered above");
    let volume = (*scene
        .world
        .get::<&LightProbeVolume>(entity)
        .expect("filtered above"))
    .clone();
    let transform = scene
        .world
        .get::<&crate::components::Transform>(entity)
        .map(|t| *t)
        .unwrap_or_default();
    let origin = transform.position - transform.scale * 0.5;
    Some((name.to_string(), volume, origin))
}

/// Load a scene's bake and fold it for this frame — everything a caller needs to
/// hand the renderer a field.
///
/// `base_dir` is the **scene file's** directory, because `bake` is relative to
/// it like every other asset path (invariant 3). Returns `None` when the scene
/// has no volume, and also when the file is missing or malformed: those are
/// `gi_bake_missing` and `gi_bake_malformed`, reported by `validate`, which
/// every render path runs first. A render is the wrong place to discover a
/// broken bake and the worst place to report one.
pub fn field_for_scene(
    scene: &crate::scene::Scene,
    base_dir: &std::path::Path,
    lights: &ResolvedLights,
    environment: &EnvironmentSettings,
) -> Option<IrradianceField> {
    let (volume, baked) = load_for_scene(scene, base_dir)?;
    Some(evaluate(&baked, &volume, lights, environment))
}

/// Read and parse the scene's bake, without folding it.
///
/// Split out for the viewer, which holds the result for the life of the session
/// and folds it once a frame: the fold is what `daylight` moves, and re-reading
/// an NDJSON file sixty times a second to learn the same numbers is not.
pub fn load_for_scene(
    scene: &crate::scene::Scene,
    base_dir: &std::path::Path,
) -> Option<(LightProbeVolume, BakedGi)> {
    let (_, volume, _) = rendered_volume(scene)?;
    let text = std::fs::read_to_string(base_dir.join(&volume.bake)).ok()?;
    let baked = BakedGi::parse(&text).ok()?;
    Some((volume, baked))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gi::bake::{bake_volume, BakeParams, Bvh};
    use crate::gi::{Probe, NUMBERS_PER_BASIS};

    fn field_from(
        probes: Vec<Probe>,
        grid: [u32; 3],
        environment: &EnvironmentSettings,
    ) -> IrradianceField {
        let baked = BakedGi {
            header: crate::gi::BakeHeader {
                format: crate::gi::FORMAT.into(),
                scene: "t.json".into(),
                entity: "L".into(),
                inputs_hash: "0".into(),
                grid,
                origin: [0.0, 0.0, 0.0],
                spacing: 1.0,
                basis: [("sky".to_string(), crate::gi::SKY_BANDS as u32)]
                    .into_iter()
                    .collect(),
                samples: 64,
                bounces: 1,
                relocated: 0,
            },
            probes,
        };
        let volume = LightProbeVolume {
            spacing: 1.0,
            bake: "t.gi.json".into(),
            bounces: 1,
            intensity: 1.0,
            blend: 0.0,
        };
        let lights = ResolvedLights {
            sun_direction: Vec3::NEG_Y,
            sun_color: Vec3::ONE,
            ambient: Vec3::splat(0.3),
            points: Default::default(),
            point_count: 0,
        };
        evaluate(&baked, &volume, &lights, environment)
    }

    /// `sky_ambient`, transcribed from `mesh.wgsl`. The thing GI must reproduce
    /// where there is nothing in the way.
    fn sky_ambient(n: Vec3, ambient: Vec3, env: &EnvironmentSettings) -> Vec3 {
        let up = n.y * 0.5 + 0.5;
        let mixed = env.sky_ground + (env.sky_zenith - env.sky_ground) * up;
        let mean = ((env.sky_ground + env.sky_zenith) * 0.5).max(Vec3::splat(MEAN_FLOOR));
        ambient * (mixed / mean)
    }

    #[test]
    fn an_unoccluded_probe_reproduces_sky_ambient() {
        // Design §3.1, and the single most load-bearing assertion in M35: an
        // open-sky probe must evaluate to the fill term the engine already
        // computes. If this drifts, turning GI on changes the brightness of
        // every open scene and no render difference is attributable to
        // geometry any more.
        let environment = EnvironmentSettings {
            sky: true,
            sky_zenith: Vec3::new(0.2, 0.4, 0.9),
            sky_ground: Vec3::new(0.25, 0.2, 0.15),
            ..Default::default()
        };

        // An empty world: every ray escapes, which is the unoccluded case by
        // construction rather than by a hand-written transfer vector.
        let bvh = Bvh::build(Vec::new());
        let (probes, _) = bake_volume(
            &bvh,
            Vec3::ZERO,
            [2, 2, 2],
            1.0,
            &BakeParams {
                samples: 4096,
                bounces: 1,
            },
        );
        let field = field_from(probes, [2, 2, 2], &environment);
        let ambient = Vec3::new(0.3, 0.3, 0.3);

        for n in [
            Vec3::Y,
            -Vec3::Y,
            Vec3::X,
            Vec3::new(0.0, 0.7, 0.7).normalize(),
            Vec3::new(-0.3, -0.5, 0.8).normalize(),
        ] {
            let got = field.sample(Vec3::splat(0.5), n);
            let want = sky_ambient(n, ambient, &environment);
            let error = (got - want).abs().max_element();
            assert!(
                error < 2.0e-3,
                "normal {n:?}: GI gave {got:?}, sky_ambient gives {want:?} (error {error})"
            );
        }
    }

    #[test]
    fn with_no_sky_the_fold_collapses_to_flat_ambient() {
        // A scene with `sky: false` has no sky colour to redistribute. GI must
        // then land on `frame.ambient.rgb` — the other of the two lines it
        // replaces — or the AMBIENT anchor's unoccluded case is not exact.
        let environment = EnvironmentSettings {
            sky: false,
            ..Default::default()
        };
        let bvh = Bvh::build(Vec::new());
        let (probes, _) = bake_volume(
            &bvh,
            Vec3::ZERO,
            [2, 2, 2],
            1.0,
            &BakeParams {
                samples: 4096,
                bounces: 1,
            },
        );
        let field = field_from(probes, [2, 2, 2], &environment);

        for n in [Vec3::Y, -Vec3::Y, Vec3::X] {
            let got = field.sample(Vec3::splat(0.5), n);
            let error = (got - Vec3::splat(0.3)).abs().max_element();
            assert!(error < 2.0e-3, "normal {n:?} gave {got:?}, wanted flat 0.3");
        }
    }

    #[test]
    fn intensity_zero_is_the_pre_m35_look() {
        // The one-field A/B: `intensity: 0.0` must weigh GI at nothing
        // everywhere inside the volume, so the mix lands on the fallback and
        // the render is the pre-M35 one.
        let mut field = field_from(
            vec![
                Probe {
                    p: [0, 0, 0],
                    sky: vec![vec![0.0; NUMBERS_PER_BASIS]; 2],
                };
                8
            ]
            .into_iter()
            .enumerate()
            .map(|(i, mut p)| {
                p.p = [i as u32 % 2, (i as u32 / 2) % 2, i as u32 / 4];
                p
            })
            .collect(),
            [2, 2, 2],
            &EnvironmentSettings::default(),
        );
        field.intensity = 0.0;
        assert_eq!(field.weight(Vec3::splat(0.5)), 0.0);
    }

    #[test]
    fn the_boundary_fades_rather_than_steps() {
        let mut field = field_from(
            (0..8)
                .map(|i: u32| Probe {
                    p: [i % 2, (i / 2) % 2, i / 4],
                    sky: vec![vec![0.0; NUMBERS_PER_BASIS]; 2],
                })
                .collect(),
            [2, 2, 2],
            &EnvironmentSettings::default(),
        );
        field.blend = 0.25;

        // Outside is zero, the middle is full, and the shell between is a ramp.
        assert_eq!(field.weight(Vec3::new(-0.1, 0.5, 0.5)), 0.0);
        assert_eq!(field.weight(Vec3::splat(0.5)), 1.0);
        let edge = field.weight(Vec3::new(0.125, 0.5, 0.5));
        assert!(
            (edge - 0.5).abs() < 1.0e-6,
            "half a blend distance in should be half weight, got {edge}"
        );
    }

    #[test]
    fn a_sheltered_probe_is_darker_than_an_open_one() {
        // The assertion a picture cannot make. A ceiling over one probe and
        // nothing over the other, one bake, and the numbers have to differ in
        // the direction that says GI is occluding rather than merely running.
        let ceiling = crate::gi::bake::Triangle::new(
            Vec3::new(-8.0, 4.0, -8.0),
            Vec3::new(8.0, 4.0, -8.0),
            Vec3::new(0.0, 4.0, 8.0),
            Vec3::splat(0.2),
        );
        let bvh = Bvh::build(vec![ceiling]);
        let params = BakeParams {
            samples: 512,
            bounces: 1,
        };
        let (sheltered, _) = bake_volume(&bvh, Vec3::ZERO, [2, 2, 2], 1.0, &params);
        let (open, _) = bake_volume(&bvh, Vec3::new(100.0, 0.0, 100.0), [2, 2, 2], 1.0, &params);

        let environment = EnvironmentSettings {
            sky: true,
            ..Default::default()
        };
        let under = field_from(sheltered, [2, 2, 2], &environment);
        let outside = field_from(open, [2, 2, 2], &environment);

        let a = under.sample(Vec3::splat(0.5), Vec3::Y).length();
        let b = outside
            .sample(Vec3::new(100.5, 0.5, 100.5), Vec3::Y)
            .length();
        assert!(
            a < b * 0.75,
            "an up-facing surface under a ceiling gathered {a}, in the open {b}"
        );
        assert!(
            under.openness(Vec3::splat(0.5)) < outside.openness(Vec3::new(100.5, 0.5, 100.5)),
            "openness must fall under a ceiling too, or the scalar is not measuring what it says"
        );
    }
}
