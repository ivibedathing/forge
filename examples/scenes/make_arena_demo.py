#!/usr/bin/env python3
"""Author `arena_shooter_demo.input.jsonl` — the canned fifteen-second run.

The arena's demo used to be fourteen hand-written lines, because aiming was
four arrow keys and "hold ArrowUp for two seconds" is a thing a person can
write down. With the mouse (M28) it is not: aiming is a *point on the frame*,
and which pixel is on a drone depends on where the camera happens to be. So
the timeline is authored the way `make_car_track_lap.py` authors the car's
lap — a closed loop that replays what it has written so far, asks the engine
where everything ended up, and appends the next tenth of a second:

    simulate --steps N --input <timeline so far>   ->  where is everything?
    project the nearest drone through the camera   ->  where should the cursor be?
    append one keyframe                            ->  repeat

    python3 examples/scenes/make_arena_demo.py

Two things are worth knowing about it.

**The projection here is the inverse of `world.cursor_ground`, and it is a
second implementation of one transform.** That is a real cost, taken with
open eyes: the alternative is a demo that never hits anything. It is kept
honest by the outcome rather than by agreement — this script prints the score
and the wave the run reaches, and a projection that drifted would show up
immediately as a run that shoots at nothing. The camera's *pose* is not
re-derived: it is read back out of the engine's own report every chunk.

**The director is not an AI and the timeline is not a replay of one.** It is
a choreography: aim at the nearest live drone, back away when one is close,
hold the trigger down. What lands in the repo is the finished keyframe file,
which is what the engine replays.
"""

import json
import math
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
ENGINE = os.path.join(ROOT, "bin", "engine")
SCENE = os.path.join(HERE, "arena_shooter.json")
OUT = os.path.join(HERE, "arena_shooter_demo.input.jsonl")

# The frame the timeline is authored against. A cursor is a fraction of the
# frame and the ray through it depends on the aspect, so a demo authored at
# 16:9 replays at any 16:9 size — which is what the doc's screenshot command
# uses (M28, `designs/mouse-input-design.md` §5).
WIDTH, HEIGHT = 960, 540
ASPECT = WIDTH / HEIGHT

# From the scene: `Eye` carries these, and the script never changes them.
FOV = 46.0
PITCH = -60.0

STEPS = 900  # fifteen seconds at 60 Hz
CHUNK = 6  # steps between decisions — a tenth of a second is 6 steps
DRONES = 10
DRONE_HOVER = 0.95  # the altitude they hold; the aim point's height

# The menu the run has to get through first (M28). Where the button *is* comes
# from the engine — see `play_cursor` — because since M31 nothing in either
# script knows: the menu is a `HudPanel` tree and the layout is the engine's.
CLICK_AT = 30  # step the demo presses PLAY
START_AT = 42  # first step the director drives


def run(steps, timeline, entities):
    """`engine simulate` over the timeline written so far.

    `--entity` narrows the report *and* refuses names that do not exist, and a
    drone this run already shot no longer does — it broke into fragments and
    was despawned. So an unknown name is not a failure here, it is the answer:
    drop what the error names and ask again. The error is structured JSON with
    the entity in it, which is what makes that a two-line loop rather than a
    substring hunt.
    """
    with open(OUT, "w") as f:
        f.write("\n".join(timeline) + "\n")
    while True:
        args = [ENGINE, "simulate", SCENE, "--steps", str(steps), "--input", OUT]
        for name in entities:
            args += ["--entity", name]
        out = subprocess.run(args, capture_output=True, text=True)
        if out.returncode == 0:
            report = json.loads(out.stdout)
            return {e["entity"]: e["position"] for e in report["entities"]}, entities
        missing = set()
        for line in out.stderr.splitlines():
            try:
                error = json.loads(line)
            except json.JSONDecodeError:
                continue
            if error.get("error") == "entity_not_found":
                missing.add(error["entity"])
        if not missing:
            raise SystemExit(f"simulate failed:\n{out.stderr}")
        entities = [name for name in entities if name not in missing]


