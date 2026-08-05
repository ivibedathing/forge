// Side-view walk: carry the walker along +X at exactly one stride per
// second (1.5894 m/s), so the clip's ground-driven phase advances at its
// authored rate. Everything else is the engine's — phase follows ground
// covered, and the feet follow the flat terrain under them.
//
// The ground is flat and matches the clip's own authoring plane, which is
// what makes this scene an oracle: planting here must be a near no-op, so
// any crouch, shuffle or hip bob beyond what the clip authors is the
// solver's fault and nothing else's.
//
// (Rhai: a top-level `const` is not visible inside `fn`, so the speed is
// a literal here.)

fn step(world, step) {
    let t = step / 60.0;
    world.set_position("Walker", -2.5 + 1.5894 * t, 0.0, 0.0);
}
