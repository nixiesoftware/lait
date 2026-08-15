use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use display_protocol::auth::{
    authenticate_request, sha256, RequestContext, RequestMethod, RequestRoute,
    AUTHORIZATION_SCHEME, HEADER_ASSET, HEADER_ASSIGNMENT, HEADER_BODY_SHA256, HEADER_CHALLENGE,
    HEADER_CURRENT_ITEM, HEADER_DEVICE, HEADER_ELAPSED_MS, HEADER_PROGRAM, HEADER_PROTOCOL_MAJOR,
    HEADER_REVISION, HEADER_ROUTE, HEADER_WAIT_MS,
};
use display_protocol::bounds::{
    MAX_ASSET_BYTES, MAX_HEALTH_BODY_BYTES, MAX_HTTP_BODY_BYTES, MAX_PAIRING_BODY_BYTES,
    MAX_PROGRAM_BODY_BYTES, MAX_STAGED_BYTES,
};
use display_protocol::ids::{
    Challenge, DisplayAssetId, DisplayAssignmentId, DisplayDeviceId, DisplayProgramId,
    DisplayProgramItemId, PollKey, ProgramRevision, ProofKey, ReceiverNonce,
};
use display_protocol::pairing::{
    authenticate_pairing_complete, authenticate_pairing_status, confirmation_phrase,
    validate_challenge_lifetime, validate_instance, validate_pairing_start_response,
    validate_pairing_status, CoordinatorInstance, CoordinatorTrust, PairingCompleteRequest,
    PairingCompleteResponse, PairingStartRequest, PairingStartResponse, PairingStatus,
    PairingStatusRequest, ReceiverBootstrap,
};
use display_protocol::program::{
    validate_program, DisplayAsset, DisplayAssetMediaType, DisplayPlayback, DisplayProgram,
    DisplayScene, ProgramChange,
};
use display_protocol::receiver::{
    validate_capabilities, validate_health, ApiRefusal, ApiRefusalCode, ChallengeRequest,
    ChallengeResponse, ConnectionState, Fault, LatencyBucket, ReceiverCapabilities, ReceiverHealth,
};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::playback::{Presenter, Runtime, StagedAsset};
use crate::state::{CredentialState, Vault};
use crate::transport::{HttpResponse, Transport};

#[derive(Debug)]
enum SessionSignal {
    RePair,
    Revoked,
}

impl fmt::Display for SessionSignal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RePair => formatter.write_str("coordinator requires a new pairing ceremony"),
            Self::Revoked => formatter.write_str("receiver credential was revoked"),
        }
    }
}

impl std::error::Error for SessionSignal {}

enum SessionDisposition {
    RePair,
    Revoked,
}

struct Session {
    device: DisplayDeviceId,
    proof_key: ProofKey,
    challenge: Option<Challenge>,
}

struct AuthorizedFields<'a> {
    assignment: Option<&'a DisplayAssignmentId>,
    program: Option<&'a DisplayProgramId>,
    revision: Option<&'a ProgramRevision>,
    current_item: Option<&'a DisplayProgramItemId>,
    elapsed_ms: Option<u32>,
    wait_ms: Option<u32>,
    asset: Option<&'a DisplayAssetId>,
}

impl AuthorizedFields<'_> {
    const fn empty() -> Self {
        Self {
            assignment: None,
            program: None,
            revision: None,
            current_item: None,
            elapsed_ms: None,
            wait_ms: None,
            asset: None,
        }
    }
}

pub struct ReferenceReceiver {
    bootstrap: ReceiverBootstrap,
    transport: Transport,
    vault: Vault,
    capabilities: ReceiverCapabilities,
    presenter: Presenter,
    cache: PathBuf,
}

