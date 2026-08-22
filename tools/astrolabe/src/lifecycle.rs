//! Closing, minimising, and what exit actually means.
//!
//! The daemon is an always-running local service that outlives every window.
//! Closing the client does not stop a peer, and that is not a nicety — a Space
//! stops converging the moment its device goes, and a person who closed a window
//! did not ask for that.
//!
//! So closing minimises to the tray, and *explicit* exit asks which of two
//! different things it means. Under both, daemons this client did not start are
//! left running: ownership is the safety boundary on the way out as much as on
//! the way in.

use lait_workbench::Supervisor;

/// What closing the window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnClose {
    /// Keep serving; the window goes to the tray. The default, and the only one
    /// that does not surprise somebody who clicked the wrong X.
    #[default]
    MinimiseToTray,
    /// The person chose an explicit exit from the menu.
    Exit(ExitRequest),
}

/// The two things "exit" can mean, asked rather than assumed.
///
/// A single Quit that stopped the daemon would take a person's Spaces offline
/// because they were done looking at a window. One that left it running would
/// leave a process they thought they had closed. Neither is a safe default, so
/// the question is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitRequest {
    /// Stop this device's daemon. Its Spaces stop converging until it returns.
    GoOffline,
    /// Close the client and leave the device serving.
    StayOnline,
}

/// What an exit did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExitReport {
    /// Devices this client owned and stopped.
    pub stopped: Vec<String>,
    /// Devices left running, and why. Populated under `StayOnline` for owned
    /// devices, and under *both* choices for external ones.
    pub left_running: Vec<(String, LeftRunning)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftRunning {
    /// The person chose to stay online.
    ByChoice,
    /// This client did not start it, so it is not this client's to stop.
    NotOurs,
}

/// Carry out an exit.
///
/// Returns what happened rather than nothing, because "close and exit" is the
/// last thing a person sees and a client that silently left three daemons
/// running has told them something false by omission.
pub async fn exit(supervisor: &Supervisor, request: ExitRequest) -> ExitReport {
    // Read ownership *before* anything stops: a device that has just been
    // stopped is no longer owned, and classifying afterwards would report every
    // stop as "not ours".
    let snapshot = supervisor.snapshot().await;
    let report = classify(&snapshot.devices, request);

    match request {
        ExitRequest::GoOffline => {
            // `shutdown` detaches the client first and then stops only what is
            // owned, which is exactly this policy — so it is called rather
            // than reimplemented here where the two could drift apart.
            supervisor.shutdown().await;
        }
        ExitRequest::StayOnline => {
            // A browser head is a child of this client, not an always-running
            // service. Leaving the daemons online must not orphan those heads
            // (or the observer task) when the host process exits.
            supervisor.detach().await;
        }
    }

    report
}

/// What an exit *would* do, given what is running.
///
/// Separated from the effect so the policy can be tested against every device
/// shape without a supervisor, a daemon, or a process to stop. The rule this
/// encodes is small and the cost of getting it wrong is somebody's Spaces going
/// offline, which is exactly the ratio that deserves a pure function.
pub fn classify(devices: &[lait_workbench::DeviceSnapshot], request: ExitRequest) -> ExitReport {
    let mut report = ExitReport::default();
    for device in devices {
        if !device.owned {
            // External under either choice. A daemon this client did not start
            // may be serving somebody else's work, and closing a window is not
            // a mandate over it.
            if is_up(device.state) {
                report
                    .left_running
                    .push((device.id.clone(), LeftRunning::NotOurs));
            }
            continue;
        }
        match request {
            ExitRequest::GoOffline => report.stopped.push(device.id.clone()),
            ExitRequest::StayOnline => report
                .left_running
                .push((device.id.clone(), LeftRunning::ByChoice)),
        }
    }
    report
}

const fn is_up(state: lait_workbench::LifecycleState) -> bool {
    matches!(
        state,
        lait_workbench::LifecycleState::Running
            | lait_workbench::LifecycleState::External
            | lait_workbench::LifecycleState::Starting
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closing must not be a way to take somebody offline by accident.
    #[test]
    fn closing_the_window_keeps_the_device_serving() {
        assert_eq!(OnClose::default(), OnClose::MinimiseToTray);
    }

    #[test]
    fn the_two_exits_are_distinct_choices() {
        assert_ne!(ExitRequest::GoOffline, ExitRequest::StayOnline);
    }

    fn device(id: &str, owned: bool, state: lait_workbench::LifecycleState) -> DeviceSnapshot {
        DeviceSnapshot {
            id: id.into(),
            label: id.into(),
            home: "home".into(),
            log_path: "log".into(),
            state,
            pid: owned.then_some(1),
            owned,
            started_at_ms: None,
            last_error: None,
            facts: None,
            observation: lait_workbench::ObservationHealth::default(),
            image: None,
        }
    }

    use lait_workbench::{DeviceSnapshot, LifecycleState};

    /// The rule that holds under *both* choices, and the one it would be
    /// easiest to lose: a daemon this client did not start is never stopped by
    /// this client exiting.
    #[test]
    fn an_external_daemon_survives_either_exit() {
        let devices = vec![
            device("alice", true, LifecycleState::Running),
            device("bob", false, LifecycleState::External),
        ];

        for request in [ExitRequest::GoOffline, ExitRequest::StayOnline] {
            let report = classify(&devices, request);
            assert!(
                !report.stopped.iter().any(|id| id == "bob"),
                "{request:?} stopped a daemon this client did not start"
            );
            assert!(
                report
                    .left_running
                    .iter()
                    .any(|(id, why)| id == "bob" && *why == LeftRunning::NotOurs),
                "{request:?} did not report the external daemon as left running"
            );
        }
    }

    #[test]
    fn going_offline_stops_what_this_client_owns() {
        let report = classify(
            &[device("alice", true, LifecycleState::Running)],
            ExitRequest::GoOffline,
        );
        assert_eq!(report.stopped, vec!["alice".to_owned()]);
        assert!(report.left_running.is_empty());
    }

    #[test]
    fn staying_online_stops_nothing_and_says_so() {
        let report = classify(
            &[device("alice", true, LifecycleState::Running)],
            ExitRequest::StayOnline,
        );
        assert!(report.stopped.is_empty(), "staying online stopped a daemon");
        assert_eq!(
            report.left_running,
            vec![("alice".to_owned(), LeftRunning::ByChoice)]
        );
    }

    /// A device that is already down is not reported as left running — there is
    /// nothing running to leave, and listing it would make an honest report
    /// read like a warning.
    #[test]
    fn a_stopped_external_device_is_not_reported_as_left_running() {
        let report = classify(
            &[device("bob", false, LifecycleState::Stopped)],
            ExitRequest::GoOffline,
        );
        assert!(report.left_running.is_empty());
        assert!(report.stopped.is_empty());
    }
}
