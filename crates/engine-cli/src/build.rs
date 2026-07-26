//! `engine build` — cargo with the engine's error convention.
//!
//! Per the design doc §6, its value over plain `cargo build` is error shape
//! only: it runs `cargo build --message-format=json` and re-emits compiler
//! diagnostics as structured engine errors. If the workspace is broken enough
//! that `engine` itself won't build, plain `cargo build` is the documented
//! fallback — this command is a convenience layer, not the only path.

use std::process::Command;

use engine_core::{EngineError, Result};
use serde_json::Value;

pub fn build() -> Result<()> {
    let output = Command::new("cargo")
        .args(["build", "--workspace", "--message-format=json"])
        .output()
        .map_err(|e| EngineError::new("cargo_not_found", format!("could not run cargo: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut error_count = 0u32;
    let mut warning_count = 0u32;

    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-message" {
            continue;
        }

        let diagnostic = &message["message"];
        let level = diagnostic["level"].as_str().unwrap_or("");
        match level {
            "error" => error_count += 1,
            "warning" => warning_count += 1,
            _ => continue,
        }

        let mut error = EngineError::new(
            if level == "error" {
                "compile_error"
            } else {
                "compile_warning"
            },
            diagnostic["message"].as_str().unwrap_or("").to_string(),
        );

        // The primary span is where rustc points its caret; that is the
        // file/line an agent should open.
        if let Some(spans) = diagnostic["spans"].as_array() {
            if let Some(primary) = spans.iter().find(|s| s["is_primary"] == true) {
                if let Some(file) = primary["file_name"].as_str() {
                    error = error.file(file);
                }
                if let Some(line) = primary["line_start"].as_u64() {
                    error = error.line(line as u32);
                }
                if let Some(column) = primary["column_start"].as_u64() {
                    error = error.column(column as u32);
                }
            }
        }

        error.emit();
    }

    if output.status.success() {
        println!(
            "{}",
            serde_json::json!({
                "built": true,
                "warnings": warning_count,
            })
        );
        Ok(())
    } else {
        Err(EngineError::new(
            "build_failed",
            format!("cargo build failed with {error_count} error(s)"),
        ))
    }
}
