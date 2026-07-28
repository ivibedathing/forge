// M12 fixture script: the HUD-component accessors under pin. Writes the
// step counter into a HudText and stretches a HudRect one pixel per step,
// so the pinned render proves script-driven HUD state is reproducible at a
// fixed --steps.
fn step(world, step) {
    world.set_hud_text("StepCounter", "STEP " + (step + 1));
    let grown = 40.0 + (step + 1).to_float();
    world.set_hud_rect_size("GrowBar", grown, 10.0);
}
