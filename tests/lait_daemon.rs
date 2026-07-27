//! Process-topology contract for the identity-scoped Lait daemon.
//!
//! Two cwd-selected Orbits share one host PID. Their per-home sockets and lock
//! files remain compatibility adapters owned by that process, not evidence of
//! one daemon process per Space.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_lait")
}

fn temp_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("lait-host-topology-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn lait(config: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .current_dir(cwd)
        .env_remove("LAIT_HOME")
        .env_remove("LAIT_STORE")
        .env("LAIT_CONFIG_ROOT", config)
        .env("LAIT_NETWORK", "isolated")
        .env("LAIT_IDLE_SECS", "0")
        .args(args)
        .output()
        .expect("run lait")
}

fn init(config: &Path, project: &Path, name: &str) {
    std::fs::create_dir_all(project).unwrap();
    let output = lait(config, project, &["init", "--name", name, "--nick", "test"]);
    assert!(output.status.success(), "init failed: {output:?}");
}

fn pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .trim()
        .parse()
        .unwrap()
}

fn coordinates(output: std::process::Output) -> runtime::VerifiedCoordinates {
    assert!(output.status.success(), "invite failed: {output:?}");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("invite emits JSON");
    let ticket = value["text"].as_str().expect("invite text DTO");
    runtime::SignedCoordinates::parse_link(ticket)
        .expect("parse invite")
        .verify()
        .expect("verify invite")
}

#[test]
fn two_cwd_orbits_share_one_lait_daemon_process() {
    let root = temp_root();
    let config = root.join("config");
    let project_a = root.join("a");
    let project_b = root.join("b");
    init(&config, &project_a, "A");
    init(&config, &project_b, "B");

    let first = lait(&config, &project_a, &["new", "from a"]);
    assert!(first.status.success(), "first request failed: {first:?}");
    let second = lait(&config, &project_b, &["new", "from b"]);
    assert!(second.status.success(), "second request failed: {second:?}");

    let daemon_pid = pid(&config.join("daemon").join("daemon.pid"));
    let orbit_a_pid = pid(&project_a.join(".lait").join("daemon.pid"));
    let orbit_b_pid = pid(&project_b.join(".lait").join("daemon.pid"));
    assert_eq!(orbit_a_pid, daemon_pid);
    assert_eq!(orbit_b_pid, daemon_pid);

    let invite_a = coordinates(lait(&config, &project_a, &["--json", "invite"]));
    let invite_b = coordinates(lait(&config, &project_b, &["--json", "invite"]));
    assert_eq!(
        invite_a.approach_station, invite_b.approach_station,
        "both Spaces use the same device identity"
    );
    assert!(
        !invite_a.approach_routes.is_empty(),
        "isolated mode must advertise its direct endpoint"
    );
    assert_eq!(
        invite_a.approach_routes, invite_b.approach_routes,
        "one device identity advertises one shared transport endpoint"
    );

    let stopped = lait(&config, &project_a, &["shutdown"]);
    assert!(stopped.status.success(), "shutdown failed: {stopped:?}");

    let daemon_home = config.join("daemon");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !matches!(
        runtime.block_on(lait::control::probe(&daemon_home)),
        lait::control::Probe::Absent
    ) {
        assert!(Instant::now() < deadline, "Lait daemon did not stop");
        std::thread::sleep(Duration::from_millis(100));
    }
    for orbit in [project_a.join(".lait"), project_b.join(".lait")] {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !matches!(
            runtime.block_on(lait::control::probe(&orbit)),
            lait::control::Probe::Absent
        ) {
            assert!(
                Instant::now() < deadline,
                "compatibility endpoint for {} did not close",
                orbit.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    std::fs::remove_dir_all(root).ok();
}
