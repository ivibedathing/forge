// The M17 fixture's driver: one flicker signal reaching everything the fire
// does, including — new in M17 — the light it casts.
//
// The point of this file is that all four consumers move together. A fire whose
// flame flares while its light holds steady reads as a video playing on a lamp;
// the coupling is the effect.

fn step(world, step) {
    let t = step.to_float() / 60.0;

    // Fast shimmer plus a slow breath. Incommensurable frequencies, so the sum
    // never repeats over any run length a baseline would use.
    let flick = sin(t * 9.1) * 0.5 + sin(t * 14.7) * 0.3 + sin(t * 3.3) * 0.2;
    let breath = sin(t * 1.7) * 0.6 + sin(t * 0.9) * 0.4;
    let heat = 1.0 + flick * 0.18 + breath * 0.12;

    // The flame's three layers scale together, so the fire dies down and flares
    // as one body rather than its layers drifting apart.
    world.set_particle_rate("FireBase", 70.0 * heat);
    world.set_particle_rate("Fire", 260.0 * heat);
    world.set_particle_rate("FireTongues", 46.0 * heat);
    world.set_particle_rate("Embers", 20.0 * (1.0 + flick * 0.5));

    // The coal bed swells a little — an emissive surface is still the only way
    // a script can make geometry look hotter.
    // These are `builtin:sphere` diameters, not radii: a builtin is one metre
    // across at scale 1 (M34), so the bed is 0.88 m wide and swells by 0.08.
    let s = 0.88 + flick * 0.08;
    world.set_scale("Coals", s, s * 0.30, s);

    // The light. Intensity follows the same heat; the color rides with it, and
    // because red is already saturated it is green and blue that climb, so a
    // flare reads as *hotter* rather than merely brighter.
    world.set_light_intensity("FireLight", 2.6 * heat);
    world.set_light_color("FireLight", 1.0, 0.46 + flick * 0.09, 0.14 + flick * 0.07);
    world.set_position("FireLight", 0.0, 0.82 + flick * 0.09, 0.0);
}
