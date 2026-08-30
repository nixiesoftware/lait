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
    chronicle_entry, chronicle_over_http, publish_over_http, Chronicled, Label, RoutePublish,
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
    ratchet(identity_home, registry_base, &chronicled.receipt)?;
    Ok(chronicled)
}

/// The forward-only half: pin the first head this identity accepts, and
/// thereafter require every later one to prove it extends the pin. Refusals
/// stay distinct sentences, because they are distinct facts — a registrar
/// that could not be *asked* is not one that *lied*, and a *different signer*
/// is not the pinned holder equivocating.
///
/// The head judged is the one the **chronicle surface** serves, not the one
/// the receipt carried: a receipt head can be minted over a private side
/// branch, so the canonical head is the authority, and the receipt is then
/// checked to sit on that same chain (below).
fn ratchet(identity_home: &Path, registry_base: &str, receipt: &Receipt) -> Result<()> {
    use mechanics::chronicle::{advance, Advance, Refusal};

    let held = crate::display::pin::load(identity_home);

    // Ask for the current head first (no size), so a chronicle now *shorter*
    // than the pin comes back as a head the ratchet reads as Rollback rather
    // than a 404 that would fold into "could not be asked".
    let current = chronicle_over_http(registry_base, None)
        .context("the registrar's chronicle could not be asked; the pin holds")?;

    let Some(held) = held else {
        // Trust on first use. But a registrar that answered a *chronicled*
        // receipt and then serves no head is not a fresh pin — it is one
        // suppressing the ratchet, so a receipt head with no pin still pins.
        let first = receipt.head.as_ref().unwrap_or(&current.head);
        first
            .verify()
            .map_err(|refusal| anyhow::anyhow!("the chronicle head did not verify: {refusal}"))?;
        crate::display::pin::save(identity_home, first);
        return Ok(());
    };
    let pinned = mechanics::chronicle::PinnedHead::from(&held);

    // A pin is held. A receipt that now carries *no* head is a registrar that
    // stopped chronicling after we pinned — suppression, not a fresh start —
    // and must be loud, never a silent Ok.
    if receipt.head.is_none() {
        anyhow::bail!(
            "the registrar answered without a chronicle head, but this identity holds a pin \
             at size {} — it has stopped proving its memory; the pin holds",
            pinned.size
        );
    }

    // If the current head already covers the pin, fetch the consistency path
    // from the pin's size; advance judges rollback/divergence/extension.
    let consistency = if current.head.size > pinned.size {
        chronicle_over_http(registry_base, Some(pinned.size))
            .context("the registrar's chronicle could not be asked; the pin holds")?
            .consistency
    } else {
        Vec::new()
    };

    let outcome = advance(Some(&pinned), &current.head, &consistency);
    match outcome {
        Ok(Advance::Unchanged) => Ok(()),
        Ok(_) => {
            // The head is honest against the pin. Now bind the receipt to it:
            // the entry we just published must sit on the canonical chronicle,
            // not on a side branch the receipt could have been minted over.
            reconcile_receipt(receipt, registry_base)?;
            crate::display::pin::save(identity_home, &current.head);
            Ok(())
        }
        Err(Refusal::Diverged) => {
            crate::display::pin::keep_divergence(identity_home, &held, &current.head);
            anyhow::bail!(
                "the registrar's chronicle DIVERGED from the head this identity pinned \
                 (both signed by the pinned device at size {}, different roots) — the \
                 registrar equivocated, and both artifacts are retained beside the identity \
                 as evidence",
                pinned.size
            )
        }
        Err(Refusal::WrongSigner) => anyhow::bail!(
            "the registrar's chronicle head is signed by a different device than the one this \
             identity pinned — it is not the holder you followed; the pin holds"
        ),
        Err(Refusal::Unproven) => anyhow::bail!(
            "the registrar could not prove its chronicle (size {}) extends the pinned head \
             (size {}) — the pin holds",
            current.head.size,
            pinned.size
        ),
        Err(Refusal::Rollback) => anyhow::bail!(
            "the registrar served a chronicle head older than the pinned one \
             (size {} against {}) — a replayed or truncated copy; the pin holds",
            current.head.size,
            pinned.size
        ),
        Err(refusal) => anyhow::bail!("the registrar's chronicle head did not verify: {refusal}"),
    }
}

