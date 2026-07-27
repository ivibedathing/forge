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

    /// A JSON Pointer (`/entities/3/components/0/asset`) into the offending
    /// file. `line` is for humans and editors; `path` is for `jq` and
    /// programmatic edits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,

    /// Set when the offending name is a near-miss for something known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<String>,

    /// Concrete replacement *text* (e.g. a rustc machine-applicable fix), as
    /// opposed to `did_you_mean`, which is a known *name*: `did_you_mean`
    /// corrects an identifier, `suggestion` is splice-ready source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,

    /// The entities or values an ambiguous choice was between, so an agent can
    /// resolve the ambiguity without re-reading the scene.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,

    /// Present only as `"warning"`; absence means error. Warnings ride the
    /// same stderr stream and never affect the exit code unless promoted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<&'static str>,
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

    /// Attach the JSON Pointer of the offending value.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.context_mut().path = Some(path.into());
        self
    }

    /// Attach splice-ready replacement text (see [`ErrorContext::suggestion`]).
    pub fn suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.context_mut().suggestion = Some(suggestion.into());
        self
    }

    /// Mark this diagnostic as a warning: reported on the same stream with
    /// `"severity": "warning"`, but not counted against validity.
    pub fn warning(mut self) -> Self {
        self.context_mut().severity = Some("warning");
        self
    }

    /// Whether this diagnostic is a warning rather than an error.
    pub fn is_warning(&self) -> bool {
        self.context().is_some_and(|c| c.severity == Some("warning"))
    }

    /// The process exit code this error's registered class dictates
    /// (see [`crate::codes`]).
    pub fn exit_code(&self) -> i32 {
        crate::codes::exit_code(self.error)
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
pub(crate) fn closest_match<'a>(
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

/// Optimal-string-alignment distance: Levenshtein plus adjacent
/// transposition as a single edit. Transposing two letters ("cubiod",
/// "dynmaic") is the most common typo there is; charging it two edits pushed
/// real mistakes over the suggestion budget. Full matrix rather than rolling
/// rows — suggestion strings are short, and the transposition case needs
/// `d[i-2][j-2]` anyway.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());

    let mut d = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=n {
        d[0][j] = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let substitution = d[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let deletion = d[i - 1][j] + 1;
            let insertion = d[i][j - 1] + 1;
            let mut best = substitution.min(deletion).min(insertion);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }

    d[m][n]
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
        // Adjacent transpositions are one edit (OSA), not two — the typo
        // class the physics enums surfaced.
        assert_eq!(levenshtein("cubiod", "cuboid"), 1);
        assert_eq!(levenshtein("dynmaic", "dynamic"), 1);
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
