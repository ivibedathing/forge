# Distribution: getting the engine into someone else's hands

## 1. The problem

Everything in this repository is built for an agent operating the engine, and none of it was
built for an agent operating the engine *somewhere else*. Installing meant cloning the workspace
and running `cargo build` — a Rust toolchain, a git checkout of egui, and several minutes before
the first `engine screenshot`. That is a reasonable price for working *on* the engine and an
unreasonable one for working *with* it.

There is a second half, and it is the one that decides whether this works at all. Suppose the
binary is installed. An agent starts in an empty directory with `engine` on its `$PATH`. It has
no way to know that the edit → validate → screenshot → *look at the PNG* loop is the point of the
tool, that `engine list-components` will hand it the entire scene schema, that lights aim down
−Z, or that a scene with one light and no ambient renders mostly black. The repository's own
`CLAUDE.md` answers all of that, and it is the wrong document: it is 500 lines about building the
engine — milestone history, ULP sensitivity, how to bless a baseline — for a reader who wants to
author a scene.

So: **a binary anyone can install, and an orientation that travels with it.**

## 2. Prebuilt binaries

`.github/workflows/release.yml`, on a `v*` tag, builds `engine` on four runners and uploads
tarballs (zip on Windows) plus a `SHA256SUMS` file. `install.sh` is POSIX `sh` — it runs before
anything is installed, on whatever shell the machine has — and resolves the latest tag, verifies
the checksum, and moves the binary into `~/.local/bin`.

Decisions:

- **Native build per runner, no cross-compilation.** One runner per target costs CI minutes and
  saves a cross toolchain, `cross`, and the class of bug where a linker is subtly wrong for a
  platform nobody on the project has.
- **Linux builds on the oldest supported Ubuntu**, not `ubuntu-latest`. The artifact's glibc floor
  is whatever built it, so "latest" quietly narrows who can run the release.
- **`--locked`.** The lockfile pins the egui master rev; a release must build what was tested.
- **Checksum failure is fatal, a missing checksum tool is not.** Refusing to install because the
  machine has no `shasum` helps nobody.

**crates.io is closed to this workspace, and it is worth writing down why so it is not
rediscovered.** `engine-editor` depends on `eframe`/`egui` at a git revision (the released line
pairs with wgpu 29 while this workspace is on wgpu 30), cargo refuses to publish any crate with a
git dependency, and `engine-cli` depends on `engine-editor`. So `publish = false` in the workspace
manifest is a *consequence*, not a policy, and `cargo install engine-cli` cannot work until egui
0.36 ships and that pin becomes a version. Until then the from-source path is
`cargo install --git`, which handles git dependencies fine.

CI (`.github/workflows/ci.yml`) gates on `cargo test --workspace`. The render tests skip cleanly
with no adapter, so **CI proves the GPU-free half only** — validation, physics, the geometry
generators, the formatter, the CLI contract. Baselines are per-adapter artifacts and stay a local
check; there is nothing CI could assert about them that would not be a false failure on somebody's
machine.

`cargo fmt --check` and `cargo clippy` run but are **advisory** (`continue-on-error`), because the
workspace is clean under neither today. Making either blocking means a first commit that reformats
every crate, and a wide mechanical diff is the worst thing to bury ULP-sensitive shader and
geometry code under. They are here so the noise is visible and shrinking; promote them on the
commit that cleans them up.

## 3. `engine init`

Scaffolds a project: `AGENTS.md`, `CLAUDE.md`, `.gitignore`, `first.json`, `scripts/spin.rhai`.

- **Two doc filenames, one document.** Codex, Cursor and Amp read `AGENTS.md`; Claude Code reads
  `CLAUDE.md`. The scaffolded `CLAUDE.md` is a pointer with an `@AGENTS.md` import, so there is
  one source of truth and no pair of copies to drift. It degrades to a sentence telling the reader
  where to look if the import is not honoured.
- **The scene sits at the project root, not under `scenes/`.** Asset paths — meshes, scripts,
  clips — resolve relative to the *scene file*. A scene one directory down reaches its own scripts
  through `../scripts/`, which is exactly the mistake anyone copying the layout would inherit.
  (This was found by running the scaffold, not by reasoning about it: the first version put the
  scene in `scenes/` and failed validation with `asset_not_found`.)
- **A non-empty target is refused** (`init_target_not_empty`, exit 2) unless `--force`. Every name
  it writes is a name a project already has, and silently overwriting somebody's `CLAUDE.md` is
  not a recoverable mistake.
- **Files are `include_str!`'d into the binary**, so a `curl | sh` install with no checkout
  carries all of them.

The starter scene is deliberately not minimal: terrain with two slope-selected layers, a tree, a
sun with shadows, a sky, a script spinning a cube, and a sphere that falls onto the ground under
physics. A cube on a plane would validate and render and teach nothing about what the engine does;
this renders something worth looking at within one command of installing, and every part of it is
a component the reader can go look up.

## 4. `engine agent-guide`

Prints the same `AGENTS.md` text to stdout. Together with `--help` and `list-components`, the
binary is fully self-describing: an agent can onboard with no repository and no network.

It prints **markdown, not JSON** — a documented exception beside `--help` and `--version` in
`docs/cli-contract.md`. Wrapping a 200-line document in a JSON string with every newline escaped
serves no caller. A CLI test asserts that `init`'s `AGENTS.md` is byte-identical to what
`agent-guide` prints, so the two cannot drift.

## 5. What the guide says, and why that list

It is written for the opposite audience from the repository's `CLAUDE.md`: the loop, the
stdout/stderr/exit-code contract, the scene file's anatomy, how to read the schema, and the
conventions that cost time to discover — lights and cameras aim down local −Z, colors are linear
RGB and not sRGB bytes, `rotation[1]` stops being the yaw past ±90°, a scene with one light gets
no ambient, `--steps` advances simulation while `--time` only poses, particles exist only under
`--steps`, and a baseline belongs to the machine that blessed it.

Keeping it accurate is part of adding a component, the same way adding to the showcase tour is.

## 6. Not done

- **No Homebrew tap and no Windows package manager.** Both are cheap once tags exist; neither is
  worth doing before the first release proves the workflow.
- **No Docker image.** Agent sandboxes — Claude Code on the web, Codex cloud — are headless Linux
  containers with no GPU, and `engine screenshot` is precisely the command that has to work there.
  A software-Vulkan image (lavapipe) is the obvious answer, and whether this renderer comes up on
  lavapipe is untested. That is the next thing worth finding out.
- **The release workflow has never run.** It cannot be exercised without pushing a tag, and
  nothing in this repository has ever been built for Windows or Linux. Expect the first tag to
  find something — most likely a missing system library on the Linux runner.
- **No `engine init --template <name>`.** One scaffold is enough until there is evidence of a
  second thing people want to start from.
