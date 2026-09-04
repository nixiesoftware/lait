//! A Space's neighbor-selection as a **declarative, serializable value** —
//! interpreted deterministically, never a hardcoded strategy.
//!
//! The gradient is not a builtin here; it is *one authored composition*
//! ([`gradient`]) of the same primitives anything else is written in. A host or
//! a World declares an [`Architecture`] — a `View`, a `Metric`, and a
//! `Preference` expression — and the interpreter ([`Architecture::realize`])
//! produces exactly that ordering. Declaring it gives you it; there is no
//! measurement, no "see what emerges", on the realization path.
//!
//! ## Why a value, not a trait
//!
//! A pressure-test of an earlier trait-generic sketch found the fatal seam: a
//! `Architecture<Sam, Met, Pref>` is a compile-time type assembly — you cannot
//! serialize it, store it in a Space's config, or name a preference at runtime.
//! So the composition IS data: [`Preference`] is a small AST, [`Metric`] is a
//! declared list of `(dimension, direction)` pairs (the comparator is *derived*
//! from it, not hand-written), and the whole thing round-trips through serde.
//! Deterministic by construction: a pure interpreter over a closed vocabulary.
//!
//! ## What is deliberately NOT here yet
//!
//! `SelfRelative` (keep candidates ranked against THIS node) and
//! `View::GossipSample` are the *emergent* half — a self-relative rule iterated
//! over a bounded gossip view is what makes a gradient self-organize rather than
//! merely sort. The engine has no gossip-sampling loop today, so a self-relative
//! rule over the whole set would degenerate. They are named in the design and
//! omitted from the vocabulary until the substrate exists: global-selection
//! architectures work now; emergent ones are gated on that loop.

use serde::{Deserialize, Serialize};

use crate::gradient::{AnchorUtility, Reach};

/// One comparable field of an [`AnchorUtility`]. A [`Metric`] names these in
/// priority order; the interpreter derives the comparator from that list, so the
/// ordering is *data*, never a copied Rust closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dimension {
    /// Advisory reachability, `Reachable < Unknown < Unreachable` as raw keys.
    Reach,
    /// Whether the candidate advertises news (raw `true = 1`).
    HasNews,
    /// Whether it is in retry backoff (raw `true = 1`).
    InBackoff,
    /// Advertised accepted-transaction count.
    FrontierCount,
    /// Consecutive Contact failures.
    Failures,
    /// Receiver-local last-seen wall clock (ms).
    LastSeen,
}

impl Dimension {
    /// The raw comparable key for this dimension, before direction is applied.
    /// Chosen so that the [`gradient`] metric reproduces `AnchorUtility`'s own
    /// `sort_key` exactly (see the equivalence test).
    fn key(self, u: &AnchorUtility) -> u64 {
        match self {
            Dimension::Reach => match u.reach {
                Reach::Reachable => 0,
                Reach::Unknown => 1,
                Reach::Unreachable => 2,
            },
            Dimension::HasNews => u64::from(u.has_news),
            Dimension::InBackoff => u64::from(u.in_backoff),
            Dimension::FrontierCount => u.frontier_count,
            Dimension::Failures => u64::from(u.failures),
            Dimension::LastSeen => u.last_seen_ms,
        }
    }
}

/// Which way a dimension orders: `Asc` puts the smaller raw key first (better),
/// `Desc` puts the larger first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dir {
    Asc,
    Desc,
}

/// A declared scoring: dimensions in priority order, each with a direction.
/// Lexicographic — the first dimension that separates two candidates decides.
pub type Metric = Vec<(Dimension, Dir)>;

fn cmp_by_metric(metric: &Metric, a: &AnchorUtility, b: &AnchorUtility) -> std::cmp::Ordering {
    for (dim, dir) in metric {
        let (ka, kb) = (dim.key(a), dim.key(b));
        let ord = match dir {
            Dir::Asc => ka.cmp(&kb),
            Dir::Desc => kb.cmp(&ka),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// The local selection rule, as an AST of combinators over a [`Metric`].
/// Serializable, so a Space can carry one in its config and name it at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preference {
    /// Total order by the metric — global selection.
    Rank(Metric),
    /// Truncate the inner ordering to the single best candidate (a hub/star).
    HighestOnly(Box<Preference>),
    /// Truncate the inner ordering to `n` — the degree / fan-out bound.
    TopK(usize, Box<Preference>),
}

impl Preference {
    fn apply<T>(&self, mut items: Vec<(T, AnchorUtility)>) -> Vec<(T, AnchorUtility)> {
        match self {
            // Stable sort — equal-fitness candidates keep input order, matching
            // `gradient::rank`.
            Preference::Rank(metric) => {
                items.sort_by(|(_, a), (_, b)| cmp_by_metric(metric, a, b));
                items
            }
            Preference::HighestOnly(inner) => {
                let mut ordered = inner.apply(items);
                ordered.truncate(1);
                ordered
            }
            Preference::TopK(n, inner) => {
                let mut ordered = inner.apply(items);
                ordered.truncate(*n);
                ordered
            }
        }
    }
}

/// What the rule runs over. `WholeSet` is every candidate the caller passes;
/// `GossipSample` (the emergent substrate) is not built — see the module note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum View {
    WholeSet,
}

/// A declared network architecture: a pipeline of `View → Metric → Preference`,
/// interpreted deterministically. Declared inline on a Space or referenced by
/// name from a registry (a World default, a Space override).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Architecture {
    pub view: View,
    pub preference: Preference,
}

