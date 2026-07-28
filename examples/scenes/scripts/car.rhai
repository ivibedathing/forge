// Arrow-key car on real wheels (M12). The Car is a dynamic box chassis; the
// four Wheel components are the physics: each casts a suspension ray, a
// spring/damper carries the load, and a tire model grips, drives, and brakes
// at the contact point. This script is only the driver — pedals and a
// steering wheel. It never touches velocities; rapier owns the dynamics.
//
// Controls: ArrowUp throttle (rear-wheel drive), ArrowDown brake (then
// reverse), Left/Right steer the front wheels. Steering authority shrinks
// with speed, and the wheel turns at a finite rate rather than snapping.
//
// The HUD (speedometer + lap timer) is world.hud lines pushed fresh each
// step; lap memory — where the lap started, the best time — lives in
// world.state, the per-run store that replays deterministically.

// Fixed two-decimal seconds: Rhai prints raw floats with a jittering tail,
// unreadable on a HUD.
fn fmt_secs(t) {
    let cents = (t * 100.0).round().to_int();
    let c = cents % 100;
    let frac = if c < 10 { "0" + c } else { "" + c };
    (cents / 100).to_string() + "." + frac
}

fn step(world, step) {
    let dt = world.dt();

    let drive_force = 3400.0;   // N total at the rear axle
    let reverse_force = 1800.0;
    let brake_force = 25.0;     // brake impulse cap, per wheel
    let idle_brake = 1.5;       // rolling resistance while coasting
    let top_speed = 14.0;       // m/s (~50 km/h)
    let top_reverse = 4.0;
    let steer_slow = 35.0;      // deg of front-wheel lock at standstill
    let steer_fast = 18.0;      // deg remaining at top speed
    let steer_rate = 140.0;     // deg/s the steering wheel can move

    // Signed forward speed in the car's frame (ground plane only).
    let f = world.forward("Car");
    let v = world.linear_velocity("Car");
    let forward_speed = v[0] * f[0] + v[2] * f[2];

    let throttle = world.key("ArrowUp");
    let braking = world.key("ArrowDown");

    // Pedals. ArrowDown brakes while rolling forward, backs up once stopped.
    let engine = 0.0;
    let brake = 0.0;
    if throttle && forward_speed < top_speed {
        // Low-gear torque: extra shove below 8 m/s, so the car launches and
        // powers through full-lock corners instead of stalling against the
        // front tires' slip drag.
        let boost = 2.0 - forward_speed / 8.0;
        if boost > 2.0 { boost = 2.0; }
        if boost < 1.0 { boost = 1.0; }
        engine = drive_force / 2.0 * boost;
    }
    if braking {
        if forward_speed > 0.3 {
            brake = brake_force;
        } else if forward_speed > -top_reverse {
            engine = -reverse_force / 2.0;
        }
    }
    if !throttle && !braking {
        brake = idle_brake;
    }
    world.set_engine_force("WheelRL", engine);
    world.set_engine_force("WheelRR", engine);
    world.set_brake("WheelRL", brake);
    world.set_brake("WheelRR", brake);
    world.set_brake("WheelFL", brake);
    world.set_brake("WheelFR", brake);

    // Steering wheel: aim for full lock scaled down by speed, move toward
    // it at a finite rate, and center itself when the keys are released.
    let steer = 0.0;
    if world.key("ArrowLeft") { steer += 1.0; }
    if world.key("ArrowRight") { steer -= 1.0; }
    let speed_frac = abs(forward_speed) / top_speed;
    if speed_frac > 1.0 { speed_frac = 1.0; }
    let lock = steer_slow + (steer_fast - steer_slow) * speed_frac;
    let target = steer * lock;
    let current = world.steering("WheelFL");
    let delta = target - current;
    let max_move = steer_rate * dt;
    if delta > max_move { delta = max_move; }
    if delta < -max_move { delta = -max_move; }
    let angle = current + delta;
    world.set_steering("WheelFL", angle);
    world.set_steering("WheelFR", angle);

    // Chase camera: spring toward a point behind the car, then aim over the
    // cab. The lag is what makes speed visible. Height springs too — the
    // circuit climbs and drops nearly eight meters, so a camera pinned to a
    // fixed altitude would sink into the hill on the way up Raidillon.
    let p = world.position("Car");
    let goal_x = p[0] - f[0] * 8.0;
    let goal_z = p[2] - f[2] * 8.0;
    let cam = world.position("ChaseCam");
    let k = 1.0 - exp(-6.0 * dt);
    world.set_position(
        "ChaseCam",
        cam[0] + (goal_x - cam[0]) * k,
        cam[1] + (p[1] + 2.3 - cam[1]) * k,
        cam[2] + (goal_z - cam[2]) * k,
    );
    world.look_at("ChaseCam", p[0], p[1] + 0.1, p[2]);

    // Exhaust smoke: park the emitter at the tailpipe — behind the rear
    // bumper, offset to the car's right, near the ground — every step.
    // Particles are world-space once spawned, so the moving car leaves its
    // own trail. Right of forward (yaw only) is (-f[2], 0, f[0]).
    world.set_position(
        "Exhaust",
        p[0] - f[0] * 2.05 - f[2] * 0.55,
        p[1] - 0.5,
        p[2] - f[2] * 2.05 + f[0] * 0.55,
    );

    // Tire smoke (M13 + the M13 rate accessor): the rear tires smoke when
    // they are *sliding*, not when they are rolling. Sideslip is the lateral
    // component of the chassis velocity — the car moving in a direction it
    // is not pointing — and hard braking at speed adds lockup on top. Both
    // feed one emission rate, which drops to 0 the moment grip returns; that
    // gating is the whole point of world.set_particle_rate, since an emitter
    // that cannot be switched off has to be a permanent feature of the scene.
    let right_x = -f[2];
    let right_z = f[0];
    let lateral = abs(v[0] * right_x + v[2] * right_z);
    // A deadband below 1 m/s: suspension jitter is not a skid, and a parked
    // car must not smoulder.
    let skid = (lateral - 1.0) / 3.0;
    if skid < 0.0 { skid = 0.0; }
    if braking && forward_speed > 6.0 { skid += 0.5; }
    if skid > 1.0 { skid = 1.0; }
    let tire_smoke = 120.0 * skid;
    world.set_particle_rate("SkidLeft", tire_smoke);
    world.set_particle_rate("SkidRight", tire_smoke);
    // Park each emitter at its rear contact patch (chassis-local ∓0.92 across
    // the axle, 1.25 back), just off the tarmac. The height tracks the car,
    // like the exhaust: the circuit climbs, and a contact patch pinned to a
    // fixed altitude would smoke from inside the hill.
    world.set_position(
        "SkidLeft",
        p[0] - f[0] * 1.25 - right_x * 0.92,
        p[1] - 0.78,
        p[2] - f[2] * 1.25 - right_z * 0.92,
    );
    world.set_position(
        "SkidRight",
        p[0] - f[0] * 1.25 + right_x * 0.92,
        p[1] - 0.78,
        p[2] - f[2] * 1.25 + right_z * 0.92,
    );

    // Lap timing. The start line lies across the pit straight, which climbs
    // north-north-west toward La Source: racing direction is -Z there, so a
    // lap completes when z crosses the line going negative, within a dozen
    // meters of the line's x. (Both numbers are printed by
    // make_car_track.py — they move whenever the layout does.) The car
    // *starts on* the line, and parking jitter could re-cross it, so a
    // crossing only counts after 300 steps (5 s) of lap — a real lap here
    // takes well over half a minute.
    let line_z = -37.74;
    let line_x = -65.80;

    let prev_z = world.state("lap_prev_z", line_z);
    let lap = world.state("lap", 1);
    let lap_start = world.state("lap_start", 0);
    let last_lap = world.state("last_lap", 0.0);
    let best_lap = world.state("best_lap", 0.0);

    if prev_z > line_z && p[2] <= line_z && abs(p[0] - line_x) < 12.0
        && step - lap_start.to_int() > 300 {
        let lap_time = (step - lap_start.to_int()).to_float() * dt;
        last_lap = lap_time;
        if best_lap == 0.0 || lap_time < best_lap { best_lap = lap_time; }
        lap += 1.0;
        lap_start = step.to_float();
        world.set_state("last_lap", last_lap);
        world.set_state("best_lap", best_lap);
        world.set_state("lap", lap);
        world.set_state("lap_start", lap_start);
    }
    world.set_state("lap_prev_z", p[2]);

    // The chassis's planar speed after the last physics step, in km/h.
    let speed = sqrt(v[0] * v[0] + v[2] * v[2]) * 3.6;

    // Speed bar: the HudRect gauge (M12) — full width at top speed. The
    // component write bakes and replays like any other scene state.
    let frac = speed / (top_speed * 3.6);
    if frac > 1.0 { frac = 1.0; }
    world.set_hud_rect_size("SpeedBar", 260.0 * frac, 12.0);

    let lap_time = (step - lap_start.to_int()).to_float() * dt;
    world.hud("SPEED " + speed.round().to_int() + " KM/H");
    world.hud("LAP " + lap.to_int() + "   TIME " + fmt_secs(lap_time));
    if last_lap > 0.0 {
        world.hud("LAST " + fmt_secs(last_lap) + "   BEST " + fmt_secs(best_lap));
    }
}
