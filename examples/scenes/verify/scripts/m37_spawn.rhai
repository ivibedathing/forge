// M37 verification: a launcher that fires spawned projectiles at a stack.
//
// The whole milestone in one loop — spawn from a template the scene declares,
// address the new entity by the name the call returned, and despawn it when it
// has done its job so the limit means "in flight" rather than "ever fired".

fn step(world, step) {
    // The live set is the window [first, next). Shots are fired at one rate
    // with one velocity, so they leave in the order they arrived — which is
    // what lets this reap from the oldest end and never ask about a name that
    // is already gone.
    let first = world.state("first", 1).to_int();
    let next = world.state("next", 1).to_int();

    while first < next {
        let name = "Shot#" + first;
        // Shot #n was fired at step (n-1)*11, so its age is exact rather than
        // stored per instance — the fixed rate is what makes that true.
        let age = step - (first - 1) * 11;
        let p = world.position(name);
        // Alive while it is young and still in the picture.
        if age < 55 && p[1] > 0.25 && p[0] < 7.5 {
            break;
        }
        world.despawn_entity(name);
        first += 1;
    }

    if step % 11 == 0 {
        let shot = world.spawn_entity("Shot", -5.0, 2.3, 0.0);
        // The empty name is the cap, not a failure — see the design doc §2.
        if shot != "" {
            world.set_linear_velocity(shot, 10.7, 2.75, 0.0);
            next += 1;
        }
    }

    world.set_state("first", first);
    world.set_state("next", next);
    world.hud("shots in flight " + world.spawn_count("Shot") + "/" + world.spawn_limit("Shot"));
    world.hud("fired " + (next - 1));
}
