#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! Exactly one member of a set is in force, and an override may lapse on its own.
//!
//! A [`Slot`] names its chosen member rather than flagging members active, so
//! "exactly one" is structural instead of enforced. The alternative — a boolean
//! per member, kept true for one of them — is what a partial unique index buys
//! on a single writer and cannot buy on two: concurrent activations each satisfy
//! the constraint locally and converge to a slot with two winners.
//!
//! [`Slot::merge`] never reads a clock. Expiry belongs to [`Slot::resolve_at`],
//! which is a reader's question, so replicas merging at different moments still
//! converge — and nothing has to write in order for an override to end.

use serde::{Deserialize, Serialize};

/// Longest member identity this primitive will carry.
pub const MAX_MEMBER_CHARS: usize = 128;
/// Longest chooser identity this primitive will carry.
pub const MAX_CHOOSER_CHARS: usize = 128;

/// Why a slot was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(String);

impl Invalid {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Invalid {}

/// One choice: which member, made when, by whom.
///
/// `chooser` is not provenance — the signed transaction already carries that.
/// It is here to break a tie deterministically when two choices share a
/// millisecond, so every replica picks the same winner without asking anyone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub member: String,
    pub chosen_unix_ms: u64,
    pub chooser: String,
}

impl Choice {
    fn validate(&self) -> Result<(), Invalid> {
        if self.member.is_empty() || self.member.chars().count() > MAX_MEMBER_CHARS {
            return Err(Invalid::new("choice member is empty or too long"));
        }
        if self.chooser.is_empty() || self.chooser.chars().count() > MAX_CHOOSER_CHARS {
            return Err(Invalid::new("choice chooser is empty or too long"));
        }
        Ok(())
    }

    /// The total order two replicas agree on: later wins, chooser breaks ties.
    fn rank(&self) -> (u64, &str) {
        (self.chosen_unix_ms, self.chooser.as_str())
    }

    fn later_of(left: Self, right: Self) -> Self {
        if right.rank() > left.rank() {
            right
        } else {
            left
        }
    }
}

/// A choice that stops applying at a stated moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    pub choice: Choice,
    pub until_unix_ms: u64,
}

impl Override {
    fn validate(&self) -> Result<(), Invalid> {
        self.choice.validate()?;
        if self.until_unix_ms <= self.choice.chosen_unix_ms {
            return Err(Invalid::new("override lapses at or before it was chosen"));
        }
        Ok(())
    }
}

/// The standing choice, and an override over it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Choice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<Override>,
}

/// What a slot answers now, and when that answer changes by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub member: Option<String>,
    /// The moment the answer changes with nobody writing, so a consumer can
    /// wake once instead of polling.
    pub next_boundary_unix_ms: Option<u64>,
}

impl Slot {
    pub fn validate(&self) -> Result<(), Invalid> {
        if let Some(base) = &self.base {
            base.validate()?;
        }
        if let Some(over) = &self.over {
            over.validate()?;
        }
        Ok(())
    }

    /// Which member is in force at `now_unix_ms`.
    pub fn resolve_at(&self, now_unix_ms: u64) -> Resolution {
        let live = self
            .over
            .as_ref()
            .filter(|over| now_unix_ms < over.until_unix_ms);
        match live {
            Some(over) => Resolution {
                member: Some(over.choice.member.clone()),
                next_boundary_unix_ms: Some(over.until_unix_ms),
            },
            None => Resolution {
                member: self.base.as_ref().map(|base| base.member.clone()),
                next_boundary_unix_ms: None,
            },
        }
    }

    /// Converge two slots. Commutative, associative, idempotent, and clock-free.
    pub fn merge(left: &Self, right: &Self) -> Self {
        Self {
            base: merge_option(left.base.clone(), right.base.clone(), Choice::later_of),
            over: merge_option(left.over.clone(), right.over.clone(), |a, b| Override {
                // The later choice brings its own lapse: an override *is* its
                // deadline, so taking the newer choice with the older deadline
                // would invent a third override neither replica wrote.
                until_unix_ms: if b.choice.rank() > a.choice.rank() {
                    b.until_unix_ms
                } else {
                    a.until_unix_ms
                },
                choice: Choice::later_of(a.choice, b.choice),
            }),
        }
    }
}

