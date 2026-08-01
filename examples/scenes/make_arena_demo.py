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

STEPS = 2100  # thirty-five seconds at 60 Hz
CHUNK = 6  # steps between decisions — a tenth of a second is 6 steps
DRONES = 10  # per level; the file carries LEVELS x DRONES of them
LEVELS = 4
DRONE_HOVER = 0.95  # the altitude they hold; the aim point's height
# How long the demo gives a level's drones to fly down from their park altitude
# after it presses NEXT LEVEL. Below this the director would be aiming at an
# empty arena and would wander off to the middle.
DROP_STEPS = 30

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


def button_cursor(steps=0):
    """The cursor that presses the menu's first button, asked of the engine.

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
    args = [ENGINE, "ui-layout", SCENE, "--width", str(WIDTH), "--height", str(HEIGHT),
            "--entity", "MenuBtn1"]
    # `--steps` (M36) is what makes this work for the *second* button too. The
    # menu is seven slots the game script labels per screen, so the NEXT LEVEL
    # card is a different height from the title card and its button is
    # somewhere else — a fact that is not in the file, because which slots are
    # on is what the script painted. At rest this reports the title screen,
    # which the scene authors exactly; with `--steps` it reports whatever card
    # the run has reached.
    if steps:
        args += ["--steps", str(steps), "--input", OUT]
    out = subprocess.run(args, capture_output=True, text=True)
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


def level_drones(level):
    """The ten entity names that are level `n`'s drones.

    Levels own contiguous runs of a flat numbering — `make_arena.py` prints the
    boundaries — so this is arithmetic rather than a table the two files would
    have to keep in agreement.
    """
    first = (level - 1) * DRONES + 1
    return [f"Drone{n:02d}" for n in range(first, first + DRONES)]


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
    play = button_cursor()
    timeline = [
        keyframe(0, [], (0.5, 0.5)),
        keyframe(18, [], play),
        keyframe(CLICK_AT, ["MouseLeft"], play),
        keyframe(CLICK_AT + 6, [], play),
    ]
    # The campaign, as the director sees it: a level is its ten drone names,
    # and the level is over when the report has none of them left — a broken
    # drone leaves the report entirely, which is the same signal this loop has
    # always used for a kill, read one level up.
    level = 1
    watch = ["Player", "Eye"] + level_drones(level)
    last = None
    resume_at = 0

    for step in range(START_AT, STEPS, CHUNK):
        state, watch = run(step, timeline, watch)
        player = state.get("Player")
        eye = state.get("Eye")
        if player is None or eye is None:
            raise SystemExit("the arena lost its player or its camera")

        # The nearest drone that is still flying — a broken one has left the
        # report entirely, which is how this loop knows what it killed.
        live = []
        parked = 0
        for name in level_drones(level):
            position = state.get(name)  # absent = shot
            if position is None:
                continue
            if position[1] > 6.0:
                parked += 1  # still up at its dormant altitude, or dropping in
                continue
            flat = math.hypot(position[0] - player[0], position[2] - player[2])
            live.append((flat, position))
        live.sort(key=lambda item: item[0])

        # Nothing of this level left anywhere: the card is up and the button
        # says NEXT LEVEL. Press it the way the title screen is pressed —
        # release on the button, since M31 captures a press — and start
        # watching the next level's ten.
        if not live and not parked and level < LEVELS and step >= resume_at:
            # Ask where NEXT LEVEL is *on this card*, rather than reusing the
            # title screen's rect: the cards hug their contents, so a
            # one-button card puts its button somewhere a four-button one does
            # not. `--steps` replays the timeline written so far, which is the
            # same closed loop this whole file is.
            press = button_cursor(step)
            timeline.append(keyframe(step, [], press))
            timeline.append(keyframe(step + 8, ["MouseLeft"], press))
            timeline.append(keyframe(step + 14, [], press))
            level += 1
            watch = ["Player", "Eye"] + level_drones(level)
            last = None
            resume_at = step + 14 + DROP_STEPS
            continue
        if step < resume_at:
            continue  # the level's drones are on their way down

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
    #
    # Guarded on the last step already written, because a NEXT LEVEL press near
    # the end of the run appends three keyframes up to `step + 14` and can
    # overshoot this one — and a timeline whose steps are not strictly
    # increasing is `unsorted_input_steps`, refused by the engine. The demo
    # then simply ends on whatever the last press left held, which is a worse
    # final frame and not a broken file.
    last_step = json.loads(timeline[-1])["step"]
    if STEPS - 30 > last_step:
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
    names = {name for n in range(1, LEVELS + 1) for name in level_drones(n)}
    # Exact names only: a shot drone leaves `Drone03.frag1` and friends behind,
    # which are ordinary entities and would count as survivors.
    standing = {e["entity"] for e in report["entities"] if e["entity"] in names}
    killed = len(names) - len(standing)
    print(f"wrote {OUT}")
    print(f"  {len(timeline)} keyframes over {STEPS} steps ({STEPS / 60:.0f} s)")
    print(f"  reached level {level} of {LEVELS}; {killed} drones destroyed")


if __name__ == "__main__":
    main()
