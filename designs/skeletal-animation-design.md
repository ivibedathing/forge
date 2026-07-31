# M27 — Skeletal Animation: Design

Companion to `agent-native-engine-design.md` and to `animation-system-design.md`, which deferred
this as **A2** in its §7 build order. Where any two conflict, the engine doc wins, then the
animation doc, then this one.

M9 gave the engine a time axis an agent can author as text: property clips, `--time`, `filmstrip`,
`list-animations`. What it deliberately left out was the kind of motion nobody authors as text — a
character walking. That is this milestone.

## 1. Scope

A `.glb` carrying a skin, a joint hierarchy and animation clips loads, poses, and renders; the pose
is a pure function of (files, time) like every other pose in this engine; and **the skeleton is
legible without opening the binary**, because a rig an agent cannot enumerate might as well not
exist (`animation-system-design.md` §2, principle 5).

Not in scope, and each with a reason:

- **Blending, crossfades, state machines.** M9 §8 rejected these for the ordering reason and the
  rejection still holds: blending two clips reintroduces exactly the nondeterminism that made
  "two clips animating one property" a validation error rather than last-writer-wins.
- **IK, root motion, retargeting, animation compression, morph targets.**
- **Skinned colliders.** A skinned mesh is visual. Physics sees whatever `Collider` the entity
  carries, posed by its `Transform` and nothing else.
- **Per-joint attachment components.** Parenting a prop to a hand is one line of script (§8), and
  a component for it would be a second way to express a transform.
- **Editor picking against the posed mesh.** CPU ray picking hits the rest pose. Stated, not fixed.

## 2. The split that decides everything: CPU skeleton, GPU skin

The joint palette is computed on the **CPU**, in `engine-core`, and applied to vertices on the
**GPU**, in a skinned pipeline variant. Both halves of that sentence are forced.

**Skinning cannot happen on the CPU.** M15 made `MeshSource::load_mesh` return `Arc<MeshData>` and
made the renderer's upload cache key the `Arc`'s *identity*. Posing vertices on the CPU mints a new
`Arc` every frame, so a character re-uploads its whole mesh every frame — the same argument that
put Gerstner waves in the vertex stage in M18, arriving at the same answer for the same reason.

**The skeleton cannot happen on the GPU.** Or rather: it could, and it would cost the milestone its
point. A joint palette is a few dozen matrices; computing them on the CPU is free, and *because*
they exist on the CPU:

- `engine list-joints --time 0.7` can report where every joint actually is;
- a script can ask `world.joint_position("Robot", "Hand.R")` and put a torch in it;
- the whole sampling path is GPU-free and unconditionally testable, the way `daylight.rs` is.

So the vertex data is uploaded once and never changes, and what changes per frame is ~6 KiB of
matrices. This is the arrangement every engine converges on; what is worth writing down is that
here it is *also* the thing that makes skeletal animation queryable as text, which is the half this
engine cares about most.

```
engine-assets   glTF → SkinData { joints, parents, inverse_binds, names }
                     → SkeletalClip { channels of (node, TRS, sampler) }
                     → MeshData { …, joint_indices, joint_weights }
engine-core     pose(skin, clip, t) -> Vec<Mat4>          pure, no GPU, no gltf crate
engine-render   palette → uniform → skinned pipeline variant
```

`engine-assets` stays the only crate that opens asset files; `engine-core` never learns what glTF
is. Same seam `MeshData` already sits on.

## 3. Sampling: the glTF spec, and why that is not a contradiction

M9's most load-bearing sentence is that **rotation interpolates component-wise on Euler degrees**,
because a `0 → 360` key pair must actually spin. Skeletal clips do the opposite: quaternions,
slerp, shortest path, exactly as the glTF specification says.

These are not in tension, and the distinction is *who wrote the numbers*.

A property clip's keys were typed by an agent into a JSON file. `[0, 360, 0]` is a sentence in the
file format the agent already knows — `Transform.rotation` is Euler degrees — and interpreting it
as the identity is the silent failure this engine exists to avoid. A skeletal clip's keys came out
of a DCC tool through a specified interchange format. Nobody typed them, nobody will read them, and
the only correct reading is the spec's. Re-deriving Euler angles from them to interpolate in
degrees would introduce gimbal artifacts into data that had none.

