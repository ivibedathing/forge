//! Buoyancy (M38) — `designs/buoyancy-design.md`.
//!
//! Archimedes, sampled. A floating body's collider is divided into vertical
//! columns; each column asks the water above it how deep it is; each pushes up
//! with the weight of the water it displaces, **at its own column**. That last
//! word is the whole model: forces applied away from the centre of mass make
//! torque, so a hull that rolls has more of itself under the surface on the low
//! side, and the righting moment falls out of the same sum as the lift. Nothing
//! here models pitch or roll separately, and nothing needs to.
//!
//! Everything this reads about a shape comes from the entity's own `Collider`,
//! through rapier: the displaced **volume** is the collider's exact volume, and
//! the columns are laid out over its **local** bounds, so they turn with the
//! hull. There is deliberately no authored volume or hull size — a second shape
//! description would drift from the first the day either was edited.
//!
//! The surface itself is `engine_core::water::Surface`, the same evaluator
//! `world.water_height` and `engine water-height` answer with, held to the
//! shader by the agreement test in `engine-render/tests/water.rs`. A body floats
//! at the height the render draws, and that is checkable in one command.

use engine_core::components::{Buoyancy as BuoyancyData, Name, Transform, Water};
use engine_core::water::Surface;
use glam::Vec3;
use hecs::{Entity, World};
use rapier3d::prelude::*;
use std::collections::HashMap;

use crate::PhysicsWorld;

/// One body that floats, resolved at build.
pub(crate) struct Buoyant {
    /// The rapier body the impulses land on. The hecs entity is deliberately
    /// not kept: nothing here reports, and a handle that outlives its body
    /// resolves to `None` on lookup, which is the removal case handled.
    pub(crate) handle: RigidBodyHandle,
    /// The `Water` entity's name. Resolved to a component every step rather
    /// than cached, because a water surface can be moved by a script or an
    /// animation and a boat has to follow the pond it is in.
    pub(crate) water: String,
    pub(crate) samples: u32,
    pub(crate) drag: f32,
    pub(crate) angular_drag: f32,
    /// The collider's own volume in m³, at density 1 — rapier's exact figure
    /// for the shape, so a sphere displaces a sphere and not its bounding box.
    pub(crate) volume: f32,
    /// The damping the `RigidBody` authored, which is the body's drag **in
    /// air**. Submerged drag is added on top of this and falls away with the
    /// body's submersion, so a hull thrown clear of the pond stops being
    /// dragged the moment it leaves.
    pub(crate) base_damping: (f32, f32),
}

impl PhysicsWorld {
    /// Collect the scene's floating bodies, in entity-name order.
    ///
    /// Name order rather than hecs order for the reason every other list in
    /// this crate is sorted: impulses to distinct bodies commute, but the ones
    /// this applies land through the solver in the order they were added, and
    /// "whatever archetype layout hecs chose" is not a thing a golden trace can
    /// promise across a rebuild.
    pub(crate) fn collect_buoyant(&mut self, world: &World) {
        let mut found: Vec<(String, Buoyant)> = Vec::new();
        for (entity, name, buoyancy) in world.query::<(Entity, &Name, &BuoyancyData)>().iter() {
            // Validation guarantees a dynamic body and a collider, so a miss
            // here means the component was added to a live world some other
            // way; skipping is the safe reading and costs the entity nothing
            // but its float.
            let Some(&handle) = self.body_of.get(&entity) else {
                continue;
            };
            let Some(body) = self.bodies.get(handle) else {
                continue;
            };
            if !body.is_dynamic() {
                continue;
            }

            // Sum over the body's colliders: one `Collider` component is one
            // collider today, but summing is the reading that stays right if
            // that ever stops being true, and it costs one iterator.
            let volume: f32 = body
                .colliders()
                .iter()
                .filter_map(|&handle| self.colliders.get(handle))
                .map(|collider| collider.shape().mass_properties(1.0).mass())
                .sum();
            // Negated so NaN fails: a collider whose volume did not compute is
            // one this body cannot displace water with, and treating it as
            // buoyant would apply a NaN impulse that poisons the whole solver.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            if !(volume > 0.0) {
                continue;
            }

            found.push((
                name.0.clone(),
                Buoyant {
                    handle,
                    water: buoyancy.water.clone(),
                    samples: buoyancy.samples.clamp(1, 4),
                    drag: buoyancy.drag,
                    angular_drag: buoyancy.angular_drag,
                    volume,
                    base_damping: (body.linear_damping(), body.angular_damping()),
                },
            ));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        self.buoyant = found.into_iter().map(|(_, body)| body).collect();
    }

