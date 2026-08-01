# The car demo and its generated circuit

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §The car demo and its generated circuit.*

`examples/scenes/car_track.json` — box chassis (`builtin:cube`, ≈1.5 t via density) + four cylinder
wheels; `scripts/car.rhai` is only the *driver* (pedals, speed-scaled steering wheel with finite slew
rate, low-gear torque boost below 8 m/s so full-lock corners don't stall against front-tire slip
drag, chase camera). `car_track_lap.input.jsonl` is a committed recording driving three clockwise
laps and parking just past the start line — pinned by CLI test and `verify/baselines/m11_lap.png`.
Both were re-authored in M23 when the plates became a `Road`.

**The circuit is generated** (M15, rebuilt on `Road` in M23): `examples/scenes/make_car_track.py`
emits the scene from a closed polygon of 14 named corners (Spa in miniature), ≈546 m round with
≈7.6 m of elevation and grades to 7.5%. Authoring the loop as a *polygon* is what makes closure free:
a closed polygon returns to its first vertex and its exterior angles sum to one turn, so position,
heading, and the height profile all shut without a solver — corners carry `(x, z, radius, height)`
and nothing carries a heading. Two things the polygon can't guarantee refuse to build: a corner
radius too big for the edges feeding it (the *engine* checks this — `road_corner_does_not_fit`) and a
grade too steep for the car to climb (the emitter's business). Three geometry lessons are baked in
and easy to reintroduce by "simplifying":

- **One collider, not two.** Road and shoulder as two colliders at different heights builds a ledge
  at the asphalt edge, and a wheel that drops off it wedges against the step and stops the car dead.
  This is now a property of the `Road` component, which cannot express the two-surface version.
- **The guardrail is continuous.** Posts are spaced 5 m and are 5.4 m long; dashed barriers let the
  car slip between two and fall off the elevated road. They are placed along the centerline the
  engine reports (`engine road-centerline`), not one the emitter re-derives.
- **Radii are sized for the car, not the map.** The layout is Spa at ~1/15 but the car is full size,
  so no corner is under 12 m however tight the real one is.

`make_car_track_lap.py` authors the input timeline the same way it is replayed: a closed loop that
replays the whole timeline from step 0 each round, reads the car's state back out of the `simulate`
report's `hud` (a scratch copy of the scene whose driver pushes one telemetry line — HUD is output,
never input, so it drives identically), and appends the next tenth of a second of keys. Steering is
pure pursuit; the throttle brakes on a `v² = v_corner² + 2ad` envelope, without which the car reads
corners correctly and arrives far too fast anyway. Regenerating the track means regenerating the
timeline and re-blessing the baseline — both scripts print the start-line constants `car.rhai` needs.

**The circuit stands in weather**: it now carries `{"sky": true, "fog_density": 0.0012,
"shadows": true, "shadow_distance": 70.0, "samples": 4}`, keeping the hand-tuned `Sun`;
`make_car_track.py` also scatters **58 `Tree`s** by dart-throwing (rejecting any candidate within
`TREE_CLEARANCE` of the *road's own* reported centerline, so the treeline re-fits itself when the
corners move) and rings the track with **six `Cloud`s**. Three things are load-bearing. **No tree
carries a `Collider`** — they are scenery the car reaches only through a guardrail, and a
colliderless forest is what keeps the drive, the timeline, and the lap test's pinned HUD strings
(`LAP 4`, `LAST 63.70   BEST 59.47`) the ones the bare circuit had. **The clouds ring the circuit
rather than sitting over it** because `TopCam` looks down from ~270 m, so a cloud over the infield
hides the infield. **Placement is a hand-rolled LCG in the script**, because the forest is committed
scene data and `random` reshuffling under a Python upgrade would surface as a baseline diff that
looks like a renderer bug. This is also a data point on M22's MSAA caveat — 58 trees at `samples: 4`
against a *flat* ground plane rendered byte-identically 6 runs running, so it is relief, not fine
geometry alone, that costs this adapter its determinism.
