//! Product-neutral application calls into hosted Worlds.
//!
//! Runtime's [`runtime::world::World`] contract describes semantic intents and
//! projections. This crate describes the outer application boundary: a
//! versioned, opaque call addressed to one World, the matching reply, and the
//! object-safe handler a compile-time product package supplies.
//!
//! The payload codec belongs to the product. A host can therefore route a
//! World it does not understand without importing that World's request or
//! response types.

use std::fmt;

use crate::session::Session;
use crate::world::LocalIdentity;
use replica::body::WorldId;
use serde::{de, ser::SerializeStruct, Deserialize, Deserializer, Serialize, Serializer};

/// Maximum decoded request payload accepted by the local World-call boundary.
///
/// The semantic Runtime applies its own, usually tighter, per-World limits
/// after a product handler decodes this application request.
pub const MAX_WORLD_CALL_PAYLOAD: usize = 2 * 1024 * 1024;

/// Maximum decoded response payload accepted by the local World-call boundary.
///
/// Projections can legitimately be larger than one semantic action payload.
pub const MAX_WORLD_REPLY_PAYLOAD: usize = 64 * 1024 * 1024;

const MAX_OPERATION_LEN: usize = 128;
const MAX_ENCODED_PAYLOAD: usize = (MAX_WORLD_REPLY_PAYLOAD * 4 / 3) + 4;
const MAX_ENCODED_CALL_PAYLOAD: usize = (MAX_WORLD_CALL_PAYLOAD * 4 / 3) + 4;

/// Opaque product bytes encoded as unpadded URL-safe base64 on JSON wires.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct OpaquePayload(Vec<u8>);

impl OpaquePayload {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Failure> {
        if bytes.len() > MAX_WORLD_REPLY_PAYLOAD {
            return Err(Failure::new(
                Code::InvalidCall,
                format!(
                    "World payload is {} bytes; limit is {}",
                    bytes.len(),
                    MAX_WORLD_REPLY_PAYLOAD
                ),
            ));
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl fmt::Debug for OpaquePayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OpaquePayload")
            .field(&format_args!("{} bytes", self.0.len()))
            .finish()
    }
}

impl Serialize for OpaquePayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&data_encoding::BASE64URL_NOPAD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for OpaquePayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PayloadVisitor;

        impl de::Visitor<'_> for PayloadVisitor {
            type Value = OpaquePayload;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an unpadded URL-safe base64 World payload")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_ENCODED_PAYLOAD {
                    return Err(E::custom(format!(
                        "encoded World payload exceeds {MAX_ENCODED_PAYLOAD} bytes"
                    )));
                }
                let bytes = data_encoding::BASE64URL_NOPAD
                    .decode(value.as_bytes())
                    .map_err(E::custom)?;
                OpaquePayload::new(bytes).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(PayloadVisitor)
    }
}

/// One product-defined application operation addressed to a World.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Call {
    world: WorldId,
    operation: String,
    version: u32,
    payload: OpaquePayload,
}

impl<'de> Deserialize<'de> for Call {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireCall {
            world: WorldId,
            operation: String,
            version: u32,
            payload: String,
        }

        let wire = WireCall::deserialize(deserializer)?;
        if wire.payload.len() > MAX_ENCODED_CALL_PAYLOAD {
            return Err(de::Error::custom(format!(
                "encoded World call payload exceeds {MAX_ENCODED_CALL_PAYLOAD} bytes"
            )));
        }
        let payload = data_encoding::BASE64URL_NOPAD
            .decode(wire.payload.as_bytes())
            .map_err(de::Error::custom)?;
        Call::new(wire.world, wire.operation, wire.version, payload).map_err(de::Error::custom)
    }
}

impl Call {
    pub fn new(
        world: WorldId,
        operation: impl Into<String>,
        version: u32,
        payload: Vec<u8>,
    ) -> Result<Self, Failure> {
        let call = Self {
            world,
            operation: operation.into(),
            version,
            payload: OpaquePayload::new(payload)?,
        };
        call.validate()?;
        Ok(call)
    }