So: `Step`, `Linear` (slerp for rotation, lerp for translation and scale), and `CubicSpline` with
glTF's in-tangent/value/out-tangent triplets, normalized after evaluation for rotations. Shortest
path is taken by negating one quaternion when their dot product is negative — without it a
180°-crossing key pair spins the long way, which is a bug that only shows on one frame in ten and
is therefore worth pinning with a test rather than eyeballing.

Clip duration is the largest key time across channels, matching M9's rule that there is no separate
duration field to drift.

## 4. Scene integration: no new component

`AnimationPlayer.clip` gains a fragment form, which `animation-system-design.md` §4 already
specified and which nothing has used until now:

```json
{ "type": "AnimationPlayer", "clip": "meshes/robot.glb#Walk", "speed": 1.0, "looping": true }
```

One field, both kinds of animation. There is deliberately **no `Skin` or `Skeleton` component**:

- A skin is a property of the *asset*, not of the scene. `Mesh.asset` already names the asset, and
  a component restating what the file contains is a second source of truth for it (invariant 8's
  reasoning, applied to assets rather than to the editor).
- Components are plain data (invariant 5). A joint hierarchy is not data an agent authors or edits;
  it is data an agent *reads*, which is what §6 is for.

**Consequence, recorded rather than hidden:** the showcase tour's growth contract
(`repo_contracts.rs::showcase_tour_uses_every_component_the_engine_has`) keys on schema components,
so it structurally cannot notice that skeletal animation exists. The tour gets a rigged character
anyway (§9), because "every system running at once" is the tour's claim and a contract that cannot
see a system does not weaken the claim — but the gap is real and this paragraph is the record of
it. M21 put the first hole in that contract's premise; this is the second, and the two have
different shapes: M21's is an exemption the contract computes, this one is a system the contract
was never able to see.

### The ownership rules

M9 settled that a property clip animating a **dynamic** rigid body is `animation_on_dynamic_body`.
Skeletal animation needs three rules of its own, all of them validation errors rather than silent
behaviour:

- **The player and the mesh must name the same file.** A skeletal player on an entity whose
  `Mesh.asset` is a different glTF (or a `builtin:`) is `skeletal_player_mesh_mismatch`. The skin
  lives in the mesh file; a player pointing elsewhere is describing a rig that will never be
  applied.
- **The fragment is required.** `meshes/robot.glb` with no `#Clip` is `clip_needs_fragment`, even
  when the file contains exactly one clip. Defaulting to the only clip is friendlier right up until
  someone exports a second one, at which point which clip plays changes silently — the failure
  class this engine trades convenience to avoid.
- **The file must actually have a skin.** `mesh_has_no_skin` otherwise, because a skeletal player
  on an unskinned file is a request the engine can only answer by drawing the rest pose forever.

`unknown_clip` carries `did_you_mean` from the file's real clip names, per M9 §4.

### The transform composition, and the glTF rule that trips it

glTF says the transform of the node referencing a skinned mesh **is ignored**: joint matrices are
already expressed in the skin's space. The engine's own `Transform` on the entity is what places
the character in the world. So the vertex composition is

```
world = entity_model · Σ wᵢ · (jointGlobalᵢ · inverseBindᵢ) · position
```

and the trap is that `gltf_mesh.rs` currently **bakes node transforms into the vertices** — the
right call for static geometry (what a glTF viewer shows is what the entity renders) and exactly
wrong for a skinned primitive, whose vertices must stay in skin space for the palette to mean
anything. A skinned primitive therefore loads *unbaked*. This is one `if` in the loader and one
sentence in its module doc, and it is the single most likely thing to be "simplified" back into a
bug: the symptom is a character that renders in the right pose at the wrong place, or doubles its
own root transform.

Normals are skinned by the same palette's rotation part, then by the usual normal matrix.

## 5. The bind-group budget, again

M26 spent the fourth and last bind group. Under `downlevel_defaults` `max_bind_groups` is 4, and
since M26 the mesh pipelines use every one: **0** object, **1** frame, **2** frame textures,
**3** material. The palette has nowhere to go as a group of its own.

It goes in **group 0**, as a second binding with its own dynamic offset, under a group-0 layout
used by the skinned pipelines only. That mirrors what `water_objects`, `cloud_objects` and
`road_objects` already do — a per-draw uniform array addressed by dynamic offset — and it costs
the plain pipelines nothing, because they keep the group-0 layout they have.

