//! Runtime entity spawning (M37).
//!
//! A script cannot construct an entity out of nothing — that would put
//! geometry and materials in a `.rhai` and make the scene file stop being the
//! whole truth about what can exist (invariant 2). It names a
//! [`TemplateDef`](crate::scene::TemplateDef) the scene file declares, and this
//! module materializes a copy.
//!
//! That is the trade M14 breaking already took: a break spawns *pre-authored*
//! fragments. `templates` is the general form of the same bargain.
//!
//! What lives here is the bookkeeping the spawn needs and the ECS write it
//! performs. What does *not* live here is physics: a spawned body enters rapier
//! from the caller, between the script step and the physics step, because the
//! scripting crate does not depend on the physics crate and M13 and M14 both
//! declined to make it.

use std::collections::HashMap;

use glam::Vec3;
use hecs::{Entity, EntityBuilder, World};

use crate::components::{ComponentData, Name, Transform};
use crate::scene::TemplateDef;

/// The per-run ledger behind `world.spawn_entity` / `world.despawn_entity`.
///
/// Holds the templates, the instance counter, and which live instance came
/// from which template. Every field is a pure function of the spawn and
/// despawn calls made so far, in order — which is what keeps a replay of the
/// same input timeline producing the same instances with the same names.
#[derive(Debug, Default, Clone)]
pub struct SpawnLedger {
    /// The scene's templates, by name. Empty for a scene with no `templates`
    /// block, which is the pre-M37 engine exactly.
    templates: HashMap<String, TemplateDef>,
    /// The next instance index per template. **Never resets and never
    /// reuses**, even after every instance despawns: reuse would make a
    /// `--trace` ambiguous, because two different objects would appear under
    /// one row name.
    next: HashMap<String, u32>,
    /// How many instances of each template are live — spawned minus
    /// despawned. Checked against [`TemplateDef::limit`].
    live: HashMap<String, u32>,
    /// Which template each live instance came from, so a despawn knows which
    /// counter to decrement. An authored entity is absent from this map,
    /// which is exactly what makes despawning one cost nothing.
    origin: HashMap<String, String>,
    /// Total spawns this run, for the `simulate` report.
    total: u64,
}

/// Why a spawn produced no entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnRefusal {
    /// No template of that name. A script error at the call site — the name is
    /// a typo, and a silent no-op would hide it until the gun stopped firing.
    UnknownTemplate,
    /// The template's `limit` live instances already exist. **Not** an error:
    /// a gun that fires faster than its bullets expire is an ordinary game,
    /// and the empty name it returns is a value the script reads.
    AtLimit,
}

impl SpawnLedger {
    /// Build the ledger for a run from the scene's templates and the world it
    /// will spawn into.
    ///
    /// The world is read because **a baked scene is an ordinary scene file**:
    /// `simulate --bake` splices live spawned entities back in as entities, so
    /// a resumed run opens a file that already contains `Bullet#7`. Starting
    /// the counter at 1 would mint a second `Bullet#1` and put two entities
    /// under one name, which is invariant 4 broken at runtime. So any
    /// `Template#N` already in the file advances the counter past it and
    /// counts as live — which makes a resumed run behave exactly like the run
    /// that produced the file, including its `limit`.
    pub fn new(templates: &[TemplateDef], world: &World) -> Self {
        let mut ledger = Self {
            templates: templates
                .iter()
                .map(|t| (t.name.clone(), t.clone()))
                .collect(),
            ..Self::default()
        };
        if ledger.templates.is_empty() {
            return ledger;
        }

        let existing: Vec<String> = world
            .query::<&Name>()
            .iter()
            .map(|name| name.0.clone())
            .collect();
        for name in existing {
            let Some((stem, index)) = name.rsplit_once('#') else {
                continue;
            };
            let Ok(index) = index.parse::<u32>() else {
                continue;
            };
            if !ledger.templates.contains_key(stem) {
                continue;
            }
            let stem = stem.to_string();
            let next = ledger.next.entry(stem.clone()).or_insert(1);
            *next = (*next).max(index.saturating_add(1));
            *ledger.live.entry(stem.clone()).or_insert(0) += 1;
            ledger.origin.insert(name, stem);
        }
        ledger
    }

