// Runs once per fixed simulation step, advanced by `--steps` on screenshot,
// simulate, and diff-render, and by the wall clock in `engine run-scene`.
//
// The `world` API is the whole universe a script can reach: no clock, no file
// system, no randomness. That is what keeps a run reproducible — the same
// scene and the same step count give byte-identical results every time.

fn step(world, step) {
    // Entities are addressed by name, exactly as they are named in the scene.
    let spin = step * world.dt() * 45.0;
    world.set_rotation("SpinningCube", 0.0, spin, 0.0);

    // A debug line, cleared every step and drawn over the frame. It also comes
    // back as "hud" in the `engine simulate` report, so it is readable without
    // rendering anything.
    world.hud("step " + step);
}
