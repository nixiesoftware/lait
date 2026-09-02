//! The device pull, shared by every claim that stands on it: parse the
//! ticket, stand up the ledger and Replica on real OPFS, reach the relay, and
//! pull the Space whole over `lait/contact/2`. `tests/space.rs` proves this
//! crossing by itself; `tests/space_call.rs` composes the engine on top of
//! what it returns. One flow, so a drift between "the pull the pull-test
//! proves" and "the pull the engine stands on" cannot open.
//!
//! A ticket that carries an admission capability makes this the tab's ENTER:
//! the device self-incepts, stashes the pending admission request, and its
//! first dials push that request out on the symmetric reverse phase until an
//! admin redeems it — request-and-founder-redeems, never self-admit. The
//! inception is DETERMINISTIC in `(device seed, space)`: every nonce it needs
//! is derived, never drawn from entropy, so any re-entry that still holds the
//! same seed — a reload, a crash, a wiped local ledger — re-mints
//! byte-identical inception material and the same actor. That determinism is
//! what keeps the single-use invite nonce alive across re-entries (redemption
//! is idempotent per actor on the founder). It does NOT survive losing the
//! seed itself: cleared site data or a different browser profile mints a new
//! seed, hence a new actor a spent single-use invite cannot admit — that
//! person needs an admin to re-mint, and slice-3's boot must say so legibly.

use std::sync::Arc;

use comms::policy::{LocalNet, Network};
use comms::{DefaultFactory, Protocols, Transport, TransportFactory};
use contact::authority::{LedgerAuthority, PendingAdmission, SharedLedgerAuthority};
use contact::coordinates::SignedCoordinates;
use contact::pull::{pull_whole, Deadlines};
use contact::{Outcome, OutboundTransfer};
use mechanics::ids::{ActorId, SpaceId};
use mechanics::space::{Authority, Effect, Genesis};
use mechanics::station::Key;
use replica::Replica;

/// Decode a 64-char hex seed the harness minted.
pub fn unhex32(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex seed");
    }
    out
}

fn unique_dir(tag: &str) -> String {
    let mut noise = [0u8; 8];
    let _ = getrandom03::fill(&mut noise);
    let hex: String = noise.iter().map(|b| format!("{b:02x}")).collect();
    format!("space-{tag}-{hex}")
}

/// Everything the pull stood up and everything a caller needs to go further:
/// re-pull for idempotence, or compose an engine over the member's Replica.
pub struct PulledSpace {
    pub space: SpaceId,
    pub authority: SharedLedgerAuthority,
    pub replica: Replica,
    pub outcome: Outcome,
    /// The live transport, kept so a caller can re-pull after the Replica has
    /// been composed into a Station (which takes it by value) — the seam a
    /// live re-pull installs new material through.
    pub transport: Arc<dyn Transport>,
    pub responder: Key,
    pub seed: [u8; 32],
}

impl PulledSpace {
    /// Pull again over the same transport; convergence makes it idempotent.
    pub async fn pull_again(&mut self) -> Outcome {
        pull_whole(
            self.transport.as_ref(),
            &self.responder,
            &self.space,
            &self.seed,
            &self.authority.bundle(),
            &mut self.replica,
            None,
            Deadlines::default(),
        )
        .await
        .expect("the repeated pull completes")
    }
}

