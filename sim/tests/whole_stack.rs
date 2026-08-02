//! Does the whole stack replay from a seed?
//!
//! Not the schedule — `runtime`'s convergence simulation already replays that.
//! This asks the harder question: does the *material* replay? Same transaction
//! commitments, same sealed bytes, same minted ids, byte for byte, across runs
//! and across machines.
//!
//! It is the question that decides whether a seed is a debugging aid or a real
//! reproduction. A schedule that replays lets a colleague see the same
//! assertion fail. A stack that replays lets them see the same *bytes*, which
//! is what you need when the bug is in the bytes.

use std::sync::Arc;

use mechanics::authorization::{
    AuthorizationDemand, AuthorizedBodyKey, PolicyCapability, Resource,
};
use mechanics::ids::SpaceId;
use replica::body::{
    BodyBinding, BodyId, BodyKey, EncodingId, Op, SchemaId, StaticBodyKeys, SupportedSchemas,
    WorldId, MUTATION_COLLABORATIVE,
};
use replica::frontier::AuthorityFrontier;
use replica::transaction::{
    CommitAuthorization, CommitContext, SeedSigner, StaticAuthorizer, NO_PARENT_ROOT,
};
use replica::Replica;

const EPOCH: [u8; 16] = [5u8; 16];
const EPOCH_KEY: [u8; 32] = [6u8; 32];
const WRITER: [u8; 32] = [0x41; 32];

fn space() -> SpaceId {
    SpaceId::from_digest([16u8; 16])
}

fn world() -> WorldId {
    WorldId::parse("com.example.notes").expect("world id")
}

fn body(index: u8) -> BodyKey {
    BodyKey::new(world(), BodyId::from_bytes([index; 16]))
}

fn binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("note").expect("schema"),
        schema_version: 1,
        encoding: EncodingId::parse("collab").expect("encoding"),
        mutation_model: MUTATION_COLLABORATIVE,
    }
}

fn supported() -> SupportedSchemas {
    let mut schemas = SupportedSchemas::new();
    schemas.declare(
        world(),
        SchemaId::parse("note").expect("schema"),
        1,
        EncodingId::parse("collab").expect("encoding"),
        MUTATION_COLLABORATIVE,
    );
    schemas
}

/// A short authoring run, reduced to the bytes it produced.
///
/// Commitments rather than counters: a commitment is a hash over the signed
/// transaction and its sealed payload, so two runs agreeing on them means the
/// nonces, the minted ids, the Loro export and the signature all agreed. It is
/// the strictest summary available without dumping the store.
fn run(seed: u64) -> (Vec<String>, u64) {
    lait_sim::seed(seed);

    let mut replica = Replica::loro().with_keys(Arc::new(StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
    )));
    replica.set_supported(supported());

    let space = space();
    let signer = SeedSigner(&WRITER);
    let authorizer = StaticAuthorizer {
        world: world(),
        implementation_id: [0u8; 32],
    };
    let demand = AuthorizationDemand::require(
        PolicyCapability::new(world().as_str(), "write"),
        Resource::root(world().as_str()),
    )
    .encode_canonical()
    .expect("demand encodes");

    for round in 0..6u8 {
        let ctx = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![7]),
        };
        let mut request = [0u8; 16];
        request[0] = round;
        let key = body(round % 3);
        replica
            .commit_action(
                &ctx,
                &CommitAuthorization {
                    actor: "actor",
                    parent_manifest_root: NO_PARENT_ROOT,
                    demand: demand.clone(),
                    intent_digest: [1u8; 32],
                    authorizer: &authorizer,
                },
                &world(),
                &mechanics::actor::device_from_seed(&WRITER),
                &request,
                &[1u8; 32],
                vec![],
                vec![],
                "bump",
                &[
                    (key.clone(), Op::Create),
                    (
                        key.clone(),
                        Op::CounterAdd {
                            path: "votes".into(),
                            delta: i64::from(round) + 1,
                        },
                    ),
                ],
                &[(key, binding())],
                &[],
            )
            .expect("commit");
    }

    let mut commitments: Vec<String> = replica
        .head_commitments()
        .into_iter()
        .map(|(key, commitment)| {
            let hex: String = commitment.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("{key:?}/{hex}")
        })
        .collect();
    commitments.sort();
    (commitments, lait_sim::draws())
}

