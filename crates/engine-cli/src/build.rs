//! `engine build` — cargo with the engine's error convention.
//!
//! Per the design doc §6, its value over plain `cargo build` is error shape
//! only: it runs `cargo build --message-format=json` and re-emits compiler
//! diagnostics as structured engine errors, including rustc's
//! machine-applicable fixes as splice-ready `suggestion` text. If the
//! workspace is broken enough that `engine` itself won't build, plain
//! `cargo build` is the documented fallback — this command is a convenience
//! layer, not the only path.

use std::process::Command;

use engine_core::{codes, EngineError, Result};
use serde_json::Value;

pub fn build(check: bool) -> Result<()> {
    let subcommand = if check { "check" } else { "build" };
    let output = Command::new("cargo")
        .args([subcommand, "--workspace", "--message-format=json"])
        .output()
        .map_err(|e| {
            EngineError::new(codes::CARGO_NOT_FOUND, format!("could not run cargo: {e}"))
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let translated = translate_diagnostics(&stdout);
    for diagnostic in translated.diagnostics {
        diagnostic.emit();
    }

    if output.status.success() {
        println!(
            "{}",
            serde_json::json!({
                "built": true,
                "checked_only": check,
                "warnings": translated.warnings,
            })
        );
        Ok(())
    } else if translated.errors > 0 {
        Err(EngineError::new(
            codes::BUILD_FAILED,
            format!("cargo {subcommand} failed with {} error(s)", translated.errors),
        ))
    } else {
        // cargo failed without emitting a single compiler diagnostic: broken
        // manifest, dependency resolution, an ICE. Its stderr tail is the only
        // explanation there is — carry it rather than a bare "build failed".
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(EngineError::new(
            codes::CARGO_ERROR,
            format!(
                "cargo {subcommand} failed without compiler diagnostics: {}",
                tail_of(&stderr)
            ),
        ))
    }
}

struct Translated {
    diagnostics: Vec<EngineError>,
    errors: u32,
    warnings: u32,
}

/// Re-shape `cargo --message-format=json` output into engine errors. Split
/// from `build` so the translation is testable against captured fixture lines
/// without invoking real cargo.
fn translate_diagnostics(stdout: &str) -> Translated {
    let mut translated = Translated {
        diagnostics: Vec::new(),
        errors: 0,
        warnings: 0,
    };

    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-message" {
            continue;
        }

        let diagnostic = &message["message"];
        let (code, is_warning) = match diagnostic["level"].as_str().unwrap_or("") {
            "error" => (codes::COMPILE_ERROR, false),
            "warning" => (codes::COMPILE_WARNING, true),
            _ => continue,
        };
        if is_warning {
            translated.warnings += 1;
        } else {
            translated.errors += 1;
        }

        let mut error = EngineError::new(
            code,
            diagnostic["message"].as_str().unwrap_or("").to_string(),
        );
        if is_warning {
            error = error.warning();
        }

        // The primary span is where rustc points its caret; that is the
        // file/line an agent should open.
        if let Some(primary) = diagnostic["spans"]
            .as_array()
            .and_then(|spans| spans.iter().find(|s| s["is_primary"] == true))
        {
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

        // rustc's machine-applicable fixes are exactly the splice-ready text
        // the `suggestion` field exists for.
        if let Some(replacement) = machine_applicable_replacement(diagnostic) {
            error = error.suggestion(replacement);
        }

        translated.diagnostics.push(error);
    }

    translated
}

/// The first machine-applicable `suggested_replacement` among the
/// diagnostic's children, if any.
fn machine_applicable_replacement(diagnostic: &Value) -> Option<String> {
    diagnostic["children"].as_array()?.iter().find_map(|child| {
        child["spans"].as_array()?.iter().find_map(|span| {
            if span["suggestion_applicability"] == "MachineApplicable" {
                span["suggested_replacement"].as_str().map(str::to_string)
            } else {
                None
            }
        })
    })
}

/// The last non-empty lines of cargo's stderr — enough to explain a manifest
/// or resolution failure without dumping an entire build log into one field.
fn tail_of(stderr: &str) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured shapes from `cargo build --message-format=json` — testing
    // against fixtures rather than compiling a throwaway crate, because
    // invoking real cargo in tests is slow and network-adjacent.
    const ERROR_WITH_SUGGESTION: &str = r#"{"reason":"compiler-message","message":{"rendered":"...","level":"error","message":"cannot find value `widht` in this scope","spans":[{"file_name":"src/main.rs","line_start":42,"column_start":13,"is_primary":true}],"children":[{"level":"help","message":"a local variable with a similar name exists","spans":[{"file_name":"src/main.rs","line_start":42,"column_start":13,"is_primary":true,"suggested_replacement":"width","suggestion_applicability":"MachineApplicable"}]}]}}"#;
    const PLAIN_WARNING: &str = r#"{"reason":"compiler-message","message":{"rendered":"...","level":"warning","message":"unused variable: `x`","spans":[{"file_name":"src/lib.rs","line_start":7,"column_start":9,"is_primary":true}],"children":[]}}"#;
    const ARTIFACT_NOISE: &str = r#"{"reason":"compiler-artifact","package_id":"whatever"}"#;

    #[test]
    fn translates_an_error_with_its_span_and_suggestion() {
        let input = format!("{ARTIFACT_NOISE}\n{ERROR_WITH_SUGGESTION}\n");
        let translated = translate_diagnostics(&input);
        assert_eq!(translated.errors, 1);
        assert_eq!(translated.warnings, 0);

        let error = &translated.diagnostics[0];
        assert_eq!(error.error, "compile_error");
        let context = error.context().unwrap();
        assert_eq!(context.file.as_deref(), Some("src/main.rs"));
        assert_eq!(context.line, Some(42));
        assert_eq!(
            context.suggestion.as_deref(),
            Some("width"),
            "the machine-applicable fix must ride in `suggestion`"
        );
        assert!(!error.is_warning());
    }

    #[test]
    fn translates_a_warning_as_severity_warning() {
        let translated = translate_diagnostics(PLAIN_WARNING);
        assert_eq!(translated.warnings, 1);
        let warning = &translated.diagnostics[0];
        assert_eq!(warning.error, "compile_warning");
        assert!(warning.is_warning());
        assert!(warning.context().unwrap().suggestion.is_none());
    }

    #[test]
    fn ignores_non_diagnostic_lines_and_garbage() {
        let input = format!("not json at all\n{ARTIFACT_NOISE}\n");
        let translated = translate_diagnostics(&input);
        assert!(translated.diagnostics.is_empty());
    }

    #[test]
    fn tail_keeps_the_end_of_a_long_log() {
        let log: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let tail = tail_of(&log);
        assert!(tail.ends_with("line 99"));
        assert!(!tail.contains("line 0\n"));
    }
}
