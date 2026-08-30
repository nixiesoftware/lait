//! Two-party display enrollment.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use display_protocol::auth::{verify_request, RequestContext};
use display_protocol::bounds::{MAX_CHALLENGE_LIFETIME_MS, MAX_PAIRING_LIFETIME_MS};
use display_protocol::ids::{
    AuthenticationTag, Challenge, CoordinatorFingerprint, DisplayDeviceId, DisplayPairingId,
    PollKey, ProofKey, RendezvousId,
};
use display_protocol::pairing::{
    authenticate_pairing_complete, authenticate_pairing_status, confirmation_phrase,
    group_rendezvous_code, rendezvous_from_code, validate_instance, CoordinatorInstance,
    PairingCompleteRequest, PairingCompleteResponse, PairingRejectionReason, PairingStartRequest,
    PairingStartResponse, PairingStatus, PairingStatusRequest, RENDEZVOUS_CODE_ALPHABET,
    RENDEZVOUS_CODE_CHARS,
};
use display_protocol::receiver::{
    validate_capabilities, ChallengeResponse, ReceiverCapabilities, ReceiverHealth,
};

use super::{CoordinatorStore, DeviceRecord};
use crate::control::{
    DisplayAssignmentSyncSetting, DisplayStaleActionSetting, DisplayThemeSetting,
};

const PAIRING_RETRY_AFTER_MS: u32 = 1_500;

/// How long a minted code stays good for. Longer than a pairing, because the
/// code is carried between rooms; short enough that a code left on a screen
/// is not a standing door.
pub(crate) const RENDEZVOUS_LIFETIME_MS: u64 = 15 * 60 * 1_000;

/// How many unspent codes one coordinator holds at once. A bound on a table
/// anyone with a controller can grow, not a limit anyone will meet.
const MAX_OUTSTANDING_RENDEZVOUS: usize = 32;

#[derive(Debug, Clone)]
pub struct PendingPairingView {
    pub pairing: DisplayPairingId,
    pub confirmation_phrase: Vec<String>,
    pub coordinator_fingerprint: CoordinatorFingerprint,
    pub capabilities: ReceiverCapabilities,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

/// What a rendezvous pins its receiver to once it has enrolled: an assignment
/// with everything but the device, which does not exist until then.
#[derive(Debug, Clone)]
pub struct AssignmentIntent {
    pub orbit: String,
    pub world: String,
    pub surface: String,
    pub input: serde_json::Value,
    pub theme: DisplayThemeSetting,
    pub stale_after_ms: u32,
    pub on_stale: DisplayStaleActionSetting,
    pub sync: Option<DisplayAssignmentSyncSetting>,
    pub expires_at_unix_ms: Option<u64>,
}

/// Where a code is in its life. A spent code is not gone: it is the
/// television it became, and a controller watching for that television
/// needs to see it arrive under the same name it minted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendezvousState {
    /// Minted, not yet entered anywhere.
    Waiting,
    /// A television entered it and is proving its new key.
    Connecting { device: DisplayDeviceId },
    /// The television enrolled.
    Connected { device: DisplayDeviceId },
}

/// A code minted for a television to enter, as the controller sees it.
#[derive(Debug, Clone)]
pub struct RendezvousView {
    pub rendezvous: RendezvousId,
    /// Grouped for reading: `XXXX-XXXX`.
    pub code: String,
    pub label: String,
    pub assignment: Option<AssignmentIntent>,
    pub state: RendezvousState,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

/// What the coordinator does with a receiver the moment a rendezvous enrols
/// it. Supplied by the runtime, which is what can resolve an Orbit and a
/// surface; the pairing service only knows that something was promised.
pub type EnrollmentHook =
    Arc<dyn Fn(&DisplayDeviceId, &AssignmentIntent) -> Result<()> + Send + Sync>;

/// A pairing start named a rendezvous this coordinator does not hold: never
/// minted, already spent, expired, or revoked. One refusal for all four, so
/// the public route is not an oracle for which.
#[derive(Debug)]
pub struct RendezvousRefused;

impl std::fmt::Display for RendezvousRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("display rendezvous is not one this coordinator holds")
    }
}

impl std::error::Error for RendezvousRefused {}

#[derive(Debug, Clone)]
pub struct AuthorizedDevice {
    pub record: DeviceRecord,
    pub next_challenge: Challenge,
}

#[derive(Debug)]
pub enum AuthorizationRefusal {
    NotEnrolled,
    Revoked,
    ChallengeUnavailable,
    ChallengeExpired,
    ChallengeConsumed,
    Authentication,
    Internal(anyhow::Error),
}

impl std::fmt::Display for AuthorizationRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEnrolled => formatter.write_str("display device is not enrolled"),
            Self::Revoked => formatter.write_str("display device is revoked"),
            Self::ChallengeUnavailable => {
                formatter.write_str("display request challenge is unavailable")
            }
            Self::ChallengeExpired => formatter.write_str("display request challenge has expired"),
            Self::ChallengeConsumed => {
                formatter.write_str("display request challenge was consumed")
            }
            Self::Authentication => formatter.write_str("display receiver authentication failed"),
            Self::Internal(error) => write!(formatter, "display authorization failed: {error:#}"),
        }
    }
}

impl std::error::Error for AuthorizationRefusal {}

pub struct DisplayPairingService {
    store: Arc<CoordinatorStore>,
    instance: CoordinatorInstance,
    fingerprint: CoordinatorFingerprint,
    state: Mutex<PairingState>,
    enrollment_hook: Option<EnrollmentHook>,
}

