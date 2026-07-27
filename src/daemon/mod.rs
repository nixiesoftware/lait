//! Local host-plane types shared by clients and the LaitDaemon.
//!
//! This module owns no product or Space state. [`OrbitDirectory`] discovers
//! durable local Orbit bindings, while [`ControlRouter`] establishes or reuses
//! their [`StationPlacement`] and dispatches the existing control protocol.
//! Process hosting and per-home IPC remain compatibility adapters.

mod control_router;
mod directory;
mod scope;

pub use control_router::{ControlRouter, OrbitDoorbell, PlacementHost, StationPlacement};
pub use directory::{OrbitBinding, OrbitDirectory, ResolvedOrbit, StationIdentity};
pub use scope::{ClientScope, LocalOrbitId, OrbitAddress, ScopeDenied};