    /// Whether this scene declares anything spawnable. The sim loop's spawn
    /// handling is behind this, so a scene without templates takes exactly the
    /// pre-M37 path.
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// Every template name, sorted — what an unknown-template error offers as
    /// suggestions.
    pub fn template_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.templates.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// How many instances of `template` are live, or `None` if no such
    /// template — `world.spawn_count`.
    pub fn live_count(&self, template: &str) -> Option<u32> {
        self.templates
            .contains_key(template)
            .then(|| self.live.get(template).copied().unwrap_or(0))
    }

    /// The template's `limit`, or `None` if no such template.
    pub fn limit(&self, template: &str) -> Option<u32> {
        self.templates.get(template).map(|t| t.limit)
    }

    /// How many entities this run has spawned in total. Reported by
    /// `simulate` when it is non-zero.
    pub fn total_spawned(&self) -> u64 {
        self.total
    }

    /// Spawn one instance of `template` at `position`, into `world`.
    ///
    /// Returns the instance's name — `Bullet#1`, `Bullet#2`, … — which is what
    /// the script addresses it by for the rest of the step. The entity exists
    /// in the ECS before this returns, so `world.set_linear_velocity` on the
    /// very next line works; what it does not have yet is a rapier body.
    pub fn spawn(
        &mut self,
        world: &mut World,
        template: &str,
        position: Vec3,
    ) -> Result<(String, Entity), SpawnRefusal> {
        let Some(def) = self.templates.get(template) else {
            return Err(SpawnRefusal::UnknownTemplate);
        };
        let live = self.live.get(template).copied().unwrap_or(0);
        if live >= def.limit {
            return Err(SpawnRefusal::AtLimit);
        }

        // The counter is the instance index, not the live count: it counts
        // spawns, so a name is never reused after a despawn frees its slot.
        let index = self.next.entry(template.to_string()).or_insert(1);
        let name = format!("{template}#{index}");
        *index += 1;

        let entity = instantiate(world, def, &name, position);

        self.live.insert(template.to_string(), live + 1);
        self.origin.insert(name.clone(), template.to_string());
        self.total += 1;
        Ok((name, entity))
    }

    /// Record that `name` is gone, freeing its template's slot.
    ///
    /// Idempotent, and a no-op for an authored entity: the caller applies the
    /// despawn to the world and to physics, this only keeps the count honest.
    pub fn forget(&mut self, name: &str) {
        if let Some(template) = self.origin.remove(name) {
            if let Some(live) = self.live.get_mut(&template) {
                *live = live.saturating_sub(1);
            }
        }
    }
}

