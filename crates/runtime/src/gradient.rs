//! The convergence gradient — ranking who to pull from, by measured utility.
//!
//! A Station that wants to advance its Replica frontier must choose a
//! convergence anchor: one of the Neighbors it knows, to dial and pull from.
//! Nothing assigns that role. Each candidate offers some *utility* — is it
//! ahead of me, can I reach it, is it healthy to dial — and the best-utility
//! reachable candidate is the one to pull from *now*. Do this continuously and
//! the topology self-organizes: the freshest, most-reachable, most-stable nodes
//! sink toward the center as the anchors everyone converges through, with no
//! hard-coded "hub" and no tier. A browser tab, a Pi, and a cloud node each land
//! at a gradient position by what they measurably offer, not by what they are.
//!
//! ## This is utility, never authority
//!
//! Ranking answers "who do I pull from," which is a *convergence* choice and
//! never a *legitimacy* one. The bytes a chosen anchor serves are validated on
//! receipt regardless of how it ranks here — a high rank buys a peer no trust,
//! and a low one costs it none. That separation (possession / convergence /
//! legitimacy) is exactly what lets this utility be emergent while authority
//! stays fixed and cryptographic: reordering the gradient can never admit a
//! forged transaction, so the gradient is free to be a pure measurement.
//!
//! ## Unmeasured is absent, not zero
//!
//! A candidate never reached is [`Reach::Unknown`], which ranks *between*
//! reachable and measured-unreachable — a real candidate worth attempting, not
//! one demoted as if a dial had failed. Folding "could not ask" into
//! "unreachable" is the false-disconnection defect; the three-way [`Reach`]
//! keeps them apart.
//!
//! The module is pure — primitive signals in, an ordering out — so it carries
//! no clock, no I/O, and no Neighbor-registry types, and compiles for wasm
//! unchanged. The registry supplies the signals (`neighbors.rs`).

use std::cmp::{Ordering, Reverse};

/// Advisory reachability of a candidate anchor — never standing, only a hint at
/// whether a dial would land. Ordered by desirability as a pull target:
/// [`Reachable`](Reach::Reachable) is best, [`Unknown`](Reach::Unknown) is a
/// real-but-unmeasured candidate, [`Unreachable`](Reach::Unreachable) last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A presence or Contact success made this candidate reachable (advisory).
    Reachable,
    /// Never measured. ABSENT, not zero — rankable, above measured-unreachable.
    Unknown,
    /// A measured failure, or announced dormancy (signed quiescence folds here).
    Unreachable,
}

impl Reach {
    /// Smaller is better: the first key of the fitness ordering.
    fn rank(self) -> u8 {
        match self {
            Reach::Reachable => 0,
            Reach::Unknown => 1,
            Reach::Unreachable => 2,
        }
    }
}

/// The measured utility a candidate anchor offers *this* Station right now — the
/// self-organizing gradient's input vector for one Neighbor. Every field is a
/// measurement or a verified-and-advertised signal; nothing here is authority.
///
/// Built by the caller from what it holds about the Neighbor (the registry does
/// this in `neighbors.rs`), then ordered by [`rank`]. Fields are public so a
/// test — or a caller with signals from another source — can construct one
/// directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorUtility {
    /// Can I reach it? The first practical filter — a pull rides a live dial.
    pub reach: Reach,
    /// Does its advertised frontier differ from mine? Only an anchor with news
    /// can advance my convergence; an anchor level with me has nothing to give
    /// (it stays a valid live peer for other planes, just not a pull target).
    /// This is the registry's `newsworthy` test: the full `(root, count)` pair,
    /// since a root alone is not path-independence-safe.
    pub has_news: bool,
    /// The anchor's advertised accepted-transaction count. Higher means a more
    /// complete view, so one pull converges me further — a tiebreaker among
    /// news-bearing candidates, never a substitute for `has_news`.
    pub frontier_count: u64,
    /// Is it in Contact retry backoff right now? A candidate mid-backoff is
    /// deprioritized so the gradient does not hammer a failing anchor.
    pub in_backoff: bool,
    /// Consecutive Contact failures — retry health. Fewer is better.
    pub failures: u32,
    /// When it was last heard from (receiver-local wall clock). More recent is
    /// better; advisory liveness recency, never standing.
    pub last_seen_ms: u64,
}

