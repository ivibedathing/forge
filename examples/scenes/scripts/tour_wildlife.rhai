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
        // The ground is terrain now, so a resting height is a height *above the
        // ground*, not a world coordinate — ask where the ground is. A critter
        // on a fixed y walks through the hills instead of over them.
        let y = world.terrain_height("Ground", x, z) + rest[i] + hop;
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

    // The walker (M27), on a slow circuit through the forest station. Its
    // *legs* are a skeletal clip and this only carries it along the ground —
    // which is the whole division of labour the milestone is about: the pose
    // is a pure function of (files, time) and a script never touches a joint,
    // so what a script does with a character is decide where it is.
    //
    // The stride is tuned to the clip rather than the other way round: `Walk`
    // is one second long and covers about 0.9 m of ground per cycle, so a
    // speed much off that skates the feet. Nothing enforces it — foot IK is
    // out of scope and named so in the design — so the number is a comment
    // with a value attached.
    // The circuit sits *between* the station-01 camera and the trees rather
    // than among them: a two-metre figure thirty metres away and behind a
    // canopy is a pale smudge, and the point of putting it here is that the
    // legs are legibly legs.
    let wa = 0.34 * t + 2.2;
    let wr = 2.6;
    let wx = -18.6 + wr * sin(wa);
    let wz = -10.4 + wr * cos(wa);
    world.set_position("Walker", wx, world.terrain_height("Ground", wx, wz), wz);
    let wahead = wa + 0.10;
    world.look_at(
        "Walker",
        -13.2 + wr * sin(wahead),
        world.terrain_height("Ground", wx, wz),
        -9.6 + wr * cos(wahead),
    );
}