- **Fixed-size, `MAX_JOINTS = 128`**, the `MAX_POINT_LIGHTS` / `MAX_ROAD_KERBS` idiom: a rig with
  more is `too_many_joints` at **validate** time, before a device exists, rather than a character
  that renders correctly up to joint 128 and explodes past it.
- **Packed as three `vec4` rows**, not `mat4x4`. A joint matrix is affine; its fourth column is
  `(0,0,0,1)` and storing it wastes a quarter of the budget. 128 joints × 48 bytes = 6 KiB, against
  the 16 KiB `max_uniform_buffer_binding_size` `downlevel_defaults` guarantees. At `mat4x4` the
  same rig costs 8 KiB and 128 is the ceiling rather than a comfortable limit.
- **Joint order is the skin's `joints` array order**, which is the order `JOINTS_0` indexes. It is
  not sorted and must not be: unlike point lights, whose uniform index must not depend on archetype
  iteration and are therefore name-sorted, a joint's index is *written into the vertex data*.

## 6. Legibility: `engine list-joints`

The command that makes this milestone agent-native rather than merely present.

```
engine list-joints <scene-or-mesh> [--entity NAME] [--time T]
```

Without `--time`, the rig: each joint's name, parent, index, and rest transform. With `--time`, the
same plus each joint's **posed world transform** at that moment — which is the thing an agent needs
and cannot otherwise get, because a filmstrip shows that *something* moved and never that the hand
reached the doorknob.

That closes the loop the README promises. *Discover by looking* is `filmstrip`; *verify by
querying* is:

```bash
bin/engine list-joints scene.json --entity Robot --time 0.7 | jq '.joints[] | select(.name=="Hand.R") | .world.position'
```

Design points, following M24's precedents:

- **Reports do not pretty-print**; schemas do. One JSON object on stdout.
- `--time` takes `allow_hyphen_values`, on the class M24 fixed, so a negative time parses.
- It **needs no `Collider` and no GPU**, which is what separates it from every other way of asking
  where something is.
- Joint order is the skin's, not sorted, matching §5 — and the report says so by carrying `index`.
- Applied to a `.glb` directly it reports the rig; applied to a scene it reports every skinned
  entity's, or one with `--entity`.

`engine list-animations` learns about glTF at the same time: clip names, durations, and channel
targets, including **channels that target nodes outside the skin**, which are ignored during
sampling. An ignored channel that nothing reports is invisible; an ignored channel the CLI names is
a fact about the asset.

## 7. The renderer: a producer, and an anchor that has to change

Skinning is a `Producer` at the surface seam M26 named — a prelude plus anchored substitutions
against `mesh.wgsl`, composed with the others so that a skinned character can also be textured and
can also refract. The plain pipeline still compiles `mesh.wgsl` as it sits on disk, byte-identical
by construction, and every producer's substitution is asserted to land.

**But two producers now want the same anchor, and that is new.** `texture_producer` replaces
`anchor::VERTEX_STAGE` wholesale, because it needs an attribute the plain stage does not carry. So
does skinning, for the same reason — and a rigged character is *precisely* the thing that also
wants an albedo map and a normal map, so the two must compose rather than compete. Whole-stage
replacement worked while exactly one producer did it; it does not survive two.

The fix is to make the vertex stage **assemble from contributions** instead of being one
replaceable blob: a producer declares the attributes it adds, the varyings it adds, and the
expressions that produce the skinned position and normal, and `with_surface` builds the stage. The
composition is ordered — skinning transforms the position that texturing then passes through
untouched — and the assembled stage for "no producers" must be byte-identical to the one in
`mesh.wgsl`, which is an assertion, not a hope.

This is the milestone's structural risk. It touches the mechanism that guards M16's four
untouchable lines, so it gets the `ab-check` against `main` before anything else lands, and the
answer that matters is *zero pixels moved in the whole manifest*, not *the baselines still pass*.

### Vertex data

`MeshData` gains `joint_indices: Vec<[u16; 4]>` and `joint_weights: Vec<[f32; 4]>`, **empty for
every mesh that is not skinned** — which is every mesh in the repo today, so no committed vertex
buffer or vertex layout changes. Two extra vertex buffers are uploaded for skinned meshes only,
bringing them to five of the eight `downlevel_defaults` allows.

More than four influences per vertex (`JOINTS_1`) is `ASSET_UNSUPPORTED` with a message saying so,
rather than silently dropping the fifth influence — a dropped influence shows up as a wrist that
collapses under rotation, which is a hard thing to trace back to the loader.

### Shadows

