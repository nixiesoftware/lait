#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "implementation tables and canonical payloads validate dimensions before fixed-layout access"
)]
//! The World implementation identity — the authority-approved
//! compatibility/trust identity a Space pins.
//!
//! `WorldImplementationId` is the canonical digest of a
//! [`Implementation`]. It is **not** native-code attestation:
//! trusted in-process Rust self-asserts its embedded descriptor; Runtime
//! hashes it and requires the resulting id to be **active in Mechanics at the
//! pinned authority frontier** before activation, dock, submit, query,
//! projection/audit helpers, or IAM expansion planning. Upgrade and rollback
//! are explicit authority operations
//! ([`mechanics::membership::AclAction::ActivateWorldImplementation`]), never
//! deployment configuration.
//!
//! The descriptor body is a fixed-order binary tuple: `u16` version
//! (big-endian); `u16`-length-prefixed canonical WorldId bytes; `u32` policy
//! protocol and implementation version (big-endian); `u16` schema count
//! followed by `u32`-length-prefixed schema descriptors sorted by their
//! complete canonical bytes; then exactly 32-byte policy-table commitment and
//! 32-byte artifact identity. Schema duplicates, unsorted entries, unknown
//! version, and trailing bytes reject. Every count word is a `u16`, so a list
//! longer than [`MAX_ENCODABLE_ENTRIES`] has no encoding at all and `encode`
//! refuses it rather than writing a count that describes fewer entries than
//! follow it.
//!
//! After the body comes an optional **section table** — `u16` count, then per
//! section a `u16` tag, a `u32` payload length, and that many payload bytes,
//! tags strictly ascending. The version word is chosen by the content rather
//! than by the build: [`DESCRIPTOR_VERSION_SECTIONLESS`] when there are no
//! sections and [`DESCRIPTOR_VERSION_SECTIONED`] when there are. That is what
//! the table buys — inventing a section kind does not move the id of a World
//! that declares nothing of that kind, whereas two more fields in a fixed-order
//! tuple would move every id in the system.
//!
//! An unknown tag **rejects**; it is not skipped. Skipping would make the
//! implementation id a digest over bytes this build did not interpret, which is
//! the one thing a reviewed trust identity may not be. A section's inner
//! grammar is frozen by its tag for the same reason: a new field is a new tag,
//! which moves the id of every World that declares that section and no others.
//!
//! The hash domain does **not** move with the encoding version. It stays
//! `lait.world-implementation.v1` because it is what every shipped activation
//! record was derived under, and moving it invalidates all of them at once
//! (`docs/COMPATIBILITY.md` §1, "A hash domain").

use replica::body::{MutationModel, Schema};
use replica::body::{SchemaId, WorldId};

use crate::{
    exec, find,
    world::{Descriptor, ScopeSchema, SignalSchema},
};

/// BLAKE3 derive-key context for the implementation id.
const IMPLEMENTATION_CONTEXT: &str = "lait.world-implementation.v1";
/// BLAKE3 derive-key context for the policy-table commitment.
const POLICY_TABLE_CONTEXT: &str = "lait.world-policy-table.v1";

/// The descriptor version word when the section table is absent.
pub const DESCRIPTOR_VERSION_SECTIONLESS: u16 = 1;
/// The descriptor version word when the section table is present.
pub const DESCRIPTOR_VERSION_SECTIONED: u16 = 2;

/// The longest list a count word can describe.
///
/// Enforced at encode rather than at registration, unlike the *value* bounds
/// `Builder::build` applies: those are policy about what a declaration
/// may say, this is a fact about the format. A list past it has no canonical
/// encoding, and an unchecked cast would truncate the count, derive an id over
/// bytes carrying it, and leave `decode` refusing the very bytes the id
/// commits to. The section table needs no such guard — tags strictly ascend
/// over a four-tag set, which bounds it at four.
pub const MAX_ENCODABLE_ENTRIES: usize = u16::MAX as usize;

/// The section tags this build interprets. A tag outside this set rejects.
pub mod section {
    /// Declared transient scopes.
    pub const SCOPE_SCHEMAS: u16 = 0x0001;
    /// Declared World signals.
    pub const SIGNAL_SCHEMAS: u16 = 0x0002;
    /// Declared World-owned Find vocabularies.
    pub const FIND_SCHEMAS: u16 = 0x0003;
    /// Declared callable Exec Specs, including their Link Specs.
    pub const EXEC_SPECS: u16 = 0x0004;
}

/// Why a descriptor failed to encode/decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    UnknownVersion(u16),
    UnsortedOrDuplicateSchemas,
    Truncated,
    TrailingBytes,
    BadWorldId,
    /// A section tag this build does not interpret.
    UnknownSectionTag(u16),
    UnsortedOrDuplicateSections,
    /// A version-2 encoding whose table declares no sections — the same value
    /// as a version-1 encoding, spelled a second way.
    EmptySectionTable,
    /// A section whose payload does not fill, or overruns, its own declared
    /// length, or whose entries are malformed.
    BadSection(u16),
    UnsortedOrDuplicateDeclarations(u16),
    /// A schema list past [`MAX_ENCODABLE_ENTRIES`].
    TooManySchemas,
    /// A section's entry list past [`MAX_ENCODABLE_ENTRIES`].
    TooManyDeclarations(u16),
    /// The re-encode backstop: a decode that succeeded but does not reproduce
    /// its own input. Unreachable while the explicit rules above stay
    /// exhaustive, which is exactly when it is worth being told.
    NonCanonical,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Invalid {}

/// The complete implementation descriptor a World embeds and self-asserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Implementation {
    pub world: WorldId,
    /// The policy-protocol version this implementation speaks (demand
    /// selection semantics).
    pub policy_protocol: u32,
    pub implementation_version: u32,
    /// Canonical schema-descriptor bytes ([`canonical_schema_bytes`]),
    /// sorted by their complete canonical bytes, no duplicates.
    pub schemas: Vec<Vec<u8>>,
    /// BLAKE3 derive-key commitment over the checked-in exhaustive policy
    /// table bytes ([`policy_table_commitment`]).
    pub policy_table_commitment: [u8; 32],
    /// An authority-reviewed, build-embedded 32-byte release id — not a
    /// platform binary hash or attestation.
    pub artifact_identity: [u8; 32],
    /// The section table, strictly ascending by tag. Empty is the ordinary
    /// case, and an empty table is not encoded at all. Last, matching wire
    /// order.
    pub sections: Vec<DescriptorSection>,
}

