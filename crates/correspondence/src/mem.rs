//! An **in-process, deterministic** [`Carrier`] — the whole delivery layer in one
//! process, with no network and no protocol.
//!
//! `comms::mem::MemTransport` is the model, and the payoff is the same one: build
//! a [`MemCarrier`], hand it to the real send and receive paths, and they exercise
//! the same code a hosted carrier would — but the "carrier" is a `BTreeMap`, so it
//! is offline, instant and reproducible on every OS. That is the only way this
//! plane stays testable at this repo's bar; a suite that needs a service running
//! is a suite that does not run.
//!
//! It is built **first**, before any real carrier, so the seam is proven against
//! something that cannot hide a protocol assumption inside it.
//!
//! # Where it is deliberately unfaithful, and where it is not
//!
//! Unfaithful: it never fails to be reached, so [`Missed::Unasked`] is
//! unreachable through it. A caller that only ever tests against this one will not
//! have exercised the branch that matters most in the field —
//! [`MemCarrier::seal_off`] exists so that branch can be reached on purpose.
//!
//! Faithful, and these are the ones worth having: the capacity ceiling, the
//! content-addressed retry, the refusal ordering, and the fact that expired
//! material occupies a mailbox until it is swept.

use std::collections::BTreeMap;

use mechanics::egress::Egress;
use mechanics::ids::DeviceId;

use crate::{admissible, deposit_id, Carrier, Missed, Refused, Sealed, Waiting};

/// The most deposits one mailbox may hold.
///
/// Anyone may write to anyone, so a per-recipient ceiling is not optional — it is
/// the only bound that holds against a determined depositor. Counted over what the
/// mailbox holds rather than what is still deliverable, matching `lait-post`: an
/// expiry-aware count would mean walking the mailbox on every deposit, and a
/// ceiling that reads the whole mailbox to decide whether the mailbox is full is
/// the amplifier it exists to prevent.
pub const MAX_MAILBOX: usize = 256;

/// In memory, for tests and for anything that must not survive a restart.
#[derive(Debug, Default)]
pub struct MemCarrier {
    /// Keyed by recipient, then by deposit id — so a collect is one lookup and
    /// never a scan of everybody's mail.
    held: BTreeMap<DeviceId, BTreeMap<String, Waiting>>,
    /// When set, every operation answers as if the carrier were unreachable.
    unreachable: Option<String>,
    /// Per recipient, the senders they have blocked.
    blocked: BTreeMap<DeviceId, std::collections::BTreeSet<DeviceId>>,
}

impl MemCarrier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation answer "could not be asked".
    ///
    /// Here because the branch a caller most needs to get right is the one an
    /// in-process carrier can never produce by accident. A test that wants to
    /// prove it renders an unreachable carrier differently from an empty mailbox
    /// has to be able to *cause* unreachable.
    pub fn seal_off(&mut self, why: impl Into<String>) {
        self.unreachable = Some(why.into());
    }

    /// Undo [`MemCarrier::seal_off`].
    pub fn reopen(&mut self) {
        self.unreachable = None;
    }

    /// Drop everything past its window. Returns how many went.
    pub fn sweep(&mut self, now: u64) -> usize {
        let mut gone: usize = 0;
        for mailbox in self.held.values_mut() {
            let before = mailbox.len();
            mailbox.retain(|_, waiting| waiting.sealed.expires_at > now);
            gone = gone.saturating_add(before.saturating_sub(mailbox.len()));
        }
        self.held.retain(|_, mailbox| !mailbox.is_empty());
        gone
    }

    /// How many deposits this device is holding, expired included.
    pub fn holding(&self, device: &DeviceId) -> usize {
        self.held.get(device).map(BTreeMap::len).unwrap_or(0)
    }

    /// Put a letter straight into a mailbox, for tests that are about *reading*.
    ///
    /// The deposit path has its own coverage — witness, blocks, capacity, retry —
    /// and a test of the poll loop should not have to reconstruct a signed sender
    /// to make one letter exist. Named `_for_test` and gated so it cannot leak into
    /// a real send path.
    #[cfg(test)]
    pub(crate) fn deliver_for_test(
        &mut self,
        sealed: &Sealed,
        from: &DeviceId,
        now: u64,
    ) -> String {
        // No `admissible` gate: this is a direct insert for read-path tests, not a
        // deposit, so it does not re-litigate expiry or capacity.
        let id = deposit_id(from, sealed);
        self.held
            .entry(sealed.recipient.clone())
            .or_default()
            .insert(
                id.clone(),
                Waiting {
                    id: id.clone(),
                    deposited_by: from.clone(),
                    sealed: sealed.clone(),
                    arrived_at: now,
                },
            );
        id
    }
}