/// Derive one of the enter flow's nonces from `(device seed, space)` — the
/// determinism that keeps re-entry byte-identical. Each caller passes its own
/// derivation context, so no two nonces collide.
fn derive16(context: &str, seed: &[u8; 32], space: &SpaceId) -> [u8; 16] {
    let mut material = Vec::with_capacity(32 + space.as_str().len());
    material.extend_from_slice(seed);
    material.extend_from_slice(space.as_str().as_bytes());
    let full = blake3::derive_key(context, &material);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// Self-incept deterministically and stash the pending admission request the
/// export closure will serve. The recovery seed is DERIVED from the device
/// seed — a tab has nowhere durable to keep an independent one — which trades
/// recovery independence for reload-stability; a tab-born actor's recovery is
/// only as separate as its device seed. Stated, not hidden.
fn stash_admission_request(
    authority: &SharedLedgerAuthority,
    coordinates: &SignedCoordinates,
    admission: &contact::coordinates::AdmissionCapability,
    seed: &[u8; 32],
    space: &SpaceId,
) {
    let recovery_seed = {
        let mut material = Vec::with_capacity(32 + space.as_str().len());
        material.extend_from_slice(seed);
        material.extend_from_slice(space.as_str().as_bytes());
        blake3::derive_key("lait.browser-enter.recovery.v1", &material)
    };
    let recovery_pub = mechanics::actor::device_from_seed(&recovery_seed);
    let recovery_commit =
        mechanics::actor::recovery_commitment(&recovery_pub).expect("recovery commitment");
    let (inception, candidate_actor) = mechanics::actor::incept_single(
        seed,
        space,
        derive16("lait.browser-enter.incept-nonce.v1", seed, space),
        derive16("lait.browser-enter.binding-nonce.v1", seed, space),
        Some(recovery_commit),
    );
    let coordinates_digest = coordinates.digest();
    let space_bytes =
        <[u8; 29]>::try_from(space.as_str().as_bytes()).expect("space id is 29 rendered bytes");
    let accepted_at = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .expect("wall clock")
        .as_secs();
    let proof = contact::coordinates::InvitationAcceptanceProof::sign(
        seed,
        accepted_at,
        derive16("lait.browser-enter.acceptance-nonce.v1", seed, space),
        &coordinates_digest,
        &space_bytes,
        &admission.issuer,
        &admission.capability_id(),
        candidate_actor.as_str(),
    )
    .expect("sign acceptance proof");
    let pending = PendingAdmission {
        admission: postcard::to_stdvec(admission).expect("admission encodes"),
        inception: postcard::to_stdvec(&inception).expect("inception encodes"),
        proof: postcard::to_stdvec(&proof).expect("proof encodes"),
        coordinates_digest,
    };
    match authority.0.lock() {
        Ok(mut inner) => inner.stash_admission(pending),
        Err(poisoned) => poisoned.into_inner().stash_admission(pending),
    }
}

/// Is this device's actor admitted on the local ledger replay?
fn admitted(authority: &SharedLedgerAuthority) -> bool {
    match authority.0.lock() {
        Ok(mut inner) => inner.admitted(),
        Err(poisoned) => poisoned.into_inner().admitted(),
    }
}

/// Build the admission-only reverse transfer an unadmitted joiner pushes —
/// byte-identical in shape to the native driver's `build_outbound` for a
/// signer with no authoring standing: the export's records, nothing else.
fn admission_push(authority: &SharedLedgerAuthority) -> Option<OutboundTransfer> {
    let (records, frontier) = match authority.0.lock() {
        Ok(mut inner) => (inner.export_records(), inner.frontier()),
        Err(poisoned) => {
            let mut inner = poisoned.into_inner();
            (inner.export_records(), inner.frontier())
        }
    };
    if records.is_empty() {
        return None;
    }
    Some(OutboundTransfer {
        authority_frontier: frontier.as_bytes().to_vec(),
        authority_records: records,
        manifest_root_bytes: Vec::new(),
        manifest_nodes: Vec::new(),
        bodies: Vec::new(),
    })
}

/// Join and pull the Space whole: ticket → genesis + founder inception →
/// fresh ledger and Replica on OPFS → relay-only transport → `pull_whole`.
///
/// A ticket carrying an admission capability runs the ENTER: the device
/// self-incepts (deterministically — see the module doc), stashes the pending
/// admission request, and loops pull-with-push until an admin redeems it and
/// the membership arrives on the next pull. A pre-admitted device's ticket
/// resolves on the first pull and the loop never spins.
///
/// `configure` runs on the fresh Replica BEFORE the pull — the seam for
/// declarations that must precede incorporation, such as the engine's
/// supported schemas (an undeclared body is retained opaque, and only
/// re-receipt upgrades it). The bare pull claim passes a no-op.
pub async fn pull_space(
    relay: &str,
    seed: [u8; 32],
    ticket: &str,
    configure: impl FnOnce(&mut Replica),
) -> PulledSpace {
    // The ticket is the whole public bundle: space identity, genesis
    // material, and whom to approach.
    let coordinates = SignedCoordinates::parse_link(ticket).expect("ticket parses");
    let verified = coordinates.verify().expect("ticket verifies");
    let space = verified.space.clone();
    let founder_inception: mechanics::actor::SignedEvent =
        postcard::from_bytes(&coordinates.payload.founder_inception)
            .expect("founder inception decodes");
    let genesis = Genesis {
        space_id: space.clone(),
        founding_actors: vec![ActorId::from_incept_hash(&founder_inception.hash())],
        salt: coordinates.payload.salt,
        recovery_root: coordinates.payload.recovery_root,
    };

    // The member device's whole world, on real browser storage.
    let ledger_medium = journal::OpfsMedium::open(&unique_dir("ledger"))
        .await
        .expect("opfs");
    let mut ledger = Authority::create_on(Arc::new(ledger_medium), genesis).expect("fresh ledger");
    ledger
        .commit_batch(&[Effect::Actor(founder_inception).encode()], &[])
        .expect("founder inception lands");
    let authority = SharedLedgerAuthority::new(LedgerAuthority::new(space.clone(), ledger, seed));
    // A ticket carrying an admission makes this an enter: self-incept and
    // stash the request the export closure serves until an admin redeems it.
    if let Some(admission) = verified.admission.as_ref() {
        if !admitted(&authority) {
            stash_admission_request(&authority, &coordinates, admission, &seed, &space);
        }
    }
    let replica_medium = journal::OpfsMedium::open(&unique_dir("replica"))
        .await
        .expect("opfs");
    let mut replica = Replica::open_on(Arc::new(replica_medium), Arc::new(authority.clone()))
        .expect("fresh replica");
    configure(&mut replica);

    // The transport, exactly as the live claim proved it: relay-only,
    // learn-then-dial.
    let network = Network::Local(LocalNet {
        relays: vec![relay.to_owned()],
    });
    let transport = DefaultFactory
        .build(&seed, &network, Protocols::framed(&[]))
        .await
        .expect("browser endpoint");
    let responder = Key::from_key_bytes(coordinates.payload.approach_station);
    transport.learn(responder.as_device(), &[]);

    // The await-admission loop. Each iteration pulls the responder's material
    // AND — while unadmitted with a stashed request — pushes the admission on
    // the same dial's reverse phase. The responder redeems asynchronously
    // after acking the push, so the membership and sealed keys arrive on a
    // LATER pull; the loop's re-dial is load-bearing, not a retry. A member
    // device (no pending request) breaks after its first successful pull,
    // exactly the old behavior. A FAILED pull is "could not be asked", never
    // "refused" — a browser join rides a residential network, so a transient
    // dial failure retries until the deadline instead of killing the enter.
    let deadline = n0_future::time::Instant::now() + n0_future::time::Duration::from_secs(60);
    let outcome = loop {
        let reverse = admission_push(&authority);
        let awaiting = reverse.is_some();
        match pull_whole(
            transport.as_ref(),
            &responder,
            &space,
            &seed,
            &authority.bundle(),
            &mut replica,
            reverse,
            Deadlines::default(),
        )
        .await
        {
            Ok(outcome) => {
                if !awaiting || admitted(&authority) {
                    break outcome;
                }
                // The two terminal absences are different facts and say so:
                // the peer ANSWERED but no admin has redeemed the request.
                assert!(
                    n0_future::time::Instant::now() < deadline,
                    "the peer answered but the admission was not redeemed within the deadline"
                );
            }
            Err(failure) => {
                assert!(
                    n0_future::time::Instant::now() < deadline,
                    "the Space could not be pulled within the deadline: {failure:?}"
                );
            }
        }
        n0_future::time::sleep(n0_future::time::Duration::from_millis(750)).await;
    };

    PulledSpace {
        space,
        authority,
        replica,
        outcome,
        transport,
        responder,
        seed,
    }
}
