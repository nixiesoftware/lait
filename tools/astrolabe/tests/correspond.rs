//! Two clients, two identities, one Post — the composition, not the parts.
//!
//! Every piece under this has its own unit test and passes. What has never been
//! exercised is the whole chain in the shape the app actually runs it: two
//! separate identity homes on disk, each founding its own profile from its own
//! keys, exchanging reach cards as a person would paste them, and reaching each
//! other through `Correspondent` — the same enum the runtime holds and the same
//! methods its action handlers call.
//!
//! `CLAUDE.md` records why this file has to exist: the client-to-process seam has
//! been wrong twice, both times with every component correct and the composition
//! wrong. A test that asserted the parts would have passed both times.
//!
//! The Post is real and in-process, so the carriage is HTTP, challenge-response
//! and sealed bytes rather than a mock. `POST_SMOKE_URL` points it at a deployed
//! one instead.

use std::path::Path;
use std::sync::{Arc, Mutex};

use astrolabe::client::correspondence::Correspondent;
use lait_post::http::{router, Shared};
use lait_post::{FsStore, Post};

/// The Post keeps its own clock and refuses an expiry it thinks is unreasonable,
/// so a real carrier has to be driven from real time. A fixed constant here
/// passed every unit test and was refused by the first real deposit.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

async fn serve() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a deposit root");
    if let Ok(remote) = std::env::var("POST_SMOKE_URL") {
        return (remote.trim_end_matches('/').to_owned(), dir);
    }
    let store = FsStore::open(dir.path()).expect("open the store");
    let shared: Shared = Arc::new(Mutex::new(Post::new(store)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(shared)).await;
    });
    (format!("http://127.0.0.1:{port}"), dir)
}

/// One person's machine: a real identity home with a real key on it.
fn machine(root: &Path, who: &str) -> std::path::PathBuf {
    let home = root.join(who);
    std::fs::create_dir_all(&home).expect("home");
    lait::config::load_or_create_identity(&home).expect("identity");
    home
}

/// Stand a client up the way the runtime does, through the same resolution.
///
/// Deliberately `build_correspondent` rather than a hand-built plane: this test
/// exists for the composition, and re-implementing the wiring would leave the
/// wiring untested — including whether `identity_home` resolves the directory
/// the person's keys are actually in.
fn client(home: &Path, base: &str) -> Correspondent {
    let held = astrolabe::runtime::build_correspondent(Some(home), Some(base.to_owned()), false)
        .expect("the runtime connects a hosted plane for this identity");
    held.into_inner().expect("a fresh lock is never poisoned")
}

/// The plane keeps itself as part of announcing and learning, so there is
/// nothing for a caller to remember — and nothing for this test to fake.

fn address(client: &Correspondent) -> String {
    client.snapshot().contacts[0].id.clone()
}

fn bodies_from_others(client: &Correspondent, peer: &str) -> Vec<String> {
    client
        .snapshot()
        .conversations
        .into_iter()
        .find(|c| c.peer_id == peer)
        .map(|c| {
            c.messages
                .into_iter()
                .filter(|m| !m.mine)
                .filter_map(|m| m.body)
                .collect()
        })
        .unwrap_or_default()
}

/// Ada and Grace have never met, share no Space, and reach each other anyway.
#[tokio::test(flavor = "multi_thread")]
async fn two_people_who_share_no_space_exchange_cards_and_then_mail() {
    let (base, _post) = serve().await;
    let root = tempfile::tempdir().expect("root");
    let ada_home = machine(root.path(), "ada");
    let grace_home = machine(root.path(), "grace");

    let mut ada = client(&ada_home, &base);
    let mut grace = client(&grace_home, &base);

    let ada_address = address(&ada);
    let grace_address = address(&grace);
    assert_ne!(
        ada_address, grace_address,
        "two identity homes are two people"
    );

    // The exchange, as a person performs it: each publishes a card, hands it
    // over, and the other takes it in. Nothing else is shared between them.
    let ada_card = ada.announce().expect("Ada publishes her reach");
    let grace_card = grace.announce().expect("Grace publishes hers");
    assert_eq!(
        grace.learn(&ada_card).expect("Grace takes Ada's card"),
        ada_address
    );
    assert_eq!(
        ada.learn(&grace_card).expect("Ada takes Grace's card"),
        grace_address
    );

    // Now a letter, sealed to a device set Grace learned rather than one she
    // was told, deposited over real HTTP and fetched back by its recipient.
    ada.send(&grace_address, "no Space in common", now())
        .expect("Ada writes to Grace");
    grace.collect(now() + 1).expect("Grace asks the Post");

    assert_eq!(
        bodies_from_others(&grace, &ada_address),
        vec!["no Space in common".to_owned()],
        "the letter arrived in Ada's conversation, proven from Ada's device"
    );

    // And back the other way, which is the half a one-directional test misses.
    grace
        .send(&ada_address, "received, and replying", now() + 2)
        .expect("Grace replies");
    ada.collect(now() + 3).expect("Ada asks the Post");
    assert_eq!(
        bodies_from_others(&ada, &grace_address),
        vec!["received, and replying".to_owned()],
    );
}

