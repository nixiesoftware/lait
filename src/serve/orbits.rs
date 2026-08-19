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

/// Why a row carries no name.
///
/// A picker that cannot name a Space must say which of these it hit. They are
/// different facts and only one of them is worth acting on: a store that is
/// gone needs attention, a Station that is merely down needs a click, and a
/// Space that is genuinely unnamed needs nothing at all.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Unnamed {
    /// The registered path no longer holds a Space store.
    StoreMissing,
    /// No Station answered, so the name was never read.
    NotProbed,
    /// A Station answered but its World is not docked — it has no name to give.
    NotDocked,
}

/// One row of the browser's Orbit picker.
#[derive(Debug, Clone, Serialize)]
pub struct SpaceRow {
    /// Stable local Orbit handle for URLs and control routing.
    pub id: LocalOrbitId,
    /// The replicated Space id expected at this Orbit.
    pub space: String,
    /// The Catalog name, read from a live Station — `None` when it could not be
    /// read, and [`Self::unnamed`] then says why.
    ///
    /// There is deliberately no fallback. The registry remembers the name this
    /// device saw at founding, and serving that when the probe misses is what
    /// made a renamed Space answer to its birth name indefinitely, with nothing
    /// on the wire to mark the difference.
    pub name: Option<String>,
    /// Set when `name` is `None`. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unnamed: Option<Unnamed>,
    pub path: String,
    pub origin: String,
    pub last_opened: u64,
    /// `up` | `idle` | `missing`, as the registry knows it.
    pub status: &'static str,
    pub identity: StationIdentity,
}

/// Probe one Orbit's compatibility control adapter without activating it.
///
/// Listing must remain passive: opening the picker cannot wake every registered
/// Orbit. The Catalog name is read when a live Station answers, and every path
/// that fails to read one says which failure it was — the four of them used to
/// collapse into a single `None` that the caller could only paper over.
async fn status(daemon: &Client, entry: &Entry) -> (&'static str, Result<String, Unnamed>) {
    if orbits::presence(entry) == Presence::Missing {
        return ("missing", Err(Unnamed::StoreMissing));
    }
    let Some(space) = SpaceId::parse(&entry.space) else {
        return ("idle", Err(Unnamed::NotProbed));
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
        // An undocked Station reports `name_unavailable`; its blank name is the
        // absence of a reading, not a Space without a name. A docked one is
        // believed even when the name is empty — that is a measurement.
        Ok(Ok(Response::Status(info))) if info.name_unavailable => ("up", Err(Unnamed::NotDocked)),
        Ok(Ok(Response::Status(info))) => ("up", Ok(info.name.clone())),
        Ok(Ok(_)) => ("up", Err(Unnamed::NotProbed)),
        _ => ("idle", Err(Unnamed::NotProbed)),
    }
}

/// List visible Orbits newest-first, probing their status concurrently.
pub async fn list(directory: &Catalog, daemon: &Client) -> Vec<SpaceRow> {
    let mut probes = tokio::task::JoinSet::new();
    for binding in directory.bindings() {
        let daemon = daemon.clone();
        probes.spawn(async move {
            let (status, catalog_name) = status(&daemon, &binding.entry).await;
            let (name, unnamed) = match catalog_name {
                Ok(name) => (Some(name), None),
                Err(why) => (None, Some(why)),
            };
            SpaceRow {
                id: LocalOrbitId::for_store(Path::new(&binding.entry.path)),
                space: binding.entry.space.clone(),
                name,
                unnamed,
                path: binding.entry.path.clone(),
                origin: binding.entry.origin.to_string(),
                last_opened: binding.entry.last_opened,
                status,
                identity: binding.identity,
            }
        });
    }

    let mut rows = probes.join_all().await;
    rows.sort_by_key(|row| std::cmp::Reverse(row.last_opened));
    rows
}
