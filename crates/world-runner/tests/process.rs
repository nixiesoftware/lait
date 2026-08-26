use std::path::{Path, PathBuf};

use world_runner::{Instance, Operation, Provenance, Release, Reply, Stopped};

fn fixture_binary() -> PathBuf {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let test_binary = std::env::current_exe().expect("running process test binary");
    let profile_dir = test_binary
        .parent()
        .and_then(Path::parent)
        .expect("Cargo test binary under <target>/<profile>/deps");
    profile_dir.join(format!("world-fixture{suffix}"))
}

fn staged_release(version: &str, digest: [u8; 32]) -> (tempfile::TempDir, Release) {
    let source = fixture_binary();
    assert!(
        source.is_file(),
        "build the workspace binaries before this test; fixture absent at {}",
        source.display()
    );
    let root = tempfile::tempdir().expect("release root");
    let name = source.file_name().expect("fixture filename");
    std::fs::copy(&source, root.path().join(name)).expect("stage fixture program");
    let release = Release::under(
        root.path(),
        "com.lait.fixture",
        version,
        Provenance::Sealed(digest),
        Path::new(name),
        Vec::new(),
        None::<&Path>,
    )
    .expect("release");
    (root, release)
}

#[test]
fn a_world_is_pinned_until_an_explicit_relaunch() {
    let (_first_root, first) = staged_release("1.0.0", [0x11; 32]);
    let (_second_root, second) = staged_release("2.0.0", [0x22; 32]);
    let mut instance = Instance::launch(first).expect("launch first release");
    let first_pid = instance.pid();
    assert_eq!(instance.release().version, "1.0.0");
    assert_eq!(
        instance
            .request(Operation::Call {
                operation: "echo".to_string(),
                payload: b"from-one".to_vec(),
            })
            .expect("call first release"),
        Reply::Call {
            payload: b"from-one".to_vec()
        }
    );
    assert_eq!(
        instance
            .request_with(
                Operation::Call {
                    operation: "roundtrip".to_string(),
                    payload: b"from-host".to_vec(),
                },
                |operation, payload| {
                    assert_eq!(operation, "uppercase");
                    Ok(payload.iter().map(u8::to_ascii_uppercase).collect())
                },
            )
            .expect("the World calls back into its host"),
        Reply::Call {
            payload: b"FROM-HOST".to_vec()
        }
    );

    // Merely making another release available changes nothing about the live
    // generation. Relaunch is the transition and yields a new owned process.
    assert_eq!(
        instance.release().provenance,
        Provenance::Sealed([0x11; 32])
    );
    let (stopped, mut replacement) = instance.relaunch(second).expect("relaunch");
    assert_eq!(stopped, Stopped::Stopped);
    assert_ne!(replacement.pid(), first_pid);
    assert_eq!(replacement.release().version, "2.0.0");
    assert_eq!(
        replacement.release().provenance,
        Provenance::Sealed([0x22; 32])
    );
    replacement.ping().expect("replacement answers");
    assert_eq!(
        replacement.stop().expect("stop replacement"),
        Stopped::Stopped
    );
}

#[test]
fn a_world_callback_can_reenter_the_same_generation() {
    let (_root, release) = staged_release("1.0.0", [0x55; 32]);
    let mut instance = Instance::launch(release).expect("launch");
    let mut outer = instance.client().expect("prepare outer request");
    let mut nested = instance.client().expect("prepare nested request");

    let reply = outer
        .request_with(
            Operation::Call {
                operation: "roundtrip".to_string(),
                payload: b"reentrant".to_vec(),
            },
            |operation, payload| {
                assert_eq!(operation, "uppercase");
                assert_eq!(
                    nested
                        .request(Operation::Call {
                            operation: "echo".to_string(),
                            payload: b"nested".to_vec(),
                        })
                        .map_err(|error| error.to_string())?,
                    Reply::Call {
                        payload: b"nested".to_vec()
                    }
                );
                Ok(payload.iter().map(u8::to_ascii_uppercase).collect())
            },
        )
        .expect("callback re-enters the same World process");

    assert_eq!(
        reply,
        Reply::Call {
            payload: b"REENTRANT".to_vec()
        }
    );
    assert_eq!(instance.stop().expect("stop fixture"), Stopped::Stopped);
}

#[test]
fn dropping_an_owned_instance_collects_the_process() {
    let (_root, release) = staged_release("1.0.0", [0x33; 32]);
    let instance = Instance::launch(release).expect("launch");
    let pid = instance.pid();
    drop(instance);

    #[cfg(windows)]
    {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .expect("ask tasklist");
        let listing = String::from_utf8_lossy(&output.stdout);
        assert!(
            listing.contains("No tasks are running") || !listing.contains(&pid.to_string()),
            "dropped World process still exists: {listing}"
        );
    }

    #[cfg(unix)]
    {
        let status = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .expect("probe process");
        assert!(!status.success(), "dropped World process still exists");
    }
}

#[test]
fn a_crashed_child_is_restored_from_the_same_immutable_generation() {
    let (_root, release) = staged_release("1.0.0", [0x44; 32]);
    let mut instance = Instance::launch(release).expect("launch");
    let first_pid = instance.pid();

    #[cfg(windows)]
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &first_pid.to_string(), "/F", "/T"])
        .status()
        .expect("terminate fixture");
    #[cfg(unix)]
    let status = std::process::Command::new("kill")
        .args(["-KILL", &first_pid.to_string()])
        .status()
        .expect("terminate fixture");
    assert!(status.success(), "fixture process did not terminate");

    for _ in 0..100 {
        if instance.restart_if_gone().unwrap_or(false) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_ne!(instance.pid(), first_pid, "the dead process was retained");
    assert_eq!(instance.release().version, "1.0.0");
    assert_eq!(
        instance.release().provenance,
        Provenance::Sealed([0x44; 32])
    );
    instance.ping().expect("replacement answers");
}
