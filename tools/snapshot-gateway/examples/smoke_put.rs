//! End-to-end smoke against a DEPLOYED gateway: mint a real Space, sign a write
//! as its founder, and drive create → stale-conflict → authorized-retry over
//! real HTTP and real GCS. Proves the wiring the unit tests cannot — routing,
//! the metadata-server credential, and GCS's `ifGenerationMatch` passthrough.
//!
//!   GATEWAY=https://foundation-snapshot-gateway-...run.app \
//!     cargo run -p lait-snapshot-gateway --example smoke_put
//!
//! It writes a throwaway Space's snapshot (a fresh founder seed each run, so a
//! new capability key), then leaves it — the object is a few KiB and harmless.

use std::sync::Arc;

use contact::gateway::{object_key, sign_write, WriteEnvelope};
use contact::snapshot::SpaceSnapshot;
use mechanics::space::{
    derive_space_id, mint_recovery_key, recovery_commit, Authority as Ledger, Effect, Genesis,
};

fn mint_space(founder_seed: [u8; 32]) -> (Vec<u8>, mechanics::ids::SpaceId) {
    let founder_device = mechanics::actor::device_from_seed(&founder_seed);
    let salt = [7u8; 16];
    let (recovery_pub, _) = mint_recovery_key().unwrap();
    let recovery_root = recovery_commit(&recovery_pub).unwrap();
    let space = derive_space_id(&founder_device, &salt, &recovery_root);
    let (founder_inception, founder_actor) =
        mechanics::actor::incept_single(&founder_seed, &space, [1u8; 16], [2u8; 16], None);
    let genesis = Genesis {
        space_id: space.clone(),
        founding_actors: vec![founder_actor.clone()],
        salt,
        recovery_root,
    };
    let mut ledger =
        Ledger::create_on(Arc::new(journal::MemMedium::new()), genesis.clone()).unwrap();
    ledger
        .commit_batch(&[Effect::Actor(founder_inception.clone()).encode()], &[])
        .unwrap();
    let _ = founder_actor;
    let mut records = Vec::new();
    for effect in ledger.export_effects() {
        records.push(contact::authority::AuthorityRecord::Effect(effect).encode());
    }
    let snapshot = SpaceSnapshot {
        genesis,
        founder_inception: postcard::to_stdvec(&founder_inception).unwrap(),
        staged: replica::convergence::StagedContactMaterial {
            authority_records: records,
            manifest_root_bytes: Vec::new(),
            manifest_nodes: Vec::new(),
            bodies: Vec::new(),
        },
    };
    (snapshot.encode(), space)
}

fn envelope(seed: &[u8; 32], space: &mechanics::ids::SpaceId, gen: u64, blob: &[u8]) -> Vec<u8> {
    WriteEnvelope {
        request: sign_write(seed, space, gen, blob),
        blob: blob.to_vec(),
    }
    .encode()
}

fn put(url: &str, body: &[u8]) -> (u16, String) {
    match ureq::put(url)
        .set("Content-Type", "application/octet-stream")
        .send_bytes(body)
    {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(e) => (0, format!("transport: {e}")),
    }
}

fn main() {
    let base = std::env::var("GATEWAY").expect("set GATEWAY to the deployed service URL");
    let founder: [u8; 32] = std::array::from_fn(|i| {
        // A fresh-ish founder each run: mix the wall clock into the seed.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        ((now >> (i % 16)) as u8) ^ (i as u8)
    });
    let (blob, space) = mint_space(founder);
    let key = object_key(&space);
    let basename = key.strip_prefix("spaces/").unwrap();
    let url = format!("{base}/s/{basename}");
    println!("space   {}", space.as_str());
    println!("object  {key}");

    // Pull the `generation` / `current` number out of the gateway's JSON —
    // whatever it is. GCS generations are large, non-sequential timestamps; a
    // client learns the current one from the answer and NEVER predicts it.
    let number = |body: &str, field: &str| -> u64 {
        body.split(&format!("\"{field}\":"))
            .nth(1)
            .and_then(|rest| rest.trim_start().split([',', '}']).next())
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    };

    let (c1, b1) = put(&url, &envelope(&founder, &space, 0, &blob));
    println!("create (gen 0)        -> {c1} {b1}");
    assert_eq!(
        c1, 200,
        "the founder's first write should create the object"
    );
    let created = number(&b1, "generation");

    let (c2, b2) = put(&url, &envelope(&founder, &space, 0, &blob));
    println!("stale replay (gen 0)  -> {c2} {b2}");
    assert_eq!(c2, 412, "a second write still at gen 0 should conflict");
    let current = number(&b2, "current");
    assert_eq!(
        current, created,
        "the conflict names the generation just created"
    );

    let (c3, b3) = put(&url, &envelope(&founder, &space, current, &blob));
    println!("retry (gen {current}) -> {c3} {b3}");
    assert_eq!(
        c3, 200,
        "re-signed against the reported generation, the write lands"
    );

    println!("\nOK: create, conflict, and read-then-retry all behaved.");
}
