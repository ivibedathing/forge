// M33's fixture: two identical walkers walk into two identical crates, and
// the only difference between them is a `SkinnedCollider`.
//
// The script does nothing physics would do. It carries each root forward at a
// fixed speed — the M30 division of labour, and M32's fixture's script almost
// verbatim — while the clip does the legs. What the proxied walker's legs then
// do to the crate in front of it is the milestone.

fn carry(world, name, x, z0, t) {
    world.set_position(name, x, 0.0, z0 - 1.0 * t);
}

fn step(world, step) {
    let t = step / 60.0;
    carry(world, "Proxied", -1.3, 1.0, t);
    carry(world, "Loose", 1.3, 1.0, t);
}