#[derive(Default)]
struct PairingState {
    pairings: BTreeMap<String, PendingPairing>,
    challenges: BTreeMap<String, ChallengeLease>,
    health: BTreeMap<String, ReportedHealth>,
    /// Unspent codes, keyed by the rendezvous id each names on the wire.
    rendezvous: BTreeMap<String, PendingRendezvous>,
}

/// A receiver's last report and when it arrived. Health is held in memory
/// only, so the time is what tells "reported a moment ago" from "reported
/// once, hours back, and has not spoken since" — the report itself says
/// `online` in both cases.
#[derive(Debug, Clone)]
pub struct ReportedHealth {
    pub health: ReceiverHealth,
    pub reported_at_unix_ms: u64,
}

struct PendingPairing {
    poll_key: PollKey,
    capabilities: ReceiverCapabilities,
    confirmation_phrase: Vec<String>,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    decision: PairingDecision,
    /// The assignment a rendezvous promised this receiver, carried here from
    /// the moment the code was spent until enrollment commits it.
    promised: Option<AssignmentIntent>,
    /// The code that opened this pairing, so its enrollment can be recorded
    /// against the code the controller is watching.
    rendezvous: Option<String>,
}

struct PendingRendezvous {
    code: String,
    label: String,
    assignment: Option<AssignmentIntent>,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    /// Set by the start that spent it; `enrolled` by the completion that
    /// committed it. A spent code lingers one more lifetime so the
    /// controller sees the television arrive, then is swept.
    spent: Option<SpentRendezvous>,
}

struct SpentRendezvous {
    device: DisplayDeviceId,
    enrolled: bool,
}

impl PendingRendezvous {
    fn state(&self) -> RendezvousState {
        match &self.spent {
            None => RendezvousState::Waiting,
            Some(SpentRendezvous {
                device,
                enrolled: false,
            }) => RendezvousState::Connecting {
                device: device.clone(),
            },
            Some(SpentRendezvous {
                device,
                enrolled: true,
            }) => RendezvousState::Connected {
                device: device.clone(),
            },
        }
    }

    /// Whether the controller should still see this code: unspent until it
    /// expires, spent for one lifetime after that.
    fn visible(&self, now_unix_ms: u64) -> bool {
        match self.spent {
            None => self.expires_at_unix_ms > now_unix_ms,
            Some(_) => {
                self.expires_at_unix_ms
                    .saturating_add(RENDEZVOUS_LIFETIME_MS)
                    > now_unix_ms
            }
        }
    }

    fn view(&self, rendezvous: RendezvousId) -> RendezvousView {
        RendezvousView {
            rendezvous,
            code: self.code.clone(),
            label: self.label.clone(),
            assignment: self.assignment.clone(),
            state: self.state(),
            created_at_unix_ms: self.created_at_unix_ms,
            expires_at_unix_ms: self.expires_at_unix_ms,
        }
    }
}

enum PairingDecision {
    Pending,
    Approved {
        label: String,
        device: DisplayDeviceId,
        proof_key: ProofKey,
        enrollment_challenge: Challenge,
        completed: bool,
    },
    Rejected(PairingRejectionReason),
}

struct ChallengeLease {
    challenge: Challenge,
    expires_at_unix_ms: u64,
}

impl DisplayPairingService {
    pub fn new(
        store: Arc<CoordinatorStore>,
        instance: CoordinatorInstance,
        fingerprint: CoordinatorFingerprint,
    ) -> Result<Self> {
        validate_instance(&instance).context("validate display coordinator instance")?;
        Ok(Self {
            store,
            instance,
            fingerprint,
            state: Mutex::new(PairingState::default()),
            enrollment_hook: None,
        })
    }

    /// What to do with a receiver a rendezvous enrols. Without one, a code
    /// still enrols; it just cannot keep its promise of an assignment.
    #[must_use]
    pub fn with_enrollment_hook(mut self, hook: EnrollmentHook) -> Self {
        self.enrollment_hook = Some(hook);
        self
    }

    pub fn instance(&self) -> &CoordinatorInstance {
        &self.instance
    }

    pub fn start(
        &self,
        request: PairingStartRequest,
        now_unix_ms: u64,
    ) -> Result<PairingStartResponse> {
        if request.protocol_major != display_protocol::PROTOCOL_MAJOR {
            return Err(anyhow!("unsupported display protocol major"));
        }
        validate_capabilities(&request.capabilities).context("validate receiver capabilities")?;
        let pairing = random_pairing_id()?;
        let phrase = confirmation_phrase(&self.instance.profile, &pairing, &request.receiver_nonce)
            .context("derive display confirmation phrase")?;
        let expires_at_unix_ms = now_unix_ms
            .checked_add(u64::from(MAX_PAIRING_LIFETIME_MS))
            .ok_or_else(|| anyhow!("display pairing expiry overflow"))?;
        let mut state = self.lock()?;
        let (decision, promised, rendezvous) = match &request.rendezvous {
            None => (PairingDecision::Pending, None, None),
            // The doorbell carried a secret the controller handed out, which
            // is the assurance the six-word compare buys the other way round.
            // Spending it here — before any answer leaves — is what makes the
            // code single-use: a second start naming it is refused exactly as
            // one that never held it.
            Some(named) => {
                let device = random_device_id()?;
                let proof_key = random_proof_key()?;
                let enrollment_challenge = random_challenge()?;
                let held = state
                    .rendezvous
                    .get_mut(named.as_str())
                    .filter(|held| held.spent.is_none() && held.expires_at_unix_ms > now_unix_ms)
                    .ok_or_else(|| anyhow::Error::new(RendezvousRefused))?;
                held.spent = Some(SpentRendezvous {
                    device: device.clone(),
                    enrolled: false,
                });
                (
                    PairingDecision::Approved {
                        label: held.label.clone(),
                        device,
                        proof_key,
                        enrollment_challenge,
                        completed: false,
                    },
                    held.assignment.clone(),
                    Some(named.as_str().to_string()),
                )
            }
        };
        state.pairings.insert(
            pairing.as_str().to_string(),
            PendingPairing {
                poll_key: request.poll_key,
                capabilities: request.capabilities,
                confirmation_phrase: phrase.clone(),
                created_at_unix_ms: now_unix_ms,
                expires_at_unix_ms,
                decision,
                promised,
                rendezvous,
            },
        );
        drop(state);
        Ok(PairingStartResponse {
            coordinator_profile: self.instance.profile.clone(),
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            pairing,
            expires_in_ms: MAX_PAIRING_LIFETIME_MS,
            confirmation_phrase: phrase,
            coordinator_fingerprint: self.fingerprint.clone(),
        })
    }

