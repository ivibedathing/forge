//! Line numbers for JSON values.
//!
//! Invariant 6 says every error carries `file` and `line` — "an error an agent
//! cannot locate from the payload alone is a bug." `serde_json::Value` throws
//! spans away during parsing, so validation works on a tree that no longer
//! knows where anything came from. This module recovers that: one pass over
//! the raw source with a minimal JSON lexer, recording the line on which every
//! value starts, keyed by a JSON-Pointer-like path.
//!
//! ```text
//! ""                              the root object
//! "/entities/2"                   third entity
//! "/entities/2/components/0"      its first component
//! "/entities/2/components/0/fov"  a field inside that component
//! ```
//!
//! The lexer is deliberately naive: it assumes the source already parsed as
//! JSON (validation runs `serde_json::from_str` first and bails on syntax
//! errors, which carry their own line from serde). It does not unescape keys,
//! so a key containing `/` or `~` would produce an unaddressable path — no
//! component or scene field has such a name, and the failure mode is a missing
//! line, not a wrong one.

use std::collections::HashMap;

/// Map from value paths to 1-based line numbers.
pub struct LineIndex {
    lines: HashMap<String, u32>,
}

enum Frame {
    /// `expecting_key` distinguishes `"a"` in `{"a": 1}` (a key) from the same
    /// token in `["a"]` (a value).
    Object {
        key: Option<String>,
        expecting_key: bool,
    },
    Array {
        index: usize,
    },
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut lines = HashMap::new();
        let mut stack: Vec<Frame> = Vec::new();
        let mut line: u32 = 1;

        let mut chars = source.char_indices().peekable();

        while let Some((_, c)) = chars.next() {
            match c {
                '\n' => line += 1,
                c if c.is_whitespace() => {}

                '"' => {
                    let mut text = String::new();
                    while let Some((_, c)) = chars.next() {
                        match c {
                            '\\' => {
                                // Skip the escaped character; its exact value
                                // never matters for path construction.
                                text.push(c);
                                if let Some((_, escaped)) = chars.next() {
                                    text.push(escaped);
                                }
                            }
                            '"' => break,
                            c => text.push(c),
                        }
                    }

                    match stack.last_mut() {
                        Some(Frame::Object {
                            key,
                            expecting_key: expecting @ true,
                        }) => {
                            *key = Some(text);
                            *expecting = false;
                        }
                        _ => Self::record(&mut lines, &stack, line),
                    }
                }

                '{' => {
                    Self::record(&mut lines, &stack, line);
                    stack.push(Frame::Object {
                        key: None,
                        expecting_key: true,
                    });
                }
                '[' => {
                    Self::record(&mut lines, &stack, line);
                    stack.push(Frame::Array { index: 0 });
                }
                '}' | ']' => {
                    stack.pop();
                }

                ',' => match stack.last_mut() {
                    Some(Frame::Object { key, expecting_key }) => {
                        *key = None;
                        *expecting_key = true;
                    }
                    Some(Frame::Array { index }) => *index += 1,
                    None => {}
                },

                // `:` separates key from value; the key is already stored.
                ':' => {}

                // Numbers, true/false/null: record at the first character and
                // let subsequent characters fall through the match harmlessly.
                c if c.is_ascii_digit() || c == '-' || c == 't' || c == 'f' || c == 'n' => {
                    Self::record(&mut lines, &stack, line);
                    while let Some(&(_, next)) = chars.peek() {
                        if next.is_ascii_alphanumeric() || next == '.' || next == '+' || next == '-'
                        {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }

                _ => {}
            }
        }

        Self { lines }
    }

    /// The line on which the value at `path` starts, if the path exists.
    pub fn line_of(&self, path: &str) -> Option<u32> {
        self.lines.get(path).copied()
    }

    /// `line_of`, walking up to enclosing values until something is known.
    /// A missing field has no line of its own; its parent object does.
    pub fn line_of_or_parent(&self, path: &str) -> Option<u32> {
        let mut path = path;
        loop {
            if let Some(line) = self.line_of(path) {
                return Some(line);
            }
            path = &path[..path.rfind('/')?];
        }
    }

    fn record(lines: &mut HashMap<String, u32>, stack: &[Frame], line: u32) {
        let mut path = String::new();
        for frame in stack {
            match frame {
                Frame::Object { key: Some(key), .. } => {
                    path.push('/');
                    path.push_str(key);
                }
                // An object frame with no current key means we are at a
                // structural position that cannot hold a value (between
                // entries); nothing to record.
                Frame::Object { key: None, .. } => return,
                Frame::Array { index } => {
                    path.push('/');
                    path.push_str(&index.to_string());
                }
            }
        }
        lines.entry(path).or_insert(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"{
  "name": "demo",
  "entities": [
    {
      "name": "Player",
      "components": [
        { "type": "Camera", "fov": 60.0,
          "active": true }
      ]
    },
    { "name": "Cube1" }
  ]
}"#;

    #[test]
    fn indexes_nested_values_by_path() {
        let index = LineIndex::new(SOURCE);
        assert_eq!(index.line_of(""), Some(1));
        assert_eq!(index.line_of("/name"), Some(2));
        assert_eq!(index.line_of("/entities"), Some(3));
        assert_eq!(index.line_of("/entities/0"), Some(4));
        assert_eq!(index.line_of("/entities/0/name"), Some(5));
        assert_eq!(index.line_of("/entities/0/components/0"), Some(7));
        assert_eq!(index.line_of("/entities/0/components/0/fov"), Some(7));
        assert_eq!(index.line_of("/entities/0/components/0/active"), Some(8));
        assert_eq!(index.line_of("/entities/1"), Some(11));
    }

    #[test]
    fn falls_back_to_the_enclosing_value() {
        let index = LineIndex::new(SOURCE);
        // `/entities/1/components` does not exist; its parent entity does.
        assert_eq!(
            index.line_of_or_parent("/entities/1/components/0"),
            Some(11)
        );
    }

    #[test]
    fn survives_escaped_quotes_and_strings_with_braces() {
        let source = r#"{
  "a": "quote \" and { brace [",
  "b": 2
}"#;
        let index = LineIndex::new(source);
        assert_eq!(index.line_of("/a"), Some(2));
        assert_eq!(index.line_of("/b"), Some(3));
    }

    #[test]
    fn distinguishes_string_values_from_keys_in_arrays() {
        let index = LineIndex::new("{\n  \"tags\": [\"x\",\n    \"y\"]\n}");
        assert_eq!(index.line_of("/tags/0"), Some(2));
        assert_eq!(index.line_of("/tags/1"), Some(3));
    }
}
