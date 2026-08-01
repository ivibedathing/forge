# Water (M18)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Water.*

**A body of water is one entity with one component.** `Water` owns its surface geometry — a
tessellated unit grid (`segments`, 1..512, identical to `builtin:plane` at `segments: 1`, generated
and `Arc`-cached in `engine-core/src/water.rs`) sized by `Transform.scale` — so the entity carries
**no** `Mesh` and **no** `Material` (`water_with_mesh`). Waves are evaluated in **world space**, so
scaling never stretches them and two water entities at the same height form one continuous surface.
`Scene::water_items` returns name-sorted `WaterItem`s and needs no `MeshSource`.

- **Gerstner waves, displaced in the vertex stage** (`shaders/water.wgsl`), with normals from the
  analytic derivatives of the same sum. CPU displacement was never close: a 192² grid is 37k vertices
  and would mint a new `Arc<MeshData>` every frame, defeating M15's geometry cache. `Q` is packed as
  `steepness / (k · A)`, which makes each wave's contribution to the horizontal Jacobian equal to its
  own `steepness` — so **sum of steepness ≤ 1 is exactly the non-folding condition**, enforced as
  `water_waves_self_intersect` with the arithmetic in the message. Dividing `Q` by the wave count (as
  most references do) would make the same file calmer as waves were added.
- **Detail is a slope field with no height behind it**: four golden-angle-rotated sine trains at
  deep-water dispersion speeds, perturbing the normal only. Two numbers in it are load-bearing — the
  base amplitude (`0.010 · wavelength`; the first attempt was ~4× steeper and rendered white noise,
  since the layers are in phase *somewhere* and a slope field is a shaken mirror) and the **fade with
  view distance**, without which sub-pixel ripples alias into sparkle that reads as broken. Nothing
  physical may depend on these normals.
- **The frame gains a pass, but only when there is water.** Absorption and shore foam need the depth
  behind the surface and a pass cannot sample its own depth attachment, so a water scene renders as
  opaque (depth stored) → depth copy (`shaders/depth_resolve.wgsl`, one fullscreen triangle into a
  single-sampled `R32Float`, `textureLoad`, sample 0 under MSAA) → water and transparency →
  particles. `water_present` gates the split: with no water the pass structure, attachments, and
  load/store ops are the exact pre-M18 ones. Water sorts into the **same** back-to-front `Blended`
  list as transparent meshes, because an ice floe in a pond is transparent geometry *inside* a water
  surface and two passes would fix which always draws over the other.
- **The clock.** Water is a pure function of (file, `time`): `--time T` when given, otherwise
  `steps / timestep_hz` (`scene_time` in the CLI); the viewer uses whole fixed steps since load.
  That is what lets water sit under a `diff-render` baseline.
- **`mesh.wgsl` is untouched.** `water.wgsl` duplicates `FrameUniform` and the shadow lookup rather
  than sharing them (the `sky.wgsl` precedent, the M16 reason); only `sky_common.wgsl` is shared. The
  body is lit with the **up** normal while the view-facing normal drives reflection, Fresnel, and
  specular — conflating them made water black from below.

Not here, deliberately: scene reflections (sky and sun only), a CPU wave evaluator and therefore no
buoyancy (`water.rs` is where the Rust mirror goes, with an agreement test, when a boat needs to
float), and point lights on water. Refraction landed in M27 (below).
Fixture `verify/m18_water.json` at `--steps 120`.
