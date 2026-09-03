#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! Provider-neutral identity and inventory for first-class Lait agents.
//!
//! An agent is a global [`ProfileId`], not a Space-scoped actor. Ownership is
//! an immutable agreement signed by one currently resolved device of the agent
//! and one currently resolved device of its owner. Inventory describes the
//! primitives the identity possesses; it neither instantiates a provider nor
//! grants use of one.
//!
//! Three projections keep inspection separate from custody:
//!
//! - public and contact readers receive only public-safe fields of items their
//!   proven relationship admits;
//! - the owner receives every item and its owner controls, but only safe status
//!   for secret bindings;
//! - the agent's local trusted runtime receives opaque secret references. The
//!   secret values themselves never enter this crate.

mod backend;
mod console;
mod store;

use std::collections::BTreeSet;
use std::fmt;

use mechanics::actor::{device_from_seed, sign_detached, verify_detached};
use mechanics::ids::DeviceId;
use mechanics::kinship::{ProfileId, Signature};
use serde::{Deserialize, Serialize};

pub use backend::{
    AgentRuntimeBackend, EngineClientEnvironment, LimitEnforcement, OciRuntimeBackend,
    PreparedRuntime, RuntimeBackendError, RuntimeBinding, RuntimeConfigurationBinding,
    RuntimeEnforcement, RuntimeLimits, RuntimeProviderPosture, RuntimeProviderStanding,
    RuntimeProviderUnavailable, RuntimeScope, MAX_RUNTIME_COORDINATE_BYTES,
};
pub use console::{
    ConsoleCompletion, ConsoleExecutionBinding, ConsoleLedger, ConsoleOperation,
    ConsoleOperationId, ConsoleOperationInput, ConsoleReplyStanding, ConsoleStanding,
    MAX_CONSOLE_COORDINATE_BYTES, MAX_CONSOLE_INPUT_BYTES, MAX_CONSOLE_OPERATIONS,
    MAX_CONSOLE_REPLY_BYTES,
};
pub use store::AgentStore;

pub const OWNERSHIP_VERSION: u16 = 1;
pub const AGENT_RECORD_VERSION: u16 = 1;
pub const INVENTORY_VERSION: u16 = 1;

pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_INTRODUCTION_BYTES: usize = 4 * 1024;
pub const MAX_ITEMS: usize = 256;
pub const MAX_ITEM_ID_BYTES: usize = 96;
pub const MAX_KIND_BYTES: usize = 128;
pub const MAX_LABEL_BYTES: usize = 128;
pub const MAX_SUMMARY_BYTES: usize = 2 * 1024;
pub const MAX_FIELDS_PER_CLASS: usize = 64;
pub const MAX_FIELD_KEY_BYTES: usize = 96;
pub const MAX_FIELD_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_SECRET_BINDINGS: usize = 32;
pub const MAX_SECRET_REF_BYTES: usize = 256;
pub const MAX_OPAQUE_ITEM_BYTES: usize = 64 * 1024;

