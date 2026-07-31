//! **Runtime** — LAIT's orbital lifecycle.
//!
//! ```text
//! Space
//!   +-- Orbit: one device's durable relationship to the Space
//!         +-- Replica: durable local materialization
//!         +-- Station: that Orbit activated for exclusive local operation
//!               +-- hosted World implementation
//!                     +-- docked Session
//! ```
//!
//! Runtime owns the domain lifecycle: forming/entering/observing/acquiring
//! Orbits, activating them into Stations, hosting Worlds, docking Sessions,
//! Contact policy, and Observation publication. It exposes **no** CRDT, iroh,
//! stream, file, key, ciphertext, mutex, or product request types — those live
//! below the boundary in [`fabric`], [`comms`], and [`mechanics`].
//!
//! An Orbit is the durable relationship and persists while vacant or occupied.
//! The Rust handles encode its exclusive operational lease:
//! [`Orbit::activate`] consumes the vacant Orbit handle and returns a
//! [`Station`]; [`Station::vacate`] consumes the active Station handle and
//! returns a vacant Orbit handle. Those ownership transfers are not an
//! ontological conversion between Orbit and Station.
//!
//! S0 establishes the sealed lifecycle contract surface and a **real, tested**
//! immutable World registry (duplicate registration is rejected). The lifecycle
//! transitions are wired in later stages (Orbit in S2, Station in S3,
//! World/Session/Contact in S5); their signatures here fix ownership and
//! consumption semantics.

pub mod action;
pub mod admission;
pub mod beacon;
pub mod budget;
pub mod contact;
pub mod contact_driver;
pub mod content_host;
pub mod coordinates;
#[cfg(test)]
mod dispatch_tests;
pub mod dto;
pub mod error;
pub mod fetch;
pub mod freight;
pub mod implementation;
pub mod lifecycle;
pub mod live;
pub mod neighbor_presence;
pub mod neighbors;
#[path = "planes.rs"]
pub mod plane;
pub mod plane_driver;
pub mod plane_stream;
pub mod registry;
pub mod session;
pub mod signal;
pub mod store;
pub mod transfer;
pub mod transient;
pub(crate) mod wire;
pub mod world;

pub use action::{ActionError, IdempotencyKey, RequestId, SignedWorldAction, WorldActionHeader};
pub use beacon::{BeaconError, RouteHint, SignedBeacon, VerifiedBeacon};
pub use contact::{
    AccepterEvent, AccepterValidator, ContactFrame, ContactId, ContactWireError, InitiatorReceiver,
    InitiatorState, Offer, Progress, Proof, ReceivedMaterial,
};
pub use contact_driver::{Authority, CommsOptions, GossipOptions, MAX_CONTACTS_IN_FLIGHT};
pub use coordinates::{
    canonical_routes, AdmissionCapability, ApproachRoute, CoordinatesAdmission, CoordinatesError,
    CoordinatesPayload, SignedCoordinates, VerifiedCoordinates,
};
pub use error::{ContactError, DormancyError, LifecycleError, StationExit, WorldError};
pub use lifecycle::{
    ActivationOptions, CancelToken, ContactOutcome, Neighbor, Orbit, OrbitStatus, Reachability,
    RemovalConfirmation, Runtime, Station,
};
pub use neighbor_presence::{
    PresenceAck, PresenceError, PresenceProbe, PRESENCE_ALPN, PRESENCE_PROTOCOL,
};
pub use neighbors::{NeighborRecord, NeighborRegistry, RegistryError, StoredRoute};
pub use registry::{Registry, RuntimeBuilder};
pub use session::{
    CommittedEffect, Observation, ObservationCursor, ObservationStream, ObservationStreamError,
    Session, DEFAULT_OBSERVATION_CAPACITY, MAX_OBSERVATION_CAPACITY,
};
pub use world::{
    AuthorityView, BodyDeclaration, BodyReader, Context, Descriptor, Effect, Intent, Limits,
    LocalIdentity, PrincipalFacts, PrincipalResolution, Projection, Query, Version, World,
};