    pub fn world(&self) -> &WorldId {
        &self.world
    }

    /// The same call, addressed to `world`.
    ///
    /// The host's re-keying seam. A product encodes a call addressed to the
    /// id its tree declares, because that is the only id its code knows; the
    /// host serves a local World under an id it assigned, and the runner —
    /// told that id by the launcher — refuses a call addressed any other way.
    /// Re-addressing is the same decision `served_world` makes, made by the
    /// party that knows the assignment.
    pub fn readdressed(mut self, world: WorldId) -> Self {
        self.world = world;
        self
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn payload(&self) -> &[u8] {
        self.payload.as_bytes()
    }

    pub fn validate(&self) -> Result<(), Failure> {
        let valid_operation = !self.operation.is_empty()
            && self.operation.len() <= MAX_OPERATION_LEN
            && self.operation.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            });
        if !valid_operation {
            return Err(Failure::new(
                Code::InvalidCall,
                format!(
                    "World operation must be 1..={MAX_OPERATION_LEN} lowercase ASCII \
                     letters, digits, '.', '_' or '-'"
                ),
            ));
        }
        if self.version == 0 {
            return Err(Failure::new(
                Code::InvalidCall,
                "World call version must be non-zero",
            ));
        }
        if self.payload.as_bytes().len() > MAX_WORLD_CALL_PAYLOAD {
            return Err(Failure::new(
                Code::InvalidCall,
                format!(
                    "World call payload is {} bytes; limit is {}",
                    self.payload.as_bytes().len(),
                    MAX_WORLD_CALL_PAYLOAD
                ),
            ));
        }
        Ok(())
    }
}

/// Whether a validated product call only reads or may author.
///
/// The registered handler derives this from the decoded product request. A host
/// never trusts a client-supplied read/write claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Query,
    Command,
}

/// Stable host-level failure classes. Product errors belong in a successful
/// payload and retain the product's own response schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    InvalidCall,
    UnsupportedOperation,
    UnsupportedVersion,
    Denied,
    Unavailable,
    Internal,
}

/// A failure to validate or host an application call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: Code,
    diagnostic: String,
}

impl Failure {
    pub fn new(code: Code, diagnostic: impl Into<String>) -> Self {
        Self {
            code,
            diagnostic: diagnostic.into(),
        }
    }

    /// The stable label of a failure class, for a diagnostic that says nothing.
    fn label(code: Code) -> &'static str {
        match code {
            Code::InvalidCall => "invalid call",
            Code::UnsupportedOperation => "unsupported operation",
            Code::UnsupportedVersion => "unsupported version",
            Code::Denied => "denied",
            Code::Unavailable => "unavailable",
            Code::Internal => "internal failure",
        }
    }

    /// The diagnostic the raising site wrote, or the class label when it wrote
    /// none. Every raise site names what actually went wrong — an address
    /// mismatch, a wrong World, an oversized payload — and collapsing them all
    /// to the class label is what made every refusal read as `invalid call`.
    pub fn message(&self) -> &str {
        if self.diagnostic.is_empty() {
            Self::label(self.code)
        } else {
            &self.diagnostic
        }
    }
}

impl Serialize for Failure {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("Failure", 2)?;
        state.serialize_field("code", &self.code)?;
        state.serialize_field("message", self.message())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Failure {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            code: Code,
            message: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            code: wire.code,
            diagnostic: wire.message,
        })
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Failure {}

/// One reply bound to the exact World operation and payload version requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    world: WorldId,
    operation: String,
    version: u32,
    #[serde(flatten)]
    outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Ok { payload: OpaquePayload },
    Error { error: Failure },
}

impl Reply {
    pub fn ok(call: &Call, payload: Vec<u8>) -> Self {
        match OpaquePayload::new(payload) {
            Ok(payload) => Self {
                world: call.world.clone(),
                operation: call.operation.clone(),
                version: call.version,
                outcome: Outcome::Ok { payload },
            },
            Err(error) => Self::error(call, Code::Internal, error.message()),
        }
    }

