//! The daemon says where its identity answers.
//!
//! One act at overlay start: build a lean announcement from the identity's
//! carried genesis — the projection carries no avowal because the registry
//! validates lineage and reads nothing else — sign the route with the
//! identity's device, and hand it to the registry over HTTP. Blocking, called
//! through `spawn_blocking`.
//!
//! The epoch is the wall clock in milliseconds: monotonic across restarts
//! without a counter to persist, and the registry's only use for it is
//! refusing rollback.
//!
//! The registrar answers with a chronicle receipt — a signed head and the
//! inclusion path for the entry this publication became. Both are checked
//! here, at the one place the raw answer enters: a receipt that does not
//! verify is a registrar claiming to have recorded something it can not
//! prove it recorded, and that is a refusal, not a warning.

use std::path::Path;

use anyhow::{Context, Result};
use lait_directory::registry::{
    chronicle_entry, publish_over_http, Chronicled, Label, RoutePublish,
};
use lait_directory::Receipt;

pub fn publish_route(
    identity_home: &Path,
    label: &str,
    registry_base: &str,
    endpoint: &str,
) -> Result<Chronicled> {
    let label = Label::parse(label).map_err(|refusal| {
        anyhow::anyhow!("identity.label is not a publishable label: {refusal}")
    })?;
    let first = crate::config::load_identity(identity_home)?;
    // The genesis is read, never derived: the boot founded or carried it
    // before anything served, and a route published under a re-derivation
    // would name a profile no receiver is paired to.
    let Some(genesis) = addressbook::ReachStore::at(identity_home)
        .load()
        .map_err(|error| anyhow::anyhow!("read the kinship store: {error}"))?
        .and_then(|state| state.genesis)
    else {
        anyhow::bail!("this identity carries no genesis; the daemon founds it at boot");
    };
    let log = mechanics::kinship::KinshipLog::found(genesis.clone())
        .map_err(|error| anyhow::anyhow!("found log: {error:?}"))?;
    let profile = log.profile().clone();
    let epoch = mechanics::wallclock::now_millis();
    let projection = log
        .project(&first, epoch, &mechanics::kinship::Standing::default())
        .map_err(|error| anyhow::anyhow!("project: {error:?}"))?;
    let announcement = addressbook::Announcement::new(profile, genesis, projection)
        .encode()
        .context("encode announcement")?;
    let publish = RoutePublish::sign(label, announcement, endpoint.to_string(), epoch, &first);
    let chronicled = publish_over_http(registry_base, &publish)?;
    check_receipt(&publish, &chronicled.receipt)?;
    follow(identity_home, registry_base, &chronicled.receipt)?;
    Ok(chronicled)
}

/// Follow the registrar as a marker: the pin, the ratchet and its distinct
/// refusals live in [`crate::daemon::markers`], which is the identity's one
/// pin store — the display was never the right owner of a fact every other
/// follower of the same log would need, and two stores would have been two
/// answers to "has this service equivocated".
///
/// A refusal fails the publication, as it always has: a route this identity
/// cannot place in the log the registrar claims to have put it in is not a
/// published route, and the daemon logs the sentence and serves anyway.
fn follow(identity_home: &Path, registry_base: &str, receipt: &Receipt) -> Result<()> {
    let entry = crate::daemon::markers::entry_for(identity_home, registry_base);
    // A receipt that now carries *no* head, to an identity that already holds
    // a pin, is a registrar that stopped chronicling after we pinned —
    // suppression, not a fresh start — and must be loud, never a silent Ok.
    if receipt.head.is_none() {
        if let Some(pinned) = crate::daemon::markers::pinned(identity_home, &entry) {
            anyhow::bail!(
                "the registrar answered without a chronicle head, but this identity holds a pin \
                 at size {} — it has stopped proving its memory; the pin holds",
                pinned.size
            );
        }
    }
    match crate::daemon::markers::ratchet(identity_home, &entry, Some(receipt)).refused() {
        Some(why) => anyhow::bail!(why),
        None => Ok(()),
    }
}

/// A receipt without a head is a registrar that keeps no chronicle — allowed
/// while the fleet turns over. A receipt *with* one must prove itself whole.
fn check_receipt(publish: &RoutePublish, receipt: &Receipt) -> Result<()> {
    let Some(head) = &receipt.head else {
        return Ok(());
    };
    head.verify()
        .map_err(|refusal| anyhow::anyhow!("the chronicle receipt's head: {refusal}"))?;
    let Some(entry) = receipt.entry else {
        anyhow::bail!("the chronicle receipt names no entry for this publication");
    };
    let leaf = mechanics::chronicle::Chronicle::leaf_of(&chronicle_entry(publish));
    mechanics::chronicle::verify_inclusion(&leaf, entry, head.size, &head.root, &receipt.inclusion)
        .map_err(|refusal| {
            anyhow::anyhow!("the registrar could not prove it recorded this publication: {refusal}")
        })
}

