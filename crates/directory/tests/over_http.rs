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
    Challenge, Chronicler, MemStore, Service, SignedPublish, SignedResolve,
};
use mechanics::{
    actor::device_from_seed,
    kinship::{Audience, DeviceLink, Standing},
};

/// Serve the directory on an ephemeral port and answer its base URL.
async fn serve() -> String {
    mounted(Service::new(MemStore::new())).await
}

/// The same surface over a directory that chronicles what it accepts.
async fn serve_chronicled() -> String {
    let chronicler = Chronicler::shared(MemStore::new(), [77u8; 32]).expect("open the chronicle");
    mounted(Service::with_chronicler(MemStore::new(), chronicler)).await
}

async fn mounted(service: Service<MemStore>) -> String {
    let shared: Shared<MemStore> = Arc::new(Mutex::new(service));
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

/// The receipt rides beside the address, flattened and all-defaulting, so a
/// client built before the directory chronicled anything decodes `{address}`
/// exactly as it always did. Growing the answer this way is what lets an old
/// daemon keep publishing through a new Post — and a resolution still carries
/// nothing but the bytes, because a receipt there would be a chronicle fact
/// about somebody who did not ask for one to be published.
#[tokio::test(flavor = "multi_thread")]
async fn a_receipt_rides_beside_the_address_and_an_old_client_still_decodes_it() {
    /// The `Published` shape as it was before any of this. Deliberately a
    /// second declaration: asserting the current type against itself would
    /// prove the test compiles, not that the wire stayed compatible.
    #[derive(serde::Deserialize)]
    struct OldPublished {
        address: String,
    }

    let base = serve_chronicled().await;
    let (seed, announcement) = announced(21, 22, 2);
    let request: SignedPublish = sign::publish(
        &seed,
        &challenge(&base, &seed),
        announcement.encode().expect("encode"),
    );
    let body: serde_json::Value = ureq::post(&format!("{base}/directory/publish"))
        .send_json(serde_json::to_value(&request).expect("serialize"))
        .expect("publish")
        .into_json()
        .expect("json");

    let old: OldPublished = serde_json::from_value(body.clone()).expect("an old client decodes");
    let address = Address::parse(&old.address).expect("still just an address");
    assert_eq!(
        body["entry"].as_u64(),
        Some(0),
        "the receipt is flattened beside the address: {body}"
    );
    let marks = body["marks"].as_array().expect("marks rode along");
    assert!(!marks.is_empty(), "a chronicled publication marked nobody");
    assert!(
        body["head"]["by"].is_string(),
        "the head names its signer: {body}"
    );

    // The receipt is the publisher's. A resolution — which anyone holding the
    // address may ask for — still answers the bytes and nothing else, so no
    // chronicle fact about a publisher leaks to whoever looked them up.
    let asker = [90u8; 32];
    let resolve: SignedResolve = sign::resolve(&asker, &challenge(&base, &asker), &address);
    let answered: serde_json::Value = ureq::post(&format!("{base}/directory/resolve"))
        .send_json(serde_json::to_value(&resolve).expect("serialize"))
        .expect("resolve")
        .into_json()
        .expect("json");
    assert_eq!(
        answered.as_object().map(serde_json::Map::len),
        Some(1),
        "a resolution carried something beyond the announcement: {answered}"
    );

    // And an address nobody holds still answers what a withheld one would,
    // from a directory that chronicles. Absence and denial stay one answer.
    let unheld = Address::mint(&[0x37; 16]);
    let request = sign::resolve(&asker, &challenge(&base, &asker), &unheld);
    let error = ureq::post(&format!("{base}/directory/resolve"))
        .send_json(serde_json::to_value(&request).expect("serialize"))
        .expect_err("nobody holds it");
    let ureq::Error::Status(status, response) = error else {
        panic!("the directory was unreachable rather than answering");
    };
    assert_eq!(status, 404);
    let refused: serde_json::Value = response.into_json().expect("json");
    assert_eq!(refused["refusal"], "not_available");
    assert_eq!(
        refused.as_object().map(serde_json::Map::len),
        Some(1),
        "a refusal grew a field a prober could read: {refused}"
    );
}
