// Arrow-key car controller. ArrowUp/ArrowDown throttle, ArrowLeft/ArrowRight
// steer; the chase camera follows the car's heading. Speeds are per-second and
// scaled by dt, so the feel is identical at any timestep_hz — and identical
// between a live run-scene session and a replayed --input timeline.

fn step(world, step) {
    let dt = world.dt();
    let top_speed = 9.0;      // m/s while ArrowUp is held
    let reverse_speed = 4.0;  // m/s while ArrowDown is held
    let turn_rate = 110.0;    // deg/s at full steer

    let rot = world.rotation("Car");
    let heading = rot[1];

    let drive = 0.0;
    if world.key("ArrowUp") { drive += top_speed; }
    if world.key("ArrowDown") { drive -= reverse_speed; }

    // Steering only bites while driving, and flips in reverse — car rules.
    if drive != 0.0 {
        let steer = 0.0;
        if world.key("ArrowLeft") { steer += 1.0; }
        if world.key("ArrowRight") { steer -= 1.0; }
        let direction = if drive > 0.0 { 1.0 } else { -1.0 };
        heading += steer * direction * turn_rate * dt;
    }

    // The truck faces its local -Z, the same forward convention as lights
    // and cameras, so yaw maps to a forward vector of (-sin, 0, -cos).
    let rad = heading * 3.1415926535897932 / 180.0;
    let p = world.position("Car");
    let x = p[0] - sin(rad) * drive * dt;
    let z = p[2] - cos(rad) * drive * dt;

    // Keep the car on the ground plane; there is nothing to drive on beyond it.
    if x > 28.0 { x = 28.0; }
    if x < -28.0 { x = -28.0; }
    if z > 20.0 { z = 20.0; }
    if z < -20.0 { z = -20.0; }

    world.set_position("Car", x, p[1], z);
    world.set_rotation("Car", rot[0], heading, rot[2]);

    // Chase camera: behind and above the car, aimed just over its cab.
    world.set_position("ChaseCam", x + sin(rad) * 8.0, 3.2, z + cos(rad) * 8.0);
    world.look_at("ChaseCam", x, 1.0, z);
}