const OWNERSHIP_DOMAIN: &[u8] = b"lait/agent/ownership/1";
const INVENTORY_DOMAIN: &[u8] = b"lait/agent/inventory/1";
const INVENTORY_PUBLICATION_DOMAIN: &[u8] = b"lait/agent/inventory-publication/1";

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Bound(&'static str),
    UnsupportedVersion { artifact: &'static str, found: u16 },
    BadSignature(&'static str),
    UnrootedSigner(&'static str),
    Unauthorized,
    Conflict { expected: u64, actual: u64 },
    RevisionExhausted,
    Corrupt(&'static str),
    Io(std::io::Error),
    Storage(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(what) => write!(f, "invalid {what}"),
            Self::Bound(what) => write!(f, "{what} exceeds its bound"),
            Self::UnsupportedVersion { artifact, found } => {
                write!(f, "unsupported {artifact} version {found}")
            }
            Self::BadSignature(side) => write!(f, "the {side} ownership signature is invalid"),
            Self::UnrootedSigner(side) => {
                write!(
                    f,
                    "the {side} signer is not a resolved device of that profile"
                )
            }
            Self::Unauthorized => f.write_str("the requester is not authorized"),
            Self::Conflict { expected, actual } => {
                write!(
                    f,
                    "revision conflict: expected {expected}, current is {actual}"
                )
            }
            Self::RevisionExhausted => f.write_str("the revision counter is exhausted"),
            Self::Corrupt(what) => write!(f, "corrupt agent state: {what}"),
            Self::Io(error) => write!(f, "agent state I/O: {error}"),
            Self::Storage(message) => write!(f, "private agent storage: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// The immutable terms both profiles sign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipTerms {
    pub version: u16,
    pub agent: ProfileId,
    pub owner: ProfileId,
    pub created_at: u64,
    pub nonce: [u8; 16],
}

impl OwnershipTerms {
    #[must_use]
    pub fn new(agent: ProfileId, owner: ProfileId, created_at: u64, nonce: [u8; 16]) -> Self {
        Self {
            version: OWNERSHIP_VERSION,
            agent,
            owner,
            created_at,
            nonce,
        }
    }

    pub fn sign(&self, role: OwnershipRole, seed: &[u8; 32]) -> Result<OwnershipHalf, Error> {
        self.validate()?;
        let profile = match role {
            OwnershipRole::Agent => self.agent.clone(),
            OwnershipRole::Owner => self.owner.clone(),
        };
        let by = device_from_seed(seed);
        let signature = Signature(sign_detached(seed, &self.preimage()?));
        Ok(OwnershipHalf {
            role,
            profile,
            by,
            signature,
        })
    }

    fn validate(&self) -> Result<(), Error> {
        check_version("ownership", self.version, OWNERSHIP_VERSION)?;
        if self.agent == self.owner {
            return Err(Error::Invalid("ownership profiles are not distinct"));
        }
        Ok(())
    }

    fn preimage(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        frame(&mut out, OWNERSHIP_DOMAIN)?;
        frame(&mut out, &self.version.to_be_bytes())?;
        frame(&mut out, self.agent.as_str().as_bytes())?;
        frame(&mut out, self.owner.as_str().as_bytes())?;
        frame(&mut out, &self.created_at.to_be_bytes())?;
        frame(&mut out, &self.nonce)?;
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnershipRole {
    Agent,
    Owner,
}

/// One side's signature. It is not an ownership bond until assembled with the
/// other role and checked against both profiles' resolved device sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipHalf {
    pub role: OwnershipRole,
    pub profile: ProfileId,
    pub by: DeviceId,
    pub signature: Signature,
}

/// An immutable, dual-signed relationship between an agent profile and owner
/// profile. No transfer operation exists in this generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipBond {
    terms: OwnershipTerms,
    agent_by: DeviceId,
    agent_signature: Signature,
    owner_by: DeviceId,
    owner_signature: Signature,
}

impl OwnershipBond {
    pub fn assemble(
        terms: OwnershipTerms,
        agent: OwnershipHalf,
        owner: OwnershipHalf,
        agent_devices: &[DeviceId],
        owner_devices: &[DeviceId],
    ) -> Result<Self, Error> {
        terms.validate()?;
        check_half(&terms, &agent, OwnershipRole::Agent)?;
        check_half(&terms, &owner, OwnershipRole::Owner)?;
        let bond = Self {
            terms,
            agent_by: agent.by,
            agent_signature: agent.signature,
            owner_by: owner.by,
            owner_signature: owner.signature,
        };
        bond.verify(agent_devices, owner_devices)?;
        Ok(bond)
    }

    #[must_use]
    pub const fn terms(&self) -> &OwnershipTerms {
        &self.terms
    }

    #[must_use]
    pub const fn agent(&self) -> &ProfileId {
        &self.terms.agent
    }

    #[must_use]
    pub const fn owner(&self) -> &ProfileId {
        &self.terms.owner
    }

    #[must_use]
    pub const fn agent_signer(&self) -> &DeviceId {
        &self.agent_by
    }

    #[must_use]
    pub const fn owner_signer(&self) -> &DeviceId {
        &self.owner_by
    }

    /// Verify signatures and that each signing device belongs to the profile
    /// whose role it accepted. Device resolution is an explicit input because
    /// a `ProfileId` is a content address, not a signing key.
    pub fn verify(
        &self,
        agent_devices: &[DeviceId],
        owner_devices: &[DeviceId],
    ) -> Result<(), Error> {
        self.verify_signatures()?;
        if !agent_devices.contains(&self.agent_by) {
            return Err(Error::UnrootedSigner("agent"));
        }
        if !owner_devices.contains(&self.owner_by) {
            return Err(Error::UnrootedSigner("owner"));
        }
        Ok(())
    }

    /// Verify the immutable bytes without making a current profile-membership
    /// claim. Store decoding uses this; callers crossing an authority boundary
    /// must additionally call [`OwnershipBond::verify`].
    pub fn verify_signatures(&self) -> Result<(), Error> {
        self.terms.validate()?;
        let preimage = self.terms.preimage()?;
        verify_side("agent", &self.agent_by, &self.agent_signature, &preimage)?;
        verify_side("owner", &self.owner_by, &self.owner_signature, &preimage)
    }
}

fn check_half(
    terms: &OwnershipTerms,
    half: &OwnershipHalf,
    expected: OwnershipRole,
) -> Result<(), Error> {
    if half.role != expected {
        return Err(Error::Invalid("ownership half role"));
    }
    let profile = match expected {
        OwnershipRole::Agent => &terms.agent,
        OwnershipRole::Owner => &terms.owner,
    };
    if &half.profile != profile {
        return Err(Error::Invalid("ownership half profile"));
    }
    verify_side(
        match expected {
            OwnershipRole::Agent => "agent",
            OwnershipRole::Owner => "owner",
        },
        &half.by,
        &half.signature,
        &terms.preimage()?,
    )
}

fn verify_side(
    side: &'static str,
    device: &DeviceId,
    signature: &Signature,
    preimage: &[u8],
) -> Result<(), Error> {
    let key = device.key_bytes().ok_or(Error::BadSignature(side))?;
    if verify_detached(&key, preimage, signature.bytes()) {
        Ok(())
    } else {
        Err(Error::BadSignature(side))
    }
}

fn frame(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    let len = u64::try_from(bytes.len()).map_err(|_| Error::Bound("ownership preimage"))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentLifecycle {
    Active,
    Suspended,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub version: u16,
    pub revision: u64,
    pub ownership: OwnershipBond,
    pub name: String,
    pub introduction: String,
    pub lifecycle: AgentLifecycle,
}

impl AgentRecord {
    pub fn new(
        ownership: OwnershipBond,
        name: String,
        introduction: String,
    ) -> Result<Self, Error> {
        let record = Self {
            version: AGENT_RECORD_VERSION,
            revision: 0,
            ownership,
            name,
            introduction,
            lifecycle: AgentLifecycle::Active,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn apply(
        &mut self,
        requester: &ProfileId,
        expected_revision: u64,
        mutation: RecordMutation,
    ) -> Result<(), Error> {
        authorize(self.ownership.owner(), requester)?;
        expect_revision(self.revision, expected_revision)?;
        let mut candidate = self.clone();
        match mutation {
            RecordMutation::Present { name, introduction } => {
                candidate.name = name;
                candidate.introduction = introduction;
            }
            RecordMutation::SetLifecycle(lifecycle) => candidate.lifecycle = lifecycle,
        }
        candidate.revision = next_revision(candidate.revision)?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), Error> {
        check_version("agent record", self.version, AGENT_RECORD_VERSION)?;
        self.ownership.verify_signatures()?;
        check_nonempty("agent name", &self.name, MAX_NAME_BYTES)?;
        check_text(
            "agent introduction",
            &self.introduction,
            MAX_INTRODUCTION_BYTES,
        )
    }

    #[must_use]
    pub fn public_view(&self) -> PublicAgentView {
        PublicAgentView {
            agent: self.ownership.agent().clone(),
            owner: self.ownership.owner().clone(),
            name: self.name.clone(),
            introduction: self.introduction.clone(),
            lifecycle: self.lifecycle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordMutation {
    Present { name: String, introduction: String },
    SetLifecycle(AgentLifecycle),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicAgentView {
    pub agent: ProfileId,
    pub owner: ProfileId,
    pub name: String,
    pub introduction: String,
    pub lifecycle: AgentLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Contacts,
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VisibilityOverride {
    Inherit,
    Contacts,
    Private,
}

impl VisibilityOverride {
    #[must_use]
    pub fn effective(self, collection: Visibility) -> Visibility {
        let floor = match self {
            Self::Inherit => collection,
            Self::Contacts => Visibility::Contacts,
            Self::Private => Visibility::Private,
        };
        collection.max(floor)
    }
}

/// Authenticated owner context for one revision. The seed is borrowed for the
/// duration of the mutation and is never retained by a record or store.
pub struct OwnerAuthor<'a> {
    pub profile: &'a ProfileId,
    pub seed: &'a [u8; 32],
    pub resolved_devices: &'a [DeviceId],
}

impl OwnerAuthor<'_> {
    fn authorize(&self, bond: &OwnershipBond) -> Result<DeviceId, Error> {
        authorize(bond.owner(), self.profile)?;
        let by = device_from_seed(self.seed);
        if !self.resolved_devices.contains(&by) {
            return Err(Error::UnrootedSigner("owner"));
        }
        Ok(by)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InventoryItemId(String);

impl InventoryItemId {
    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        check_token("inventory item id", &value, MAX_ITEM_ID_BYTES, false)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PrimitiveKind(String);

impl PrimitiveKind {
    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        check_token("primitive kind", &value, MAX_KIND_BYTES, true)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FieldKey(String);

impl FieldKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        check_token("inventory field key", &value, MAX_FIELD_KEY_BYTES, false)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValue {
    Boolean(bool),
    Integer(i64),
    Unsigned(u64),
    DurationMillis(u64),
    ByteSize(u64),
    Text(String),
    Choice(String),
    ContentRef(String),
    Profile(ProfileId),
}

impl FieldValue {
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Text(value) => check_text("inventory field text", value, MAX_FIELD_TEXT_BYTES),
            Self::Choice(value) => {
                check_token("inventory field choice", value, MAX_FIELD_KEY_BYTES, false)
            }
            Self::ContentRef(value) => check_token(
                "inventory content reference",
                value,
                MAX_SECRET_REF_BYTES,
                false,
            ),
            Self::Boolean(_)
            | Self::Integer(_)
            | Self::Unsigned(_)
            | Self::DurationMillis(_)
            | Self::ByteSize(_)
            | Self::Profile(_) => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryField {
    pub key: FieldKey,
    pub value: FieldValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrimitiveStanding {
    Ready,
    Unavailable,
    Suspended,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        check_token("secret reference", &value, MAX_SECRET_REF_BYTES, false)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretStanding {
    Connected,
    Missing,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretBinding {
    pub label: String,
    pub reference: SecretRef,
    pub standing: SecretStanding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryItem {
    pub id: InventoryItemId,
    pub kind: PrimitiveKind,
    pub label: String,
    pub summary: String,
    pub visibility: VisibilityOverride,
    pub standing: PrimitiveStanding,
    pub public_fields: Vec<InventoryField>,
    pub owner_fields: Vec<InventoryField>,
    pub secrets: Vec<SecretBinding>,
}

impl InventoryItem {
    pub fn validate(&self) -> Result<(), Error> {
        check_token(
            "inventory item id",
            self.id.as_str(),
            MAX_ITEM_ID_BYTES,
            false,
        )?;
        check_token("primitive kind", self.kind.as_str(), MAX_KIND_BYTES, true)?;
        check_nonempty("inventory item label", &self.label, MAX_LABEL_BYTES)?;
        check_text("inventory item summary", &self.summary, MAX_SUMMARY_BYTES)?;
        validate_fields("public inventory fields", &self.public_fields)?;
        validate_fields("owner inventory fields", &self.owner_fields)?;
        let public_keys: BTreeSet<&FieldKey> =
            self.public_fields.iter().map(|field| &field.key).collect();
        if self
            .owner_fields
            .iter()
            .any(|field| public_keys.contains(&field.key))
        {
            return Err(Error::Invalid("public and owner field keys overlap"));
        }
        if self.secrets.len() > MAX_SECRET_BINDINGS {
            return Err(Error::Bound("secret bindings per inventory item"));
        }
        let mut refs = BTreeSet::new();
        for secret in &self.secrets {
            check_nonempty("secret binding label", &secret.label, MAX_LABEL_BYTES)?;
            check_token(
                "secret reference",
                secret.reference.as_str(),
                MAX_SECRET_REF_BYTES,
                false,
            )?;
            if !refs.insert(secret.reference.clone()) {
                return Err(Error::Invalid("duplicate secret reference"));
            }
        }
        Ok(())
    }

    fn public_view(&self) -> PublicItemView {
        PublicItemView {
            id: self.id.clone(),
            kind: self.kind.clone(),
            label: self.label.clone(),
            summary: self.summary.clone(),
            standing: Some(self.standing),
            fields: self.public_fields.clone(),
        }
    }

    fn owner_view(&self) -> OwnerItemView {
        OwnerItemView {
            public: self.public_view(),
            visibility: self.visibility,
            fields: self.owner_fields.clone(),
            secrets: self
                .secrets
                .iter()
                .map(|secret| SecretSummary {
                    label: secret.label.clone(),
                    standing: secret.standing,
                })
                .collect(),
            editable: true,
        }
    }
}

/// Forward-compatible item envelope. A newer producer places the part an old
/// client cannot decode in `body` while keeping a bounded, public-safe generic
/// description outside it. Old clients preserve this entry byte-for-byte on
/// every unrelated mutation and never offer an edit operation for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpaqueInventoryItem {
    pub id: InventoryItemId,
    pub kind: PrimitiveKind,
    pub label: String,
    pub summary: String,
    pub visibility: VisibilityOverride,
    pub body_version: u16,
    pub body: Vec<u8>,
}

impl OpaqueInventoryItem {
    pub fn validate(&self) -> Result<(), Error> {
        check_token(
            "opaque inventory item id",
            self.id.as_str(),
            MAX_ITEM_ID_BYTES,
            false,
        )?;
        check_token(
            "opaque primitive kind",
            self.kind.as_str(),
            MAX_KIND_BYTES,
            true,
        )?;
        check_nonempty("opaque inventory item label", &self.label, MAX_LABEL_BYTES)?;
        check_text(
            "opaque inventory item summary",
            &self.summary,
            MAX_SUMMARY_BYTES,
        )?;
        if self.body.len() > MAX_OPAQUE_ITEM_BYTES {
            return Err(Error::Bound("opaque inventory item body"));
        }
        Ok(())
    }

    fn public_view(&self) -> PublicItemView {
        PublicItemView {
            id: self.id.clone(),
            kind: self.kind.clone(),
            label: self.label.clone(),
            summary: self.summary.clone(),
            standing: None,
            fields: Vec::new(),
        }
    }

    fn owner_view(&self) -> OwnerItemView {
        OwnerItemView {
            public: self.public_view(),
            visibility: self.visibility,
            fields: Vec::new(),
            secrets: Vec::new(),
            editable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryManifest {
    pub version: u16,
    pub revision: u64,
    pub agent: ProfileId,
    pub default_visibility: Visibility,
    pub items: Vec<InventoryItem>,
    pub opaque_items: Vec<OpaqueInventoryItem>,
    pub authored_by: DeviceId,
    pub signature: Signature,
    public_publication: SignedInventoryPublication,
    contact_publication: SignedInventoryPublication,
}

impl InventoryManifest {
    pub fn empty(
        bond: &OwnershipBond,
        default_visibility: Visibility,
        author: &OwnerAuthor<'_>,
    ) -> Result<Self, Error> {
        let by = author.authorize(bond)?;
        let mut manifest = Self {
            version: INVENTORY_VERSION,
            revision: 0,
            agent: bond.agent().clone(),
            default_visibility,
            items: Vec::new(),
            opaque_items: Vec::new(),
            authored_by: by.clone(),
            signature: Signature([0; 64]),
            public_publication: SignedInventoryPublication::blank(
                bond.agent().clone(),
                PublicationAudience::Public,
                by.clone(),
            ),
            contact_publication: SignedInventoryPublication::blank(
                bond.agent().clone(),
                PublicationAudience::Contacts,
                by,
            ),
        };
        manifest.sign(bond, author)?;
        Ok(manifest)
    }

    pub fn apply(
        &mut self,
        bond: &OwnershipBond,
        author: &OwnerAuthor<'_>,
        expected_revision: u64,
        mutation: InventoryMutation,
    ) -> Result<(), Error> {
        author.authorize(bond)?;
        if &self.agent != bond.agent() {
            return Err(Error::Invalid("inventory agent does not match ownership"));
        }
        // The standing revision may have been signed by an owner device that
        // has since rotated out. Its signature remains historical evidence;
        // only the *new* author's device must be currently resolved.
        self.validate()?;
        expect_revision(self.revision, expected_revision)?;
        let mut candidate = self.clone();
        match mutation {
            InventoryMutation::SetDefaultVisibility(visibility) => {
                candidate.default_visibility = visibility;
            }
            InventoryMutation::Add(item) => {
                if candidate.items.iter().any(|held| held.id == item.id) {
                    return Err(Error::Invalid("duplicate inventory item id"));
                }
                if candidate.opaque_items.iter().any(|held| held.id == item.id) {
                    return Err(Error::Invalid("duplicate inventory item id"));
                }
                candidate.items.push(item);
            }
            InventoryMutation::AddOpaque(item) => {
                if candidate.items.iter().any(|held| held.id == item.id)
                    || candidate.opaque_items.iter().any(|held| held.id == item.id)
                {
                    return Err(Error::Invalid("duplicate inventory item id"));
                }
                candidate.opaque_items.push(item);
            }
            InventoryMutation::Replace(item) => {
                let Some(held) = candidate.items.iter_mut().find(|held| held.id == item.id) else {
                    return Err(Error::Invalid("inventory item does not exist"));
                };
                *held = item;
            }
            InventoryMutation::Remove(id) => {
                if candidate.opaque_items.iter().any(|held| held.id == id) {
                    return Err(Error::Invalid("opaque inventory items are read-only"));
                }
                let before = candidate.items.len();
                candidate.items.retain(|held| held.id != id);
                if before == candidate.items.len() {
                    return Err(Error::Invalid("inventory item does not exist"));
                }
            }
        }
        candidate
            .items
            .sort_by(|left, right| left.id.cmp(&right.id));
        candidate.revision = next_revision(candidate.revision)?;
        candidate.sign(bond, author)?;
        *self = candidate;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), Error> {
        self.validate_core()?;
        self.verify_signatures()?;
        self.verify_publications()
    }

    fn validate_core(&self) -> Result<(), Error> {
        check_version("inventory manifest", self.version, INVENTORY_VERSION)?;
        let count = self
            .items
            .len()
            .checked_add(self.opaque_items.len())
            .ok_or(Error::Bound("inventory items"))?;
        if count > MAX_ITEMS {
            return Err(Error::Bound("inventory items"));
        }
        let mut ids = BTreeSet::new();
        for item in &self.items {
            item.validate()?;
            if !ids.insert(item.id.clone()) {
                return Err(Error::Invalid("duplicate inventory item id"));
            }
        }
        for item in &self.opaque_items {
            item.validate()?;
            if !ids.insert(item.id.clone()) {
                return Err(Error::Invalid("duplicate inventory item id"));
            }
        }
        Ok(())
    }

    /// Verify owner authorship against the currently resolved owner profile.
    pub fn verify(&self, bond: &OwnershipBond, owner_devices: &[DeviceId]) -> Result<(), Error> {
        if &self.agent != bond.agent() {
            return Err(Error::Invalid("inventory agent does not match ownership"));
        }
        self.validate()?;
        if !owner_devices.contains(&self.authored_by) {
            return Err(Error::UnrootedSigner("owner"));
        }
        self.public_publication.verify(bond, owner_devices)?;
        self.contact_publication.verify(bond, owner_devices)
    }

    fn verify_signatures(&self) -> Result<(), Error> {
        let preimage = inventory_preimage(self)?;
        verify_side("owner", &self.authored_by, &self.signature, &preimage)
    }

    fn sign(&mut self, bond: &OwnershipBond, author: &OwnerAuthor<'_>) -> Result<(), Error> {
        let by = author.authorize(bond)?;
        self.authored_by = by.clone();
        self.validate_core()?;
        self.signature = Signature(sign_detached(author.seed, &inventory_preimage(self)?));
        self.public_publication =
            self.make_publication(PublicationAudience::Public, by.clone(), author.seed)?;
        self.contact_publication =
            self.make_publication(PublicationAudience::Contacts, by, author.seed)?;
        self.validate()
    }

    fn make_publication(
        &self,
        audience: PublicationAudience,
        authored_by: DeviceId,
        seed: &[u8; 32],
    ) -> Result<SignedInventoryPublication, Error> {
        let visibility = match audience {
            PublicationAudience::Public => Visibility::Public,
            PublicationAudience::Contacts => Visibility::Contacts,
        };
        let mut publication = SignedInventoryPublication {
            version: INVENTORY_VERSION,
            agent: self.agent.clone(),
            revision: self.revision,
            audience,
            items: self.public_items(visibility),
            authored_by,
            signature: Signature([0; 64]),
        };
        publication.signature = Signature(sign_detached(seed, &publication.preimage()?));
        Ok(publication)
    }

    fn verify_publications(&self) -> Result<(), Error> {
        for (publication, audience, visibility) in [
            (
                &self.public_publication,
                PublicationAudience::Public,
                Visibility::Public,
            ),
            (
                &self.contact_publication,
                PublicationAudience::Contacts,
                Visibility::Contacts,
            ),
        ] {
            publication.verify_signature()?;
            if publication.agent != self.agent
                || publication.revision != self.revision
                || publication.audience != audience
                || publication.authored_by != self.authored_by
                || publication.items != self.public_items(visibility)
            {
                return Err(Error::Invalid(
                    "inventory publication diverges from manifest",
                ));
            }
        }
        Ok(())
    }

    pub fn project(
        &self,
        bond: &OwnershipBond,
        reader: InventoryReader<'_>,
    ) -> Result<InventoryProjection, Error> {
        self.validate()?;
        if &self.agent != bond.agent() {
            return Err(Error::Invalid("inventory agent does not match ownership"));
        }
        match reader {
            InventoryReader::Public => {
                Ok(self.audience_view(Visibility::Public, &self.public_publication))
            }
            InventoryReader::Contact => {
                Ok(self.audience_view(Visibility::Contacts, &self.contact_publication))
            }
            InventoryReader::Owner(profile) => {
                authorize(bond.owner(), profile)?;
                Ok(InventoryProjection::Owner(OwnerInventoryView {
                    revision: self.revision,
                    default_visibility: self.default_visibility,
                    items: self
                        .items
                        .iter()
                        .map(InventoryItem::owner_view)
                        .chain(
                            self.opaque_items
                                .iter()
                                .map(OpaqueInventoryItem::owner_view),
                        )
                        .collect(),
                }))
            }
            InventoryReader::Secret(profile) => {
                authorize(bond.agent(), profile)?;
                Ok(InventoryProjection::Secret(SecretInventoryView {
                    revision: self.revision,
                    default_visibility: self.default_visibility,
                    items: self.items.clone(),
                }))
            }
        }
    }

    fn audience_view(
        &self,
        audience: Visibility,
        publication: &SignedInventoryPublication,
    ) -> InventoryProjection {
        if audience < self.default_visibility {
            return InventoryProjection::Hidden;
        }
        InventoryProjection::Public(publication.clone())
    }

    fn public_items(&self, audience: Visibility) -> Vec<PublicItemView> {
        self.items
            .iter()
            .filter(|item| audience >= item.visibility.effective(self.default_visibility))
            .map(InventoryItem::public_view)
            .chain(
                self.opaque_items
                    .iter()
                    .filter(|item| audience >= item.visibility.effective(self.default_visibility))
                    .map(OpaqueInventoryItem::public_view),
            )
            .collect()
    }
}

fn inventory_preimage(manifest: &InventoryManifest) -> Result<Vec<u8>, Error> {
    #[derive(Serialize)]
    struct Core<'a> {
        version: u16,
        revision: u64,
        agent: &'a ProfileId,
        default_visibility: Visibility,
        items: &'a [InventoryItem],
        opaque_items: &'a [OpaqueInventoryItem],
        authored_by: &'a DeviceId,
    }
    let core = Core {
        version: manifest.version,
        revision: manifest.revision,
        agent: &manifest.agent,
        default_visibility: manifest.default_visibility,
        items: &manifest.items,
        opaque_items: &manifest.opaque_items,
        authored_by: &manifest.authored_by,
    };
    let bytes = postcard::to_stdvec(&core).map_err(|_| Error::Invalid("inventory encoding"))?;
    let mut out = Vec::new();
    frame(&mut out, INVENTORY_DOMAIN)?;
    frame(&mut out, &bytes)?;
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryMutation {
    SetDefaultVisibility(Visibility),
    Add(InventoryItem),
    /// Introduce a future-version item through the bounded compatibility
    /// envelope. Once present, this generation preserves it and treats it as
    /// read-only; only a client that understands its body may replace it.
    AddOpaque(OpaqueInventoryItem),
    Replace(InventoryItem),
    Remove(InventoryItemId),
}

#[derive(Debug, Clone, Copy)]
pub enum InventoryReader<'a> {
    Public,
    Contact,
    Owner(&'a ProfileId),
    /// Local trusted runtime for this exact agent. This is the only projection
    /// containing opaque secret references; never expose it as inventory API
    /// output.
    Secret(&'a ProfileId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryProjection {
    /// The inventory exists, but no count, category, identifier, revision, or
    /// timestamp is disclosed to this reader.
    Hidden,
    Public(PublicInventoryView),
    Owner(OwnerInventoryView),
    Secret(SecretInventoryView),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicInventoryView {
    pub version: u16,
    pub agent: ProfileId,
    pub revision: u64,
    pub audience: PublicationAudience,
    pub items: Vec<PublicItemView>,
    pub authored_by: DeviceId,
    pub signature: Signature,
}

pub type SignedInventoryPublication = PublicInventoryView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationAudience {
    Public,
    Contacts,
}

impl SignedInventoryPublication {
    fn blank(agent: ProfileId, audience: PublicationAudience, authored_by: DeviceId) -> Self {
        Self {
            version: INVENTORY_VERSION,
            agent,
            revision: 0,
            audience,
            items: Vec::new(),
            authored_by,
            signature: Signature([0; 64]),
        }
    }

    fn preimage(&self) -> Result<Vec<u8>, Error> {
        #[derive(Serialize)]
        struct Core<'a> {
            version: u16,
            agent: &'a ProfileId,
            revision: u64,
            audience: PublicationAudience,
            items: &'a [PublicItemView],
            authored_by: &'a DeviceId,
        }
        let bytes = postcard::to_stdvec(&Core {
            version: self.version,
            agent: &self.agent,
            revision: self.revision,
            audience: self.audience,
            items: &self.items,
            authored_by: &self.authored_by,
        })
        .map_err(|_| Error::Invalid("inventory publication encoding"))?;
        let mut out = Vec::new();
        frame(&mut out, INVENTORY_PUBLICATION_DOMAIN)?;
        frame(&mut out, &bytes)?;
        Ok(out)
    }

    fn verify_signature(&self) -> Result<(), Error> {
        check_version("inventory publication", self.version, INVENTORY_VERSION)?;
        verify_side(
            "owner",
            &self.authored_by,
            &self.signature,
            &self.preimage()?,
        )
    }

    pub fn verify(&self, bond: &OwnershipBond, owner_devices: &[DeviceId]) -> Result<(), Error> {
        if &self.agent != bond.agent() {
            return Err(Error::Invalid("inventory publication agent"));
        }
        self.verify_signature()?;
        if !owner_devices.contains(&self.authored_by) {
            return Err(Error::UnrootedSigner("owner"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicItemView {
    pub id: InventoryItemId,
    pub kind: PrimitiveKind,
    pub label: String,
    pub summary: String,
    /// `None` means an opaque future item rendered generically and read-only.
    pub standing: Option<PrimitiveStanding>,
    pub fields: Vec<InventoryField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerInventoryView {
    pub revision: u64,
    pub default_visibility: Visibility,
    pub items: Vec<OwnerItemView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerItemView {
    pub public: PublicItemView,
    pub visibility: VisibilityOverride,
    pub fields: Vec<InventoryField>,
    /// Safe standing only. The opaque references remain absent.
    pub secrets: Vec<SecretSummary>,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSummary {
    pub label: String,
    pub standing: SecretStanding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretInventoryView {
    pub revision: u64,
    pub default_visibility: Visibility,
    pub items: Vec<InventoryItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentState {
    pub record: AgentRecord,
    pub inventory: InventoryManifest,
}

impl AgentState {
    pub fn new(record: AgentRecord, inventory: InventoryManifest) -> Result<Self, Error> {
        let state = Self { record, inventory };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), Error> {
        self.record.validate()?;
        self.inventory.validate()?;
        if self.record.ownership.agent() != &self.inventory.agent {
            return Err(Error::Invalid("inventory agent does not match record"));
        }
        Ok(())
    }

    /// Verify identity authorship with device evidence appropriate to these
    /// artifacts. A historical loader may supply the device sets proven at
    /// their signed heads; mutation separately requires the new owner author
    /// to be in the currently resolved owner set.
    pub fn verify(
        &self,
        agent_devices: &[DeviceId],
        owner_devices: &[DeviceId],
    ) -> Result<(), Error> {
        self.validate()?;
        self.record.ownership.verify(agent_devices, owner_devices)?;
        self.inventory.verify(&self.record.ownership, owner_devices)
    }

    #[must_use]
    pub const fn head(&self) -> StateRevision {
        StateRevision {
            record: self.record.revision,
            inventory: self.inventory.revision,
        }
    }

    pub fn apply(
        &mut self,
        author: &OwnerAuthor<'_>,
        expected: StateRevision,
        mutation: StateMutation,
    ) -> Result<(), Error> {
        expected.check(self.head())?;
        author.authorize(&self.record.ownership)?;
        match mutation {
            StateMutation::Record(mutation) => {
                self.record
                    .apply(author.profile, expected.record, mutation)?;
            }
            StateMutation::Inventory(mutation) => {
                self.inventory.apply(
                    &self.record.ownership,
                    author,
                    expected.inventory,
                    mutation,
                )?;
            }
        }
        self.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRevision {
    pub record: u64,
    pub inventory: u64,
}

impl StateRevision {
    fn check(self, actual: Self) -> Result<(), Error> {
        if self.record != actual.record {
            return Err(Error::Conflict {
                expected: self.record,
                actual: actual.record,
            });
        }
        if self.inventory != actual.inventory {
            return Err(Error::Conflict {
                expected: self.inventory,
                actual: actual.inventory,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateMutation {
    Record(RecordMutation),
    Inventory(InventoryMutation),
}

fn authorize(expected: &ProfileId, requester: &ProfileId) -> Result<(), Error> {
    if expected == requester {
        Ok(())
    } else {
        Err(Error::Unauthorized)
    }
}

fn expect_revision(actual: u64, expected: u64) -> Result<(), Error> {
    if actual == expected {
        Ok(())
    } else {
        Err(Error::Conflict { expected, actual })
    }
}

fn next_revision(current: u64) -> Result<u64, Error> {
    current.checked_add(1).ok_or(Error::RevisionExhausted)
}

fn check_version(artifact: &'static str, found: u16, expected: u16) -> Result<(), Error> {
    if found == expected {
        Ok(())
    } else {
        Err(Error::UnsupportedVersion { artifact, found })
    }
}

fn check_text(what: &'static str, value: &str, max: usize) -> Result<(), Error> {
    if value.len() > max {
        Err(Error::Bound(what))
    } else if value.chars().any(char::is_control) {
        Err(Error::Invalid(what))
    } else {
        Ok(())
    }
}

fn check_nonempty(what: &'static str, value: &str, max: usize) -> Result<(), Error> {
    check_text(what, value, max)?;
    if value.trim().is_empty() {
        Err(Error::Invalid(what))
    } else {
        Ok(())
    }
}

fn check_token(
    what: &'static str,
    value: &str,
    max: usize,
    require_namespace: bool,
) -> Result<(), Error> {
    if value.is_empty() {
        return Err(Error::Invalid(what));
    }
    if value.len() > max {
        return Err(Error::Bound(what));
    }
    if require_namespace && !value.contains('.') {
        return Err(Error::Invalid(what));
    }
    if value.starts_with(['.', '-', '_'])
        || value.ends_with(['.', '-', '_'])
        || value.bytes().any(|byte| {
            !(byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'_'))
        })
    {
        return Err(Error::Invalid(what));
    }
    Ok(())
}

fn validate_fields(what: &'static str, fields: &[InventoryField]) -> Result<(), Error> {
    if fields.len() > MAX_FIELDS_PER_CLASS {
        return Err(Error::Bound(what));
    }
    let mut keys = BTreeSet::new();
    for field in fields {
        check_token(
            "inventory field key",
            field.key.as_str(),
            MAX_FIELD_KEY_BYTES,
            false,
        )?;
        field.value.validate()?;
        if !keys.insert(field.key.clone()) {
            return Err(Error::Invalid("duplicate inventory field key"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
