#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "rank operations validate the ASCII alphabet and rank bounds before positional arithmetic"
)]
//! Variable-path labels inside one bounded order-maintenance block.
//!
//! A flat fractional rank cannot offer bounded labels and bounded update work
//! forever. Boards therefore order stable leaf blocks first, then at most 128
//! exact-head placements inside a leaf. A dense leaf is split; label pressure
//! relabels only that bounded block. Block labels use the same primitive under
//! a separately fenced topology head and bounded maintenance window.
//!
//! These helpers deliberately expose `Option`: adjacency is a signal to the
//! indirection layer to relabel or split, never a user-visible "rank exhausted"
//! refusal. Maintenance overlays name the exact transition or block revision,
//! so concurrent user intent makes stale maintenance inert.
//!
//! Two concurrent moves can land on the same rank. That is not a conflict worth
//! preventing: the reader breaks ties on the milestone id, so both replicas
//! agree on the same order, and the next deliberate move separates them.

/// The rank alphabet, in ASCII order, so a byte comparison *is* the rank
/// comparison and no reader needs this module to sort.
const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// The durable per-level wire ceiling. The block protocol maintains slack well
/// before this bound; reaching it triggers bounded structural maintenance.
pub const MAX_BYTES: usize = 256;

/// Start maintenance well before the wire ceiling. One maintenance window can
/// carry at most [`MAINTENANCE_WINDOW`] exact-head overlays.
pub const SOFT_BYTES: usize = 32;
pub const MAINTENANCE_WINDOW: usize = 128;

/// A canonical durable rank. The minimum digit may appear inside a path but
/// not at its end: `p` and `p0` are adjacent in bytewise lexical order, so
/// accepting the latter would admit a seam with no representable midpoint.
pub fn valid(rank: &str) -> bool {
    !rank.is_empty()
        && rank.len() <= MAX_BYTES
        && rank.bytes().all(|byte| DIGITS.contains(&byte))
        && rank.as_bytes().last().copied() != DIGITS.first().copied()
}

pub fn under_pressure(rank: &str) -> bool {
    rank.len() > SOFT_BYTES
}

fn value(rank: &str, index: usize) -> usize {
    rank.as_bytes()
        .get(index)
        .and_then(|byte| DIGITS.iter().position(|d| d == byte))
        // Past the end of a rank is the bottom of the alphabet, which is what
        // makes `"V"` sort before `"V0"` — the same rule byte comparison uses.
        .unwrap_or(0)
}

/// A rank strictly between `lo` and `hi`.
///
/// `lo` empty means "the start of the list" and `hi` `None` means "the end", so
/// `between("", None)` is the first rank in an empty list. The caller must pass
/// `lo < hi`; they are neighbours in a sorted list, so it always holds.
pub fn try_between(lo: &str, hi: Option<&str>) -> Option<String> {
    if (!lo.is_empty() && !valid(lo))
        || hi.is_some_and(|hi| !valid(hi))
        || hi.is_some_and(|hi| lo >= hi)
    {
        return None;
    }
    let mut out = String::new();
    let mut index = 0;
    // Whether `hi` still bounds us. It stops as soon as we place a digit
    // strictly below `hi`'s at the same position: everything after that is
    // already less than `hi` whatever we append.
    let mut bounded = hi.is_some();
    loop {
        let low = value(lo, index);
        let high = match hi {
            Some(hi) if bounded => value(hi, index),
            // Unbounded above: one past the top of the alphabet, so the midpoint
            // below lands inside it.
            _ => DIGITS.len(),
        };
        if high - low > 1 {
            out.push(DIGITS[low + (high - low) / 2] as char);
            return valid(&out).then_some(out);
        }
        // No room at this digit. Take the low bound and look one deeper — the
        // result stays above `lo` because it will be strictly longer.
        out.push(DIGITS[low] as char);
        if low < high {
            bounded = false;
        }
        index += 1;
        if index >= MAX_BYTES {
            return None;
        }
    }
}

/// Infallible convenience for bounded fixtures and deterministic migration.
/// Normal user-action paths use [`try_between`] and invoke the block protocol
/// on `None` or pressure.
pub fn between(lo: &str, hi: Option<&str>) -> String {
    try_between(lo, hi).expect("canonical ranks have room below the product hard bound")
}

