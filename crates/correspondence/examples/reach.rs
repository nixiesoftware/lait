//! A narrated walk of reaching a person who shares no Space with you.
//!
//! Run: `cargo run -p correspondence --example reach`
//!
//! Everything here is the real substrate — the kinship registry, the
//! correspondence letter, a carrier — with `println!`s between the steps so the
//! handshake is legible. Alice and Bob share no Space; the only thing that lets
//! Bob reach Alice is a profile she chose to make reachable.

use addressbook::{registry, Registry};
use correspondence::{Carrier, Content, Letter, Mailbox, MemCarrier, Missed};
use mechanics::actor::{
    self, consent_sign, device_from_seed, sign_event, ActorOp, ConsentCtx, SignedEvent,
};
use mechanics::egress;
use mechanics::ids::{ActorId, SpaceId, SystemUlidSource};
use mechanics::kinship::{Audience, DeviceLink, Party, Standing};

const NOW: u64 = 1_800_000_000;
const ALICE_A: [u8; 32] = [11u8; 32];
const ALICE_B: [u8; 32] = [12u8; 32];
const BOB: [u8; 32] = [40u8; 32];
const MALLORY: [u8; 32] = [99u8; 32];

fn incept(seed: &[u8; 32], nonce: u8, space: &SpaceId) -> (Vec<SignedEvent>, ActorId) {
    let devices = vec![device_from_seed(seed)];
    let binding = consent_sign(
        seed,
        space.as_str(),
        [nonce; 16],
        &ConsentCtx::Incept {
            incept_nonce: &[nonce; 16],
            devices: &devices,
            recovery_commit: &None,
        },
    );
    let event = sign_event(
        seed,
        &ActorOp::Incept {
            space: space.as_str().to_owned(),
            nonce: [nonce; 16],
            devices: vec![binding],
            recovery_commit: None,
        },
        vec![],
        space,
    );
    let id = ActorId::from_incept_hash(&event.hash());
    (vec![event], id)
}

fn short(device: &mechanics::ids::DeviceId) -> String {
    let s = device.as_str();
    format!("{}…{}", &s[..8], &s[s.len() - 4..])
}

fn main() {
    let bob_device = device_from_seed(&BOB);
    let bob_standing = Standing {
        device: Some(bob_device.clone()),
        ..Standing::default()
    };

    println!("── Reaching a person who shares no Space with you ──\n");

    // 1. Alice publishes a profile from two of her devices.
    let mut alice = Registry::new();
    let genesis = DeviceLink::seal(&ALICE_A, &ALICE_B, [7u8; 16], 1).expect("link");
    let profile = alice.found(genesis.clone()).expect("found");
    println!("1. Alice founds a profile from her two devices.");
    println!("     profile   {}", profile.as_str());
    println!(
        "     devices   {} , {}",
        short(&device_from_seed(&ALICE_A)),
        short(&device_from_seed(&ALICE_B))
    );

    // 2. She makes her devices reachable to Bob and projects it for him.
    alice
        .avow_reachable(
            &profile,
            Audience::Correspondent(Party::Device(bob_device.clone())),
            &ALICE_A,
            5,
            [3u8; 16],
        )
        .expect("avow");
    let projection = alice
        .project(&profile, &ALICE_A, 5, &bob_standing)
        .expect("project");
    println!("\n2. Alice avows her set reachable *to Bob* and projects it.");
    println!(
        "     the projection carries {} avowal bodies + a signed head",
        projection.bodies.len()
    );

    // 3. A forger cannot substitute the device set.
    println!("\n3. Before Bob trusts it — can a stranger forge this? Try a wrong anchor:");
    let mut victim = Registry::new();
    let mallory_genesis = DeviceLink::seal(&MALLORY, &BOB, [1u8; 16], 1).expect("link");
    match victim.absorb(projection.clone(), &mallory_genesis, &bob_standing) {
        Err(registry::Failure::Unanchored) => {
            println!(
                "     refused: Unanchored — the genesis must hash to the very profile it claims."
            );
        }
        other => panic!("a mis-anchored projection must be refused, got {other:?}"),
    }

    // 4. Bob absorbs the genuine projection and resolves Alice's devices.
    let mut bob = Registry::new();
    bob.absorb(projection, &genesis, &bob_standing)
        .expect("absorb");
    let recipients = bob.resolve(&profile).expect("resolved");
    println!("\n4. Bob absorbs the genuine projection (anchored) and resolves Alice:");
    for device in &recipients {
        println!("     -> {}", short(device));
    }

    // 5. Bob seals a signed letter to those devices and hands it to a carrier.
    let letter = Letter::compose(
        &BOB,
        Content::Message {
            body: "no Space in common, and yet".into(),
        },
        NOW,
    );
    let addressed = recipients[0].clone();
    let sealed = letter
        .seal_to_devices(&recipients, &addressed, NOW + 3600)
        .expect("seal");
    let space = SpaceId::mint(&SystemUlidSource);
    let (bob_events, bob_actor) = incept(&BOB, 9, &space);
    let plane = actor::replay(&space, &bob_events);
    let bob_egress = egress::authorize(&plane, &bob_actor, &bob_device).expect("egress");
    let mut carrier = MemCarrier::new();
    let id = carrier.deposit(&bob_egress, &sealed, NOW).expect("deposit");
    println!("\n5. Bob seals a signed letter to that set and deposits it at the carrier.");
    println!(
        "     sealed    {} bytes (the carrier can never read them)",
        sealed.bytes.len()
    );
    println!("     deposit   {id}");
    println!("     …time passes; Alice was offline…");

    // 6. Alice collects on her device, opens it, and the signature verifies.
    let alice_seed = if addressed == device_from_seed(&ALICE_A) {
        ALICE_A
    } else {
        ALICE_B
    };
    let waiting = match carrier.collect(&addressed, NOW + 10) {
        Missed::Held(waiting) => waiting,
        Missed::Unasked(why) => panic!("carrier unreachable: {why}"),
    };
    let mut mailbox = Mailbox::new();
    mailbox.ingest(&alice_seed, &addressed, &waiting);
    let received = &mailbox.letters()[0];
    println!(
        "\n6. Alice comes online, collects on {}, and opens it:",
        short(&addressed)
    );
    println!(
        "     from      {}  (proven by signature)",
        short(&received.letter.from)
    );
    match &received.letter.content {
        Content::Message { body } => println!("     message   \"{body}\""),
        other => println!("     {other:?}"),
    }

    println!(
        "\n── Reached. No Space in common, no third channel, and the carrier stayed blind. ──"
    );
}
