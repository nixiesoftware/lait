//! Local host-plane types shared by clients and the Daemon.
//!
//! This module owns no product or Space state. [`crate::orbits::Catalog`] discovers
//! durable local Orbit bindings, while [`crate::orbits::Router`] establishes or reuses
//! their [`crate::orbits::Placement`], owns identity-keyed transport hubs, and
//! dispatches the existing control protocol.
//! [`Client`] is the one client entrance to the identity-scoped host;
//! per-home IPC remains only as an internal compatibility adapter.

pub(crate) mod host;
pub(crate) mod scope;
pub(crate) mod transport_hub;

pub use crate::orbits::OrbitDoorbell;
pub use host::run_lait_daemon;
pub use host::{Client, Daemon, OrbitSubscription};
pub(crate) use scope::{ClientScope, LocalOrbitId, OrbitAddress};