impl AnchorUtility {
    /// The fitness sort key, where **smaller is better**, in priority order:
    /// reachability, then has-news, then not-in-backoff, then higher
    /// frontier-count, then fewer failures, then more-recent last-seen.
    ///
    /// Reachability leads because a pull rides a live dial: an anchor you cannot
    /// reach cannot converge you however fresh it is. In practice the caller
    /// feeds this the news-bearing (pending) set, so every candidate already has
    /// news and the ordering turns on reach and health — but the has-news tier
    /// keeps the ordering correct if a mixed set is passed.
    fn sort_key(&self) -> (u8, u8, u8, Reverse<u64>, u32, Reverse<u64>) {
        (
            self.reach.rank(),
            u8::from(!self.has_news),
            u8::from(self.in_backoff),
            Reverse(self.frontier_count),
            self.failures,
            Reverse(self.last_seen_ms),
        )
    }

    /// Compare two candidates by fitness, best-first (a better anchor is
    /// [`Ordering::Less`], so a plain ascending sort puts the best first).
    pub fn cmp_fitness(&self, other: &Self) -> Ordering {
        self.sort_key().cmp(&other.sort_key())
    }

    /// Whether this anchor is worth dialing to converge *right now*: it has news
    /// to give, it is not measured-unreachable, and it is not in backoff.
    /// Unknown reachability is pullable — you attempt it and learn.
    pub fn is_pullable(&self) -> bool {
        self.has_news && self.reach != Reach::Unreachable && !self.in_backoff
    }
}

/// Order candidate anchors best-first by [`AnchorUtility::cmp_fitness`]. Each
/// candidate is paired with an opaque handle `T` (a Station key, a record, an
/// index) the caller carries through untouched.
///
/// The sort is **stable**, so equal-fitness candidates keep input order — feed
/// them in a deterministic order (the registry iterates a key-sorted map) and
/// the ranking is fully deterministic with no tiebreak baked into the utility.
pub fn rank<T>(mut items: Vec<(T, AnchorUtility)>) -> Vec<(T, AnchorUtility)> {
    items.sort_by(|(_, a), (_, b)| a.cmp_fitness(b));
    items
}

