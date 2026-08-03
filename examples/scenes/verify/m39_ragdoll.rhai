// M39's fixture: two identical walkers, and the only difference between them
// is a `Ragdoll`.
//
// M33's script almost verbatim — each root is carried forward at a fixed
// speed while the clip does the legs — plus the two lines this milestone is
// about: one walker is handed to physics at step 40, and the carry stops for
// anything physics owns. `world.is_ragdoll` exists for exactly that second
// line: a script that kept driving `set_position` would drag the corpse along
// the floor by its pelvis, which is the most likely way to get this wrong and
// looks like the ragdoll never fired.

fn carry(world, name, x, z0, t) {
    world.set_position(name, x, 0.0, z0 - 1.0 * t);
}

fn step(world, step) {
    let t = step / 60.0;

    if step == 40 {
        world.ragdoll("Dropped");
        // And the arcade half, in one call: a kick to the hitbox that was hit,
        // addressed by the part name every report already uses.
        world.ragdoll_impulse("Dropped", "Chest", 0.0, 1.2, -6.0);
    }

    if !world.is_ragdoll("Dropped") {
        carry(world, "Dropped", -1.3, 1.0, t);
    }
    carry(world, "Walking", 1.3, 1.0, t);
}
