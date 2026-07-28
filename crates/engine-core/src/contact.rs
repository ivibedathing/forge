//! Contact vocabulary shared across crates.
//!
//! `engine-physics` produces [`ContactEvent`]s each step; traces record them,
//! and [`ContactState`] folds them into "who is touching whom" so scripts can
//! ask (`engine-script` reads it, and depends on neither physics nor rapier).
//! Living here keeps the dependency graph a straight line: physics and
//! scripting both see this crate and never each other.

use std::collections::BTreeSet;

/// One contact begin/end between two named entities — what traces record.
/// Names are sorted (`a < b`) so a pair has one spelling everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactEvent {
    pub a: String,
    pub b: String,
    pub started: bool,
}

/// The touching-pairs set, folded from contact events step by step.
///
/// Scripts run *before* physics in the fixed system order, so what they see
/// is the state after the previous physics step: a fresh world has no
/// contacts at step 0, and a hit at physics step N is visible to scripts at
/// step N+1. That one-step latency is the causal order, not a bug.
#[derive(Debug, Default, Clone)]
pub struct ContactState {
    /// Pairs currently in contact, `(a, b)` with `a < b`. BTreeSet keeps
    /// every read deterministic.
    touching: BTreeSet<(String, String)>,
    /// Pairs whose contact began on the most recent step.
    started: BTreeSet<(String, String)>,
}

impl ContactState {
    /// Fold one step's events in. Call once per physics step, with every
    /// event that step produced (an empty slice still clears `started`).
    pub fn apply(&mut self, events: &[ContactEvent]) {
        self.started.clear();
        for event in events {
            let pair = (event.a.clone(), event.b.clone());
            if event.started {
                self.started.insert(pair.clone());
                self.touching.insert(pair);
            } else {
                self.touching.remove(&pair);
            }
        }
    }

    /// Everything currently in contact with `name`, sorted.
    pub fn touching(&self, name: &str) -> Vec<String> {
        Self::partners(&self.touching, name)
    }

    /// Everything whose contact with `name` began on the most recent step,
    /// sorted — the "did I just hit something" query.
    pub fn started_with(&self, name: &str) -> Vec<String> {
        Self::partners(&self.started, name)
    }

    fn partners(pairs: &BTreeSet<(String, String)>, name: &str) -> Vec<String> {
        pairs
            .iter()
            .filter_map(|(a, b)| {
                if a == name {
                    Some(b.clone())
                } else if b == name {
                    Some(a.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(a: &str, b: &str, started: bool) -> ContactEvent {
        ContactEvent {
            a: a.into(),
            b: b.into(),
            started,
        }
    }

    #[test]
    fn contacts_accumulate_and_end() {
        let mut state = ContactState::default();
        state.apply(&[event("Ball", "Ground", true)]);
        assert_eq!(state.touching("Ball"), ["Ground"]);
        assert_eq!(state.started_with("Ball"), ["Ground"]);
        assert_eq!(state.touching("Ground"), ["Ball"]);

        // Next step: nothing new — started clears, touching persists.
        state.apply(&[]);
        assert_eq!(state.touching("Ball"), ["Ground"]);
        assert!(state.started_with("Ball").is_empty());

        state.apply(&[event("Ball", "Ground", false)]);
        assert!(state.touching("Ball").is_empty());
    }

    #[test]
    fn partners_are_sorted_and_scoped_to_the_name() {
        let mut state = ContactState::default();
        state.apply(&[
            event("Ball", "Wall", true),
            event("Ball", "Ground", true),
            event("Crate", "Ground", true),
        ]);
        assert_eq!(state.touching("Ball"), ["Ground", "Wall"]);
        assert_eq!(state.touching("Ground"), ["Ball", "Crate"]);
        assert!(state.touching("Nobody").is_empty());
    }
}