/// The single best anchor to pull from now, or `None` if nothing is pullable
/// ([`AnchorUtility::is_pullable`]). A convenience over [`rank`] for the common
/// "give me one anchor" caller.
pub fn best_pullable<T>(items: Vec<(T, AnchorUtility)>) -> Option<(T, AnchorUtility)> {
    rank(items).into_iter().find(|(_, u)| u.is_pullable())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A baseline utility we perturb one field at a time in the ordering tests.
    fn base() -> AnchorUtility {
        AnchorUtility {
            reach: Reach::Reachable,
            has_news: true,
            frontier_count: 100,
            in_backoff: false,
            failures: 0,
            last_seen_ms: 1_000,
        }
    }

    /// Rank a set of labelled utilities and return the labels best-first.
    fn order(items: Vec<(&'static str, AnchorUtility)>) -> Vec<&'static str> {
        rank(items).into_iter().map(|(label, _)| label).collect()
    }

    #[test]
    fn reachable_beats_unknown_beats_unreachable() {
        // Unmeasured is ABSENT, not zero: Unknown sits between the two measured
        // outcomes, never folded down to Unreachable.
        let reachable = AnchorUtility {
            reach: Reach::Reachable,
            ..base()
        };
        let unknown = AnchorUtility {
            reach: Reach::Unknown,
            ..base()
        };
        let unreachable = AnchorUtility {
            reach: Reach::Unreachable,
            ..base()
        };
        assert_eq!(
            order(vec![
                ("unreachable", unreachable),
                ("reachable", reachable),
                ("unknown", unknown),
            ]),
            vec!["reachable", "unknown", "unreachable"],
        );
    }

    #[test]
    fn among_reachable_news_beats_level() {
        // A reachable anchor level with me has nothing to converge, so it ranks
        // below a reachable one that carries news — even though both are live.
        let news = base();
        let level = AnchorUtility {
            has_news: false,
            ..base()
        };
        assert_eq!(
            order(vec![("level", level), ("news", news)]),
            vec!["news", "level"],
        );
    }

    #[test]
    fn reachability_outranks_news() {
        // Reachability leads: a pull rides a live dial, so a reachable-but-level
        // anchor still outranks an unreachable-with-news one in the raw order.
        // (A caller that only wants actionable pulls uses is_pullable, which
        // filters the unreachable one out regardless of rank — see below.)
        let reachable_level = AnchorUtility {
            reach: Reach::Reachable,
            has_news: false,
            ..base()
        };
        let unreachable_news = AnchorUtility {
            reach: Reach::Unreachable,
            has_news: true,
            ..base()
        };
        assert_eq!(
            order(vec![
                ("unreachable_news", unreachable_news),
                ("reachable_level", reachable_level),
            ]),
            vec!["reachable_level", "unreachable_news"],
        );
    }

    #[test]
    fn backoff_sinks_below_a_healthy_peer() {
        let healthy = base();
        let backoff = AnchorUtility {
            in_backoff: true,
            ..base()
        };
        assert_eq!(
            order(vec![("backoff", backoff), ("healthy", healthy)]),
            vec!["healthy", "backoff"],
        );
    }

    #[test]
    fn higher_frontier_count_wins_all_else_equal() {
        let more = AnchorUtility {
            frontier_count: 500,
            ..base()
        };
        let less = AnchorUtility {
            frontier_count: 50,
            ..base()
        };
        assert_eq!(
            order(vec![("less", less), ("more", more)]),
            vec!["more", "less"],
        );
    }

    #[test]
    fn fewer_failures_then_more_recent_last_seen_break_ties() {
        // Same reach/news/backoff/count: fewer failures first.
        let clean = AnchorUtility {
            failures: 0,
            last_seen_ms: 10,
            ..base()
        };
        let flaky = AnchorUtility {
            failures: 3,
            last_seen_ms: 9_999,
            ..base()
        };
        assert_eq!(
            order(vec![("flaky", flaky), ("clean", clean)]),
            vec!["clean", "flaky"],
            "failures dominate last_seen",
        );
        // With failures equal, the more-recently-seen wins.
        let older = AnchorUtility {
            failures: 1,
            last_seen_ms: 100,
            ..base()
        };
        let newer = AnchorUtility {
            failures: 1,
            last_seen_ms: 900,
            ..base()
        };
        assert_eq!(
            order(vec![("older", older), ("newer", newer)]),
            vec!["newer", "older"],
        );
    }

    #[test]
    fn equal_fitness_keeps_input_order() {
        // Two identical utilities: the stable sort preserves input order, so a
        // caller feeding a deterministic order gets a deterministic ranking with
        // no tiebreak baked into the utility itself.
        let a = base();
        let b = base();
        assert_eq!(
            order(vec![("first", a), ("second", b)]),
            vec!["first", "second"],
        );
    }

    #[test]
    fn is_pullable_requires_news_reachable_and_not_backoff() {
        assert!(
            base().is_pullable(),
            "reachable + news + healthy is pullable"
        );
        assert!(
            AnchorUtility {
                reach: Reach::Unknown,
                ..base()
            }
            .is_pullable(),
            "unknown reachability is still worth attempting",
        );
        assert!(
            !AnchorUtility {
                has_news: false,
                ..base()
            }
            .is_pullable(),
            "no news: nothing to converge",
        );
        assert!(
            !AnchorUtility {
                reach: Reach::Unreachable,
                ..base()
            }
            .is_pullable(),
            "measured-unreachable: a dial would not land",
        );
        assert!(
            !AnchorUtility {
                in_backoff: true,
                ..base()
            }
            .is_pullable(),
            "in backoff: do not hammer a failing anchor",
        );
    }

    #[test]
    fn best_pullable_skips_the_top_ranked_when_it_is_not_actionable() {
        // The top of the raw ranking is a reachable anchor with no news (nothing
        // to pull); best_pullable passes over it to the reachable news-bearing
        // one below, and never returns the unreachable news one.
        let reachable_level = AnchorUtility {
            reach: Reach::Reachable,
            has_news: false,
            ..base()
        };
        let reachable_news = AnchorUtility {
            reach: Reach::Reachable,
            has_news: true,
            frontier_count: 10,
            ..base()
        };
        let unreachable_news = AnchorUtility {
            reach: Reach::Unreachable,
            has_news: true,
            frontier_count: 999,
            ..base()
        };
        let chosen = best_pullable(vec![
            ("reachable_level", reachable_level),
            ("unreachable_news", unreachable_news),
            ("reachable_news", reachable_news),
        ]);
        assert_eq!(chosen.map(|(label, _)| label), Some("reachable_news"));
    }

    #[test]
    fn best_pullable_is_none_when_nothing_is_actionable() {
        let level = AnchorUtility {
            has_news: false,
            ..base()
        };
        let unreachable = AnchorUtility {
            reach: Reach::Unreachable,
            ..base()
        };
        assert_eq!(
            best_pullable(vec![("level", level), ("unreachable", unreachable)])
                .map(|(label, _)| label),
            None,
        );
    }
}