A skinned caster needs a **skinned shadow pipeline**: `shadow.wgsl` reads nothing but the object
uniform's MVP and has no vertex skinning, so a walking character would cast its rest-pose shadow —
a wrongness that reads as a renderer bug and is actually a missing pipeline. M26 already set this
precedent with the alpha-cutout caster. Same prelude, same palette binding, depth-only.

Note for whoever debugs a missing shadow here: the solid caster is **front-face culled** (M16's
peeling margin), and that is documented in CLAUDE.md as a trap for single-sided cards. It applies
to characters too.

## 8. Scripts

Two read-only getters, following M21's precedent of adding exactly what is needed and no setter:

- `world.joint_position(entity, joint)` — world position, as a vec3 the script can assign.
- `world.joint_transform(entity, joint)` — position and rotation, for aiming as well as placing.

There is no setter, for M21's reason: a script-settable joint is hidden state (invariant 2), and
the pose must stay a function of (files, time). Attaching a prop to a hand is then

```rhai
let p = world.joint_position("Robot", "Hand.R");
world.set_position("Torch", p[0], p[1], p[2]);
```

which bakes change-based like every other script-driven transform, needs no new component, and is
visible in the trace. An unknown joint name is a runtime error with `did_you_mean`, matching
`world.key`.

## 9. Verification

- **Fixture** `examples/scenes/verify/m27_skeletal.json` plus its baseline, aimed at its subject
  with no terrain in frame — M22's rule, so it carries a hard bit-exact pin rather than a
  `diff_args` tolerance.
- **Test asset** `examples/meshes/rigged_arm.gltf`, generated by
  `examples/meshes/make_rigged_arm.py`: text glTF, embedded base64 buffer, three joints, one clip.
  Same discipline as `pyramid.gltf` and `textured_quad.gltf` — the checked-in asset is reproducible
  from text, which is as close to invariant 1 as a mesh gets.
- **A filmstrip** of the fixture, looked at. Every model rule in the tree system came out of
  looking at renders.
- **`list-joints --time` under test**: the arm's tip is somewhere different at t=0.5 than at t=0,
  asserted numerically. This is the milestone proving its own claim — motion verified without a
  pixel.
- **Determinism**: pose is a pure function of (files, time), so `--time T` and `t = loop period`
  reproduce byte-identically, the M9 property, now on a skinned mesh.
- **Build profile**: the palette is CPU floating-point trigonometry (slerp calls `acos` and `sin`),
  which is what made **tree and cloud baselines per build profile** in M19/M20 — libm, not FMA.
  Assume skeletal baselines are too, bless from the **debug** binary, and measure rather than
  assume.
- The full sweep: `validate --strict`, `cargo test --workspace`, `bin/verify-baselines`, and the
  A/B between binaries for §7.

