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

    // A second, slower signal: a fire does not only shimmer, it *breathes* —
    // flaring for a second or two and then settling. Driving everything from
    // the fast flicker alone gives a strobe; the product of a fast and a slow
    // term is what reads as combustion.
    let breath = sin(t * 1.7) * 0.6 + sin(t * 0.9) * 0.4;
    let heat = 1.0 + flick * 0.18 + breath * 0.12;

    // The coal bed swells with the flame — the emissive sphere is still the
    // only way a script can make a *surface* look hotter, since materials are
    // not in the curated API. What is new (M17) is that the fire now also
    // lights everything around it, below.
    let s = 0.74 + flick * 0.06;
    world.set_scale("FireCore", s, s * 0.30, s);

    // One signal, four consumers: the flame's three layers and the embers.
    // Each is scaled rather than offset, so the whole fire dies down and flares
    // together instead of the layers drifting out of proportion.
    world.set_particle_rate("FireBase", 85.0 * heat);
    world.set_particle_rate("Fire", 300.0 * heat);
    world.set_particle_rate("FireTongues", 54.0 * heat);
    world.set_particle_rate("Sparks", 24.0 * (1.0 + flick * 0.5));

    // The light the fire casts (M17). Intensity tracks the same heat, and the
    // color rides with it: a hotter flame is whiter, so the green and blue
    // channels climb faster than the red one, which is already at 1. That is
    // what makes the flicker read as temperature rather than as a dimmer
    // switch on an orange bulb.
    world.set_light_intensity("FireLight", 5.5 * heat);
    world.set_light_color("FireLight", 1.0, 0.46 + flick * 0.09, 0.14 + flick * 0.07);

    // The light's *center* rises and falls with the flame body, so the pool of
    // firelight on the ground breathes with it instead of being pinned to one
    // radius. A few centimetres is plenty at this distance.
    world.set_position("FireLight", 9.0, 1.05 + flick * 0.12, -13.0);

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
