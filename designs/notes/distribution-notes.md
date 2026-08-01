# Distribution (`designs/distribution-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Distribution.*

*The design doc for this milestone is `designs/distribution-design.md` — it has the rejected
alternatives; this file has what the build learned.*

- **Prebuilt binaries.** `.github/workflows/release.yml` builds `engine` natively on four runners
  (macos-14/13, ubuntu-22.04, windows-2022) on a `v*` tag and uploads tarballs plus `SHA256SUMS`;
  `install.sh` (POSIX sh, `curl | sh`) resolves the latest tag, verifies the checksum, and drops the
  binary in `~/.local/bin`. Linux is built on the *oldest* supported Ubuntu deliberately — the
  artifact's glibc floor is whatever runner built it. `.github/workflows/ci.yml` runs fmt, clippy and
  the workspace tests; the render tests skip there for want of an adapter, so **CI proves the GPU-free
  half only** and baselines stay a local, per-adapter check. Two things had to be true for that skip to
  be real, and neither was: the runner offers a **software GL adapter**, so "no adapter" was never the
  case there — `Gpu::new` now checks the three formats every frame attaches
  (`Rgba8UnormSrgb`, `Depth32Float`, `R32Float`) and refuses an adapter that cannot render one, naming
  it and the capability, because before that check every render died deep inside
  `create_render_pipeline` on `R32Float` and reported itself as `internal_panic` — *an engine bug* —
  which is exactly the wrong diagnosis. And **the pinned car drive is a per-platform artifact** the way
  a baseline is a per-adapter one: eleven thousand steps of a chaotic vehicle sim through glibc's trig
  instead of Apple's park the car ~53 m off, so
  `the_committed_lap_timeline_drives_the_car_around_the_track` skips off aarch64 macOS. Making that one
  cross-platform means routing every trig call in the engine *and* in Rhai through one deterministic
  libm — a milestone, not a fixup. **crates.io is closed to this workspace**:
  `engine-editor` pins egui to a git rev, cargo refuses to publish anything with a git dependency, and
  `engine-cli` depends on the editor — so `publish = false` is a *consequence*, and
  `cargo install --git` is the toolchain path until egui 0.36 lets that pin become a version.
- **`engine init [dir]`** scaffolds a project, because the binary alone is not enough: an agent in an
  empty directory has no way to know the loop is the point. It writes `AGENTS.md` (Codex/Cursor/Amp)
  and `CLAUDE.md` (an `@AGENTS.md` import so there is one source of truth), a starter scene, and a
  script. **The scene sits at the project root, not under `scenes/`** — asset paths resolve relative
  to the *scene file*, so a nested scene reaches its own scripts through `../scripts/`, which is the
  first thing anyone copying the layout gets wrong. It refuses a non-empty directory
  (`init_target_not_empty`, exit 2) unless `--force`. Files are `include_str!`'d, so a `curl | sh`
  install with no checkout carries all of them.
- **`engine agent-guide`** prints that same `AGENTS.md` text — the binary is self-describing, so
  `--help` + `agent-guide` + `list-components` is a complete onboarding with no repo. It is
  **markdown on stdout**, a documented exception beside `--help`/`--version` in
  `docs/cli-contract.md`. A CLI test asserts `init`'s `AGENTS.md` is byte-identical to
  `agent-guide`'s output, so the two cannot drift. The guide is written for someone *using* the
  engine, which is the opposite audience from this file — keeping it accurate is part of adding a
  component, the same way the showcase tour is.
