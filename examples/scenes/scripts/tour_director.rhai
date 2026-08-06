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
//   leg 5     900-1079   west to the hamlet, which is bare ground by now
//   leg 6    1080-1259   it builds itself, solved live, while the camera
//                        crosses it (M50)
//   leg 7    1260-1439   the way back, and then the whole thing again
//
// The village gets two legs because it needs both: an approach, since the aim
// only swings to a new subject at 62% of a leg and a village arrived at is a
// village already built; and a crossing, whose aim is *constant* — the only
// leg whose two aim keys are the same point — so the hamlet stays centred for
// the whole three seconds it takes to solve.
//
// Past step 900 the camera neither stops nor parks, and the tour repeats on a
// 1440-step lap. That return leg is the whole reason the path is a *cycle* of
// eight legs over nine keys (the ninth is the first again) rather than five
// legs that clamp at the last one — clamping is what made the finale replay
// its own three seconds over and over while everything else in the world kept
// moving.
// Nothing resets on a new lap: the crates stay broken, the fire keeps burning,
// the truck keeps circling. The camera is the only thing that comes round —
// and the village, which is rebuilt from scratch on every lap and comes out
// different each time, because its seed advances with the step rather than
// with the lap position.
//
// `total` stays 900: the tour *proper* is still the five stations in fifteen
// seconds, and the village is on the way home. That is also what keeps every
// frame before step 900 arithmetically identical to the run six baselines were
// blessed from — the counter's denominator is the only thing a new leg could
// have moved, and it does not move.

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
    let cycle = 1440;   // one lap: five stations, two village legs, the way home

    // Nine camera keys for eight legs: each station's framing, the next one's,
    // and finally the first one again so the path closes. The camera is
    // *always* travelling; only the aim settles.
    let eyes = [
        [-27.0,  7.5, -20.5],
        [ 16.5,  4.5, -24.0],
        [ 32.5,  7.0,   8.5],
        [ -1.6,  3.8,  23.5],
        [  3.0, 14.0,  40.0],
        [-20.0, 23.0,  36.0],
        [-30.0, 16.0,  30.0],
        [-64.0, 14.0, -14.0],
        [-27.0,  7.5, -20.5],
    ];
    let aims = [
        [-13.0, 2.5, -10.0],
        [  9.0, 1.4, -13.0],
        [ 15.5, 1.2,   4.0],
        [ -1.0, 1.2,  15.0],
        [  0.0, 3.0,   8.0],
        [  0.0, 3.0,   2.0],
        [-46.0, 3.0,   2.0],
        [-46.0, 3.0,   2.0],
        [-13.0, 2.5, -10.0],
    ];
    let titles = [
        "01 FOREST",
        "02 CAMPFIRE",
        "03 WATER AND ICE",
        "04 BREAKING",
        "05 THE WHOLE WORLD",
        "06 TO THE VILLAGE",
        "07 TILE SYNTHESIS",
        "08 THE WAY BACK",
    ];
    let systems = [
        "MESHES . GLTF . MATERIALS . LIGHTING . ANIMATION",
        "ADDITIVE FIRE . TURBULENCE . FIRELIGHT . EMBERS",
        "GERSTNER WAVES . DEPTH . REFRACTION . FOAM . SPRAY . FROST",
        "RIGID BODIES . COLLISION . FRAGMENTS . BLAST",
        "VEHICLE . WHEELS . HUD . CAMERA . ALL OF IT",
        "TILESETS . SOCKETS . A GRID WITH NOTHING IN IT",
        "336 CELLS . SOLVED IN BLOCKS . WHILE YOU WATCH",
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

    // Stations 06 and 07: the village builds itself, solved live (M50).
    //
    // The clear fires at the top of the approach, while the camera is still
    // 60 m away and aimed at the arena, so what the aim finds when it swings is
    // bare cobble — and the village then grows across it rather than blinking
    // into place. From the fill the solve is strict (M49's do-no-harm rule has
    // no violations to preserve), which is why this builds rather than churns.
    //
    // `p`, not `step`, because the leg is a position on the lap. The *seed* is
    // the other way round on purpose: it advances with the step, so lap two
    // grows a different village on the same ground, which a still layout
    // cannot show and is the whole reason this is a station.
    if p == 900 { world.clear_tiles("Hamlet"); }
    if p >= 1080 && p < 1260 && p % 12 == 0 {
        // A **raster**, not a line. `world.synthesize` takes a disc, so a
        // radius wide enough to reach the far side of the grid in z is also
        // wide enough to re-solve all of it in x; a line sweep at a usable
        // radius only ever meets the middle band of blocks and leaves the
        // north and south thirds standing as cobble. Fifteen positions, z outer
        // and x inner — the solver's own scan order — build the hamlet row by
        // row over the three seconds the camera spends crossing it.
        //
        // The spacing is load-bearing for a second reason: a disc re-solves
        // whole *blocks*, and passing over one repeatedly grows a single roof
        // mass with no street in it. Do-no-harm counts violations rather than
        // measuring them, so once one over-size region exists, adding to it is
        // free. One pass per block is what leaves a village with rooms.
        let pass = (p - 1080) / 12;
        if pass < 15 {
            let x = -58.0 + (pass % 5).to_float() * 6.0;
            let z = -6.0 + (pass / 5).to_float() * 8.0;
            world.synthesize("Hamlet", x, z, 4.0, 900 + step);
        }
    }

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
