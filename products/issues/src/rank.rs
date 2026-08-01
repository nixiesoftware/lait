#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "rank operations validate the ASCII alphabet and rank bounds before positional arithmetic"
)]
//! Fractional index ranks — an order over LWW map records.
//!
//! Milestones live in a catalog *map*, not a list, so their order cannot be the
//! storage order the way a board's is (`catalog.boards[P]` is a movable list and
//! carries its own). A rank on each record gives the map an order without giving
//! it a second structure to keep in sync.
//!
//! Fractional rather than an integer index, because a plain index makes "move M3
//! between M0 and M1" a renumbering of every record after the insertion point —
//! and in a set that converges by last-writer-wins, a write that touches N
//! records to move one is how two people reordering at once lose each other's
//! work. A rank between two ranks is always available, so a move is exactly one
//! record write.
//!
//! Two concurrent moves can land on the same rank. That is not a conflict worth
//! preventing: the reader breaks ties on the milestone id, so both replicas
//! agree on the same order, and the next deliberate move separates them.

/// The rank alphabet, in ASCII order, so a byte comparison *is* the rank
/// comparison and no reader needs this module to sort.
const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

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
pub fn between(lo: &str, hi: Option<&str>) -> String {
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
            return out;
        }
        // No room at this digit. Take the low bound and look one deeper — the
        // result stays above `lo` because it will be strictly longer.
        out.push(DIGITS[low] as char);
        if low < high {
            bounded = false;
        }
        index += 1;
    }
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
        let mid = between(lo, hi);
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
        // The case a float rank loses: halve the same gap over and over. The
        // string grows a character every few rounds instead of running out of
        // precision, so there is no depth at which a move stops being possible.
        let lo = between("", None);
        let mut hi = between(&lo, None);
        for _ in 0..200 {
            hi = assert_between(&lo, Some(&hi));
        }
    }

    #[test]
    fn walks_past_tight_digits() {
        // `"0"` and `"01"` are adjacent at every digit they share, so the result
        // has to go deeper than either. This is the case a naive midpoint loops
        // on forever.
        let mid = assert_between("0", Some("01"));
        assert!(mid.len() > 2);
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
}