    pub fn pending(&self, now_unix_ms: u64) -> Result<Vec<PendingPairingView>> {
        let state = self.lock()?;
        Ok(state
            .pairings
            .iter()
            .filter_map(|(id, pending)| {
                if pending.expires_at_unix_ms <= now_unix_ms
                    || !matches!(pending.decision, PairingDecision::Pending)
                {
                    return None;
                }
                Some(PendingPairingView {
                    pairing: DisplayPairingId::parse(id.clone()).ok()?,
                    confirmation_phrase: pending.confirmation_phrase.clone(),
                    coordinator_fingerprint: self.fingerprint.clone(),
                    capabilities: pending.capabilities.clone(),
                    created_at_unix_ms: pending.created_at_unix_ms,
                    expires_at_unix_ms: pending.expires_at_unix_ms,
                })
            })
            .collect())
    }

    pub fn approve(
        &self,
        pairing: &DisplayPairingId,
        label: String,
        now_unix_ms: u64,
    ) -> Result<DisplayDeviceId> {
        validate_label(&label)?;
        let mut state = self.lock()?;
        let pending = state
            .pairings
            .get_mut(pairing.as_str())
            .ok_or_else(|| anyhow!("display pairing is unknown"))?;
        if pending.expires_at_unix_ms <= now_unix_ms {
            return Err(anyhow!("display pairing has expired"));
        }
        match &pending.decision {
            PairingDecision::Pending => {}
            PairingDecision::Approved { device, .. } => return Ok(device.clone()),
            PairingDecision::Rejected(_) => return Err(anyhow!("display pairing was rejected")),
        }
        let device = random_device_id()?;
        pending.decision = PairingDecision::Approved {
            label,
            device: device.clone(),
            proof_key: random_proof_key()?,
            enrollment_challenge: random_challenge()?,
            completed: false,
        };
        Ok(device)
    }

    pub fn reject(&self, pairing: &DisplayPairingId, reason: PairingRejectionReason) -> Result<()> {
        let mut state = self.lock()?;
        let pending = state
            .pairings
            .get_mut(pairing.as_str())
            .ok_or_else(|| anyhow!("display pairing is unknown"))?;
        pending.decision = PairingDecision::Rejected(reason);
        Ok(())
    }

    /// Mint a code a television enters to enrol as `label` — and, if an
    /// assignment is promised, to be pinned to it the moment it does.
    ///
    /// The controller that asks is the same one that would otherwise compare
    /// six words on two screens; handing the television a secret from that
    /// controller is the same trust decision made once, in advance.
    pub fn mint_rendezvous(
        &self,
        label: String,
        assignment: Option<AssignmentIntent>,
        now_unix_ms: u64,
    ) -> Result<RendezvousView> {
        validate_label(&label)?;
        let expires_at_unix_ms = now_unix_ms
            .checked_add(RENDEZVOUS_LIFETIME_MS)
            .ok_or_else(|| anyhow!("display rendezvous expiry overflow"))?;
        let mut state = self.lock()?;
        state.rendezvous.retain(|_, held| held.visible(now_unix_ms));
        let unspent = state
            .rendezvous
            .values()
            .filter(|held| held.spent.is_none())
            .count();
        if unspent >= MAX_OUTSTANDING_RENDEZVOUS {
            return Err(anyhow!(
                "this coordinator already holds {MAX_OUTSTANDING_RENDEZVOUS} unspent codes"
            ));
        }
        let (code, rendezvous) = loop {
            let code = random_rendezvous_code()?;
            let rendezvous =
                rendezvous_from_code(&code).context("derive display rendezvous from its code")?;
            if !state.rendezvous.contains_key(rendezvous.as_str()) {
                break (code, rendezvous);
            }
        };
        let code = group_rendezvous_code(&code).context("group display rendezvous code")?;
        let held = PendingRendezvous {
            code,
            label,
            assignment,
            created_at_unix_ms: now_unix_ms,
            expires_at_unix_ms,
            spent: None,
        };
        let view = held.view(rendezvous.clone());
        state
            .rendezvous
            .insert(rendezvous.as_str().to_string(), held);
        Ok(view)
    }