/// Bind the just-published entry to the registrar's *canonical* chronicle.
///
/// `check_receipt` already proved the entry is included under the receipt's
/// own head; this proves that head is on the public chronicle, not a private
/// side branch. Treat the receipt head as a mini-pin and ask the chronicle
/// surface to prove its current head extends it: a genuine receipt head is a
/// prefix of the canonical log (the consistency proof validates), while a
/// side-branch head that includes the entry cannot be a prefix of a canonical
/// log that omits it, so the proof fails and the false receipt is caught. A
/// canonical head *behind* the receipt's claimed size is a rollback, also
/// caught. Same-signer is required by `advance`, so a receipt signed by a
/// different key than the public head fails too.
fn reconcile_receipt(receipt: &Receipt, registry_base: &str) -> Result<()> {
    use mechanics::chronicle::{advance, Advance, PinnedHead};

    let Some(receipt_head) = &receipt.head else {
        return Ok(());
    };
    let as_pin = PinnedHead::from(receipt_head);
    let answer = chronicle_over_http(registry_base, Some(receipt_head.size))
        .context("the registrar's chronicle could not be asked to place the receipt")?;
    match advance(Some(&as_pin), &answer.head, &answer.consistency) {
        Ok(Advance::Unchanged | Advance::Extended | Advance::Pinned) => Ok(()),
        Ok(other) => anyhow::bail!("unexpected receipt reconciliation outcome: {other:?}"),
        Err(err) => anyhow::bail!(
            "the receipt's head is not on the registrar's canonical chronicle ({err}) — the \
             registrar proved a recording its public log does not contain"
        ),
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
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use lait_directory::registry::{ChronicleStore, Label, MemRegistry, Registrar, RegistryStore};
    use lait_directory::Chronicler;
    use mechanics::kinship::ProfileId;

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

    async fn spawn_registry(chronicle: MemRegistry, profile: &ProfileId) -> String {
        spawn_registry_signed(chronicle, profile, [51u8; 32]).await
    }

    /// A registrar over a fresh route store, feeding a chronicle the caller
    /// supplies — so a test can hand it a log with a history of its own.
    async fn spawn_registry_signed(
        chronicle: MemRegistry,
        profile: &ProfileId,
        seed: [u8; 32],
    ) -> String {
        let mut store = MemRegistry::default();
        store
            .bind(&Label::parse("acme").expect("label"), profile)
            .expect("bind");
        let chronicler = Chronicler::shared(chronicle, seed).expect("open the chronicle");
        let registrar = Registrar::with_chronicler(store, chronicler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let held = Arc::new(Mutex::new(registrar));
        tokio::spawn(async move {
            axum::serve(listener, lait_directory::registry::router(held))
                .await
                .ok();
        });
        base
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

    #[tokio::test(flavor = "multi_thread")]
    async fn publishing_pins_the_head_and_a_second_publish_extends_it() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let base = spawn_registry(MemRegistry::default(), &profile).await;

        publish(home.path().to_path_buf(), base.clone())
            .await
            .expect("first publish");
        let pinned = crate::display::pin::load(home.path()).expect("a pin was taken");
        assert_eq!(pinned.size, 1);

        publish(home.path().to_path_buf(), base)
            .await
            .expect("second publish");
        let moved = crate::display::pin::load(home.path()).expect("the pin survived");
        assert_eq!(moved.size, 2, "the pin advanced along a proven extension");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registrar_with_a_different_memory_at_the_pinned_size_is_caught() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let honest = spawn_registry(MemRegistry::default(), &profile).await;
        publish(home.path().to_path_buf(), honest)
            .await
            .expect("first publish");
        let held = crate::display::pin::load(home.path()).expect("pinned");

        // A second registrar with the same binding and no shared history:
        // after it accepts this publication its chronicle also has size 1 —
        // a different signed head at the pinned size. The constructed
        // equivocation, and the ratchet catches it cold.
        let forked = spawn_registry(MemRegistry::default(), &profile).await;
        let error = publish(home.path().to_path_buf(), forked)
            .await
            .expect_err("a diverged chronicle is a refusal");
        assert!(
            error.to_string().contains("DIVERGED"),
            "the refusal names the divergence: {error}"
        );
        assert!(
            std::fs::metadata(home.path().join("registry-chronicle.diverged")).is_ok(),
            "both signed heads were retained as evidence"
        );
        let unmoved = crate::display::pin::load(home.path()).expect("still pinned");
        assert_eq!(unmoved, held, "a caught lie does not move the pin");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registrar_that_cannot_link_to_the_pin_does_not_move_it() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let honest = spawn_registry(MemRegistry::default(), &profile).await;
        publish(home.path().to_path_buf(), honest)
            .await
            .expect("first publish");
        let held = crate::display::pin::load(home.path()).expect("pinned");

        // A registrar with a longer, foreign history: its consistency path is
        // valid for *its* chronicle and still cannot link to the pin. That is
        // suspicion, not proof — a different sentence than DIVERGED, and the
        // same unmoved pin.
        let mut foreign = MemRegistry::default();
        foreign.append_chronicle(0, [1u8; 32]).expect("seed");
        foreign.append_chronicle(1, [2u8; 32]).expect("seed");
        let rewritten = spawn_registry(foreign, &profile).await;
        let error = publish(home.path().to_path_buf(), rewritten)
            .await
            .expect_err("an unprovable extension is a refusal");
        assert!(
            error.to_string().contains("could not prove"),
            "the refusal says unproven, not diverged: {error}"
        );
        let unmoved = crate::display::pin::load(home.path()).expect("still pinned");
        assert_eq!(unmoved, held);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registrar_signing_under_a_new_key_is_not_the_holder_you_pinned() {
        let home = home_with_identity();
        let profile = profile_of(home.path());
        let honest = spawn_registry_signed(MemRegistry::default(), &profile, [51u8; 32]).await;
        publish(home.path().to_path_buf(), honest)
            .await
            .expect("first publish");
        let held = crate::display::pin::load(home.path()).expect("pinned");

        // A registrar that serves a well-formed, honestly-extending chronicle
        // but signs it under a *different* device. Without the signer bind
        // this passed as a normal extension and an attacker who minted a key
        // owned the ratchet. It must read as WrongSigner, not Diverged (no
        // accusation against the pinned holder) and never as fine.
        let usurper = spawn_registry_signed(MemRegistry::default(), &profile, [99u8; 32]).await;
        let error = publish(home.path().to_path_buf(), usurper)
            .await
            .expect_err("a different signer is refused");
        assert!(
            error.to_string().contains("different device"),
            "the refusal names the wrong signer: {error}"
        );
        assert!(
            std::fs::metadata(home.path().join("registry-chronicle.diverged")).is_err(),
            "a stranger's head is not equivocation evidence against the pinned holder"
        );
        let unmoved = crate::display::pin::load(home.path()).expect("still pinned");
        assert_eq!(unmoved, held, "a foreign signer does not move the pin");
    }
}
