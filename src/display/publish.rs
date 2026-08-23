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

use std::path::Path;

use anyhow::{Context, Result};
use lait_directory::registry::{publish_over_http, Label, Resolved, RoutePublish};

pub fn publish_route(
    identity_home: &Path,
    label: &str,
    registry_base: &str,
    endpoint: &str,
) -> Result<Resolved> {
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
    publish_over_http(registry_base, &publish)
}