/// One section of a descriptor.
///
/// Typed rather than opaque bytes — the opposite of how `schemas` works. The
/// opaque form pushes ordering to the decoder and leaves the inner grammar
/// unchecked, which is survivable for a blob nothing else reads; here the
/// non-emptiness and entry-ordering rules *are* the canonicality rules, so
/// enforcing them beside the section table means one place decides what a
/// canonical descriptor is. Unknown tags reject, so there is never an
/// uninterpreted section to carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorSection {
    ScopeSchemas(Vec<ScopeSchema>),
    SignalSchemas(Vec<SignalSchema>),
    FindSchemas(Vec<find::Schema>),
    ExecSpecs(Vec<exec::Spec>),
}

impl DescriptorSection {
    pub fn tag(&self) -> u16 {
        match self {
            DescriptorSection::ScopeSchemas(_) => section::SCOPE_SCHEMAS,
            DescriptorSection::SignalSchemas(_) => section::SIGNAL_SCHEMAS,
            DescriptorSection::FindSchemas(_) => section::FIND_SCHEMAS,
            DescriptorSection::ExecSpecs(_) => section::EXEC_SPECS,
        }
    }

    /// Entries sort by **name**, not by whole-entry bytes. Whole-entry order
    /// would admit `("note", 8)` and `("note", 16)` in one descriptor — two
    /// ceilings for one name, ordered and therefore canonical. Sorting by name
    /// and demanding strict ascent makes a duplicate name unrepresentable
    /// rather than merely rejected.
    fn validate(&self) -> Result<(), Invalid> {
        let tag = self.tag();
        match self {
            DescriptorSection::ScopeSchemas(entries) => {
                validate_names(tag, entries.iter().map(|entry| &entry.name))?;
            }
            DescriptorSection::SignalSchemas(entries) => {
                validate_names(tag, entries.iter().map(|entry| &entry.name))?;
            }
            DescriptorSection::FindSchemas(entries) => {
                if entries.is_empty() {
                    return Err(Invalid::BadSection(tag));
                }
                if entries.len() > MAX_ENCODABLE_ENTRIES {
                    return Err(Invalid::TooManyDeclarations(tag));
                }
                if !entries
                    .windows(2)
                    .all(|pair| pair[0].reference < pair[1].reference)
                    || entries.iter().any(|entry| entry.validate().is_err())
                {
                    return Err(Invalid::UnsortedOrDuplicateDeclarations(tag));
                }
            }
            DescriptorSection::ExecSpecs(entries) => {
                if entries.is_empty() {
                    return Err(Invalid::BadSection(tag));
                }
                if entries.len() > MAX_ENCODABLE_ENTRIES {
                    return Err(Invalid::TooManyDeclarations(tag));
                }
                if !entries
                    .windows(2)
                    .all(|pair| (&pair[0].name, pair[0].version) < (&pair[1].name, pair[1].version))
                    || entries.iter().any(|entry| entry.validate().is_err())
                {
                    return Err(Invalid::UnsortedOrDuplicateDeclarations(tag));
                }
            }
        }
        Ok(())
    }

    /// The count casts here are lossless because [`Self::validate`] has already
    /// refused a longer list, and `encode` runs it first.
    fn encode_payload(&self) -> Result<Vec<u8>, Invalid> {
        let mut out = Vec::new();
        match self {
            DescriptorSection::ScopeSchemas(entries) => {
                out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
                for entry in entries {
                    push_name(&mut out, &entry.name);
                    out.extend_from_slice(&entry.max_key_bytes.to_be_bytes());
                }
            }
            DescriptorSection::SignalSchemas(entries) => {
                out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
                for entry in entries {
                    push_name(&mut out, &entry.name);
                    out.extend_from_slice(&entry.max_payload_bytes.to_be_bytes());
                    out.extend_from_slice(&(entry.demand.len() as u32).to_be_bytes());
                    out.extend_from_slice(&entry.demand);
                }
            }
            DescriptorSection::FindSchemas(entries) => {
                out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
                for entry in entries {
                    let bytes = entry
                        .encode()
                        .map_err(|_| Invalid::BadSection(section::FIND_SCHEMAS))?;
                    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                    out.extend_from_slice(&bytes);
                }
            }
            DescriptorSection::ExecSpecs(entries) => {
                out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
                for entry in entries {
                    let bytes = entry
                        .encode()
                        .map_err(|_| Invalid::BadSection(section::EXEC_SPECS))?;
                    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                    out.extend_from_slice(&bytes);
                }
            }
        }
        Ok(out)
    }

    fn decode_payload(tag: u16, payload: &[u8]) -> Result<Self, Invalid> {
        // The tag decides the grammar before a byte of the payload is read, so
        // an uninterpreted section is refused whole rather than diagnosed as a
        // malformed known one.
        type Parse = fn(&mut SectionReader<'_>, usize) -> Result<DescriptorSection, Invalid>;
        let parse: Parse = match tag {
            section::SCOPE_SCHEMAS => decode_scope_entries,
            section::SIGNAL_SCHEMAS => decode_signal_entries,
            section::FIND_SCHEMAS => decode_find_entries,
            section::EXEC_SPECS => decode_exec_entries,
            _ => return Err(Invalid::UnknownSectionTag(tag)),
        };
        let mut r = SectionReader::new(tag, payload);
        let count = r.u16()? as usize;
        if count == 0 {
            return Err(Invalid::BadSection(tag));
        }
        let section = parse(&mut r, count)?;
        r.finish()?;
        section.validate()?;
        Ok(section)
    }
}

fn validate_names<'a>(
    tag: u16,
    names: impl ExactSizeIterator<Item = &'a SchemaId>,
) -> Result<(), Invalid> {
    if names.len() == 0 {
        return Err(Invalid::BadSection(tag));
    }
    if names.len() > MAX_ENCODABLE_ENTRIES {
        return Err(Invalid::TooManyDeclarations(tag));
    }
    let names: Vec<&SchemaId> = names.collect();
    if !names.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(Invalid::UnsortedOrDuplicateDeclarations(tag));
    }
    Ok(())
}

fn decode_scope_entries(
    r: &mut SectionReader<'_>,
    count: usize,
) -> Result<DescriptorSection, Invalid> {
    let mut entries = Vec::new();
    for _ in 0..count {
        let name = r.name()?;
        let max_key_bytes = r.u32()?;
        entries.push(ScopeSchema {
            name,
            max_key_bytes,
        });
    }
    Ok(DescriptorSection::ScopeSchemas(entries))
}

