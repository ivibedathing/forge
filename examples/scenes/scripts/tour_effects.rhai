// The elemental stations: fire that flickers, water that moves, spray that
// pulses. Everything here is a script driving components that already exist —
// no effect system, by design. What the engine gives is a deterministic
// emitter whose rate a script can write, and Transforms it can push around;
// the *look* of fire and water is authored in these numbers.
//
// Where the honest limits are: the fire is particles and a point light, not
// combustion, and the water reflects a gradient sky rather than the trees
// standing next to it. What is no longer faked here: the ice is real
// transmission (M16), the flame is additive and turbulent (M17), and the pond
// is a real water surface with its own waves (M18) — which is why this file no
// longer moves it.

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
    // A `builtin:sphere` diameter, not a radius: a builtin is one metre across
    // at scale 1 (M34), so the bed is 1.48 m wide and swells by 0.12.
    let s = 1.48 + flick * 0.12;
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
    let pit = world.terrain_height("Ground", 9.0, -13.0);
    world.set_position("FireLight", 9.0, pit + 1.05 + flick * 0.12, -13.0);

    // --- Embers (M37) -------------------------------------------------------
    // Real objects, not sprites. Each one is an entity spawned from the scene's
    // `Ember` template, thrown out of the fire on its own arc, bouncing off the
    // terrain, and reaped once it has burned out. Until M37 a run could only
    // ever shrink, so this had to be either a particle — which has no collider
    // and cannot land on anything — or a prop parked out of frame from step 0.
    let first = world.state("ember_first", 1).to_int();
    let next = world.state("ember_next", 1).to_int();

    // Reaped by age from the oldest end. Ember #n was spawned at step
    // (n-1)*24, which is exact because the rate is fixed *and* the limit is
    // never reached: 150 steps of life at one every 24 is at most seven alive
    // against a limit of eight, so no spawn is ever refused and no index is
    // ever skipped. Raise the rate without raising the limit and this stops
    // being true.
    while first < next {
        if step - (first - 1) * 24 < 150 {
            break;
        }
        world.despawn_entity("Ember#" + first);
        first += 1;
    }
    if step % 24 == 0 {
        let ember = world.spawn_entity("Ember", 9.0, pit + 1.5, -13.0);
        if ember != "" {
            // The sandbox has no randomness by design, so the spread comes off
            // the clock — the flicker's trick, on three slower periods.
            world.set_linear_velocity(
                ember,
                sin(t * 2.3) * 1.7,
                3.5 + sin(t * 3.7) * 0.6,
                cos(t * 1.9) * 1.7,
            );
            next += 1;
        }
    }
    world.set_state("ember_first", first);
    world.set_state("ember_next", next);

    // --- Pond ---------------------------------------------------------------
    // Nothing here any more, and that is the point. Until M18 this was sixteen
    // cube tiles that a loop pushed up and down on one travelling sine: every
    // tile stayed flat, so the surface had no normal variation at all, and the
    // seams between them were visible in every screenshot. One `Water`
    // component replaced the geometry, the loop, and both artifacts.

    // --- Waterfall ----------------------------------------------------------
    // The plunge pool surges: the fall is a constant curtain, the spray at its
    // foot is not.
    world.set_particle_rate("Spray", 95.0 + sin(t * 5.5) * 25.0);
}
