//! Web projection of the daemon's Orbit directory.
//!
//! This module owns the browser-specific row shape and passive status probing.
//! Discovery, identity scoping, address resolution, and Station placement live
//! in [`crate::daemon`].

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::control::{ControlRoute, Request, Response};
use crate::daemon::{Client, LocalOrbitId, OrbitAddress};
use crate::orbits::{self, Catalog, Entry, Presence, StationIdentity};
use mechanics::ids::SpaceId;

/// One row of the browser's Orbit picker.
#[derive(Debug, Clone, Serialize)]
pub struct SpaceRow {
    /// Stable local Orbit handle for URLs and control routing.
    pub id: LocalOrbitId,
    /// The replicated Space id expected at this Orbit.
    pub space: String,
    /// Display name at last open (advisory when the Orbit is idle).
    pub name: String,
    pub path: String,
    pub origin: String,
    pub last_opened: u64,
    /// `up` | `idle` | `missing`, as the registry knows it.
    pub status: &'static str,
    pub identity: StationIdentity,
    pub projects: Vec<orbits::ProjectBrief>,
}

/// Probe one Orbit's compatibility control adapter without activating it.
///
/// Listing must remain passive: opening the picker cannot wake every registered
/// Orbit. The authoritative Space name is used when a live Station answers.
async fn status(daemon: &Client, entry: &Entry) -> (&'static str, Option<String>) {
    if orbits::presence(entry) == Presence::Missing {
        return ("missing", None);
    }
    let Some(space) = SpaceId::parse(&entry.space) else {
        return ("idle", None);
    };
    let route = ControlRoute::Orbit {
        address: OrbitAddress::for_store(Path::new(&entry.path), space),
    };
    let reply = tokio::time::timeout(
        Duration::from_millis(300),
        daemon.request_if_running(route, &Request::Status),
    )
    .await;
    match reply {
        Ok(Ok(Response::Status(info))) => {
            let name = (!info.name.trim().is_empty()).then(|| info.name.clone());
            ("up", name)
        }
        Ok(Ok(_)) => ("up", None),
        _ => ("idle", None),
    }
}

/// List visible Orbits newest-first, probing their status concurrently.
pub async fn list(directory: &Catalog, daemon: &Client) -> Vec<SpaceRow> {
    let mut probes = tokio::task::JoinSet::new();
    for binding in directory.bindings() {
        let daemon = daemon.clone();
        probes.spawn(async move {
            let (status, catalog_name) = status(&daemon, &binding.entry).await;
            SpaceRow {
                id: LocalOrbitId::for_store(Path::new(&binding.entry.path)),
                space: binding.entry.space.clone(),
                name: catalog_name.unwrap_or_else(|| binding.entry.name.clone()),
                path: binding.entry.path.clone(),
                origin: binding.entry.origin.to_string(),
                last_opened: binding.entry.last_opened,
                status,
                identity: binding.identity,
                projects: binding.entry.projects.clone(),
            }
        });
    }

    let mut rows = probes.join_all().await;
    rows.sort_by_key(|row| std::cmp::Reverse(row.last_opened));
    rows
}
