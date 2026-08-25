//! Attested names, and resolution relative to the reader.
//!
//! The reflex when a system needs human-readable identities is a global
//! namespace: one authority, unique bindings, first-come-first-served, and a
//! dispute process bolted on once squatting arrives. There is no authority here
//! to be the registrar, and inventing one would put a central naming service
//! underneath a local-first system.
//!
//! So a name is conferred by a circle and valid within it. Resolution answers
//! with a **ranked set**, never a unique binding, and two parties known by one
//! name are both correct — to different readers. Collisions are the normal case
//! rather than a failure mode, and the ranking is what makes them navigable.
//!
//! Nothing here confers authority. A name still cannot shadow a key, an id, or a
//! unique id prefix, and every authority resolver continues to ignore this
//! module entirely. What changes is only that a reader can now say *how well
//! supported* a name is, instead of treating every claim as equal.

use mechanics::ids::DeviceId;
use mechanics::kinship::{Audience, Avowal, Claim, Party, Standing};

/// How much one attestation counts, by how internal its audience is.
///
/// Ordered by what the audience actually costs the attestor to make: a name
/// avowed to one counterparty is a statement they can be held to, and one
/// published to the directory is a statement to nobody in particular. This is
/// the mechanical form of "tiers of internal-ness" — and it is *this reader's*
/// ordering, not a global truth.
const fn tier_weight(audience: &Audience) -> u32 {
    match audience {
        Audience::Correspondent(_) => 8,
        Audience::Own | Audience::Kin => 4,
        Audience::Members(_) => 2,
        Audience::Public => 1,
    }
}

/// One name, as this reader resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedName {
    pub name: String,
    /// The subject avowed this name itself. Worth exactly what a self-signature
    /// is worth, which is why it contributes nothing to `weight`.
    pub declared: bool,
    /// Attestations by other parties that this reader is permitted to read.
    pub attestations: usize,
    /// Rank for this reader. Higher is better supported. Never comparable
    /// across readers, because the inputs are not the same set.
    pub weight: u32,
}

/// Every name known for `subject`, ranked for this reader.
///
/// `subject_devices` is what separates an avowal from an attestation: a
/// signature by a device of the subject is the subject speaking. The caller
/// resolves that set rather than this function guessing it, because a guess
/// would silently promote every attestation to a self-claim or the reverse.
///
/// An empty result means no name was avowed *that this reader may read*. It
/// never means the subject does not exist, and callers must not render it that
/// way.
#[must_use]
pub fn names_of(
    subject: &Party,
    avowals: &[Avowal],
    reader: &Standing,
    subject_devices: &[DeviceId],
) -> Vec<ResolvedName> {
    let mut found: Vec<ResolvedName> = Vec::new();
    for avowal in readable_names(subject, avowals, reader) {
        let Claim::Called(name) = &avowal.claim else {
            continue;
        };
        let is_self = avowal.is_self_signed(subject_devices);
        let slot = match found.iter_mut().find(|held| &held.name == name) {
            Some(slot) => slot,
            None => {
                found.push(ResolvedName {
                    name: name.clone(),
                    declared: false,
                    attestations: 0,
                    weight: 0,
                });
                match found.last_mut() {
                    Some(slot) => slot,
                    None => continue,
                }
            }
        };
        if is_self {
            slot.declared = true;
        } else {
            slot.attestations = slot.attestations.saturating_add(1);
            slot.weight = slot.weight.saturating_add(tier_weight(&avowal.audience));
        }
    }
    sort_by_support(&mut found);
    found
}

/// Which parties this reader knows by `name`, best-supported first.
///
/// Two subjects avowing one name both appear. There is deliberately no dispute
/// path, no uniqueness check and nothing to squat — a collision is answered by
/// ranking, not by refusing the second claimant.
#[must_use]
pub fn parties_called(name: &str, avowals: &[Avowal], reader: &Standing) -> Vec<(Party, u32)> {
    let mut found: Vec<(Party, u32)> = Vec::new();
    for avowal in avowals {
        if avowal.verify().is_err() || avowal.legible_to(reader).is_err() {
            continue;
        }
        let Claim::Called(claimed) = &avowal.claim else {
            continue;
        };
        if claimed != name {
            continue;
        }
        let weight = tier_weight(&avowal.audience);
        if let Some(slot) = found.iter_mut().find(|(party, _)| party == &avowal.subject) {
            slot.1 = slot.1.saturating_add(weight);
        } else {
            found.push((avowal.subject.clone(), weight));
        }
    }
    // Descending support, then the party's wire spelling so the order is total
    // and reproducible rather than dependent on input order.
    found.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.wire().cmp(&right.0.wire()))
    });
    found
}