/// Restarting is not forgetting: the address a person handed out still names
/// them, and a correspondent learned before the restart is still reachable.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_the_address_and_the_correspondent() {
    let (base, _post) = serve().await;
    let root = tempfile::tempdir().expect("root");
    let ada_home = machine(root.path(), "ada");
    let grace_home = machine(root.path(), "grace");

    let mut ada = client(&ada_home, &base);
    let mut grace = client(&grace_home, &base);
    let ada_address = address(&ada);
    let grace_address = address(&grace);

    // Both directions, because filing a letter under its sender needs the
    // recipient to hold that sender's profile — a stranger's first letter has
    // nobody to be filed under and lands in the transcript that always exists.
    let ada_card = ada.announce().expect("publish");
    let grace_card = grace.announce().expect("publish");
    ada.learn(&grace_card).expect("learn");
    grace.learn(&ada_card).expect("learn");

    // The client goes away entirely and comes back from what is on disk.
    drop(ada);
    let mut ada = client(&ada_home, &base);

    assert_eq!(
        address(&ada),
        ada_address,
        "the address Ada handed out still names her"
    );
    ada.send(&grace_address, "still know where you are", now() + 4)
        .expect("a correspondent learned before the restart is still reachable");

    grace.collect(now() + 5).expect("collect");
    assert_eq!(
        bodies_from_others(&grace, &ada_address),
        vec!["still know where you are".to_owned()],
    );
}

/// The property `ReachStore` exists for, proven across a restart.
///
/// A republished card has to *supersede* the one a correspondent already holds.
/// `Registry::absorb` takes a publication only when its epoch is at least the
/// held one and answers `Ok` either way — so an epoch that reset on restart
/// would leave every correspondent on a stale device set while both sides
/// reported success. Nothing about that is visible from one side alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_card_republished_after_a_restart_supersedes_the_one_already_held() {
    let (base, _post) = serve().await;
    let root = tempfile::tempdir().expect("root");
    let ada_home = machine(root.path(), "ada");
    let grace_home = machine(root.path(), "grace");

    let mut ada = client(&ada_home, &base);
    let mut grace = client(&grace_home, &base);
    let ada_address = address(&ada);

    let first = ada.announce().expect("Ada publishes");
    let grace_card = grace.announce().expect("Grace publishes");
    grace.learn(&first).expect("Grace holds the first");
    ada.learn(&grace_card).expect("and Ada holds Grace's");

    // Ada restarts. Her epoch has to come back from disk, not from 1.
    drop(ada);
    let mut ada = client(&ada_home, &base);
    let second = ada.announce().expect("Ada publishes again");
    assert_ne!(first, second, "a republication is a new card");

    grace
        .learn(&second)
        .expect("the second card is taken, not silently ignored as older");

    // Proven by carriage rather than by the return value, because `absorb`
    // answers `Ok` for a publication it discarded.
    ada.send(&address(&grace), "after republishing", now())
        .expect("send");
    grace.collect(now() + 1).expect("collect");
    assert_eq!(
        bodies_from_others(&grace, &ada_address),
        vec!["after republishing".to_owned()],
    );
}

