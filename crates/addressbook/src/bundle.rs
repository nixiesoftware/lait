//! Shareable card-exchange artifact. A suggestion, trusted for nothing.
//!
//! This is not a Book export. Notes that stay, graves, redirects, My Card,
//! and [`Handle::LocalAgent`] never leave the machine.

use serde::{Deserialize, Serialize};

use crate::bounds::{
    MAX_BUNDLE_BYTES, MAX_CARDS_PER_BUNDLE, MAX_HANDLES_PER_CARD, MAX_NAME_BYTES, MAX_NOTE_BYTES,
};
use crate::types::{Card, Handle};
use crate::Error;

/// Version this build writes.
pub const BUNDLE_VERSION: u8 = 1;

/// One Card as it may leave this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedCard {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handles: Vec<String>,
}

/// A versioned bundle of [`SharedCard`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardBundle {
    pub version: u8,
    pub cards: Vec<SharedCard>,
}

impl CardBundle {
    /// Project live Cards into a shareable bundle. Local-agent handles drop.
    pub fn from_cards<'a>(cards: impl IntoIterator<Item = &'a Card>) -> Result<Self, Error> {
        let mut shared = Vec::new();
        for card in cards {
            shared.push(shared_from_card(card)?);
            if shared.len() > MAX_CARDS_PER_BUNDLE {
                return Err(Error::Bound("cards per bundle"));
            }
        }
        Ok(Self {
            version: BUNDLE_VERSION,
            cards: shared,
        })
    }

    /// Wrap already-shareable cards after the same checks export applies.
    pub fn propose(cards: Vec<SharedCard>) -> Result<Self, Error> {
        let bundle = Self {
            version: BUNDLE_VERSION,
            cards,
        };
        bundle.check()?;
        Ok(bundle)
    }

    /// Encode for a local file. Fails before producing an oversize artifact.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.check()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| Error::Invalid("bundle encode"))?;
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(Error::Bound("bundle bytes"));
        }
        Ok(bytes)
    }

    /// Decode a local file. Unknown versions and oversize input fail first.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(Error::Bound("bundle bytes"));
        }
        let bundle: Self =
            serde_json::from_slice(bytes).map_err(|_| Error::Corrupt("bundle decode"))?;
        bundle.check()?;
        Ok(bundle)
    }

    fn check(&self) -> Result<(), Error> {
        if self.version != BUNDLE_VERSION {
            return Err(Error::UnsupportedVersion(self.version));
        }
        if self.cards.len() > MAX_CARDS_PER_BUNDLE {
            return Err(Error::Bound("cards per bundle"));
        }
        for card in &self.cards {
            check_shared(card)?;
        }
        Ok(())
    }
}

fn shared_from_card(card: &Card) -> Result<SharedCard, Error> {
    let mut handles = Vec::new();
    for link in &card.handles {
        if !link.handle.may_leave_device() {
            continue;
        }
        handles.push(link.handle.to_wire());
        if handles.len() > MAX_HANDLES_PER_CARD {
            return Err(Error::Bound("handles per card"));
        }
    }
    let shared = SharedCard {
        name: card.name.value.clone(),
        note: card.note.value.clone(),
        handles,
    };
    check_shared(&shared)?;
    Ok(shared)
}

