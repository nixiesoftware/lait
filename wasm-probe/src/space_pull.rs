//! The member-device pull, shared by every claim that stands on it: parse the
//! ticket, stand up the ledger and Replica on real OPFS, reach the relay, and
//! pull the Space whole over `lait/contact/2`. `tests/space.rs` proves this
//! crossing by itself; `tests/space_call.rs` composes the engine on top of
//! what it returns. One flow, so a drift between "the pull the pull-test
//! proves" and "the pull the engine stands on" cannot open.
//!
//! Deliberately a *member device pull*, never a second self-inception: the
//! seed already maps to an admitted actor in the replicated ledger, and
//! re-entering would mint a different actor.

use std::sync::Arc;

use comms::policy::{LocalNet, Network};
use comms::{DefaultFactory, Protocols, Transport, TransportFactory};
use contact::authority::{LedgerAuthority, SharedLedgerAuthority};
use contact::coordinates::SignedCoordinates;
use contact::pull::{pull_whole, Deadlines};
use contact::Outcome;
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
    transport: Arc<dyn Transport>,
    responder: Key,
    seed: [u8; 32],
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
            Deadlines::default(),
        )
        .await
        .expect("the repeated pull completes")
    }
}

/// Join as the pre-admitted member device and pull the Space whole: ticket →
/// genesis + founder inception → fresh ledger and Replica on OPFS → relay-only
/// transport → `pull_whole`.
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

    let outcome = pull_whole(
        transport.as_ref(),
        &responder,
        &space,
        &seed,
        &authority.bundle(),
        &mut replica,
        Deadlines::default(),
    )
    .await
    .expect("the pull completes");

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
