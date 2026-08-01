// M36's fixture script: the two additions that have pixels behind them.
//
// Both are step-gated rather than authored, which is the whole point — a
// render at `--steps 0` is the pre-M36 picture and a render at `--steps 90` is
// what a script did to it. Neither is expressible in the scene file, which is
// what makes this a fixture rather than an environment block.
//
// * **The `environment` block is writable** (M36). `shadows` starts off in the
//   file and comes on at step 30, so the baseline pins a shadow the file does
//   not author. `samples` is deliberately *not* touched here: it rebuilds
//   every pipeline in the viewer, and pinning a render against a mid-run
//   pipeline swap is a worse test than the CLI one that reads the value back.
// * **`set_animation_clip` is a hard cut** (M36). `Cut` swaps from `Idle` to
//   `Run` at step 45; `Held` never does. **The two soldiers are the
//   assertion** — M30's fixture logic for the fourth time — since they share a
//   file, a mesh and a material, so anything that made both wrong would leave
//   them identical. Only a real clip switch makes one stride and the other
//   breathe.
//
// The cut also resets `phase` to 0, which is why `Cut` is mid-stride at step
// 90 rather than wherever a carried-over phase would have put it.

fn step(world, step) {
    // Off in the file, on from step 30. Written every step once it is on,
    // because the setter is idempotent and a script that only writes on the
    // edge has to remember whether it already did.
    world.set_shadows(step >= 30);

    let clip = "../../meshes/rigged_soldier.gltf#Idle";
    if step >= 45 { clip = "../../meshes/rigged_soldier.gltf#Run"; }
    world.set_animation_clip("Cut", clip);
}