impl ReferenceReceiver {
    pub fn open(
        bootstrap: ReceiverBootstrap,
        state_directory: PathBuf,
        output_directory: PathBuf,
        capabilities: ReceiverCapabilities,
    ) -> Result<Self> {
        display_protocol::pairing::validate_bootstrap(&bootstrap)
            .context("validate receiver bootstrap")?;
        validate_capabilities(&capabilities).context("validate receiver capabilities")?;
        let agent = crate::tls::agent(&bootstrap.trust)?;
        let origin = crate::tls::origin(&bootstrap.trust).to_owned();
        let vault = Vault::open(state_directory)?;
        let cache = vault.directory().join("assets");
        fs::create_dir_all(&cache).context("create receiver asset cache")?;
        Ok(Self {
            bootstrap,
            transport: Transport::new(agent, origin),
            vault,
            capabilities,
            presenter: Presenter::open(output_directory)?,
            cache,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        let instance = self.fetch_instance()?;
        println!(
            "Connected securely to {} ({})",
            instance.label, instance.instance
        );
        loop {
            let Some(state) = self.vault.load()? else {
                self.start_pairing()?;
                continue;
            };
            if state.trust() != &self.bootstrap.trust {
                bail!(
                    "stored receiver credential belongs to different coordinator trust material ({})",
                    self.vault.path().display()
                );
            }
            match state {
                CredentialState::Pairing { .. } => {
                    let confirmed = self.confirm_pairing(state)?;
                    let enrolling = self.poll_pairing(&confirmed)?;
                    self.vault.save(&enrolling)?;
                }
                CredentialState::Enrolling { .. } => {
                    let (paired, challenge) = self.finish_enrollment(&state)?;
                    self.vault.save(&paired)?;
                    let disposition = self.run_paired(&paired, Some(challenge))?;
                    if self.handle_disposition(&paired, disposition)? {
                        return Ok(());
                    }
                }
                CredentialState::Paired { .. } => {
                    let disposition = self.run_paired(&state, None)?;
                    if self.handle_disposition(&state, disposition)? {
                        return Ok(());
                    }
                }
                CredentialState::Revoked { device, .. } => {
                    println!("Receiver {device} is revoked; re-enable or remove it in Astrolabe.");
                    return Ok(());
                }
            }
        }
    }

    fn handle_disposition(
        &mut self,
        paired: &CredentialState,
        disposition: SessionDisposition,
    ) -> Result<bool> {
        match disposition {
            SessionDisposition::RePair => {
                println!("Coordinator requires a fresh trust ceremony.");
                self.vault.clear()?;
                Ok(false)
            }
            SessionDisposition::Revoked => {
                let CredentialState::Paired { trust, device, .. } = paired else {
                    return Err(anyhow!("revocation received outside a paired session"));
                };
                self.vault.save(&CredentialState::Revoked {
                    trust: trust.clone(),
                    device: device.clone(),
                })?;
                self.presenter.unassigned(device.as_str())?;
                println!("Receiver {device} was revoked by Astrolabe.");
                Ok(true)
            }
        }
    }

    fn fetch_instance(&self) -> Result<CoordinatorInstance> {
        let response = self
            .transport
            .get("/head/v1/instance", &[], MAX_PAIRING_BODY_BYTES)?;
        let instance: CoordinatorInstance = decode_success_json(response, "coordinator instance")?;
        validate_instance(&instance).context("validate coordinator instance")?;
        if instance.trust != self.bootstrap.trust {
            bail!("coordinator instance does not match bootstrapped trust material");
        }
        Ok(instance)
    }

    fn start_pairing(&self) -> Result<()> {
        let receiver_nonce = random_receiver_nonce()?;
        let poll_key = random_poll_key()?;
        let request = PairingStartRequest {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            receiver_nonce: receiver_nonce.clone(),
            poll_key: poll_key.clone(),
            rendezvous: self.bootstrap.rendezvous.clone(),
            capabilities: self.capabilities.clone(),
        };
        let response: PairingStartResponse = self.public_post(
            "/head/v1/pairings",
            &request,
            MAX_PAIRING_BODY_BYTES,
            "pairing offer",
        )?;
        validate_pairing_start_response(&response).context("validate pairing offer")?;
        if let CoordinatorTrust::PinnedCertificate { sha256, .. } = &self.bootstrap.trust {
            if &response.coordinator_fingerprint != sha256 {
                bail!("pairing offer fingerprint differs from the TLS certificate pin");
            }
        }
        let phrase = confirmation_phrase(
            &response.coordinator_fingerprint,
            &response.pairing,
            &receiver_nonce,
        )
        .context("derive pairing confirmation phrase")?;
        if phrase != response.confirmation_phrase {
            bail!("pairing confirmation phrase failed its integrity check");
        }
        self.vault.save(&CredentialState::Pairing {
            trust: self.bootstrap.trust.clone(),
            pairing: response.pairing,
            receiver_nonce,
            poll_key,
            fingerprint: response.coordinator_fingerprint,
            phrase,
            user_confirmed: false,
        })
    }

    fn confirm_pairing(&self, state: CredentialState) -> Result<CredentialState> {
        let CredentialState::Pairing {
            trust,
            pairing,
            receiver_nonce,
            poll_key,
            fingerprint,
            phrase,
            user_confirmed,
        } = state
        else {
            return Err(anyhow!("receiver is not awaiting pairing confirmation"));
        };
        let expected = confirmation_phrase(&fingerprint, &pairing, &receiver_nonce)
            .context("verify stored pairing phrase")?;
        if expected != phrase {
            bail!("stored pairing phrase failed its integrity check");
        }
        if !user_confirmed {
            println!("\nAstrolabe pairing fingerprint:\n{fingerprint}");
            println!("\nConfirmation phrase:\n{}\n", phrase.join(" "));
            print!("Confirm the same fingerprint and words in Astrolabe, then type yes: ");
            io::stdout().flush().context("flush pairing prompt")?;
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .context("read local pairing confirmation")?;
            if answer.trim() != "yes" {
                bail!("pairing was not locally confirmed");
            }
        }
        let confirmed = CredentialState::Pairing {
            trust,
            pairing,
            receiver_nonce,
            poll_key,
            fingerprint,
            phrase,
            user_confirmed: true,
        };
        self.vault.save(&confirmed)?;
        Ok(confirmed)
    }

    fn poll_pairing(&self, state: &CredentialState) -> Result<CredentialState> {
        let CredentialState::Pairing {
            trust,
            pairing,
            poll_key,
            ..
        } = state
        else {
            return Err(anyhow!("receiver is not in pairing state"));
        };
        println!("Waiting for Astrolabe to approve pairing {pairing}…");
        loop {
            let proof = authenticate_pairing_status(poll_key, pairing)
                .context("authenticate pairing status request")?;
            let response: PairingStatus = self.public_post(
                "/head/v1/pairings/status",
                &PairingStatusRequest {
                    protocol_major: display_protocol::PROTOCOL_MAJOR,
                    pairing: pairing.clone(),
                    proof,
                },
                MAX_PAIRING_BODY_BYTES,
                "pairing status",
            )?;
            validate_pairing_status(&response).context("validate pairing status")?;
            match response {
                PairingStatus::Pending { retry_after_ms } => {
                    thread::sleep(Duration::from_millis(u64::from(retry_after_ms.max(1_000))));
                }
                PairingStatus::Approved {
                    device,
                    proof_key,
                    enrollment_challenge,
                } => {
                    return Ok(CredentialState::Enrolling {
                        trust: trust.clone(),
                        pairing: pairing.clone(),
                        device,
                        proof_key,
                        enrollment_challenge,
                    });
                }
                PairingStatus::Rejected { reason } => {
                    self.vault.clear()?;
                    bail!("Astrolabe rejected pairing: {reason:?}");
                }
                PairingStatus::Expired => {
                    self.vault.clear()?;
                    bail!("Astrolabe pairing offer expired; restart to begin a new ceremony");
                }
            }
        }
    }

    fn finish_enrollment(&self, state: &CredentialState) -> Result<(CredentialState, Challenge)> {
        let CredentialState::Enrolling {
            trust,
            pairing,
            device,
            proof_key,
            enrollment_challenge,
        } = state
        else {
            return Err(anyhow!("receiver is not enrolling"));
        };
        let proof = authenticate_pairing_complete(proof_key, pairing, device, enrollment_challenge)
            .context("authenticate pairing completion")?;
        let response: PairingCompleteResponse = self.public_post(
            "/head/v1/pairings/complete",
            &PairingCompleteRequest {
                protocol_major: display_protocol::PROTOCOL_MAJOR,
                pairing: pairing.clone(),
                device: device.clone(),
                enrollment_challenge: enrollment_challenge.clone(),
                proof,
            },
            MAX_PAIRING_BODY_BYTES,
            "pairing completion",
        )?;
        let (response_device, next_challenge) = match response {
            PairingCompleteResponse::Enrolled {
                device,
                next_challenge,
            }
            | PairingCompleteResponse::AlreadyEnrolled {
                device,
                next_challenge,
            } => (device, next_challenge),
        };
        if &response_device != device {
            bail!("pairing completion changed the receiver identity");
        }
        Ok((
            CredentialState::Paired {
                trust: trust.clone(),
                device: device.clone(),
                proof_key: proof_key.clone(),
            },
            next_challenge,
        ))
    }

    fn run_paired(
        &mut self,
        state: &CredentialState,
        initial_challenge: Option<Challenge>,
    ) -> Result<SessionDisposition> {
        let CredentialState::Paired {
            device, proof_key, ..
        } = state
        else {
            return Err(anyhow!("receiver is not paired"));
        };
        let mut session = Session {
            device: device.clone(),
            proof_key: proof_key.clone(),
            challenge: initial_challenge,
        };
        let mut runtime: Option<Runtime> = None;
        let mut backoff = Duration::from_secs(1);
        let mut capabilities_accepted = false;
        println!(
            "Receiver {} is paired and entering the live program loop.",
            device
        );
        loop {
            let result =
                self.session_iteration(&mut session, &mut runtime, &mut capabilities_accepted);
            match result {
                Ok(()) => backoff = Duration::from_secs(1),
                Err(error) => {
                    if let Some(signal) = error.downcast_ref::<SessionSignal>() {
                        return Ok(match signal {
                            SessionSignal::RePair => SessionDisposition::RePair,
                            SessionSignal::Revoked => SessionDisposition::Revoked,
                        });
                    }
                    if let Some(active) = runtime.as_mut() {
                        active.mark_health_due();
                    }
                    eprintln!("Display receiver recovering: {error:#}");
                    recovery_wait(runtime.as_mut(), &mut self.presenter, backoff)?;
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
                }
            }
        }
    }

    fn session_iteration(
        &mut self,
        session: &mut Session,
        runtime: &mut Option<Runtime>,
        capabilities_accepted: &mut bool,
    ) -> Result<()> {
        if !*capabilities_accepted {
            let body =
                serde_json::to_vec(&self.capabilities).context("encode receiver capabilities")?;
            let response = self.authorized(
                session,
                RequestMethod::Post,
                RequestRoute::Capabilities,
                "/head/v1/capabilities",
                &body,
                AuthorizedFields::empty(),
                MAX_HTTP_BODY_BYTES,
            )?;
            let accepted: Accepted = decode_success_json(response, "capability response")?;
            accepted.validate()?;
            *capabilities_accepted = true;
        }

        let Some(active) = runtime.as_mut() else {
            let response = self.authorized(
                session,
                RequestMethod::Get,
                RequestRoute::ProgramSnapshot,
                "/head/v1/program",
                &[],
                AuthorizedFields::empty(),
                MAX_PROGRAM_BODY_BYTES,
            )?;
            let change: ProgramChange = decode_success_json(response, "program snapshot")?;
            self.adopt_change(session, runtime, change, None)?;
            if runtime.is_none() {
                thread::sleep(Duration::from_secs(5));
            }
            return Ok(());
        };

        present_runtime(active, &mut self.presenter)?;
        if active.should_refresh_snapshot() {
            *runtime = None;
            return Ok(());
        }
        let health_due = active.health_due();
        let sent = active.playback()?;
        let wait_ms = active.wait_ms()?;
        let program = active.program();
        let item = program
            .items
            .get(usize::from(sent.current_index))
            .ok_or_else(|| anyhow!("receiver playback item is absent"))?;
        let fields = AuthorizedFields {
            assignment: Some(&program.assignment),
            program: Some(&program.program),
            revision: Some(&program.revision),
            current_item: Some(&item.id),
            elapsed_ms: Some(sent.elapsed_ms),
            wait_ms: Some(wait_ms),
            asset: None,
        };
        let response = self.authorized(
            session,
            RequestMethod::Get,
            RequestRoute::ProgramChanges,
            "/head/v1/program/changes",
            &[],
            fields,
            MAX_PROGRAM_BODY_BYTES,
        )?;
        let change: ProgramChange = decode_success_json(response, "program change")?;
        self.adopt_change(session, runtime, change, Some(&sent))?;
        if health_due {
            if let Some(active) = runtime.as_mut() {
                self.report_health(session, active)?;
                active.mark_health_reported();
            }
        }
        Ok(())
    }

    fn adopt_change(
        &mut self,
        session: &mut Session,
        runtime: &mut Option<Runtime>,
        change: ProgramChange,
        sent: Option<&DisplayPlayback>,
    ) -> Result<()> {
        match change {
            ProgramChange::Snapshot { program } => {
                validate_program(&program).context("validate display program")?;
                if runtime
                    .as_ref()
                    .is_some_and(|active| active.program().revision == program.revision)
                {
                    let active = runtime
                        .as_mut()
                        .ok_or_else(|| anyhow!("active receiver program disappeared"))?;
                    let response_cursor = program.playback.clone();
                    let sent_cursor = sent.unwrap_or(&response_cursor);
                    active.reconcile(&response_cursor, sent_cursor)?;
                    present_runtime(active, &mut self.presenter)?;
                    return Ok(());
                }
                let staged = self.stage_program(session, &program)?;
                let previous = runtime.take();
                let mut next = Runtime::new(program, staged);
                present_runtime(&mut next, &mut self.presenter)?;
                if let Some(previous) = previous {
                    cleanup_retired(previous.staged(), next.staged());
                }
                *runtime = Some(next);
                Ok(())
            }
            ProgramChange::NoChange { revision, playback } => {
                let active = runtime
                    .as_mut()
                    .ok_or_else(|| anyhow!("no-change response arrived without a program"))?;
                if active.program().revision != revision {
                    bail!("no-change response names a different program revision");
                }
                let sent = sent.ok_or_else(|| anyhow!("no-change response has no sent cursor"))?;
                active.reconcile(&playback, sent)?;
                present_runtime(active, &mut self.presenter)
            }
            ProgramChange::Reset { .. } => {
                *runtime = None;
                Ok(())
            }
            ProgramChange::Unassigned => {
                *runtime = None;
                self.presenter.unassigned(session.device.as_str())
            }
            ProgramChange::Revoked => Err(anyhow::Error::new(SessionSignal::Revoked)),
            ProgramChange::RePair => Err(anyhow::Error::new(SessionSignal::RePair)),
        }
    }

    fn stage_program(
        &self,
        session: &mut Session,
        program: &DisplayProgram,
    ) -> Result<BTreeMap<DisplayAssetId, StagedAsset>> {
        let mut staged = BTreeMap::new();
        let mut total = 0_u32;
        for item in &program.items {
            let asset = match &item.scene {
                DisplayScene::Frame { asset } => asset,
                DisplayScene::Blank { .. } => continue,
                DisplayScene::Media { .. } => {
                    bail!("reference frame receiver does not support media programs")
                }
            };
            if staged.contains_key(&asset.id) {
                continue;
            }
            total = total
                .checked_add(asset.encoded_len)
                .ok_or_else(|| anyhow!("program staged byte count overflow"))?;
            if total > self.capabilities.max_staged_bytes || total > MAX_STAGED_BYTES {
                bail!("program exceeds the negotiated staging byte bound");
            }
            let staged_asset = self.fetch_asset(session, program, asset)?;
            staged.insert(asset.id.clone(), staged_asset);
        }
        Ok(staged)
    }

    fn fetch_asset(
        &self,
        session: &mut Session,
        program: &DisplayProgram,
        asset: &DisplayAsset,
    ) -> Result<StagedAsset> {
        if asset.encoded_len > self.capabilities.max_asset_bytes
            || asset.encoded_len > MAX_ASSET_BYTES
        {
            bail!("asset exceeds the negotiated receiver byte bound");
        }
        let path = format!("/head/v1/assets/{}", asset.id);
        let fields = AuthorizedFields {
            assignment: Some(&program.assignment),
            program: Some(&program.program),
            revision: Some(&program.revision),
            current_item: None,
            elapsed_ms: None,
            wait_ms: None,
            asset: Some(&asset.id),
        };
        let response = self.authorized(
            session,
            RequestMethod::Get,
            RequestRoute::Asset,
            &path,
            &[],
            fields,
            usize::try_from(asset.encoded_len).context("convert asset byte bound")?,
        )?;
        verify_asset_response(&response, asset)?;
        let decoded = image::load_from_memory_with_format(&response.body, image::ImageFormat::Png)
            .context("decode staged PNG frame")?;
        if Some(decoded.width()) != asset.width || Some(decoded.height()) != asset.height {
            bail!("decoded frame dimensions do not match the program");
        }
        let final_path = self.cache.join(format!("{}.png", asset.id));
        let temporary = self.cache.join(format!("{}.png.tmp", asset.id));
        let mut file = File::create(&temporary).context("create staged frame candidate")?;
        file.write_all(&response.body)
            .context("write staged frame candidate")?;
        file.sync_all().context("flush staged frame candidate")?;
        drop(file);
        mechanics::secretfs::persist_replace(&temporary, &final_path)
            .context("commit verified staged frame")?;
        Ok(StagedAsset {
            descriptor: asset.clone(),
            path: final_path,
        })
    }

    fn report_health(&self, session: &mut Session, runtime: &mut Runtime) -> Result<()> {
        let (playback, current_item, displayed, playback_state, staged_items, staged_bytes) =
            runtime.health_sample()?;
        let health = ReceiverHealth {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            platform: self.capabilities.platform,
            build: self.capabilities.build.clone(),
            revision: runtime.program().revision.clone(),
            current_item: current_item.clone(),
            elapsed_ms: playback.elapsed_ms,
            last_displayed_asset: displayed,
            connection: ConnectionState::Online,
            playback: playback_state,
            last_error: Fault::None,
            staged_items,
            staged_bytes,
            decode_latency: LatencyBucket::Unobserved,
            swap_latency: LatencyBucket::Unobserved,
            drift_residual_ms: 0,
            correction_events: 0,
            pipeline_unobservable: true,
        };
        validate_health(&health).context("validate receiver health")?;
        let body = serde_json::to_vec(&health).context("encode receiver health")?;
        let program = runtime.program();
        let fields = AuthorizedFields {
            assignment: Some(&program.assignment),
            program: Some(&program.program),
            revision: Some(&program.revision),
            current_item: Some(&current_item),
            elapsed_ms: Some(playback.elapsed_ms),
            wait_ms: None,
            asset: None,
        };
        let response = self.authorized(
            session,
            RequestMethod::Post,
            RequestRoute::Health,
            "/head/v1/health",
            &body,
            fields,
            MAX_HEALTH_BODY_BYTES,
        )?;
        let accepted: Accepted = decode_success_json(response, "health response")?;
        accepted.validate()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the arguments are the closed authenticated HTTP request shape"
    )]
    fn authorized(
        &self,
        session: &mut Session,
        method: RequestMethod,
        route: RequestRoute,
        path: &str,
        body: &[u8],
        fields: AuthorizedFields<'_>,
        maximum_bytes: usize,
    ) -> Result<HttpResponse> {
        self.ensure_challenge(session)?;
        let challenge = session
            .challenge
            .take()
            .ok_or_else(|| anyhow!("receiver challenge disappeared before use"))?;
        let body_sha256 = sha256(body).context("digest display request body")?;
        let context = RequestContext {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            method,
            route,
            device: &session.device,
            assignment: fields.assignment,
            program: fields.program,
            revision: fields.revision,
            current_item: fields.current_item,
            elapsed_ms: fields.elapsed_ms,
            wait_ms: fields.wait_ms,
            asset: fields.asset,
            range: None,
            challenge: &challenge,
            body_sha256: &body_sha256,
        };
        let tag = authenticate_request(&session.proof_key, &context)
            .context("authenticate display request")?;
        let headers = request_headers(&context, &tag.to_string());
        let response = match method {
            RequestMethod::Get => self.transport.get(path, &headers, maximum_bytes),
            RequestMethod::Post => self.transport.post(path, &headers, body, maximum_bytes),
        }?;
        let next_header = response
            .next_challenge
            .as_deref()
            .map(Challenge::parse)
            .transpose()
            .context("validate next display challenge header")?;
        if (200..300).contains(&response.status) {
            let next = next_header
                .ok_or_else(|| anyhow!("authenticated response omitted its next challenge"))?;
            session.challenge = Some(next);
            return Ok(response);
        }
        let refusal: ApiRefusal = serde_json::from_slice(&response.body)
            .context("decode authenticated display refusal")?;
        validate_refusal(&refusal, next_header.as_ref())?;
        session.challenge = next_header;
        match refusal.code {
            ApiRefusalCode::RePairRequired => Err(anyhow::Error::new(SessionSignal::RePair)),
            ApiRefusalCode::Revoked => Err(anyhow::Error::new(SessionSignal::Revoked)),
            _ => Err(anyhow!(
                "coordinator refused {} with HTTP {} ({:?})",
                route.wire_name(),
                response.status,
                refusal.code
            )),
        }
    }

    fn ensure_challenge(&self, session: &mut Session) -> Result<()> {
        if session.challenge.is_some() {
            return Ok(());
        }
        let response: ChallengeResponse = self.public_post(
            "/head/v1/challenges",
            &ChallengeRequest {
                protocol_major: display_protocol::PROTOCOL_MAJOR,
                device: session.device.clone(),
            },
            MAX_PAIRING_BODY_BYTES,
            "receiver challenge",
        )?;
        if response.protocol_major != display_protocol::PROTOCOL_MAJOR {
            bail!("coordinator challenge uses an unsupported protocol major");
        }
        validate_challenge_lifetime(response.expires_in_ms)
            .context("validate receiver challenge lifetime")?;
        session.challenge = Some(response.challenge);
        Ok(())
    }

    fn public_post<Request: Serialize, Response: DeserializeOwned>(
        &self,
        path: &str,
        request: &Request,
        maximum_bytes: usize,
        name: &str,
    ) -> Result<Response> {
        let body = serde_json::to_vec(request).with_context(|| format!("encode {name}"))?;
        let response = self.transport.post(path, &[], &body, maximum_bytes)?;
        decode_success_json(response, name)
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    kind: String,
}

impl Accepted {
    fn validate(&self) -> Result<()> {
        if self.kind != "accepted" {
            bail!("coordinator response was not accepted");
        }
        Ok(())
    }
}

fn decode_success_json<T: DeserializeOwned>(response: HttpResponse, name: &str) -> Result<T> {
    if !(200..300).contains(&response.status) {
        bail!("coordinator returned HTTP {} for {name}", response.status);
    }
    let content_type = response
        .content_type
        .as_deref()
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type != Some("application/json") {
        bail!("coordinator returned a non-JSON {name}");
    }
    serde_json::from_slice(&response.body).with_context(|| format!("decode {name}"))
}

fn validate_refusal(refusal: &ApiRefusal, header: Option<&Challenge>) -> Result<()> {
    if refusal.protocol_major != display_protocol::PROTOCOL_MAJOR {
        bail!("coordinator refusal uses an unsupported protocol major");
    }
    if refusal
        .retry_after_ms
        .is_some_and(|retry| retry == 0 || retry > 60_000)
    {
        bail!("coordinator refusal retry interval is outside protocol bounds");
    }
    if refusal.next_challenge.as_ref() != header {
        bail!("coordinator refusal challenge body/header mismatch");
    }
    Ok(())
}

fn request_headers(context: &RequestContext<'_>, tag: &str) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "Authorization".to_owned(),
            format!("{AUTHORIZATION_SCHEME} {tag}"),
        ),
        (
            HEADER_PROTOCOL_MAJOR.to_owned(),
            context.protocol_major.to_string(),
        ),
        (
            HEADER_ROUTE.to_owned(),
            context.route.wire_name().to_owned(),
        ),
        (HEADER_DEVICE.to_owned(), context.device.to_string()),
        (HEADER_CHALLENGE.to_owned(), context.challenge.to_string()),
        (
            HEADER_BODY_SHA256.to_owned(),
            context.body_sha256.to_string(),
        ),
    ];
    optional_header(&mut headers, HEADER_ASSIGNMENT, context.assignment);
    optional_header(&mut headers, HEADER_PROGRAM, context.program);
    optional_header(&mut headers, HEADER_REVISION, context.revision);
    optional_header(&mut headers, HEADER_CURRENT_ITEM, context.current_item);
    optional_number(&mut headers, HEADER_ELAPSED_MS, context.elapsed_ms);
    optional_number(&mut headers, HEADER_WAIT_MS, context.wait_ms);
    optional_header(&mut headers, HEADER_ASSET, context.asset);
    headers
}

