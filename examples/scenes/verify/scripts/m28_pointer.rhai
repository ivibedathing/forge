// M28 fixture: everything the mouse reaches, in one step function.
//
// * the marker sits where the cursor's ray meets the ground, so the render
//   shows the *inverse projection* working rather than a number;
// * the crosshair is two HudRects moved to the cursor's pixel, which is what
//   `set_hud_offset` exists for;
// * the button is hit-tested in HUD pixels — cursor fraction × viewport — and
//   held clicks drop a sphere on the marker, which is a menu press in
//   miniature;
// * the readout prints the cursor and the world point it resolves to, so a
//   failure is legible in the `simulate` report without reading pixels.

fn step(world, step) {
    let w = world.viewport_width().to_float();
    let h = world.viewport_height().to_float();
    let px = world.cursor_x() * w;
    let py = world.cursor_y() * h;

    // The crosshair, centred on the pointer. Offsets are pixels inward from
    // the top-left anchor, so the cursor's pixel *is* the offset.
    world.set_hud_offset("Crosshair", px - 7.0, py - 1.0);
    world.set_hud_offset("CrosshairStem", px - 1.0, py - 7.0);

    // Where the pointer meets the ground plane.
    let g = world.cursor_ground(0.0);
    world.set_position("Marker", g[0], 0.05, g[2]);

    // The button: a 132x26 plate 16 px in from the *bottom-right* corner.
    // Hit testing is plain arithmetic in the same pixel space the HUD is
    // laid out in — no engine call, because a menu is game logic — and the
    // anchor is undone here rather than in the engine, which is the whole
    // reason `viewport_width` exists.
    let left = w - 16.0 - 132.0;
    let top = h - 16.0 - 26.0;
    let over = true;
    if px < left { over = false; }
    if px > left + 132.0 { over = false; }
    if py < top { over = false; }
    if py > top + 26.0 { over = false; }

    let firing = false;
    if over { firing = world.mouse("MouseLeft"); }

    let label = "  FIRE  ";
    if over { label = "[ FIRE ]"; }
    world.set_hud_text("ButtonText", label);

    // A held click parks the sphere on the marker; releasing drops it back
    // out of frame. One line of `world.state` is all an edge needs — there is
    // deliberately no click-edge query in the API.
    if firing { world.set_position("Held", g[0], 0.45, g[2]); }
    if !firing { world.set_position("Held", 0.0, -5.0, 0.0); }

    // Printed in whole pixels and tenths of a metre: a float's full decimal
    // expansion is unreadable on an 8-pixel font, and the point of the
    // readout is that a wrong aim is legible at a glance.
    let line = "CURSOR " + (px.round().to_int()).to_string();
    line += "," + (py.round().to_int()).to_string();
    line += " PX   GROUND " + ((g[0] * 10.0).round() / 10.0).to_string();
    line += ", " + ((g[2] * 10.0).round() / 10.0).to_string();
    world.set_hud_text("Readout", line);
    // Also as a debug line, so the aim is legible in a `simulate` report with
    // no image read at all — short, because the line panel draws over the
    // top of the frame and this fixture is a picture first.
    let brief = "G " + ((g[0] * 10.0).round() / 10.0).to_string();
    brief += " " + ((g[2] * 10.0).round() / 10.0).to_string();
    if firing { brief += " FIRE"; }
    world.hud(brief);
}