    /// The codes the controller can still see: waiting to be entered, or
    /// spent and recently become a television.
    pub fn outstanding_rendezvous(&self, now_unix_ms: u64) -> Result<Vec<RendezvousView>> {
        let state = self.lock()?;
        Ok(state
            .rendezvous
            .iter()
            .filter(|(_, held)| held.visible(now_unix_ms))
            .filter_map(|(id, held)| Some(held.view(RendezvousId::parse(id.clone()).ok()?)))
            .collect())
    }

    /// Withdraw a code — before anything enters it, or from the list after
    /// it has become a television. The television itself is unaffected.
    pub fn revoke_rendezvous(&self, rendezvous: &RendezvousId) -> Result<()> {
        self.lock()?
            .rendezvous
            .remove(rendezvous.as_str())
            .map(|_| ())
            .ok_or_else(|| anyhow!("display rendezvous is unknown"))
    }

    pub fn status(&self, request: PairingStatusRequest, now_unix_ms: u64) -> Result<PairingStatus> {
        if request.protocol_major != display_protocol::PROTOCOL_MAJOR {
            return Err(anyhow!("unsupported display protocol major"));
        }
        let state = self.lock()?;
        let pending = state
            .pairings
            .get(request.pairing.as_str())
            .ok_or_else(|| anyhow!("display pairing is unknown"))?;
        let expected = authenticate_pairing_status(&pending.poll_key, &request.pairing)
            .context("authenticate display pairing status")?;
        if expected != request.proof {
            return Err(anyhow!("display pairing status authentication failed"));
        }
        if pending.expires_at_unix_ms <= now_unix_ms {
            return Ok(PairingStatus::Expired);
        }
        match &pending.decision {
            PairingDecision::Pending => Ok(PairingStatus::Pending {
                retry_after_ms: PAIRING_RETRY_AFTER_MS,
            }),
            PairingDecision::Approved {
                device,
                proof_key,
                enrollment_challenge,
                ..
            } => Ok(PairingStatus::Approved {
                device: device.clone(),
                proof_key: proof_key.clone(),
                enrollment_challenge: enrollment_challenge.clone(),
            }),
            PairingDecision::Rejected(reason) => Ok(PairingStatus::Rejected { reason: *reason }),
        }
    }

    pub fn complete(
        &self,
        request: PairingCompleteRequest,
        now_unix_ms: u64,
    ) -> Result<PairingCompleteResponse> {
        if request.protocol_major != display_protocol::PROTOCOL_MAJOR {
            return Err(anyhow!("unsupported display protocol major"));
        }
        let mut state = self.lock()?;
        let (device, was_completed, promised, rendezvous) = {
            let pending = state
                .pairings
                .get_mut(request.pairing.as_str())
                .ok_or_else(|| anyhow!("display pairing is unknown"))?;
            if pending.expires_at_unix_ms <= now_unix_ms {
                return Err(anyhow!("display pairing has expired"));
            }
            let PairingDecision::Approved {
                label,
                device,
                proof_key,
                enrollment_challenge,
                completed,
            } = &mut pending.decision
            else {
                return Err(anyhow!("display pairing is not approved"));
            };
            if request.device != *device || request.enrollment_challenge != *enrollment_challenge {
                return Err(anyhow!(
                    "display pairing completion does not match its approval"
                ));
            }
            let expected = authenticate_pairing_complete(
                proof_key,
                &request.pairing,
                device,
                enrollment_challenge,
            )
            .context("authenticate display pairing completion")?;
            if expected != request.proof {
                return Err(anyhow!("display pairing completion authentication failed"));
            }
            let was_completed = *completed;
            if !was_completed {
                self.store.enrol(
                    DeviceRecord {
                        version: 1,
                        device: device.clone(),
                        label: label.clone(),
                        capabilities: pending.capabilities.clone(),
                        issued_at_unix_ms: now_unix_ms,
                        revoked_at_unix_ms: None,
                    },
                    proof_key.clone(),
                )?;
                *completed = true;
            }
            let device = device.clone();
            // Taken, not cloned: the promise is kept once, by the completion
            // that enrolled. A repeated completion is idempotent enrollment
            // and must not be a second assignment.
            let promised = if was_completed {
                None
            } else {
                pending.promised.take()
            };
            (device, was_completed, promised, pending.rendezvous.clone())
        };
        // The code the controller is watching becomes the television it
        // minted: same entry, one state on.
        if !was_completed {
            if let Some(spent) = rendezvous
                .as_deref()
                .and_then(|id| state.rendezvous.get_mut(id))
                .and_then(|held| held.spent.as_mut())
            {
                spent.enrolled = true;
            }
        }
        let next_challenge = random_challenge()?;
        state.challenges.insert(
            device.as_str().to_string(),
            ChallengeLease {
                challenge: next_challenge.clone(),
                expires_at_unix_ms: challenge_expiry(now_unix_ms)?,
            },
        );
        drop(state);
        // Enrollment has committed; the promise is kept outside the lock,
        // because keeping it resolves an Orbit and a surface. A promise that
        // cannot be kept leaves an enrolled, unassigned receiver — the state
        // an operator can see and fix — never an un-enrolled one.
        if let Some(intent) = promised {
            match &self.enrollment_hook {
                Some(hook) => {
                    if let Err(error) = hook(&device, &intent) {
                        tracing::warn!(
                            %device,
                            error = format!("{error:#}"),
                            "a rendezvous enrolled its receiver, but the assignment it promised was refused; \
                             the receiver is enrolled and unassigned"
                        );
                    }
                }
                None => tracing::warn!(
                    %device,
                    "a rendezvous promised an assignment and this coordinator has nothing to commit one with"
                ),
            }
        }
        if was_completed {
            Ok(PairingCompleteResponse::AlreadyEnrolled {
                device: device.clone(),
                next_challenge,
            })
        } else {
            Ok(PairingCompleteResponse::Enrolled {
                device,
                next_challenge,
            })
        }
    }