fn decode_signal_entries(
    r: &mut SectionReader<'_>,
    count: usize,
) -> Result<DescriptorSection, Invalid> {
    let mut entries = Vec::new();
    for _ in 0..count {
        let name = r.name()?;
        let max_payload_bytes = r.u32()?;
        let demand_len = r.u32()? as usize;
        let demand = r.take(demand_len)?.to_vec();
        entries.push(SignalSchema {
            name,
            max_payload_bytes,
            demand,
        });
    }
    Ok(DescriptorSection::SignalSchemas(entries))
}

fn decode_find_entries(
    r: &mut SectionReader<'_>,
    count: usize,
) -> Result<DescriptorSection, Invalid> {
    let mut entries = Vec::new();
    for _ in 0..count {
        let length = r.u32()? as usize;
        let bytes = r.take(length)?;
        let schema = find::Schema::decode_canonical(bytes)
            .map_err(|_| Invalid::BadSection(section::FIND_SCHEMAS))?;
        entries.push(schema);
    }
    Ok(DescriptorSection::FindSchemas(entries))
}

fn decode_exec_entries(
    r: &mut SectionReader<'_>,
    count: usize,
) -> Result<DescriptorSection, Invalid> {
    let mut entries = Vec::new();
    for _ in 0..count {
        let length = r.u32()? as usize;
        let bytes = r.take(length)?;
        let spec = exec::Spec::decode_canonical(bytes)
            .map_err(|_| Invalid::BadSection(section::EXEC_SPECS))?;
        entries.push(spec);
    }
    Ok(DescriptorSection::ExecSpecs(entries))
}

