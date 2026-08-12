//! Storage and transfers.
//!
//! V1 is read-only, and the read half is thinner than it looks: *no engine read
//! answers bytes on disk, object count, or when integrity was last verified*.
//! That is SUB-5, and until it lands this module reports the figures as absent.
//!
//! Absent is the entire point. A synthesised number that makes the surface look
//! populated is the same defect class as rendering a sampling failure as "no
//! peers", and it is harder to spot because it looks like data.

/// What an Orbit is holding.
///
/// Every figure is optional because none of them has a supplier yet. When SUB-5
/// lands, these fill in; until then a surface draws "not measured" and a person
/// is told the truth.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageFacts {
    pub orbit: String,
    pub bytes_on_disk: Option<u64>,
    pub object_count: Option<u64>,
    pub last_verified_ms: Option<u64>,
}

/// A transfer in flight.
///
/// The session's progress lane exists and is plumbed end to end, but nothing in
/// the engine feeds it — the only producer today is the lane's own tests. That
/// is SUB-3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferFacts {
    pub orbit: String,
    pub peer: String,
    pub direction: Direction,
    pub bytes_done: u64,
    /// `None` when the total is genuinely unknown, which is not the same as
    /// zero and must never be drawn as a full bar.
    pub bytes_total: Option<u64>,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
}

impl StorageFacts {
    /// Nothing measured. The honest state today.
    pub fn unmeasured(orbit: impl Into<String>) -> Self {
        Self {
            orbit: orbit.into(),
            ..Self::default()
        }
    }

    /// Whether anything here was actually read from the engine.
    pub fn is_measured(&self) -> bool {
        self.bytes_on_disk.is_some() || self.object_count.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one rule this module exists to hold: unmeasured is not zero.
    #[test]
    fn an_unmeasured_footprint_is_absent_rather_than_zero() {
        let facts = StorageFacts::unmeasured("orb_one");
        assert!(!facts.is_measured());
        assert_eq!(
            facts.bytes_on_disk, None,
            "an unmeasured figure became a number"
        );
        assert_eq!(facts.object_count, None);
        assert_eq!(facts.last_verified_ms, None);
    }

    /// A transfer with no known total must not be drawable as complete.
    #[test]
    fn an_unknown_transfer_total_is_not_zero_and_not_done() {
        let transfer = TransferFacts {
            orbit: "orb_one".into(),
            peer: "peer".into(),
            direction: Direction::Incoming,
            bytes_done: 1_024,
            bytes_total: None,
            state: "transferring".into(),
        };
        assert!(transfer.bytes_total.is_none());
    }
}
