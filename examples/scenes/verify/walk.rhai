// M32's fixture: two identical walkers cross the same slope, and the only
// difference between them is a `FootPlant`.
//
// The script does nothing a locomotion system would do. It carries each root
// along at a fixed speed and drops it onto the terrain — the M30 division of
// labour — and everything else is the engine's: the clip's phase follows the
// ground covered (`stride`), and the planted one's feet follow the ground
// under each foot.

const SPEED = 1.0;

fn carry(world, name, x0, z, t) {
    let x = x0 + 1.0 * t;
    world.set_position(name, x, world.terrain_height("Ground", x, z), z);
}

fn step(world, step) {
    let t = step / 60.0;
    carry(world, "Planted", -0.8, -0.15, t);
    carry(world, "Loose", 0.8, -0.15, t);
}