/// Sealed means sealed: a third identity on the same Post learns nothing.
///
/// The first assertion a reader looks for in something called sealed
/// correspondence, and one no unit test in this tree makes at this level.
#[tokio::test(flavor = "multi_thread")]
async fn a_third_identity_on_the_same_post_gets_nothing() {
    let (base, post_root) = serve().await;
    let root = tempfile::tempdir().expect("root");
    let ada_home = machine(root.path(), "ada");
    let grace_home = machine(root.path(), "grace");
    let eve_home = machine(root.path(), "eve");

    let mut ada = client(&ada_home, &base);
    let mut grace = client(&grace_home, &base);
    let mut eve = client(&eve_home, &base);

    let grace_card = grace.announce().expect("publish");
    let ada_card = ada.announce().expect("publish");
    ada.learn(&grace_card).expect("learn");
    grace.learn(&ada_card).expect("learn");

    let secret = "for Grace only";
    ada.send(&address(&grace), secret, now()).expect("send");

    // Eve shares the carrier and holds neither key.
    eve.collect(now() + 1).expect("Eve may ask");
    let seen: Vec<String> = eve
        .snapshot()
        .conversations
        .into_iter()
        .flat_map(|c| c.messages)
        .filter_map(|m| m.body)
        .collect();
    assert!(seen.is_empty(), "a bystander collects nothing: {seen:?}");

    // And the carrier itself is holding ciphertext, not the sentence. Only
    // meaningful against the local Post, whose spool is on this disk.
    if std::env::var("POST_SMOKE_URL").is_err() {
        let mut found = false;
        for entry in walkdir(post_root.path()) {
            let bytes = std::fs::read(&entry).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(secret),
                "the Post is holding the plaintext at {}",
                entry.display()
            );
            found = true;
        }
        assert!(found, "the deposit reached the spool at all");
    }
}

/// Every file under a directory, so the spool can be read for what it holds.
fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// An invitation crosses as an invitation, and arrives as one.
///
/// The carry half of CORR-24. Accepting it is `space_enter`, which needs a real
/// daemon and is proven where daemons are; what is proven here is the half that
/// was silently broken — an invitation was discarded in both projections, so it
/// could never reach a surface from the hosted arm however well it travelled.
#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_crosses_the_post_and_arrives_as_an_invitation() {
    let (base, _post) = serve().await;
    let root = tempfile::tempdir().expect("root");
    let ada_home = machine(root.path(), "ada");
    let grace_home = machine(root.path(), "grace");

    let mut ada = client(&ada_home, &base);
    let mut grace = client(&grace_home, &base);
    let ada_address = address(&ada);
    let grace_address = address(&grace);

    let ada_card = ada.announce().expect("publish");
    let grace_card = grace.announce().expect("publish");
    ada.learn(&grace_card).expect("learn");
    grace.learn(&ada_card).expect("learn");

    // A link as a person receives one. Opaque to everything on this path: the
    // carrier cannot read it, `crates/correspondence` will not decode it, and
    // this client only re-spells it. It verifies at the Space or nowhere.
    let link = "lait://join/aebagbafaydqqcikbmga";
    ada.send_invitation(&grace_address, decode_link(link), now())
        .expect("Ada carries an invitation to Grace");
    grace.collect(now() + 1).expect("Grace asks the Post");

    let arrived: Vec<_> = grace
        .snapshot()
        .conversations
        .into_iter()
        .find(|c| c.peer_id == ada_address)
        .expect("a conversation with Ada")
        .messages
        .into_iter()
        .filter(|m| !m.mine)
        .collect();

    assert_eq!(arrived.len(), 1, "one letter arrived");
    let invitation = &arrived[0];
    assert_eq!(invitation.kind, "invitation", "and it arrived as one");
    assert!(
        invitation.body.is_none(),
        "an invitation is acted on, not read"
    );
    let id = invitation
        .id
        .clone()
        .expect("a received letter names itself");
    assert_eq!(
        grace.invitation(&id).as_deref(),
        Some(decode_link(link).as_slice()),
        "the coordinates crossed intact, which is what lets the Space judge them"
    );
    assert!(
        grace.invitation("iss_not_a_deposit").is_none(),
        "and an id no letter carries names nothing"
    );
}

/// The bare base32 body of an invite link, as bytes.
fn decode_link(link: &str) -> Vec<u8> {
    let body = link.trim().strip_prefix("lait://join/").unwrap_or(link);
    data_encoding::BASE32_NOPAD
        .decode(body.to_uppercase().as_bytes())
        .expect("a base32 link body")
}

/// An address nobody has handed over is *not reachable* — which is a different
/// answer from the message failing, and is the one a surface can act on.
#[tokio::test(flavor = "multi_thread")]
async fn a_stranger_is_not_reachable_rather_than_a_failed_send() {
    let (base, _post) = serve().await;
    let root = tempfile::tempdir().expect("root");
    let ada = machine(root.path(), "ada");
    let stranger = machine(root.path(), "stranger");

    let mut ada = client(&ada, &base);
    let unknown = address(&client(&stranger, &base));

    let refused = ada
        .send(&unknown, "hello?", now())
        .expect_err("an unlearned profile is not reachable");
    assert!(
        !refused.retryable,
        "trying again does not teach us where they are"
    );
    assert!(
        refused.message.contains("reach"),
        "the refusal says what is missing: {}",
        refused.message
    );
}
