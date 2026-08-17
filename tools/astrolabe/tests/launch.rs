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

async fn wait_for_new_receiver(client: &Client, existing: &str) -> String {
    for _ in 0..100 {
        let display = client
            .display_status()
            .await
            .expect("read enrolled display status");
        if let Some(device) = display
            .devices
            .iter()
            .find(|device| device.device != existing)
        {
            if display.pending_pairings.is_empty() {
                return device.device.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("second reference receiver did not complete enrollment within ten seconds");
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
            assert!(
                (1..=2).contains(&health.staged_items),
                "receiver staged an unexpected number of Signage frames"
            );
            assert!(health.staged_bytes > 0);
            assert!(health.pipeline_unobservable);
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("coordinator never observed health for the presented Signage revision");
}

async fn wait_for_group_boundary(first: &Path, second: &Path, group: &str) {
    let read = |path: &Path| {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    };
    let first_frame = first.with_file_name("frame.png");
    let second_frame = second.with_file_name("frame.png");
    let mut initial = None;
    for _ in 0..400 {
        if let (Some(first), Some(second)) = (read(first), read(second)) {
            let aligned = first["sync"]["group"] == group
                && second["sync"]["group"] == group
                && first["sync"]["mode"] == "stay_in_sync"
                && second["sync"]["mode"] == "stay_in_sync";
            if aligned {
                if let (Ok(first_bytes), Ok(second_bytes)) =
                    (std::fs::read(&first_frame), std::fs::read(&second_frame))
                {
                    if first_bytes == second_bytes {
                        initial = Some(first_bytes);
                        break;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let initial = initial.expect("two receivers never adopted one boundary-sync target");

    let observed = std::time::Instant::now();
    let mut first_boundary = None;
    let mut second_boundary = None;
    for _ in 0..400 {
        let first_bytes = std::fs::read(&first_frame).ok();
        let second_bytes = std::fs::read(&second_frame).ok();
        if first_boundary.is_none() && first_bytes.as_ref().is_some_and(|bytes| bytes != &initial) {
            first_boundary = Some(observed.elapsed());
        }
        if second_boundary.is_none() && second_bytes.as_ref().is_some_and(|bytes| bytes != &initial)
        {
            second_boundary = Some(observed.elapsed());
        }
        if let (Some(first_at), Some(second_at), Some(first_bytes), Some(second_bytes)) =
            (first_boundary, second_boundary, first_bytes, second_bytes)
        {
            assert_eq!(
                first_bytes, second_bytes,
                "sync group advanced to different presented frames"
            );
            assert!(
                first_at.abs_diff(second_at) <= Duration::from_millis(500),
                "boundary-synced receivers drifted by more than 500 ms"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("two assigned receivers never crossed a shared program boundary");
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
        items: vec![
            signage::SignageItem {
                id: "welcome".into(),
                title: "Astrolabe is coordinating this display".into(),
                body: "This frame came from the durable Signage World.".into(),
                background: "102030".into(),
                foreground: "ffffff".into(),
                live_resource: None,
                duration_ms: Some(2_000),
            },
            signage::SignageItem {
                id: "coordinated".into(),
                title: "Receivers share this program boundary".into(),
                body: "Astrolabe supplied one group-aligned cursor.".into(),
                background: "305010".into(),
                foreground: "ffffff".into(),
                live_resource: None,
                duration_ms: Some(2_000),
            },
        ],
        windows: Vec::new(),
    };
    write_signage_program(client, store, &orbit.space, program.clone()).await;
    (orbit.space.clone(), program.id)
}

/// The store path as the daemon spelled it when it registered the Orbit.
///
/// The same resolution `client/display.rs` performs for a real caller: ask the
/// host for its Orbits and take the registered path, rather than assuming this
/// process and the daemon spell one directory the same way.
async fn registered_store(client: &Client, space: &str) -> String {
    let context = client
        .host_context()
        .await
        .expect("read the registered Orbits");
    context
        .orbits
        .iter()
        .find(|orbit| orbit.space == space)
        .map(|orbit| orbit.path.clone())
        .expect("the Space has a registered local Orbit")
}

async fn write_signage_program(
    client: &Client,
    store: &Path,
    space: &str,
    program: signage::SignageProgram,
) {
    let space_id = mechanics::ids::SpaceId::parse(space).expect("founded Space id");
    // Address the Orbit by the path the *daemon* registered, never by the one
    // this test happens to hold. The Orbit id is derived from the path as
    // spelled — `normalize` settles separators, trailing slashes and Windows
    // case and deliberately resolves nothing — so two spellings of one
    // directory are two Orbits, and the host answers `InvalidCall`. A tempdir
    // reaches the daemon canonicalised, and neither spelling is recoverable
    // from the other: macOS adds `/private`, and Windows `canonicalize`
    // returns a `\\?\` UNC path the daemon never used. Production has this
    // right (`client/display.rs` resolves through `host_context`); the test
    // was the half that guessed, which is why it passed only where tempdirs
    // are already canonical.
    let store = registered_store(client, space).await;
    let store = std::path::Path::new(&store);
    let space = space_id;
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
                live_resource: None,
                duration_ms: Some(60_000),
            },
            signage::SignageItem {
                id: "after-boundary".into(),
                title: "After the schedule boundary".into(),
                body: "This revision arrived without another World write.".into(),
                background: "305010".into(),
                foreground: "ffffff".into(),
                live_resource: None,
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
        let child = Command::new(&receiver_executable)
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
        let sync_group = "lobby-wall";
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
                sync: Some(lait::control::DisplayAssignmentSyncSetting {
                    group: sync_group.into(),
                    mode: lait::control::DisplaySyncModeSetting::Positional,
                    static_delay_ms: 0,
                }),
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

        // A second independently paired process joins the same requested
        // positional group. Both reference receivers declare boundary-only
        // sync, so the coordinator must degrade the whole group to one shared
        // boundary cursor rather than pretend positional guarantees exist.
        let second_receiver_root = tempfile::tempdir().expect("second reference receiver root");
        let second_bootstrap_path = second_receiver_root.path().join("bootstrap.json");
        let second_state_path = second_receiver_root.path().join("state");
        let second_output_path = second_receiver_root.path().join("output");
        std::fs::write(
            &second_bootstrap_path,
            serde_json::to_vec_pretty(&bootstrap).expect("encode second receiver bootstrap"),
        )
        .expect("write second receiver bootstrap");
        let second_child = Command::new(&receiver_executable)
            .arg("--bootstrap")
            .arg(&second_bootstrap_path)
            .arg("--state")
            .arg(&second_state_path)
            .arg("--output")
            .arg(&second_output_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch the second reference display receiver");
        let mut second_receiver = OwnedReceiver(second_child);
        let second_pairing = wait_for_pairing(&client).await;
        second_receiver
            .0
            .stdin
            .take()
            .expect("second receiver confirmation input")
            .write_all(b"yes\n")
            .expect("confirm pairing at the second receiver");
        client
            .display_pairing_approve(second_pairing, "Synced receiver".to_owned())
            .await
            .expect("approve the second receiver in Astrolabe");
        let second_device = wait_for_new_receiver(&client, &device).await;
        wait_for_unassigned(&second_output_path.join("active.json"), &second_device).await;
        client
            .display_assignment_put(DisplayAssignmentInput {
                device: second_device.clone(),
                orbit: assignment.space.clone(),
                world: signage::contract::PRODUCT_WORLD.into(),
                surface: "signage.program".into(),
                input: serde_json::json!({ "program": signage_program }),
                theme: lait::control::DisplayThemeSetting::Dark,
                stale_after_ms: 60_000,
                on_stale: lait::control::DisplayStaleActionSetting::Blank,
                sync: Some(lait::control::DisplayAssignmentSyncSetting {
                    group: sync_group.into(),
                    mode: lait::control::DisplaySyncModeSetting::Positional,
                    static_delay_ms: 0,
                }),
                expires_at_unix_ms: None,
            })
            .await
            .expect("assign the second receiver to the sync group");
        let synced_status = client
            .display_status()
            .await
            .expect("read the second committed display assignment");
        let second_assignment = synced_status
            .assignments
            .iter()
            .find(|row| row.device == second_device && row.revoked_at_unix_ms.is_none())
            .expect("the second receiver has one active assignment");
        assert_eq!(
            second_assignment
                .sync
                .as_ref()
                .map(|sync| sync.group.as_str()),
            Some(sync_group),
            "the second assignment lost its sync group"
        );
        let second_assignment_id = second_assignment.assignment.clone();
        let second_receiver_program = second_assignment.program.clone();
        let (second_revision, second_item) = wait_for_assigned(
            &second_output_path.join("active.json"),
            &second_assignment_id,
            &second_receiver_program,
        )
        .await;
        wait_for_health(&client, &second_device, &second_revision, &second_item).await;
        wait_for_group_boundary(
            &output_path.join("active.json"),
            &second_output_path.join("active.json"),
            sync_group,
        )
        .await;

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
        let (second_before_revision, _) = wait_for_revision_change(
            &second_output_path.join("active.json"),
            &second_assignment_id,
            &second_receiver_program,
            &second_revision,
            "second pre-boundary",
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
        let (second_revision, second_item) = wait_for_revision_change(
            &second_output_path.join("active.json"),
            &second_assignment_id,
            &second_receiver_program,
            &second_before_revision,
            "second post-boundary",
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
        wait_for_health(&client, &second_device, &second_revision, &second_item).await;

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
        assert!(
            restarted_displays.assignments.iter().any(|row| {
                row.assignment == second_assignment_id
                    && row.program == second_receiver_program
                    && row.revoked_at_unix_ms.is_none()
                    && row
                        .sync
                        .as_ref()
                        .is_some_and(|sync| sync.group == sync_group)
            }),
            "restarted daemon lost the second synchronized Signage assignment"
        );
        wait_for_health(&restarted, &device, &revision, &item).await;
        wait_for_health(&restarted, &second_device, &second_revision, &second_item).await;
        restarted
            .display_device_revoke(device.clone())
            .await
            .expect("revoke the recovered receiver");
        restarted
            .display_device_revoke(second_device)
            .await
            .expect("revoke the recovered synchronized receiver");
        wait_for_receiver_exit(&mut receiver).await;
        wait_for_receiver_exit(&mut second_receiver).await;
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

// ---------------------------------------------------------------------------
// The staged-swap chain (CLIENT-65).
//
// The seam: a signed feed names a tree artifact, `lait::update::tree` stages
// it beside the live tree, and the `astrolabe-stub` launcher — a real
// process, spawned here — proves it and swaps it in by rename before
// starting the client. Every part is unit-tested where it lives; this is the
// composition, which is the thing this file exists to assert. The "client"
// the trees carry is `chain-probe`, a reference binary that announces the
// version of the tree it actually ran from, so the assertions below are
// about *which* tree launched, never merely that something did.

/// The stub binary beside the test binary, or a panic — like `sidecar()`,
/// and unlike the reference receiver: the stub is the thing under test, and
/// reporting `ok` without it would be a guard trusted while guarding
/// nothing.
fn stub_binary() -> PathBuf {
    built_binary("astrolabe-stub").unwrap_or_else(|| {
        panic!(
            "no astrolabe-stub binary beside the test binary, so the staged-swap seam was not \
             exercised; build the workspace bins (cargo build -p astrolabe-stub) first"
        )
    })
}

/// The reference entry binary the fabricated trees carry.
fn probe_binary() -> PathBuf {
    built_binary("chain-probe").unwrap_or_else(|| {
        panic!(
            "no chain-probe binary beside the test binary, so the staged-swap seam was not \
             exercised; build the workspace bins (cargo build -p astrolabe-stub) first"
        )
    })
}

/// The tree's entry name on this platform — the same convention
/// `lait::update::tree` records and the stub launches.
fn tree_entry_name() -> &'static str {
    if cfg!(windows) {
        "astrolabe.exe"
    } else {
        "astrolabe"
    }
}

/// The sidecar name the pair contract requires beside the entry.
fn tree_sidecar_name() -> &'static str {
    if cfg!(windows) {
        "lait.exe"
    } else {
        "lait"
    }
}

/// A target triple whose platform half matches this host, so the staged
/// entry name and the launched entry name agree.
fn tree_target() -> &'static str {
    if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// Seal a payload into the feed's envelope: base64 payload bytes, detached
/// ed25519 over exactly those bytes. The shape `feed::open_envelope`
/// verifies; sealed here with `mechanics` directly because the feed's own
/// test sealer is crate-private — which is right, and this duplication is
/// itself covered by the resolve call below refusing anything misshapen.
fn seal_feed_object(payload: &serde_json::Value, seed: &[u8; 32]) -> Vec<u8> {
    let bytes = serde_json::to_vec(payload).expect("a payload encodes");
    let signature = mechanics::actor::sign_detached(seed, &bytes);
    serde_json::json!({
        "payload": data_encoding::BASE64.encode(&bytes),
        "signature": data_encoding::BASE64.encode(&signature),
    })
    .to_string()
    .into_bytes()
}

/// A tree artifact as the feed publishes them: gzip'd tar, one root
/// directory, entry executable at the root.
fn tree_artifact(version: &str, entry_bytes: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let root = format!("astrolabe-{version}");
        let mut file = |path: &str, contents: &[u8], mode: u32| {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("{root}/{path}"), contents)
                .expect("a tree entry appends");
        };
        file(tree_entry_name(), entry_bytes, 0o755);
        // The sidecar half of the pair. Not a real lait — the stager's
        // contract is that the tree carries one at its root, which is what
        // makes sidecar::beside and custody_of agree after a swap, and that
        // shape is what this fixture has to honour.
        file(tree_sidecar_name(), b"the sidecar half of the pair", 0o755);
        file("version.txt", version.as_bytes(), 0o644);
        file("data/asset.bin", b"an asset the tree carries", 0o644);
        builder
            .into_inner()
            .expect("the tar seals")
            .finish()
            .expect("the gzip seals");
    }
    bytes
}

/// Seal a whole feed — pointer and manifest — naming one tree artifact, and
/// resolve it with the matching key, exactly as an installed machine would.
fn resolve_sealed_tree_release(
    version: &str,
    archive: &[u8],
) -> (
    std::collections::HashMap<String, Vec<u8>>,
    lait::update::feed::Resolved,
) {
    let seed = [41u8; 32];
    let pubkey_hex = mechanics::actor::device_from_seed(&seed)
        .as_str()
        .to_string();
    let decoded = data_encoding::HEXLOWER
        .decode(pubkey_hex.as_bytes())
        .expect("a device id is lowercase hex of the public key");
    let pubkey: [u8; 32] = decoded.try_into().expect("a feed key is exactly 32 bytes");

    let url = format!("https://feed.example/releases/{version}/astrolabe-tree.tar.gz");
    let manifest = serde_json::json!({
        "version": version,
        "bundles": { lait::update::tree::TREE_BUNDLE: version },
        "artifacts": { lait::update::tree::TREE_BUNDLE: { tree_target(): {
            "url": url,
            "blake3": blake3_hex(archive),
            "size": archive.len(),
        }}},
    });
    let pointer = serde_json::json!({
        "kind": "release",
        "version": version,
        "manifest": "https://feed.example/releases/manifest.json",
    });

    let mut objects = std::collections::HashMap::new();
    objects.insert(
        "https://feed.example/channels/test".to_string(),
        seal_feed_object(&pointer, &seed),
    );
    objects.insert(
        "https://feed.example/releases/manifest.json".to_string(),
        seal_feed_object(&manifest, &seed),
    );
    objects.insert(url, archive.to_vec());

    let resolved = lait::update::feed::resolve_with(
        |asked| {
            objects.get(asked).cloned().ok_or_else(|| {
                lait::update::feed::Failure::Unreachable(format!("no object at {asked}"))
            })
        },
        lait::update::feed::Channel::Test,
        "https://feed.example",
        &[pubkey],
        None,
    )
    .expect("the sealed feed resolves against its own key");
    (objects, resolved)
}

/// blake3 as the feed manifests spell it: lowercase hex.
fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Run the stub as a real process against `root`, with the probe announcing
/// into `root`, and wait for it to exit.
///
/// The stub holds its claim for the lifetime of the client it starts, so
/// waiting on the stub waits on the whole tree of processes — which is also
/// what keeps the next phase from racing a still-exiting client over the
/// directory it is about to rename.
fn run_stub(root: &Path) {
    let status = Command::new(root.join(if cfg!(windows) {
        "astrolabe-stub.exe"
    } else {
        "astrolabe-stub"
    }))
    .env("CHAIN_PROBE_ANNOUNCE", root)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .expect("the stub spawns");
    assert!(
        status.success(),
        "the stub exited with a failure, and the stub must launch even when it refuses to apply"
    );
}

/// Wait for the probe's announcement and hand back the version it saw.
fn wait_for_announcement(root: &Path) -> String {
    let path = root.join("launched.txt");
    for _ in 0..150 {
        if let Ok(version) = std::fs::read_to_string(&path) {
            std::fs::remove_file(&path).expect("the announcement is consumed");
            return version;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("no tree announced itself within the budget");
}

/// The chain, end to end: signed pointer → manifest → verified tree
/// artifact → staged tree → a real stub process swaps by rename → the
/// launched entry announces the *new* tree — with the deferred, tampered,
/// and rollback arms asserted on the same install root.
#[test]
fn a_staged_release_is_applied_by_the_stub_and_the_previous_tree_survives() {
    let stub = stub_binary();
    let probe = probe_binary();
    let probe_bytes = std::fs::read(&probe).expect("the probe binary's bytes");

    let scratch = tempfile::tempdir().expect("an install root");
    let root = scratch.path();
    std::fs::copy(
        &stub,
        root.join(stub.file_name().expect("the stub has a name")),
    )
    .expect("the stub lands in the install root");

    // The live tree, version 0.0.1 — the install as the person has it.
    let current = root.join("current");
    std::fs::create_dir(&current).expect("the live tree");
    std::fs::copy(&probe, current.join(tree_entry_name())).expect("the live entry");
    std::fs::write(current.join("version.txt"), "0.0.1").expect("the live version");

    // Release 0.0.2, sealed into a feed and staged exactly as a daemon
    // would: resolve against the pinned key, verify, extract, record.
    let archive = tree_artifact("0.0.2", &probe_bytes);
    let (objects, resolved) = resolve_sealed_tree_release("0.0.2", &archive);
    let staged = lait::update::tree::stage_tree_with(
        |asked, _limit| {
            objects.get(asked).cloned().ok_or_else(|| {
                lait::update::feed::Failure::Unreachable(format!("no object at {asked}"))
            })
        },
        &resolved,
        tree_target(),
        root,
    )
    .expect("the 0.0.2 tree stages");
    assert_eq!(
        staged.version, "0.0.2",
        "the stage carries the release version"
    );

    // Recorded before anything moves, so the path-stability assertion after
    // the swap compares against what the person actually had.
    let entry_before_swap = current.join(tree_entry_name());

    // A live client holds the installation: the apply defers, is said, and
    // the person still gets their client — the old one.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("instance.lock"))
        .expect("the instance lock file");
    fs2::FileExt::try_lock_exclusive(&lock).expect("the test plays the live client");
    run_stub(root);
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.1",
        "an apply ran under a live client, or the wrong tree launched"
    );
    assert!(
        root.join("staged.manifest.json").is_file(),
        "a deferred stage was consumed"
    );
    fs2::FileExt::unlock(&lock).expect("the live client exits");

    // The lock is free: this launch applies, and the new tree is what runs.
    run_stub(root);
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.2",
        "the staged release did not become the running client"
    );
    assert_eq!(
        std::fs::read_to_string(current.join("version.txt")).expect("the live version"),
        "0.0.2",
        "the live tree is not the staged release"
    );
    assert!(
        !root.join("staged.manifest.json").exists(),
        "a consumed stage manifest was left behind"
    );
    // The pair, after the swap: astrolabe and lait as flat siblings, which
    // is what sidecar::beside and custody_of both mean by "beside". A swap
    // that delivered a tree without it would install a client that cannot
    // find its daemon.
    assert!(
        current.join(tree_sidecar_name()).is_file(),
        "the swapped-in tree does not carry its sidecar beside the entry"
    );
    // The entry's path is the same string it was before the update. This is
    // the half of the macOS identity rule a test can hold: TCC grants key on
    // signing identity, bundle id and *path*, so a layout that versioned the
    // live directory — Squirrel's `app-1.0.0/`, the obvious alternative to
    // this one — would silently drop every permission the person had granted
    // on the first update. The stable `current/` name is what prevents it,
    // and an assertion is what keeps it stable.
    assert_eq!(
        current.join(tree_entry_name()),
        entry_before_swap,
        "the update moved the client's path, which is how macOS loses TCC grants"
    );

    // The previous tree is kept, and kept *bootable*: it runs and announces
    // itself, which is what makes it a rollback target rather than a copy.
    let previous_entry = root.join("previous").join(tree_entry_name());
    let status = Command::new(&previous_entry)
        .env("CHAIN_PROBE_ANNOUNCE", root)
        .status()
        .expect("the previous tree's entry spawns");
    assert!(status.success(), "the previous tree is not bootable");
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.1",
        "the kept previous tree is not the prior release"
    );

    // A tampered stage: release 0.0.3 stages cleanly, then a byte changes on
    // disk. The stub must refuse by name and leave the live tree untouched.
    let archive = tree_artifact("0.0.3", &probe_bytes);
    let (objects, resolved) = resolve_sealed_tree_release("0.0.3", &archive);
    lait::update::tree::stage_tree_with(
        |asked, _limit| {
            objects.get(asked).cloned().ok_or_else(|| {
                lait::update::feed::Failure::Unreachable(format!("no object at {asked}"))
            })
        },
        &resolved,
        tree_target(),
        root,
    )
    .expect("the 0.0.3 tree stages");
    std::fs::write(root.join("staged").join("version.txt"), "0.0.3-tampered").expect("the tamper");
    run_stub(root);
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.2",
        "a tampered stage was applied, or the launch did not survive the refusal"
    );
    let log = std::fs::read_to_string(root.join("stub.log")).expect("the stub said its refusals");
    assert!(
        log.contains("verification failed"),
        "the tamper refusal must name verification, not fail vaguely: {log}"
    );
}