/// Compute `count` ordered labels inside one open interval, placing the first
/// midpoint at the center and recursing on each half. Compared with repeatedly
/// inserting from one edge this grows depth logarithmically for the bounded
/// window. External bounds are retained, so this is safe to publish as exact-
/// head overlays without renumbering the rest of the lane.
pub fn balanced_between(lo: &str, hi: Option<&str>, count: usize) -> Option<Vec<String>> {
    if count > MAINTENANCE_WINDOW {
        return None;
    }
    let mut out = vec![String::new(); count];
    fn fill(out: &mut [String], lo: &str, hi: Option<&str>) -> Option<()> {
        if out.is_empty() {
            return Some(());
        }
        let middle = out.len() / 2;
        let rank = try_between(lo, hi)?;
        out[middle] = rank.clone();
        fill(&mut out[..middle], lo, Some(&rank))?;
        fill(&mut out[middle + 1..], &rank, hi)?;
        Some(())
    }
    fill(&mut out, lo, hi)?;
    Some(out)
}

/// The rank that appends after everything in `ranks` (any order, may be empty).
pub fn after_all(ranks: &[String]) -> String {
    match ranks.iter().max() {
        Some(last) => between(last, None),
        None => between("", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole module exists for: whatever the neighbours, the
    /// result sorts strictly between them by plain byte comparison.
    fn assert_between(lo: &str, hi: Option<&str>) -> String {
        let mid = try_between(lo, hi).expect("room between canonical test ranks");
        assert!(mid.as_str() > lo, "{mid:?} must sort after {lo:?}");
        if let Some(hi) = hi {
            assert!(mid.as_str() < hi, "{mid:?} must sort before {hi:?}");
        }
        mid
    }

    #[test]
    fn first_rank_and_appending() {
        let first = assert_between("", None);
        let second = assert_between(&first, None);
        let third = assert_between(&second, None);
        assert!(first < second && second < third);
    }

    #[test]
    fn inserts_between_neighbours() {
        let a = between("", None);
        let c = between(&a, None);
        assert_between(&a, Some(&c));
    }

    #[test]
    fn inserts_before_the_first() {
        let a = between("", None);
        assert_between("", Some(&a));
    }

    #[test]
    fn survives_repeated_insertion_at_the_same_seam() {
        // The case a float rank loses: halve one gap repeatedly. This primitive
        // reports pressure; the board protocol then relabels/splits the leaf.
        let lo = between("", None);
        let mut hi = between(&lo, None);
        for _ in 0..200 {
            hi = assert_between(&lo, Some(&hi));
        }
        assert!(under_pressure(&hi));
    }

    #[test]
    fn adjacent_or_noncanonical_seam_requests_structural_maintenance() {
        assert!(try_between("0", Some("01")).is_none());
        assert!(try_between("V", Some("V0")).is_none());
    }

    #[test]
    fn appends_after_the_maximum_not_the_last_listed() {
        // `after_all` takes the greatest rank, not the final element, because the
        // caller's vector is a map iteration and carries no order of its own.
        let ranks = vec!["Z".to_string(), "1".to_string(), "V".to_string()];
        let next = after_all(&ranks);
        assert!(ranks.iter().all(|rank| next.as_str() > rank.as_str()));
        assert_eq!(after_all(&[]), between("", None));
    }

    #[test]
    fn rejects_noncanonical_and_exhausted_seams() {
        assert!(!valid("0"));
        assert!(!valid("V0"));
        assert!(try_between("", Some("0")).is_none());
        assert!(try_between("V", Some("V0")).is_none());
        assert!(try_between("z", Some("V")).is_none());
    }

    #[test]
    fn balanced_maintenance_window_is_ordered_and_short() {
        let labels = balanced_between("F", Some("v"), MAINTENANCE_WINDOW)
            .expect("bounded interval has labels");
        assert_eq!(labels.len(), MAINTENANCE_WINDOW);
        assert!(labels
            .iter()
            .all(|rank| valid(rank) && !under_pressure(rank)));
        assert!(labels.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(labels.first().is_some_and(|rank| rank.as_str() > "F"));
        assert!(labels.last().is_some_and(|rank| rank.as_str() < "v"));
    }
}