impl Architecture {
    /// Realize the architecture over a candidate set: apply the view, then the
    /// preference. Pure and deterministic. Each candidate carries an opaque
    /// handle `T` (a Station key, a record) through untouched.
    pub fn realize<T>(&self, items: Vec<(T, AnchorUtility)>) -> Vec<(T, AnchorUtility)> {
        let viewed = match self.view {
            // The caller already holds the whole set; the view is the identity.
            View::WholeSet => items,
        };
        self.preference.apply(viewed)
    }
}

/// The gradient, as an authored composition — the reference architecture. Its
/// metric reproduces `AnchorUtility::sort_key` (proven by the equivalence test),
/// so routing selection through this interpreter changes no behavior; it only
/// makes the behavior a declared value.
pub fn gradient() -> Architecture {
    Architecture {
        view: View::WholeSet,
        preference: Preference::Rank(vec![
            (Dimension::Reach, Dir::Asc),
            (Dimension::HasNews, Dir::Desc),
            (Dimension::InBackoff, Dir::Asc),
            (Dimension::FrontierCount, Dir::Desc),
            (Dimension::Failures, Dir::Asc),
            (Dimension::LastSeen, Dir::Desc),
        ]),
    }
}

/// A star: converge on the single most-central node only. Reuses the gradient's
/// own ordering under `HighestOnly` — a preference wrapping a preference.
pub fn star() -> Architecture {
    Architecture {
        view: View::WholeSet,
        preference: Preference::HighestOnly(Box::new(gradient().preference)),
    }
}

/// A bounded-degree gradient: the gradient ordering, kept to the `n` best.
pub fn bounded(n: usize) -> Architecture {
    Architecture {
        view: View::WholeSet,
        preference: Preference::TopK(n, Box::new(gradient().preference)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gradient::{self, Reach};

    fn util(
        reach: Reach,
        has_news: bool,
        count: u64,
        backoff: bool,
        fails: u32,
        seen: u64,
    ) -> AnchorUtility {
        AnchorUtility {
            reach,
            has_news,
            frontier_count: count,
            in_backoff: backoff,
            failures: fails,
            last_seen_ms: seen,
        }
    }

    /// A 10-candidate mixed set that exercises every tie-break tier.
    fn mixed() -> Vec<(u32, AnchorUtility)> {
        vec![
            (0, util(Reach::Unreachable, true, 999, false, 0, 100)),
            (1, util(Reach::Reachable, true, 50, false, 0, 100)),
            (2, util(Reach::Reachable, true, 200, false, 0, 100)),
            (3, util(Reach::Reachable, false, 200, false, 0, 100)),
            (4, util(Reach::Unknown, true, 120, false, 0, 100)),
            (5, util(Reach::Reachable, true, 200, true, 0, 100)),
            (6, util(Reach::Reachable, true, 200, false, 3, 100)),
            (7, util(Reach::Reachable, true, 200, false, 0, 900)),
            (8, util(Reach::Reachable, true, 200, false, 0, 100)),
            (9, util(Reach::Unknown, false, 10, true, 5, 5)),
        ]
    }

    #[test]
    fn composed_gradient_matches_the_hardcoded_rank() {
        // The declared gradient, interpreted, must produce the identical order as
        // the reference `gradient::rank` — the metric is a genuine re-derivation,
        // not a delegation to `cmp_fitness`.
        let items = mixed();
        let declared: Vec<u32> = gradient()
            .realize(items.clone())
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let reference: Vec<u32> = gradient::rank(items)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(declared, reference, "declared gradient == hardcoded rank");
    }

    #[test]
    fn star_keeps_only_the_best() {
        let best = gradient()
            .realize(mixed())
            .into_iter()
            .next()
            .map(|(id, _)| id);
        let star_pick: Vec<u32> = star()
            .realize(mixed())
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(star_pick.len(), 1, "a star selects exactly one");
        assert_eq!(
            star_pick.first().copied(),
            best,
            "and it is the gradient's best"
        );
    }

    #[test]
    fn bounded_truncates_to_degree() {
        let three: Vec<u32> = bounded(3)
            .realize(mixed())
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let full: Vec<u32> = gradient()
            .realize(mixed())
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            three,
            full[..3],
            "bounded(3) is the top three of the gradient"
        );
    }

    #[test]
    fn an_architecture_round_trips_through_serde() {
        // The whole point of "declarative": it is data. A Space can store it.
        for arch in [gradient(), star(), bounded(4)] {
            let json = serde_json::to_string(&arch).expect("serialize");
            let back: Architecture = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(arch, back, "architecture survives a serde round trip");
        }
    }
}
