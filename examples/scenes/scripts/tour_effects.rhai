// The elemental stations: fire that flickers, water that moves, spray that
// pulses. Everything here is a script driving components that already exist —
// no effect system, by design. What the engine gives is a deterministic
// emitter whose rate a script can write, and Transforms it can push around;
// the *look* of fire and water is authored in these numbers.
//
// Where the honest limits are: the pond is opaque geometry with a low
// roughness, not refraction, and the ice is a pale dielectric, not
// transmission. Both stations become real the day materials grow alpha and
// transmission — and this file is where that upgrade will show up first.

fn step(world, step) {
    let t = step.to_float() / 60.0;

    // --- Campfire -----------------------------------------------------------
    // Three incommensurable sines: the sum never repeats inside the tour, so
    // the flame does not visibly loop.
    let flick = sin(t * 9.1) * 0.5 + sin(t * 14.7) * 0.3 + sin(t * 3.3) * 0.2;

    // The emissive core swells with the flame — an unlit sphere is the only
    // way a script can make something look brighter, since materials are not
    // in the curated API.
    let s = 0.46 + flick * 0.09;
    world.set_scale("FireCore", s, s * 0.85, s);

    world.set_particle_rate("Fire", 130.0 + flick * 45.0);
    world.set_particle_rate("Sparks", 22.0 + flick * 12.0);

    // --- Pond ---------------------------------------------------------------
    // Sixteen tiles on one travelling wave. The phase runs along both axes, so
    // the crest crosses the pond diagonally instead of the whole surface
    // breathing in unison.
    for i in 0..4 {
        for j in 0..4 {
            let x = 10.35 + i.to_float() * 3.1;
            let z = 1.35 + j.to_float() * 3.1;
            let phase = t * 2.4 - i.to_float() * 0.85 - j.to_float() * 0.55;
            world.set_position("Water" + i + j, x, 0.14 + sin(phase) * 0.07, z);
        }
    }

    // --- Waterfall ----------------------------------------------------------
    // The plunge pool surges: the fall is a constant curtain, the spray at its
    // foot is not.
    world.set_particle_rate("Spray", 95.0 + sin(t * 5.5) * 25.0);
}
