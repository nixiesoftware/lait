//! Product-side bridges into the Worlds hosted by one active Space.
//!
//! [`runtime::WorldRegistry`] owns the immutable semantic implementations used
//! by a Station. This module owns the application-side half of that boundary:
//! a compile-time [`WorldPackage`] for each product, and one [`WorldBridge`] per
//! package inside an active Space. A package carries the reviewed semantic
//! implementation plus its optional product-neutral call handler; orbital code never
//! needs to name the product behind either one.
//!
//! A [`WorldBridgeRegistry`] belongs to one Space bridge. It is not a process,
//! does not own a listener, and has no autonomous background loop.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use mechanics::ids::DeviceId;
use replica::ids::WorldId;
use runtime::registry::RegistrationError;
use runtime::{
    LifecycleError, LocalIdentity, RuntimeBuilder, Session, Station, World, WorldRegistration,
    WorldRegistry,
};

use crate::control::{Request, Response};

pub use ::world_bridge::{
    WorldCall, WorldCallAccess, WorldCallContext, WorldCallError, WorldCallErrorCode,
    WorldCallHandler, WorldReply,
};

/// Temporary translation between the generic World-call boundary and the
/// historical issue-shaped per-Orbit control protocol.
///
/// New products need only [`WorldCallHandler`]. A package supplies this codec
/// only while older standalone SpaceBridge processes for that product remain
/// attachable by LaitDaemon.
pub trait LegacyWorldCodec: Send + Sync {
    /// Whether this codec owns one historical root request.
    fn handles(&self, request: &Request) -> bool;

    /// Translate an old typed request into the product's versioned opaque call.
    fn encode_call(&self, request: Request) -> Result<WorldCall, WorldCallError>;

    /// Translate a generic call for dispatch to an attached historical daemon.
    fn decode_call(&self, call: &WorldCall) -> Result<Request, WorldCallError>;

    /// Wrap a historical response for the generic caller.
    fn encode_reply(&self, call: &WorldCall, response: Response) -> WorldReply {
        match serde_json::to_vec(&response) {
            Ok(payload) => WorldReply::ok(call, payload),
            Err(error) => WorldReply::error(
                call,
                WorldCallErrorCode::Internal,
                format!("encode legacy World response: {error}"),
            ),
        }
    }

    /// Decode a generic reply for an old typed caller.
    fn decode_reply(&self, reply: WorldReply) -> Response {
        match reply.into_result() {
            Ok(payload) => serde_json::from_slice(&payload)
                .unwrap_or_else(|error| Response::err(format!("decode World response: {error}"))),
            Err(error) if error.code == WorldCallErrorCode::Denied => {
                Response::denied(error.message)
            }
            Err(error) => Response::err(error.message),
        }
    }
}

/// One product package available to the application build.
#[derive(Clone)]
pub struct WorldPackage {
    registration: WorldRegistration,
    implementation: Arc<dyn World>,
    reviewed_implementation: [u8; 32],
    control: Option<Arc<dyn WorldCallHandler>>,
    legacy: Option<Arc<dyn LegacyWorldCodec>>,
}

impl std::fmt::Debug for WorldPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldPackage")
            .field("world", &self.registration.id)
            .field(
                "reviewed_implementation",
                &data_encoding::HEXLOWER.encode(&self.reviewed_implementation[..8]),
            )
            .field("has_call_handler", &self.control.is_some())
            .field("has_legacy_codec", &self.legacy.is_some())
            .finish()
    }
}

impl WorldPackage {
    pub fn new(
        registration: WorldRegistration,
        implementation: Arc<dyn World>,
        reviewed_implementation: [u8; 32],
    ) -> Self {
        Self {
            registration,
            implementation,
            reviewed_implementation,
            control: None,
            legacy: None,
        }
    }

    pub fn with_control(mut self, control: Arc<dyn WorldCallHandler>) -> Self {
        self.control = Some(control);
        self
    }

    pub fn with_legacy_codec(mut self, legacy: Arc<dyn LegacyWorldCodec>) -> Self {
        self.legacy = Some(legacy);
        self
    }

    pub fn world_id(&self) -> &WorldId {
        &self.registration.id
    }
}

/// Compile-time composition of the Worlds bundled by one application build.
///
/// The package set is cloned down the LaitDaemon → Station placement →
/// SpaceBridge call stack. Each Space freezes its own Runtime registry and
/// bridge objects from the same reviewed set.
#[derive(Clone, Default)]
pub struct WorldPackages {
    packages: Vec<WorldPackage>,
}

