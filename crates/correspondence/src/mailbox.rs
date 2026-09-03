//! Where collected letters live once they are opened.
//!
//! The carrier holds sealed bytes; this holds what a device made of them —
//! [`Letter`]s it could open and whose signatures verified. Ingesting is the act
//! of turning a [`Waiting`] into a [`Received`]: open it, verify it, and keep it,
//! or drop it on the floor if it is not for this device or not from who it claims.
//!
//! # Provenance sits beside the content, never inside it
//!
//! Each [`Received`] carries two facts about who sent it, and they are separate on
//! purpose (CORR-9). `letter.from` is proven — the letter's own signature verified
//! against it, end to end, independent of any carrier. `deposited_by` is what the
//! carrier said, corroboration a carrier could have lied about. A surface renders
//! the proven one as the author and may show the other as agreement or
//! disagreement, but never treats the carrier's word as authority.
//!
//! # This is the durable local inbox, not yet the convergent one
//!
//! A `Mailbox` holds what *this* device has collected. Converging it across a
//! person's other devices — so a letter read on a laptop is gone from the phone —
//! is the `message`/`thread`/`mailbox` Body work (CORR-8), the convergent store
//! this is the front of. Its durable projection lives beside the reach registry,
//! keyed by the carrier's deposit id so the same letter collected twice — even
//! across a restart — is one entry.

use std::collections::BTreeMap;

use mechanics::ids::DeviceId;

use crate::{Letter, Waiting};

/// One opened, verified letter, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// The carrier's deposit id, so a re-collection dedups against it.
    pub id: String,
    /// The letter, opened and signature-verified. `letter.from` is the proven
    /// sender.
    pub letter: Letter,
    /// The device the carrier says deposited it. Corroboration, not authority —
    /// see the module docs. Usually equals `letter.from`; when it does not, a
    /// surface should say so rather than pick one.
    pub deposited_by: DeviceId,
    /// When the carrier observed the deposit.
    pub arrived_at: u64,
    /// Exact durable encoding of the signed letter. Private because callers read
    /// the verified `letter`; retaining it here makes projecting durable state
    /// infallible and keeps re-encoding out of every save.
    encoded: Vec<u8>,
}

impl Received {
    /// Whether the carrier's word matches the letter's proof.
    ///
    /// `true` is the ordinary case. `false` is not proof of anything wrong — a
    /// sender may deposit through a device other than the one that composed the
    /// letter — but it is worth surfacing rather than hiding, because it is the
    /// one place the two provenance facts disagree.
    pub fn provenance_agrees(&self) -> bool {
        self.deposited_by == self.letter.from
    }
}

/// The letters one device has collected and opened.
#[derive(Debug, Clone, Default)]
pub struct Mailbox {
    received: BTreeMap<String, Received>,
}

/// What filing one carrier answer changed, and which deposits it proved safe
/// to acknowledge after that changed state is durable.
pub(crate) struct Ingested {
    pub filed: usize,
    pub acknowledge: Vec<String>,
    pub collisions: Vec<String>,
}

