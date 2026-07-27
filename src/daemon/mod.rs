//! Local host-plane types shared by clients and the LaitDaemon.
//!
//! This module owns no product or Space state. [`OrbitDirectory`] discovers
//! durable local Orbit bindings, while [`ControlRouter`] establishes or reuses
//! their [`StationPlacement`], owns identity-keyed transport hubs, and
//! dispatches the existing control protocol.
//! [`LaitDaemonClient`] is the one client entrance to the identity-scoped host;
//! per-home IPC remains only as an internal compatibility adapter.

mod control_router;
mod directory;
mod host;
mod scope;
mod transport_hub;

pub use control_router::{ControlRouter, OrbitDoorbell, PlacementHost, StationPlacement};
pub use directory::{OrbitBinding, OrbitDirectory, ResolvedOrbit, StationIdentity};
pub use host::{run_lait_daemon, LaitDaemonClient, OrbitSubscription};
pub use scope::{ClientScope, LocalOrbitId, OrbitAddress, ScopeDenied};