impl WorldPackages {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one reviewed World package.
    ///
    /// Duplicate ids and registration/implementation mismatches are rejected by
    /// the existing [`RuntimeBuilder`] validation when [`Self::build`] freezes
    /// the set.
    pub fn with_package(mut self, package: WorldPackage) -> Self {
        self.packages.push(package);
        self
    }

    /// Add a semantic-only World. Kept for independent Runtime adopters that
    /// need no application control adapter.
    pub fn register(
        self,
        registration: WorldRegistration,
        implementation: Arc<dyn World>,
        reviewed_implementation: [u8; 32],
    ) -> Self {
        self.with_package(WorldPackage::new(
            registration,
            implementation,
            reviewed_implementation,
        ))
    }

    pub fn world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.packages.iter().map(WorldPackage::world_id)
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.packages
            .iter()
            .any(|package| package.world_id() == world)
    }

    pub fn accepts(&self, world: &WorldId, request: &Request) -> bool {
        self.packages
            .iter()
            .find(|package| package.world_id() == world)
            .and_then(|package| package.legacy.as_deref())
            .is_some_and(|legacy| legacy.handles(request))
    }

    pub fn accepts_call(&self, call: &WorldCall) -> bool {
        self.call_access(call).is_ok()
    }

    pub fn call_access(&self, call: &WorldCall) -> Result<WorldCallAccess, WorldCallError> {
        let control = self
            .packages
            .iter()
            .find(|package| package.world_id() == call.world())
            .and_then(|package| package.control.as_deref())
            .ok_or_else(|| {
                WorldCallError::new(
                    WorldCallErrorCode::UnsupportedOperation,
                    format!("World '{}' has no application call handler", call.world()),
                )
            })?;
        control.access(call)
    }

    pub fn legacy_codec(&self, world: &WorldId) -> Option<&dyn LegacyWorldCodec> {
        self.packages
            .iter()
            .find(|package| package.world_id() == world)
            .and_then(|package| package.legacy.as_deref())
    }

    /// Freeze the semantic registry and create one application bridge per
    /// registered World.
    pub fn build(&self) -> Result<(WorldRegistry, WorldBridgeRegistry), RegistrationError> {
        let mut runtime = RuntimeBuilder::new();
        let mut bridges = Vec::with_capacity(self.packages.len());
        for package in &self.packages {
            bridges.push((
                package.registration.id.clone(),
                package.reviewed_implementation,
                package.control.clone(),
                package.legacy.clone(),
            ));
            runtime =
                runtime.register(package.registration.clone(), package.implementation.clone());
        }
        let registry = runtime.build()?;
        Ok((
            registry,
            WorldBridgeRegistry::new(
                bridges
                    .into_iter()
                    .map(|(world, reviewed, control, legacy)| {
                        (
                            world.clone(),
                            WorldBridge::new(world, reviewed, control, legacy),
                        )
                    })
                    .collect(),
            ),
        ))
    }
}

/// Compatibility name for downstream code that built semantic-only World
/// registries before packages also carried application control adapters.
pub type WorldBridgesBuilder = WorldPackages;

/// The sole product-side entrance to one World in one active Space.
///
/// Primary and sponsored-agent Sessions cannot be reused across Worlds because
/// each bridge owns only the Sessions docked to its own [`WorldId`].
pub struct WorldBridge {
    world: WorldId,
    reviewed_implementation: [u8; 32],
    control: Option<Arc<dyn WorldCallHandler>>,
    legacy: Option<Arc<dyn LegacyWorldCodec>>,
    primary_session: Mutex<Option<Session>>,
    agent_sessions: Mutex<HashMap<DeviceId, Session>>,
}

impl std::fmt::Debug for WorldBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldBridge")
            .field("world", &self.world)
            .finish_non_exhaustive()
    }
}

