//! Structured, machine-readable errors.
//!
//! Every error this engine surfaces to the outside world is JSON on stderr with
//! a non-zero exit code. The agent operating this engine parses these; it does
//! not scrape prose. See the design doc, principle 5.
//!
//! ```json
//! {"error": "unknown_component", "entity": "Cube1", "component": "Meterial", "did_you_mean": "Material"}
//! ```
//!
//! This type exists at M0 — before there is much to report — deliberately. The
//! convention is far cheaper to establish now than to retrofit across a
//! codebase that has already grown its own ad-hoc error prose.

use std::fmt;

use serde::Serialize;

/// Where an error happened, and what it was about.
///
/// Split out and boxed so `EngineError` stays small: it is the error type in
/// every `Result` the engine returns, including per-frame render calls, and an
/// inline ~190-byte error variant would be paid on the success path too. Most
/// errors set none of these.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ErrorContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,

    /// Set when the offending name is a near-miss for something known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<String>,

    /// The entities or values an ambiguous choice was between, so an agent can
    /// resolve the ambiguity without re-reading the scene.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
}

/// A structured engine error.
///
/// `error` is a stable snake_case code an agent can match on; everything else
/// is optional context that is omitted from the JSON when absent. Context is
/// flattened into the top-level object, so the wire format is flat regardless
/// of this split.
#[derive(Debug, Clone, Serialize)]
pub struct EngineError {
    /// Stable machine-readable code, e.g. `"unknown_component"`.
    pub error: &'static str,

    /// Human-readable explanation. Never parse this — parse `error`.
    pub message: String,

    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    context: Option<Box<ErrorContext>>,
}

impl EngineError {
    pub fn new(error: &'static str, message: impl Into<String>) -> Self {
        Self {
            error,
            message: message.into(),
            context: None,
        }
    }

    /// Context attached to this error, if any.
    pub fn context(&self) -> Option<&ErrorContext> {
        self.context.as_deref()
    }

    fn context_mut(&mut self) -> &mut ErrorContext {
        self.context.get_or_insert_with(Box::default)
    }

    pub fn file(mut self, file: impl Into<String>) -> Self {
        self.context_mut().file = Some(file.into());
        self
    }

    pub fn line(mut self, line: u32) -> Self {
        self.context_mut().line = Some(line);
        self
    }

    pub fn column(mut self, column: u32) -> Self {
        self.context_mut().column = Some(column);
        self
    }

    pub fn entity(mut self, entity: impl Into<String>) -> Self {
        self.context_mut().entity = Some(entity.into());
        self
    }

    pub fn component(mut self, component: impl Into<String>) -> Self {
        self.context_mut().component = Some(component.into());
        self
    }

    pub fn field(mut self, field: impl Into<String>) -> Self {
        self.context_mut().field = Some(field.into());
        self
    }

    pub fn did_you_mean(mut self, suggestion: impl Into<String>) -> Self {
        self.context_mut().did_you_mean = Some(suggestion.into());
        self
    }

    pub fn candidates<I, S>(mut self, candidates: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.context_mut().candidates = Some(candidates.into_iter().map(Into::into).collect());
        self
    }

    /// Attach the closest match from `candidates`, if one is close enough to be
    /// worth suggesting. Typo-tolerance is a core affordance here: an agent that
    /// writes `Meterial` should be told the answer, not left to guess.
    pub fn suggest_from<'a>(
        self,
        needle: &str,
        candidates: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        match closest_match(needle, candidates) {
            Some(s) => self.did_you_mean(s),
            None => self,
        }
    }

    /// Serialize to a single line of JSON.
    pub fn to_json(&self) -> String {
        // The derive above cannot fail on these field types, but a panic here
        // would replace a real error with a confusing one. Degrade instead.
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                r#"{{"error":"error_serialization_failed","message":"{}"}}"#,
                self.error
            )
        })
    }

    /// Print this error as JSON to stderr.
    pub fn emit(&self) {
        eprintln!("{}", self.to_json());
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json())
    }
}

impl std::error::Error for EngineError {}

pub type Result<T> = std::result::Result<T, EngineError>;

/// Closest candidate to `needle` by Levenshtein distance, if within a
/// similarity threshold that scales with word length.
fn closest_match<'a>(
    needle: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let needle_lower = needle.to_lowercase();

    let (best, distance) = candidates
        .into_iter()
        .map(|c| {
            let d = levenshtein(&needle_lower, &c.to_lowercase());
            (c, d)
        })
        .min_by_key(|(_, d)| *d)?;

    // Allow roughly one edit per four characters, always at least one. Beyond
    // that the "suggestion" is noise and actively misleads.
    let budget = (needle.chars().count() / 4).max(1);
    (distance <= budget).then(|| best.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();

    // Single-row DP: `prev[j]` is the distance for the previous `a` prefix.
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0usize; b_chars.len() + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let substitution = prev[j] + usize::from(ca != cb);
            let deletion = prev[j + 1] + 1;
            let insertion = curr[j] + 1;
            curr[j + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("meterial", "material"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn suggests_near_miss_case_insensitively() {
        let err = EngineError::new("unknown_component", "nope")
            .suggest_from("Meterial", ["Transform", "Mesh", "Material", "Camera"]);
        assert_eq!(
            err.context().unwrap().did_you_mean.as_deref(),
            Some("Material")
        );
    }

    #[test]
    fn declines_to_suggest_when_nothing_is_close() {
        let err = EngineError::new("unknown_component", "nope")
            .suggest_from("Rigidbody", ["Transform", "Mesh", "Camera"]);
        assert!(err.context().is_none_or(|c| c.did_you_mean.is_none()));
    }

    #[test]
    fn stays_small_enough_to_return_by_value() {
        // This type rides in every `Result` the engine returns, including the
        // per-frame render path. Boxing the context is what keeps it cheap; if
        // this grows, box more rather than paying it on every success.
        assert!(
            size_of::<EngineError>() <= 48,
            "EngineError grew to {} bytes",
            size_of::<EngineError>()
        );
    }

    #[test]
    fn omits_absent_context_from_json() {
        let json = EngineError::new("bad_thing", "it broke").to_json();
        assert_eq!(json, r#"{"error":"bad_thing","message":"it broke"}"#);
    }

    #[test]
    fn matches_the_documented_wire_format() {
        let json = EngineError::new("unknown_component", "no such component")
            .entity("Cube1")
            .component("Meterial")
            .did_you_mean("Material")
            .to_json();

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["error"], "unknown_component");
        assert_eq!(value["entity"], "Cube1");
        assert_eq!(value["component"], "Meterial");
        assert_eq!(value["did_you_mean"], "Material");
    }
}
