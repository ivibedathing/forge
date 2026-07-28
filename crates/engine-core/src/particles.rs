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

use crate::components::{Name, ParticleEmitter, Transform};

/// One live particle. Positions and velocities are world-space; emitters do
/// not drag their old smoke along when they move.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Particle {
    position: Vec3,
    velocity: Vec3,
    age: f32,
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
            state.particles.retain(|p| p.age < emitter.lifetime);

            // Semi-implicit Euler, then damping — `drag` halves nothing
            // exactly, it is `v / (1 + drag·dt)` per step, the stable form.
            let damping = 1.0 / (1.0 + emitter.drag * dt);
            for particle in &mut state.particles {
                particle.velocity =
                    (particle.velocity + emitter.acceleration * dt) * damping;
                particle.position += particle.velocity * dt;
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
                let direction = sample_cone(&mut state.rng, emitter.spread.to_radians());
                state.particles.push(Particle {
                    position: transform.position,
                    velocity: rotation * direction * emitter.speed,
                    age: 0.0,
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
                let t = (particle.age / emitter.lifetime).clamp(0.0, 1.0);
                out.push(ParticleInstance {
                    position: particle.position,
                    size: lerp(emitter.start_size, emitter.end_size, t),
                    color: emitter.start_color.lerp(emitter.end_color, t),
                    alpha: lerp(emitter.start_alpha, emitter.end_alpha, t),
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