/// **The claim.** With the entropy door closed, the same seed produces the same
/// bytes — not merely the same schedule.
///
/// ## The warm-up, and why it is not a fudge
///
/// The FIRST run in a process draws more than later ones — measured at 15
/// against a steady 13. That is one-time initialisation somewhere in the stack
/// taking entropy once and caching it, which is ordinary and correct behaviour
/// for a process, not a leak.
///
/// It has a practical consequence worth stating plainly: **a replay must be a
/// fresh process, or a warmed one.** In practice that is free, because nextest
/// runs every test in its own process — so a colleague running the same seeded
/// test gets a cold process exactly like yours, and the counts line up. This
/// test compares two runs INSIDE one process, so it warms first.
///
/// The draw count is asserted alongside the commitments because it fails
/// earlier and more informatively: two runs that drew a different number of
/// times have already diverged, even if this summary has not noticed yet.
#[test]
fn the_same_seed_produces_the_same_bytes() {
    let _warm = run(0xA11CE);
    let (first, first_draws) = run(0xA11CE);
    let (second, second_draws) = run(0xA11CE);
    assert_eq!(
        first_draws, second_draws,
        "the stack drew entropy a different number of times from the same seed"
    );
    assert_eq!(
        first, second,
        "the same seed produced different transaction commitments — something          is still reaching past getrandom"
    );
    assert!(
        !first.is_empty(),
        "no commitments were produced; this asserts nothing"
    );
}

/// One-time initialisation exists, and this is where it is written down.
///
/// Asserted rather than merely worked around, so that if it ever stops being
/// true — or gets worse — somebody finds out here rather than while chasing a
/// replay that will not line up.
#[test]
fn the_first_run_in_a_process_draws_more_than_later_ones() {
    let (_, cold) = run(0xC0FFEE);
    let (_, warm) = run(0xC0FFEE);
    let (_, warmer) = run(0xC0FFEE);
    assert!(
        cold > warm,
        "expected one-time initialisation on the first run: cold={cold} warm={warm}"
    );
    assert_eq!(
        warm, warmer,
        "draws should be steady once the process is warm"
    );
}

/// A different seed is a different run, or the seed is being ignored and the
/// test above would pass on a stack that had no entropy at all.
#[test]
fn a_different_seed_produces_different_bytes() {
    let (a, _) = run(0xA11CE);
    let (b, _) = run(0xB0B);
    assert_ne!(a, b, "two seeds produced identical bytes — the seed is inert");
}

/// The stack really is drawing entropy, and this backend really is serving it.
///
/// Without this the pair above could both pass on a build where the cfg was
/// mis-set and getrandom quietly used the OS — the seeds would be ignored and
/// so would the difference. Nonzero draws is the proof that the door exists and
/// traffic goes through it.
#[test]
fn the_stack_draws_through_this_backend() {
    let (_, draws) = run(1);
    assert!(
        draws > 0,
        "the stack drew no entropy at all — the custom backend is not wired in"
    );
}

/// A second of wall clock passing must not change the bytes.
///
/// This test exists because its absence nearly shipped a lie. With entropy
/// seeded but Loro still recording a wall-clock second on every change, the two
/// runs in `the_same_seed_produces_the_same_bytes` agreed — because they
/// happened inside the same second. The suite was green and the property was
/// false; a slower machine, or a run that straddled a tick, would have failed
/// at random.
///
/// It costs a second and a half of test time and is worth every millisecond: a
/// reproducibility claim that holds only within one second is not one.
#[test]
fn a_second_of_wall_clock_does_not_change_the_bytes() {
    let _warm = run(0xA11CE);
    let (before, _) = run(0xA11CE);
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    let (after, _) = run(0xA11CE);
    assert_eq!(
        before, after,
        "the bytes changed when a second passed — something is still reading          the wall clock into a commitment"
    );
}