fn optional_header<T: fmt::Display>(
    headers: &mut Vec<(String, String)>,
    name: &str,
    value: Option<&T>,
) {
    if let Some(value) = value {
        headers.push((name.to_owned(), value.to_string()));
    }
}

fn optional_number(headers: &mut Vec<(String, String)>, name: &str, value: Option<u32>) {
    if let Some(value) = value {
        headers.push((name.to_owned(), value.to_string()));
    }
}

fn verify_asset_response(response: &HttpResponse, asset: &DisplayAsset) -> Result<()> {
    if response.content_length != Some(u64::from(asset.encoded_len)) {
        bail!("asset Content-Length does not match the program");
    }
    if response.body.len()
        != usize::try_from(asset.encoded_len).context("convert asset encoded length")?
    {
        bail!("asset body length does not match the program");
    }
    let expected_type = match asset.media_type {
        DisplayAssetMediaType::ImagePng => "image/png",
        DisplayAssetMediaType::ImageJpeg => "image/jpeg",
        DisplayAssetMediaType::ImageWebp => "image/webp",
        DisplayAssetMediaType::HlsManifest | DisplayAssetMediaType::DashManifest => {
            bail!("reference frame receiver was offered a media manifest")
        }
    };
    let actual_type = response
        .content_type
        .as_deref()
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if actual_type != Some(expected_type) {
        bail!("asset Content-Type does not match the program");
    }
    if sha256(&response.body).context("digest received asset")? != asset.sha256 {
        bail!("asset SHA-256 does not match the program");
    }
    Ok(())
}

