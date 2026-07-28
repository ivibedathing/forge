"""Author examples/scenes/car_track_lap.input.jsonl: a driven lap of the track.

The timeline this writes is a *recording* of arrow keys, the same file
`run-scene --record-input` would produce from a human at the keyboard, and it
is replayed by `--input` on simulate/screenshot/diff-render. Nothing here runs
at replay time; this script exists only because the recording has to come from
somewhere, and a track that is generated needs a lap that can be regenerated
with it.

How it drives: a closed loop against the real engine. Each round it replays the
timeline built so far from step 0, reads where the car ended up, picks the keys
for the next tenth of a second, and appends them. Replaying from zero every
round rather than resuming from a bake is the point — what the autopilot sees
is exactly what a replay will see, so the finished timeline cannot drift from
the drive that authored it.

Steering is pure pursuit along the centerline: aim at a point some meters
ahead, hold the arrow that closes the angle. The throttle looks further ahead
still, at how sharply the road bends, and brakes for anything too tight to take
at the current speed.

The car reports itself through the HUD. `simulate` returns the final step's
`world.hud` lines in its JSON report, so a scratch copy of the scene whose
driver script pushes one extra line of telemetry turns the engine into a
readable process — and since HUD lines are output, never input, the scratch
copy drives identically to the committed one.

Usage:
    python3 examples/scenes/make_car_track.py --centerline /tmp/centerline.json
    python3 examples/scenes/make_car_track_lap.py --centerline /tmp/centerline.json \\
        --engine target/release/engine [--laps 3]
"""

import argparse
import json
import math
import os
import shutil
import subprocess
import tempfile

CHUNK = 6               # steps committed per round: a tenth of a second blind
KEYS = ("ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight")

# Pure pursuit. The lookahead grows with speed, because a fixed one oscillates
# when fast and cuts corners when slow.
LOOKAHEAD_BASE = 7.0
LOOKAHEAD_PER_SPEED = 0.75
LOOKAHEAD_MAX = 24.0
STEER_DEADBAND = 1.5    # degrees of heading error worth touching the wheel for

# Speed policy, in two halves. A corner's own speed comes from lateral grip
# (v = sqrt(a / kappa)); the speed allowed *here* is that corner speed plus
# whatever braking can still shed before reaching it. Without the second half
# the car reads the corner correctly and arrives at it far too fast anyway.
LATERAL_GRIP = 3.6      # m/s^2 the tires will actually hold on this car
BRAKE_DECEL = 4.5       # m/s^2 the brakes reliably deliver
BRAKE_MARGIN = 0.6      # m/s over target before lifting into the brakes
SCAN_AHEAD = 45.0       # meters of road the throttle looks over
EDGE_CAUTION = 3.0      # meters off the centerline that counts as running wide

# Getting unstuck. Nose the guardrail slowly enough and the car will sit there
# with the throttle open, so a stall is detected and reversed out of rather
# than driven into harder.
STALL_SPEED = 0.6       # m/s below which the car counts as not moving
STALL_ROUNDS = 4        # consecutive stalled rounds before backing up
REVERSE_STEPS = 54      # steps spent backing away before trying again

PARKED_SPEED = 0.1      # m/s at which the car counts as stopped
SETTLE_STEPS = 90       # steps of nothing held before the recording ends


def load_centerline(path):
    data = json.load(open(path))
    nodes = [(n[0], n[1], n[2]) for n in data["nodes"]]
    # Drop the repeated closing node: the autopilot wraps around instead.
    if math.dist(nodes[0], nodes[-1]) < 1e-6:
        nodes = nodes[:-1]
    cumulative = [0.0]
    for i in range(1, len(nodes) + 1):
        cumulative.append(cumulative[-1] + math.dist(nodes[i - 1], nodes[i % len(nodes)]))
    return nodes, cumulative, cumulative[-1]


def scratch_scene(scene_path, workdir):
    """A copy of the scene whose driver also reports the car's state."""
    scene = json.load(open(scene_path))
    source = os.path.join(os.path.dirname(scene_path), "scripts", "car.rhai")
    driver = open(source).read()

    # Positions in centimeters and the heading in thousandths: integers, so
    # Rhai's float formatting never enters into it.
    driver += """
fn report(world) {
    let p = world.position("Car");
    let f = world.forward("Car");
    let v = world.linear_velocity("Car");
    let speed = sqrt(v[0] * v[0] + v[2] * v[2]);
    world.hud("AP " + (p[0] * 100.0).round().to_int()
        + " " + (p[1] * 100.0).round().to_int()
        + " " + (p[2] * 100.0).round().to_int()
        + " " + (f[0] * 1000.0).round().to_int()
        + " " + (f[2] * 1000.0).round().to_int()
        + " " + (speed * 100.0).round().to_int());
}
"""
    driver = driver.replace("fn step(world, step) {",
                            "fn step(world, step) {\n    report(world);", 1)

    os.makedirs(os.path.join(workdir, "scripts"), exist_ok=True)
    open(os.path.join(workdir, "scripts", "car.rhai"), "w").write(driver)
    out = os.path.join(workdir, "scene.json")
    json.dump(scene, open(out, "w"))
    return out


