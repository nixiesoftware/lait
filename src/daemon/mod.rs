//! Local host-plane types shared by clients and the future LaitDaemon.
//!
//! This module owns no Space state. It names the local Orbit through which a
//! client may reach a Space and keeps the caller's allowed Orbit set separate
//! from the wire request. The current per-home compatibility host consumes the
//! same address; the future daemon broker will resolve it through its catalog.

mod scope;

pub use scope::{ClientScope, LocalOrbitId, OrbitAddress, ScopeDenied};