/// The naming avowals about `subject` this reader may actually read.
///
/// Verification and legibility are separate questions and both are asked here:
/// a well-signed avowal shown out of tier is skipped exactly as a forged one is,
/// and neither is reported as the subject having no name.
fn readable_names<'a>(
    subject: &'a Party,
    avowals: &'a [Avowal],
    reader: &'a Standing,
) -> impl Iterator<Item = &'a Avowal> {
    avowals.iter().filter(move |avowal| {
        &avowal.subject == subject
            && matches!(avowal.claim, Claim::Called(_))
            && avowal.verify().is_ok()
            && avowal.legible_to(reader).is_ok()
    })
}

fn sort_by_support(names: &mut [ResolvedName]) {
    names.sort_by(|left, right| {
        right
            .weight
            .cmp(&left.weight)
            .then_with(|| right.attestations.cmp(&left.attestations))
            .then_with(|| left.name.cmp(&right.name))
    });
}

/// A subject's portrait, as this reader resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPortrait {
    /// The picture's content hash, when one is presented.
    pub picture: Option<[u8; 32]>,
    /// The self-description line. May be empty.
    pub detail: String,
    /// The avowal epoch this resolution came from.
    pub epoch: u64,
}

/// The subject's own filter: an avowal about one of `subject_devices`, signed
/// by one of them. Unlike a name, which anyone may attest and the ranking
/// weighs, these claims have exactly one party who may speak — the subject.
/// Somebody else's signature about how you present is skipped, not ranked
/// low, because no weight makes it admissible.
fn self_spoken<'a>(
    avowals: &'a [Avowal],
    reader: &'a Standing,
    subject_devices: &'a [mechanics::ids::DeviceId],
) -> impl Iterator<Item = &'a Avowal> {
    avowals.iter().filter(move |avowal| {
        matches!(&avowal.subject, Party::Device(device) if subject_devices.contains(device))
            && avowal.is_self_signed(subject_devices)
            && avowal.verify().is_ok()
            && avowal.legible_to(reader).is_ok()
    })
}

/// The latest of a subject's own claims, under a total order — epoch first,
/// then the signature bytes, so two artifacts at one epoch resolve the same
/// way for every reader rather than by input order.
fn latest<'a>(picked: impl Iterator<Item = &'a Avowal>) -> Option<&'a Avowal> {
    picked.max_by(|left, right| {
        left.epoch
            .cmp(&right.epoch)
            .then_with(|| left.signature.bytes().cmp(right.signature.bytes()))
    })
}

/// The subject's portrait: its latest self-signed [`Claim::Portrait`] this
/// reader may read. `None` never means "has no portrait" — only that none was
/// avowed where this reader could see it.
#[must_use]
pub fn portrait_of(
    avowals: &[Avowal],
    reader: &Standing,
    subject_devices: &[mechanics::ids::DeviceId],
) -> Option<ResolvedPortrait> {
    let picked = self_spoken(avowals, reader, subject_devices)
        .filter(|avowal| matches!(avowal.claim, Claim::Portrait { .. }));
    let avowal = latest(picked)?;
    let Claim::Portrait { picture, detail } = &avowal.claim else {
        return None;
    };
    Some(ResolvedPortrait {
        picture: *picture,
        detail: detail.clone(),
        epoch: avowal.epoch,
    })
}

/// The name the subject most recently declared for itself, if this reader may
/// read one. Worth exactly what a self-claim is worth — [`names_of`] is the
/// ranked resolution; this is only "what do *they* call themselves".
#[must_use]
pub fn declared_by(
    avowals: &[Avowal],
    reader: &Standing,
    subject_devices: &[mechanics::ids::DeviceId],
) -> Option<String> {
    let picked = self_spoken(avowals, reader, subject_devices)
        .filter(|avowal| matches!(avowal.claim, Claim::Called(_)));
    let avowal = latest(picked)?;
    let Claim::Called(name) = &avowal.claim else {
        return None;
    };
    Some(name.clone())
}