    pub fn challenge(
        &self,
        device: &DisplayDeviceId,
        now_unix_ms: u64,
    ) -> Result<ChallengeResponse, AuthorizationRefusal> {
        let _enrolled = self
            .store
            .device(device)
            .map_err(AuthorizationRefusal::Internal)?
            .ok_or(AuthorizationRefusal::NotEnrolled)?;
        // A revoked installation may still obtain a challenge. The following
        // authenticated request proves possession and receives the typed
        // Revoked refusal; exposing revocation from this public device-id-only
        // route would both leak policy and strand receivers after restart.
        let challenge = random_challenge().map_err(AuthorizationRefusal::Internal)?;
        self.lock()
            .map_err(AuthorizationRefusal::Internal)?
            .challenges
            .insert(
                device.as_str().to_string(),
                ChallengeLease {
                    challenge: challenge.clone(),
                    expires_at_unix_ms: challenge_expiry(now_unix_ms)
                        .map_err(AuthorizationRefusal::Internal)?,
                },
            );
        Ok(ChallengeResponse {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            challenge,
            expires_in_ms: MAX_CHALLENGE_LIFETIME_MS,
        })
    }

    /// Verify and consume a receiver request challenge, returning the enrolled
    /// record and the replacement every authenticated response must carry.
    pub fn authorize(
        &self,
        context: &RequestContext<'_>,
        tag: &AuthenticationTag,
        now_unix_ms: u64,
    ) -> Result<AuthorizedDevice, AuthorizationRefusal> {
        let record = self
            .store
            .device(context.device)
            .map_err(AuthorizationRefusal::Internal)?
            .ok_or(AuthorizationRefusal::NotEnrolled)?;
        let revoked = record.revoked_at_unix_ms.is_some();
        // Custody is fetched by its own call, never carried along with standing.
        let proof_key = self
            .store
            .proof_key(context.device)
            .map_err(AuthorizationRefusal::Internal)?
            .ok_or(AuthorizationRefusal::NotEnrolled)?;
        let mut state = self.lock().map_err(AuthorizationRefusal::Internal)?;
        let lease = state
            .challenges
            .get(context.device.as_str())
            .ok_or(AuthorizationRefusal::ChallengeUnavailable)?;
        if lease.expires_at_unix_ms <= now_unix_ms {
            return Err(AuthorizationRefusal::ChallengeExpired);
        }
        if lease.challenge != *context.challenge {
            return Err(AuthorizationRefusal::ChallengeConsumed);
        }
        verify_request(&proof_key, context, tag)
            .map_err(|_| AuthorizationRefusal::Authentication)?;
        if revoked {
            state.challenges.remove(context.device.as_str());
            return Err(AuthorizationRefusal::Revoked);
        }
        let next_challenge = random_challenge().map_err(AuthorizationRefusal::Internal)?;
        state.challenges.insert(
            context.device.as_str().to_string(),
            ChallengeLease {
                challenge: next_challenge.clone(),
                expires_at_unix_ms: challenge_expiry(now_unix_ms)
                    .map_err(AuthorizationRefusal::Internal)?,
            },
        );
        Ok(AuthorizedDevice {
            record,
            next_challenge,
        })
    }

    pub fn accept_capabilities(
        &self,
        device: &DisplayDeviceId,
        capabilities: ReceiverCapabilities,
    ) -> Result<()> {
        validate_capabilities(&capabilities).context("validate receiver capabilities")?;
        let mut record = self
            .store
            .device(device)?
            .ok_or_else(|| anyhow!("display device is not enrolled"))?;
        if !narrower_or_equal(&capabilities, &record.capabilities) {
            return Err(anyhow!(
                "receiver capability negotiation may only narrow enrollment limits"
            ));
        }
        record.capabilities = capabilities;
        self.store.update_device(record)
    }

    pub fn record_health(&self, device: &DisplayDeviceId, health: ReceiverHealth) -> Result<()> {
        display_protocol::receiver::validate_health(&health).context("validate receiver health")?;
        self.lock()?.health.insert(
            device.as_str().to_string(),
            ReportedHealth {
                health,
                reported_at_unix_ms: mechanics::wallclock::now_millis(),
            },
        );
        Ok(())
    }

