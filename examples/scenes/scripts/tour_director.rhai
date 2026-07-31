// The tour's director: it flies the camera, narrates in the HUD, and fires
// the timed events the breaking station is built around.
//
// 900 steps at 60 Hz is exactly 15 seconds — five 180-step stations. Every
// number below is in *steps*, not seconds, because the fixed clock is the
// only clock a script can see: the same file at the same --steps renders the
// same frame on any machine, which is what lets the tour sit under a
// diff-render baseline.
//
//   station 0   0-179    forest      meshes, materials, lighting, animation
//   station 1 180-359    campfire    particles, emissive, point light
//   station 2 360-539    water/ice   PBR response, scripted waves, spray
//   station 3 540-719    breaking    rigid bodies, fragments, explosion
//   station 4 720-899    the world   vehicle, wheels, HUD, camera
//   leg 5     900-1079   the way back, and then the whole thing again
//
// Past step 900 the camera neither stops nor parks. Leg 5 flies it from the
// wide finale back down to the forest, and the tour repeats on a 1080-step
// lap. That return leg is the whole reason the path is a *cycle* of six legs
// over seven keys (the seventh is the first again) rather than five legs that
// clamp at the last one — clamping is what made the finale replay its own
// three seconds over and over while everything else in the world kept moving.
// Nothing resets on a new lap: the crates stay broken, the fire keeps burning,
// the truck keeps circling. The camera is the only thing that comes round.

fn ease(t) {
    if t < 0.0 { t = 0.0; }
    if t > 1.0 { t = 1.0; }
    t * t * (3.0 - 2.0 * t)
}

fn mix3(a, b, t) {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn step(world, step) {
    let seg = 180;
    let total = 900;    // the tour proper: five stations
    let cycle = 1080;   // one lap of the loop: those five legs plus the way home

    // Seven camera keys for six legs: each station's framing, the next one's,
    // and finally the first one again so the path closes. The camera is
    // *always* travelling; only the aim settles.
    let eyes = [
        [-27.0,  7.5, -20.5],
        [ 16.5,  4.5, -24.0],
        [ 32.5,  7.0,   8.5],
        [ -1.6,  3.8,  23.5],
        [  3.0, 14.0,  40.0],
        [-20.0, 23.0,  36.0],
        [-27.0,  7.5, -20.5],
    ];
    let aims = [
        [-13.0, 2.5, -10.0],
        [  9.0, 1.4, -13.0],
        [ 15.5, 1.2,   4.0],
        [ -1.0, 1.2,  15.0],
        [  0.0, 3.0,   8.0],
        [  0.0, 3.0,   2.0],
        [-13.0, 2.5, -10.0],
    ];
    let titles = [
        "01 FOREST",
        "02 CAMPFIRE",
        "03 WATER AND ICE",
        "04 BREAKING",
        "05 THE WHOLE WORLD",
        "06 THE WAY BACK",
    ];
    let systems = [
        "MESHES . GLTF . MATERIALS . LIGHTING . ANIMATION",
        "ADDITIVE FIRE . TURBULENCE . FIRELIGHT . EMBERS",
        "GERSTNER WAVES . DEPTH . FOAM . SPRAY . FROST",
        "RIGID BODIES . COLLISION . FRAGMENTS . BLAST",
        "VEHICLE . WHEELS . HUD . CAMERA . ALL OF IT",
        "NOTHING RESET . THE WORLD KEPT RUNNING",
    ];

    // The lap position, not the step: for the first fifteen seconds the two
    // are the same integer, which is what keeps this leg of the arithmetic
    // byte-identical to the run every committed baseline was blessed from.
    let p = step % cycle;
    let i = p / seg;
    let local = (p % seg).to_float() / seg.to_float();

    // The dolly runs the whole segment; the aim holds the current station for
    // the first two thirds and only then swings to the next. Moving the eye
    // while the aim is pinned is what makes a station feel looked-at rather
    // than driven past.
    let eye = mix3(eyes[i], eyes[i + 1], ease((local - 0.30) / 0.70));
    let aim = mix3(aims[i], aims[i + 1], ease((local - 0.62) / 0.38));
    world.set_position("TourCam", eye[0], eye[1], eye[2]);
    world.look_at("TourCam", aim[0], aim[1], aim[2]);

    // Station 04's three ways to break something, in the order the design doc
    // lists them: a collision, a script call, and an explosion.
    //
    // The boulder has to be *held* until its cue. On the old flat ground a
    // sphere parked on the floor stayed parked; on terrain it is a sphere on a
    // hillside, and it rolled thirteen metres downhill during the nine seconds
    // before anything asked it to move — arriving at the crates early, from the
    // wrong side and at the wrong speed. Zeroing its velocity every step is the
    // whole fix: gravity still settles it against the slope, and its cue is
    // simply the step this stops.
    if step < 545 {
        world.set_linear_velocity("Boulder", 0.0, 0.0, 0.0);
        world.set_angular_velocity("Boulder", 0.0, 0.0, 0.0);
    }
    if step == 545 { world.set_linear_velocity("Boulder", 13.0, 0.0, 0.0); }
    if step == 600 { world.break_entity("IcePillar"); }
    if step == 636 { world.explode(-1.0, 0.9, 15.0, 6.5, 210.0); }

    // Blast dust: one second of it, then silence. An emitter that cannot be
    // switched off would have to smoulder through the other four stations.
    let dust = 0.0;
    if step >= 636 && step < 696 { dust = 400.0; }
    world.set_particle_rate("Dust", dust);

    // The fireball is a fifth of a second of a full-sphere emitter. An
    // emissive mesh would have been the obvious "make it bright" reach, but
    // an opaque sphere is exactly what a blast is not: this needs to be
    // additive-looking and gone, which is what alpha-blended particles are.
    let blast = 0.0;
    if step >= 636 && step < 648 { blast = 900.0; }
    world.set_particle_rate("Blast", blast);

    world.set_hud_text("StationLabel", titles[i]);
    world.set_hud_text("SystemsLabel", systems[i]);

    // The bar fills over the fifteen seconds, then over each lap after them.
    // What is chosen here is the *numerator and denominator*, not a finished
    // fraction, so the arithmetic below stays the single expression six
    // blessed baselines were drawn with — 320 * x / n and 320 * (x / n) are
    // not the same float.
    let filled = step + 1;
    let span = total;
    if step >= total {
        filled = (step - total) % cycle + 1;
        span = cycle;
    }
    world.set_hud_rect_size("TimeBar", 320.0 * filled.to_float() / span.to_float(), 8.0);

    // One line HUD line, so the tour stays observable without pixels:
    // `simulate` and `screenshot` report it, and `--trace` logs every change.
    // The narration itself is HudText components in the lower third — the
    // line panel owns the top-left corner and would draw over them.
    // The counter is built separately for the same reason the bar's fraction
    // is: `fn` bodies get a shallow expression-depth budget in a debug build,
    // and one more level of parentheses here does not parse.
    let counter = (step + 1) + "/" + total;
    if step >= total {
        counter = "LAP " + (2 + (step - total) / cycle);
    }
    world.hud("TOUR " + counter + "  " + titles[i]);
}
