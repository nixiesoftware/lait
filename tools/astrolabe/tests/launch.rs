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
use astrolabe::client::Client;
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
            Ok(daemon) => !matches!(daemon.probe().await, lait::control::Probe::Healthy),
            Err(_) => true,
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
        // Not a silent pass: the suite that runs this always builds `lait`
        // first, so an absent binary means somebody ran this alone.
        eprintln!("no lait binary beside the test binary; skipping the real-head launch test");
        return;
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
