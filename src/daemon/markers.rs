//! The markers this identity follows, one pinned chronicle each.
//!
//! A **marker** is a service that keeps a chronicle and signs what it recorded.
//! Following one is the reader's whole side of that: pin the head, only ever
//! move it along a proven extension of the *same signer*, and keep the
//! irreconcilable pair when the signer contradicts itself. That ratchet lives
//! in [`mechanics::chronicle::advance`]; this module is where a reader's half
//! of it is durable, and where the answer is turned into a sentence a surface
//! can render.
//!
//! # One store, keyed by the signer
//!
//! `<identity>/markers/<by>.json`, one file per marker device, with
//! `<identity>/markers/<by>.diverged` beside it when that device has been
//! caught equivocating. Keyed by **signer** rather than by host because a
//! service moves: the Post that answers at a name today answered from a
//! different machine yesterday, and a pin filed under the host would be lost
//! by the move — which reads, at the surface, exactly like the marker having
//! been replaced. The base is recorded *in* the file so the other direction
//! still works: a different signer answering at a base this identity already
//! follows is [`MarkerStanding::WrongSigner`], not quietly a second marker.
//!
//! The book (`marks.book`) can name the signer with a base, and when it does
//! there is no trust on first use at all — the first answer is checked like
//! every later one. That is the difference between a marker a person chose and
//! whatever key happened to be answering at a host name.
//!
//! # Absence is the fourth thing
//!
//! No file means **never asked**, which is neither "fine" nor "no". A marker
//! that could not be reached is [`MarkerStanding::CouldNotAsk`] *written down*;
//! a marker whose answer did not verify is [`MarkerStanding::Refused`]; a
//! marker that contradicted itself is [`MarkerStanding::Diverged`]. Folding any
//! two of those together is the false-disconnection defect one layer up, and
//! the reason this is an enum with a file behind it rather than a boolean.
//!
//! # A mark confers nothing
//!
//! The marks a receipt carries are verified here and stored beside the pin,
//! *replaced* on every chronicled receipt rather than accumulated: a
//! certification that only ever grew could never be withdrawn, and a device the
//! marker's newest publication does not avow has to lose the tier. Nothing in
//! an ACL, an admission or the net plane reads any of this — a mark says a
//! publication was recorded, in a log, at a position, and that is all it will
//! ever say.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lait_directory::registry::chronicle_over_http;
use lait_directory::Receipt;
use mechanics::chronicle::{advance, Advance, Head, PinnedHead, Refusal};
use mechanics::ids::DeviceId;
use mechanics::kinship::{Avowal, Party};
use serde::{Deserialize, Serialize};

use crate::config::MarkerEntry;

/// Where per-marker records live under an identity directory.
const MARKERS: &str = "markers";

/// The single-registrar pin this store replaces. Adopted once, then removed.
const LEGACY_PIN: &str = "registry-chronicle.pin";
/// Its evidence half, moved with it.
const LEGACY_EVIDENCE: &str = "registry-chronicle.diverged";

/// One mark, kept with the path that makes it checkable on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// The marker's signed statement.
    pub avowal: Avowal,
    /// The inclusion path the marker served with it.
    #[serde(default)]
    pub inclusion: Vec<[u8; 32]>,
    /// Whether *this reader* checked the signature, the inclusion path, and
    /// the mark's place on the chronicle it pinned. An unproven mark is kept
    /// and not rendered: what a marker said and what a reader verified are
    /// different facts, and a surface that showed the first would be quoting.
    pub proven: bool,
}

/// What this identity last learned about one marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerRecord {
    /// The head this reader accepted, whole and signed — not the reduced pin.
    /// On divergence the two irreconcilable heads *are* the evidence, and
    /// evidence with the signature stripped is a claim.
    ///
    /// `None` when this marker has been asked and nothing it said was ever
    /// acceptable. That is a fact worth keeping and is not the same as never
    /// having asked, which is the absence of this file.
    #[serde(default)]
    pub pin: Option<Head>,
    /// Where this marker answered. The other half of the key: it is what lets
    /// a different signer at a base this identity already follows read as
    /// `WrongSigner` instead of silently becoming a second marker.
    pub base: String,
    /// Unix seconds of the last completed check.
    pub checked_at: u64,
    pub standing: MarkerStanding,
    /// The marks from the most recent chronicled receipt, by subject device.
    ///
    /// **Replaced, never merged** — see the module note: this is the whole
    /// mechanism by which a certification is withdrawn.
    #[serde(default)]
    pub marks: BTreeMap<DeviceId, Mark>,
}

