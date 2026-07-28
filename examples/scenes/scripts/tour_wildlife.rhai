// Small animals, running around. Four critters on lazy circles through the
// forest, each with its own radius, rate, and phase so the group never reads
// as one rig.
//
// They are *kinematic* rigid bodies: the script writes the Transform and
// physics follows, which is the supported ownership direction (a clip or a
// script driving a dynamic body is a validation error). Their colliders sit
// on the "critter" layer and only collide with "ground", so a critter cannot
// nudge a crate at the breaking station 25 metres away — the collision-layer
// filter is what lets script-driven props share a world with simulated ones.
//
// Randomness is not available to a script, deliberately: sums of sines are
// the deterministic stand-in, and they replay bit-identically.

fn step(world, step) {
    let t = step.to_float() / 60.0;

    let names = ["Critter1", "Critter2", "Critter3", "Critter4"];
    let cx    = [-12.5, -15.5, -11.0, -14.0];
    let cz    = [-10.0,  -8.0, -13.0,  -6.5];
    let rad   = [  2.6,   2.0,   3.1,   1.7];
    let rate  = [  1.35,  -1.9,  1.05,  -2.3];
    let phase = [  0.0,   2.1,   4.2,   5.6];
    let rest  = [  0.32,  0.29,  0.27,  0.31];

    for i in 0..4 {
        let a = rate[i] * t + phase[i];
        let x = cx[i] + rad[i] * sin(a);
        let z = cz[i] + rad[i] * cos(a);

        // Four hops per circuit-radian: the bounce is what reads as "running"
        // on a shape this small at this distance.
        let hop = abs(sin(a * 4.0)) * 0.22;
        let y = rest[i] + hop;
        world.set_position(names[i], x, y, z);

        // Face where the next moment puts them. look_at keeps the horizon
        // level; composing a yaw by hand through the XYZ Euler order would
        // roll the critter onto its side.
        let ahead = a + 0.12;
        world.look_at(
            names[i],
            cx[i] + rad[i] * sin(ahead),
            y,
            cz[i] + rad[i] * cos(ahead),
        );
    }
}