impl WorldBridge {
    fn new(
        world: WorldId,
        reviewed_implementation: [u8; 32],
        control: Option<Arc<dyn WorldCallHandler>>,
        legacy: Option<Arc<dyn LegacyWorldCodec>>,
    ) -> Self {
        Self {
            world,
            reviewed_implementation,
            control,
            legacy,
            primary_session: Mutex::new(None),
            agent_sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn world_id(&self) -> &WorldId {
        &self.world
    }

    pub fn reviewed_implementation(&self) -> &[u8; 32] {
        &self.reviewed_implementation
    }

    pub fn control(&self) -> Option<&dyn WorldCallHandler> {
        self.control.as_deref()
    }

    pub fn legacy_codec(&self) -> Option<&dyn LegacyWorldCodec> {
        self.legacy.as_deref()
    }

    /// Ensure the Space's primary identity has a Session for this World.
    ///
    /// Docking remains lazy: an unadmitted joiner can keep its SpaceBridge
    /// active to drive Contact before Mechanics grants it standing.
    pub fn ensure_primary(
        &self,
        station: &Station,
        identity: &LocalIdentity,
    ) -> Result<(), LifecycleError> {
        let mut session = self.primary_session.lock().expect("World Session lock");
        if session.is_none() {
            *session = Some(station.dock(&self.world, identity)?);
        }
        Ok(())
    }

    pub fn with_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        let session = self.primary_session.lock().expect("World Session lock");
        session.as_ref().map(f)
    }

    /// Dock or reuse a sponsored local agent's Session, then run `f` with it.
    pub fn with_agent<R>(
        &self,
        station: &Station,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, LifecycleError> {
        let mut sessions = self
            .agent_sessions
            .lock()
            .expect("agent World Sessions lock");
        let device = identity.device().clone();
        if !sessions.contains_key(&device) {
            sessions.insert(device.clone(), station.dock(&self.world, identity)?);
        }
        Ok(f(sessions.get(&device).expect("agent Session just docked")))
    }
}

/// The World bridges enabled inside one active SpaceBridge.
pub struct WorldBridgeRegistry {
    bridges: BTreeMap<WorldId, WorldBridge>,
}

impl std::fmt::Debug for WorldBridgeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldBridgeRegistry")
            .field("worlds", &self.bridges.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl WorldBridgeRegistry {
    fn new(bridges: BTreeMap<WorldId, WorldBridge>) -> Self {
        Self { bridges }
    }

    pub fn world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.bridges.keys()
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.bridges.contains_key(world)
    }

    pub fn bridge(&self, world: &WorldId) -> Option<&WorldBridge> {
        self.bridges.get(world)
    }

    pub fn reviewed_implementations(&self) -> impl Iterator<Item = (&WorldId, &[u8; 32])> {
        self.bridges
            .iter()
            .map(|(world, bridge)| (world, bridge.reviewed_implementation()))
    }

    pub fn ensure_primary(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
    ) -> Result<(), LifecycleError> {
        self.bridge(world)
            .ok_or_else(|| LifecycleError::UnknownWorld(world.clone()))?
            .ensure_primary(station, identity)
    }

    pub fn with_primary<R>(&self, world: &WorldId, f: impl FnOnce(&Session) -> R) -> Option<R> {
        self.bridge(world)?.with_primary(f)
    }

    /// Run `f` with any docked primary Session.
    ///
    /// Station observations and authority doorbells are shared across Worlds,
    /// so Space-level adapters need exactly one Session to publish that plane.
    pub fn with_any_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        for bridge in self.bridges.values() {
            let session = bridge.primary_session.lock().expect("World Session lock");
            if let Some(session) = session.as_ref() {
                return Some(f(session));
            }
        }
        None
    }

    pub fn with_agent<R>(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, LifecycleError> {
        self.bridge(world)
            .ok_or_else(|| LifecycleError::UnknownWorld(world.clone()))?
            .with_agent(station, identity, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::{BodySchema, WorldId};
    use runtime::{
        WorldContext, WorldEffect, WorldIntent, WorldLimits, WorldProjection, WorldQuery,
        WorldVersion,
    };

    struct NoopWorld {
        id: WorldId,
        schemas: Vec<BodySchema>,
    }

    struct ProjectControl;

    impl WorldCallHandler for ProjectControl {
        fn access(&self, call: &WorldCall) -> Result<WorldCallAccess, WorldCallError> {
            self.decode_call(call).map(|_| WorldCallAccess::Query)
        }

        fn call(&self, call: &WorldCall, _context: &WorldCallContext<'_>) -> WorldReply {
            self.encode_reply(
                call,
                Response::Projects {
                    projects: Vec::new(),
                },
            )
        }
    }

    impl LegacyWorldCodec for ProjectControl {
        fn handles(&self, request: &Request) -> bool {
            matches!(request, Request::ProjectList)
        }

        fn encode_call(&self, request: Request) -> Result<WorldCall, WorldCallError> {
            if !self.handles(&request) {
                return Err(WorldCallError::new(
                    WorldCallErrorCode::UnsupportedOperation,
                    "not a project request",
                ));
            }
            WorldCall::new(
                WorldId::parse("com.example.notes").unwrap(),
                "projects.control",
                1,
                serde_json::to_vec(&request).unwrap(),
            )
        }

        fn decode_call(&self, call: &WorldCall) -> Result<Request, WorldCallError> {
            if call.operation() != "projects.control" || call.version() != 1 {
                return Err(WorldCallError::new(
                    WorldCallErrorCode::UnsupportedOperation,
                    "unsupported project call",
                ));
            }
            serde_json::from_slice(call.payload()).map_err(|error| {
                WorldCallError::new(
                    WorldCallErrorCode::InvalidCall,
                    format!("decode project request: {error}"),
                )
            })
        }
    }

    impl World for NoopWorld {
        fn id(&self) -> WorldId {
            self.id.clone()
        }

        fn schemas(&self) -> &[BodySchema] {
            &self.schemas
        }

        fn submit(
            &self,
            _ctx: &mut WorldContext<'_>,
            _intent: WorldIntent,
        ) -> Result<WorldEffect, runtime::WorldError> {
            unreachable!("registry tests never execute the World")
        }

        fn query(
            &self,
            _ctx: &WorldContext<'_>,
            _query: WorldQuery,
        ) -> Result<WorldProjection, runtime::WorldError> {
            unreachable!("registry tests never execute the World")
        }
    }

    fn package(id: &str, marker: u8) -> (WorldRegistration, Arc<dyn World>, [u8; 32]) {
        let id = WorldId::parse(id).expect("test World id");
        let schemas = Vec::new();
        (
            WorldRegistration {
                id: id.clone(),
                implementation_version: WorldVersion(1),
                schemas: schemas.clone(),
                limits: WorldLimits::default(),
            },
            Arc::new(NoopWorld { id, schemas }),
            [marker; 32],
        )
    }

    #[test]
    fn one_space_has_one_bridge_per_registered_world() {
        let a = package("com.example.files", 1);
        let b = package("com.example.notes", 2);
        let (registry, bridges) = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1, a.2))
            .with_package(WorldPackage::new(b.0, b.1, b.2))
            .build()
            .unwrap();

        let ids: Vec<_> = bridges.world_ids().map(WorldId::as_str).collect();
        assert_eq!(ids, ["com.example.files", "com.example.notes"]);
        assert_eq!(registry.len(), 2);
        assert_eq!(
            bridges
                .reviewed_implementations()
                .map(|(id, implementation)| (id.as_str(), implementation[0]))
                .collect::<Vec<_>>(),
            [("com.example.files", 1), ("com.example.notes", 2)]
        );
        assert_ne!(
            bridges
                .bridge(&WorldId::parse("com.example.files").unwrap())
                .unwrap() as *const WorldBridge,
            bridges
                .bridge(&WorldId::parse("com.example.notes").unwrap())
                .unwrap() as *const WorldBridge,
            "each World must have a distinct bridge object"
        );
    }

    #[test]
    fn duplicate_worlds_still_fail_through_the_runtime_registry_contract() {
        let a = package("com.example.files", 1);
        let b = package("com.example.files", 2);
        let err = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1, a.2))
            .with_package(WorldPackage::new(b.0, b.1, b.2))
            .build()
            .unwrap_err();
        assert_eq!(
            err,
            RegistrationError::DuplicateWorld(
                WorldId::parse("com.example.files").expect("test World id")
            )
        );
    }

    #[test]
    fn control_claims_are_owned_by_the_registered_package() {
        let files = package("com.example.files", 1);
        let notes = package("com.example.notes", 2);
        let control = Arc::new(ProjectControl);
        let packages = WorldPackages::new()
            .with_package(WorldPackage::new(files.0, files.1, files.2))
            .with_package(
                WorldPackage::new(notes.0, notes.1, notes.2)
                    .with_control(control.clone())
                    .with_legacy_codec(control),
            );
        let files = WorldId::parse("com.example.files").unwrap();
        let notes = WorldId::parse("com.example.notes").unwrap();

        assert!(!packages.accepts(&files, &Request::ProjectList));
        assert!(packages.accepts(&notes, &Request::ProjectList));
        assert!(!packages.accepts(&notes, &Request::Members));
        let (_, bridges) = packages.build().unwrap();
        assert!(bridges.bridge(&files).unwrap().control().is_none());
        assert!(bridges.bridge(&notes).unwrap().control().is_some());
        assert!(bridges.bridge(&notes).unwrap().legacy_codec().is_some());
    }
}
