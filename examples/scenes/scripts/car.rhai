// Arrow-key car with real dynamics. The Car is a dynamic RigidBody: gravity,
// mass (collider density × volume ≈ 1.5 t), and collisions with the barriers
// all belong to physics. This script is the drivetrain and the tires — each
// step it reads the body's post-physics velocity, applies engine force,
// braking, drag, and lateral grip in the car's frame, and writes the velocity
// back; rapier integrates and resolves whatever the world does to the car.
//
// Controls: ArrowUp throttle, ArrowDown brake (then reverse), Left/Right
// steer. Steering is speed-scaled — no turning in place — and flips in
// reverse. All rates are per-second, scaled by dt.

fn step(world, step) {
    let dt = world.dt();

    let accel = 7.0;          // engine, m/s^2
    let brake = 12.0;         // braking, m/s^2
    let reverse_accel = 4.0;  // m/s^2
    let top_speed = 13.0;     // m/s (~47 km/h; the straights are short)
    let top_reverse = 5.0;
    let drag = 0.25;          // aero drag, fraction of speed per second
    let rolling = 1.2;        // rolling resistance, m/s^2, when coasting
    let grip = 8.0;           // lateral-slip bleed rate per second; lower = drift
    let steer_max = 110.0;    // deg/s of yaw at full grip

    // The car's frame. world.forward reads the physics-integrated rotation
    // correctly whatever its Euler representation — never do yaw math on
    // rotation[1], which is only "the yaw" for |yaw| <= 90°.
    let f = world.forward("Car");
    let fx = f[0];
    let fz = f[2];
    let rx = -fz;             // right = forward × up
    let rz = fx;

    // Decompose the body's velocity into the car's frame.
    let v = world.linear_velocity("Car");
    let forward_speed = v[0] * fx + v[2] * fz;
    let lateral_speed = v[0] * rx + v[2] * rz;

    let throttle = world.key("ArrowUp");
    let braking = world.key("ArrowDown");

    if throttle { forward_speed += accel * dt; }
    if braking {
        if forward_speed > 0.2 {
            forward_speed -= brake * dt;          // brake…
        } else {
            forward_speed -= reverse_accel * dt;  // …then back up
        }
    }

    // Aero drag always; rolling resistance only while coasting, and it
    // stops at zero instead of oscillating around it.
    forward_speed -= forward_speed * drag * dt;
    if !throttle && !braking {
        if forward_speed > rolling * dt { forward_speed -= rolling * dt; }
        else if forward_speed < -rolling * dt { forward_speed += rolling * dt; }
        else { forward_speed = 0.0; }
    }
    if forward_speed > top_speed { forward_speed = top_speed; }
    if forward_speed < -top_reverse { forward_speed = -top_reverse; }

    // Tires: bleed sideways velocity. What survives a step is the drift.
    let hold = 1.0 - grip * dt;
    if hold < 0.0 { hold = 0.0; }
    lateral_speed *= hold;

    // Steering: yaw rate grows with speed up to full authority at 6 m/s,
    // and flips when reversing — front wheels, not a turret.
    let steer = 0.0;
    if world.key("ArrowLeft") { steer += 1.0; }
    if world.key("ArrowRight") { steer -= 1.0; }
    let authority = forward_speed / 5.0;
    if authority > 1.0 { authority = 1.0; }
    if authority < -1.0 { authority = -1.0; }
    world.set_angular_velocity("Car", 0.0, steer * steer_max * authority, 0.0);

    // Recompose in world space; vertical velocity stays whatever gravity
    // and the ground decided.
    world.set_linear_velocity(
        "Car",
        fx * forward_speed + rx * lateral_speed,
        v[1],
        fz * forward_speed + rz * lateral_speed,
    );

    // Chase camera: spring toward a point behind the car, then aim over the
    // cab. The lag is what makes speed visible.
    let p = world.position("Car");
    let goal_x = p[0] - fx * 8.0;
    let goal_z = p[2] - fz * 8.0;
    let cam = world.position("ChaseCam");
    let k = 1.0 - exp(-6.0 * dt);
    world.set_position(
        "ChaseCam",
        cam[0] + (goal_x - cam[0]) * k,
        3.2,
        cam[2] + (goal_z - cam[2]) * k,
    );
    world.look_at("ChaseCam", p[0], 1.0, p[2]);
}
