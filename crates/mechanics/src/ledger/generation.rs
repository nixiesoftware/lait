//! Construction of a current Mechanics generation from committed prior facts.

use std::path::Path;

use super::{Authority, Failure, LedgerMeta};

/// Evidence that the rebuilt authority ledger exposes the same authoritative
/// facts and frontier as its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verification {
    evidence: [u8; 32],
    effects: u64,
}

impl Verification {
    pub fn evidence(self) -> [u8; 32] {
        self.evidence
    }

    pub fn effects(self) -> u64 {
        self.effects
    }
}

/// Build and verify a current authority ledger from a prior committed store.
/// Prior checkpoints are treated as derived material: opening the target
/// verifies them at the current replay-semantics version and deterministically
/// rebuilds a stale current checkpoint from signed effects.
pub fn build_prior(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<Verification, Failure> {
    let source = journal::GenerationSource::open(source.as_ref())?;
    let meta: LedgerMeta = postcard::from_bytes(source.meta()).map_err(Failure::corrupt)?;
    if meta.version != 1 || postcard::to_stdvec(&meta).map_err(Failure::corrupt)? != source.meta() {
        return Err(Failure::corrupt("non-canonical prior ledger metadata"));
    }

    let evidence = fact_evidence(&meta)?;
    let frontier = meta.frontier.clone();
    let space = meta.genesis.space_id.clone();
    let effects = u64::try_from(meta.effects.len()).unwrap_or(u64::MAX);
    let mut added = Vec::with_capacity(source.objects().len());
    for object in source.objects() {
        added.push(source.read_object(object)?);
    }

    let target_path = target.as_ref().to_path_buf();
    let mut store = journal::Store::open(&target_path)?;
    if store.manifest().is_some() {
        return Err(Failure::corrupt("generation target already holds a ledger"));
    }
    store.commit_required_set(&added, &[], source.meta().to_vec())?;
    drop(store);

    let rebuilt = Authority::open(&target_path)?;
    if rebuilt.space() != &space
        || rebuilt.frontier() != frontier
        || u64::try_from(rebuilt.export_effects().len()).unwrap_or(u64::MAX) != effects
    {
        return Err(Failure::corrupt(
            "rebuilt ledger is not equivalent to its source",
        ));
    }
    drop(rebuilt);
    let target_store = journal::Store::open(target_path)?;
    let target_meta = target_store
        .caller_meta()?
        .ok_or_else(|| Failure::corrupt("rebuilt ledger has no metadata"))?;
    let target_meta: LedgerMeta = postcard::from_bytes(&target_meta).map_err(Failure::corrupt)?;
    if fact_evidence(&target_meta)? != evidence {
        return Err(Failure::corrupt(
            "rebuilt ledger changed authoritative facts",
        ));
    }
    Ok(Verification { evidence, effects })
}

fn fact_evidence(meta: &LedgerMeta) -> Result<[u8; 32], Failure> {
    // Checkpoints are intentionally absent: they are verified materializations
    // of signed effects and may change when replay semantics change.
    let facts = (
        &meta.genesis,
        &meta.effects,
        &meta.sealed,
        &meta.receipts,
        &meta.frontier,
        &meta.ceremony,
        meta.ceremony_next_seq,
        &meta.ceremony_audits,
    );
    let bytes = postcard::to_stdvec(&facts).map_err(Failure::corrupt)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait/mechanics-generation/1/equivalence");
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}
