//! The artifact that carries reach: how a person hands somebody their address.
//!
//! An [`Announcement`] is what [`Registry::project`](crate::Registry::project)
//! produces plus the genesis that anchors it. It is *evidence, never authority*
//! — [`Registry::absorb`](crate::Registry::absorb) re-derives the profile id
//! from the genesis and refuses anything that does not match, so a mutated or
//! substituted announcement is caught by the reader rather than trusted.
//!
//! It travels by hand today: encoded, base32'd, and pasted into whatever channel
//! two people already share. A directory later carries the same bytes; nothing
//! above this changes when it does.

use serde::{Deserialize, Serialize};

use crate::bounds::MAX_ANNOUNCEMENT_BYTES;
use crate::Error;

/// Version this build writes.
pub const ANNOUNCEMENT_VERSION: u8 = 1;

/// What a profile hands a correspondent so they can reach it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announcement {
    pub version: u8,
    pub profile: mechanics::kinship::ProfileId,
    /// The genesis link the profile id is the hash of. The reader's anchor.
    pub genesis: mechanics::kinship::DeviceLink,
    pub projection: mechanics::kinship::Projection,
}

impl Announcement {
    #[must_use]
    pub fn new(
        profile: mechanics::kinship::ProfileId,
        genesis: mechanics::kinship::DeviceLink,
        projection: mechanics::kinship::Projection,
    ) -> Self {
        Self {
            version: ANNOUNCEMENT_VERSION,
            profile,
            genesis,
            projection,
        }
    }

    /// Encode for carriage. Bounded, because this arrives from a stranger.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let bytes = postcard::to_stdvec(self).map_err(|_| Error::Invalid("announcement encode"))?;
        if bytes.len() > MAX_ANNOUNCEMENT_BYTES {
            return Err(Error::Bound("announcement bytes"));
        }
        Ok(bytes)
    }

    /// Decode one. Refuses before it deserializes, and refuses a version it does
    /// not speak — a decoder that repaired what it did not understand would make
    /// the reader's anchor check the only thing standing between a stranger and
    /// this identity's device set.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_ANNOUNCEMENT_BYTES {
            return Err(Error::Bound("announcement bytes"));
        }
        let announcement: Self =
            postcard::from_bytes(bytes).map_err(|_| Error::Corrupt("announcement decode"))?;
        if announcement.version != ANNOUNCEMENT_VERSION {
            return Err(Error::UnsupportedVersion(announcement.version));
        }
        Ok(announcement)
    }

    /// The spelling a person can paste. Lower-case base32, no padding — the same
    /// alphabet an invite link uses, so one grammar spans both introductions.
    pub fn render(&self) -> Result<String, Error> {
        Ok(data_encoding::BASE32_NOPAD
            .encode(&self.encode()?)
            .to_lowercase())
    }

    /// Parse a pasted spelling. Whitespace is forgiven because a person moved
    /// this by hand; the encoding itself is not.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = data_encoding::BASE32_NOPAD
            .decode(cleaned.to_uppercase().as_bytes())
            .map_err(|_| Error::Corrupt("announcement base32"))?;
        Self::decode(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::kinship::{Audience, DeviceLink, Standing};

    use crate::Registry;

    fn plane(a: [u8; 32], b: [u8; 32]) -> (Registry, mechanics::kinship::ProfileId, DeviceLink) {
        let genesis = DeviceLink::seal(&a, &b, [7u8; 16], 1).expect("genesis");
        let mut registry = Registry::new();
        let profile = registry.found(genesis.clone()).expect("found");
        (registry, profile, genesis)
    }

    fn announcement() -> Announcement {
        let (mut registry, profile, genesis) = plane([21u8; 32], [22u8; 32]);
        let reader = Standing {
            device: Some(mechanics::actor::device_from_seed(&[31u8; 32])),
            ..Standing::default()
        };
        registry
            .avow_reachable(&profile, Audience::Public, &[21u8; 32], 2, [2u8; 16])
            .expect("avow");
        let projection = registry
            .project(&profile, &[21u8; 32], 2, &reader)
            .expect("project");
        Announcement::new(profile, genesis, projection)
    }

    #[test]
    fn an_announcement_survives_the_trip_a_person_carries_it_on() {
        let original = announcement();
        let pasted = format!("  {}\n", original.render().expect("render"));
        assert_eq!(Announcement::parse(&pasted).expect("parse"), original);
    }

    /// A substituted announcement is caught by the **reader**, not by the codec.
    ///
    /// Worth stating as a test because the opposite is the natural assumption:
    /// postcard carries no checksum and a signature is only bytes, so a mutation
    /// frequently still decodes. The codec's job is bounds and version. Integrity
    /// is `absorb`'s — it re-derives the profile id from the genesis and refuses
    /// anything that does not hash to it, which is what stops a stranger swapping
    /// in a device set of their own.
    #[test]
    fn a_substituted_device_set_is_caught_by_the_reader() {
        let honest = announcement();

        // Someone else's genesis, on the profile id the sender expects.
        let (_, _, foreign) = plane([41u8; 32], [42u8; 32]);
        let forged = Announcement::new(honest.profile.clone(), foreign, honest.projection.clone());

        // It encodes and decodes perfectly well. That is the point.
        let bytes = forged.encode().expect("encode");
        let received = Announcement::decode(&bytes).expect("a forgery is well-formed");

        let reader = Standing {
            device: Some(mechanics::actor::device_from_seed(&[31u8; 32])),
            ..Standing::default()
        };
        let mut theirs = Registry::new();
        assert!(
            theirs
                .absorb(received.projection, &received.genesis, &reader)
                .is_err(),
            "a genesis that does not hash to the profile is refused"
        );
        assert!(
            theirs
                .absorb(honest.projection, &honest.genesis, &reader)
                .is_ok(),
            "and the honest one still lands"
        );
    }

    #[test]
    fn a_version_this_build_does_not_speak_is_refused() {
        let mut ahead = announcement();
        ahead.version = ANNOUNCEMENT_VERSION + 1;
        let bytes = postcard::to_stdvec(&ahead).expect("encode");
        assert!(matches!(
            Announcement::decode(&bytes),
            Err(Error::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn an_oversized_announcement_is_refused_before_it_is_parsed() {
        assert!(matches!(
            Announcement::decode(&vec![0u8; MAX_ANNOUNCEMENT_BYTES + 1]),
            Err(Error::Bound("announcement bytes"))
        ));
    }
}