    pub fn error(call: &Call, code: Code, message: impl Into<String>) -> Self {
        Self {
            world: call.world.clone(),
            operation: call.operation.clone(),
            version: call.version,
            outcome: Outcome::Error {
                error: Failure::new(code, message),
            },
        }
    }

    /// Whether this reply carries a payload rather than an error.
    ///
    /// Exposed so a host can ask "did anything happen" without consuming the
    /// reply, which `into_result` would.
    pub fn succeeded(&self) -> bool {
        matches!(self.outcome, Outcome::Ok { .. })
    }

    pub fn validate_for(&self, call: &Call) -> Result<(), Failure> {
        if self.world != call.world
            || self.operation != call.operation
            || self.version != call.version
        {
            return Err(Failure::new(
                Code::Internal,
                "World handler returned a reply for a different call contract",
            ));
        }
        if let Outcome::Ok { payload } = &self.outcome {
            if payload.as_bytes().len() > MAX_WORLD_REPLY_PAYLOAD {
                return Err(Failure::new(
                    Code::Internal,
                    "World handler returned an oversized reply",
                ));
            }
        }
        Ok(())
    }

    pub fn into_result(self) -> Result<Vec<u8>, Failure> {
        match self.outcome {
            Outcome::Ok { payload } => Ok(payload.into_bytes()),
            Outcome::Error { error } => Err(error),
        }
    }

    /// The reply's parts, for a transport that carries the payload *beside* its
    /// header rather than inside it.
    ///
    /// [`Serialize`] puts the payload in the encoded form, which costs a base64
    /// pass and a third more bytes — right for a format with no way to say
    /// "bytes", wrong for a framed channel that can simply declare a length and
    /// then write them. This is the seam that lets such a channel exist without
    /// the payload's opacity leaking: the parts are still bytes to everyone who
    /// touches them.
    pub fn into_parts(self) -> (WorldId, String, u32, Result<Vec<u8>, Failure>) {
        let outcome = match self.outcome {
            Outcome::Ok { payload } => Ok(payload.into_bytes()),
            Outcome::Error { error } => Err(error),
        };
        (self.world, self.operation, self.version, outcome)
    }

    /// Rebuild a reply a transport took apart. The inverse of
    /// [`Reply::into_parts`], and it re-applies the payload bound rather than
    /// trusting a length that arrived over a wire.
    pub fn from_parts(
        world: WorldId,
        operation: String,
        version: u32,
        outcome: Result<Vec<u8>, Failure>,
    ) -> Result<Self, Failure> {
        let outcome = match outcome {
            Ok(payload) => Outcome::Ok {
                payload: OpaquePayload::new(payload)?,
            },
            Err(error) => Outcome::Error { error },
        };
        Ok(Self {
            world,
            operation,
            version,
            outcome,
        })
    }
}

/// Principal facts supplied to a World's application handler.
///
/// This is deliberately smaller than [`runtime::world::Context`]. The handler
/// can resolve user-facing input and sign through the supplied Session, but it
/// receives no Mechanics, Replica, transport, or storage handle.
pub trait SessionAccess: Send + Sync {
    fn principal_facts(&self) -> Result<crate::world::PrincipalFacts, crate::world::Rejection>;
    fn space_id(&self) -> &mechanics::ids::SpaceId;
    fn world_id(&self) -> &WorldId;
    fn submit(
        &self,
        action: crate::world::SignedWorldAction,
    ) -> Result<crate::world::CommittedEffect, crate::world::Failure>;
    fn submit_lifecycle_from(
        &self,
        action: crate::world::SignedWorldAction,
        source: crate::world::LifecycleSourceCoordinate,
    ) -> Result<crate::world::CommittedEffect, crate::world::Failure>;
    fn query(
        &self,
        query: crate::world::Query,
    ) -> Result<crate::world::Projection, crate::world::Failure>;
    fn query_at(
        &self,
        publication: crate::publication::WorldPublicationId,
        query: crate::world::Query,
    ) -> Result<crate::world::Projection, crate::world::Failure>;
    fn find(&self, query: crate::find::Query) -> Result<crate::find::Answer, crate::find::Failure>;
    fn find_at(
        &self,
        publication: crate::publication::WorldPublicationId,
        query: crate::find::Query,
    ) -> Result<crate::find::Answer, crate::find::Failure>;
    fn operation_status_for(
        &self,
        operation: crate::world::RequestId,
        intent: &crate::world::Intent,
    ) -> Result<crate::world::OperationStatus, crate::world::Failure>;
    fn with_lifecycle_source(
        &self,
        source: &crate::world::LifecycleSourceCoordinate,
        prepare: &mut dyn FnMut(
            &crate::world::Context<'_>,
        ) -> Result<Vec<u8>, crate::world::Rejection>,
    ) -> Result<Result<Vec<u8>, crate::world::Rejection>, crate::world::Failure>;