def read_state(engine, scene, timeline_path, steps):
    """Replay the timeline from step 0 and report where the car ended up."""
    result = subprocess.run(
        [engine, "simulate", scene, "--steps", str(steps), "--input", timeline_path],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"engine simulate failed:\n{result.stderr}")
    report = json.loads(result.stdout)
    for line in report.get("hud", []):
        if line.startswith("AP "):
            n = [int(v) for v in line.split()[1:]]
            return {
                "position": (n[0] / 100.0, n[1] / 100.0, n[2] / 100.0),
                "forward": (n[3] / 1000.0, n[4] / 1000.0),
                "speed": n[5] / 100.0,
            }
    raise SystemExit("the scratch driver reported no AP line; is the HUD full?")


def nearest_index(nodes, position, previous):
    """Index of the closest centerline node, searched forward from the last."""
    best, best_distance = previous, float("inf")
    for offset in range(-4, 40):
        i = (previous + offset) % len(nodes)
        d = math.dist((nodes[i][0], nodes[i][2]), (position[0], position[2]))
        if d < best_distance:
            best, best_distance = i, d
    return best, best_distance


def point_ahead(nodes, cumulative, total, index, distance):
    """The centerline point a given distance further around the lap."""
    target = (cumulative[index] + distance) % total
    for k in range(len(nodes)):
        i = (index + k) % len(nodes)
        j = (i + 1) % len(nodes)
        span = (cumulative[i + 1] - cumulative[i])
        offset = (target - cumulative[i]) % total
        if span > 0.0 and offset <= span:
            f = offset / span
            return (nodes[i][0] + (nodes[j][0] - nodes[i][0]) * f,
                    nodes[i][2] + (nodes[j][2] - nodes[i][2]) * f)
    return (nodes[index][0], nodes[index][2])


def corner_speed(nodes, cumulative, total, index):
    """The fastest the road ahead allows, braking distance included."""
    limit = 99.0
    step = 4.0
    distance = 0.0
    while distance < SCAN_AHEAD:
        a = point_ahead(nodes, cumulative, total, index, distance)
        b = point_ahead(nodes, cumulative, total, index, distance + step)
        c = point_ahead(nodes, cumulative, total, index, distance + 2 * step)
        # Menger curvature of the three samples: 4 * area / (product of sides).
        area = abs((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])) / 2.0
        sides = math.dist(a, b) * math.dist(b, c) * math.dist(a, c)
        if sides > 1e-6:
            curvature = 4.0 * area / sides
            if curvature > 1e-4:
                corner = math.sqrt(LATERAL_GRIP / curvature)
                # v^2 = corner^2 + 2 a d: fast now is fine if the brakes can
                # still wash it off before the corner arrives.
                limit = min(limit, math.sqrt(
                    corner * corner + 2.0 * BRAKE_DECEL * distance))
        distance += step
    return limit


def drive(state, nodes, cumulative, total, index, offside, top_speed):
    """One round of decisions: which arrows are held for the next chunk."""
    position, forward, speed = state["position"], state["forward"], state["speed"]
    lookahead = min(LOOKAHEAD_MAX, LOOKAHEAD_BASE + LOOKAHEAD_PER_SPEED * speed)
    target = point_ahead(nodes, cumulative, total, index, lookahead)

    to_target = (target[0] - position[0], target[1] - position[2])
    distance = math.hypot(*to_target)
    if distance < 1e-6:
        return []
    to_target = (to_target[0] / distance, to_target[1] / distance)

    # Signed angle from the car's nose to the aiming point. Right of forward
    # is (-f_z, f_x), the same convention the driver script uses.
    cross = forward[0] * to_target[1] - forward[1] * to_target[0]
    dot = max(-1.0, min(1.0, forward[0] * to_target[0] + forward[1] * to_target[1]))
    error = math.degrees(math.atan2(cross, dot))

    state["error"] = error
    state["target"] = target
    held = []
    if error > STEER_DEADBAND:
        held.append("ArrowRight")
    elif error < -STEER_DEADBAND:
        held.append("ArrowLeft")

    wanted = min(top_speed, corner_speed(nodes, cumulative, total, index))
    # Running wide is the failure that ends a recording: off the asphalt edge
    # the car trips over the guardrail. Back off until it is back on line.
    if offside > EDGE_CAUTION:
        wanted = min(wanted, 6.0)
    # Reversing into the scenery is worse than a slow lap: never brake below
    # walking pace, just coast.
    if speed > wanted + BRAKE_MARGIN and speed > 3.0:
        held.append("ArrowDown")
    elif speed < wanted:
        held.append("ArrowUp")
    return held


