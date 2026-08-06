// M50/M51's fixture: a village that builds itself while the scene runs.
//
// Step 0 clears the grid to the known-good fill — cobble below, air above,
// with the two locked **plot** floors kept, because locks are the one thing a
// clear does not throw away (M51: a plot is how a layout says "a house goes
// here"; the sockets then force a whole house to grow around it). Four calls
// then re-solve the grid one block at a time, in the solver's own scan order.
//
// One call per block, not a raster of overlapping discs: a disc re-solves
// whole blocks, so two discs meeting one block re-roll it twice and the houses
// flicker through arrangements. The committed layout is solved at block 6,5
// for the same lattice — the disc centres below are the four block centres,
// at radius 1 so no disc can reach a neighbouring block's interior.
//
// A script's `step` is 0-based — it is the index the step *begins* at — so the
// clear is step 0. Ordering it after the first solve wipes it, which the
// `synthesized` count catches and a picture does not.

fn step(world, step) {
    if step == 0 {
        world.clear_tiles("Village");
        return;
    }

    // World-metre centres of the four blocks' plots and streets. The grid is
    // 10x8 cells of 2 m centred on the origin.
    if step == 8  { world.synthesize("Village", -5.0, -3.0, 1.0, 713); } // plot A
    if step == 24 { world.synthesize("Village",  5.0, -3.0, 1.0, 726); } // street
    if step == 40 { world.synthesize("Village", -5.0,  5.0, 1.0, 739); } // street
    if step == 56 { world.synthesize("Village",  5.0,  5.0, 1.0, 752); } // plot B
}