impl Mailbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restore the opened inbox from its bounded durable projection.
    ///
    /// Every letter is decoded and its signature re-verified. The disk envelope's
    /// digest proves only that the bytes were written whole, not that a local file
    /// was authored by a correspondent, so invalid material refuses the plane
    /// instead of becoming a trusted transcript row.
    pub(crate) fn restore(
        held: std::collections::BTreeMap<String, addressbook::reach_store::Received>,
    ) -> Result<Self, &'static str> {
        let mut received = BTreeMap::new();
        for (id, stored) in held {
            let letter: Letter =
                serde_json::from_slice(&stored.letter).map_err(|_| "decode opened letter")?;
            if !letter.verifies() {
                return Err("verify opened letter");
            }
            received.insert(
                id.clone(),
                Received {
                    id,
                    letter,
                    deposited_by: stored.deposited_by,
                    arrived_at: stored.arrived_at,
                    encoded: stored.letter,
                },
            );
        }
        Ok(Self { received })
    }

    /// The bounded durable projection written with the reach state.
    pub(crate) fn state(
        &self,
    ) -> std::collections::BTreeMap<String, addressbook::reach_store::Received> {
        self.received
            .iter()
            .map(|(id, received)| {
                (
                    id.clone(),
                    addressbook::reach_store::Received {
                        letter: received.encoded.clone(),
                        deposited_by: received.deposited_by.clone(),
                        arrived_at: received.arrived_at,
                    },
                )
            })
            .collect()
    }

    /// Open and file everything in `waiting` that this device can read and trust.
    ///
    /// Returns how many were newly filed. A letter this device cannot open, or one
    /// whose signature does not verify, is silently dropped — it is not for this
    /// device or not from who it claims, and neither is an error the caller can act
    /// on. A letter already filed (same deposit id) is not counted again, so
    /// ingesting a re-collection is idempotent.
    ///
    /// The seed is an argument rather than a field, so a `Mailbox` holds no key:
    /// it is a store of already-opened letters, and the one moment a key is needed
    /// is here, passing through.
    pub fn ingest(&mut self, seed: &[u8; 32], device: &DeviceId, waiting: &[Waiting]) -> usize {
        self.ingest_for_ack(seed, device, waiting).filed
    }

    /// The collect path's richer filing result.
    ///
    /// An id is safe to acknowledge when its letter was opened and filed now,
    /// or when that id was already in the durable mailbox and the redelivery
    /// opens to the same canonical signed letter with the same carrier
    /// provenance and arrival. Material that cannot be opened or verified, and
    /// a reused id naming different material, is deliberately left at the
    /// carrier: receipt is not evidence that this device holds a trustworthy
    /// durable copy.
    pub(crate) fn ingest_for_ack(
        &mut self,
        seed: &[u8; 32],
        device: &DeviceId,
        waiting: &[Waiting],
    ) -> Ingested {
        let mut filed: usize = 0;
        let mut acknowledge = std::collections::BTreeSet::new();
        let mut collisions = std::collections::BTreeSet::new();
        for item in waiting {
            if let Some(stored) = self.received.get(&item.id) {
                let same = Letter::open(seed, device, &item.sealed)
                    .and_then(|letter| serde_json::to_vec(&letter).ok())
                    .is_some_and(|encoded| {
                        encoded == stored.encoded
                            && item.deposited_by == stored.deposited_by
                            && item.arrived_at == stored.arrived_at
                    });
                if same && !collisions.contains(&item.id) {
                    acknowledge.insert(item.id.clone());
                } else {
                    // A carrier-issued id is not authority to overwrite or drop
                    // different material. One collision poisons that id for the
                    // whole answer, even if another row in it happened to match.
                    acknowledge.remove(&item.id);
                    collisions.insert(item.id.clone());
                }
                continue;
            }
            let Some(letter) = Letter::open(seed, device, &item.sealed) else {
                continue;
            };
            let Ok(encoded) = serde_json::to_vec(&letter) else {
                continue;
            };
            self.received.insert(
                item.id.clone(),
                Received {
                    id: item.id.clone(),
                    letter,
                    deposited_by: item.deposited_by.clone(),
                    arrived_at: item.arrived_at,
                    encoded,
                },
            );
            if !collisions.contains(&item.id) {
                acknowledge.insert(item.id.clone());
            }
            filed = filed.saturating_add(1);
        }
        Ingested {
            filed,
            acknowledge: acknowledge.into_iter().collect(),
            collisions: collisions.into_iter().collect(),
        }
    }

    /// Everything filed, oldest first by when it was written.
    ///
    /// By `sent_at`, not by arrival: what a person reads as the order of a
    /// conversation is when each letter was written, and the carrier's arrival
    /// order can differ under retries and store-and-forward.
    pub fn letters(&self) -> Vec<&Received> {
        let mut out: Vec<&Received> = self.received.values().collect();
        out.sort_by_key(|received| (received.letter.sent_at, received.id.clone()));
        out
    }

    /// How many letters are filed.
    pub fn len(&self) -> usize {
        self.received.len()
    }

    /// Whether the mailbox is empty.
    pub fn is_empty(&self) -> bool {
        self.received.is_empty()
    }

    /// The deposit ids of everything filed, to acknowledge them to the carrier.
    ///
    /// A person who has read their mail tells the carrier so, and the carrier drops
    /// it — otherwise every letter is re-delivered until it expires. What is filed
    /// here is exactly what is safe to acknowledge, because it is what was opened.
    pub fn filed_ids(&self) -> Vec<String> {
        self.received.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Content;
    use mechanics::actor::device_from_seed;

    const NOW: u64 = 1_800_000_000;

    fn waiting(id: &str, letter: &Letter, from: &DeviceId, arrived: u64) -> Waiting {
        // Seal to the recipient the test opens as. The letter names its own
        // recipient by what it was sealed to, not by a field, so the fixture seals
        // to the opener.
        Waiting {
            id: id.to_owned(),
            deposited_by: from.clone(),
            sealed: letter
                .seal_to(&device_from_seed(&[2u8; 32]), NOW + 3600)
                .expect("seal"),
            arrived_at: arrived,
        }
    }

    /// Ingesting opens, verifies, dedups, and orders by when each was written.
    #[test]
    fn a_mailbox_files_what_it_can_open_and_orders_by_send_time() {
        let bob_seed = [2u8; 32];
        let bob = device_from_seed(&bob_seed);
        let alice = device_from_seed(&[1u8; 32]);

        let second = Letter::compose(
            &[1u8; 32],
            Content::Message {
                body: "second".into(),
            },
            200,
        );
        let first = Letter::compose(
            &[1u8; 32],
            Content::Message {
                body: "first".into(),
            },
            100,
        );

        let items = vec![
            waiting("b", &second, &alice, 20),
            waiting("a", &first, &alice, 10),
        ];

        let mut mailbox = Mailbox::new();
        assert_eq!(mailbox.ingest(&bob_seed, &bob, &items), 2);
        assert_eq!(mailbox.len(), 2);

        // Ordered by when they were written, not by arrival or id.
        let letters = mailbox.letters();
        assert!(matches!(
            &letters[0].letter.content,
            Content::Message { body } if body == "first"
        ));
        assert!(matches!(
            &letters[1].letter.content,
            Content::Message { body } if body == "second"
        ));

        // Ingesting the same items again files nothing new.
        assert_eq!(mailbox.ingest(&bob_seed, &bob, &items), 0);
        assert_eq!(mailbox.len(), 2);
    }

    /// A letter this device cannot open is silently dropped, not filed.
    #[test]
    fn a_letter_for_someone_else_is_not_filed() {
        // Sealed to bob, ingested by a stranger.
        let stranger_seed = [9u8; 32];
        let stranger = device_from_seed(&stranger_seed);
        let alice = device_from_seed(&[1u8; 32]);
        let letter = Letter::compose(&[1u8; 32], Content::Message { body: "hi".into() }, NOW);
        let items = vec![waiting("x", &letter, &alice, NOW)];

        let mut mailbox = Mailbox::new();
        assert_eq!(
            mailbox.ingest(&stranger_seed, &stranger, &items),
            0,
            "a device that cannot open a letter must not file it"
        );
        assert!(mailbox.is_empty());
    }

    /// Provenance is two facts, and disagreement is visible rather than resolved.
    #[test]
    fn provenance_carries_both_the_proof_and_the_carriers_word() {
        let bob_seed = [2u8; 32];
        let bob = device_from_seed(&bob_seed);
        let alice = device_from_seed(&[1u8; 32]);
        let letter = Letter::compose(&[1u8; 32], Content::Message { body: "hi".into() }, NOW);

        // Deposited by a device other than the one that signed the letter.
        let odd_courier = device_from_seed(&[5u8; 32]);
        let sealed = letter.seal_to(&bob, NOW + 3600).expect("seal");
        let items = vec![Waiting {
            id: "q".into(),
            deposited_by: odd_courier.clone(),
            sealed,
            arrived_at: NOW,
        }];

        let mut mailbox = Mailbox::new();
        mailbox.ingest(&bob_seed, &bob, &items);
        let received = &mailbox.letters()[0];
        assert_eq!(received.letter.from, alice, "the proven author is alice");
        assert_eq!(
            received.deposited_by, odd_courier,
            "the carrier's word is kept, distinct from the proof"
        );
        assert!(
            !received.provenance_agrees(),
            "the disagreement is visible, not silently resolved to one or the other"
        );
    }

    #[test]
    fn a_reused_deposit_id_for_different_material_is_not_safe_to_acknowledge() {
        let bob_seed = [2u8; 32];
        let bob = device_from_seed(&bob_seed);
        let alice = device_from_seed(&[1u8; 32]);
        let first = Letter::compose(
            &[1u8; 32],
            Content::Message {
                body: "first".into(),
            },
            NOW,
        );
        let different = Letter::compose(
            &[1u8; 32],
            Content::Message {
                body: "different".into(),
            },
            NOW + 1,
        );
        let mut mailbox = Mailbox::new();
        assert_eq!(
            mailbox.ingest(&bob_seed, &bob, &[waiting("same", &first, &alice, NOW)]),
            1
        );

        let outcome =
            mailbox.ingest_for_ack(&bob_seed, &bob, &[waiting("same", &different, &alice, NOW)]);
        assert_eq!(outcome.filed, 0);
        assert!(outcome.acknowledge.is_empty());
        assert_eq!(outcome.collisions, vec!["same"]);
        assert!(matches!(
            &mailbox.letters()[0].letter.content,
            Content::Message { body } if body == "first"
        ));
    }
}