    /// The concrete in-process Session, available only to Runtime's own
    /// identity adapter. An independently hosted context returns `None`; its
    /// identity signs by asking the authoritative host over the runner seam.
    #[doc(hidden)]
    fn local_session(&self) -> Option<&Session> {
        None
    }
}

impl SessionAccess for Session {
    fn principal_facts(&self) -> Result<crate::world::PrincipalFacts, crate::world::Rejection> {
        self.fresh_principal()
    }
    fn space_id(&self) -> &mechanics::ids::SpaceId {
        self.space_id()
    }

    fn world_id(&self) -> &WorldId {
        self.world_id()
    }

    fn submit(
        &self,
        action: crate::world::SignedWorldAction,
    ) -> Result<crate::world::CommittedEffect, crate::world::Failure> {
        self.submit(action)
    }

    fn submit_lifecycle_from(
        &self,
        action: crate::world::SignedWorldAction,
        source: crate::world::LifecycleSourceCoordinate,
    ) -> Result<crate::world::CommittedEffect, crate::world::Failure> {
        self.submit_lifecycle_from(action, source)
    }

    fn query(
        &self,
        query: crate::world::Query,
    ) -> Result<crate::world::Projection, crate::world::Failure> {
        self.query(query)
    }

    fn query_at(
        &self,
        publication: crate::publication::WorldPublicationId,
        query: crate::world::Query,
    ) -> Result<crate::world::Projection, crate::world::Failure> {
        self.query_at(publication, query)
    }

    fn find(&self, query: crate::find::Query) -> Result<crate::find::Answer, crate::find::Failure> {
        self.find(query)
    }

    fn find_at(
        &self,
        publication: crate::publication::WorldPublicationId,
        query: crate::find::Query,
    ) -> Result<crate::find::Answer, crate::find::Failure> {
        self.find_at(publication, query)
    }

    fn operation_status_for(
        &self,
        operation: crate::world::RequestId,
        intent: &crate::world::Intent,
    ) -> Result<crate::world::OperationStatus, crate::world::Failure> {
        self.operation_status_for(operation, intent)
    }

    fn with_lifecycle_source(
        &self,
        source: &crate::world::LifecycleSourceCoordinate,
        prepare: &mut dyn FnMut(
            &crate::world::Context<'_>,
        ) -> Result<Vec<u8>, crate::world::Rejection>,
    ) -> Result<Result<Vec<u8>, crate::world::Rejection>, crate::world::Failure> {
        self.with_lifecycle_source(source, prepare)
    }

    fn local_session(&self) -> Option<&Session> {
        Some(self)
    }
}

pub trait IdentityAccess: Send + Sync {
    fn device(&self) -> &mechanics::ids::DeviceId;
    fn sign_action(
        &self,
        session: &dyn SessionAccess,
        request: crate::world::RequestId,
        intent: crate::world::Intent,
    ) -> Result<crate::world::SignedWorldAction, crate::world::Rejection>;
}

