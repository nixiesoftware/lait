//! Process-topology contract for the identity-scoped Lait daemon.
//!
//! Two Orbits share one host PID. Their per-store sockets and lock files remain
//! compatibility adapters owned by that process, not evidence of one daemon
//! process per Space.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::head::{temp_root, Head};

fn pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .trim()
        .parse()
        .unwrap()
}

fn coordinates(reply: &serde_json::Value) -> runtime::coordinates::VerifiedCoordinates {
    let ticket = reply["reff"]
        .as_str()
        .unwrap_or_else(|| panic!("invite returns a ref DTO: {reply}"));
    runtime::coordinates::SignedCoordinates::parse_link(ticket)
        .expect("parse invite")
        .verify()
        .expect("verify invite")
}

#[test]
fn two_orbits_share_one_lait_daemon_process() {
    let root = temp_root("topology");
    let config = root.join("config");
    let project_a = root.join("a");
    let project_b = root.join("b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();

    // One head, one daemon under it, two Spaces founded through the host plane.
    let head = Head::start(&config, None);
    let orbit_a = head.found(&project_a, "A");
    let orbit_b = head.found(&project_b, "B");

    // Address each Orbit so both are really placed, not merely registered.
    for orbit in [&orbit_a, &orbit_b] {
        let (status, info) = head.space(orbit, serde_json::json!({ "cmd": "status" }));
        assert_eq!(status, 200, "status for {orbit}: {info}");
    }

    let daemon_pid = pid(&config.join("daemon").join("daemon.pid"));
    let orbit_a_pid = pid(&project_a.join(".lait").join("daemon.pid"));
    let orbit_b_pid = pid(&project_b.join(".lait").join("daemon.pid"));
    assert_eq!(orbit_a_pid, daemon_pid);
    assert_eq!(orbit_b_pid, daemon_pid);

    let (status, invite_a) = head.space(&orbit_a, serde_json::json!({ "cmd": "invite" }));
    assert_eq!(status, 200, "invite A: {invite_a}");
    let (status, invite_b) = head.space(&orbit_b, serde_json::json!({ "cmd": "invite" }));
    assert_eq!(status, 200, "invite B: {invite_b}");
    let invite_a = coordinates(&invite_a);
    let invite_b = coordinates(&invite_b);
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

    // Stopping the one daemon closes both compatibility endpoints, which is the
    // same claim from the other side: they are its adapters, not its peers.
    head.stop();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for endpoint in [
        config.join("daemon"),
        project_a.join(".lait"),
        project_b.join(".lait"),
    ] {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !matches!(
            runtime.block_on(lait::control::probe(&endpoint)),
            lait::control::Probe::Absent
        ) {
            assert!(
                Instant::now() < deadline,
                "endpoint for {} did not close",
                endpoint.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    std::fs::remove_dir_all(root).ok();
}