/// Build one entity from a template, at `position`, and put it in the world.
///
/// The position is an argument because everything else about a spawned entity
/// comes from the template and *where it goes* cannot: "somewhere else" is the
/// entire point of spawning. It overwrites `Transform.position` and nothing
/// else, so a template's authored `rotation` and `scale` survive; a template
/// with no `Transform` at all gets one, because an entity that physics or the
/// renderer will look at needs a placement more than it needs the omission
/// respected.
fn instantiate(world: &mut World, def: &TemplateDef, name: &str, position: Vec3) -> Entity {
    let mut components = def.components.clone();
    match components.iter_mut().find_map(|c| match c {
        ComponentData::Transform(t) => Some(t),
        _ => None,
    }) {
        Some(transform) => transform.position = position,
        None => components.push(ComponentData::Transform(Transform {
            position,
            ..Transform::default()
        })),
    }

    let mut builder = EntityBuilder::new();
    builder.add(Name(name.to_string()));
    for component in components {
        component.add_to(&mut builder);
    }
    world.spawn(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Mesh;

    fn template(name: &str, limit: u32) -> TemplateDef {
        TemplateDef {
            name: name.to_string(),
            limit,
            components: vec![
                ComponentData::Transform(Transform {
                    scale: Vec3::splat(2.0),
                    ..Transform::default()
                }),
                ComponentData::Mesh(Mesh {
                    asset: "builtin:cube".to_string(),
                }),
            ],
        }
    }

    #[test]
    fn names_count_spawns_and_never_reuse_after_a_despawn() {
        let mut ledger = SpawnLedger::new(&[template("Bullet", 2)], &World::new());
        let mut world = World::new();

        let (first, _) = ledger.spawn(&mut world, "Bullet", Vec3::ZERO).unwrap();
        let (second, _) = ledger.spawn(&mut world, "Bullet", Vec3::ZERO).unwrap();
        assert_eq!((first.as_str(), second.as_str()), ("Bullet#1", "Bullet#2"));

        assert_eq!(
            ledger.spawn(&mut world, "Bullet", Vec3::ZERO),
            Err(SpawnRefusal::AtLimit),
            "a third instance is over the limit of two"
        );

        // Freeing a slot lets the next spawn through — under a *new* name, so
        // a trace can never show two objects on one row.
        ledger.forget(&first);
        let (third, _) = ledger.spawn(&mut world, "Bullet", Vec3::ZERO).unwrap();
        assert_eq!(third, "Bullet#3");
        assert_eq!(ledger.live_count("Bullet"), Some(2));
        assert_eq!(ledger.total_spawned(), 3);
    }

    #[test]
    fn the_position_is_the_only_thing_the_call_overwrites() {
        let mut ledger = SpawnLedger::new(&[template("Bullet", 8)], &World::new());
        let mut world = World::new();
        let (_, entity) = ledger
            .spawn(&mut world, "Bullet", Vec3::new(1.0, 2.0, 3.0))
            .unwrap();

        let transform = *world.get::<&Transform>(entity).unwrap();
        assert_eq!(transform.position, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(
            transform.scale,
            Vec3::splat(2.0),
            "the template's scale survives the spawn"
        );
        assert_eq!(world.get::<&Mesh>(entity).unwrap().asset, "builtin:cube");
    }

    #[test]
    fn a_template_with_no_transform_still_lands_where_it_was_put() {
        let mut def = template("Spark", 4);
        def.components
            .retain(|c| !matches!(c, ComponentData::Transform(_)));
        let mut ledger = SpawnLedger::new(&[def], &World::new());
        let mut world = World::new();

        let (_, entity) = ledger
            .spawn(&mut world, "Spark", Vec3::new(0.0, 5.0, 0.0))
            .unwrap();
        assert_eq!(
            world.get::<&Transform>(entity).unwrap().position,
            Vec3::new(0.0, 5.0, 0.0)
        );
    }

    #[test]
    fn forgetting_an_authored_entity_costs_nothing() {
        let mut ledger = SpawnLedger::new(&[template("Bullet", 1)], &World::new());
        let mut world = World::new();
        ledger.spawn(&mut world, "Bullet", Vec3::ZERO).unwrap();

        ledger.forget("Ground");
        ledger.forget("Ground");
        assert_eq!(
            ledger.live_count("Bullet"),
            Some(1),
            "despawning something the ledger never spawned frees no slot"
        );
    }

    /// The bake-resume case: `simulate --bake` writes live instances back as
    /// ordinary entities, so a resumed run opens a file that already holds
    /// `Bullet#7`. Minting a second one would be invariant 4 broken at
    /// runtime.
    #[test]
    fn a_resumed_run_counts_past_the_instances_already_in_the_file() {
        let mut world = World::new();
        for name in [
            "Ground",
            "Bullet#1",
            "Bullet#7",
            "NotATemplate#3",
            "Bullet#x",
        ] {
            let mut builder = EntityBuilder::new();
            builder.add(Name(name.to_string()));
            world.spawn(builder.build());
        }

        let mut ledger = SpawnLedger::new(&[template("Bullet", 4)], &world);
        assert_eq!(
            ledger.live_count("Bullet"),
            Some(2),
            "the two baked instances are live; `Bullet#x` is not an instance name \
             and `NotATemplate#3` is not this template"
        );

        let (name, _) = ledger.spawn(&mut world, "Bullet", Vec3::ZERO).unwrap();
        assert_eq!(name, "Bullet#8", "past the highest index in the file");

        // And the baked instances despawn like any other, freeing their slots.
        ledger.forget("Bullet#1");
        assert_eq!(ledger.live_count("Bullet"), Some(2));
    }

    #[test]
    fn an_unknown_template_is_refused_rather_than_invented() {
        let mut ledger = SpawnLedger::new(&[template("Bullet", 1)], &World::new());
        let mut world = World::new();
        assert_eq!(
            ledger.spawn(&mut world, "Bulet", Vec3::ZERO),
            Err(SpawnRefusal::UnknownTemplate)
        );
        assert_eq!(ledger.template_names(), vec!["Bullet"]);
    }
}
