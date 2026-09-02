//! The portable authority seam: a mechanics ledger standing behind the
//! replica traits a Contact needs, with no daemon around it.
//!
//! Natively these impls live on the daemon's `SpaceAuthority`, wrapped in its
//! locks and join-lifecycle files. A pulling peer with no daemon — a browser
//! tab — needs the same substance and none of the housing: validate signer
//! standing at referenced frontiers, verify transaction receipts against
//! signed history, commit received authority batches atomically, and unseal
//! the epoch keys addressed to this device. [`AuthorityRecord`] moves here
//! with it, because the record is what rides Contact's authority section —
//! both encoders must be one type or they will drift.
//!
//! What this deliberately does NOT do: redeem admissions. Only an admin's
//! incorporator can; everyone else retains the material it rides beside,
//! exactly as the native incorporator behaves for a non-admin.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use mechanics::authorization::{open_sealed, AuthorizedBodyKey, SpaceKey};
use mechanics::ids::{DeviceId, SpaceId};
use mechanics::space::Authority as Ledger;
use replica::frontier::AuthorityFrontier;
use serde::{Deserialize, Serialize};

use crate::Authority;

/// One authority-record unit riding Contact's authority section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthorityRecord {
    /// One canonical signed ledger effect (actor event, ACL op, or terminal
    /// SpaceAuthority event). Import validates the complete batch, then
    /// commits atomically.
    Effect(Vec<u8>),
    /// One canonical [`mechanics::recovery::CeremonyMaterial`] record —
    /// ceremony transcript traffic under its distinct material-class tag and
    /// signing domain. Rides the same mechanics channel but commits to the
    /// ledger's separate bounded ceremony log, never an authority frontier.
    Ceremony(Vec<u8>),
    /// One sealed key-epoch envelope record. Authorization is the signed
    /// mint; a forged envelope is inert.
    SealedKey(Vec<u8>),
    /// A joiner's admission redemption: the admin-signed capability, the
    /// joiner's canonical inception bytes, the joiner's signed acceptance
    /// proof, and the canonical digest of the Coordinates it accepted. An
    /// admin incorporator validates the proof + capability and redeems it;
    /// everyone else retains the effect material it rides beside.
    Admission {
        admission: Vec<u8>,
        inception: Vec<u8>,
        proof: Vec<u8>,
        coordinates_digest: [u8; 32],
    },
}

impl AuthorityRecord {
    // Postcard serialization of this closed, allocation-backed enum has no
    // data-dependent failure mode; changing this durable helper's shape would
    // obscure the invariant at every caller.
    #[allow(clippy::expect_used)]
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("authority record")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

/// The ledger and the device standing behind it: everything a pulling peer's
/// authority seam needs, and nothing a daemon owns.
pub struct LedgerAuthority {
    pub space: SpaceId,
    pub ledger: Ledger,
    pub seed: [u8; 32],
    pub me: DeviceId,
    pub keyring: BTreeMap<[u8; 16], SpaceKey>,
    /// A joiner's PENDING admission request, self-incepted but not yet redeemed
    /// by an admin. While set and this device is not yet a member, [`export`]
    /// serves it as an `AuthorityRecord::Admission` — the request an admin's
    /// incorporator redeems. Byte-exact and reused across reloads (the single-
    /// use nonce burns if a re-incept changes the actor), so these are the
    /// already-encoded records, not re-derived. Cleared implicitly once
    /// admitted: `export` stops serving them when membership resolves.
    pub pending_admission: Option<PendingAdmission>,
}

/// The already-encoded material a joiner serves to request admission — the
/// portable mirror of the daemon's `pending_admission`/`inception`/`proof`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAdmission {
    /// Postcard `mechanics`-minted `AdmissionCapability`.
    pub admission: Vec<u8>,
    /// Postcard joiner `actor::SignedEvent` inception.
    pub inception: Vec<u8>,
    /// Postcard `InvitationAcceptanceProof`.
    pub proof: Vec<u8>,
    pub coordinates_digest: [u8; 32],
}

impl LedgerAuthority {
    pub fn new(space: SpaceId, ledger: Ledger, seed: [u8; 32]) -> Self {
        let me = mechanics::actor::device_from_seed(&seed);
        let mut this = Self {
            space,
            ledger,
            seed,
            me,
            keyring: BTreeMap::new(),
            pending_admission: None,
        };
        this.refresh_keyring();
        this
    }

