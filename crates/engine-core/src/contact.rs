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
///
/// `a` and `b` are **entity** names even when the contact was through a
/// skinned collider proxy (M33): a proxy is not an entity, and a name in a
/// report that no command accepts is exactly what invariant 4 forbids. Which
/// part was hit rides alongside in `a_part`/`b_part`, so every pre-M33 reader
/// of this type is unchanged and a trace line grows a key only when a proxy is
/// actually involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactEvent {
    pub a: String,
    pub b: String,
    /// The proxy part of `a` that was touched, if `a` was touched through one.
    pub a_part: Option<String>,
    pub b_part: Option<String>,
    pub started: bool,
}

/// Separates an owner from its part in a proxy address (`Walker/Head`).
///
/// A slash, deliberately: entity names already contain dots (`Crate.frag0`),
/// and an address must be unmistakably not one.
pub const PART_SEPARATOR: char = '/';

impl ContactEvent {
    /// A contact between two ordinary colliders.
    pub fn new(a: impl Into<String>, b: impl Into<String>, started: bool) -> Self {
        Self {
            a: a.into(),
            b: b.into(),
            a_part: None,
            b_part: None,
            started,
        }
    }

    /// How `a` is addressed: its entity name, or `Entity/Part` when the
    /// contact was through a proxy.
    pub fn address_a(&self) -> String {
        address(&self.a, self.a_part.as_deref())
    }

    /// How `b` is addressed.
    pub fn address_b(&self) -> String {
        address(&self.b, self.b_part.as_deref())
    }
}

/// `Entity`, or `Entity/Part` when there is a part.
pub fn address(entity: &str, part: Option<&str>) -> String {
    match part {
        Some(part) => format!("{entity}{PART_SEPARATOR}{part}"),
        None => entity.to_string(),
    }
}

/// The entity an address belongs to — everything before the separator.
pub fn owner_of(address: &str) -> &str {
    address
        .split_once(PART_SEPARATOR)
        .map_or(address, |(owner, _)| owner)
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
    /// The same two sets at proxy resolution (M33): the pairs as *addresses*,
    /// so `Walker/Head` and `Walker/ShinL` are two answers rather than one.
    /// With no proxies in the scene these hold exactly what the two above do,
    /// which is what makes `touching_parts` safe to reach for anywhere.
    touching_parts: BTreeSet<(String, String)>,
    started_parts: BTreeSet<(String, String)>,
}

impl ContactState {
    /// Fold one step's events in. Call once per physics step, with every
    /// event that step produced (an empty slice still clears `started`).
    pub fn apply(&mut self, events: &[ContactEvent]) {
        self.started.clear();
        self.started_parts.clear();
        for event in events {
            let pair = (event.a.clone(), event.b.clone());
            let addressed = (event.address_a(), event.address_b());
            if event.started {
                self.started.insert(pair.clone());
                self.touching.insert(pair);
                self.started_parts.insert(addressed.clone());
                self.touching_parts.insert(addressed);
            } else {
                self.touching_parts.remove(&addressed);
                // An entity may touch through several parts at once, so the
                // entity-level pair only ends when the last of them does —
                // otherwise a character brushing a wall with two hitboxes
                // would stop "touching" it the moment either one let go.
                if !self
                    .touching_parts
                    .iter()
                    .any(|(a, b)| (owner_of(a), owner_of(b)) == (pair.0.as_str(), pair.1.as_str()))
                {
                    self.touching.remove(&pair);
                }
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

    /// Everything currently in contact with `name`, as **addresses** — so a
    /// bullet in a character's head answers `["Walker/Head"]` where
    /// [`touching`](Self::touching) answers `["Walker"]` (M33).
    ///
    /// `name` may itself be an entity or an address: asking about `"Walker"`
    /// finds contacts on any of its parts, which is the query a script that
    /// wants "was I hit at all" writes.
    pub fn touching_parts(&self, name: &str) -> Vec<String> {
        Self::addressed(&self.touching_parts, name)
    }

    /// The same, restricted to contacts that began on the most recent step.
    pub fn started_parts_with(&self, name: &str) -> Vec<String> {
        Self::addressed(&self.started_parts, name)
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

    fn addressed(pairs: &BTreeSet<(String, String)>, name: &str) -> Vec<String> {
        let matches = |side: &str| side == name || owner_of(side) == name;
        pairs
            .iter()
            .filter_map(|(a, b)| {
                if matches(a) {
                    Some(b.clone())
                } else if matches(b) {
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
        ContactEvent::new(a, b, started)
    }

    fn part_event(a: &str, b: &str, b_part: &str, started: bool) -> ContactEvent {
        ContactEvent {
            b_part: Some(b_part.to_string()),
            ..ContactEvent::new(a, b, started)
        }
    }

    #[test]
    fn a_proxy_contact_reports_the_entity_and_the_part() {
        let mut state = ContactState::default();
        state.apply(&[part_event("Bullet", "Walker", "Head", true)]);

        // The entity-level answer is what every pre-M33 script reads, and it
        // names something `world.set_position` would accept.
        assert_eq!(state.touching("Bullet"), ["Walker"]);
        assert_eq!(state.touching_parts("Bullet"), ["Walker/Head"]);
        // Asking about the owner finds the contact through any of its parts.
        assert_eq!(state.touching_parts("Walker"), ["Bullet"]);
        assert_eq!(state.touching_parts("Walker/Head"), ["Bullet"]);
    }

    #[test]
    fn the_entity_stays_in_contact_until_the_last_part_lets_go() {
        let mut state = ContactState::default();
        state.apply(&[
            part_event("Wall", "Walker", "Head", true),
            part_event("Wall", "Walker", "ShinL", true),
        ]);
        assert_eq!(state.touching("Wall"), ["Walker"]);

        state.apply(&[part_event("Wall", "Walker", "Head", false)]);
        assert_eq!(
            state.touching("Wall"),
            ["Walker"],
            "one hitbox letting go must not end the entity's contact"
        );
        assert_eq!(state.touching_parts("Wall"), ["Walker/ShinL"]);

        state.apply(&[part_event("Wall", "Walker", "ShinL", false)]);
        assert!(state.touching("Wall").is_empty());
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