    /// Push every floating body up by the weight of the water it displaces.
    ///
    /// Runs before the solver integrates, so a boat rises the same step the
    /// wave under it does.
    pub(crate) fn apply_buoyancy(&mut self, world: &World, time: f32) {
        // One prepared surface per water entity, not per body: packing `k`, `ω`
        // and `Q` is the same arithmetic for every hull in one pond.
        let mut surfaces: HashMap<&str, Surface> = HashMap::new();
        let mut water_query = world.query::<(&Name, &Water, &Transform)>();
        let waters: HashMap<&str, (&Water, &Transform)> = water_query
            .iter()
            .map(|(name, water, transform)| (name.0.as_str(), (water, transform)))
            .collect();

        let dt = self.parameters.dt;
        let gravity = self.gravity;
        // Moved out and back so the loop can read the float list and write to
        // `self.bodies` at once. The list is rebuilt only when the entity set
        // changes, so this is a pointer swap either side of the step.
        let floats = std::mem::take(&mut self.buoyant);

        for float in &floats {
            // A name that resolves to no water is validation's to refuse, and
            // it already has. Reaching it here means the surface was removed
            // from a live world, and the honest answer is that there is nothing
            // to float on.
            let Some(&(water, transform)) = waters.get(float.water.as_str()) else {
                continue;
            };
            let surface = surfaces
                .entry(float.water.as_str())
                .or_insert_with(|| Surface::new(water, transform));

            let Some(body) = self.bodies.get(float.handle) else {
                continue;
            };

            // The sample points ride in the **body's own frame**, and that is
            // the load-bearing decision in this function.
            //
            // Laying them over the body's *world* AABB instead is the obvious
            // first thing to write and it is unstable: a tilted hull has a
            // larger world AABB than an upright one, so its corner columns
            // acquire lever arms the hull does not physically have, and each
            // one pushes the tilt further. A raft dropped into a pond that way
            // tumbled end over end within a few hundred steps — measured, not
            // feared. Points fixed to the body cannot do that: each one is a
            // real quarter of a real hull, so the torque it makes is the torque
            // the shape has.
            //
            // Local XZ is treated as the deck plane. That is a convention, and
            // it is the engine's usual one — the same "the body's own axes mean
            // something" reading that puts forward down local −Z.
            let mut local: Option<Aabb> = None;
            for &handle in body.colliders() {
                let Some(collider) = self.colliders.get(handle) else {
                    continue;
                };
                let offset = collider
                    .position_wrt_parent()
                    .copied()
                    .unwrap_or_else(Pose::identity);
                let own = collider.shape().compute_aabb(&offset);
                local = Some(match local {
                    Some(current) => current.merged(&own),
                    None => own,
                });
            }
            let Some(local) = local else { continue };

            // The body's **draft**: how far it travels between first touching
            // the surface and being fully under. Local, like the sample points,
            // and that is a correction over the obvious reading.
            //
            // Taking it from the *world* AABB instead — "how tall does it stand
            // right now" — sounds more physical and floats a tilted hull too
            // high: a plank at 20° has a world AABB three times its thickness,
            // so the same submerged fraction puts its centre three times
            // further above the water, and the raft visibly hovers. Equilibrium
            // draft is a property of the shape and its density, not of which
            // way it happens to be leaning, and only the local extent says so.
            let draft = local.maxs.y - local.mins.y;
            // Negated so NaN fails, as above: a zero or NaN draft would divide
            // the submersion ramp by nothing.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            if !(draft > 1e-6) {
                continue;
            }

            let n = float.samples;
            let cells = (n * n) as f32;
            let pose = *body.position();
            let span = Vec3::new(
                (local.maxs.x - local.mins.x) / n as f32,
                0.0,
                (local.maxs.z - local.mins.z) / n as f32,
            );
            let mid_y = (local.mins.y + local.maxs.y) * 0.5;
            // The whole body's displaced mass, split evenly between columns and
            // then weighted by how submerged each one is.
            let per_column = water.density * float.volume / cells;

            let mut submerged = 0.0;
            let mut impulses: Vec<(Vec3, Vec3)> = Vec::new();
            for i in 0..n {
                for j in 0..n {
                    let point = pose
                        * Vec3::new(
                            local.mins.x + (i as f32 + 0.5) * span.x,
                            mid_y,
                            local.mins.z + (j as f32 + 0.5) * span.z,
                        );
                    // No water over this column — past the edge of the pond, or
                    // past the edge of the world. Not a force of zero with a
                    // surface at 0.0, which would hold a boat up over dry land.
                    let Some(sample) = surface.sample_at(point.x, point.z, time) else {
                        continue;
                    };
                    // The sample sits at the column's middle, so level with the
                    // surface is half under: submersion runs 0 to 1 across the
                    // body's own draft, centred on the point.
                    let fraction = (0.5 + (sample.height - point.y) / draft).clamp(0.0, 1.0);
                    if fraction <= 0.0 {
                        continue;
                    }
                    submerged += fraction;
                    // Straight up (against gravity), never along the surface
                    // normal. A normal-aligned force integrates into net
                    // transport across a wave train, and the moored buoy leaves
                    // the frame — see the design doc §6.
                    impulses.push((-gravity * (per_column * fraction * dt), point));
                }
            }

            let Some(body) = self.bodies.get_mut(float.handle) else {
                continue;
            };
            for (impulse, point) in impulses {
                body.apply_impulse_at_point(impulse, point, true);
            }

            // Drag scales with submersion, which is what makes it *water* drag
            // rather than a second body property: half in, half dragged; out,
            // and only the authored air damping is left.
            let mean = submerged / cells;
            body.set_linear_damping(float.base_damping.0 + float.drag * mean);
            body.set_angular_damping(float.base_damping.1 + float.angular_drag * mean);
        }

        self.buoyant = floats;
    }
}