    /// Stash a self-incepted admission request to serve until an admin redeems
    /// it. The caller (the joiner's pull setup) mints the inception + acceptance
    /// proof once, persists them durably, and reuses them byte-for-byte — a
    /// re-incept would mint a different actor and burn the single-use nonce.
    pub fn stash_admission(&mut self, pending: PendingAdmission) {
        self.pending_admission = Some(pending);
    }

    /// This device's admitted actor, if the ledger now carries it — the signal
    /// that a pending admission has been redeemed and need no longer be served.
    /// Public because a joiner's await-admission loop exits on exactly this
    /// predicate — the same one `LedgerAuthorityView::resolve` answers with.
    pub fn admitted(&mut self) -> bool {
        let Some(actor) = self.ledger.actor_plane().actor_of_device(&self.me).cloned() else {
            return false;
        };
        self.ledger
            .acl_state()
            .map(|state| state.is_member(&actor))
            .unwrap_or(false)
    }

    /// The authority records this peer serves over Contact. A pulling member
    /// serves nothing; an unadmitted joiner serves exactly its pending admission
    /// request, and stops the moment membership resolves — the portable mirror
    /// of the daemon's `export_records` admission branch.
    pub fn export_records(&mut self) -> Vec<Vec<u8>> {
        if self.admitted() {
            return Vec::new();
        }
        match &self.pending_admission {
            Some(pending) => vec![AuthorityRecord::Admission {
                admission: pending.admission.clone(),
                inception: pending.inception.clone(),
                proof: pending.proof.clone(),
                coordinates_digest: pending.coordinates_digest,
            }
            .encode()],
            None => Vec::new(),
        }
    }

    pub fn frontier(&self) -> AuthorityFrontier {
        AuthorityFrontier::from_canonical_bytes(self.ledger.frontier())
    }

    /// Unseal every authorized epoch key addressed to this device into the
    /// keyring, bound to the signed mint's commitment.
    pub fn refresh_keyring(&mut self) {
        // An unreplayable ledger here leaves the keyring as it was: an empty
        // keyring is indistinguishable from "no epochs are authorized for
        // me", and the node would quietly fail to decrypt what it holds.
        let acl_state = match self.ledger.acl_state() {
            Ok(state) => state,
            Err(failure) => {
                tracing::error!(?failure, "authority ledger could not be replayed");
                return;
            }
        };
        for e in acl_state.epochs() {
            if self.keyring.contains_key(&e.id) {
                continue;
            }
            if let Some(sealed) = self.ledger.sealed_for(&e.id, &self.me) {
                if let Some(raw) = open_sealed(&self.seed, &self.me, &sealed) {
                    if let Ok(key) = <SpaceKey>::try_from(raw.as_slice()) {
                        if *blake3::hash(&key).as_bytes() == e.key_commit {
                            self.keyring.insert(e.id, key);
                        }
                    }
                }
            }
        }
    }

    fn active_epoch(&mut self) -> Option<mechanics::membership::EpochAuth> {
        self.ledger
            .acl_state()
            .ok()?
            .epochs()
            .into_iter()
            .max_by(|a, b| a.gen.cmp(&b.gen).then_with(|| a.id.cmp(&b.id)))
    }
}

/// The shareable form the traits hang off — the same lock shape the daemon's
/// `SpaceAuthority` uses, minus the daemon.
#[derive(Clone)]
pub struct SharedLedgerAuthority(pub Arc<Mutex<LedgerAuthority>>);