fn push_name(out: &mut Vec<u8>, name: &SchemaId) {
    let bytes = name.as_bytes();
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// A reader bounded to one section's payload.
///
/// The tag travels with it because a shortfall inside a *known* boundary is a
/// malformed section, not a truncated record — the enclosing length already
/// said how many bytes there were.
struct SectionReader<'a> {
    tag: u16,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> SectionReader<'a> {
    fn new(tag: u16, bytes: &'a [u8]) -> Self {
        Self { tag, bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Invalid> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(Invalid::BadSection(self.tag))?;
        if end > self.bytes.len() {
            return Err(Invalid::BadSection(self.tag));
        }
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16, Invalid> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, Invalid> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn name(&mut self) -> Result<SchemaId, Invalid> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .ok()
            .and_then(SchemaId::parse)
            .ok_or(Invalid::BadSection(self.tag))
    }

    /// A section's parse must land exactly on its declared length: bytes left
    /// inside would be a place to hide an extension of a frozen grammar.
    fn finish(self) -> Result<(), Invalid> {
        if self.pos != self.bytes.len() {
            return Err(Invalid::BadSection(self.tag));
        }
        Ok(())
    }
}

/// The canonical bytes of one schema declaration inside a descriptor:
/// `u16`+SchemaId bytes, `u32` version (BE), `u16`+EncodingId bytes, one
/// mutation-model tag byte, `u16` readable-predecessor count followed by each
/// `u32` (BE, sorted ascending).
pub fn canonical_schema_bytes(schema: &Schema) -> Vec<u8> {
    let mut out = Vec::new();
    let id = schema.id.as_str().as_bytes();
    out.extend_from_slice(&(id.len() as u16).to_be_bytes());
    out.extend_from_slice(id);
    out.extend_from_slice(&schema.version.to_be_bytes());
    let enc = schema.encoding.as_str().as_bytes();
    out.extend_from_slice(&(enc.len() as u16).to_be_bytes());
    out.extend_from_slice(enc);
    out.push(match schema.mutation {
        MutationModel::Atomic => 0,
        MutationModel::ImmutableAtomic => 1,
        MutationModel::Collaborative(_) => 2,
    });
    let mut predecessors = schema.readable_predecessors.clone();
    predecessors.sort_unstable();
    predecessors.dedup();
    out.extend_from_slice(&(predecessors.len() as u16).to_be_bytes());
    for p in predecessors {
        out.extend_from_slice(&p.to_be_bytes());
    }
    out
}

/// The policy-table commitment over the checked-in exhaustive table bytes.
pub fn policy_table_commitment(table_bytes: &[u8]) -> [u8; 32] {
    blake3::derive_key(POLICY_TABLE_CONTEXT, table_bytes)
}

impl Implementation {
    /// Build a descriptor from a complete World registration: schemas
    /// canonicalized and sorted, declarations sorted by name, and a section
    /// omitted entirely when its list is empty.
    ///
    /// This replaces a schemas-only constructor rather than joining it. A
    /// surviving schemas-only form is a constructor that silently drops
    /// sections, so the day a World declares its first signal its
    /// implementation id would not move — a descriptor that under-reports its
    /// own review is worse than a compile error. It also reads
    /// `implementation_version` off the registration instead of taking it
    /// again, removing the second source of truth a caller could disagree with
    /// itself about.
    pub fn from_registration(
        registration: &Descriptor,
        policy_protocol: u32,
        policy_table_commitment: [u8; 32],
        artifact_identity: [u8; 32],
    ) -> Self {
        let mut canonical: Vec<Vec<u8>> = registration
            .schemas
            .iter()
            .map(canonical_schema_bytes)
            .collect();
        canonical.sort();
        canonical.dedup();

        let mut sections = Vec::new();
        if !registration.scope_schemas.is_empty() {
            let mut scopes = registration.scope_schemas.clone();
            scopes.sort_by(|a, b| a.name.cmp(&b.name));
            sections.push(DescriptorSection::ScopeSchemas(scopes));
        }
        if !registration.signal_schemas.is_empty() {
            let mut signals = registration.signal_schemas.clone();
            signals.sort_by(|a, b| a.name.cmp(&b.name));
            sections.push(DescriptorSection::SignalSchemas(signals));
        }
        if !registration.find_schemas.is_empty() {
            let mut schemas: Vec<find::Schema> = registration
                .find_schemas
                .iter()
                .map(find::Schema::canonicalized)
                .collect();
            schemas.sort_by(|left, right| left.reference.cmp(&right.reference));
            sections.push(DescriptorSection::FindSchemas(schemas));
        }
        if !registration.exec_specs.is_empty() {
            let mut specs = registration.exec_specs.clone();
            specs.sort_by(|left, right| {
                (&left.name, left.version).cmp(&(&right.name, right.version))
            });
            sections.push(DescriptorSection::ExecSpecs(specs));
        }

        Self {
            world: registration.id.clone(),
            policy_protocol,
            implementation_version: registration.implementation_version.0,
            schemas: canonical,
            policy_table_commitment,
            artifact_identity,
            sections,
        }
    }

    /// The version word this descriptor's content selects.
    pub fn version(&self) -> u16 {
        if self.sections.is_empty() {
            DESCRIPTOR_VERSION_SECTIONLESS
        } else {
            DESCRIPTOR_VERSION_SECTIONED
        }
    }

    /// The canonical descriptor encoding.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        if self.schemas.len() > MAX_ENCODABLE_ENTRIES {
            return Err(Invalid::TooManySchemas);
        }
        for w in self.schemas.windows(2) {
            if w[0] >= w[1] {
                return Err(Invalid::UnsortedOrDuplicateSchemas);
            }
        }
        for w in self.sections.windows(2) {
            if w[0].tag() >= w[1].tag() {
                return Err(Invalid::UnsortedOrDuplicateSections);
            }
        }
        for s in &self.sections {
            s.validate()?;
        }
        let find_schemas = self
            .sections
            .iter()
            .find_map(|section| match section {
                DescriptorSection::FindSchemas(schemas) => Some(schemas.as_slice()),
                DescriptorSection::ScopeSchemas(_)
                | DescriptorSection::SignalSchemas(_)
                | DescriptorSection::ExecSpecs(_) => None,
            })
            .unwrap_or_default();
        if let Some(DescriptorSection::ExecSpecs(specs)) = self
            .sections
            .iter()
            .find(|section| matches!(section, DescriptorSection::ExecSpecs(_)))
        {
            for spec in specs {
                spec.validate_with_find(find_schemas)
                    .map_err(|_| Invalid::BadSection(section::EXEC_SPECS))?;
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(&self.version().to_be_bytes());
        let world = self.world.as_str().as_bytes();
        out.extend_from_slice(&(world.len() as u16).to_be_bytes());
        out.extend_from_slice(world);
        out.extend_from_slice(&self.policy_protocol.to_be_bytes());
        out.extend_from_slice(&self.implementation_version.to_be_bytes());
        out.extend_from_slice(&(self.schemas.len() as u16).to_be_bytes());
        for schema in &self.schemas {
            out.extend_from_slice(&(schema.len() as u32).to_be_bytes());
            out.extend_from_slice(schema);
        }
        out.extend_from_slice(&self.policy_table_commitment);
        out.extend_from_slice(&self.artifact_identity);
        if !self.sections.is_empty() {
            out.extend_from_slice(&(self.sections.len() as u16).to_be_bytes());
            for s in &self.sections {
                let payload = s.encode_payload()?;
                out.extend_from_slice(&s.tag().to_be_bytes());
                out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                out.extend_from_slice(&payload);
            }
        }
        Ok(out)
    }

    /// Strict decode of the canonical encoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, Invalid> {
        let mut pos = 0usize;
        // `n` is peer-supplied and reaches `u32::MAX` below. `*pos + n` wraps on
        // a 32-bit target, and a wrapped sum passes a `bytes.len() <` guard and
        // then indexes with a reversed range — a remotely-triggerable abort in
        // a decoder. `SectionReader::take` in this same file gets this right;
        // this closure is the copy that did not.
        let take = |pos: &mut usize, n: usize| -> Result<&[u8], Invalid> {
            let end = pos.checked_add(n).ok_or(Invalid::Truncated)?;
            if end > bytes.len() {
                return Err(Invalid::Truncated);
            }
            let s = &bytes[*pos..end];
            *pos = end;
            Ok(s)
        };
        let version = u16::from_be_bytes(take(&mut pos, 2)?.try_into().unwrap());
        if version != DESCRIPTOR_VERSION_SECTIONLESS && version != DESCRIPTOR_VERSION_SECTIONED {
            return Err(Invalid::UnknownVersion(version));
        }
        let world_len = u16::from_be_bytes(take(&mut pos, 2)?.try_into().unwrap()) as usize;
        let world_bytes = take(&mut pos, world_len)?;
        let world = std::str::from_utf8(world_bytes)
            .ok()
            .and_then(WorldId::parse)
            .ok_or(Invalid::BadWorldId)?;
        let policy_protocol = u32::from_be_bytes(take(&mut pos, 4)?.try_into().unwrap());
        let implementation_version = u32::from_be_bytes(take(&mut pos, 4)?.try_into().unwrap());
        let count = u16::from_be_bytes(take(&mut pos, 2)?.try_into().unwrap()) as usize;
        // Neither this count nor the section count below reserves capacity.
        // Both are wire numbers read before the bytes they describe, so
        // reserving against either lets a seventeen-byte record ask for
        // megabytes. Growing as entries are actually read costs a few
        // reallocations on the honest path and bounds the dishonest one by the
        // length of the input.
        let mut schemas = Vec::new();
        let mut prev: Option<Vec<u8>> = None;
        for _ in 0..count {
            let len = u32::from_be_bytes(take(&mut pos, 4)?.try_into().unwrap()) as usize;
            let schema = take(&mut pos, len)?.to_vec();
            if let Some(prev) = &prev {
                if prev >= &schema {
                    return Err(Invalid::UnsortedOrDuplicateSchemas);
                }
            }
            prev = Some(schema.clone());
            schemas.push(schema);
        }
        let policy_table_commitment: [u8; 32] = take(&mut pos, 32)?.try_into().unwrap();
        let artifact_identity: [u8; 32] = take(&mut pos, 32)?.try_into().unwrap();

        let mut sections = Vec::new();
        if version == DESCRIPTOR_VERSION_SECTIONED {
            let section_count = u16::from_be_bytes(take(&mut pos, 2)?.try_into().unwrap());
            if section_count == 0 {
                return Err(Invalid::EmptySectionTable);
            }
            let mut prev_tag: Option<u16> = None;
            for _ in 0..section_count {
                let tag = u16::from_be_bytes(take(&mut pos, 2)?.try_into().unwrap());
                let len = u32::from_be_bytes(take(&mut pos, 4)?.try_into().unwrap()) as usize;
                let payload = take(&mut pos, len)?;
                if let Some(prev) = prev_tag {
                    if prev >= tag {
                        return Err(Invalid::UnsortedOrDuplicateSections);
                    }
                }
                prev_tag = Some(tag);
                sections.push(DescriptorSection::decode_payload(tag, payload)?);
            }
        }

        if pos != bytes.len() {
            return Err(Invalid::TrailingBytes);
        }
        let decoded = Self {
            world,
            policy_protocol,
            implementation_version,
            schemas,
            policy_table_commitment,
            artifact_identity,
            sections,
        };
        // The backstop that makes "exactly one spelling per value" a property
        // rather than a list. It subsumes every rule above it — a version-2
        // encoding with an empty table re-encodes as version 1 and cannot
        // compare equal — and the explicit rules stay above it only so a
        // failure names its reason.
        if decoded.encode()? != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(decoded)
    }

    /// The canonical implementation id.
    pub fn id(&self) -> Result<[u8; 32], Invalid> {
        Ok(blake3::derive_key(IMPLEMENTATION_CONTEXT, &self.encode()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::body::{EncodingId, SchemaId};

    fn schema(name: &str, version: u32) -> Schema {
        Schema {
            id: SchemaId::parse(name).unwrap(),
            version,
            encoding: EncodingId::parse("json").unwrap(),
            mutation: MutationModel::Atomic,
            readable_predecessors: vec![],
        }
    }

    fn registration(schemas: &[Schema]) -> Descriptor {
        Descriptor {
            id: WorldId::parse("com.example.notes").unwrap(),
            implementation_version: crate::world::Version(7),
            schemas: schemas.to_vec(),
            limits: crate::world::Limits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: Vec::new(),
            find_extractors: Vec::new(),
            exec_specs: Vec::new(),
        }
    }

    fn descriptor(schemas: &[Schema]) -> Implementation {
        Implementation::from_registration(&registration(schemas), 1, [3u8; 32], [4u8; 32])
    }

    /// Bumping the declared version must move the id.
    ///
    /// The whole catch-up story rests on this: the id is what a Space records
    /// and compares, and the version is what orders two of them. If a version
    /// bump left the id alone, two builds that disagree about their contract
    /// would look identical to every node, and a fleet would silently run
    /// mixed. Asserted rather than assumed, because the encoding is the only
    /// thing that makes it true and encodings get edited.
    #[test]
    fn the_declared_version_is_part_of_the_identity() {
        let schemas = [schema("notes.note", 1)];
        let mut a = descriptor(&schemas);
        let mut b = descriptor(&schemas);
        b.implementation_version = a.implementation_version + 1;
        assert_ne!(
            a.id().unwrap(),
            b.id().unwrap(),
            "a version bump must produce a different implementation id"
        );
        // …and nothing else moved: same version, same id.
        a.implementation_version = b.implementation_version;
        assert_eq!(a.id().unwrap(), b.id().unwrap());
    }

    fn scope(name: &str, max_key_bytes: u32) -> ScopeSchema {
        ScopeSchema {
            name: SchemaId::parse(name).unwrap(),
            max_key_bytes,
        }
    }

    fn signal(name: &str, max_payload_bytes: u32) -> SignalSchema {
        SignalSchema {
            name: SchemaId::parse(name).unwrap(),
            max_payload_bytes,
            demand: vec![9, 9, 9],
        }
    }

    fn demand(capability: &str) -> Vec<u8> {
        mechanics::authorization::AuthorizationDemand::require(
            mechanics::authorization::PolicyCapability::new("com.example.notes", capability),
            mechanics::authorization::Resource::root("com.example.notes"),
        )
        .encode_canonical()
        .unwrap()
    }

    fn find_ref(name: &str, version: u32) -> find::SchemaRef {
        find::SchemaRef {
            name: SchemaId::parse(name).unwrap(),
            version,
        }
    }

    fn find_bound(value: u64) -> find::Bound {
        find::Bound {
            decoded_bodies: value,
            postings_read: value,
            edges_visited: value,
            nodes_visited: value,
            paths_retained: value,
            candidates_per_branch: value,
            score_evaluations: value,
            projected_bytes: value,
            packed_tokens: value,
            wall_millis: value,
        }
    }

    fn find_schema(name: &str) -> find::Schema {
        let reference = find_ref(name, 1);
        let analyzer = find::AnalyzerRef {
            schema: reference.clone(),
            name: SchemaId::parse("plain").unwrap(),
        };
        let gate = find::GateRef {
            schema: reference.clone(),
            name: SchemaId::parse("read").unwrap(),
        };
        find::Schema {
            reference: reference.clone(),
            sources: vec![find::SourceRef {
                name: SchemaId::parse("aa").unwrap(),
                version: 1,
            }],
            fields: vec![find::Field {
                reference: find::FieldRef {
                    schema: reference.clone(),
                    name: SchemaId::parse("title").unwrap(),
                },
                kind: find::FieldKind::Text,
                analyzer: Some(analyzer.clone()),
            }],
            edges: vec![find::Edge {
                reference: find::EdgeRef {
                    schema: reference.clone(),
                    name: SchemaId::parse("related").unwrap(),
                },
                target: reference.clone(),
                gate: gate.clone(),
            }],
            gates: vec![find::Gate {
                reference: gate,
                demand: demand("find.read"),
            }],
            analyzers: vec![find::Analyzer {
                reference: analyzer,
                configuration: b"unicode-v1".to_vec(),
            }],
            features: vec![find::Feature {
                reference: find::FeatureRef {
                    schema: reference,
                    name: SchemaId::parse("semantic").unwrap(),
                },
                stamp: [5; 32],
            }],
            ops: find::OpSet::ALL,
            modes: find::ModeSet::ALL,
            bound: find_bound(100),
        }
    }

    fn exec_spec(name: &str, find_name: Option<&str>) -> exec::Spec {
        let payload = |name: &str| exec::PayloadSpec {
            schema: exec::SchemaRef {
                name: SchemaId::parse(name).unwrap(),
                version: 1,
            },
            max_inline_bytes: 1_024,
            max_content_refs: 1,
            max_content_bytes: 4_096,
            read: demand("payload.read"),
            max_additional_input_bytes: 0,
        };
        let queries = find_name
            .map(|name| {
                vec![find::Grant {
                    schemas: vec![find_ref(name, 1)],
                    ops: find::OpSet::SEEK,
                    fields: Vec::new(),
                    edges: Vec::new(),
                    gates: Vec::new(),
                    modes: find::ModeSet::EXACT,
                    features: Vec::new(),
                    bound: find_bound(10),
                }]
            })
            .unwrap_or_default();
        exec::Spec {
            name: SchemaId::parse(name).unwrap(),
            version: 1,
            access: exec::Access {
                start: demand("exec.start"),
                offer: demand("exec.offer"),
                control: demand("exec.control"),
                accept: demand("exec.accept"),
            },
            input: payload("exec.input"),
            output: payload("exec.output"),
            mode: exec::Mode::Unary,
            resume: exec::Resume::Restart,
            effects: exec::Effects::Pure,
            accept: exec::AcceptRule::World,
            queries,
            service: None,
            links: Vec::new(),
            limits: exec::Limits {
                attempts: 2,
                events: 64,
                checkpoints: 0,
                child_runs: 2,
                progress_bytes: 4_096,
                checkpoint_bytes: 0,
                wall_millis: 30_000,
            },
        }
    }

    #[test]
    fn roundtrip_empty_min_max_schema_sets() {
        for schemas in [
            vec![],
            vec![schema("a", 1)],
            (0..16).map(|i| schema(&format!("s{i:02}"), 1)).collect(),
        ] {
            let d = descriptor(&schemas);
            let bytes = d.encode().unwrap();
            let back = Implementation::decode(&bytes).unwrap();
            assert_eq!(d, back);
            assert_eq!(d.id().unwrap(), back.id().unwrap());
        }
    }

    #[test]
    fn reordered_and_duplicate_schemas_reject() {
        let d = descriptor(&[schema("aa", 1), schema("bb", 1)]);
        let good = d.encode().unwrap();
        Implementation::decode(&good).unwrap();

        // Manually swap the two schema entries.
        let mut manual = d.clone();
        manual.schemas.swap(0, 1);
        assert_eq!(manual.encode(), Err(Invalid::UnsortedOrDuplicateSchemas));
        // A duplicated entry rejects on decode.
        let mut dup = d.clone();
        dup.schemas = vec![d.schemas[0].clone(), d.schemas[0].clone()];
        assert!(dup.encode().is_err());
    }

    #[test]
    fn every_field_perturbation_changes_the_id() {
        let base = descriptor(&[schema("aa", 1)]);
        let base_id = base.id().unwrap();
        let mut d = base.clone();
        d.policy_protocol = 2;
        assert_ne!(d.id().unwrap(), base_id);
        let mut d = base.clone();
        d.implementation_version = 8;
        assert_ne!(d.id().unwrap(), base_id);
        let mut d = base.clone();
        d.world = WorldId::parse("com.example.other").unwrap();
        assert_ne!(d.id().unwrap(), base_id);
        // One-bit substitutions of the two 32-byte identities.
        for byte in 0..32 {
            for bit in 0..8 {
                let mut d = base.clone();
                d.policy_table_commitment[byte] ^= 1 << bit;
                assert_ne!(d.id().unwrap(), base_id, "commitment bit {byte}:{bit}");
                let mut d = base.clone();
                d.artifact_identity[byte] ^= 1 << bit;
                assert_ne!(d.id().unwrap(), base_id, "artifact bit {byte}:{bit}");
            }
        }
    }

    #[test]
    fn unknown_version_and_trailing_bytes_reject() {
        let d = descriptor(&[schema("aa", 1)]);
        let mut bytes = d.encode().unwrap();
        let mut wrong = bytes.clone();
        wrong[1] = 3;
        assert_eq!(
            Implementation::decode(&wrong),
            Err(Invalid::UnknownVersion(3))
        );
        bytes.push(0);
        assert_eq!(Implementation::decode(&bytes), Err(Invalid::TrailingBytes));
    }

    #[test]
    fn a_version_two_encoding_with_no_sections_is_a_second_spelling() {
        // Hand-built, because the encoder cannot produce it: a descriptor with
        // nothing to declare emits version 1, so an empty table is the same
        // value written a second way and there may only be one.
        let d = descriptor(&[schema("aa", 1)]);
        let mut bytes = d.encode().unwrap();
        bytes[1] = DESCRIPTOR_VERSION_SECTIONED as u8;
        bytes.extend_from_slice(&0u16.to_be_bytes());
        assert_eq!(
            Implementation::decode(&bytes),
            Err(Invalid::EmptySectionTable)
        );
    }

    #[test]
    fn sections_round_trip_alone_and_together() {
        for sections in [
            vec![DescriptorSection::ScopeSchemas(vec![scope("board", 64)])],
            vec![DescriptorSection::SignalSchemas(vec![
                signal("note", 1024),
                signal("typing", 8),
            ])],
            vec![DescriptorSection::FindSchemas(vec![find_schema("notes")])],
            vec![DescriptorSection::ExecSpecs(vec![exec_spec(
                "summarize",
                None,
            )])],
            vec![
                DescriptorSection::ScopeSchemas(vec![scope("board", 64), scope("card", 32)]),
                DescriptorSection::SignalSchemas(vec![signal("note", 1024)]),
                DescriptorSection::FindSchemas(vec![find_schema("notes")]),
                DescriptorSection::ExecSpecs(vec![exec_spec("summarize", Some("notes"))]),
            ],
        ] {
            let mut d = descriptor(&[schema("aa", 1)]);
            d.sections = sections;
            let bytes = d.encode().unwrap();
            assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 2);
            let back = Implementation::decode(&bytes).unwrap();
            assert_eq!(d, back);
            assert_eq!(d.id().unwrap(), back.id().unwrap());
        }
    }

    #[test]
    fn a_declaring_world_builds_its_sections_from_its_registration() {
        // Declaration order at the registration is not descriptor order: the
        // constructor sorts, so two builds that list the same declarations
        // differently are the same implementation.
        let mut reg = registration(&[schema("aa", 1)]);
        reg.scope_schemas = vec![scope("card", 32), scope("board", 64)];
        reg.signal_schemas = vec![signal("typing", 8), signal("note", 1024)];
        reg.find_schemas = vec![find_schema("z-notes"), find_schema("a-notes")];
        reg.exec_specs = vec![
            exec_spec("z-run", None),
            exec_spec("a-run", Some("a-notes")),
        ];
        let d = Implementation::from_registration(&reg, 1, [3u8; 32], [4u8; 32]);
        assert_eq!(
            d.sections,
            vec![
                DescriptorSection::ScopeSchemas(vec![scope("board", 64), scope("card", 32)]),
                DescriptorSection::SignalSchemas(vec![signal("note", 1024), signal("typing", 8)]),
                DescriptorSection::FindSchemas(vec![
                    find_schema("a-notes"),
                    find_schema("z-notes"),
                ]),
                DescriptorSection::ExecSpecs(vec![
                    exec_spec("a-run", Some("a-notes")),
                    exec_spec("z-run", None),
                ]),
            ]
        );
        assert_eq!(d.implementation_version, 7);
        Implementation::decode(&d.encode().unwrap()).unwrap();
    }

    #[test]
    fn a_swapped_or_repeated_section_table_rejects() {
        let mut d = descriptor(&[schema("aa", 1)]);
        d.sections = vec![
            DescriptorSection::SignalSchemas(vec![signal("note", 1024)]),
            DescriptorSection::ScopeSchemas(vec![scope("board", 64)]),
        ];
        assert_eq!(d.encode(), Err(Invalid::UnsortedOrDuplicateSections));

        d.sections = vec![
            DescriptorSection::ScopeSchemas(vec![scope("board", 64)]),
            DescriptorSection::ScopeSchemas(vec![scope("card", 32)]),
        ];
        assert_eq!(d.encode(), Err(Invalid::UnsortedOrDuplicateSections));
    }

    #[test]
    fn only_the_canonical_order_of_every_section_permutation_encodes() {
        let sections = [
            DescriptorSection::ScopeSchemas(vec![scope("board", 64)]),
            DescriptorSection::SignalSchemas(vec![signal("note", 1_024)]),
            DescriptorSection::FindSchemas(vec![find_schema("notes")]),
            DescriptorSection::ExecSpecs(vec![exec_spec("summarize", None)]),
        ];
        let canonical_tags = [
            section::SCOPE_SCHEMAS,
            section::SIGNAL_SCHEMAS,
            section::FIND_SCHEMAS,
            section::EXEC_SPECS,
        ];

        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        if [a, b, c, d]
                            .iter()
                            .copied()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            != 4
                        {
                            continue;
                        }
                        let mut descriptor = descriptor(&[schema("aa", 1)]);
                        descriptor.sections = vec![
                            sections[a].clone(),
                            sections[b].clone(),
                            sections[c].clone(),
                            sections[d].clone(),
                        ];
                        let tags: Vec<_> = descriptor
                            .sections
                            .iter()
                            .map(DescriptorSection::tag)
                            .collect();
                        if tags == canonical_tags {
                            let bytes = descriptor.encode().unwrap();
                            assert_eq!(Implementation::decode(&bytes), Ok(descriptor));
                        } else {
                            assert_eq!(
                                descriptor.encode(),
                                Err(Invalid::UnsortedOrDuplicateSections),
                                "permutation {tags:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_or_misordered_section_rejects() {
        let mut d = descriptor(&[schema("aa", 1)]);
        d.sections = vec![DescriptorSection::ScopeSchemas(Vec::new())];
        assert_eq!(d.encode(), Err(Invalid::BadSection(section::SCOPE_SCHEMAS)));

        // Two ceilings for one name is what sorting by name rather than by
        // whole-entry bytes makes unrepresentable.
        d.sections = vec![DescriptorSection::SignalSchemas(vec![
            signal("note", 8),
            signal("note", 16),
        ])];
        assert_eq!(
            d.encode(),
            Err(Invalid::UnsortedOrDuplicateDeclarations(
                section::SIGNAL_SCHEMAS
            ))
        );

        d.sections = vec![DescriptorSection::ExecSpecs(vec![
            exec_spec("z-run", None),
            exec_spec("a-run", None),
        ])];
        assert_eq!(
            d.encode(),
            Err(Invalid::UnsortedOrDuplicateDeclarations(
                section::EXEC_SPECS
            ))
        );
    }

    #[test]
    fn every_section_kind_rejects_duplicate_declaration_coordinates() {
        let find = find_schema("notes");
        let exec = exec_spec("summarize", None);
        for (tag, section) in [
            (
                section::SCOPE_SCHEMAS,
                DescriptorSection::ScopeSchemas(vec![scope("board", 32), scope("board", 64)]),
            ),
            (
                section::SIGNAL_SCHEMAS,
                DescriptorSection::SignalSchemas(vec![signal("note", 8), signal("note", 16)]),
            ),
            (
                section::FIND_SCHEMAS,
                DescriptorSection::FindSchemas(vec![find.clone(), find]),
            ),
            (
                section::EXEC_SPECS,
                DescriptorSection::ExecSpecs(vec![exec.clone(), exec]),
            ),
        ] {
            let mut descriptor = descriptor(&[schema("aa", 1)]);
            descriptor.sections = vec![section];
            assert_eq!(
                descriptor.encode(),
                Err(Invalid::UnsortedOrDuplicateDeclarations(tag)),
                "section {tag:#06x}"
            );
        }
    }

    #[test]
    fn exec_section_tag_bytes_and_find_containment_are_frozen() {
        let mut registration = registration(&[schema("aa", 1)]);
        registration.find_schemas = vec![find_schema("notes")];
        registration.exec_specs = vec![exec_spec("summarize", Some("notes"))];
        let descriptor = Implementation::from_registration(&registration, 1, [3; 32], [4; 32]);
        assert_eq!(
            descriptor
                .sections
                .iter()
                .map(DescriptorSection::tag)
                .collect::<Vec<_>>(),
            vec![section::FIND_SCHEMAS, section::EXEC_SPECS]
        );
        let bytes = descriptor.encode().unwrap();
        assert_eq!(Implementation::decode(&bytes), Ok(descriptor.clone()));
        assert_eq!(
            blake3::hash(&bytes).to_hex().as_str(),
            "9673ac7603f569cc978a702750ae6d63b0d0819658b4d9fdf059824a1870b074"
        );

        let mut missing_find = descriptor;
        missing_find
            .sections
            .retain(|section| !matches!(section, DescriptorSection::FindSchemas(_)));
        assert_eq!(
            missing_find.encode(),
            Err(Invalid::BadSection(section::EXEC_SPECS))
        );
    }

    #[test]
    fn a_list_no_count_word_can_describe_refuses_instead_of_wrapping() {
        // The count is a `u16`. Truncating it would produce a descriptor whose
        // id is derived over bytes `decode` refuses — an encoding that does not
        // decode, which is what the re-encode backstop exists to make
        // impossible.
        let mut d = descriptor(&[schema("aa", 1)]);
        let scopes: Vec<ScopeSchema> = (0..=MAX_ENCODABLE_ENTRIES)
            .map(|i| scope(&format!("s{i:05}"), 8))
            .collect();
        d.sections = vec![DescriptorSection::ScopeSchemas(scopes.clone())];
        assert_eq!(
            d.encode(),
            Err(Invalid::TooManyDeclarations(section::SCOPE_SCHEMAS))
        );

        // And the largest list that does have a spelling still round-trips.
        d.sections = vec![DescriptorSection::ScopeSchemas(
            scopes[..MAX_ENCODABLE_ENTRIES].to_vec(),
        )];
        let bytes = d.encode().unwrap();
        assert_eq!(Implementation::decode(&bytes).unwrap(), d);

        d.sections = Vec::new();
        d.schemas = (0..=MAX_ENCODABLE_ENTRIES)
            .map(|i| (i as u32).to_be_bytes().to_vec())
            .collect();
        assert_eq!(d.encode(), Err(Invalid::TooManySchemas));
    }

    #[test]
    fn an_unknown_section_tag_rejects_rather_than_being_skipped() {
        // Skipping would make the id a digest over bytes this build did not
        // interpret, which is what the version word already refuses to allow.
        let d = descriptor(&[schema("aa", 1)]);
        let mut bytes = d.encode().unwrap();
        bytes[1] = DESCRIPTOR_VERSION_SECTIONED as u8;
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0x0009u16.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
        assert_eq!(
            Implementation::decode(&bytes),
            Err(Invalid::UnknownSectionTag(0x0009))
        );
    }

    #[test]
    fn a_section_may_not_run_short_of_or_past_its_own_length() {
        let mut d = descriptor(&[schema("aa", 1)]);
        d.sections = vec![DescriptorSection::ScopeSchemas(vec![scope("board", 64)])];
        let good = d.encode().unwrap();

        // A byte hidden in the section's own tail is how a frozen grammar would
        // otherwise be extended.
        let mut fat = good.clone();
        let len_at = good.len() - d.sections[0].encode_payload().unwrap().len() - 4;
        let len = u32::from_be_bytes(good[len_at..len_at + 4].try_into().unwrap());
        fat[len_at..len_at + 4].copy_from_slice(&(len + 1).to_be_bytes());
        fat.push(0);
        assert_eq!(
            Implementation::decode(&fat),
            Err(Invalid::BadSection(section::SCOPE_SCHEMAS))
        );

        // And a length that stops inside an entry is the section's fault, not
        // the record's: the enclosing length already said how many bytes there
        // were.
        let mut short = good.clone();
        short[len_at..len_at + 4].copy_from_slice(&(len - 1).to_be_bytes());
        short.pop();
        assert_eq!(
            Implementation::decode(&short),
            Err(Invalid::BadSection(section::SCOPE_SCHEMAS))
        );
    }

    #[test]
    fn adversarial_lengths_reject_before_section_payload_allocation() {
        let raw_section = |tag: u16, payload: &[u8]| {
            let mut bytes = descriptor(&[schema("aa", 1)]).encode().unwrap();
            bytes[1] = DESCRIPTOR_VERSION_SECTIONED as u8;
            bytes.extend_from_slice(&1u16.to_be_bytes());
            bytes.extend_from_slice(&tag.to_be_bytes());
            bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            bytes.extend_from_slice(payload);
            bytes
        };

        // Scope and Signal first read their bounded u16 name length; Find and
        // Exec first read their bounded u32 standalone-entry length. None may
        // allocate from the claimed size before proving those bytes exist.
        for tag in [section::SCOPE_SCHEMAS, section::SIGNAL_SCHEMAS] {
            let bytes = raw_section(tag, &[0, 1, 0xff, 0xff]);
            assert_eq!(
                Implementation::decode(&bytes),
                Err(Invalid::BadSection(tag))
            );
        }
        for tag in [section::FIND_SCHEMAS, section::EXEC_SPECS] {
            let mut payload = 1u16.to_be_bytes().to_vec();
            payload.extend_from_slice(&u32::MAX.to_be_bytes());
            let bytes = raw_section(tag, &payload);
            assert_eq!(
                Implementation::decode(&bytes),
                Err(Invalid::BadSection(tag))
            );
        }

        // The enclosing table applies the same rule before dispatching a
        // section grammar at all.
        let mut bytes = descriptor(&[schema("aa", 1)]).encode().unwrap();
        bytes[1] = DESCRIPTOR_VERSION_SECTIONED as u8;
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&section::EXEC_SPECS.to_be_bytes());
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(Implementation::decode(&bytes), Err(Invalid::Truncated));
    }

    #[test]
    fn a_declaration_and_its_bound_are_both_in_the_id() {
        let base = descriptor(&[schema("aa", 1)]);
        let base_id = base.id().unwrap();

        let mut with_scope = base.clone();
        with_scope.sections = vec![DescriptorSection::ScopeSchemas(vec![scope("board", 64)])];
        let scoped_id = with_scope.id().unwrap();
        assert_ne!(scoped_id, base_id);

        let mut retuned = with_scope.clone();
        retuned.sections = vec![DescriptorSection::ScopeSchemas(vec![scope("board", 32)])];
        assert_ne!(retuned.id().unwrap(), scoped_id);

        let mut with_both = with_scope.clone();
        with_both
            .sections
            .push(DescriptorSection::SignalSchemas(vec![signal("note", 1024)]));
        assert_ne!(with_both.id().unwrap(), scoped_id);

        let mut redemanded = with_both.clone();
        redemanded.sections[1] = DescriptorSection::SignalSchemas(vec![SignalSchema {
            demand: vec![7, 7, 7],
            ..signal("note", 1024)
        }]);
        assert_ne!(redemanded.id().unwrap(), with_both.id().unwrap());
    }

    #[test]
    fn every_find_semantic_coordinate_is_identity_material() {
        let mut declared = descriptor(&[schema("aa", 1)]);
        declared.sections = vec![DescriptorSection::FindSchemas(vec![find_schema("notes")])];
        let declared_id = declared.id().unwrap();

        let changed = |mut declaration: find::Schema| {
            let mut descriptor = descriptor(&[schema("aa", 1)]);
            declaration = declaration.canonicalized();
            descriptor.sections = vec![DescriptorSection::FindSchemas(vec![declaration])];
            assert_ne!(descriptor.id().unwrap(), declared_id);
        };

        let mut semantic = find_schema("notes");
        semantic.fields[0].kind = find::FieldKind::Bytes;
        semantic.fields[0].analyzer = None;
        changed(semantic);

        let mut gate = find_schema("notes");
        gate.gates[0].demand = demand("find.other");
        changed(gate);

        let mut analyzer = find_schema("notes");
        analyzer.analyzers[0].configuration.push(1);
        changed(analyzer);

        let mut feature = find_schema("notes");
        feature.features[0].stamp[0] ^= 1;
        changed(feature);

        let mut operators = find_schema("notes");
        operators.ops = find::OpSet::SEEK;
        changed(operators);

        let mut bound = find_schema("notes");
        bound.bound.wall_millis -= 1;
        changed(bound);
    }

    #[test]
    fn the_sectionless_encoding_is_byte_identical_to_what_shipped() {
        // Generated from the build before the section table existed and pinned
        // here, because the whole claim of the two-version rule is that a World
        // declaring nothing keeps the id an authority already activated. Nothing
        // else in the tree pins an implementation id as a literal.
        let d = descriptor(&[schema("aa", 1)]);
        assert_eq!(d.version(), DESCRIPTOR_VERSION_SECTIONLESS);
        let hex: String = d.id().unwrap().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "526e78499a134f4b99e8cfe7a1e92368fbfc3264d33164f2206434070068ac3e"
        );
    }

    #[test]
    fn policy_table_commitment_is_content_bound() {
        assert_ne!(
            policy_table_commitment(b"table-a"),
            policy_table_commitment(b"table-b")
        );
    }
}
