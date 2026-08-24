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
    chronicle_entry, publish_over_http, Chronicled, Label, RoutePublish,
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
    Ok(receipt)
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