impl SharedLedgerAuthority {
    pub fn new(inner: LedgerAuthority) -> Self {
        Self(Arc::new(Mutex::new(inner)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerAuthority> {
        match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The bundle the pull takes. The export closure serves this peer's
    /// authority records: nothing for an admitted member, and an unadmitted
    /// joiner's pending admission REQUEST — carried out on the symmetric-Contact
    /// push so a tab can join without a daemon on its device.
    pub fn bundle(&self) -> Authority {
        let source = self.clone();
        let frontier = self.clone();
        let export = self.clone();
        Authority {
            source: Arc::new(source),
            incorporator: Arc::new(Mutex::new(self.clone())),
            export: Arc::new(move || export.lock().export_records()),
            frontier: Arc::new(move || frontier.lock().frontier()),
        }
    }
}

impl replica::transaction::AuthoritySource for SharedLedgerAuthority {
    fn signer_authorized(&self, signer: &[u8; 32], frontier: &AuthorityFrontier) -> bool {
        // Standing is evaluated at the **referenced** frontier — the exact
        // effect closure its heads name — never against current state.
        self.lock()
            .ledger
            .signer_authorized_at(signer, frontier.as_bytes())
    }

    fn verify_transaction(
        &self,
        tx: &replica::transaction::Transaction,
    ) -> Result<(), replica::transaction::Refusal> {
        let receipt = tx.receipt().map_err(|error| {
            replica::transaction::Refusal::Unauthorized(format!("receipt decode: {error:?}"))
        })?;
        self.lock()
            .ledger
            .verify_receipt(
                &receipt,
                &mechanics::authorization::ReceiptExpectations {
                    device: &tx.core.signer,
                    authority_frontier: tx.core.authority_frontier.as_bytes(),
                    parent_manifest_root: &tx.core.parent_manifest_root,
                    intent_digest: &tx.core.intent_digest,
                    demand: &tx.core.demand,
                    effect_operations_digest: &tx.core.operations_digest,
                    body_transaction_core_digest: &tx.core.digest(),
                },
            )
            .map_err(|invalid| {
                replica::transaction::Refusal::Unauthorized(format!("receipt: {invalid:?}"))
            })
    }
}

impl replica::body::BodyKeySource for SharedLedgerAuthority {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        let mut inner = self.lock();
        let epoch = inner.active_epoch()?;
        let key = inner.keyring.get(&epoch.id)?;
        Some(AuthorizedBodyKey::for_authorized_epoch(epoch.id, *key))
    }
    fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
        let mut inner = self.lock();
        // Only an AUTHORIZED epoch's key may open material.
        inner.ledger.acl_state().ok()?.epoch(epoch)?;
        let key = inner.keyring.get(epoch)?;
        Some(AuthorizedBodyKey::for_authorized_epoch(*epoch, *key))
    }
}

impl replica::convergence::AuthorityIncorporator for SharedLedgerAuthority {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<replica::convergence::AuthorityBatchReceipt, replica::convergence::Failure> {
        let mut inner = self.lock();
        // Split the staged records: effects + sealed keys commit as ONE
        // atomic ledger batch (an invalid record refuses the whole batch);
        // admissions are retained, not redeemed — only an admin can.
        let mut effects: Vec<Vec<u8>> = Vec::new();
        let mut sealed: Vec<Vec<u8>> = Vec::new();
        let mut ceremony: Vec<Vec<u8>> = Vec::new();
        for raw in records {
            match AuthorityRecord::decode(raw) {
                Some(AuthorityRecord::Effect(bytes)) => effects.push(bytes),
                Some(AuthorityRecord::SealedKey(bytes)) => sealed.push(bytes),
                Some(AuthorityRecord::Ceremony(bytes)) => ceremony.push(bytes),
                Some(AuthorityRecord::Admission { .. }) => {
                    tracing::debug!("admission retained; redemption is an admin's act");
                }
                None => return Err(replica::convergence::Failure::Invalid),
            }
        }
        // Pre-validate the ceremony batch BEFORE committing anything, so one
        // invalid record refuses the whole incorporation with the durable
        // ledger unchanged.
        for record in &ceremony {
            let material = mechanics::recovery::CeremonyMaterial::decode(record)
                .map_err(|_| replica::convergence::Failure::Invalid)?;
            if !material.verify(&inner.space) {
                return Err(replica::convergence::Failure::Invalid);
            }
        }
        let prior = inner.frontier();
        let receipt = inner
            .ledger
            .commit_batch(&effects, &sealed)
            .map_err(incorporation_failure)?;
        // The ceremony-material class commits at its own linearization point,
        // after the authority batch (a crash between the two exposes the
        // complete earlier phase; idempotent retry re-lands the rest).
        inner
            .ledger
            .commit_ceremony_batch(&ceremony)
            .map_err(incorporation_failure)?;
        inner.refresh_keyring();
        Ok(replica::convergence::AuthorityBatchReceipt {
            space: inner.space.clone(),
            prior_frontier: prior,
            resulting_frontier: inner.frontier(),
            batch_digest: receipt.batch_digest,
        })
    }
}

fn incorporation_failure(failure: mechanics::space::Failure) -> replica::convergence::Failure {
    match failure {
        mechanics::space::Failure::InvalidRecord | mechanics::space::Failure::MalformedFrontier => {
            replica::convergence::Failure::Invalid
        }
        mechanics::space::Failure::MissingHistory => replica::convergence::Failure::Refusal,
        mechanics::space::Failure::Journal(_) | mechanics::space::Failure::Corrupt => {
            replica::convergence::Failure::Operation
        }
    }
}