fn check_shared(card: &SharedCard) -> Result<(), Error> {
    if card.name.is_empty() {
        return Err(Error::Invalid("empty name"));
    }
    if card.name.len() > MAX_NAME_BYTES {
        return Err(Error::Bound("name bytes"));
    }
    if card.note.len() > MAX_NOTE_BYTES {
        return Err(Error::Bound("note bytes"));
    }
    if card.handles.len() > MAX_HANDLES_PER_CARD {
        return Err(Error::Bound("handles per card"));
    }
    for raw in &card.handles {
        let handle = Handle::parse_wire(raw)?;
        if !handle.may_leave_device() {
            return Err(Error::Invalid("local-agent handle"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_encoding::HEXLOWER;
    use mechanics::ids::{ActorId, DeviceId, SpaceId, UlidSource};

    use crate::ids::CardId;
    use crate::ids::PathHash;
    use crate::types::{Evidence, Field, Stamp};

    struct Seq(u64);
    impl UlidSource for Seq {
        fn now_ms(&self) -> u64 {
            self.0
        }
        fn rand80(&self) -> u128 {
            u128::from(self.0)
        }
    }

    fn stamp() -> Stamp {
        Stamp {
            lamport: 1,
            by: DeviceId::from_key_bytes(&[1; 32]),
            at: 1,
        }
    }

    fn card_named(name: &str, handles: Vec<Handle>) -> Card {
        Card {
            id: CardId::mint(&Seq(1)),
            name: Field {
                value: name.into(),
                stamp: stamp(),
            },
            note: Field {
                value: String::new(),
                stamp: stamp(),
            },
            groups: Vec::new(),
            handles: handles
                .into_iter()
                .map(|handle| crate::types::Link {
                    handle,
                    tag: crate::types::Tag {
                        device: DeviceId::from_key_bytes(&[1; 32]),
                        counter: 1,
                    },
                    evidence: Evidence::Declared,
                    added: stamp(),
                    last_seen: None,
                })
                .collect(),
            self_claim: Some(stamp()),
            created: stamp(),
        }
    }

    #[test]
    fn local_agent_handles_never_leave() {
        let agent = Handle::LocalAgent {
            store: PathHash::parse("0123456789abcdef").expect("hash"),
            name: "grok".into(),
        };
        let device = Handle::Device(DeviceId::from_key_bytes(&[3; 32]));
        let card = card_named("Ada", vec![agent, device.clone()]);
        let bundle = CardBundle::from_cards([&card]).expect("export");
        assert_eq!(bundle.cards.len(), 1);
        assert_eq!(bundle.cards[0].handles, vec![device.to_wire()]);
        assert!(
            !bundle
                .encode()
                .expect("encode")
                .windows(5)
                .any(|w| w == b"agent"),
            "a LocalAgent spelling leaked"
        );
    }

    #[test]
    fn my_card_claim_does_not_travel() {
        let card = card_named("Me", Vec::new());
        assert!(card.self_claim.is_some());
        let encoded = CardBundle::from_cards([&card]).unwrap().encode().unwrap();
        let text = String::from_utf8(encoded).unwrap();
        assert!(!text.contains("self"), "{text}");
    }

    #[test]
    fn unknown_version_fails_before_cards_apply() {
        let json = br#"{"version":9,"cards":[{"name":"Ada"}]}"#;
        match CardBundle::decode(json) {
            Err(Error::UnsupportedVersion(9)) => {}
            other => panic!("expected unsupported version, got {other:?}"),
        }
    }

    #[test]
    fn oversize_declared_bytes_fail_before_decode() {
        let too_big = vec![b'{'; MAX_BUNDLE_BYTES.saturating_add(1)];
        match CardBundle::decode(&too_big) {
            Err(Error::Bound("bundle bytes")) => {}
            other => panic!("expected bound, got {other:?}"),
        }
    }

    #[test]
    fn a_local_agent_handle_in_an_import_is_refused() {
        let json =
            br#"{"version":1,"cards":[{"name":"Ada","handles":["agent:0123456789abcdef:grok"]}]}"#;
        match CardBundle::decode(json) {
            Err(Error::Invalid("local-agent handle")) => {}
            other => panic!("expected local-agent refusal, got {other:?}"),
        }
    }

    #[test]
    fn actor_handles_round_trip() {
        let actor = Handle::Actor {
            space: SpaceId::from_digest([5; 16]),
            actor: ActorId::from_incept_hash(&HEXLOWER.encode(&[5; 32])),
        };
        let card = card_named("Ada", vec![actor.clone()]);
        let bytes = CardBundle::from_cards([&card]).unwrap().encode().unwrap();
        let again = CardBundle::decode(&bytes).unwrap();
        assert_eq!(again.cards[0].handles, vec![actor.to_wire()]);
    }

    #[test]
    fn empty_name_is_refused() {
        match CardBundle::decode(br#"{"version":1,"cards":[{"name":""}]}"#) {
            Err(Error::Invalid("empty name")) => {}
            other => panic!("{other:?}"),
        }
    }
}
