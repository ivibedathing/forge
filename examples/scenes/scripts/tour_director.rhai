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
//   station 1 180-359    campfire    particles, emissive
//   station 2 360-539    water/ice   PBR response, scripted waves, spray
//   station 3 540-719    breaking    rigid bodies, fragments, explosion
//   station 4 720-899    the world   vehicle, wheels, HUD, camera

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
    let total = 900;

    // Six camera keys for five segments: each station's framing, then the
    // next one's. The camera is *always* travelling; only the aim settles.
    let eyes = [
        [-27.0,  7.5, -20.5],
        [ 16.5,  4.5, -24.0],
        [ 32.5,  7.0,   8.5],
        [ -1.6,  3.8,  23.5],
        [  3.0, 14.0,  40.0],
        [-20.0, 23.0,  36.0],
    ];
    let aims = [
        [-13.0, 2.5, -10.0],
        [  9.0, 1.4, -13.0],
        [ 15.5, 1.2,   4.0],
        [ -1.0, 1.2,  15.0],
        [  0.0, 3.0,   8.0],
        [  0.0, 3.0,   2.0],
    ];
    let titles = [
        "01 FOREST",
        "02 CAMPFIRE",
        "03 WATER AND ICE",
        "04 BREAKING",
        "05 THE WHOLE WORLD",
    ];
    let systems = [
        "MESHES . GLTF . MATERIALS . LIGHTING . ANIMATION",
        "PARTICLES . EMISSIVE . SCRIPTED FLICKER",
        "PBR RESPONSE . SCRIPTED WAVES . SPRAY . FROST",
        "RIGID BODIES . COLLISION . FRAGMENTS . BLAST",
        "VEHICLE . WHEELS . HUD . CAMERA . ALL OF IT",
    ];

    let i = step / seg;
    if i > 4 { i = 4; }
    let local = (step % seg).to_float() / seg.to_float();

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
    world.set_hud_rect_size("TimeBar", 320.0 * (step + 1).to_float() / total.to_float(), 8.0);

    // One line HUD line, so the tour stays observable without pixels:
    // `simulate` and `screenshot` report it, and `--trace` logs every change.
    // The narration itself is HudText components in the lower third — the
    // line panel owns the top-left corner and would draw over them.
    world.hud("TOUR " + (step + 1) + "/" + total + "  " + titles[i]);
}