/// How the last check went. Every refusal is its own fact because every one of
/// them is acted on differently, and the surface says a different sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "standing", rename_all = "snake_case")]
pub enum MarkerStanding {
    /// The first head this reader accepted from this marker.
    Pinned { size: u64 },
    /// The same head again. The marker has recorded nothing since.
    Unchanged { size: u64 },
    /// A longer chronicle that proved it extends the pin.
    Extended { from: u64, to: u64 },
    /// The marker could not be asked. Never rendered as "uncertified": an
    /// unreachable witness is not a witness who said no, and the last proven
    /// marks stand.
    CouldNotAsk { why: String },
    /// The head came back under a device this reader does not follow. A
    /// refusal to be fooled, not an accusation — anyone can mint a key.
    WrongSigner { pinned: DeviceId, offered: DeviceId },
    /// A chronicle shorter than the pin: a replayed or truncated copy.
    Rollback { pinned: u64, offered: u64 },
    /// Longer, and not shown to extend the pin. Suspicion, never "diverged"
    /// and never "fine".
    Unproven { pinned: u64, offered: u64 },
    /// Two signed heads at one size under one key. The caught lie; both
    /// artifacts are retained beside the record.
    Diverged { size: u64 },
    /// The answer was not a head this reader can verify at all. Categorically
    /// not "could not ask" — the marker answered, and what it said was wrong.
    /// The same split `update::watch::Standing` draws between a host that is
    /// down and bytes that did not verify.
    Refused { why: String },
}

impl MarkerStanding {
    /// The sentence a *publisher* fails on, or `None` when the marker's memory
    /// checked out.
    ///
    /// A publication that cannot be placed in the log it claims to be in is
    /// not a published route, and the daemon says so and serves anyway — a
    /// coordinator that cannot announce is degraded, not absent. Written once,
    /// here, so the refusals stay distinct sentences all the way to the
    /// journal.
    #[must_use]
    pub fn refused(&self) -> Option<String> {
        match self {
            Self::Pinned { .. } | Self::Unchanged { .. } | Self::Extended { .. } => None,
            Self::CouldNotAsk { why } => Some(format!(
                "the marker's chronicle could not be asked ({why}); the pin holds"
            )),
            Self::Refused { why } => Some(format!(
                "the marker's chronicle head did not verify: {why}; the pin holds"
            )),
            Self::WrongSigner { pinned, offered } => Some(format!(
                "the marker's chronicle head is signed by a different device ({}) than the one \
                 this identity pinned ({}) — it is not the holder you followed; the pin holds",
                offered.as_str(),
                pinned.as_str()
            )),
            Self::Rollback { pinned, offered } => Some(format!(
                "the marker served a chronicle head older than the pinned one (size {offered} \
                 against {pinned}) — a replayed or truncated copy; the pin holds"
            )),
            Self::Unproven { pinned, offered } => Some(format!(
                "the marker could not prove its chronicle (size {offered}) extends the pinned \
                 head (size {pinned}) — the pin holds"
            )),
            Self::Diverged { size } => Some(format!(
                "the marker's chronicle DIVERGED from the head this identity pinned (both \
                 signed by the pinned device at size {size}, different roots) — the marker \
                 equivocated, and both artifacts are retained beside the identity as evidence"
            )),
        }
    }
}

fn markers_dir(identity: &Path) -> PathBuf {
    identity.join(MARKERS)
}

fn record_path(identity: &Path, by: &DeviceId) -> PathBuf {
    markers_dir(identity).join(format!("{}.json", by.as_str()))
}

fn evidence_path(identity: &Path, by: &DeviceId) -> PathBuf {
    markers_dir(identity).join(format!("{}.diverged", by.as_str()))
}

/// What this identity holds about one marker, or `None` for never asked.
///
/// A record that does not decode reads as absent — loudly, because a reader
/// that re-pins has no divergence protection for that one step while looking
/// exactly like one that does.
#[must_use]
pub fn load(identity: &Path, by: &DeviceId) -> Option<MarkerRecord> {
    let path = record_path(identity, by);
    let read = |path: &Path| -> Result<MarkerRecord, addressbook::Error> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|_| addressbook::Error::Corrupt("marker record"))
    };
    match addressbook::durable::open_or_recover(&path, read) {
        Ok(record) => record,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "the marker record did not read; this marker re-pins without divergence \
                 protection for one step"
            );
            None
        }
    }
}

/// Record what this identity now holds about a marker. Loud on failure, for
/// the reason the update feed's stamp is: a node that silently cannot persist
/// this has no replay protection while looking exactly like one that does.
pub fn save(identity: &Path, by: &DeviceId, record: &MarkerRecord) {
    let path = record_path(identity, by);
    let Ok(bytes) = serde_json::to_vec_pretty(record) else {
        tracing::warn!(path = %path.display(), "the marker record did not encode");
        return;
    };
    if let Err(error) = addressbook::durable::atomic_replace(&path, &bytes) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not record the marker pin; this identity cannot detect a rewritten chronicle"
        );
    }
}

/// Keep both irreconcilable signed heads. This file existing is the fact a
/// surface reports; its contents are what a third party checks — so a second
/// divergence must not clobber the first's evidence. Written once, never
/// overwritten: the earliest incriminating pair is the one that is kept.
pub fn keep_divergence(identity: &Path, by: &DeviceId, held: &Head, offered: &Head) {
    let path = evidence_path(identity, by);
    if path.exists() {
        tracing::error!(
            path = %path.display(),
            "chronicle divergence again — earlier evidence retained, this pair not overwritten"
        );
        return;
    }
    let Ok(bytes) = serde_json::to_vec(&serde_json::json!({ "held": held, "offered": offered }))
    else {
        return;
    };
    if let Err(error) = addressbook::durable::atomic_replace(&path, &bytes) {
        tracing::error!(
            path = %path.display(),
            %error,
            "could not retain the divergence evidence"
        );
    }
}