/// These assert the chain and not the parts, per the rule `launch.rs` states:
/// a real identity home, real HTTP against the mounted registry surface, and
/// the pin moving — or refusing to — on disk where the daemon will read it.
///
/// The dishonest registrars answer **at the base the honest one did**: the pin
/// store is keyed by signer and indexed by base, so a lying registrar on a
/// second port is a second marker rather than the same one caught changing its
/// story. Swapping the registrar behind one listener is what a key rotation, a
/// truncation and an equivocation actually look like from a reader.
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use lait_directory::registry::{ChronicleStore, Label, MemRegistry, Registrar, RegistryStore};
    use lait_directory::Chronicler;
    use mechanics::chronicle::Head;
    use mechanics::kinship::ProfileId;

    type Held = Arc<Mutex<Registrar<MemRegistry>>>;

    fn home_with_identity() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let hex = data_encoding::HEXLOWER.encode(&[42u8; 32]);
        std::fs::write(dir.path().join("secret.key"), hex).expect("write identity");
        dir
    }

    fn profile_of(home: &std::path::Path) -> ProfileId {
        crate::config::identity_profile(home).expect("profile")
    }

    fn endpoint() -> String {
        mechanics::actor::device_from_seed(&[77u8; 32])
            .as_str()
            .to_string()
    }

    /// A registrar over a fresh route store, feeding a chronicle the caller
    /// supplies — so a test can hand it a log with a history of its own — and
    /// signing under a seed the caller names.
    fn registrar(
        chronicle: MemRegistry,
        profile: &ProfileId,
        seed: [u8; 32],
    ) -> Registrar<MemRegistry> {
        let mut store = MemRegistry::default();
        store
            .bind(&Label::parse("acme").expect("label"), profile)
            .expect("bind");
        let chronicler = Chronicler::shared(chronicle, seed).expect("open the chronicle");
        Registrar::with_chronicler(store, chronicler)
    }

    async fn spawn(held: Held) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, lait_directory::registry::router(held))
                .await
                .ok();
        });
        base
    }

    /// The same host, answering as somebody else from now on.
    fn becomes(held: &Held, replacement: Registrar<MemRegistry>) {
        *held.lock().expect("the registrar is not poisoned") = replacement;
    }

    async fn publish(home: std::path::PathBuf, base: String) -> anyhow::Result<()> {
        tokio::task::spawn_blocking(move || {
            // Epochs are wall-clock milliseconds; two publishes in one
            // millisecond read as a replay, which is not what these test.
            std::thread::sleep(std::time::Duration::from_millis(3));
            super::publish_route(&home, "acme", &base, &endpoint()).map(|_| ())
        })
        .await
        .expect("join")
    }

    /// The head this identity holds for the registrar it publishes to, read
    /// back out of the one marker store the way the daemon reads it.
    fn pinned(home: &std::path::Path, base: &str) -> Option<Head> {
        let entry = crate::daemon::markers::entry_for(home, base);
        let by = crate::daemon::markers::pinned(home, &entry)?.by;
        crate::daemon::markers::load(home, &by)?.pin
    }

    fn evidence_against(home: &std::path::Path, seed: [u8; 32]) -> std::path::PathBuf {
        home.join("markers").join(format!(
            "{}.diverged",
            mechanics::actor::device_from_seed(&seed).as_str()
        ))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn publishing_pins_the_head_and_a_second_publish_extends_it() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let held: Held = Arc::new(Mutex::new(registrar(
            MemRegistry::default(),
            &profile,
            [51u8; 32],
        )));
        let base = spawn(held).await;

        publish(home.path().to_path_buf(), base.clone())
            .await
            .expect("first publish");
        let taken = pinned(home.path(), &base).expect("a pin was taken");
        assert_eq!(taken.size, 1);

        publish(home.path().to_path_buf(), base.clone())
            .await
            .expect("second publish");
        let moved = pinned(home.path(), &base).expect("the pin survived");
        assert_eq!(moved.size, 2, "the pin advanced along a proven extension");
    }

    /// The book names the signer, so the *first* answer is checked like every
    /// later one. Without this line a fresh install pins whatever key replies
    /// at a host name — and the recovery from a bad first pin is deleting a
    /// file on every device in the fleet.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pinned_signer_in_the_book_refuses_a_different_signer_at_first_contact() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let held: Held = Arc::new(Mutex::new(registrar(
            MemRegistry::default(),
            &profile,
            [51u8; 32],
        )));
        let base = spawn(held).await;

        let named = mechanics::actor::device_from_seed(&[7u8; 32]);
        std::fs::write(
            home.path().join("config.json"),
            serde_json::json!({ "marks.book": format!("{base}@{}", named.as_str()) }).to_string(),
        )
        .expect("write the book");

        let error = publish(home.path().to_path_buf(), base.clone())
            .await
            .expect_err("a signer the book does not name is refused before anything is pinned");
        assert!(
            error.to_string().contains("different device"),
            "the refusal names the wrong signer: {error}"
        );
        let answered = mechanics::actor::device_from_seed(&[51u8; 32]);
        assert!(
            crate::daemon::markers::load(home.path(), &answered).is_none(),
            "nothing is filed under the device that answered — that is the whole of the trust on \
             first use this removes"
        );
        assert!(
            pinned(home.path(), &base).is_none(),
            "and no head was pinned at all"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registrar_with_a_different_memory_at_the_pinned_size_is_caught() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let held: Held = Arc::new(Mutex::new(registrar(
            MemRegistry::default(),
            &profile,
            [51u8; 32],
        )));
        let base = spawn(held.clone()).await;
        publish(home.path().to_path_buf(), base.clone())
            .await
            .expect("first publish");
        let was = pinned(home.path(), &base).expect("pinned");

        // The same signer, with no memory of what it recorded: after it accepts
        // this publication its chronicle also has size 1 — a different signed
        // head at the pinned size. The constructed equivocation, and the
        // ratchet catches it cold.
        becomes(
            &held,
            registrar(MemRegistry::default(), &profile, [51u8; 32]),
        );
        let error = publish(home.path().to_path_buf(), base.clone())
            .await
            .expect_err("a diverged chronicle is a refusal");
        assert!(
            error.to_string().contains("DIVERGED"),
            "the refusal names the divergence: {error}"
        );
        assert!(
            std::fs::metadata(evidence_against(home.path(), [51u8; 32])).is_ok(),
            "both signed heads were retained as evidence"
        );
        let unmoved = pinned(home.path(), &base).expect("still pinned");
        assert_eq!(unmoved, was, "a caught lie does not move the pin");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registrar_that_cannot_link_to_the_pin_does_not_move_it() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let held: Held = Arc::new(Mutex::new(registrar(
            MemRegistry::default(),
            &profile,
            [51u8; 32],
        )));
        let base = spawn(held.clone()).await;
        publish(home.path().to_path_buf(), base.clone())
            .await
            .expect("first publish");
        let was = pinned(home.path(), &base).expect("pinned");

        // A longer, foreign history: its consistency path is valid for *that*
        // chronicle and still cannot link to the pin. That is suspicion, not
        // proof — a different sentence than DIVERGED, and the same unmoved pin.
        let mut foreign = MemRegistry::default();
        foreign.append_chronicle(0, [1u8; 32]).expect("seed");
        foreign.append_chronicle(1, [2u8; 32]).expect("seed");
        becomes(&held, registrar(foreign, &profile, [51u8; 32]));
        let error = publish(home.path().to_path_buf(), base.clone())
            .await
            .expect_err("an unprovable extension is a refusal");
        assert!(
            error.to_string().contains("could not prove"),
            "the refusal says unproven, not diverged: {error}"
        );
        let unmoved = pinned(home.path(), &base).expect("still pinned");
        assert_eq!(unmoved, was);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registrar_signing_under_a_new_key_is_not_the_holder_you_pinned() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let held: Held = Arc::new(Mutex::new(registrar(
            MemRegistry::default(),
            &profile,
            [51u8; 32],
        )));
        let base = spawn(held.clone()).await;
        publish(home.path().to_path_buf(), base.clone())
            .await
            .expect("first publish");
        let was = pinned(home.path(), &base).expect("pinned");

        // A registrar that serves a well-formed, honestly-extending chronicle
        // but signs it under a *different* device. Without the signer bind
        // this passed as a normal extension and an attacker who minted a key
        // owned the ratchet. It must read as WrongSigner, not Diverged (no
        // accusation against the pinned holder) and never as fine.
        becomes(
            &held,
            registrar(MemRegistry::default(), &profile, [99u8; 32]),
        );
        let error = publish(home.path().to_path_buf(), base.clone())
            .await
            .expect_err("a different signer is refused");
        assert!(
            error.to_string().contains("different device"),
            "the refusal names the wrong signer: {error}"
        );
        assert!(
            std::fs::metadata(evidence_against(home.path(), [51u8; 32])).is_err(),
            "a stranger's head is not equivocation evidence against the pinned holder"
        );
        let unmoved = pinned(home.path(), &base).expect("still pinned");
        assert_eq!(unmoved, was, "a foreign signer does not move the pin");
    }
}