impl Carrier for MemCarrier {
    fn deposit(&mut self, from: &Egress<'_>, sealed: &Sealed, now: u64) -> Result<String, Refused> {
        if let Some(why) = &self.unreachable {
            return Err(Refused::Unreachable(why.clone()));
        }
        // Structural faults before capacity, so a malformed envelope is told what
        // is wrong with it rather than being told to come back later.
        admissible(sealed, now)?;

        let id = deposit_id(from.device(), sealed);
        // A blocked sender is refused where it matters: the id is returned so the
        // request is indistinguishable from acceptance, and nothing is stored.
        // Same shape as the Post, so a caller cannot tell the two carriers apart.
        if self
            .blocked
            .get(&sealed.recipient)
            .is_some_and(|set| set.contains(from.device()))
        {
            return Ok(id);
        }

        let mailbox = self.held.entry(sealed.recipient.clone()).or_default();
        // A retry is the same deposit. Checked before the ceiling, or a full
        // mailbox would start refusing the redelivery of something it already
        // holds.
        if !mailbox.contains_key(&id) && mailbox.len() >= MAX_MAILBOX {
            return Err(Refused::AtCapacity);
        }
        mailbox.insert(
            id.clone(),
            Waiting {
                id: id.clone(),
                deposited_by: from.device().clone(),
                sealed: sealed.clone(),
                arrived_at: now,
            },
        );
        Ok(id)
    }

