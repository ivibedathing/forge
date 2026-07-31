//! `engine init` — scaffold a project an agent can start working in, and
//! `engine agent-guide` — the orientation that scaffold hands out.
//!
//! Installing the binary is only half of getting someone started. An agent
//! dropped into an empty directory with `engine` on its `$PATH` has no way to
//! know that the edit → validate → screenshot → *look at the PNG* loop is the
//! point, or that `engine list-components` will hand it the entire scene
//! schema. So the guide is **embedded in the binary** rather than living in
//! the engine's repository: a `curl | sh` install with no checkout still
//! carries it, and `engine init` writes it into the new project as the file
//! the agent's tool already reads.
//!
//! `AGENTS.md` is the one file with content; `CLAUDE.md` points at it. Codex,
//! Cursor and Amp read the former, Claude Code reads the latter, and one
//! source of truth beats two copies that drift.

use std::path::{Path, PathBuf};

use engine_core::{codes, EngineError, Result};

/// The agent-facing orientation, printed by `engine agent-guide` and written
/// as `AGENTS.md` by `engine init`.
pub const AGENT_GUIDE: &str = include_str!("scaffold/AGENTS.md");

/// One file the scaffold writes, at a path relative to the target directory.
struct ScaffoldFile {
    path: &'static str,
    contents: &'static str,
}

/// Everything `engine init` writes.
///
/// `gitignore` is stored without its leading dot: a real `.gitignore` sitting
/// in the engine's own source tree would apply to the engine's own sources.
const FILES: &[ScaffoldFile] = &[
    ScaffoldFile {
        path: "AGENTS.md",
        contents: AGENT_GUIDE,
    },
    ScaffoldFile {
        path: "CLAUDE.md",
        contents: include_str!("scaffold/CLAUDE.md"),
    },
    ScaffoldFile {
        path: ".gitignore",
        contents: include_str!("scaffold/gitignore"),
    },
    // The scene sits at the project root because asset paths — meshes,
    // scripts, animation clips — resolve relative to the *scene file*. A
    // scene one directory down would have to reach its own scripts through
    // `../scripts/`, which is the first thing anyone copying this layout
    // would get wrong.
    ScaffoldFile {
        path: "first.json",
        contents: include_str!("scaffold/first.json"),
    },
    ScaffoldFile {
        path: "scripts/spin.rhai",
        contents: include_str!("scaffold/spin.rhai"),
    },
];

/// Write the scaffold into `dir`, creating it if it does not exist.
///
/// Refuses a directory that already holds anything unless `force`, because the
/// files it writes are exactly the names a project already has — overwriting
/// somebody's `CLAUDE.md` because they ran `engine init` in the wrong place is
/// not a recoverable mistake.
pub fn init(dir: PathBuf, force: bool) -> Result<()> {
    if !force {
        if let Ok(mut entries) = std::fs::read_dir(&dir) {
            if entries.next().is_some() {
                return Err(EngineError::new(
                    codes::INIT_TARGET_NOT_EMPTY,
                    format!(
                        "{} already holds files; scaffold into an empty \
                         directory, or pass --force to write over it",
                        dir.display()
                    ),
                )
                .file(dir.display().to_string()));
            }
        }
    }

    create_dir(&dir)?;
    for file in FILES {
        let target = dir.join(file.path);
        if let Some(parent) = target.parent() {
            create_dir(parent)?;
        }
        std::fs::write(&target, file.contents).map_err(|e| {
            EngineError::new(
                codes::INIT_WRITE_FAILED,
                format!("could not write {}: {e}", target.display()),
            )
            .file(target.display().to_string())
        })?;
    }

    let scene = dir.join("first.json");
    let written: Vec<String> = FILES
        .iter()
        .map(|f| dir.join(f.path).display().to_string())
        .collect();

    let result = serde_json::json!({
        "created": dir.display().to_string(),
        "files": written,
        "next": [
            format!("engine validate {}", scene.display()),
            format!("engine screenshot {} --out /tmp/first.png --steps 120", scene.display()),
            "engine agent-guide",
        ],
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| {
            EngineError::new(
                codes::OUTPUT_SERIALIZATION_FAILED,
                format!("could not serialize the init result: {e}"),
            )
        })?
    );

    Ok(())
}

fn create_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| {
        EngineError::new(
            codes::INIT_WRITE_FAILED,
            format!("could not create {}: {e}", dir.display()),
        )
        .file(dir.display().to_string())
    })
}