/// The marker as this identity's book has it: a base always, and the signer
/// the book named for that base when it named one.
///
/// A base that is in no book is still followed — the registry an operator
/// pointed this daemon at is a marker whether or not anybody wrote it down —
/// it simply has no signer to check the first answer against.
#[must_use]
pub fn entry_for(identity: &Path, base: &str) -> MarkerEntry {
    let base = base.trim_end_matches('/').to_string();
    crate::config::Settings::load(Some(identity))
        .marks_book()
        .unwrap_or_default()
        .into_iter()
        .find(|entry| entry.base == base)
        .unwrap_or(MarkerEntry { base, by: None })
}

/// The head this identity holds for `entry`, if it holds one.
#[must_use]
pub fn pinned(identity: &Path, entry: &MarkerEntry) -> Option<PinnedHead> {
    let (_, record) = following(identity, entry)?;
    record.pin.as_ref().map(PinnedHead::from)
}

/// Which record this entry is about: the book's signer when it names one — so
/// the very first answer is checked — else whichever record is already
/// following this base.
fn following(identity: &Path, entry: &MarkerEntry) -> Option<(DeviceId, MarkerRecord)> {
    if let Some(by) = &entry.by {
        return load(identity, by).map(|record| (by.clone(), record));
    }
    let dir = std::fs::read_dir(markers_dir(identity)).ok()?;
    for found in dir.flatten() {
        let path = found.path();
        if path.extension().is_none_or(|kind| kind != "json") {
            continue;
        }
        let Some(by) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(DeviceId::parse)
        else {
            continue;
        };
        if let Some(record) = load(identity, &by) {
            if record.base == entry.base {
                return Some((by, record));
            }
        }
    }
    None
}

/// Follow one marker: ask its chronicle, ratchet the pin, and — when a receipt
/// came with the call — check the marks it carried against the head that was
/// accepted.
///
/// Blocking: the chronicle is asked over HTTP. The standing is the whole
/// answer, and it is written down before it is returned, so a caller that
/// ignores it still leaves the fact where a surface can find it.
pub fn ratchet(identity: &Path, entry: &MarkerEntry, receipt: Option<&Receipt>) -> MarkerStanding {
    let held = following(identity, entry);
    let pin = held
        .as_ref()
        .and_then(|(_, record)| record.pin.as_ref())
        .map(PinnedHead::from);
    // Where a refusal gets written before any head is known: the book's signer
    // when it names one, else whoever this identity already follows here. With
    // neither, this marker has never been asked and an unanswerable ask leaves
    // no file — absence stays "never asked".
    let known = entry
        .by
        .clone()
        .or_else(|| held.as_ref().map(|(by, _)| by.clone()));

    // Ask for the current head first (no size), so a chronicle now *shorter*
    // than the pin comes back as a head the ratchet reads as Rollback rather
    // than a 404 that would fold into "could not be asked".
    let current = match chronicle_over_http(&entry.base, None) {
        Ok(answer) => answer.head,
        Err(error) => {
            return keep(
                identity,
                known.as_ref(),
                entry,
                held,
                MarkerStanding::CouldNotAsk {
                    why: error.to_string(),
                },
            )
        }
    };

    // The book's signer is checked before the ratchet and before any pin
    // exists. This is the whole of "no trust on first use": a marker that has
    // moved hosts is exactly where a first contact would otherwise pin an
    // impostor, and there would be no later answer that could undo it.
    if let Some(want) = &entry.by {
        if current.by != *want {
            return keep(
                identity,
                known.as_ref(),
                entry,
                held,
                MarkerStanding::WrongSigner {
                    pinned: want.clone(),
                    offered: current.by,
                },
            );
        }
    }

    // A pre-marker identity holds one pin, for one registrar, in a file that
    // records no base at all. Adopt it when the device answering now is the
    // one that signed it — dropping it would re-pin the whole fleet in a
    // single unprotected step, against the service it has followed longest.
    let (held, pin, known) = match pin {
        Some(pin) => (held, Some(pin), known),
        None => match adopt_legacy(identity, &entry.base, &current.by) {
            Some(record) => {
                let pin = record.pin.as_ref().map(PinnedHead::from);
                (
                    Some((current.by.clone(), record)),
                    pin,
                    Some(current.by.clone()),
                )
            }
            None => (held, None, known),
        },
    };

    // If the served head already covers the pin, fetch the consistency path
    // from the pin's size; `advance` judges rollback, divergence, extension.
    let consistency = match &pin {
        Some(pin) if current.size > pin.size => {
            match chronicle_over_http(&entry.base, Some(pin.size)) {
                Ok(answer) => answer.consistency,
                Err(error) => {
                    return keep(
                        identity,
                        known.as_ref(),
                        entry,
                        held,
                        MarkerStanding::CouldNotAsk {
                            why: error.to_string(),
                        },
                    )
                }
            }
        }
        _ => Vec::new(),
    };

    match advance(pin.as_ref(), &current, &consistency) {
        Ok(outcome) => {
            let accepted = accept(&current, pin.as_ref(), receipt);
            let standing = match outcome {
                Advance::Pinned => MarkerStanding::Pinned {
                    size: accepted.size,
                },
                Advance::Unchanged => MarkerStanding::Unchanged {
                    size: accepted.size,
                },
                Advance::Extended => MarkerStanding::Extended {
                    from: pin.as_ref().map_or(0, |pin| pin.size),
                    to: accepted.size,
                },
            };
            let by = accepted.by.clone();
            let mut record = record_of(entry, held, standing.clone());
            if let Some(marks) = marks_of(&entry.base, &accepted, receipt) {
                record.marks = marks;
            }
            record.pin = Some(accepted);
            save(identity, &by, &record);
            standing
        }
        Err(refusal) => {
            let standing = refusal_standing(&refusal, pin.as_ref(), &current);
            if matches!(standing, MarkerStanding::Diverged { .. }) {
                if let (Some(by), Some((_, record))) = (known.as_ref(), held.as_ref()) {
                    if let Some(head) = record.pin.as_ref() {
                        keep_divergence(identity, by, head, &current);
                    }
                }
            }
            keep(identity, known.as_ref(), entry, held, standing)
        }
    }
}

