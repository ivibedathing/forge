//! Deterministic CPU particle simulation (M13).
//!
//! Particle state is derived, disposable, and reproducible: a
//! [`ParticleSystem`] built from a world and stepped N times on the fixed
//! clock always produces the same particles for the same files — the emitter's
//! `seed` drives a private xorshift generator, and nothing here touches wall
//! clocks, global RNGs, or the GPU. That is what lets smoke appear in
//! `engine screenshot --steps N` and diff-render bit-exactly against a
//! committed baseline.
//!
//! The system order is: animations → scripts → physics → **particles** →
//! render. Particles read the emitter's transform *after* physics and scripts
//! have moved it, so an exhaust emitter parented to the demo truck trails the
//! truck's actual path. Nothing reads particle state back: it is never baked
//! and never traced.

use glam::Vec3;
use hecs::{Entity, World};

use crate::components::{Name, ParticleBlend, ParticleEmitter, Transform};

/// One live particle. Positions and velocities are world-space; emitters do
/// not drag their old smoke along when they move.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Particle {
    position: Vec3,
    velocity: Vec3,
    age: f32,
    /// This particle's own lifespan in seconds, fixed at birth — the emitter's
    /// `lifetime` scaled by its `lifetime_jitter` draw. Stored per particle
    /// rather than read from the component so a population can die ragged.
    lifetime: f32,
    /// Multiplier on both `start_size` and `end_size`, fixed at birth, so a
    /// jittered particle keeps the authored growth curve at a different scale.
    size_scale: f32,
    /// Where in the turbulence field this particle samples. Two particles on
    /// the same trajectory with different offsets wander differently, which is
    /// what stops turbulence from carving one shared braid every particle
    /// follows. Zero (and unused) unless `turbulence` is on.
    noise_offset: Vec3,
}

/// One billboard to draw: plain data, no GPU types, so extraction is testable
/// headlessly — the same split `RenderItem` uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParticleInstance {
    pub position: Vec3,
    /// Billboard half-size in world units.
    pub size: f32,
    /// Linear RGB (unlit).
    pub color: Vec3,
    pub alpha: f32,
    /// World-space velocity, carried so the renderer can stretch the sprite
    /// along the direction of travel. Meaningless when `stretch` is 0.
    pub velocity: Vec3,
    /// Seconds of travel to elongate the billboard by; 0 keeps it round.
    pub stretch: f32,
    /// Whether this sprite adds light to the scene or occludes it.
    pub blend: ParticleBlend,
}

/// Per-emitter runtime state. The component stays authoritative for every
/// parameter — it is re-read each step, so an animated or live-edited `rate`
/// takes effect immediately; only the particles themselves live here.
struct EmitterState {
    entity: Entity,
    rng: u32,
    /// Fractional spawns carried between steps, so `rate * dt < 1` still
    /// emits at the authored long-run rate.
    credit: f32,
    particles: Vec<Particle>,
}

/// All emitters of one scene, stepped together on the fixed clock.
pub struct ParticleSystem {
    emitters: Vec<EmitterState>,
}

