//! The daemon says where its identity answers.
//!
//! One act at overlay start: build a lean announcement from the identity's
//! own seeds — the genesis is deterministic, the projection carries no
//! avowal because the registry validates lineage and reads nothing else —
//! sign the route with the identity's first device, and hand it to the
//! registry over HTTP. Blocking, called through `spawn_blocking`.
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

pub fn publish_route(
    identity_home: &Path,
    label: &str,
    registry_base: &str,
    endpoint: &str,
) -> Result<Chronicled> {
    let label = Label::parse(label).map_err(|refusal| {
        anyhow::anyhow!("identity.label is not a publishable label: {refusal}")
    })?;
    let seeds = crate::config::load_or_create_kinship_seeds(identity_home)?;
    let Some(first) = seeds.first().copied() else {
        anyhow::bail!("this identity holds no kinship seed");
    };
    // The same fixed genesis every derivation of this identity uses.
    let genesis = correspondence::plane::ReachPlane::genesis_for(&seeds)
        .map_err(|error| anyhow::anyhow!("derive identity genesis: {error}"))?;
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
    let receipt = publish_over_http(registry_base, &publish)?;
    check_receipt(&publish, &receipt)?;
    ratchet(identity_home, registry_base, &receipt)?;
    Ok(receipt)
}

/// The forward-only half: pin the first head this identity accepts, and
/// thereafter require every later one to prove it extends the pin. The three
/// refusals stay three different sentences, because they are three different
/// facts — a registrar that could not be *asked* is not one that *lied*.
fn ratchet(identity_home: &Path, registry_base: &str, receipt: &Chronicled) -> Result<()> {
    use mechanics::chronicle::{advance, Refusal};

    let Some(head) = &receipt.head else {
        return Ok(());
    };
    let Some(held) = crate::display::pin::load(identity_home) else {
        crate::display::pin::save(identity_home, head);
        return Ok(());
    };
    let pinned = mechanics::chronicle::PinnedHead::from(&held);
    let answer = chronicle_over_http(registry_base, Some(pinned.size))
        .context("the registrar's chronicle could not be asked; the pin holds")?;
    match advance(Some(&pinned), &answer.head, &answer.consistency) {
        Ok(_) => {
            crate::display::pin::save(identity_home, &answer.head);
            Ok(())
        }
        Err(Refusal::Diverged) => {
            crate::display::pin::keep_divergence(identity_home, &held, &answer.head);
            anyhow::bail!(
                "the registrar's chronicle DIVERGED from the head this identity pinned \
                 (both signed heads at size {} disagree) — the registrar equivocated, and \
                 both artifacts are retained beside the identity as evidence",
                pinned.size
            )
        }
        Err(Refusal::Unproven) => anyhow::bail!(
            "the registrar could not prove its chronicle (size {}) extends the pinned head \
             (size {}) — the pin holds",
            answer.head.size,
            pinned.size
        ),
        Err(Refusal::Rollback) => anyhow::bail!(
            "the registrar served a chronicle head older than the pinned one \
             (size {} against {}) — a replayed copy; the pin holds",
            answer.head.size,
            pinned.size
        ),
        Err(refusal) => anyhow::bail!("the registrar's chronicle head did not verify: {refusal}"),
    }
}

/// A receipt without a head is a registrar that keeps no chronicle — allowed
/// while the fleet turns over. A receipt *with* one must prove itself whole.
fn check_receipt(publish: &RoutePublish, receipt: &Chronicled) -> Result<()> {
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

    use lait_directory::registry::{Label, MemRegistry, Registrar, RegistryStore};
    use mechanics::kinship::ProfileId;

    fn home_with_identity() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let hex = data_encoding::HEXLOWER.encode(&[42u8; 32]);
        std::fs::write(dir.path().join("secret.key"), hex).expect("write identity");
        dir
    }

    fn profile_of(home: &std::path::Path) -> ProfileId {
        let seeds = crate::config::load_or_create_kinship_seeds(home).expect("seeds");
        correspondence::plane::ReachPlane::profile_for(&seeds).expect("profile")
    }

    fn endpoint() -> String {
        mechanics::actor::device_from_seed(&[77u8; 32])
            .as_str()
            .to_string()
    }

    async fn spawn_registry(store: MemRegistry, profile: &ProfileId) -> String {
        let mut store = store;
        store
            .bind(&Label::parse("acme").expect("label"), profile)
            .expect("bind");
        let registrar = Registrar::open(store, [51u8; 32]).expect("open");
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
}