fn present_runtime(runtime: &mut Runtime, presenter: &mut Presenter) -> Result<()> {
    let view = runtime.view()?;
    presenter.present(&view)
}

fn cleanup_retired(
    previous: &BTreeMap<DisplayAssetId, StagedAsset>,
    current: &BTreeMap<DisplayAssetId, StagedAsset>,
) {
    let retained: BTreeSet<&Path> = current.values().map(|asset| asset.path.as_path()).collect();
    for retired in previous.values() {
        if !retained.contains(retired.path.as_path()) {
            let _ = fs::remove_file(&retired.path);
        }
    }
}

fn recovery_wait(
    mut runtime: Option<&mut Runtime>,
    presenter: &mut Presenter,
    duration: Duration,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(duration)
        .ok_or_else(|| anyhow!("receiver recovery deadline overflow"))?;
    while Instant::now() < deadline {
        if let Some(active) = runtime.as_deref_mut() {
            present_runtime(active, presenter)?;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(250)));
    }
    Ok(())
}

fn random_receiver_nonce() -> Result<ReceiverNonce> {
    ReceiverNonce::parse(random_hex_32()?).context("construct receiver nonce")
}

fn random_poll_key() -> Result<PollKey> {
    PollKey::parse(random_hex_32()?).context("construct pairing poll key")
}

fn random_hex_32() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("obtain receiver randomness: {error}"))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        let high = usize::from(byte >> 4);
        let low = usize::from(byte & 0x0f);
        let high = HEX
            .get(high)
            .copied()
            .ok_or_else(|| anyhow!("encode receiver randomness"))?;
        let low = HEX
            .get(low)
            .copied()
            .ok_or_else(|| anyhow!("encode receiver randomness"))?;
        encoded.push(char::from(high));
        encoded.push(char::from(low));
    }
    Ok(encoded)
}
