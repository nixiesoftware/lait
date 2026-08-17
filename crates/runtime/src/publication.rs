//! The complete identity of one World read publication.
//!
//! A Replica Manifest root identifies durable Body material. It does not say
//! which authority-approved World implementation interprets that material, or
//! which extractor declaration produced the secondary corpus used by Find and
//! semantic projections. A read coordinate that carries only the root can
//! therefore silently reinterpret the same Bodies after a World activation.
//!
//! `PublicationId` is the indivisible coordinate used by cursors, retained
//! projections, and derived-corpus ownership. Authority remains a per-request
//! coordinate: the corpus is principal-neutral and disclosure gates are
//! evaluated when it is read.

use serde::{Deserialize, Serialize};

use crate::find;

const EXTRACTOR_SCHEMA_CONTEXT: &str = "lait.extractor-schema.v1";

/// The canonical commitment to every declaration that controls corpus shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExtractorSchemaDigest([u8; 32]);

impl ExtractorSchemaDigest {
    /// Derive the digest from canonical Find schemas and their exact source
    /// bindings. Registration order is deliberately immaterial.
    pub fn derive(
        schemas: &[find::Schema],
        extractors: &[find::Extractor],
    ) -> Result<Self, find::Invalid> {
        let mut schemas: Vec<find::Schema> =
            schemas.iter().map(find::Schema::canonicalized).collect();
        schemas.sort_by(|left, right| left.reference.cmp(&right.reference));
        let mut extractors = extractors.to_vec();
        extractors.sort();

        let mut material = Vec::new();
        push_len(&mut material, schemas.len());
        for schema in schemas {
            schema.validate()?;
            let encoded = schema.encode()?;
            push_bytes(&mut material, &encoded);
        }
        push_len(&mut material, extractors.len());
        for extractor in extractors {
            push_name(&mut material, &extractor.schema.name);
            material.extend_from_slice(&extractor.schema.version.to_be_bytes());
            push_name(&mut material, &extractor.source.name);
            material.extend_from_slice(&extractor.source.version.to_be_bytes());
            material.extend_from_slice(&extractor.abi_version.to_be_bytes());
            material.extend_from_slice(&extractor.semantic_digest);
        }
        Ok(Self(blake3::derive_key(
            EXTRACTOR_SCHEMA_CONTEXT,
            &material,
        )))
    }

    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

/// Manifest, interpretation, and extraction identity for one published World
/// read generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PublicationId {
    pub manifest_root: [u8; 32],
    pub implementation_digest: [u8; 32],
    pub extractor_schema_digest: ExtractorSchemaDigest,
}

impl PublicationId {
    pub const fn new(
        manifest_root: [u8; 32],
        implementation_digest: [u8; 32],
        extractor_schema_digest: ExtractorSchemaDigest,
    ) -> Self {
        Self {
            manifest_root,
            implementation_digest,
            extractor_schema_digest,
        }
    }
}

/// Station-local identity for one readable materialization of a Manifest.
///
/// A Manifest root can remain unchanged while newly arrived authority or keys
/// make previously opaque Body material readable. That transition must move
/// every local cursor and derived-cache coordinate even though the portable
/// semantic [`PublicationId`] does not. The Station epoch supplies restart
/// identity; this sequence supplies ordering inside one activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MaterializationId(u64);

impl MaterializationId {
    pub const INITIAL: Self = Self(1);

    pub const fn from_u64(value: u64) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Complete local identity of one ready World read image.
///
/// `PublicationId` is safe to persist in semantic records. The
/// materialization component is deliberately local and belongs only in
/// cursors, leases, and caches owned by this Station activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorldPublicationId {
    pub publication: PublicationId,
    pub materialization: MaterializationId,
}

impl WorldPublicationId {
    pub const fn new(publication: PublicationId, materialization: MaterializationId) -> Self {
        Self {
            publication,
            materialization,
        }
    }
}

fn push_len(out: &mut Vec<u8>, len: usize) {
    let len = u64::try_from(len).unwrap_or(u64::MAX);
    out.extend_from_slice(&len.to_be_bytes());
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    push_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn push_name(out: &mut Vec<u8>, name: &replica::body::SchemaId) {
    push_bytes(out, name.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::body::SchemaId;

    fn schema(name: &str, source: &str) -> find::Schema {
        let reference = find::SchemaRef {
            name: SchemaId::parse(name).unwrap(),
            version: 1,
        };
        find::Schema {
            reference,
            sources: vec![find::SourceRef {
                name: SchemaId::parse(source).unwrap(),
                version: 1,
            }],
            fields: Vec::new(),
            edges: Vec::new(),
            gates: Vec::new(),
            analyzers: Vec::new(),
            features: Vec::new(),
            ops: find::OpSet::SEEK,
            modes: find::ModeSet::EXACT,
            bound: find::Policy::default().bound,
        }
    }

    fn extractor(schema: &find::Schema) -> find::Extractor {
        find::Extractor {
            schema: schema.reference.clone(),
            source: schema.sources[0].clone(),
            abi_version: find::EXTRACTOR_ABI_VERSION,
            semantic_digest: [7; 32],
        }
    }

    #[test]
    fn extractor_identity_is_canonical_and_semantic() {
        let a = schema("issues.issue", "issues.issue-body");
        let b = schema("issues.project", "issues.project-body");
        let forward =
            ExtractorSchemaDigest::derive(&[a.clone(), b.clone()], &[extractor(&a), extractor(&b)])
                .unwrap();
        let reverse =
            ExtractorSchemaDigest::derive(&[b.clone(), a.clone()], &[extractor(&b), extractor(&a)])
                .unwrap();
        assert_eq!(forward, reverse);

        let mut changed = a.clone();
        changed.bound.nodes_visited = changed.bound.nodes_visited.saturating_add(1);
        assert_ne!(
            forward,
            ExtractorSchemaDigest::derive(
                &[changed.clone(), b],
                &[extractor(&changed), extractor(&a)],
            )
            .unwrap()
        );

        let mut changed_semantics = extractor(&a);
        changed_semantics.semantic_digest[0] ^= 1;
        assert_ne!(
            forward,
            ExtractorSchemaDigest::derive(
                &[a.clone(), schema("issues.project", "issues.project-body")],
                &[
                    changed_semantics,
                    extractor(&schema("issues.project", "issues.project-body"))
                ],
            )
            .unwrap()
        );
    }

    #[test]
    fn every_publication_coordinate_is_independent() {
        let extractor = ExtractorSchemaDigest::from_digest([3; 32]);
        let base = PublicationId::new([1; 32], [2; 32], extractor);
        assert_ne!(base, PublicationId::new([4; 32], [2; 32], extractor));
        assert_ne!(base, PublicationId::new([1; 32], [4; 32], extractor));
        assert_ne!(
            base,
            PublicationId::new(
                [1; 32],
                [2; 32],
                ExtractorSchemaDigest::from_digest([4; 32]),
            )
        );
        assert_ne!(
            WorldPublicationId::new(base, MaterializationId::INITIAL),
            WorldPublicationId::new(base, MaterializationId::INITIAL.next())
        );
    }
}