impl ParticleSystem {
    /// Collect the world's emitters. Ordered by entity name so iteration —
    /// and therefore RNG consumption and draw order — never depends on
    /// archetype layout.
    pub fn build(world: &World) -> Self {
        let mut named: Vec<(String, EmitterState)> = world
            .query::<(Entity, &Name, &ParticleEmitter)>()
            .iter()
            .map(|(entity, name, emitter)| {
                (
                    name.0.clone(),
                    EmitterState {
                        entity,
                        rng: seed_state(emitter.seed),
                        credit: 0.0,
                        particles: Vec::new(),
                    },
                )
            })
            .collect();
        named.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            emitters: named.into_iter().map(|(_, state)| state).collect(),
        }
    }

    /// Whether the world has any emitter at all — the viewer uses this to
    /// decide it needs a live simulation.
    pub fn scene_has_emitters(world: &World) -> bool {
        world.query::<&ParticleEmitter>().iter().next().is_some()
    }

    /// True when the system tracks no emitters (stepping would do nothing).
    pub fn is_empty(&self) -> bool {
        self.emitters.is_empty()
    }

    /// Total live particles across all emitters.
    pub fn live_particles(&self) -> usize {
        self.emitters.iter().map(|e| e.particles.len()).sum()
    }

    /// Advance every emitter by one fixed step.
    ///
    /// Existing particles age (and die), then integrate, then new ones spawn
    /// at the emitter's current position with age 0 — a newborn renders where
    /// it was born and takes its first move next step.
    pub fn step(&mut self, world: &World, dt: f32) {
        for state in &mut self.emitters {
            let Ok(emitter) = world.get::<&ParticleEmitter>(state.entity) else {
                continue;
            };
            let transform = world
                .get::<&Transform>(state.entity)
                .map(|t| *t)
                .unwrap_or_default();

            for particle in &mut state.particles {
                particle.age += dt;
            }
            state.particles.retain(|p| p.age < p.lifetime);

            // Semi-implicit Euler, then damping — `drag` halves nothing
            // exactly, it is `v / (1 + drag·dt)` per step, the stable form.
            //
            // Turbulence rides in as extra acceleration, and is computed only
            // when it is on: an emitter that did not ask for it must integrate
            // the exact expression it did before turbulence existed.
            let damping = 1.0 / (1.0 + emitter.drag * dt);
            if emitter.turbulence > 0.0 {
                let inv_scale = 1.0 / emitter.turbulence_scale;
                for particle in &mut state.particles {
                    let swirl = turbulence_field(
                        particle.position * inv_scale + particle.noise_offset,
                    ) * emitter.turbulence;
                    particle.velocity =
                        (particle.velocity + (emitter.acceleration + swirl) * dt) * damping;
                    particle.position += particle.velocity * dt;
                }
            } else {
                for particle in &mut state.particles {
                    particle.velocity =
                        (particle.velocity + emitter.acceleration * dt) * damping;
                    particle.position += particle.velocity * dt;
                }
            }

            state.credit += emitter.rate * dt;
            let rotation = transform.quat();
            while state.credit >= 1.0 {
                state.credit -= 1.0;
                if state.particles.len() >= emitter.max_particles as usize {
                    // At the cap the spawn is dropped, not deferred — and the
                    // RNG is not consumed, so the drop is deterministic too.
                    continue;
                }
                // Draw order is part of the format's contract: direction, then
                // disc, then speed, size, lifetime, turbulence — each skipped
                // entirely when its field is off, so adding a field to this
                // list can never disturb a scene that leaves it at its default.
                let direction = sample_cone(&mut state.rng, emitter.spread.to_radians());

                let mut position = transform.position;
                if emitter.radius > 0.0 {
                    // Uniform over the disc (sqrt, or the samples crowd the
                    // centre), in the plane perpendicular to the aim, then
                    // carried into world space by the entity's rotation.
                    let r = emitter.radius * unit(&mut state.rng).sqrt();
                    let azimuth = unit(&mut state.rng) * std::f32::consts::TAU;
                    let offset = Vec3::new(r * azimuth.cos(), r * azimuth.sin(), 0.0);
                    position += rotation * offset;
                }

                let mut speed = emitter.speed;
                if emitter.speed_jitter > 0.0 {
                    speed *= jitter(&mut state.rng, emitter.speed_jitter);
                }
                let mut size_scale = 1.0;
                if emitter.size_jitter > 0.0 {
                    size_scale = jitter(&mut state.rng, emitter.size_jitter);
                }
                let mut lifetime = emitter.lifetime;
                if emitter.lifetime_jitter > 0.0 {
                    lifetime *= jitter(&mut state.rng, emitter.lifetime_jitter);
                }
                let mut noise_offset = Vec3::ZERO;
                if emitter.turbulence > 0.0 {
                    // An arbitrary span; large enough that two particles rarely
                    // land in the same cell of the field.
                    noise_offset = Vec3::new(
                        unit(&mut state.rng) * 64.0,
                        unit(&mut state.rng) * 64.0,
                        unit(&mut state.rng) * 64.0,
                    );
                }

                state.particles.push(Particle {
                    position,
                    velocity: rotation * direction * speed,
                    age: 0.0,
                    lifetime,
                    size_scale,
                    noise_offset,
                });
            }
        }
    }

    /// Flatten every live particle into draw data, `start_*`→`end_*` values
    /// interpolated by each particle's fraction of life lived.
    pub fn instances(&self, world: &World) -> Vec<ParticleInstance> {
        let mut out = Vec::with_capacity(self.live_particles());
        for state in &self.emitters {
            let Ok(emitter) = world.get::<&ParticleEmitter>(state.entity) else {
                continue;
            };
            for particle in &state.particles {
                let t = (particle.age / particle.lifetime).clamp(0.0, 1.0);
                out.push(ParticleInstance {
                    position: particle.position,
                    size: lerp(emitter.start_size, emitter.end_size, t) * particle.size_scale,
                    color: emitter.start_color.lerp(emitter.end_color, t),
                    alpha: lerp(emitter.start_alpha, emitter.end_alpha, t),
                    velocity: particle.velocity,
                    stretch: emitter.stretch,
                    blend: emitter.blend,
                });
            }
        }
        out
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Turn the authored seed into a non-zero xorshift state via a splitmix-style
/// finalizer, so seeds 0, 1, 2… diverge immediately rather than after a few
/// correlated draws.
fn seed_state(seed: u32) -> u32 {
    let mut z = seed.wrapping_add(0x9E37_79B9);
    z = (z ^ (z >> 16)).wrapping_mul(0x21F0_AAAD);
    z = (z ^ (z >> 15)).wrapping_mul(0x735A_2D97);
    z ^= z >> 15;
    if z == 0 {
        0x9E37_79B9
    } else {
        z
    }
}

/// xorshift32 — tiny, fast, and fully specified here so the sequence can
/// never change under a dependency upgrade.
fn next(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Uniform in `[0, 1)`, from the generator's top 24 bits (an f32 mantissa).
fn unit(state: &mut u32) -> f32 {
    (next(state) >> 8) as f32 / 16_777_216.0
}

/// A multiplier uniformly distributed in `[1 - amount, 1 + amount]` — one
/// random draw, the shape every `*_jitter` field takes.
fn jitter(state: &mut u32, amount: f32) -> f32 {
    1.0 + (unit(state) * 2.0 - 1.0) * amount
}

/// Hash of three lattice coordinates plus a salt, avalanched into the full 32
/// bits. Specified here, like the xorshift, so no dependency can change what
/// the turbulence field looks like.
fn hash3(x: i32, y: i32, z: i32, salt: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ (z as u32).wrapping_mul(0xCB1A_B31F)
        ^ salt.wrapping_mul(0x1657_1FA5);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

/// Smooth value noise in `[-1, 1]`: trilinear interpolation between hashed
/// lattice corners, with the weights run through smoothstep so the field is
/// continuous in its first derivative and a particle crossing a cell boundary
/// does not visibly kink.
///
/// Value noise rather than gradient (Perlin) noise on purpose — it is a third
/// of the arithmetic, and the difference (value noise's slight axis alignment)
/// is invisible once three of these are combined into a vector and integrated
/// over a particle's path.
fn noise3(p: Vec3, salt: u32) -> f32 {
    let base = p.floor();
    let frac = p - base;
    let (ix, iy, iz) = (base.x as i32, base.y as i32, base.z as i32);

    // smoothstep(t) = t²(3 - 2t)
    let w = frac * frac * (Vec3::splat(3.0) - 2.0 * frac);

    let corner = |dx: i32, dy: i32, dz: i32| -> f32 {
        // Hash to [-1, 1].
        (hash3(ix + dx, iy + dy, iz + dz, salt) >> 8) as f32 / 8_388_608.0 - 1.0
    };

    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), w.x);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), w.x);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), w.x);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), w.x);
    lerp(lerp(x00, x10, w.y), lerp(x01, x11, w.y), w.z)
}