def write_timeline(path, entries):
    with open(path, "w") as handle:
        for step, held in entries:
            handle.write(json.dumps({"step": step, "held": held}) + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--scene", default="examples/scenes/car_track.json")
    parser.add_argument("--centerline", required=True)
    parser.add_argument("--out", default="examples/scenes/car_track_lap.input.jsonl")
    parser.add_argument("--engine", default="target/release/engine")
    parser.add_argument("--laps", type=int, default=3)
    parser.add_argument("--top-speed", type=float, default=13.5)
    parser.add_argument("--max-steps", type=int, default=20000)
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--every", type=int, default=25)
    args = parser.parse_args()

    nodes, cumulative, total = load_centerline(args.centerline)
    workdir = tempfile.mkdtemp(prefix="car-track-lap-")
    try:
        scene = scratch_scene(args.scene, workdir)
        timeline = os.path.join(workdir, "drive.input.jsonl")

        # `simulate --steps 0` has no HUD to read, so the first chunk is the
        # one decision made blind: pull away from the line.
        entries = [(0, ["ArrowUp"])]
        held = ["ArrowUp"]
        step = CHUNK
        index = 0
        lap = 0
        stalled = 0
        reversals = 0
        reverse_until = 0
        settle_until = 0
        travelled = 0.0
        previous_progress = 0.0
        parking = False

        while step < args.max_steps:
            write_timeline(timeline, entries or [(0, [])])
            state = read_state(args.engine, scene, timeline, step)
            index, offside = nearest_index(nodes, state["position"], index)

            # Lap counting off the centerline: progress wraps once per lap.
            progress = cumulative[index]
            if progress + total / 2.0 < previous_progress:
                lap += 1
                print(f"  lap {lap} complete at step {step}", flush=True)
            travelled += (progress - previous_progress) % total
            previous_progress = progress

            if reversals > 40:
                raise SystemExit(
                    f"the autopilot could not get round: 40 stalls by step {step}"
                )

            if offside > 8.0:
                raise SystemExit(
                    f"the autopilot left the road at step {step}: "
                    f"{offside:.1f}m from the centerline near node {index}"
                )

            if lap >= args.laps and not parking:
                parking = True
                print(f"  {args.laps} laps done at step {step}; braking to a stop",
                      flush=True)

            if parking:
                # Hold the brake until the car is properly stopped, not merely
                # slow: released at a walking pace it keeps creeping, and the
                # recording ends on a speedometer that does not read zero.
                held = ["ArrowDown"] if state["speed"] > PARKED_SPEED else []
            elif step < reverse_until:
                # Backing off the wall. Reversed, the wheel works the other
                # way round: to swing the nose toward the line, steer away
                # from it.
                held = ["ArrowDown"]
                drive(state, nodes, cumulative, total, index, offside,
                      args.top_speed)
                if state["error"] > STEER_DEADBAND:
                    held.append("ArrowLeft")
                elif state["error"] < -STEER_DEADBAND:
                    held.append("ArrowRight")
            else:
                held = drive(state, nodes, cumulative, total, index,
                             offside, args.top_speed)
                if state["speed"] < STALL_SPEED:
                    stalled += 1
                    if stalled >= STALL_ROUNDS:
                        reverse_until = step + REVERSE_STEPS
                        reversals += 1
                        stalled = 0
                        print(f"  stalled at step {step} near node {index}; "
                              f"backing up ({reversals})", flush=True)
                        held = ["ArrowDown"]
                else:
                    stalled = 0

            if args.verbose and (step // CHUNK) % args.every == 0:
                print(f"  step {step:5} node {index:3} off {offside:4.1f}m "
                      f"at ({state['position'][0]:7.1f},{state['position'][2]:7.1f}) "
                      f"fwd ({state['forward'][0]:+.2f},{state['forward'][1]:+.2f}) "
                      f"err {state.get('error', 0.0):+6.1f} "
                      f"{state['speed']:5.1f} m/s  {','.join(held) or '-'}",
                      flush=True)

            # Once stopped, let it settle for a moment with everything
            # released, so the last frame of the recording is a parked car.
            if parking and state["speed"] <= PARKED_SPEED:
                if settle_until == 0:
                    settle_until = step + SETTLE_STEPS
                elif step >= settle_until:
                    break

            if not entries or entries[-1][1] != held:
                entries.append((step, held))
            step += CHUNK

        write_timeline(args.out, entries)
        state = read_state(args.engine, scene, args.out, step)
        print(f"wrote {args.out}: {len(entries)} keyframes over {step} steps")
        print(f"  {lap} laps, {travelled:.0f}m driven, parked at "
              f"({state['position'][0]:.2f}, {state['position'][2]:.2f}) "
              f"doing {state['speed']:.2f} m/s")
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    main()
