# GUI Editor — Design Document (M7)

Companion to `agent-native-engine-design.md`. That document is the source of truth for the
engine; this one covers only the M7 GUI editor. Where the two conflict, the engine doc wins.

## 1. Vision

A visual editor for people working *alongside* the agent, not instead of it. The scene JSON on
disk remains the single source of truth; the editor is a live, writable *view* onto those files
(engine doc §2.3, invariant #8). A human drags a cube in the viewport, the agent sees a clean
JSON diff. The agent edits the file, the human sees the viewport update. Neither workflow is
privileged and neither can corrupt the other.

Success criterion: with the editor open on `demo_scene.json`, an agent can run
`jq '.entities[0].components[0].position = [0,5,0]'` against the file and the editor reflects
the change within a second — and the reverse: a gizmo drag in the editor produces a JSON edit
that `git diff` renders as a small, reviewable hunk.

## 2. Core Principles

1. **The file is the document.** The editor holds no state that cannot be reconstructed from
   the files on disk. Closing the editor loses nothing; killing it loses at most the edit in
   flight.
2. **Every editor action is a text edit.** Gizmo drags, inspector tweaks, entity
   creation/deletion — each maps to a deterministic mutation of the scene JSON. If an action
   can't be expressed as a file edit, the action doesn't ship.
3. **Write-through, no Save button.** Committed actions (mouse-up on a drag, field blur/enter
   in the inspector) write to disk immediately. A "dirty buffer" that diverges from disk is
   exactly the hidden state invariant #2 forbids. Continuous gestures preview in memory and
   write once on completion.
4. **External edits win instantly.** The editor watches the scene file and reloads on change.
   It must assume another writer (an agent, `jq`, a second editor) exists at all times.
5. **Format-preserving writes.** Editor writes must not churn the file: stable key order,
   stable formatting, no reordering of entities or components the user didn't touch. Diff
   noise is corruption of the agent's medium.
6. **Same validation, same errors.** The editor surfaces `EngineError` JSON from the same
   validation path the CLI uses — file/line/`did_you_mean` and all — never a parallel
   validator with different opinions.
7. **The editor is a client of the engine crates, not a fork.** It renders with
   `engine-render`, loads with `engine-core`/`engine-assets`, and adds no engine capability of
   its own. Anything the editor can do, the CLI + a text editor can do.

## 3. Relationship to the Workspace

New crate, same workspace:

```
crates/
  engine-editor/     # egui app: viewport, hierarchy, inspector, validation panel
```

Launched as `engine edit <scene.json>` — a new subcommand on the existing CLI, consistent with
§6 of the engine doc. The editor depends on `engine-core`, `engine-render`, and
`engine-assets`; nothing depends on the editor. Deleting the crate must leave the engine whole.

## 4. Tech Stack (open — recommendation, not a decision)

The engine doc's §9 pattern applies: this gets settled deliberately, not silently. Options:

- **egui** (recommended) — immediate-mode, Rust-native, first-party wgpu/winit integration
  (`egui-wgpu`, `egui-winit`), so the editor viewport can share the existing renderer and
  window stack. Immediate mode also fits the reload-on-external-change model: there is no
  retained widget tree to reconcile when the file changes under us. Cost: a utilitarian look,
  and `egui-wgpu`/`egui-winit` version-lock against wgpu/winit — **verify at implementation
  time that a released egui line supports wgpu 30 + winit 0.30**; the pinned-versions caveat
  in CLAUDE.md applies doubly here.
- **iced / Slint** — retained-mode Rust UIs; nicer widgets, but reconciling retained state
  with external file edits is exactly the failure mode principle #4 exists to prevent.
- **Tauri + web UI** — richest widget ecosystem, but splits the codebase across two languages
  and puts a webview between the viewport and `engine-render`.

## 5. Editing Model

### Reads
The editor loads the scene through the same `engine-core` path as `engine validate`, keeping
the `lineindex` mapping so validation errors can highlight the offending line. A file watcher
(`notify` crate) triggers reload on external change.

### Writes
Committed actions serialize back to JSON with a canonical, stable formatting (the same
serializer the engine would use to write scenes — one formatter, shared, in `engine-core`, so
CLI convenience commands like `engine edit-entity` and the editor produce byte-identical
output for the same logical change).

Write cycle: read current file → apply the one logical mutation → atomic write
(temp file + rename). Atomic rename keeps a concurrently-reading agent from ever seeing a
half-written scene.

### Conflicts
Last-writer-wins, made safe by keeping writes small and immediate. The dangerous window is
"user mid-gesture while an agent writes the file." Policy: complete the gesture against the
in-memory scene, then re-read the file and apply the gesture's mutation *onto the fresh
contents* (rebase the one edit, entity + component addressed by stable `name`/`type`, not by
index). If the target entity vanished, drop the edit and say so in the status bar. No merge
UI, no lock files — locks are hidden state and would stall the agent.

### Undo/redo
Undo history is a session-local stack of inverse mutations, applied through the same
write-through path — so an undo is just another file edit, visible to the agent like any
other. History does not survive editor restart; durable history is git's job, and the editor
must stay honest about not being a second source of truth for it.

## 6. Coexistence with the Agent

The agent-native twist: most editors assume they are the only writer. This one assumes the
opposite.

- **No import step, no project database, no asset cache with its own lifecycle.** The agent's
  file edits are never "stale" relative to editor state for longer than one watch event.
- **Selection by name.** The hierarchy panel and viewport picking resolve to entity `name`
  (invariant #4), the same handle the agent and CLI use. A rename in the inspector is a rename
  in the file.
- **Live validation panel.** The same all-errors-at-once report `engine validate` produces,
  refreshed on every reload — including errors the *agent* just introduced, making the editor
  a passive monitor for a human supervising agent work.
- **Read-only mode** (`engine edit --watch`) for exactly that supervision case: full viewport
  and inspection, writes disabled.

## 7. UI Surface (v1)

- **Viewport** — the `engine-render` forward renderer with an editor camera (orbit/pan/zoom),
  translate/rotate/scale gizmos, click-to-select. Grid and selection outline are editor-side
  overlays, never scene data.
- **Hierarchy panel** — flat entity list (scene graph parenting lands with the engine, not
  the editor), filterable by name.
- **Inspector** — components of the selected entity, widgets generated from
  `schemas/component-schema.json` rather than hand-built per component, so a new component is
  editable the day it exists (invariant #7 doing double duty).
- **Validation panel** — structured errors, click-to-navigate to entity.
- **Status bar** — file path, last reload time, last external-edit source if detectable, and
  the drop-notice from §5's conflict policy.

Explicitly absent in v1: asset browser, material graph editor, play-in-editor. Run
`engine run-scene` in a terminal.

## 8. Milestones

1. **E0 — Read-only viewer.** `engine edit scene.json` opens a window: viewport + hierarchy +
   inspector, no writes. File watching + live reload working. This alone delivers the
   supervision use case.
2. **E1 — Inspector writes.** Schema-generated inspector edits with write-through and atomic
   writes. The formatter moves into `engine-core` and gets round-trip tests
   (load → save → byte-identical for untouched content).
3. **E2 — Viewport manipulation.** Selection picking, transform gizmos, gesture-then-commit
   writes, the §5 conflict rebase.
4. **E3 — Structure edits.** Add/delete/duplicate entity, add/remove component, rename.
5. **E4 — Undo/redo + polish.** Inverse-mutation undo stack, validation panel navigation,
   read-only flag.

## 9. Open Questions

- **UI toolkit** — egui recommended (§4), pending a version-compatibility check against
  wgpu 30 / winit 0.30.
- **Formatter canonicalization** — exact key order and number formatting for the shared
  serializer; must be settled at E1 and then never changed casually, since every scene file's
  diff stability depends on it.
- **Watch granularity** — reload the whole scene on any change vs. structural diff to
  preserve viewport camera/selection across reloads. Whole-file reload is correct and simple;
  do that first, measure whether it's annoying.
- **Multi-scene / prefab editing** — out of scope until prefabs exist in the engine.

## 10. Non-Goals

- Not a general JSON editor and not a text editor — raw-text editing stays in the user's
  `$EDITOR`.
- No editor-only file formats: no `.editor` sidecars, no layout/preferences files inside the
  project repo (editor prefs live in the user's config dir, since they are not scene truth).
- No play mode, no simulation controls in v1.
- No collaborative/multi-user editing beyond the file-watching coexistence in §6.