impl IdentityAccess for LocalIdentity {
    fn device(&self) -> &mechanics::ids::DeviceId {
        self.device()
    }

    fn sign_action(
        &self,
        session: &dyn SessionAccess,
        request: crate::world::RequestId,
        intent: crate::world::Intent,
    ) -> Result<crate::world::SignedWorldAction, crate::world::Rejection> {
        let session = session
            .local_session()
            .ok_or(crate::world::Rejection::ContractViolation)?;
        self.sign_action(session, request, intent)
    }
}

pub struct Context<'a> {
    pub session: &'a dyn SessionAccess,
    pub identity: &'a dyn IdentityAccess,
    pub actor: &'a str,
    pub device: &'a str,
}

/// Somebody a World thinks should be told about a call, and what to tell them.
///
/// The World answers **who and what**; the host answers **whether they are
/// reachable and how**. That split is the whole point of the type. A World that
/// knew who was connected would be a World holding a delivery plane, and a host
/// that knew assigning means telling the assignee would be a host holding
/// product rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nudge {
    /// The actor to tell, canonical.
    pub actor: String,
    /// The signal schema, which must be one this World declared — the host
    /// refuses an undeclared one rather than sending it.
    pub schema: String,
    /// The payload, bounded by the schema's own declared ceiling.
    ///
    /// A pointer to durable material and never the material itself: the record
    /// is already committed and already converging, and a nudge that carried the
    /// fact would be a second copy of it on a plane that keeps nothing.
    pub payload: Vec<u8>,
}

/// The object-safe application handler provided by a World implementation.
pub trait Handler: Send + Sync {
    /// Decode and classify a call. Hosts use this trusted classification for
    /// policy such as delegated-agent partial-view guards.
    fn access(&self, call: &Call) -> Result<Access, Failure>;

    /// Execute a validated product call through the World's docked Session.
    fn call(&self, call: &Call, context: &Context<'_>) -> Reply;

    /// Who should be told about a call that succeeded, and what to tell them.
    ///
    /// Asked after the commit, so a World answers about work that actually
    /// happened. The default is nobody, which is what a World with no signals
    /// declared means.
    ///
    /// **The acting identity is not in the answer.** Nobody is told about their
    /// own action — a World that returned the actor here would have every person
    /// notified of everything they did.
    fn nudges(&self, _call: &Call, _reply: &Reply, _context: &Context<'_>) -> Vec<Nudge> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> Call {
        Call::new(
            WorldId::parse("com.example.files").unwrap(),
            "files.list",
            1,
            vec![0, 1, 2, 253, 254, 255],
        )
        .unwrap()
    }

    #[test]
    fn wire_is_versioned_and_payload_is_opaque_base64() {
        let call = call();
        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(json["world"], "com.example.files");
        assert_eq!(json["operation"], "files.list");
        assert_eq!(json["version"], 1);
        assert!(json["payload"].is_string());
        assert_eq!(serde_json::from_value::<Call>(json).unwrap(), call);
    }

    #[test]
    fn call_contract_rejects_invalid_versions_operations_and_bounds() {
        let world = WorldId::parse("com.example.files").unwrap();
        assert!(Call::new(world.clone(), "Files List", 1, vec![]).is_err());
        assert!(Call::new(world.clone(), "files.list", 0, vec![]).is_err());
        assert!(Call::new(world, "files.list", 1, vec![0; MAX_WORLD_CALL_PAYLOAD + 1]).is_err());
    }

    #[test]
    fn reply_is_bound_to_the_call_contract() {
        let call = call();
        let reply = Reply::ok(&call, b"[]".to_vec());
        reply.validate_for(&call).unwrap();
        assert_eq!(reply.into_result().unwrap(), b"[]");

        let other = Call::new(
            WorldId::parse("com.example.files").unwrap(),
            "files.get",
            1,
            vec![],
        )
        .unwrap();
        assert!(Reply::ok(&call, vec![]).validate_for(&other).is_err());
    }
}
