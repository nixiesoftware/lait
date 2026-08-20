//! The directory over a real socket.
//!
//! The service tests prove the rules; this proves the *surface* — the routes,
//! the JSON shapes and the status mapping, which the module doc calls "exactly
//! the kind of thing that is obviously right and quietly wrong". A router that
//! only exists inside `main` is one nothing can drive.

use std::sync::{Arc, Mutex};

use addressbook::{Announcement, Registry};
use lait_directory::{
    address::Address,
    http::{router, Shared},
    wire::sign,
    Challenge, MemStore, Service, SignedPublish, SignedResolve,
};
use mechanics::{
    actor::device_from_seed,
    kinship::{Audience, DeviceLink, Standing},
};

/// Serve the directory on an ephemeral port and answer its base URL.
async fn serve() -> String {
    let shared: Shared<MemStore> = Arc::new(Mutex::new(Service::new(MemStore::new())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(shared)).await;
    });
    format!("http://127.0.0.1:{port}")
}

/// One identity home with a real genesis and a public announcement.
fn announced(a: u8, b: u8, epoch: u64) -> ([u8; 32], Announcement) {
    let seeds = [[a; 32], [b; 32]];
    let genesis = DeviceLink::seal(&seeds[0], &seeds[1], [7u8; 16], 1).expect("genesis");
    let mut registry = Registry::new();
    let profile = registry.found(genesis.clone()).expect("found");
    registry
        .avow_reachable(&profile, Audience::Public, &seeds[0], epoch, [9u8; 16])
        .expect("avow");
    let projection = registry
        .project(&profile, &seeds[0], epoch, &Standing::default())
        .expect("project");
    (seeds[0], Announcement::new(profile, genesis, projection))
}

fn challenge(base: &str, seed: &[u8; 32]) -> Challenge {
    let device = device_from_seed(seed);
    ureq::get(&format!(
        "{base}/directory/challenge?device={}",
        device.as_str()
    ))
    .call()
    .expect("a challenge is free")
    .into_json()
    .expect("a challenge is JSON")
}

#[tokio::test(flavor = "multi_thread")]
async fn an_address_is_published_and_resolved_over_http() {
    let base = serve().await;

    let health: String = ureq::get(&format!("{base}/directory/health"))
        .call()
        .expect("health")
        .into_string()
        .expect("text");
    assert_eq!(health, "ok");

    let (seed, announcement) = announced(21, 22, 2);
    let request: SignedPublish = sign::publish(
        &seed,
        &challenge(&base, &seed),
        announcement.encode().expect("encode"),
    );
    let published: serde_json::Value = ureq::post(&format!("{base}/directory/publish"))
        .send_json(serde_json::to_value(&request).expect("serialize"))
        .expect("publish")
        .into_json()
        .expect("json");
    let address = Address::parse(published["address"].as_str().expect("an address"))
        .expect("the service minted something it would accept back");

    let asker = [90u8; 32];
    let request: SignedResolve = sign::resolve(&asker, &challenge(&base, &asker), &address);
    let resolved: serde_json::Value = ureq::post(&format!("{base}/directory/resolve"))
        .send_json(serde_json::to_value(&request).expect("serialize"))
        .expect("resolve")
        .into_json()
        .expect("json");

    let bytes = data_encoding::HEXLOWER
        .decode(resolved["announcement"].as_str().expect("hex").as_bytes())
        .expect("the answer is hex");
    let answered = Announcement::decode(&bytes).expect("decode");
    assert_eq!(answered.profile, announcement.profile);
}

/// The status is part of the answer, so a distinguishable one would undo what
/// the refusal value is careful about. 404 with the same body, both times.
#[tokio::test(flavor = "multi_thread")]
async fn an_address_nobody_holds_is_a_404_that_says_nothing_about_existence() {
    let base = serve().await;
    let asker = [90u8; 32];

    let unheld = Address::mint(&[0x33; 16]);
    let request = sign::resolve(&asker, &challenge(&base, &asker), &unheld);
    let error = ureq::post(&format!("{base}/directory/resolve"))
        .send_json(serde_json::to_value(&request).expect("serialize"))
        .expect_err("nobody holds it");

    let ureq::Error::Status(status, response) = error else {
        panic!("the directory was unreachable rather than answering");
    };
    assert_eq!(status, 404);
    let body: serde_json::Value = response.into_json().expect("json");
    assert_eq!(body["refusal"], "not_available");
}

/// `Refusal::Unavailable` carries why the *store* could not answer, and that is
/// for an operator reading logs rather than for whoever is asking.
#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_device_is_refused_without_describing_the_service() {
    let base = serve().await;
    let error = ureq::get(&format!("{base}/directory/challenge?device=not-a-key"))
        .call()
        .expect_err("not a device key");

    let ureq::Error::Status(status, response) = error else {
        panic!("the directory was unreachable rather than answering");
    };
    assert_eq!(status, 400);
    let body: serde_json::Value = response.into_json().expect("json");
    assert_eq!(body["refusal"], "malformed");
    assert_eq!(
        body.as_object().map(serde_json::Map::len),
        Some(1),
        "a refusal carried something beyond its name: {body}"
    );
}
