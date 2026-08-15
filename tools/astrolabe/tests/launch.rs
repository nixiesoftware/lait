//! `Open`, against the real binary.
//!
//! Every other test in this crate proves a rule without a process. This one
//! proves the seam that has no rule in it and three places to be wrong: the
//! supervisor spawns `lait` with the flags it thinks the launcher takes, the
//! launcher accepts them, the head announces an address before it accepts, and
//! the address it announced answers `/api/launch` with a credential that is
//! worth exactly one use.
//!
//! It exists because that seam has already been wrong once. `start_head` passed
//! `--home` to a launcher mode that did not take it, so the head exited before
//! printing anything and the supervisor reported "head exited before it
//! announced an address" — a message that describes the symptom and names
//! nothing. No unit test could have caught it, because every part was correct
//! on its own.
//!
//! ## What it does not do
//!
//! It stops one call short of `open_world`, which hands the URL to the person's
//! browser. A test that opened a browser window on a CI runner — or on the
//! machine of whoever is running the suite — would be a test that costs more
//! than it proves.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use astrolabe::client::http::{post_json, Head};
use astrolabe::client::{display::DisplayAssignmentInput, Client};
use astrolabe::Config;

/// The `lait` this build produced, if it is there.
///
/// Found relative to the running test binary rather than by walking the
/// workspace: `target/<profile>/deps/<test>-<hash>.exe` puts it two levels up,
/// and that holds for every profile without knowing which one is in use.
fn sidecar() -> Option<PathBuf> {
    built_binary("lait")
}

fn reference_receiver() -> Option<PathBuf> {
    built_binary("astrolabe-display-reference")
}

fn built_binary(name: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let profile = current.parent()?.parent()?;
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let candidate = profile.join(name);
    candidate.is_file().then_some(candidate)
}

struct OwnedReceiver(Child);

