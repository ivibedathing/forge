// M21: a lamp that knows what time it is.
//
// The only new thing here is `world.sun_altitude()`; turning that into light is
// the M17 API unchanged, which is the point — day/night did not need a way for
// scripts to touch lights, because one already existed.
//
// It fades on the sun's *altitude* rather than on the hour deliberately. An
// hour threshold has to be retuned whenever the scene's `sun_elevation` or
// `sun_azimuth` changes; "when the sun is near the horizon" keeps meaning the
// same thing.

fn step(world, step) {
    let altitude = world.sun_altitude();

    // Dark while the sun is more than 3° up, full once it is 4° under. The
    // band is wide enough that the lamp comes up over several seconds of a
    // 24-second day rather than snapping on in one step.
    let t = (3.0 - altitude) / 7.0;
    if t < 0.0 { t = 0.0; }
    if t > 1.0 { t = 1.0; }

    world.set_light_intensity("LampHead", 7.0 * t);
}