/// Which head is pinned on acceptance.
///
/// The served head is the authority — a receipt head can be minted over a
/// private side branch. But on the *first* pin the receipt's head is preferred
/// when the same device signed it: a marker that answered a chronicled receipt
/// and then serves a head that does not cover it is one suppressing the
/// ratchet, and pinning the head this identity has an inclusion proof under is
/// the conservative first step.
fn accept(current: &Head, pin: Option<&PinnedHead>, receipt: Option<&Receipt>) -> Head {
    if pin.is_some() {
        return current.clone();
    }
    receipt
        .and_then(|receipt| receipt.head.as_ref())
        .filter(|head| head.by == current.by && head.verify().is_ok())
        .cloned()
        .unwrap_or_else(|| current.clone())
}

fn refusal_standing(refusal: &Refusal, pin: Option<&PinnedHead>, offered: &Head) -> MarkerStanding {
    let pinned_size = pin.map_or(0, |pin| pin.size);
    match refusal {
        Refusal::Diverged => MarkerStanding::Diverged { size: pinned_size },
        Refusal::Rollback => MarkerStanding::Rollback {
            pinned: pinned_size,
            offered: offered.size,
        },
        Refusal::Unproven => MarkerStanding::Unproven {
            pinned: pinned_size,
            offered: offered.size,
        },
        Refusal::WrongSigner => MarkerStanding::WrongSigner {
            pinned: pin.map_or_else(|| offered.by.clone(), |pin| pin.by.clone()),
            offered: offered.by.clone(),
        },
        // Everything left is the head failing on its own terms. Not folded
        // into "could not be asked": the marker answered, and what it said was
        // not a head.
        other => MarkerStanding::Refused {
            why: other.to_string(),
        },
    }
}

