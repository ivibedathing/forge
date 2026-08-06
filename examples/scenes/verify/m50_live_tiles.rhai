// M50's fixture: a village that builds itself while the scene runs.
//
// Step 1 clears the grid to the known-good fill — cobble on the ground layer,
// air above it — and every eighth step after that solves the blocks meeting a
// disc that sweeps west to east across the 20 m footprint. What the baseline
// frame shows is therefore the *product* of the runtime path: a frame rendered
// at step 0, or with the feature deleted, is bare cobble.
//
// The clear is what makes this a build rather than a churn. M49's rule is that
// a block may not increase the violations blamed on it, which means a village
// re-solved in place stays the village it already was; from the fill there are
// no violations to increase, so the solve is strict and the buildings come out
// of nothing.

// A script's `step` is 0-based — it is the index the step *begins* at — so the
// clear is step 0 and the sweep starts at 8. Ordering these the other way round
// wipes the first pass, which the `synthesized` count catches and a picture
// does not.
fn step(world, step) {
    if step == 0 {
        world.clear_tiles("Village");
        return;
    }

    // One pass of the disc across the grid, one block-ish at a time. The grid
    // is 10x8 cells of 2 m centred on the origin, so the sweep runs from
    // x = -11 to x = +11 and both ends overhang deliberately — an overhanging
    // disc clamps, which is the behaviour a camera flying past an edge
    // depends on.
    if step % 8 != 0 { return; }

    let pass = step / 8;
    if pass > 8 { return; }

    let x = -11.0 + (pass - 1).to_float() * 2.75;
    let radius = 2.5;
    // The seed advances with the sweep so each disc is its own roll rather
    // than the same one re-applied; two runs of this file still agree, because
    // the sequence is a function of the step and nothing else.
    world.synthesize("Village", x, 0.0, radius, 700 + pass * 13);
}