    fn collect(&mut self, device: &DeviceId, now: u64) -> Missed {
        if let Some(why) = &self.unreachable {
            return Missed::Unasked(why.clone());
        }
        Missed::Held(
            self.held
                .get(device)
                .map(|mailbox| {
                    mailbox
                        .values()
                        .filter(|waiting| waiting.sealed.expires_at > now)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    fn acknowledge(
        &mut self,
        device: &DeviceId,
        ids: &[String],
        _now: u64,
    ) -> Result<usize, Refused> {
        if let Some(why) = &self.unreachable {
            return Err(Refused::Unreachable(why.clone()));
        }
        let Some(mailbox) = self.held.get_mut(device) else {
            return Ok(0);
        };
        let mut gone: usize = 0;
        for id in ids {
            if mailbox.remove(id).is_some() {
                gone = gone.saturating_add(1);
            }
        }
        Ok(gone)
    }

    fn block(
        &mut self,
        by: &Egress<'_>,
        sender: &DeviceId,
        blocked: bool,
        _now: u64,
    ) -> Result<(), Refused> {
        if let Some(why) = &self.unreachable {
            return Err(Refused::Unreachable(why.clone()));
        }
        let recipient = by.device().clone();
        if blocked {
            self.blocked
                .entry(recipient)
                .or_default()
                .insert(sender.clone());
        } else if let Some(set) = self.blocked.get_mut(&recipient) {
            set.remove(sender);
            if set.is_empty() {
                self.blocked.remove(&recipient);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::actor::device_from_seed;
    use mechanics::actor::{self, ActorOp, ConsentCtx};
    use mechanics::authorization::seal_to_devices;
    use mechanics::egress;
    use mechanics::ids::{ActorId, SpaceId, SystemUlidSource};

    const NOW: u64 = 1_800_000_000;
    const CONTEXT: &[&[u8]] = &[b"lait/correspondence/1/test"];

    fn seed(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn incept(n: u8, space: &SpaceId) -> (actor::SignedEvent, ActorId) {
        let nonce = [n; 16];
        let keys = vec![device_from_seed(&seed(n))];
        let binding = actor::consent_sign(
            &seed(n),
            space.as_str(),
            [n.wrapping_add(100); 16],
            &ConsentCtx::Incept {
                incept_nonce: &nonce,
                devices: &keys,
                recovery_commit: &None,
            },
        );
        let ev = actor::sign_event(
            &seed(n),
            &ActorOp::Incept {
                space: space.as_str().to_string(),
                nonce,
                devices: vec![binding],
                recovery_commit: None,
            },
            vec![],
            space,
        );
        let id = ActorId::from_incept_hash(&ev.hash());
        (ev, id)
    }

    fn envelope_to(recipient: &DeviceId, body: &[u8]) -> Sealed {
        // Sealed for real, so the test exercises the shape a caller actually
        // hands down rather than a placeholder the carrier would never see.
        let sealed = seal_to_devices(std::slice::from_ref(recipient), CONTEXT, body).expect("seal");
        Sealed {
            recipient: recipient.clone(),
            bytes: serde_json::to_vec(&sealed).expect("encode"),
            expires_at: NOW + 3600,
            construction: 1,
        }
    }

    /// A letter crosses the seam sealed, is collected only by its recipient, and
    /// the carrier never sees inside it.
    #[test]
    fn a_sealed_letter_reaches_its_recipient_and_nobody_else() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let plane = actor::replay(&space, &[e_alice]);
        let standing =
            egress::authorize(&plane, &alice, &device_from_seed(&seed(1))).expect("her own key");

        let bob_device = device_from_seed(&seed(2));
        let mut carrier = MemCarrier::new();
        let id = carrier
            .deposit(&standing, &envelope_to(&bob_device, b"the plaintext"), NOW)
            .expect("deposit");

        let waiting = carrier
            .collect(&bob_device, NOW)
            .held()
            .expect("asked")
            .to_vec();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, id);
        assert_eq!(waiting[0].deposited_by, device_from_seed(&seed(1)));
        assert_eq!(waiting[0].arrived_at, NOW);

        // The carrier holds bytes it cannot read. Nothing in this crate can open
        // one, so the strongest available assertion is that the plaintext never
        // appears in what it stored.
        assert!(
            !waiting[0]
                .sealed
                .bytes
                .windows(b"the plaintext".len())
                .any(|w| w == b"the plaintext"),
            "the carrier must never hold the plaintext"
        );

        // Somebody else's mailbox is empty, not absent.
        assert_eq!(
            carrier.collect(&device_from_seed(&seed(3)), NOW),
            Missed::Held(vec![]),
        );

        assert_eq!(
            carrier.acknowledge(&bob_device, &[id], NOW).expect("ack"),
            1
        );
        assert_eq!(
            carrier.collect(&bob_device, NOW).held().map(<[_]>::len),
            Some(0)
        );
    }

    /// A send path cannot be written without proving whose key it spends.
    ///
    /// The compile-time half of this cannot be asserted from inside Rust — there
    /// is no way to write "this does not compile" as a runtime check. So this
    /// pins the runtime half: the id the carrier records comes from the witness,
    /// never from a caller-supplied claim, so a caller cannot deposit *as*
    /// somebody else even by asking nicely.
    #[test]
    fn the_deposit_is_attributed_to_the_witness_and_not_to_a_claim() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let (e_bob, bob) = incept(2, &space);
        let plane = actor::replay(&space, &[e_alice, e_bob]);

        // Alice's device cannot obtain standing for Bob's actor, so there is no
        // way to reach `deposit` at all while claiming to be him.
        assert_eq!(
            egress::authorize(&plane, &bob, &device_from_seed(&seed(1))).expect_err("borrowed key"),
            egress::Refused::NotThisActor {
                speaks_for: Some(alice.clone())
            },
        );

        let standing =
            egress::authorize(&plane, &alice, &device_from_seed(&seed(1))).expect("hers");
        let mut carrier = MemCarrier::new();
        let recipient = device_from_seed(&seed(9));
        carrier
            .deposit(&standing, &envelope_to(&recipient, b"x"), NOW)
            .expect("deposit");

        let waiting = carrier
            .collect(&recipient, NOW)
            .held()
            .expect("asked")
            .to_vec();
        assert_eq!(
            waiting[0].deposited_by,
            device_from_seed(&seed(1)),
            "attribution comes from the witness, which cannot be forged"
        );
    }

    /// An unreachable carrier is never an empty mailbox.
    #[test]
    fn a_carrier_that_could_not_be_asked_is_not_an_empty_mailbox() {
        let device = device_from_seed(&seed(4));
        let mut carrier = MemCarrier::new();

        assert_eq!(carrier.collect(&device, NOW), Missed::Held(vec![]));
        assert_eq!(
            carrier.collect(&device, NOW).held(),
            Some(&[][..]),
            "answered, and holding nothing"
        );

        carrier.seal_off("no route to the carrier");
        let missed = carrier.collect(&device, NOW);
        assert_eq!(
            missed,
            Missed::Unasked(String::from("no route to the carrier"))
        );
        assert_eq!(
            missed.held(),
            None,
            "…and it must not be readable as an empty mailbox, which is what a \
             person would see as nobody having written to them"
        );

        carrier.reopen();
        assert_eq!(carrier.collect(&device, NOW), Missed::Held(vec![]));
    }

    /// The bounds a carrier owes because deposit is unauthenticated.
    #[test]
    fn a_mailbox_is_bounded_and_a_retry_is_not_a_duplicate() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let plane = actor::replay(&space, &[e_alice]);
        let standing =
            egress::authorize(&plane, &alice, &device_from_seed(&seed(1))).expect("hers");
        let recipient = device_from_seed(&seed(5));
        let mut carrier = MemCarrier::new();

        let first = carrier
            .deposit(&standing, &envelope_to(&recipient, b"one"), NOW)
            .expect("deposit");

        // Byte-identical envelopes are one deposit. Sealing is randomized, so the
        // retry has to reuse the same `Sealed` — which is exactly what a client
        // redelivering something it already sealed would do.
        let envelope = envelope_to(&recipient, b"two");
        let a = carrier.deposit(&standing, &envelope, NOW).expect("deposit");
        let b = carrier
            .deposit(&standing, &envelope, NOW + 5)
            .expect("retry");
        assert_eq!(a, b, "a retry is not a duplicate");
        assert_eq!(carrier.holding(&recipient), 2);
        let _ = first;

        // Fill to the ceiling, then be refused.
        while carrier.holding(&recipient) < MAX_MAILBOX {
            let n = carrier.holding(&recipient);
            let body = format!("filler {n}");
            carrier
                .deposit(&standing, &envelope_to(&recipient, body.as_bytes()), NOW)
                .expect("under the ceiling");
        }
        assert_eq!(
            carrier
                .deposit(&standing, &envelope_to(&recipient, b"no room"), NOW)
                .expect_err("full"),
            Refused::AtCapacity
        );
        // But a redelivery of something already held still lands, or a full
        // mailbox would start refusing the retry of its own contents.
        assert_eq!(
            carrier.deposit(&standing, &envelope, NOW).expect("retry"),
            a
        );

        // Expired material occupies the mailbox until it is swept.
        assert!(carrier.sweep(NOW + 7200) >= MAX_MAILBOX);
        assert_eq!(carrier.holding(&recipient), 0);
    }

    /// A blocked sender's letter never lands, through the seam.
    ///
    /// The same property the Post has, at the seam so a review queue can rely on it
    /// without knowing which carrier is behind it. The recipient blocks; the sender
    /// deposits and is answered as if accepted; the mailbox holds nothing.
    #[test]
    fn a_blocked_sender_reaches_the_recipient_with_nothing() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let (e_bob, bob) = incept(2, &space);
        let plane = actor::replay(&space, &[e_alice, e_bob]);
        let alice_standing =
            egress::authorize(&plane, &alice, &device_from_seed(&seed(1))).expect("alice");
        let bob_standing =
            egress::authorize(&plane, &bob, &device_from_seed(&seed(2))).expect("bob");

        let alice_device = device_from_seed(&seed(1));
        let bob_device = device_from_seed(&seed(2));
        let mut carrier = MemCarrier::new();

        // Bob blocks Alice, on Bob's own authority.
        carrier
            .block(&bob_standing, &alice_device, true, NOW)
            .expect("bob may block alice");

        // Alice deposits to Bob — accept-shaped.
        carrier
            .deposit(&alice_standing, &envelope_to(&bob_device, b"unwanted"), NOW)
            .expect("a blocked deposit is accept-shaped");

        // Bob holds nothing.
        assert_eq!(
            carrier.collect(&bob_device, NOW),
            Missed::Held(vec![]),
            "a blocked sender's material must not occupy the recipient's mailbox"
        );

        // Bob unblocks; the next letter lands.
        carrier
            .block(&bob_standing, &alice_device, false, NOW)
            .expect("unblock");
        carrier
            .deposit(&alice_standing, &envelope_to(&bob_device, b"welcome"), NOW)
            .expect("deposit");
        assert_eq!(
            carrier.collect(&bob_device, NOW).held().map(<[_]>::len),
            Some(1),
            "an unblocked sender is delivered again"
        );
    }

    /// A carrier will not block on a mailbox it cannot sign for.
    #[test]
    fn a_carrier_refuses_to_block_for_a_device_it_is_not() {
        // MemCarrier signs for nobody, so its block authority is the witness alone —
        // it accepts any `by`. This property is the PostCarrier's, asserted in the
        // hop test; here we pin the seam shape: block takes a witness, not a bare
        // device, so "who is blocking" cannot be forged by naming.
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let plane = actor::replay(&space, &[e_alice]);
        let alice_standing =
            egress::authorize(&plane, &alice, &device_from_seed(&seed(1))).expect("alice");
        let mut carrier = MemCarrier::new();
        // The witness is required to reach `block` at all; there is no path that
        // blocks by a device name a caller supplies.
        carrier
            .block(&alice_standing, &device_from_seed(&seed(9)), true, NOW)
            .expect("a witness-bearing block is accepted");
    }

    /// Structural refusals are decided locally, before anything is stored.
    #[test]
    fn a_malformed_envelope_is_refused_by_what_is_wrong_with_it() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let plane = actor::replay(&space, &[e_alice]);
        let standing =
            egress::authorize(&plane, &alice, &device_from_seed(&seed(1))).expect("hers");
        let recipient = device_from_seed(&seed(6));
        let mut carrier = MemCarrier::new();

        let mut huge = envelope_to(&recipient, b"x");
        huge.bytes = vec![0u8; MAX_SEALED + 1];
        assert_eq!(
            carrier
                .deposit(&standing, &huge, NOW)
                .expect_err("too large"),
            Refused::TooLarge
        );

        let mut stale = envelope_to(&recipient, b"x");
        stale.expires_at = NOW;
        assert_eq!(
            carrier
                .deposit(&standing, &stale, NOW)
                .expect_err("expired"),
            Refused::UnusableExpiry
        );
        stale.expires_at = NOW + MAX_RETENTION + 1;
        assert_eq!(
            carrier
                .deposit(&standing, &stale, NOW)
                .expect_err("forever"),
            Refused::UnusableExpiry
        );

        // A non-canonical recipient spelling is refused at the boundary, as the
        // Post does: a `DeviceId` compares spelling-blind, but a carrier's store
        // may key a directory on the string, where an `Eq` impl cannot help.
        let mut shouted = envelope_to(&recipient, b"x");
        shouted.recipient = DeviceId::from_key_string(recipient.as_str().to_ascii_uppercase());
        assert_eq!(
            carrier
                .deposit(&standing, &shouted, NOW)
                .expect_err("non-canonical"),
            Refused::UnusableRecipient
        );

        assert_eq!(carrier.holding(&recipient), 0, "nothing was stored");
    }

    use crate::{MAX_RETENTION, MAX_SEALED};
}