/// The marks a chronicled receipt carried, each checked twice: on its own
/// terms ([`mechanics::chronicle::verify_mark`]) and against the head this
/// reader just accepted ([`mechanics::chronicle::consistent_with`]).
///
/// `None` when there is nothing to say — no receipt, or a marker that keeps no
/// chronicle — which leaves whatever was proven before standing. `Some` always
/// replaces, empty map included: that is the withdrawal.
fn marks_of(
    base: &str,
    accepted: &Head,
    receipt: Option<&Receipt>,
) -> Option<BTreeMap<DeviceId, Mark>> {
    let receipt = receipt?;
    let signed_at = receipt.head.as_ref()?;
    let pin = PinnedHead::from(accepted);
    // The marks commit to the receipt's head. Where the accepted head has
    // moved past it, the bridge between the two is what makes them one log.
    let bridge = if signed_at.size < accepted.size {
        chronicle_over_http(base, Some(signed_at.size))
            .map(|answer| answer.consistency)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut marks = BTreeMap::new();
    for avowal in &receipt.marks {
        let Party::Device(subject) = &avowal.subject else {
            continue;
        };
        let proven = mechanics::chronicle::verify_mark(avowal, &receipt.inclusion).is_ok()
            && mechanics::chronicle::consistent_with(&pin, avowal, &bridge).is_ok();
        marks.insert(
            subject.clone(),
            Mark {
                avowal: avowal.clone(),
                inclusion: receipt.inclusion.clone(),
                proven,
            },
        );
    }
    Some(marks)
}

fn record_of(
    entry: &MarkerEntry,
    held: Option<(DeviceId, MarkerRecord)>,
    standing: MarkerStanding,
) -> MarkerRecord {
    let mut record = held.map_or_else(
        || MarkerRecord {
            pin: None,
            base: entry.base.clone(),
            checked_at: 0,
            standing: standing.clone(),
            marks: BTreeMap::new(),
        },
        |(_, record)| record,
    );
    record.base.clone_from(&entry.base);
    record.checked_at = crate::daemon::correspondence::now_secs();
    record.standing = standing;
    record
}

/// Write a standing that moved nothing. With no signer to file it under this
/// marker has never been asked, and it stays that way: inventing a file would
/// turn "never asked" into "asked, and here is a fact", which is the one
/// distinction absence carries.
fn keep(
    identity: &Path,
    by: Option<&DeviceId>,
    entry: &MarkerEntry,
    held: Option<(DeviceId, MarkerRecord)>,
    standing: MarkerStanding,
) -> MarkerStanding {
    let Some(by) = by else {
        return standing;
    };
    let record = record_of(entry, held, standing.clone());
    save(identity, by, &record);
    standing
}

/// Adopt the single-registrar pin this store replaces, once, when the device
/// answering now is the one that signed it.
///
/// Every installed machine holds one, and dropping it would silently re-pin
/// the fleet — one step with no divergence protection, on the exact service
/// this identity has been following the longest. The *signer* is the test
/// rather than the host: the old file recorded no base, and this store is
/// keyed by signer precisely because a service moves.
fn adopt_legacy(identity: &Path, base: &str, by: &DeviceId) -> Option<MarkerRecord> {
    let legacy = identity.join(LEGACY_PIN);
    let head: Head = serde_json::from_slice(&std::fs::read(&legacy).ok()?).ok()?;
    if head.by != *by {
        return None;
    }
    let record = MarkerRecord {
        standing: MarkerStanding::Pinned { size: head.size },
        pin: Some(head),
        base: base.trim_end_matches('/').to_string(),
        checked_at: 0,
        marks: BTreeMap::new(),
    };
    save(identity, by, &record);
    // The evidence half moves with the pin it accuses: a divergence already
    // caught must not be forgotten by a rename.
    let evidence = identity.join(LEGACY_EVIDENCE);
    if let Ok(kept) = std::fs::read(&evidence) {
        if !evidence_path(identity, by).exists() {
            if let Err(error) =
                addressbook::durable::atomic_replace(&evidence_path(identity, by), &kept)
            {
                tracing::error!(%error, "could not carry the divergence evidence forward");
                return Some(record);
            }
        }
        std::fs::remove_file(&evidence).ok();
    }
    std::fs::remove_file(&legacy).ok();
    Some(record)
}

/// The book as a surface reads it: one row per marker this identity weighs,
/// in the order it weighs them, and which markers have recorded each device.
///
/// Read from the files alone — nothing here asks a marker anything. A view
/// that fetched would make drawing a window a network act, and would answer
/// "could not be asked" for a marker that was answering fine ten seconds ago.
#[derive(Debug, Default)]
pub struct Weighed {
    /// One row per book entry, in the book's order. Position is the weight,
    /// and the ordering is this reader's.
    pub markers: Vec<crate::control::MarkerView>,
    /// Which markers have proven a record naming each device, by marker
    /// device id.
    certified: BTreeMap<DeviceId, Vec<String>>,
}

impl Weighed {
    /// The markers that certify one device. Empty is the ordinary answer and
    /// is not a finding: it means no marker this identity weighs has recorded
    /// a publication naming this device, which is a fact about markers.
    #[must_use]
    pub fn certifying(&self, device: &DeviceId) -> Vec<String> {
        self.certified.get(device).cloned().unwrap_or_default()
    }
}

/// Read every marker in the book, with what each has proven.
///
/// A marker that has been caught equivocating, or that answered under a
/// device this identity does not follow, certifies nobody — it is dropped
/// from the tier rather than weighed down, because there is no rank below
/// zero and a reader that kept counting a caught liar would be pretending
/// there is. Its row stays, carrying the reason.
#[must_use]
pub fn weighed(identity: &Path) -> Weighed {
    let book = crate::config::Settings::load(Some(identity))
        .marks_book()
        .unwrap_or_default();
    weighing(identity, book)
}

/// The reading itself, with the book supplied — so what a surface draws can
/// be asserted without a config root. The same split, for the same reason, as
/// `serve_checking` in [`crate::update::watch`].
#[must_use]
fn weighing(identity: &Path, book: Vec<MarkerEntry>) -> Weighed {
    let mut weighed = Weighed::default();
    for entry in book {
        let held = following(identity, &entry);
        let (by, record) = match held {
            Some((by, record)) => (Some(by), Some(record)),
            None => (entry.by.clone(), None),
        };
        if let (Some(by), Some(record)) = (by.as_ref(), record.as_ref()) {
            if weighs(&record.standing) {
                for (device, mark) in &record.marks {
                    if mark.proven {
                        weighed
                            .certified
                            .entry(device.clone())
                            .or_default()
                            .push(by.as_str().to_owned());
                    }
                }
            }
        }
        weighed.markers.push(crate::control::MarkerView {
            base: entry.base,
            by: by.map(|by| by.as_str().to_owned()),
            standing: record.as_ref().map(|record| seen(&record.standing)),
            checked_at: record.map(|record| record.checked_at),
        });
    }
    weighed
}

/// Whether a marker in this standing may still certify anybody.
///
/// The two that may not are the two where the marker itself is the problem.
/// The rest — unreachable, rolled back, unproven, unreadable — keep the pin
/// and leave what was proven earlier standing, which is the whole difference
/// between a witness caught lying and one who was not at home.
fn weighs(standing: &MarkerStanding) -> bool {
    !matches!(
        standing,
        MarkerStanding::Diverged { .. } | MarkerStanding::WrongSigner { .. }
    )
}

/// The durable standing as the control plane spells it. Exhaustive on
/// purpose: a standing added to the record must be decided for the surface
/// here rather than quietly rendering as something else.
fn seen(standing: &MarkerStanding) -> crate::control::MarkerStandingView {
    use crate::control::MarkerStandingView as Seen;
    match standing {
        MarkerStanding::Pinned { .. } => Seen::Pinned,
        MarkerStanding::Unchanged { .. } => Seen::Unchanged,
        MarkerStanding::Extended { .. } => Seen::Extended,
        MarkerStanding::CouldNotAsk { why } => Seen::CouldNotAsk { why: why.clone() },
        MarkerStanding::WrongSigner { .. } => Seen::WrongSigner,
        MarkerStanding::Rollback { .. } => Seen::Rollback,
        MarkerStanding::Unproven { .. } => Seen::Unproven,
        MarkerStanding::Diverged { .. } => Seen::Diverged,
        MarkerStanding::Refused { why } => Seen::Refused { why: why.clone() },
    }
}

/// Follow the book until the daemon stops: one marker per turn, on the
/// update watcher's period and its jitter.
///
/// One per turn rather than the whole book at once for the reason the World
/// upgrade worker advances one job at a time — a book of markers is a list of
/// blocking HTTP calls, and a daemon that made all of them back to back would
/// spend a stall on every one of them at once. The period is the staging
/// watcher's because the question is the same shape: has the thing I follow
/// changed since I last looked, and there is nothing to be gained by asking
/// faster than a person could act on the answer.
///
/// A receipt still ratchets immediately ([`ratchet`] is called on every
/// publication), so this loop is the floor for a daemon that publishes
/// nothing, never the latency.
pub async fn serve_markers(identity: PathBuf, mut stop: tokio::sync::watch::Receiver<bool>) {
    let book = crate::config::Settings::load(Some(&identity))
        .marks_book()
        .unwrap_or_default();
    if book.is_empty() {
        tracing::info!("this identity weighs no markers; nothing to follow");
        return;
    }
    // The whole book inside one period, so a second marker does not halve the
    // rate at which the first is looked at.
    let period = crate::update::watch::CHECK_PERIOD
        .checked_div(u32::try_from(book.len()).unwrap_or(u32::MAX))
        .unwrap_or(crate::update::watch::CHECK_PERIOD);
    // The first turn is spread too, so a fleet restarted together by a reboot
    // does not arrive at one marker together either.
    let mut delay = crate::update::watch::MAX_SPREAD.mul_f64(crate::update::watch::draw());
    let mut turns = book.iter().cycle();
    loop {
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            _ = stop.changed() => return,
        }
        if *stop.borrow() {
            return;
        }
        let Some(entry) = turns.next().cloned() else {
            return;
        };
        let identity = identity.clone();
        match tokio::task::spawn_blocking(move || ratchet(&identity, &entry, None)).await {
            Ok(standing) => {
                if let Some(why) = standing.refused() {
                    tracing::info!(%why, "a marker this identity follows did not check out");
                }
            }
            Err(error) => tracing::warn!(%error, "following a marker panicked"),
        }
        delay = crate::update::watch::next_delay(period, crate::update::watch::MAX_SPREAD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::chronicle::Chronicle;

    fn device(seed: u8) -> DeviceId {
        mechanics::actor::device_from_seed(&[seed; 32])
    }

    fn head(seed: u8, entries: &[&[u8]]) -> Head {
        let mut log = Chronicle::new();
        for entry in entries {
            log.append(entry).expect("append");
        }
        log.head(&[seed; 32]).expect("head")
    }

    fn record(standing: MarkerStanding, pin: Option<Head>) -> MarkerRecord {
        MarkerRecord {
            pin,
            base: "https://marker.example".to_string(),
            checked_at: 7,
            standing,
            marks: BTreeMap::new(),
        }
    }

    /// The store's own contract: a record round-trips whole (the signed head
    /// included, because the head *is* the evidence), and the absence of the
    /// file is never confused with a standing.
    #[test]
    fn an_unasked_marker_has_no_file_and_a_record_round_trips_whole() {
        let dir = tempfile::tempdir().expect("tempdir");
        let by = device(4);
        assert!(
            load(dir.path(), &by).is_none(),
            "never asked is the absence of the file, not a standing"
        );

        let held = head(4, &[b"one"]);
        save(
            dir.path(),
            &by,
            &record(MarkerStanding::Pinned { size: 1 }, Some(held.clone())),
        );
        let read = load(dir.path(), &by).expect("the record was kept");
        assert_eq!(read.standing, MarkerStanding::Pinned { size: 1 });
        assert_eq!(read.pin.as_ref(), Some(&held));
        read.pin
            .expect("pinned")
            .verify()
            .expect("the stored head still verifies");

        std::fs::write(record_path(dir.path(), &by), b"not a record").expect("corrupt");
        assert!(
            load(dir.path(), &by).is_none(),
            "a record that does not decode reads as absent, never as a standing"
        );
    }

    /// Every outcome of the ratchet is its own standing. Folding any two of
    /// them is the false-disconnection defect: "could not be asked" would
    /// otherwise absorb "lied", and "unproven" would absorb "equivocated".
    #[test]
    fn each_ratchet_outcome_is_its_own_standing() {
        let pinned = head(4, &[b"one"]);
        let pin = PinnedHead::from(&pinned);
        let stranger = head(9, &[b"one", b"two"]);
        let forked = head(4, &[b"other"]);

        assert_eq!(
            refusal_standing(&Refusal::WrongSigner, Some(&pin), &stranger),
            MarkerStanding::WrongSigner {
                pinned: device(4),
                offered: device(9)
            }
        );
        assert_eq!(
            refusal_standing(&Refusal::Diverged, Some(&pin), &forked),
            MarkerStanding::Diverged { size: 1 }
        );
        assert_eq!(
            refusal_standing(&Refusal::Rollback, Some(&pin), &head(4, &[])),
            MarkerStanding::Rollback {
                pinned: 1,
                offered: 0
            }
        );
        assert_eq!(
            refusal_standing(
                &Refusal::Unproven,
                Some(&pin),
                &head(4, &[b"a", b"b", b"c"])
            ),
            MarkerStanding::Unproven {
                pinned: 1,
                offered: 3
            }
        );
        assert!(
            matches!(
                refusal_standing(&Refusal::BadSignature, Some(&pin), &stranger),
                MarkerStanding::Refused { .. }
            ),
            "an unverifiable head is refused, never folded into could-not-ask"
        );

        // Each of the five refusals says something a person acts on
        // differently, and none of them is silence.
        let sentences: Vec<String> = [
            MarkerStanding::CouldNotAsk {
                why: "closed".into(),
            },
            MarkerStanding::WrongSigner {
                pinned: device(4),
                offered: device(9),
            },
            MarkerStanding::Rollback {
                pinned: 2,
                offered: 1,
            },
            MarkerStanding::Unproven {
                pinned: 1,
                offered: 3,
            },
            MarkerStanding::Diverged { size: 1 },
        ]
        .iter()
        .map(|standing| standing.refused().expect("a refusal says why"))
        .collect();
        let mut distinct = sentences.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            sentences.len(),
            "five facts, five sentences"
        );
        assert!(MarkerStanding::Pinned { size: 1 }.refused().is_none());
        assert!(MarkerStanding::Unchanged { size: 1 }.refused().is_none());
        assert!(MarkerStanding::Extended { from: 1, to: 2 }
            .refused()
            .is_none());
    }

    /// Non-repudiability is a file: the two signed heads are what a third
    /// party checks, and a later divergence must not overwrite the first pair.
    #[test]
    fn irreconcilable_heads_from_one_signer_are_kept_as_evidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let by = device(4);
        let held = head(4, &[b"one"]);
        let offered = head(4, &[b"other"]);
        keep_divergence(dir.path(), &by, &held, &offered);

        let kept = std::fs::read(evidence_path(dir.path(), &by)).expect("evidence was retained");
        let pair: serde_json::Value = serde_json::from_slice(&kept).expect("decode");
        let recovered: Head = serde_json::from_value(pair["held"].clone()).expect("held head");
        assert_eq!(recovered, held);
        recovered
            .verify()
            .expect("the evidence carries the signature, not a claim about it");
        assert_eq!(
            serde_json::from_value::<Head>(pair["offered"].clone()).expect("offered head"),
            offered
        );

        keep_divergence(dir.path(), &by, &held, &head(4, &[b"third"]));
        let again = std::fs::read(evidence_path(dir.path(), &by)).expect("still there");
        assert_eq!(
            again, kept,
            "the earliest incriminating pair is the one kept"
        );
        assert!(
            !evidence_path(dir.path(), &device(9)).exists(),
            "evidence is filed against the signer that equivocated, nobody else"
        );
    }

    /// One proven mark, filed under `subject`, as a record would hold it.
    fn marked(subject: &DeviceId, proven: bool) -> BTreeMap<DeviceId, Mark> {
        use mechanics::kinship::{Audience, Claim};
        let mut log = Chronicle::new();
        log.append(b"a publication").expect("append");
        let avowal = Avowal::seal(
            &[4u8; 32],
            Party::Device(subject.clone()),
            Claim::Chronicled {
                size: log.size(),
                root: log.root(),
                entry: 0,
                leaf: Chronicle::leaf_of(b"a publication"),
            },
            Audience::Public,
            log.size(),
            [0u8; 16],
        )
        .expect("seal");
        BTreeMap::from([(
            subject.clone(),
            Mark {
                avowal,
                inclusion: log.inclusion(0).expect("inclusion"),
                proven,
            },
        )])
    }

    /// What the Devices surface reads. Three things have to survive the trip,
    /// and each of them is a defect if it does not: a marker nothing has asked
    /// is not one that answered "no"; a device no marker names is not a device
    /// with a problem; and a marker caught contradicting itself certifies
    /// nobody while still appearing, because the standing is the fact worth
    /// seeing.
    #[test]
    fn the_book_reads_as_a_tier_and_never_as_a_verdict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let subject = device(11);
        let asked = device(4);
        let liar = device(5);

        let mut answered = record(MarkerStanding::Extended { from: 1, to: 2 }, None);
        answered.base = "https://asked.example".into();
        answered.marks = marked(&subject, true);
        save(dir.path(), &asked, &answered);

        let mut diverged = record(MarkerStanding::Diverged { size: 1 }, None);
        diverged.base = "https://liar.example".into();
        diverged.marks = marked(&subject, true);
        save(dir.path(), &liar, &diverged);

        let book = [
            "https://asked.example",
            "https://liar.example",
            "https://silent.example",
        ]
        .iter()
        .map(|base| MarkerEntry {
            base: (*base).into(),
            by: None,
        })
        .collect();
        let weighed = weighing(dir.path(), book);

        assert_eq!(
            weighed
                .markers
                .iter()
                .map(|marker| (marker.base.as_str(), marker.standing.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "https://asked.example",
                    Some(crate::control::MarkerStandingView::Extended)
                ),
                (
                    "https://liar.example",
                    Some(crate::control::MarkerStandingView::Diverged)
                ),
                // Never asked is the absence of a standing, not a standing
                // that means no — the row exists so a surface can say which.
                ("https://silent.example", None),
            ],
        );
        assert_eq!(
            weighed.certifying(&subject),
            [asked.as_str()],
            "a marker caught equivocating certifies nobody, and one nobody asked certifies nobody",
        );
        assert!(
            weighed.certifying(&device(12)).is_empty(),
            "a device no marker has recorded is an ordinary device, not a finding",
        );

        // A mark this reader did not itself prove is quoting, not evidence.
        let mut unproven = record(MarkerStanding::Unchanged { size: 2 }, None);
        unproven.base = "https://asked.example".into();
        unproven.marks = marked(&subject, false);
        save(dir.path(), &asked, &unproven);
        assert!(weighing(
            dir.path(),
            vec![MarkerEntry {
                base: "https://asked.example".into(),
                by: None,
            }],
        )
        .certifying(&subject)
        .is_empty());
    }

    /// A mark is checked against the log this reader follows, and a mark from
    /// a signer this reader does not follow is a stranger's assertion — the
    /// same anchor `advance` checks first, for the same reason.
    #[test]
    fn a_mark_is_proven_under_the_pinned_signer_and_refused_under_another() {
        use mechanics::kinship::{Audience, Claim};

        let mut log = Chronicle::new();
        log.append(b"a publication").expect("append");
        let pinned = log.head(&[4u8; 32]).expect("head");
        let inclusion = log.inclusion(0).expect("inclusion");
        let subject = Party::Device(device(11));
        let claim = Claim::Chronicled {
            size: log.size(),
            root: log.root(),
            entry: 0,
            leaf: Chronicle::leaf_of(b"a publication"),
        };
        let mark = Avowal::seal(
            &[4u8; 32],
            subject.clone(),
            claim.clone(),
            Audience::Public,
            log.size(),
            [0u8; 16],
        )
        .expect("seal");

        mechanics::chronicle::verify_mark(&mark, &inclusion).expect("the mark verifies on its own");
        mechanics::chronicle::consistent_with(&PinnedHead::from(&pinned), &mark, &[])
            .expect("and sits on the chronicle this reader pinned");

        // The same statement, signed by a device this reader does not follow.
        let stranger = Avowal::seal(
            &[9u8; 32],
            subject,
            claim,
            Audience::Public,
            log.size(),
            [0u8; 16],
        )
        .expect("seal");
        mechanics::chronicle::verify_mark(&stranger, &inclusion)
            .expect("a stranger's signature is still a signature");
        assert_eq!(
            mechanics::chronicle::consistent_with(&PinnedHead::from(&pinned), &stranger, &[]),
            Err(Refusal::WrongSigner),
            "a mark from a device this reader does not follow proves nothing about this log"
        );
    }

    /// The fleet's pins survive the move to this store. Losing them would
    /// re-pin every installed machine in one step, with no divergence
    /// protection, against the service it has followed longest.
    #[test]
    fn a_legacy_registry_pin_is_adopted_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = head(4, &[b"one"]);
        std::fs::write(
            dir.path().join(LEGACY_PIN),
            serde_json::to_vec(&held).expect("encode"),
        )
        .expect("write");
        std::fs::write(dir.path().join(LEGACY_EVIDENCE), b"{}").expect("write");

        assert!(
            adopt_legacy(dir.path(), "https://marker.example/", &device(9)).is_none(),
            "the old pin belongs to the device that signed it, not to whoever answers next"
        );
        assert!(
            dir.path().join(LEGACY_PIN).exists(),
            "and it is left where it is until that device does answer"
        );

        adopt_legacy(dir.path(), "https://marker.example/", &device(4)).expect("adopted");
        let adopted = load(dir.path(), &device(4)).expect("the pin was adopted");
        assert_eq!(adopted.pin.as_ref(), Some(&held));
        assert_eq!(adopted.base, "https://marker.example");
        assert!(
            evidence_path(dir.path(), &device(4)).exists(),
            "the evidence moves with the pin it accuses"
        );
        assert!(!dir.path().join(LEGACY_PIN).exists(), "adopted once");
        assert!(!dir.path().join(LEGACY_EVIDENCE).exists());

        // A second pass must not walk back over a record that has since moved.
        save(
            dir.path(),
            &device(4),
            &record(
                MarkerStanding::Extended { from: 1, to: 2 },
                Some(head(4, &[b"one", b"two"])),
            ),
        );
        assert!(adopt_legacy(dir.path(), "https://marker.example", &device(4)).is_none());
        assert_eq!(
            load(dir.path(), &device(4)).expect("still held").standing,
            MarkerStanding::Extended { from: 1, to: 2 },
            "there is nothing left to adopt, and nothing is undone"
        );
    }
}