/// The swirl a particle feels at a point: three decorrelated noise fields, one
/// per axis, each roughly in `[-1, 1]`.
///
/// This is not divergence-free (a true curl field would be), and it does not
/// need to be: nothing here conserves mass, and what the eye reads as fire is
/// coherent lateral wander, which three independent smooth fields deliver at a
/// fraction of the cost.
fn turbulence_field(p: Vec3) -> Vec3 {
    Vec3::new(noise3(p, 0), noise3(p, 1), noise3(p, 2))
}

/// A direction uniformly distributed inside the cone of `half_angle` radians
/// around local −Z (the emitter's aim, per the camera/light convention).
fn sample_cone(state: &mut u32, half_angle: f32) -> Vec3 {
    // Uniform over the spherical cap: cos φ uniform in [cos θ, 1].
    let cos_phi = 1.0 - unit(state) * (1.0 - half_angle.cos());
    let sin_phi = (1.0 - cos_phi * cos_phi).max(0.0).sqrt();
    let azimuth = unit(state) * std::f32::consts::TAU;
    Vec3::new(
        sin_phi * azimuth.cos(),
        sin_phi * azimuth.sin(),
        -cos_phi,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scene;

    fn scene(emitter_json: &str) -> Scene {
        let source = format!(
            r#"{{"name":"s","entities":[
                {{"name":"Puff","components":[
                    {{"type":"Transform","position":[1.0,2.0,3.0],"rotation":[90.0,0.0,0.0]}},
                    {emitter_json}
                ]}}
            ]}}"#
        );
        Scene::from_source(&source, "s.json").expect("test scene should validate")
    }

    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn same_seed_same_steps_identical_particles() {
        let scene = scene(r#"{"type":"ParticleEmitter","rate":30.0,"spread":45.0,"seed":7}"#);
        let run = || {
            let mut system = ParticleSystem::build(&scene.world);
            for _ in 0..120 {
                system.step(&scene.world, DT);
            }
            system.instances(&scene.world)
        };
        let (a, b) = (run(), run());
        assert!(!a.is_empty());
        assert_eq!(a, b, "identical builds must produce identical particles");
    }

    #[test]
    fn different_seeds_diverge() {
        let a = scene(r#"{"type":"ParticleEmitter","rate":30.0,"spread":45.0,"seed":0}"#);
        let b = scene(r#"{"type":"ParticleEmitter","rate":30.0,"spread":45.0,"seed":1}"#);
        let run = |scene: &Scene| {
            let mut system = ParticleSystem::build(&scene.world);
            for _ in 0..60 {
                system.step(&scene.world, DT);
            }
            system.instances(&scene.world)
        };
        assert_ne!(run(&a), run(&b), "seeds 0 and 1 must not emit in lockstep");
    }

    #[test]
    fn spawn_rate_is_exact_over_whole_seconds() {
        // rate 10 at 60 Hz: credit accumulates 1/6 per step, so one second
        // spawns exactly 10 — fractional credit must carry, not truncate.
        let scene = scene(r#"{"type":"ParticleEmitter","rate":10.0,"lifetime":100.0}"#);
        let mut system = ParticleSystem::build(&scene.world);
        for _ in 0..60 {
            system.step(&scene.world, DT);
        }
        assert_eq!(system.live_particles(), 10);
    }

    #[test]
    fn particles_die_at_lifetime() {
        // rate 10, lifetime 0.5s: the population plateaus at ~5 once deaths
        // balance births, and never grows past it.
        let scene = scene(r#"{"type":"ParticleEmitter","rate":10.0,"lifetime":0.5}"#);
        let mut system = ParticleSystem::build(&scene.world);
        for _ in 0..300 {
            system.step(&scene.world, DT);
        }
        let plateau = system.live_particles();
        assert!(
            (4..=6).contains(&plateau),
            "expected ~5 live particles at equilibrium, got {plateau}"
        );
    }

    #[test]
    fn max_particles_caps_the_population() {
        let scene =
            scene(r#"{"type":"ParticleEmitter","rate":600.0,"lifetime":100.0,"max_particles":3}"#);
        let mut system = ParticleSystem::build(&scene.world);
        for _ in 0..60 {
            system.step(&scene.world, DT);
            assert!(system.live_particles() <= 3);
        }
        assert_eq!(system.live_particles(), 3);
    }

    #[test]
    fn emission_follows_local_negative_z() {
        // The fixture rotates [90, 0, 0], which carries local −Z to world +Y
        // (the aiming convention pin). With spread 0 every particle must rise.
        let scene =
            scene(r#"{"type":"ParticleEmitter","rate":60.0,"spread":0.0,"speed":2.0,"lifetime":10.0}"#);
        let mut system = ParticleSystem::build(&scene.world);
        for _ in 0..60 {
            system.step(&scene.world, DT);
        }
        let instances = system.instances(&scene.world);
        assert!(!instances.is_empty());
        for instance in &instances {
            assert_eq!(instance.position.x, 1.0);
            assert_eq!(instance.position.z, 3.0);
            assert!(
                instance.position.y >= 2.0,
                "a [90,0,0] emitter must emit upward, got {:?}",
                instance.position
            );
        }
    }

    #[test]
    fn start_end_values_interpolate_over_life() {
        // One particle, then advance to half-life: every start/end pair reads
        // at its midpoint.
        let scene = scene(
            r#"{"type":"ParticleEmitter","rate":1.0,"lifetime":2.0,"speed":0.0,
                "start_size":0.1,"end_size":0.3,
                "start_color":[1.0,0.0,0.0],"end_color":[0.0,0.0,1.0],
                "start_alpha":1.0,"end_alpha":0.0}"#,
        );
        let mut system = ParticleSystem::build(&scene.world);
        system.step(&scene.world, 1.0); // credit 1 → one particle, age 0
        system.step(&scene.world, 1.0); // ages to 1.0 = half of lifetime 2  (credit spawns a 2nd)
        let instances = system.instances(&scene.world);
        let half = instances
            .iter()
            .find(|i| (i.alpha - 0.5).abs() < 1e-6)
            .expect("a half-life particle should exist");
        assert!((half.size - 0.2).abs() < 1e-6);
        assert!((half.color - Vec3::new(0.5, 0.0, 0.5)).length() < 1e-6);
    }

    #[test]
    fn rate_zero_emits_nothing() {
        let scene = scene(r#"{"type":"ParticleEmitter","rate":0.0}"#);
        let mut system = ParticleSystem::build(&scene.world);
        for _ in 0..120 {
            system.step(&scene.world, DT);
        }
        assert_eq!(system.live_particles(), 0);
    }

    #[test]
    fn acceleration_and_drag_shape_the_motion() {
        // Straight-down emitter, upward acceleration: velocity must flip sign
        // eventually; with heavy drag it must instead nearly stop.
        let accelerated = scene(
            r#"{"type":"ParticleEmitter","rate":1.0,"spread":0.0,"speed":1.0,
                "lifetime":100.0,"acceleration":[0.0,5.0,0.0]}"#,
        );
        let mut system = ParticleSystem::build(&accelerated.world);
        system.step(&accelerated.world, 1.0);
        let born = system.instances(&accelerated.world)[0].position;
        for _ in 0..180 {
            system.step(&accelerated.world, DT);
        }
        let later = system.instances(&accelerated.world)[0].position;
        assert!(
            later.y > born.y + 1.0,
            "upward acceleration should overcome the initial launch, {born:?} → {later:?}"
        );

        let dragged = scene(
            r#"{"type":"ParticleEmitter","rate":1.0,"spread":0.0,"speed":10.0,
                "lifetime":100.0,"drag":50.0}"#,
        );
        let mut system = ParticleSystem::build(&dragged.world);
        system.step(&dragged.world, 1.0);
        let born = system.instances(&dragged.world)[0].position;
        for _ in 0..60 {
            system.step(&dragged.world, DT);
        }
        let later = system.instances(&dragged.world)[0].position;
        assert!(
            (later - born).length() < 0.5,
            "drag 50 should nearly freeze a particle, {born:?} → {later:?}"
        );
    }

    // ── The fire fields (M17) ───────────────────────────────────────────────

    #[test]
    fn defaulted_fire_fields_consume_no_randomness() {
        // The load-bearing property of the whole M17 particle change: an
        // emitter that opted into none of it must draw the same random numbers
        // in the same order as it did before those fields existed, or every
        // committed particle baseline moves. Proven by construction — a run
        // with all five fields at their defaults must equal a run of the same
        // emitter with the fields absent from the JSON entirely.
        let explicit = scene(
            r#"{"type":"ParticleEmitter","rate":40.0,"spread":35.0,"seed":9,
                "radius":0.0,"speed_jitter":0.0,"size_jitter":0.0,
                "lifetime_jitter":0.0,"turbulence":0.0,"stretch":0.0,
                "blend":"alpha"}"#,
        );
        let absent = scene(r#"{"type":"ParticleEmitter","rate":40.0,"spread":35.0,"seed":9}"#);
        let run = |scene: &Scene| {
            let mut system = ParticleSystem::build(&scene.world);
            for _ in 0..90 {
                system.step(&scene.world, DT);
            }
            system.instances(&scene.world)
        };
        let baseline = run(&absent);
        assert!(!baseline.is_empty());
        assert_eq!(
            baseline,
            run(&explicit),
            "spelling out the M17 defaults must change nothing at all"
        );
    }

    #[test]
    fn radius_spreads_birth_over_a_disc_perpendicular_to_the_aim() {
        // The fixture aims [90, 0, 0] — local −Z onto world +Y — so the birth
        // disc lies in the world XZ plane and every particle starts at the
        // emitter's own height.
        let scene = scene(
            r#"{"type":"ParticleEmitter","rate":60.0,"spread":0.0,"speed":0.0,
                "lifetime":10.0,"radius":2.0}"#,
        );
        let mut system = ParticleSystem::build(&scene.world);
        for _ in 0..60 {
            system.step(&scene.world, DT);
        }
        let instances = system.instances(&scene.world);
        assert!(instances.len() > 10);

        let mut max_offset: f32 = 0.0;
        for instance in &instances {
            // Speed 0, so a particle never leaves where it was born.
            assert!(
                (instance.position.y - 2.0).abs() < 1e-5,
                "the disc must be perpendicular to the aim, got {:?}",
                instance.position
            );
            let offset =
                ((instance.position.x - 1.0).powi(2) + (instance.position.z - 3.0).powi(2)).sqrt();
            assert!(offset <= 2.0 + 1e-5, "born outside the disc: {offset}");
            max_offset = max_offset.max(offset);
        }
        assert!(
            max_offset > 1.0,
            "a radius-2 disc should use its area, widest was {max_offset}"
        );
    }

    #[test]
    fn lifetime_jitter_makes_a_population_die_ragged() {
        // Same birth, same motion, different death times: with no jitter every
        // particle of one spawn-step shares an age exactly, so the set of
        // distinct alphas stays small. With jitter, each has its own clock.
        let ragged = scene(
            r#"{"type":"ParticleEmitter","rate":30.0,"speed":0.0,"lifetime":2.0,
                "lifetime_jitter":0.5,"seed":4}"#,
        );
        let mut system = ParticleSystem::build(&ragged.world);
        for _ in 0..120 {
            system.step(&ragged.world, DT);
        }
        let alphas: Vec<f32> = system
            .instances(&ragged.world)
            .iter()
            .map(|i| i.alpha)
            .collect();
        assert!(alphas.len() > 20);

        let plain = scene(
            r#"{"type":"ParticleEmitter","rate":30.0,"speed":0.0,"lifetime":2.0,"seed":4}"#,
        );
        let mut system = ParticleSystem::build(&plain.world);
        for _ in 0..120 {
            system.step(&plain.world, DT);
        }
        let plain_alphas: Vec<f32> = system
            .instances(&plain.world)
            .iter()
            .map(|i| i.alpha)
            .collect();

        // Without jitter, a particle's alpha is a function of its spawn step
        // alone, so N particles show at most N distinct fades and consecutive
        // ones are evenly spaced. Jitter breaks that ladder: two particles born
        // one step apart can be anywhere in their lives relative to each other.
        let spacing = |mut v: Vec<f32>| {
            v.sort_by(f32::total_cmp);
            let gaps: Vec<f32> = v.windows(2).map(|w| w[1] - w[0]).collect();
            let mean = gaps.iter().sum::<f32>() / gaps.len() as f32;
            gaps.iter().map(|g| (g - mean).abs()).sum::<f32>() / gaps.len() as f32 / mean
        };
        assert!(
            spacing(alphas) > spacing(plain_alphas) * 3.0,
            "jittered lifetimes must not fade in an even ladder"
        );
    }

    #[test]
    fn size_and_speed_jitter_vary_the_population() {
        let scene = scene(
            r#"{"type":"ParticleEmitter","rate":60.0,"spread":0.0,"speed":2.0,
                "lifetime":10.0,"start_size":0.5,"end_size":0.5,
                "size_jitter":0.4,"speed_jitter":0.5,"seed":11}"#,
        );
        let mut system = ParticleSystem::build(&scene.world);
        for _ in 0..60 {
            system.step(&scene.world, DT);
        }
        let instances = system.instances(&scene.world);

        let (min_size, max_size) = instances.iter().fold((f32::MAX, 0.0f32), |(lo, hi), i| {
            (lo.min(i.size), hi.max(i.size))
        });
        // ±40% of 0.5 spans [0.30, 0.70]; a sample of dozens should cover most
        // of that without ever leaving it.
        assert!(min_size >= 0.5 * 0.6 - 1e-5, "size below the jitter floor");
        assert!(max_size <= 0.5 * 1.4 + 1e-5, "size above the jitter ceiling");
        assert!(max_size - min_size > 0.15, "size_jitter did not spread");

        // Speed jitter shows up as particles born on the same step ending up at
        // different distances: the aim is a beam (spread 0) up +Y.
        let heights: Vec<f32> = instances.iter().map(|i| i.position.y).collect();
        let spread = heights.iter().cloned().fold(0.0f32, f32::max)
            - heights.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread > 0.5, "speed_jitter did not spread the column");
    }

    #[test]
    fn turbulence_pushes_particles_off_a_straight_line() {
        // A beam with no turbulence rises exactly along +Y. With turbulence the
        // same beam must wander off that axis — and must still be reproducible.
        let straight = scene(
            r#"{"type":"ParticleEmitter","rate":30.0,"spread":0.0,"speed":2.0,"lifetime":10.0}"#,
        );
        let curled = scene(
            r#"{"type":"ParticleEmitter","rate":30.0,"spread":0.0,"speed":2.0,"lifetime":10.0,
                "turbulence":6.0,"turbulence_scale":0.8,"seed":3}"#,
        );
        let run = |scene: &Scene| {
            let mut system = ParticleSystem::build(&scene.world);
            for _ in 0..120 {
                system.step(&scene.world, DT);
            }
            system.instances(&scene.world)
        };

        let off_axis = |instances: &[ParticleInstance]| -> f32 {
            instances
                .iter()
                .map(|i| ((i.position.x - 1.0).powi(2) + (i.position.z - 3.0).powi(2)).sqrt())
                .fold(0.0, f32::max)
        };
        let plain = run(&straight);
        assert!(off_axis(&plain) < 1e-4, "a spread-0 beam must not wander");

        let swirled = run(&curled);
        assert!(
            off_axis(&swirled) > 0.2,
            "turbulence 6 should visibly curl the plume, drifted {}",
            off_axis(&swirled)
        );
        assert_eq!(swirled, run(&curled), "turbulence must stay reproducible");
    }

    #[test]
    fn turbulence_is_smooth_along_a_path() {
        // The field has to be continuous, not per-step noise: a particle that
        // gets an independent random shove every step vibrates in place, while
        // one moving through a smooth field arcs. Sample the field along a line
        // and check consecutive values are close — a white-noise field would
        // jump by O(1) between samples a hundredth of a cell apart.
        let mut previous = turbulence_field(Vec3::new(0.0, 0.3, -0.7));
        let mut worst: f32 = 0.0;
        for i in 1..=200 {
            let p = Vec3::new(i as f32 * 0.01, 0.3, -0.7);
            let value = turbulence_field(p);
            worst = worst.max((value - previous).length());
            previous = value;
        }
        assert!(
            worst < 0.1,
            "the field must be smooth over a hundredth of a cell, jumped {worst}"
        );
    }

    #[test]
    fn turbulence_field_stays_in_range_and_is_not_constant() {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for i in 0..500 {
            let p = Vec3::new(i as f32 * 0.37, i as f32 * -0.71, i as f32 * 1.13);
            for component in turbulence_field(p).to_array() {
                lo = lo.min(component);
                hi = hi.max(component);
            }
        }
        assert!(lo >= -1.0 && hi <= 1.0, "noise left [-1, 1]: {lo}..{hi}");
        assert!(hi - lo > 1.0, "noise barely varies: {lo}..{hi}");
    }

    #[test]
    fn blend_and_stretch_reach_the_instances() {
        let scene = scene(
            r#"{"type":"ParticleEmitter","rate":10.0,"spread":0.0,"speed":3.0,
                "lifetime":10.0,"stretch":0.25,"blend":"additive"}"#,
        );
        let mut system = ParticleSystem::build(&scene.world);
        for _ in 0..30 {
            system.step(&scene.world, DT);
        }
        let instances = system.instances(&scene.world);
        assert!(!instances.is_empty());
        for instance in &instances {
            assert_eq!(instance.blend, ParticleBlend::Additive);
            assert_eq!(instance.stretch, 0.25);
            // The aim carries local −Z to world +Y, so the velocity the
            // renderer stretches along is the one that rises.
            assert!(instance.velocity.y > 0.0, "{:?}", instance.velocity);
        }
    }

    #[test]
    fn a_particles_lifespan_is_fixed_at_birth() {
        // Documented consequence of storing lifetime per particle: retiming an
        // emitter mid-run affects new particles, not live ones. Shorten the
        // component's lifetime below a live particle's age and it must still
        // finish its own life.
        let scene = scene(r#"{"type":"ParticleEmitter","rate":1.0,"lifetime":10.0,"speed":0.0}"#);
        let mut system = ParticleSystem::build(&scene.world);
        system.step(&scene.world, 1.0);
        for _ in 0..120 {
            system.step(&scene.world, DT);
        }
        // Two, not three: 120 additions of 1/60 land a hair under 2.0 of spawn
        // credit. The exact number is beside the point — what matters is that
        // it does not change when the component is retimed below their age.
        let live = system.live_particles();
        assert!(live >= 2, "the run should have spawned a few particles");

        {
            let entity = scene.entity("Puff").unwrap();
            let mut emitter = scene.world.get::<&mut ParticleEmitter>(entity).unwrap();
            emitter.lifetime = 0.5;
            // Stop emission too, so the count can only move by a death.
            emitter.rate = 0.0;
        }
        system.step(&scene.world, DT);
        assert_eq!(
            system.live_particles(),
            live,
            "shortening lifetime must not retroactively kill live particles"
        );
    }

    #[test]
    fn emitters_step_in_name_order() {
        // Two emitters, same seed: build order in the file is reversed
        // alphabetically, but the system must consume RNG in name order, so
        // the combined instance list is reproducible run to run.
        let source = r#"{"name":"s","entities":[
            {"name":"Zeta","components":[{"type":"Transform"},{"type":"ParticleEmitter","rate":5.0,"spread":30.0}]},
            {"name":"Alpha","components":[{"type":"Transform","position":[10.0,0.0,0.0]},{"type":"ParticleEmitter","rate":5.0,"spread":30.0}]}
        ]}"#;
        let scene = Scene::from_source(source, "s.json").unwrap();
        let run = || {
            let mut system = ParticleSystem::build(&scene.world);
            for _ in 0..30 {
                system.step(&scene.world, DT);
            }
            system.instances(&scene.world)
        };
        let instances = run();
        assert_eq!(instances, run());
        // Name order: Alpha's particles (x ≈ 10) precede Zeta's (x ≈ 0).
        let first_alpha = instances.iter().position(|i| i.position.x > 5.0);
        let first_zeta = instances.iter().position(|i| i.position.x < 5.0);
        assert!(first_alpha < first_zeta, "instances must list Alpha first");
    }
}