def play_cursor():
    """The cursor that presses PLAY, asked of the engine rather than guessed.

    This used to be a pair of hand-tuned fractions, and it had to be: the menu
    was rectangles the game script computed at run time, so nothing outside
    that script could say where the button was. M31 made the menu a component
    tree, and `engine ui-layout` reports the same rectangle the hit test uses —
    at the frame this timeline is authored for. So the demo aims at the button
    the way it aims at a drone: it asks where the thing is and points at it.

    The centre of the rect as a *fraction* is the one place the pixel report
    and the fractional timeline have to agree, which is exactly the crossing
    M31's own CLI test pins.
    """
    out = subprocess.run(
        [ENGINE, "ui-layout", SCENE, "--width", str(WIDTH), "--height", str(HEIGHT),
         "--entity", "MenuButton"],
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        raise SystemExit(f"ui-layout failed:\n{out.stderr}")
    rect = json.loads(out.stdout)["elements"][0]["rect"]
    return ((rect[0] + rect[2] / 2) / WIDTH, (rect[1] + rect[3] / 2) / HEIGHT)


def to_cursor(target, eye):
    """Where `target` lands on the frame, as the cursor that points at it.

    The inverse of `Pointer::resolve`: rotate the offset into view space, divide
    by the perspective, and map [-1, 1] onto [0, 1] with the origin at the
    top-left corner.
    """
    dx = target[0] - eye[0]
    dy = target[1] - eye[1]
    dz = target[2] - eye[2]
    # The camera is pitched about X only, so the view rotation is one matrix
    # and its transpose is the view transform.
    p = math.radians(PITCH)
    cos, sin = math.cos(p), math.sin(p)
    vx = dx
    vy = cos * dy + sin * dz
    vz = -sin * dy + cos * dz
    if vz >= -0.001:
        return None  # behind the camera
    tan_half = math.tan(math.radians(FOV) * 0.5)
    ndc_x = vx / (tan_half * ASPECT * -vz)
    ndc_y = vy / (tan_half * -vz)
    return ((ndc_x + 1.0) * 0.5, (1.0 - ndc_y) * 0.5)


def clamp(v, lo, hi):
    return max(lo, min(hi, v))


def keyframe(step, held, cursor):
    cursor = [round(clamp(c, 0.01, 0.99), 3) for c in cursor]
    return json.dumps({"step": step, "held": sorted(held), "cursor": cursor})


def main():
    # Open on the title screen: move onto the button, press it, and let go —
    # the release on the button too, since M31's press capture wants a click to
    # start and finish on the same element, and a release somewhere else is
    # deliberately not a click.
    play = play_cursor()
    timeline = [
        keyframe(0, [], (0.5, 0.5)),
        keyframe(18, [], play),
        keyframe(CLICK_AT, ["MouseLeft"], play),
        keyframe(CLICK_AT + 6, [], play),
    ]
    watch = ["Player", "Eye"] + [f"Drone{n:02d}" for n in range(1, DRONES + 1)]
    last = None

    for step in range(START_AT, STEPS, CHUNK):
        state, watch = run(step, timeline, watch)
        player = state.get("Player")
        eye = state.get("Eye")
        if player is None or eye is None:
            raise SystemExit("the arena lost its player or its camera")

        # The nearest drone that is still flying — a broken one has left the
        # report entirely, which is how this loop knows what it killed.
        live = []
        for n in range(1, DRONES + 1):
            position = state.get(f"Drone{n:02d}")  # absent = shot
            if position is None or position[1] > 6.0:
                continue  # gone, or still parked up at its dormant altitude
            flat = math.hypot(position[0] - player[0], position[2] - player[2])
            live.append((flat, position))
        live.sort(key=lambda item: item[0])

        held = []
        if live:
            distance, target = live[0]
            aim = to_cursor((target[0], DRONE_HOVER, target[2]), eye)
            # Hold the trigger whenever there is something to shoot at. The
            # gun's own cooldown and auto-reload do the rest.
            held.append("MouseLeft")
            # Back off from anything inside knife range, in whichever world
            # direction is away from it — movement is world-relative, so the
            # keys map straight onto axes.
            if distance < 7.0:
                if abs(target[0] - player[0]) > abs(target[2] - player[2]):
                    held.append("KeyA" if target[0] > player[0] else "KeyD")
                else:
                    held.append("KeyW" if target[2] > player[2] else "KeyS")
        else:
            # Between waves: drift toward the middle of the arena and keep the
            # cursor ahead of the player rather than snapping it to a corner.
            aim = to_cursor((player[0] * 0.5, DRONE_HOVER, player[2] - 8.0), eye)
            if player[2] > 8.0:
                held.append("KeyW")

        if aim is None:
            continue
        line = keyframe(step, held, aim)
        # One keyframe per change: a held set and a cursor that both repeat
        # say nothing the previous line did not.
        body = json.loads(line)
        signature = (tuple(body["held"]), tuple(body["cursor"]))
        if signature != last:
            timeline.append(line)
            last = signature

    # Stop shooting at the end, so the last frame is a standing figure rather
    # than a muzzle flash.
    timeline.append(keyframe(STEPS - 30, [], (0.5, 0.4)))
    with open(OUT, "w") as f:
        f.write("\n".join(timeline) + "\n")

    # What the run actually did, which is the only check on the projection
    # above: a demo that hits nothing is a broken one.
    out = subprocess.run(
        [ENGINE, "simulate", SCENE, "--steps", str(STEPS), "--input", OUT],
        capture_output=True,
        text=True,
    )
    report = json.loads(out.stdout)
    names = {f"Drone{n:02d}" for n in range(1, DRONES + 1)}
    # Exact names only: a shot drone leaves `Drone03.frag1` and friends behind,
    # which are ordinary entities and would count as survivors.
    survivors = sum(1 for e in report["entities"] if e["entity"] in names)
    print(f"wrote {OUT}")
    print(f"  {len(timeline)} keyframes over {STEPS} steps ({STEPS / 60:.0f} s)")
    print(f"  drones still flying at the end: {survivors} of {DRONES}")


if __name__ == "__main__":
    main()
