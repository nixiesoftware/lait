//! Product-neutral application calls into hosted Worlds.
//!
//! Runtime's [`runtime::World`] contract describes semantic intents and
//! projections. This crate describes the outer application boundary: a
//! versioned, opaque call addressed to one World, the matching reply, and the
//! object-safe handler a compile-time product package supplies.
//!
//! The payload codec belongs to the product. A host can therefore route a
//! World it does not understand without importing that World's request or
//! response types.

use std::fmt;

use replica::ids::WorldId;
use runtime::{LocalIdentity, Session};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Maximum decoded request payload accepted by the local World-call boundary.
///
/// The semantic Runtime applies its own, usually tighter, per-World limits
/// after a product handler decodes this application request.
pub const MAX_WORLD_CALL_PAYLOAD: usize = 1024 * 1024;

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
    pub fn new(bytes: Vec<u8>) -> Result<Self, CallFailure> {
        if bytes.len() > MAX_WORLD_REPLY_PAYLOAD {
            return Err(CallFailure::new(
                CallFailureCode::InvalidCall,
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
pub struct WorldCall {
    world: WorldId,
    operation: String,
    version: u32,
    payload: OpaquePayload,
}

impl<'de> Deserialize<'de> for WorldCall {
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
        WorldCall::new(wire.world, wire.operation, wire.version, payload).map_err(de::Error::custom)
    }
}

impl WorldCall {
    pub fn new(
        world: WorldId,
        operation: impl Into<String>,
        version: u32,
        payload: Vec<u8>,
    ) -> Result<Self, CallFailure> {
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

    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn payload(&self) -> &[u8] {
        self.payload.as_bytes()
    }

    pub fn validate(&self) -> Result<(), CallFailure> {
        let valid_operation = !self.operation.is_empty()
            && self.operation.len() <= MAX_OPERATION_LEN
            && self.operation.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            });
        if !valid_operation {
            return Err(CallFailure::new(
                CallFailureCode::InvalidCall,
                format!(
                    "World operation must be 1..={MAX_OPERATION_LEN} lowercase ASCII \
                     letters, digits, '.', '_' or '-'"
                ),
            ));
        }
        if self.version == 0 {
            return Err(CallFailure::new(
                CallFailureCode::InvalidCall,
                "World call version must be non-zero",
            ));
        }
        if self.payload.as_bytes().len() > MAX_WORLD_CALL_PAYLOAD {
            return Err(CallFailure::new(
                CallFailureCode::InvalidCall,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldCallAccess {
    Query,
    Command,
}

/// Stable host-level failure classes. Product errors belong in a successful
/// payload and retain the product's own response schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallFailureCode {
    InvalidCall,
    UnsupportedOperation,
    UnsupportedVersion,
    Denied,
    Unavailable,
    Internal,
}

/// A failure to validate or host an application call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallFailure {
    pub code: CallFailureCode,
    pub message: String,
}

impl CallFailure {
    pub fn new(code: CallFailureCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for CallFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CallFailure {}

/// One reply bound to the exact World operation and payload version requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldReply {
    world: WorldId,
    operation: String,
    version: u32,
    #[serde(flatten)]
    outcome: WorldReplyOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum WorldReplyOutcome {
    Ok { payload: OpaquePayload },
    Error { error: CallFailure },
}

impl WorldReply {
    pub fn ok(call: &WorldCall, payload: Vec<u8>) -> Self {
        match OpaquePayload::new(payload) {
            Ok(payload) => Self {
                world: call.world.clone(),
                operation: call.operation.clone(),
                version: call.version,
                outcome: WorldReplyOutcome::Ok { payload },
            },
            Err(error) => Self::error(call, CallFailureCode::Internal, error.message),
        }
    }

    pub fn error(call: &WorldCall, code: CallFailureCode, message: impl Into<String>) -> Self {
        Self {
            world: call.world.clone(),
            operation: call.operation.clone(),
            version: call.version,
            outcome: WorldReplyOutcome::Error {
                error: CallFailure::new(code, message),
            },
        }
    }

    /// Whether this reply carries a payload rather than an error.
    ///
    /// Exposed so a host can ask "did anything happen" without consuming the
    /// reply, which `into_result` would.
    pub fn succeeded(&self) -> bool {
        matches!(self.outcome, WorldReplyOutcome::Ok { .. })
    }

    pub fn validate_for(&self, call: &WorldCall) -> Result<(), CallFailure> {
        if self.world != call.world
            || self.operation != call.operation
            || self.version != call.version
        {
            return Err(CallFailure::new(
                CallFailureCode::Internal,
                "World handler returned a reply for a different call contract",
            ));
        }
        if let WorldReplyOutcome::Ok { payload } = &self.outcome {
            if payload.as_bytes().len() > MAX_WORLD_REPLY_PAYLOAD {
                return Err(CallFailure::new(
                    CallFailureCode::Internal,
                    "World handler returned an oversized reply",
                ));
            }
        }
        Ok(())
    }

    pub fn into_result(self) -> Result<Vec<u8>, CallFailure> {
        match self.outcome {
            WorldReplyOutcome::Ok { payload } => Ok(payload.into_bytes()),
            WorldReplyOutcome::Error { error } => Err(error),
        }
    }
}

/// Principal facts supplied to a World's application handler.
///
/// This is deliberately smaller than [`runtime::Context`]. The handler
/// can resolve user-facing input and sign through the supplied Session, but it
/// receives no Mechanics, Replica, transport, or storage handle.
pub struct WorldCallContext<'a> {
    pub session: &'a Session,
    pub identity: &'a LocalIdentity,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldNudge {
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

/// The object-safe application handler bundled with a World implementation.
pub trait WorldCallHandler: Send + Sync {
    /// Decode and classify a call. Hosts use this trusted classification for
    /// policy such as delegated-agent partial-view guards.
    fn access(&self, call: &WorldCall) -> Result<WorldCallAccess, CallFailure>;

    /// Execute a validated product call through the World's docked Session.
    fn call(&self, call: &WorldCall, context: &WorldCallContext<'_>) -> WorldReply;

    /// Who should be told about a call that succeeded, and what to tell them.
    ///
    /// Asked after the commit, so a World answers about work that actually
    /// happened. The default is nobody, which is what a World with no signals
    /// declared means.
    ///
    /// **The acting identity is not in the answer.** Nobody is told about their
    /// own action — a World that returned the actor here would have every person
    /// notified of everything they did.
    fn nudges(
        &self,
        _call: &WorldCall,
        _reply: &WorldReply,
        _context: &WorldCallContext<'_>,
    ) -> Vec<WorldNudge> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> WorldCall {
        WorldCall::new(
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
        assert_eq!(serde_json::from_value::<WorldCall>(json).unwrap(), call);
    }

    #[test]
    fn call_contract_rejects_invalid_versions_operations_and_bounds() {
        let world = WorldId::parse("com.example.files").unwrap();
        assert!(WorldCall::new(world.clone(), "Files List", 1, vec![]).is_err());
        assert!(WorldCall::new(world.clone(), "files.list", 0, vec![]).is_err());
        assert!(
            WorldCall::new(world, "files.list", 1, vec![0; MAX_WORLD_CALL_PAYLOAD + 1]).is_err()
        );
    }

    #[test]
    fn reply_is_bound_to_the_call_contract() {
        let call = call();
        let reply = WorldReply::ok(&call, b"[]".to_vec());
        reply.validate_for(&call).unwrap();
        assert_eq!(reply.into_result().unwrap(), b"[]");

        let other = WorldCall::new(
            WorldId::parse("com.example.files").unwrap(),
            "files.get",
            1,
            vec![],
        )
        .unwrap();
        assert!(WorldReply::ok(&call, vec![]).validate_for(&other).is_err());
    }
}