impl Drop for OwnedReceiver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_for_daemon_stop(home: &Path) {
    for _ in 0..100 {
        let selection = lait::config::Selection::for_identity(home);
        let stopped = match lait::daemon::Client::for_selection(&selection) {
            Ok(daemon) if matches!(daemon.probe().await, lait::control::Probe::Absent) => {
                // The endpoint closes before the process has necessarily
                // released its single-instance lock while active Orbits drain.
                // A replacement is safe only after both are gone.
                lait::config::acquire_daemon_lock(daemon.home()).is_ok()
            }
            Err(_) => true,
            _ => false,
        };
        if stopped {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("identity daemon did not stop within its process bound");
}

async fn wait_for_pairing(client: &Client) -> String {
    for _ in 0..100 {
        let display = client
            .display_status()
            .await
            .expect("read display pairing status");
        if let Some(pairing) = display.pending_pairings.first() {
            return pairing.pairing.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("reference receiver did not open a pairing within ten seconds");
}

async fn wait_for_receiver(client: &Client) -> String {
    for _ in 0..100 {
        let display = client
            .display_status()
            .await
            .expect("read enrolled display status");
        if let Some(device) = display.devices.first() {
            if display.pending_pairings.is_empty() {
                return device.device.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("reference receiver did not complete enrollment within ten seconds");
}

async fn wait_for_unassigned(path: &Path, device: &str) {
    for _ in 0..150 {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if status["device"] == device && status["scene"]["reason"] == "unassigned" {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("enrolled receiver never presented its authenticated unassigned state");
}

async fn wait_for_assigned(path: &Path, assignment: &str, program: &str) -> (String, String) {
    for _ in 0..200 {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let (Some(revision), Some(item)) =
                    (status["revision"].as_str(), status["item"].as_str())
                {
                    if status["assignment"] == assignment
                        && status["program"] == program
                        && status["scene"]["kind"] == "frame"
                        && !revision.is_empty()
                        && !item.is_empty()
                    {
                        return (revision.to_owned(), item.to_owned());
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("assigned receiver never presented the compiled Signage frame");
}

async fn wait_for_revision_change(
    path: &Path,
    assignment: &str,
    program: &str,
    prior_revision: &str,
    phase: &str,
) -> (String, String) {
    for _ in 0..200 {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let (Some(revision), Some(item)) =
                    (status["revision"].as_str(), status["item"].as_str())
                {
                    if status["assignment"] == assignment
                        && status["program"] == program
                        && status["scene"]["kind"] == "frame"
                        && revision != prior_revision
                        && !item.is_empty()
                    {
                        return (revision.to_owned(), item.to_owned());
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("assigned receiver never received the {phase} semantic revision");
}

async fn wait_for_health(client: &Client, device: &str, revision: &str, item: &str) {
    for _ in 0..200 {
        let display = client.display_status().await.expect("read receiver health");
        if let Some(health) = display
            .devices
            .iter()
            .find(|row| row.device == device)
            .and_then(|row| row.health.as_ref())
            .filter(|health| health.revision == revision && health.current_item == item)
        {
            assert_eq!(health.connection, "online");
            assert_eq!(health.playback, "displaying");
            assert_eq!(health.last_error, "none");
            assert_eq!(health.staged_items, 1);
            assert!(health.staged_bytes > 0);
            assert!(health.pipeline_unobservable);
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("coordinator never observed health for the presented Signage revision");
}

async fn seed_signage_program(client: &Client, store: &Path) -> (String, String) {
    client
        .space_found(
            &store.to_string_lossy(),
            "Display recovery",
            Some("Astrolabe".into()),
        )
        .await
        .expect("found a real Space for the Signage program");
    let context = client
        .host_context()
        .await
        .expect("read the founded Signage Orbit");
    let canonical_store = store.canonicalize().expect("canonical Signage store");
    let orbit = context
        .orbits
        .iter()
        .find(|orbit| {
            Path::new(&orbit.path)
                .canonicalize()
                .is_ok_and(|path| path == canonical_store)
        })
        .expect("founded Signage Orbit is registered");
    let program = signage::SignageProgram {
        id: replica::body::BodyId::from_bytes([9; 16]).render(),
        name: "Restart proof".into(),
        cycle: signage::ProgramCycle::Loop,
        items: vec![signage::SignageItem {
            id: "welcome".into(),
            title: "Astrolabe is coordinating this display".into(),
            body: "This frame came from the durable Signage World.".into(),
            background: "102030".into(),
            foreground: "ffffff".into(),
            duration_ms: Some(60_000),
        }],
        windows: Vec::new(),
    };
    write_signage_program(client, store, &orbit.space, program.clone()).await;
    (orbit.space.clone(), program.id)
}

async fn write_signage_program(
    client: &Client,
    store: &Path,
    space: &str,
    program: signage::SignageProgram,
) {
    let space = mechanics::ids::SpaceId::parse(space).expect("founded Space id");
    let call = signage_app::encode_call(&signage_app::SignageRequest::ProgramPut {
        program: program.clone(),
    })
    .expect("encode Signage program write");
    let reply = client
        .daemon()
        .expect("identity daemon for Signage write")
        .call_world(
            lait::control::ControlRoute::World {
                address: lait::control::OrbitAddress::for_store(store, space),
                world: signage::contract::PRODUCT_WORLD.into(),
            },
            call.clone(),
            None,
        )
        .await
        .expect("write the Signage program through its real World adapter");
    let decoded = signage_app::decode_reply(&call, reply).expect("decode Signage write reply");
    let response: signage_app::SignageResponse =
        serde_json::from_value(decoded).expect("typed Signage write reply");
    assert!(
        matches!(response, signage_app::SignageResponse::Saved { program: ref saved } if saved == &program.id),
        "Signage World did not save the receiver program: {response:?}"
    );
}

async fn schedule_signage_boundary(
    client: &Client,
    store: &Path,
    space: &str,
    program: &str,
) -> u64 {
    let now = mechanics::wallclock::now_millis();
    let boundary = now
        .checked_add(10_999)
        .map(|value| value / 1_000 * 1_000)
        .expect("schedule boundary within the test clock");
    let start = boundary.saturating_sub(60_000);
    let local = |unix_ms: u64| {
        jiff::Timestamp::from_millisecond(i64::try_from(unix_ms).expect("test time fits i64"))
            .expect("valid test timestamp")
            .to_zoned(jiff::tz::TimeZone::UTC)
            .datetime()
            .to_string()
    };
    let scheduled = signage::SignageProgram {
        id: program.to_owned(),
        name: "Boundary proof".into(),
        cycle: signage::ProgramCycle::HoldLast,
        items: vec![
            signage::SignageItem {
                id: "before-boundary".into(),
                title: "Before the schedule boundary".into(),
                body: "The coordinator is holding an exact wake deadline.".into(),
                background: "102030".into(),
                foreground: "ffffff".into(),
                duration_ms: Some(60_000),
            },
            signage::SignageItem {
                id: "after-boundary".into(),
                title: "After the schedule boundary".into(),
                body: "This revision arrived without another World write.".into(),
                background: "305010".into(),
                foreground: "ffffff".into(),
                duration_ms: Some(60_000),
            },
        ],
        windows: vec![
            signage::SignageWindow {
                id: "before".into(),
                window: schedule::Window {
                    start_local: local(start),
                    duration_ms: boundary.saturating_sub(start),
                    recurrence: schedule::Recurrence::None,
                    until_unix_ms: None,
                    priority: 0,
                    enabled: true,
                    timezone: "UTC".into(),
                    exceptions: Vec::new(),
                },
                items: vec!["before-boundary".into()],
            },
            signage::SignageWindow {
                id: "after".into(),
                window: schedule::Window {
                    start_local: local(boundary),
                    duration_ms: 60_000,
                    recurrence: schedule::Recurrence::None,
                    until_unix_ms: None,
                    priority: 0,
                    enabled: true,
                    timezone: "UTC".into(),
                    exceptions: Vec::new(),
                },
                items: vec!["after-boundary".into()],
            },
        ],
    };
    assert!(scheduled.validate(), "scheduled Signage program is valid");
    write_signage_program(client, store, space, scheduled).await;
    boundary
}

async fn wait_for_receiver_exit(receiver: &mut OwnedReceiver) {
    for _ in 0..150 {
        if let Some(status) = receiver.0.try_wait().expect("inspect reference receiver") {
            assert!(
                status.success(),
                "reference receiver exited unsuccessfully: {status}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("revoked reference receiver did not leave its live loop");
}

/// Stop the daemon the head started under itself.
///
/// The head spawns it; this supervisor never owned it, so nothing here may
/// force-kill it. Asking it to stop is the only move available, and it is the
/// right one — the same request `reload` uses.
async fn stop_daemon(home: &Path) {
    let selection = lait::config::Selection::for_identity(home);
    let Ok(daemon) = lait::daemon::Client::for_selection(&selection) else {
        return;
    };
    let _ = daemon
        .request(
            lait::control::ControlRoute::Daemon,
            &lait::control::Request::Stop,
            None,
        )
        .await;
}

/// The whole handoff, minus the browser.
///
/// One test rather than four, because the value is in the chain: a head that
/// comes up but mints nothing, and a ticket that mints but never expires, are
/// both failures of `Open` rather than of a component.
#[tokio::test(flavor = "multi_thread")]
async fn a_head_comes_up_and_mints_a_credential_worth_exactly_one_use() {
    let Some(executable) = sidecar() else {
        // A failure, not a skip. This is the one test that exercises the
        // client-to-process seam against a real binary, and that seam has been
        // wrong twice — both times with every component correct and the
        // composition wrong. A run that cannot find `lait` has proven nothing,
        // and reporting `ok` for it is how the guard comes to be trusted while
        // guarding nothing.
        panic!(
            "no lait binary beside the test binary, so the launch seam was not exercised.              Build it first: `cargo build -p lait`, or run the suite that does              (`cargo nextest run --workspace`)."
        );
    };

    let managed = tempfile::tempdir().expect("a managed root");
    let identity = tempfile::tempdir().expect("an identity home");

    let mut config = Config::new(managed.path().to_path_buf(), executable.clone());
    config.identity = Some(identity.path().to_path_buf());
    let (client, signals) = Client::start(config)
        .await
        .expect("a client that starts its identity daemon");

    let selection = lait::config::Selection::for_identity(identity.path());
    let daemon = lait::daemon::Client::for_selection(&selection).expect("the identity daemon");
    assert!(
        matches!(daemon.probe().await, lait::control::Probe::Healthy),
        "client startup returned before its identity daemon answered"
    );
    let displays = client
        .display_status()
        .await
        .expect("the daemon-owned display coordinator answers Astrolabe");
    assert!(
        displays.origin.starts_with("https://")
            && !displays.certificate_sha256.is_empty()
            && displays
                .certificate_pem
                .starts_with("-----BEGIN CERTIFICATE-----\n"),
        "the display coordinator announced no pinned HTTPS identity"
    );
    assert!(
        displays.surfaces.iter().any(|surface| {
            surface.world == "com.lait.signage" && surface.surface == "signage.program"
        }),
        "the process serving Astrolabe omitted the bundled Signage display surface"
    );
    let first_pid = lait::config::daemon_pid(daemon.home()).expect("the started daemon's pid");

    // A second client is the existing-daemon half of the startup contract. It
    // attaches to the process already serving this identity rather than racing
    // it with another sidecar spawn.
    let second_managed = tempfile::tempdir().expect("another managed root");
    let mut second_config = Config::new(second_managed.path().to_path_buf(), executable.clone());
    second_config.identity = Some(identity.path().to_path_buf());
    let (attached, _attached_signals) = Client::start(second_config)
        .await
        .expect("a second client that attaches to the running identity daemon");
    assert_eq!(
        lait::config::daemon_pid(daemon.home()),
        Some(first_pid),
        "attaching to a running identity started a competing daemon"
    );
    let attached_displays = attached
        .display_status()
        .await
        .expect("the attached client reaches the same display coordinator");
    assert_eq!(
        attached_displays.instance, displays.instance,
        "attaching to the daemon produced a second display coordinator"
    );
    assert_eq!(
        attached_displays.certificate_sha256, displays.certificate_sha256,
        "the attached client observed another display trust identity"
    );
    attached.shutdown().await;

    let (signage_orbit, signage_program) = seed_signage_program(&client, identity.path()).await;

    // Run the real receiver binary against the real coordinator. This is the
    // restart/recovery seam: the public certificate copied by Astrolabe must
    // establish TLS, both halves of the pairing ceremony must enroll one
    // durable device, and the same protected receiver credential must be able
    // to authenticate to a fresh daemon process before revocation can stop it.
    let (client, signals) = if let Some(receiver_executable) = reference_receiver() {
        let receiver_root = tempfile::tempdir().expect("reference receiver root");
        let bootstrap_path = receiver_root.path().join("bootstrap.json");
        let state_path = receiver_root.path().join("state");
        let output_path = receiver_root.path().join("output");
        let bootstrap = serde_json::json!({
            "protocol_major": 1,
            "trust": {
                "kind": "pinned_certificate",
                "origin": displays.origin.clone(),
                "sha256": displays.certificate_sha256.clone(),
            },
            "certificate_pem": displays.certificate_pem.clone(),
            "rendezvous": null,
        });
        std::fs::write(
            &bootstrap_path,
            serde_json::to_vec_pretty(&bootstrap).expect("encode receiver bootstrap"),
        )
        .expect("write receiver bootstrap");
        let child = Command::new(receiver_executable)
            .arg("--bootstrap")
            .arg(&bootstrap_path)
            .arg("--state")
            .arg(&state_path)
            .arg("--output")
            .arg(&output_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch the reference display receiver");
        let mut receiver = OwnedReceiver(child);
        let pairing = wait_for_pairing(&client).await;
        receiver
            .0
            .stdin
            .take()
            .expect("receiver confirmation input")
            .write_all(b"yes\n")
            .expect("confirm pairing at the receiver");
        client
            .display_pairing_approve(pairing, "Restart receiver".to_owned())
            .await
            .expect("approve the receiver in Astrolabe");
        let device = wait_for_receiver(&client).await;
        wait_for_unassigned(&output_path.join("active.json"), &device).await;
        client
            .display_assignment_put(DisplayAssignmentInput {
                device: device.clone(),
                // The Astrolabe surface selects by Space. The client boundary
                // resolves this to the exact local Orbit id before it reaches
                // the daemon.
                orbit: signage_orbit,
                world: signage::contract::PRODUCT_WORLD.into(),
                surface: "signage.program".into(),
                input: serde_json::json!({ "program": signage_program }),
                theme: lait::control::DisplayThemeSetting::Dark,
                stale_after_ms: 60_000,
                on_stale: lait::control::DisplayStaleActionSetting::Blank,
                sync: None,
                expires_at_unix_ms: None,
            })
            .await
            .expect("assign the durable Signage program in Astrolabe");
        let assigned_status = client
            .display_status()
            .await
            .expect("read committed display assignment");
        let assignment = assigned_status
            .assignments
            .iter()
            .find(|row| row.device == device && row.revoked_at_unix_ms.is_none())
            .expect("the receiver has one active assignment");
        let assignment_id = assignment.assignment.clone();
        let receiver_program = assignment.program.clone();
        let (revision, item) = wait_for_assigned(
            &output_path.join("active.json"),
            &assignment_id,
            &receiver_program,
        )
        .await;
        let frame = std::fs::read(output_path.join("frame.png"))
            .expect("read the atomically presented Signage frame");
        assert_eq!(
            frame.get(..8),
            Some(b"\x89PNG\r\n\x1a\n".as_slice()),
            "the assigned Signage surface did not present a PNG frame"
        );
        wait_for_health(&client, &device, &revision, &item).await;

        // One World write installs two civil-time windows. The invalidation
        // pushes the first semantic revision; after that, no actor or World
        // mutation occurs. The package's exact boundary deadline completes the
        // held long poll, recompiles, and pushes the second semantic revision.
        let boundary = schedule_signage_boundary(
            &client,
            identity.path(),
            &assignment.space,
            &signage_program,
        )
        .await;
        let (before_revision, _) = wait_for_revision_change(
            &output_path.join("active.json"),
            &assignment_id,
            &receiver_program,
            &revision,
            "pre-boundary",
        )
        .await;
        let before_frame =
            std::fs::read(output_path.join("frame.png")).expect("read pre-boundary frame");
        assert!(
            mechanics::wallclock::now_millis() < boundary,
            "the test failed to observe the pre-boundary revision before its deadline"
        );
        let (revision, item) = wait_for_revision_change(
            &output_path.join("active.json"),
            &assignment_id,
            &receiver_program,
            &before_revision,
            "post-boundary",
        )
        .await;
        assert!(
            mechanics::wallclock::now_millis() >= boundary,
            "the semantic revision changed before the schedule boundary"
        );
        let after_frame =
            std::fs::read(output_path.join("frame.png")).expect("read post-boundary frame");
        assert_ne!(
            before_frame, after_frame,
            "the boundary revision did not change presented content"
        );
        wait_for_health(&client, &device, &revision, &item).await;

        client.shutdown().await;
        drop(signals);
        drop(client);
        stop_daemon(identity.path()).await;
        wait_for_daemon_stop(identity.path()).await;
        let mut restarted_config = Config::new(managed.path().to_path_buf(), executable.clone());
        restarted_config.identity = Some(identity.path().to_path_buf());
        let (restarted, restarted_signals) = Client::start(restarted_config)
            .await
            .expect("Astrolabe restarts the identity daemon");
        let restarted_displays = restarted
            .display_status()
            .await
            .expect("restarted daemon restores display state");
        assert_eq!(restarted_displays.instance, displays.instance);
        assert_eq!(
            restarted_displays.certificate_sha256,
            displays.certificate_sha256
        );
        assert!(
            restarted_displays
                .devices
                .iter()
                .any(|row| row.device == device),
            "restarted daemon lost its enrolled display"
        );
        assert!(
            restarted_displays.assignments.iter().any(|row| {
                row.assignment == assignment_id
                    && row.program == receiver_program
                    && row.revoked_at_unix_ms.is_none()
            }),
            "restarted daemon lost its active Signage assignment"
        );
        wait_for_health(&restarted, &device, &revision, &item).await;
        restarted
            .display_device_revoke(device)
            .await
            .expect("revoke the recovered receiver");
        wait_for_receiver_exit(&mut receiver).await;
        (restarted, restarted_signals)
    } else {
        eprintln!(
            "no astrolabe-display-reference binary beside the test binary; skipping the real receiver restart seam"
        );
        (client, signals)
    };

    let head = client.head().await.expect("a head for this identity");
    assert!(
        head.base.starts_with("http://127.0.0.1:"),
        "a head came up somewhere other than loopback: {}",
        head.base
    );
    assert!(!head.token.is_empty(), "a head announced no credential");

    // The head has to serve the identity the Library was read from. This is the
    // assertion the defect above walked straight past: a head started at the
    // daemon's own directory comes up, announces an address and mints a ticket
    // — and serves a self-contained identity nobody has ever used, so `Open`
    // lands on a head with no Spaces in it.
    let orientation = post_json(
        &head,
        "/api/host/rpc",
        &serde_json::json!({ "cmd": "host_context" }),
    )
    .await
    .expect("the head answers for an identity");
    let serving = orientation["identity_home"]
        .as_str()
        .expect("a head that says which identity it serves");
    let expected = client.identity().expect("a bound identity");
    assert_eq!(
        Path::new(serving).canonicalize().ok(),
        expected.canonicalize().ok(),
        "the head serves {serving}, and this client is bound to {}",
        expected.display()
    );

    // Asking twice finds the head that is already up. The alternative is a port
    // and a run credential per click.
    let again = client.head().await.expect("the same head");
    assert_eq!(again, head, "a second Open started a second head");
    assert_eq!(
        client.heads().len(),
        1,
        "one identity acquired more than one head"
    );

    let minted = client.mint(&head).await.expect("a launch credential");
    assert!(!minted.secret.is_empty());
    assert!(
        minted.expires_at_ms
            > u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("a clock after the epoch")
                    .as_millis()
            )
            .unwrap_or(u64::MAX),
        "a launch credential was minted already expired"
    );

    let launch = Client::launch_url(&head.base, "/", &minted.secret, minted.expires_at_ms)
        .expect("a launch url");
    assert!(
        launch.url.contains(&minted.secret),
        "the composed URL carries no credential"
    );

    // Two tickets are two credentials. A head that answered the same secret
    // twice would make "single-use" a property of the first launch only.
    let second = client.mint(&head).await.expect("a second credential");
    assert_ne!(
        second.secret, minted.secret,
        "a head minted the same credential twice"
    );

    // And a request the credential does not authorise is refused. The launch
    // ticket is weaker than the run token by construction; this is the run
    // token being what `/api/launch` actually requires.
    let unauthorised = Head {
        base: head.base.clone(),
        token: "0".repeat(64),
    };
    let refused = post_json(
        &unauthorised,
        "/api/launch",
        &serde_json::json!({ "orbit": "orb_test" }),
    )
    .await
    .expect_err("a head minted a credential for an unauthenticated caller");
    assert!(
        !refused.retryable,
        "a refusal was reported as worth trying again: {refused}"
    );

    client.shutdown().await;
    drop(signals);
    stop_daemon(identity.path()).await;
}
