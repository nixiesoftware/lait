//! What this build offers a World, by name (SUB-22).
//!
//! A World declares what it needs as named facts and ranges — `lait.control`
//! at `>=13, <14` — and this is the other side of that conversation: the facts
//! this build actually offers, so [`world_interface::manifest::WorldManifest::unmet`]
//! can answer whether a bundle runs here.
//!
//! ## This replaced a fingerprint, and the reason is worth keeping
//!
//! The first cut derived one opaque token over the control protocol, every
//! World's reviewed implementation, and every DTO schema, then keyed a World's
//! artifacts by it so an incompatible bundle was simply not found. Elegant,
//! and wrong in a way that only shows up in a publisher's life: because the
//! token covered everything, a change touching *nothing a publisher depended
//! on* still moved it, making every published bundle unfetchable and forcing
//! every publisher to republish an unchanged artifact. Named facts with ranges
//! survive exactly the changes that do not concern them.
//!
//! The facts are versions rather than integers so a range can be written over
//! them at all. A protocol at 13 is offered as `13.0.0`, which is what lets a
//! World say `>=13, <14` and mean it.

use std::collections::BTreeMap;

/// The control protocol a World's surface reaches this host through.
pub const CONTROL: &str = "lait.control";

/// The engine version, for a World that needs to name it. Rarely the right
/// thing to depend on — a World tied to an engine version rather than to a
/// surface has found a contract nobody wrote down.
pub const ENGINE: &str = "lait.version";
pub const RUNNER: &str = "lait.world-runner.protocol";
pub const WORLD_ABI: &str = "lait.world-sdk.protocol";

/// Every fact this build offers, by name.
///
/// Cheap and pure: it reads host protocol constants, so callers may build it
/// per check rather than caching it.
pub fn offered() -> BTreeMap<String, semver::Version> {
    let mut facts = BTreeMap::new();
    facts.insert(
        CONTROL.to_string(),
        semver::Version::new(u64::from(crate::control::CONTROL_PROTOCOL_VERSION), 0, 0),
    );
    if let Ok(engine) = semver::Version::parse(env!("LAIT_VERSION_SEMVER")) {
        facts.insert(ENGINE.to_string(), engine);
    }
    facts.insert(
        RUNNER.to_string(),
        semver::Version::new(u64::from(world_runner::PROTOCOL_VERSION), 0, 0),
    );
    facts.insert(
        WORLD_ABI.to_string(),
        semver::Version::new(u64::from(world_sdk::ABI_VERSION), 0, 0),
    );
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_protocol_is_offered_as_a_version_a_range_can_be_written_over() {
        let facts = offered();
        let control = facts.get(CONTROL).expect("the control protocol is offered");
        assert_eq!(
            control.major,
            u64::from(crate::control::CONTROL_PROTOCOL_VERSION),
            "the offered protocol is not the one this build speaks"
        );
        let range =
            semver::VersionReq::parse(&format!(">={}, <{}", control.major, control.major + 1))
                .expect("a range over it parses");
        assert!(range.matches(control));
    }

    /// The property the whole model exists for, asserted against the real
    /// facts rather than a fixture: a World naming one fact is unaffected by
    /// every other.
    #[test]
    fn the_process_protocols_are_named_independently() {
        let facts = offered();
        assert_eq!(
            facts[RUNNER].major,
            u64::from(world_runner::PROTOCOL_VERSION)
        );
        assert_eq!(facts[WORLD_ABI].major, u64::from(world_sdk::ABI_VERSION));
    }

    #[test]
    fn a_world_that_names_a_fact_this_build_does_not_offer_is_told_which_one() {
        let manifest = world_interface::manifest::WorldManifest::parse(
            serde_json::json!({
                "format": 1,
                "id": "world.example.thing",
                "mount": "thing",
                "version": "1.0.0",
                "requires": [{ "name": "lait.nonexistent", "range": ">=1" }],
            })
            .to_string()
            .as_bytes(),
        )
        .expect("a valid manifest");
        let unmet = manifest.unmet(&offered());
        assert_eq!(unmet.len(), 1);
        assert!(
            unmet[0].to_string().contains("lait.nonexistent"),
            "the refusal must name the fact: {}",
            unmet[0]
        );
    }
}