fn merge_option<T>(left: Option<T>, right: Option<T>, both: impl FnOnce(T, T) -> T) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(both(left, right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(member: &str, at: u64, by: &str) -> Choice {
        Choice {
            member: member.into(),
            chosen_unix_ms: at,
            chooser: by.into(),
        }
    }

    fn based(member: &str, at: u64, by: &str) -> Slot {
        Slot {
            base: Some(choice(member, at, by)),
            over: None,
        }
    }

    #[test]
    fn a_slot_names_its_member_so_two_winners_cannot_be_represented() {
        // The defect this primitive exists to prevent, stated as a type: there
        // is one `base`, so a merge of two activations yields one member. A
        // boolean per member could hold two trues and satisfy every local
        // check that produced them.
        let left = based("morning", 10, "dev-a");
        let right = based("evening", 10, "dev-b");
        let merged = Slot::merge(&left, &right);
        assert_eq!(
            merged.resolve_at(50).member.as_deref(),
            Some("evening"),
            "the tie breaks on chooser, and it breaks the same way everywhere"
        );
    }

    #[test]
    fn merge_is_commutative_associative_and_idempotent() {
        let a = based("a", 10, "dev-a");
        let b = based("b", 20, "dev-b");
        let c = Slot {
            base: Some(choice("c", 15, "dev-c")),
            over: Some(Override {
                choice: choice("hold", 30, "dev-a"),
                until_unix_ms: 90,
            }),
        };

        assert_eq!(Slot::merge(&a, &b), Slot::merge(&b, &a));
        assert_eq!(
            Slot::merge(&Slot::merge(&a, &b), &c),
            Slot::merge(&a, &Slot::merge(&b, &c))
        );
        assert_eq!(Slot::merge(&a, &a), a);
        assert_eq!(Slot::merge(&Slot::merge(&a, &b), &b), Slot::merge(&a, &b));
    }

    #[test]
    fn an_override_ends_without_anyone_writing() {
        let slot = Slot {
            base: Some(choice("normal", 10, "dev-a")),
            over: Some(Override {
                choice: choice("alert", 20, "dev-b"),
                until_unix_ms: 100,
            }),
        };
        assert_eq!(slot.resolve_at(99).member.as_deref(), Some("alert"));
        assert_eq!(
            slot.resolve_at(99).next_boundary_unix_ms,
            Some(100),
            "a consumer is told when to wake instead of polling"
        );
        // Same bytes, later clock, different answer. Nothing was written.
        assert_eq!(slot.resolve_at(100).member.as_deref(), Some("normal"));
        assert_eq!(slot.resolve_at(100).next_boundary_unix_ms, None);
    }

    #[test]
    fn merge_reads_no_clock_so_replicas_at_different_moments_agree() {
        // If expiry were applied during merge, a replica merging after the
        // deadline would drop what one merging before it kept, and the two
        // would never converge.
        let lapsed = Slot {
            base: Some(choice("normal", 10, "dev-a")),
            over: Some(Override {
                choice: choice("alert", 20, "dev-b"),
                until_unix_ms: 30,
            }),
        };
        let empty = Slot::default();
        assert_eq!(
            Slot::merge(&lapsed, &empty),
            lapsed,
            "a long-expired override still merges as data"
        );
        assert_eq!(
            Slot::merge(&lapsed, &empty).resolve_at(1_000).member,
            Some("normal".into()),
            "and is still ignored when read"
        );
    }

    #[test]
    fn the_later_override_brings_its_own_deadline() {
        let short = Slot {
            base: None,
            over: Some(Override {
                choice: choice("first", 10, "dev-a"),
                until_unix_ms: 20,
            }),
        };
        let long = Slot {
            base: None,
            over: Some(Override {
                choice: choice("second", 15, "dev-b"),
                until_unix_ms: 500,
            }),
        };
        let merged = Slot::merge(&short, &long);
        let over = merged.over.expect("an override survives");
        assert_eq!(over.choice.member, "second");
        assert_eq!(
            over.until_unix_ms, 500,
            "not a deadline neither replica wrote"
        );
    }

    #[test]
    fn an_empty_slot_is_in_force_as_nothing() {
        let slot = Slot::default();
        assert_eq!(slot.resolve_at(0).member, None);
        assert_eq!(slot.resolve_at(0).next_boundary_unix_ms, None);
        assert!(slot.validate().is_ok());
    }

    #[test]
    fn an_override_that_lapses_before_it_was_chosen_is_refused() {
        let slot = Slot {
            base: None,
            over: Some(Override {
                choice: choice("x", 100, "dev-a"),
                until_unix_ms: 100,
            }),
        };
        assert!(slot.validate().is_err());
    }

    #[test]
    fn identities_are_bounded() {
        let slot = based(&"m".repeat(MAX_MEMBER_CHARS + 1), 1, "dev-a");
        assert!(slot.validate().is_err());
        let slot = based("m", 1, "");
        assert!(slot.validate().is_err());
    }
}
