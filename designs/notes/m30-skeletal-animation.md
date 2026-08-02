# Skeletal animation (M30, `designs/skeletal-animation-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Skeletal animation.*

*The design doc for this milestone is `designs/skeletal-animation-design.md` — it has the rejected
alternatives; this file has what the build learned.*

**CPU skeleton, GPU skin, and both halves of that sentence are forced.** Skinning cannot happen on
the CPU: posing vertices there mints a new `Arc<MeshData>` every frame and defeats M15's upload
cache (M18's argument, same answer). The skeleton cannot happen on the GPU without costing the
milestone its point — a joint palette is a few dozen matrices, and *because* they exist on the CPU
`engine list-joints --time 0.7` can say where every joint went, a script can put a torch in a hand,
and the whole sampling path is GPU-free and unconditionally testable the way `daylight.rs` is.

- **No new component.** `AnimationPlayer.clip` gains the fragment form `meshes/robot.glb#Walk` that
  M9's design specified and nothing had used. A skin is a property of the
  *asset*, and `Mesh.asset` already names it; a `Skeleton` component would be a second source of
  truth for what the file contains. The fragment is **required** even when the file has one clip —
  defaulting is friendly right up until someone exports a second one and which clip plays changes
  silently. Ownership rules, all validation errors: `skeletal_player_mesh_mismatch`,
  `clip_needs_fragment`, `mesh_has_no_skin`, `unknown_clip` (with `did_you_mean`), and
  `too_many_joints` past `MAX_JOINTS` (128) — refused before a device exists rather than a rig that
  renders correctly up to joint 128.
- **Rotation is a quaternion here, slerped, shortest-path — the opposite of M9's rule**, and the
  distinction is *who wrote the numbers*. A property clip's keys were typed by an agent into JSON
  where `[0, 360, 0]` is a sentence that must actually spin; a skeletal clip's came out of a DCC
  tool through a specified format where the only correct reading is the spec's. Don't "unify" them.
- **A skinned primitive loads unbaked.** glTF says the transform of the node referencing a skinned
  mesh is *ignored* — the palette already speaks skin space — while `gltf_mesh.rs` bakes node
  transforms for static geometry, which is right for that and exactly wrong here. This is the single
  most likely thing to be "simplified" back into a bug; the symptom is a character posed correctly
  in the wrong place, or one that doubles its own root transform. `JOINTS_1` (a fifth influence) is
  **refused**, not dropped: a dropped influence is a wrist that collapses under rotation.
- **The palette rides group 0 at binding 1** with its own dynamic offset — `downlevel_defaults` caps
  `max_bind_groups` at 4 and M26 spent the fourth on materials, so there is nowhere else. Packed as
  **three `vec4` rows, not `mat4x4`**: a joint matrix's fourth column is always `(0,0,0,1)` and
  storing it wastes a quarter of the 16 KiB budget. **Joint order is the skin's own `joints` order
  and must not be sorted** — unlike point lights, a joint's index is written into the vertex data.
- **The vertex stage is assembled from producer contributions**, not replaced wholesale. Texturing
  needs a UV the plain stage does not carry and skinning needs two more attributes, and a rigged
  character is precisely the thing that wants both — whole-stage replacement worked while exactly
  one producer did it. A `VertexContribution` names attributes, varyings, statements, and at most
  one expression transformed in place of `position`; `an_unassisted_vertex_stage_is_the_one_in_the_file`
  asserts the empty assembly equals `mesh.wgsl`'s stage **character for character**, which is what
  keeps M16's four untouchable lines reachable. The A/B said 29 of 29 committed render artifacts
  byte-identical.
- **The skinned pipelines are built lazily**, on the first frame that has a skinned draw — six
  shader modules is a real startup cost and one scene in this repo has a rig. Same precedent as the
  shadow map, the 1×1 white texture and the colour copy.
- **A skinned caster is its own pipeline**, because `shadow.wgsl` reads nothing but the model matrix
  and a walking character would otherwise cast its **rest pose** — a wrongness that reads as a
  renderer bug and is a missing pipeline. Both casters are skinned (solid and M26's alpha-cutout).
  The solid one is front-face culled, M16's peeling margin, which applies to characters too.
- **Scripts get two read-only getters and no setter**: `world.joint_position(entity, joint)` and
  `world.joint_transform(entity, joint)` (position plus XYZ Euler degrees, six numbers in one call
  so the rig is posed once). M21's reason — a script-settable joint is hidden state (invariant 2)
  and the pose must stay a function of (files, time). Hanging a prop off a hand is then an ordinary
  `set_position`, which bakes change-based and shows up in the trace. A mistyped joint is a located
  runtime error with `did_you_mean`, matching `world.key`.
- **The rest pose still needs a palette.** `render_items(assets)` is the rest pose and
  `render_items_at(assets, Some(t))` is posed; the tempting shortcut of an identity palette collapses
  any rig whose rest pose is not exactly its bind pose, since the vertices live in skin space.

Fixture `verify/m30_skeletal.json` at `--time 0.4`: two copies of `examples/meshes/rigged_arm.gltf`,
one playing `Wave`. **The two arms are the assertion** — they share a file, a mesh and a material, so
anything that made both wrong would leave them identical; only real skinning makes one bend and the
other stand, and the bent one's shadow bends with it. **Measured rather than assumed** (§9 warned it
might go the other way): this baseline is *not* per-build-profile, unlike trees and clouds — three
joints of slerp is not enough libm to reach a pixel, and a hundred-joint rig may not inherit that.
Test assets are generated text glTF like `pyramid.gltf`: `make_rigged_arm.py` (3 joints, the fixture)
and `make_rigged_walker.py` (16 joints in a branching tree — neck and elbows included, since a
stick-straight arm is a mannequin's — `Walk` + `Idle`, UVs — the tour's
character, and the only **skinned × textured** draw in the repo). The walker's gait is built from
named phase events (heel strike, toe-off, mid-swing) rather than one offset sine per joint; the
first version bent the knee on an offset sine and put peak flexion in mid-stance, which buckled
the planted leg — the single change that most made the character read as broken.

Not here, deliberately: blending, crossfades and state machines (M9 §8's rejection still standing —
blending reintroduces exactly the nondeterminism that made two clips on one property an error), IK
and root motion, retargeting, morph targets, skinned colliders (a skinned mesh is visual; physics
sees whatever `Collider` the entity carries, posed by its `Transform`), per-joint attachment
components, and editor picking against the posed mesh — CPU ray picking hits the rest pose.
