//! Two-party display enrollment.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use display_protocol::auth::{verify_request, RequestContext};
use display_protocol::bounds::{MAX_CHALLENGE_LIFETIME_MS, MAX_PAIRING_LIFETIME_MS};
use display_protocol::ids::{
    AuthenticationTag, Challenge, CoordinatorFingerprint, DisplayDeviceId, DisplayPairingId,
    PollKey, ProofKey,
};
use display_protocol::pairing::{
    authenticate_pairing_complete, authenticate_pairing_status, confirmation_phrase,
    validate_instance, CoordinatorInstance, PairingCompleteRequest, PairingCompleteResponse,
    PairingRejectionReason, PairingStartRequest, PairingStartResponse, PairingStatus,
    PairingStatusRequest,
};
use display_protocol::receiver::{
    validate_capabilities, ChallengeResponse, ReceiverCapabilities, ReceiverHealth,
};

use super::{CoordinatorStore, DeviceRecord};

const PAIRING_RETRY_AFTER_MS: u32 = 1_500;

#[derive(Debug, Clone)]
pub struct PendingPairingView {
    pub pairing: DisplayPairingId,
    pub confirmation_phrase: Vec<String>,
    pub coordinator_fingerprint: CoordinatorFingerprint,
    pub capabilities: ReceiverCapabilities,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

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
}

#[derive(Default)]
struct PairingState {
    pairings: BTreeMap<String, PendingPairing>,
    challenges: BTreeMap<String, ChallengeLease>,
    health: BTreeMap<String, ReceiverHealth>,
}

struct PendingPairing {
    poll_key: PollKey,
    capabilities: ReceiverCapabilities,
    confirmation_phrase: Vec<String>,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    decision: PairingDecision,
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
        })
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
        self.lock()?.pairings.insert(
            pairing.as_str().to_string(),
            PendingPairing {
                poll_key: request.poll_key,
                capabilities: request.capabilities,
                confirmation_phrase: phrase.clone(),
                created_at_unix_ms: now_unix_ms,
                expires_at_unix_ms,
                decision: PairingDecision::Pending,
            },
        );
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
        if label.trim().is_empty()
            || label.len() > display_protocol::bounds::MAX_LABEL_BYTES
            || label.chars().any(char::is_control)
        {
            return Err(anyhow!("display label is invalid"));
        }
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
        let (device, was_completed) = {
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
            (device.clone(), was_completed)
        };
        let next_challenge = random_challenge()?;
        state.challenges.insert(
            device.as_str().to_string(),
            ChallengeLease {
                challenge: next_challenge.clone(),
                expires_at_unix_ms: challenge_expiry(now_unix_ms)?,
            },
        );
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
        self.lock()?
            .health
            .insert(device.as_str().to_string(), health);
        Ok(())
    }

    pub fn health(&self, device: &DisplayDeviceId) -> Result<Option<ReceiverHealth>> {
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
        std::env::temp_dir().join(format!(
            "lait-pairing-test-{}-{}",
            std::process::id(),
            mechanics::wallclock::now_millis()
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
}