The showcase tour gets a rigged character walking through a station (§4's recorded gap). Its six
baselines are re-blessed as part of that, and nothing else in the manifest may move.

## 10. Build order

1. **S0 — the skeleton as text.** glTF skin/joint/clip extraction, `#Clip` resolution, pure
   sampling, `list-joints`, `list-animations` learning glTF, the whole validation story. Verified
   by unit and CLI tests. Nothing visual. *This is M9's own A0-before-rendering lesson: the
   determinism and validation story is load-bearing, not polish.*
2. **S1 — pixels.** `MeshData`'s two new arrays, the vertex-stage anchor refactor, the skinning
   producer, the skinned shadow caster, the fixture and its baseline.
3. **S2 — reach.** The script getters, the tour character, CLAUDE.md and this document's §11.

## 11. What building it actually taught

*(Written as each stage lands, like M26 §11.5.)*

### S1 — pixels

- **The vertex-stage refactor came out smaller than §7 feared, because the fix was to make the
  *seam* narrower rather than the mechanism cleverer.** A producer does not contribute a stage; it
  contributes attributes, varyings, statements, and — at most one of them — *the expression the
  stage transforms in place of `position`*. Assembly is then one `format!` over a fixed skeleton,
  and the empty case reproduces `mesh.wgsl`'s stage character for character, which is an assertion
  rather than a hope. The A/B says all 29 committed render artifacts moved zero pixels.
- **Where the palette lives was decided by what already depends on time.** Everything else in a
  draw list was posed by the caller before `render_items` saw it; the palette is the only thing in
  it that is a function of the clock. So `render_items_at(assets, Some(t))` is a second entry point
  rather than a parameter threaded through twenty call sites that cannot have a skin — and
  `render_items(assets)`, the rest pose, is what the editor's viewport wants anyway, since it shows
  scenes at rest.
- **A rest pose still needs a palette.** The obvious shortcut — no clock, no palette, identity —
  collapses any rig whose rest pose is not exactly its bind pose, because the vertices are in skin
  space and `global · inverse_bind` is what puts them back. The fixture's second arm exists to catch
  precisely that, and it caught nothing only because the rule was written down first.
- **The skinned pipelines are built lazily, on the first frame that has a skinned draw.** Six shader
  modules is a real startup cost and every `engine screenshot` in this repo but one has no rig in
  it. The precedent was already here: the shadow map, the 1×1 white texture and the colour copy are
  all allocated by the first frame that needs them.
- **Influences are written for every primitive in a file or for none.** A file mixing a skinned
  primitive with a static one would otherwise leave the two arrays shorter than the positions they
  parallel, and the shader would read one primitive's influences against another's vertices. Static
  vertices take all-zero weights, which `skin.wgsl` reads as "leave this vertex alone" — attaching
  them to joint 0 instead would drag them around with a bone. A file where nothing turned out to be
  skinned gives the arrays back, so an unskinned mesh uploads exactly the buffers it always did.
- **§9's build-profile warning did not hold, and that is why it said to measure.** The skeletal
  baseline renders byte-identically from the debug and release binaries, unlike M19's trees and
  M20's clouds. Three joints of slerp is not enough libm to reach a pixel; a hundred-joint rig may
  not inherit that.
- **A sweep is a lossy way to observe M22's residue, and this milestone finally measured the rate.**
  Two showcase frames failed one sweep out of five and passed every other; the reflex reading is
  "the change moved a pixel". Rendering `--steps 585` ten times with each binary settled it —
  **three distinct images from this branch's binary, two from `main`'s** — so the frame is
  nondeterministic on both sides and the difference in counts is sampling noise. `showcase_90` and
  `showcase_585` join `showcase_646` and `showcase_810` in CLAUDE.md's record. The lesson for the
  next milestone: when a sweep fails and the A/B is clean, `md5` N renders of the one frame rather
  than running the sweep again.

### S2 — reach

- **The script API needed the rigs, and the layering decided how.** `engine-script` must not learn
  what glTF is, so `ScriptHost::build` takes a `&dyn RigSource` — the trait already in
  `engine-core` — and resolves the scene's skinned assets once at construction. Rigs are `Arc`s, so
  holding them costs a pointer each and spares `world.joint_position` a file read per call. Only
  *skinned* files are kept, so "does this entity have a rig" is a lookup that can fail rather than
  one that always succeeds with an empty answer.
- **`world.joint_transform` returns six numbers in one call rather than two calls of three.** Two
  calls would pose the rig twice for one question, and the second pose is not free — it walks the
  whole hierarchy. Rotation comes back as XYZ Euler degrees, the file's convention, so it can be
  written straight back through `set_rotation`.
- **`closest_match` became public.** A runtime error is not an `EngineError` until it leaves the
  script host, so the joint-name suggestion could not go through `suggest_from` — and a second
  similarity threshold beside the first is how two spellings of "close enough" start disagreeing.
- **The tour needed a character, and the fixture is not one.** `rigged_arm.gltf` is three joints in
  a chain: the smallest thing that can prove a palette composes, and unable to be *wrong* in the
  ways a character is. `make_rigged_walker.py` is the second generator — thirteen joints in a
  branching tree, limbs that pass each other mid-stride, a loop whose last keyframe is computed as
  its first rather than copied. A chain resolves parents in whatever order it is written; a tree
  does not, which is what exercises `joint_globals`' resolution on real data.
- **One added entity changed exactly the frames it is in, and that was checkable.** Five of the six
  showcase baselines moved and `showcase_450` came back byte-identical — station 03's camera is
  aimed away. The three small diffs at stations 02, 04 and 05 looked like M22's flake and were not:
  eight renders of each came back as **one** image apiece, none equal to the old baseline. The
  general technique, which S1 also needed: a stable-but-different render is a real change, a
  different-every-time render is the adapter.
- **The tour's growth contract cannot see this milestone, exactly as §4 predicted.** The record is
  now in three places — here, `showcase-tour.md`, and CLAUDE.md — because the thing a contract
  cannot check is precisely the thing that needs writing down.
