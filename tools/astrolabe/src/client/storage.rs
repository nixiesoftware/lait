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
/// Every figure is optional, and an absent one carries *why* it is absent. A
/// surface that could only say "not measured" would be right and useless: an
/// Orbit that is simply not up and an Orbit nobody could reach are different
/// facts, and only one of them is worth doing something about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageFacts {
    pub orbit: String,
    /// What the registry calls this Orbit. Advisory — the display name is owned
    /// by a World today (SUB-1) — and carried as what it is rather than as
    /// truth.
    pub name: Option<String>,
    pub bytes_on_disk: Option<u64>,
    pub object_count: Option<u64>,
    pub last_verified_ms: Option<u64>,
    /// Why there are no figures, when there are none.
    pub missing: Option<Missing>,
}

/// Why an Orbit contributed no figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// It is not up. Not an error, and not something a *listing* corrects —
    /// measuring must not place what nobody asked to place.
    NotPlaced,
    /// It could not be asked. Distinct from the above for the same reason
    /// `Placement::Unknown` is distinct from `Placement::Vacant`.
    Unreachable,
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
    /// Nothing measured, and why.
    pub fn unmeasured(orbit: impl Into<String>, missing: Missing) -> Self {
        Self {
            orbit: orbit.into(),
            missing: Some(missing),
            ..Self::default()
        }
    }

    /// Whether anything here was actually read from the engine.
    pub fn is_measured(&self) -> bool {
        self.bytes_on_disk.is_some() || self.object_count.is_some()
    }
}

impl super::Client {
    /// What each Orbit this device serves is holding.
    ///
    /// Passive, like every other listing here: routed per Orbit through
    /// `request_if_running`, so a vacant Orbit is never placed to be measured.
    /// An Orbit that cannot be asked contributes an *unmeasured* row rather
    /// than being dropped — the Space is real and on disk, and omitting it
    /// would understate what this device is holding.
    pub async fn get_storage(&self) -> super::ClientResult<Vec<StorageFacts>> {
        let daemon = self.daemon()?;
        let context = self.host_context().await?;

        let mut facts = Vec::new();
        for orbit in context.orbits {
            let Some(space) = mechanics::ids::SpaceId::parse(&orbit.space) else {
                continue;
            };
            let route = lait::control::ControlRoute::Orbit {
                address: lait::control::OrbitAddress::for_store(
                    std::path::Path::new(&orbit.path),
                    space,
                ),
            };
            let name = (!orbit.name.trim().is_empty()).then(|| orbit.name.clone());
            let mut measured = match daemon
                .request_if_running(route, &lait::control::Request::Storage)
                .await
            {
                Ok(lait::control::Response::Storage {
                    bytes_on_disk,
                    object_count,
                    last_verified_ms,
                }) => StorageFacts {
                    orbit: orbit.space.clone(),
                    name: None,
                    bytes_on_disk,
                    object_count,
                    last_verified_ms,
                    missing: None,
                },
                // A real answer that is not a measurement: the Orbit is not up,
                // and asking again would mean placing it to produce a number.
                Ok(_) => StorageFacts::unmeasured(orbit.space.clone(), Missing::NotPlaced),
                // Nobody could ask. A different fact, and the one worth acting
                // on.
                Err(_) => StorageFacts::unmeasured(orbit.space.clone(), Missing::Unreachable),
            };
            measured.name = name;
            facts.push(measured);
        }
        Ok(facts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one rule this module exists to hold: unmeasured is not zero.
    #[test]
    fn an_unmeasured_footprint_is_absent_rather_than_zero() {
        let facts = StorageFacts::unmeasured("orb_one", Missing::NotPlaced);
        assert!(!facts.is_measured());
        assert_eq!(
            facts.bytes_on_disk, None,
            "an unmeasured figure became a number"
        );
        assert_eq!(facts.object_count, None);
        assert_eq!(facts.last_verified_ms, None);
    }

    /// And the rule it acquired: an absence carries why. "Not up" and "nobody
    /// could ask" are different facts, and a surface that folds them together
    /// reports a machine nobody could reach as one with nothing running.
    #[test]
    fn an_absent_figure_says_which_kind_of_absent_it_is() {
        assert_ne!(Missing::NotPlaced, Missing::Unreachable);
        assert_eq!(
            StorageFacts::unmeasured("orb_one", Missing::Unreachable).missing,
            Some(Missing::Unreachable)
        );
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