    pub fn health(&self, device: &DisplayDeviceId) -> Result<Option<ReportedHealth>> {
        Ok(self.lock()?.health.get(device.as_str()).cloned())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, PairingState>> {
        self.state
            .lock()
            .map_err(|_| anyhow!("display pairing state lock was poisoned"))
    }
}

fn narrower_or_equal(next: &ReceiverCapabilities, enrolled: &ReceiverCapabilities) -> bool {
    next.protocol_major == enrolled.protocol_major
        && next.platform == enrolled.platform
        && next.viewport == enrolled.viewport
        && next.locale == enrolled.locale
        && next
            .image_types
            .iter()
            .all(|image| enrolled.image_types.contains(image))
        && next.max_asset_bytes <= enrolled.max_asset_bytes
        && next.max_staged_bytes <= enrolled.max_staged_bytes
        && next.max_program_items <= enrolled.max_program_items
        && next.max_staging_horizon_ms <= enrolled.max_staging_horizon_ms
        && next.accessibility == enrolled.accessibility
        && next.playback == enrolled.playback
}

fn challenge_expiry(now_unix_ms: u64) -> Result<u64> {
    now_unix_ms
        .checked_add(u64::from(MAX_CHALLENGE_LIFETIME_MS))
        .ok_or_else(|| anyhow!("display challenge expiry overflow"))
}

fn random_pairing_id() -> Result<DisplayPairingId> {
    DisplayPairingId::parse(random_hex::<16>()?).context("mint display pairing id")
}

fn random_device_id() -> Result<DisplayDeviceId> {
    DisplayDeviceId::parse(random_hex::<16>()?).context("mint display device id")
}

fn random_proof_key() -> Result<ProofKey> {
    ProofKey::parse(random_hex::<32>()?).context("mint display proof key")
}

fn random_challenge() -> Result<Challenge> {
    Challenge::parse(random_hex::<32>()?).context("mint display challenge")
}

fn validate_label(label: &str) -> Result<()> {
    if label.trim().is_empty()
        || label.len() > display_protocol::bounds::MAX_LABEL_BYTES
        || label.chars().any(char::is_control)
    {
        return Err(anyhow!("display label is invalid"));
    }
    Ok(())
}

/// Eight symbols of the code alphabet: five bits from each of eight random
/// bytes, so the randomness is the operating system's and the alphabet does
/// the rest.
pub(crate) fn random_rendezvous_code() -> Result<String> {
    let mut bytes = [0u8; RENDEZVOUS_CODE_CHARS];
    getrandom::fill(&mut bytes).context("obtain display rendezvous randomness")?;
    bytes
        .iter()
        .map(|byte| {
            RENDEZVOUS_CODE_ALPHABET
                .get(usize::from(byte & 0x1f))
                .copied()
                .map(char::from)
                .ok_or_else(|| anyhow!("rendezvous alphabet is shorter than a symbol"))
        })
        .collect()
}

fn random_hex<const N: usize>() -> Result<String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).context("obtain display coordinator randomness")?;
    Ok(data_encoding::HEXLOWER.encode(&bytes))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use display_protocol::auth::{authenticate_request, sha256, RequestMethod, RequestRoute};
    use display_protocol::ids::{PollKey, ReceiverNonce};
    use display_protocol::pairing::{
        authenticate_pairing_complete, authenticate_pairing_status, CoordinatorTrust,
    };
    use display_protocol::program::DisplayAssetMediaType;
    use display_protocol::receiver::{
        AccessibilityCapabilities, HealthGranularity, LatencyClass, PlaybackCapabilities,
        PlaybackTier, ReceiverPlatform, SyncClass, Viewport,
    };

    use super::*;

    fn root() -> PathBuf {
        // A counter beside the clock: two tests in this module start in the
        // same millisecond, and a shared directory is one test's store
        // replaced under the other's feet.
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "lait-pairing-test-{}-{}-{}",
            std::process::id(),
            mechanics::wallclock::now_millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn capabilities() -> ReceiverCapabilities {
        ReceiverCapabilities {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            platform: ReceiverPlatform::Tvos,
            build: "test".into(),
            viewport: Viewport {
                width: 1920,
                height: 1080,
                scale_milli: 1000,
            },
            image_types: vec![DisplayAssetMediaType::ImagePng],
            max_asset_bytes: 16 * 1024 * 1024,
            max_staged_bytes: 48 * 1024 * 1024,
            max_program_items: 16,
            max_staging_horizon_ms: 86_400_000,
            locale: "en-US".into(),
            accessibility: AccessibilityCapabilities {
                native_screen_reader: true,
                spoken_summary: true,
                captions: true,
                audio_description: true,
            },
            playback: PlaybackCapabilities {
                tier: PlaybackTier::Frame,
                sync_class: SyncClass::Boundary,
                rate_control_probed: false,
                latency_class: LatencyClass::Snapshot,
                health_granularity: HealthGranularity::Full,
            },
        }
    }

    #[test]
    fn approval_enrolls_only_after_receiver_proves_the_new_key() {
        let root = root();
        let seed = [42u8; 32];
        let custodian = crate::display::Custodian {
            device: mechanics::actor::device_from_seed(&seed),
            unlock: mechanics::authorization::custody::UnlockKey::RecoveryKey {
                seed,
                me: mechanics::actor::device_from_seed(&seed),
            },
        };
        let store = Arc::new(CoordinatorStore::open(&root, [7; 32], &custodian).unwrap());
        let fingerprint = CoordinatorFingerprint::parse("aa".repeat(32)).unwrap();
        let service = DisplayPairingService::new(
            store.clone(),
            CoordinatorInstance {
                protocol_major: display_protocol::PROTOCOL_MAJOR,
                instance: "11".repeat(16),
                label: "Home Astrolabe".into(),
                profile: display_protocol::ids::CoordinatorProfile::parse(format!(
                    "prf_{}",
                    "6".repeat(26)
                ))
                .unwrap(),
                trust: CoordinatorTrust::PinnedCertificate {
                    origin: "https://astrolabe.local:7443".into(),
                    sha256: fingerprint.clone(),
                },
            },
            fingerprint,
        )
        .unwrap();
        let poll_key = PollKey::parse("22".repeat(32)).unwrap();
        let started = service
            .start(
                PairingStartRequest {
                    protocol_major: display_protocol::PROTOCOL_MAJOR,
                    receiver_nonce: ReceiverNonce::parse("33".repeat(32)).unwrap(),
                    poll_key: poll_key.clone(),
                    rendezvous: None,
                    capabilities: capabilities(),
                },
                1_000,
            )
            .unwrap();
        let status_proof = authenticate_pairing_status(&poll_key, &started.pairing).unwrap();
        assert!(matches!(
            service
                .status(
                    PairingStatusRequest {
                        protocol_major: display_protocol::PROTOCOL_MAJOR,
                        pairing: started.pairing.clone(),
                        proof: status_proof.clone(),
                    },
                    1_001,
                )
                .unwrap(),
            PairingStatus::Pending { .. }
        ));
        let approved = service
            .approve(&started.pairing, "Lobby".into(), 1_002)
            .unwrap();
        assert!(store.device(&approved).unwrap().is_none());
        let PairingStatus::Approved {
            device,
            proof_key,
            enrollment_challenge,
        } = service
            .status(
                PairingStatusRequest {
                    protocol_major: display_protocol::PROTOCOL_MAJOR,
                    pairing: started.pairing.clone(),
                    proof: status_proof,
                },
                1_003,
            )
            .unwrap()
        else {
            panic!("approved pairing must return its pending credential");
        };
        let completion_proof = authenticate_pairing_complete(
            &proof_key,
            &started.pairing,
            &device,
            &enrollment_challenge,
        )
        .unwrap();
        let completed = service
            .complete(
                PairingCompleteRequest {
                    protocol_major: display_protocol::PROTOCOL_MAJOR,
                    pairing: started.pairing,
                    device: device.clone(),
                    enrollment_challenge,
                    proof: completion_proof,
                },
                1_004,
            )
            .unwrap();
        let PairingCompleteResponse::Enrolled { next_challenge, .. } = completed else {
            panic!("first completion must enroll");
        };
        assert!(store.device(&device).unwrap().is_some());

        let body = serde_json::to_vec(&capabilities()).unwrap();
        let body_sha = sha256(&body).unwrap();
        let context = RequestContext {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            method: RequestMethod::Post,
            route: RequestRoute::Capabilities,
            device: &device,
            assignment: None,
            program: None,
            revision: None,
            current_item: None,
            elapsed_ms: None,
            wait_ms: None,
            asset: None,
            range: None,
            challenge: &next_challenge,
            body_sha256: &body_sha,
        };
        let tag = authenticate_request(&proof_key, &context).unwrap();
        service.authorize(&context, &tag, 1_005).unwrap();
        assert!(matches!(
            service.authorize(&context, &tag, 1_006),
            Err(AuthorizationRefusal::ChallengeConsumed)
        ));
        store.revoke_device(&device, 1_007).unwrap();
        let revoked_challenge = service.challenge(&device, 1_008).unwrap().challenge;
        let revoked_context = RequestContext {
            challenge: &revoked_challenge,
            ..context
        };
        let revoked_tag = authenticate_request(&proof_key, &revoked_context).unwrap();
        assert!(matches!(
            service.authorize(&revoked_context, &revoked_tag, 1_009),
            Err(AuthorizationRefusal::Revoked)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    fn service_at(root: &std::path::Path) -> (Arc<CoordinatorStore>, DisplayPairingService) {
        let seed = [42u8; 32];
        let custodian = crate::display::Custodian {
            device: mechanics::actor::device_from_seed(&seed),
            unlock: mechanics::authorization::custody::UnlockKey::RecoveryKey {
                seed,
                me: mechanics::actor::device_from_seed(&seed),
            },
        };
        let store = Arc::new(CoordinatorStore::open(root, [7; 32], &custodian).unwrap());
        let fingerprint = CoordinatorFingerprint::parse("aa".repeat(32)).unwrap();
        let service = DisplayPairingService::new(
            store.clone(),
            CoordinatorInstance {
                protocol_major: display_protocol::PROTOCOL_MAJOR,
                instance: "11".repeat(16),
                label: "Home Astrolabe".into(),
                profile: display_protocol::ids::CoordinatorProfile::parse(format!(
                    "prf_{}",
                    "6".repeat(26)
                ))
                .unwrap(),
                trust: CoordinatorTrust::PinnedCertificate {
                    origin: "https://astrolabe.local:7443".into(),
                    sha256: fingerprint.clone(),
                },
            },
            fingerprint,
        )
        .unwrap();
        (store, service)
    }

    fn start_request(rendezvous: Option<RendezvousId>) -> PairingStartRequest {
        PairingStartRequest {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            receiver_nonce: ReceiverNonce::parse("33".repeat(32)).unwrap(),
            poll_key: PollKey::parse("22".repeat(32)).unwrap(),
            rendezvous,
            capabilities: capabilities(),
        }
    }

    fn lobby_loop() -> AssignmentIntent {
        AssignmentIntent {
            orbit: "orb_lobby".into(),
            world: "com.lait.signage".into(),
            surface: "signage.program".into(),
            input: serde_json::json!({ "program": "bod_lobby" }),
            theme: DisplayThemeSetting::Dark,
            stale_after_ms: 120_000,
            on_stale: DisplayStaleActionSetting::Blank,
            sync: None,
            expires_at_unix_ms: None,
        }
    }

    #[test]
    fn a_code_enrolls_a_receiver_without_a_second_screen_and_keeps_its_promise() {
        let root = root();
        let (store, service) = service_at(&root);
        let kept: Arc<Mutex<Vec<(DisplayDeviceId, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = kept.clone();
        let service = service.with_enrollment_hook(Arc::new(move |device, intent| {
            recorder
                .lock()
                .unwrap()
                .push((device.clone(), intent.orbit.clone()));
            Ok(())
        }));

        let minted = service
            .mint_rendezvous("Lobby".into(), Some(lobby_loop()), 1_000)
            .unwrap();
        // The code is what a person carries; the wire id is what they never
        // see; and the two name the same rendezvous.
        assert_eq!(minted.code.len(), 9);
        assert_eq!(minted.code.chars().nth(4), Some('-'));
        assert_eq!(
            rendezvous_from_code(&minted.code).unwrap(),
            minted.rendezvous
        );
        assert_eq!(service.outstanding_rendezvous(1_001).unwrap().len(), 1);

        let started = service
            .start(start_request(Some(minted.rendezvous.clone())), 1_002)
            .unwrap();
        // Spent by its first use: nothing pending for a person to approve —
        // approval was the code — and the code is still listed, as the
        // television that is now connecting under it.
        assert!(service.pending(1_003).unwrap().is_empty());
        let listed = service.outstanding_rendezvous(1_003).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(
            matches!(listed[0].state, RendezvousState::Connecting { .. }),
            "{:?}",
            listed[0].state
        );
        let poll_key = PollKey::parse("22".repeat(32)).unwrap();
        let status_proof = authenticate_pairing_status(&poll_key, &started.pairing).unwrap();
        let PairingStatus::Approved {
            device,
            proof_key,
            enrollment_challenge,
        } = service
            .status(
                PairingStatusRequest {
                    protocol_major: display_protocol::PROTOCOL_MAJOR,
                    pairing: started.pairing.clone(),
                    proof: status_proof,
                },
                1_004,
            )
            .unwrap()
        else {
            panic!("a start that spent a code is approved by it");
        };
        // The promise is not kept until enrollment commits.
        assert!(kept.lock().unwrap().is_empty());
        assert!(store.device(&device).unwrap().is_none());

        let completion = PairingCompleteRequest {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            pairing: started.pairing.clone(),
            device: device.clone(),
            enrollment_challenge: enrollment_challenge.clone(),
            proof: authenticate_pairing_complete(
                &proof_key,
                &started.pairing,
                &device,
                &enrollment_challenge,
            )
            .unwrap(),
        };
        let completed = service.complete(completion.clone(), 1_005).unwrap();
        assert!(matches!(
            completed,
            PairingCompleteResponse::Enrolled { .. }
        ));
        assert_eq!(store.device(&device).unwrap().unwrap().label, "Lobby");
        // And now it is the connected television, under the code's name,
        // for a while — then swept, so the list is not a history.
        let listed = service.outstanding_rendezvous(1_006).unwrap();
        assert_eq!(
            listed[0].state,
            RendezvousState::Connected {
                device: device.clone()
            }
        );
        assert!(service
            .outstanding_rendezvous(1_000 + 2 * RENDEZVOUS_LIFETIME_MS)
            .unwrap()
            .is_empty());
        assert_eq!(
            kept.lock().unwrap().as_slice(),
            &[(device.clone(), "orb_lobby".to_string())]
        );

        // A repeated completion is idempotent enrollment, not a second
        // assignment.
        let again = service.complete(completion, 1_006).unwrap();
        assert!(matches!(
            again,
            PairingCompleteResponse::AlreadyEnrolled { .. }
        ));
        assert_eq!(kept.lock().unwrap().len(), 1);

        // The spent code is refused a second time exactly as one never minted.
        let replay = service
            .start(start_request(Some(minted.rendezvous.clone())), 1_007)
            .unwrap_err();
        assert!(replay.is::<RendezvousRefused>(), "{replay:#}");
        let never = service
            .start(
                start_request(Some(RendezvousId::parse("ab".repeat(16)).unwrap())),
                1_008,
            )
            .unwrap_err();
        assert!(never.is::<RendezvousRefused>(), "{never:#}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_code_dies_on_its_own_and_can_be_withdrawn_and_the_long_way_stays_open() {
        let root = root();
        let (_store, service) = service_at(&root);
        let minted = service.mint_rendezvous("Lobby".into(), None, 0).unwrap();
        let late = service
            .start(
                start_request(Some(minted.rendezvous.clone())),
                RENDEZVOUS_LIFETIME_MS,
            )
            .unwrap_err();
        assert!(late.is::<RendezvousRefused>(), "{late:#}");
        assert!(service
            .outstanding_rendezvous(RENDEZVOUS_LIFETIME_MS)
            .unwrap()
            .is_empty());

        let other = service.mint_rendezvous("Hall".into(), None, 0).unwrap();
        service.revoke_rendezvous(&other.rendezvous).unwrap();
        assert!(service.revoke_rendezvous(&other.rendezvous).is_err());
        let withdrawn = service
            .start(start_request(Some(other.rendezvous)), 1)
            .unwrap_err();
        assert!(withdrawn.is::<RendezvousRefused>(), "{withdrawn:#}");

        // A television with no code still enrols the long way: pending, for
        // a person to compare words and approve.
        service.start(start_request(None), 2).unwrap();
        assert_eq!(service.pending(3).unwrap().len(), 1);
        assert!(service.mint_rendezvous("  ".into(), None, 4).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
