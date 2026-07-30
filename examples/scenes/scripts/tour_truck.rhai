// The truck patrols a 27 m ring around the whole diorama, so the tour always
// has something driving through it. Same dynamics as the playable car demo —
// a dynamic box chassis on four raycast-suspension Wheels — but with an
// autopilot in place of the arrow keys.
//
// The steering law needs no waypoint list: on a circle the goal direction is
// the tangent, pulled toward the ring by however far off-radius the truck has
// drifted. Heading error is the signed angle from the chassis's forward to
// that goal — read from world.forward, never from rotation[1], which stops
// being the yaw the moment physics integrates past +/-90 degrees.

fn step(world, step) {
    let dt = world.dt();

    let ring = 27.0;
    let cruise = 10.5;        // m/s (~38 km/h)
    let drive_force = 1700.0; // N per rear wheel at full throttle
    let brake_force = 20.0;
    let steer_lock = 26.0;    // deg of front-wheel lock
    let steer_rate = 150.0;   // deg/s the steering wheel can move

    let p = world.position("Truck");
    let f = world.forward("Truck");
    let v = world.linear_velocity("Truck");
    let forward_speed = v[0] * f[0] + v[2] * f[2];

    // Radial frame of the patrol circle.
    let d = sqrt(p[0] * p[0] + p[2] * p[2]);
    if d < 0.001 { d = 0.001; }
    let rx = p[0] / d;
    let rz = p[2] / d;

    // Tangent, corrected inward or outward by the radius error. Six metres of
    // error is full correction; beyond that the truck is simply aimed home.
    let err = (d - ring) / 6.0;
    if err > 1.0 { err = 1.0; }
    if err < -1.0 { err = -1.0; }
    let gx = -rz - rx * err;
    let gz = rx - rz * err;
    let gl = sqrt(gx * gx + gz * gz);
    gx /= gl;
    gz /= gl;

    // Signed heading error. The y component of forward x goal is positive when
    // the goal lies to the left, which is the sign convention `steering` uses.
    let left = f[2] * gx - f[0] * gz;
    let ahead = f[0] * gx + f[2] * gz;
    let target = left * 60.0;
    if ahead < 0.0 {
        // Goal behind: commit to full lock rather than let the linearisation
        // pick a slack answer.
        if left >= 0.0 { target = steer_lock; } else { target = -steer_lock; }
    }
    if target > steer_lock { target = steer_lock; }
    if target < -steer_lock { target = -steer_lock; }

    let current = world.steering("WheelFL");
    let delta = target - current;
    let max_move = steer_rate * dt;
    if delta > max_move { delta = max_move; }
    if delta < -max_move { delta = -max_move; }
    let angle = current + delta;
    world.set_steering("WheelFL", angle);
    world.set_steering("WheelFR", angle);

    // Cruise control: proportional on the speed error, brakes on overspeed.
    let engine = 0.0;
    let brake = 0.0;
    let speed_error = cruise - forward_speed;
    if speed_error > 0.0 {
        engine = drive_force * speed_error;
        if engine > drive_force { engine = drive_force; }
    } else {
        brake = -speed_error * 6.0;
        if brake > brake_force { brake = brake_force; }
    }
    world.set_engine_force("WheelRL", engine);
    world.set_engine_force("WheelRR", engine);
    world.set_brake("WheelRL", brake);
    world.set_brake("WheelRR", brake);
    world.set_brake("WheelFL", brake);
    world.set_brake("WheelFR", brake);

    // Exhaust and tire smoke ride along, exactly as in the car demo: park each
    // emitter at its spot on the chassis every step, and gate the skids on
    // real sideslip so a truck holding its line does not smoulder.
    let right_x = -f[2];
    let right_z = f[0];
    // Follow the ground's *height*, not a fixed altitude: on terrain a pinned
    // emitter smokes from inside a hill on the climbs and from mid-air in the
    // hollows — the lesson the car track's skid emitters already paid for.
    let exhaust_x = p[0] - f[0] * 2.05 - right_x * 0.55;
    let exhaust_z = p[2] - f[2] * 2.05 - right_z * 0.55;
    world.set_position(
        "Exhaust",
        exhaust_x,
        world.terrain_height("Ground", exhaust_x, exhaust_z) + 0.4,
        exhaust_z,
    );

    let lateral = abs(v[0] * right_x + v[2] * right_z);
    let skid = (lateral - 1.0) / 3.0;
    if skid < 0.0 { skid = 0.0; }
    if skid > 1.0 { skid = 1.0; }
    let tire_smoke = 120.0 * skid;
    world.set_particle_rate("SkidLeft", tire_smoke);
    world.set_particle_rate("SkidRight", tire_smoke);
    let left_x = p[0] - f[0] * 1.25 - right_x * 0.92;
    let left_z = p[2] - f[2] * 1.25 - right_z * 0.92;
    world.set_position(
        "SkidLeft",
        left_x,
        world.terrain_height("Ground", left_x, left_z) + 0.12,
        left_z,
    );
    let sr_x = p[0] - f[0] * 1.25 + right_x * 0.92;
    let sr_z = p[2] - f[2] * 1.25 + right_z * 0.92;
    world.set_position(
        "SkidRight",
        sr_x,
        world.terrain_height("Ground", sr_x, sr_z) + 0.12,
        sr_z,
    );

    let speed = sqrt(v[0] * v[0] + v[2] * v[2]) * 3.6;
    world.set_hud_text("TruckLabel", "TRUCK " + speed.round().to_int() + " KMH");
}
