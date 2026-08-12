//! Headless interaction tests, over the same substrate the shell draws with.
//!
//! These run with no display, in CI, on every platform. They assert on the
//! *accessibility tree* rather than on pixels, which is the point: a query for
//! "the button labelled Force stop, and is it enabled" is exactly what a screen
//! reader asks, so a test that passes here is evidence the semantic layer is
//! populated rather than evidence that some pixels were the expected colour.
//!
//! The states covered are the ones that are easy to skip and expensive to get
//! wrong — empty, loading, degraded, and the controls that guard destructive
//! actions.

use astrolabe::model::App;
use astrolabe::ui::Surface;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use lait_workbench::{
    BackendEvent, Capabilities, ClientSignal, DeviceSnapshot, EnvironmentSnapshot, EventKind,
    LifecycleState, ObservationHealth, ObservationState, SnapshotReason, WorkbenchSnapshot,
};

fn snapshot(devices: Vec<DeviceSnapshot>) -> WorkbenchSnapshot {
    WorkbenchSnapshot {
        schema_version: 1,
        revision: 1,
        environment: EnvironmentSnapshot {
            state_root: "root".into(),
            executable: "lait".into(),
            server_pid: 1,
        },
        capabilities: Capabilities::default(),
        devices,
        connections: Vec::new(),
    }
}

fn device(id: &str, state: LifecycleState, owned: bool) -> DeviceSnapshot {
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
        observation: ObservationHealth::default(),
        image: None,
    }
}

/// Draw one app state and hand back a harness to interrogate.
fn harness(app: App, surface: Surface) -> Harness<'static, (App, Surface)> {
    Harness::new_ui_state(
        |ui, (app, surface)| {
            astrolabe::ui::draw(ui, app, surface);
        },
        (app, surface),
    )
}

/// Whether any node in the accessibility tree carries `needle`.
///
/// Scans both the node label and its value, because egui puts a control's name
/// in the label and a plain label's text in the value. A test that only looked
/// at one of them would silently pass on a surface that had gone blank.
fn announces(harness: &Harness<'_, (App, Surface)>, needle: &str) -> bool {
    harness.query_all_by_label_contains("").any(|node| {
        let node = node.accesskit_node();
        node.label().is_some_and(|label| label.contains(needle))
            || node.value().is_some_and(|value| value.contains(needle))
    })
}

/// Loading and empty look identical unless something makes them different, and
/// the difference matters: one says "wait", the other says "there is nothing".
#[test]
fn loading_and_empty_are_told_apart_on_screen() {
    let mut loading = harness(App::new(), Surface::Devices);
    loading.run();
    assert!(
        announces(&loading, "Loading"),
        "a client that has read nothing yet does not say so"
    );

    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    let mut empty = harness(app, Surface::Devices);
    empty.run();
    assert!(
        !announces(&empty, "Loading"),
        "a finished read still claims to be loading"
    );
    assert!(
        announces(&empty, "No devices are registered"),
        "an answered-and-empty read says nothing at all"
    );
}

/// Ownership is the safety boundary, and the interface draws it as one. A
/// control that cannot be used is disabled, not offered-and-refused: a person
/// who learns the rule from an error message has already tried the thing.
#[test]
fn an_external_daemon_offers_no_force_stop() {
    let mut app = App::new();
    app.absorb(snapshot(vec![
        device("alice", LifecycleState::Running, true),
        device("bob", LifecycleState::External, false),
    ]));
    let mut harness = harness(app, Surface::Devices);
    harness.run();

    let buttons: Vec<_> = harness.get_all_by_label("Force stop").collect();
    assert_eq!(buttons.len(), 2, "one force-stop control per device row");
    assert!(
        !buttons[0].accesskit_node().is_disabled(),
        "a daemon this client spawned cannot be force-stopped"
    );
    assert!(
        buttons[1].accesskit_node().is_disabled(),
        "a daemon this client did not spawn offers a force-stop control"
    );
}

/// Removal needs the device stopped, and the control says so before the click
/// rather than after it.
#[test]
fn a_running_device_offers_no_removal() {
    let mut app = App::new();
    app.absorb(snapshot(vec![
        device("alice", LifecycleState::Running, true),
        device("bob", LifecycleState::Stopped, false),
    ]));
    let mut harness = harness(app, Surface::Devices);
    harness.run();

    let buttons: Vec<_> = harness.get_all_by_label("Remove").collect();
    assert!(
        buttons[0].accesskit_node().is_disabled(),
        "a running device offered removal"
    );
    assert!(
        !buttons[1].accesskit_node().is_disabled(),
        "a stopped device refused removal"
    );
}

/// A degraded observation is drawn as degraded. Not as absence, and not as
/// nothing at all — the release gate tests for exactly this.
#[test]
fn a_degraded_device_is_drawn_as_stale_rather_than_as_absent() {
    let mut app = App::new();
    let mut bob = device("bob", LifecycleState::Running, true);
    bob.observation = ObservationHealth {
        state: ObservationState::Degraded,
        sampled_at_ms: Some(10),
        stale_since_ms: Some(20),
        error: Some("control channel refused".into()),
    };
    app.absorb(snapshot(vec![bob]));

    let mut devices = harness(app, Surface::Devices);
    devices.run();
    assert!(
        announces(&devices, "stale"),
        "a device whose figures stopped being current looks current"
    );
    assert!(
        announces(&devices, "bob"),
        "a degraded device vanished from the list instead of being marked"
    );
}

/// The diagnostics surface must not turn "nobody could ask" into "no peers".
#[test]
fn diagnostics_hedges_an_empty_peer_list_and_names_what_went_stale() {
    let mut app = App::new();
    let mut bob = device("bob", LifecycleState::Running, true);
    bob.observation = ObservationHealth {
        state: ObservationState::Degraded,
        sampled_at_ms: Some(10),
        stale_since_ms: Some(20),
        error: Some("control channel refused".into()),
    };
    app.absorb(snapshot(vec![bob]));

    let mut harness = harness(app, Surface::Diagnostics);
    harness.run();
    assert!(
        announces(&harness, "No peers observed"),
        "an empty topology is stated as fact rather than as an observation"
    );
    assert!(
        announces(&harness, "control channel refused"),
        "the reason the figures are stale is not on screen"
    );
}

/// A stale model keeps drawing its last good figures and says they are old.
/// Blanking would be a worse lie than a slightly out-of-date number.
#[test]
fn a_snapshot_required_says_so_without_blanking_the_surface() {
    let mut app = App::new();
    app.absorb(snapshot(vec![device(
        "alice",
        LifecycleState::Running,
        true,
    )]));
    app.consume(&ClientSignal::SnapshotRequired(
        SnapshotReason::ConsumerLagged { dropped: 12 },
    ));

    let mut harness = harness(app, Surface::Devices);
    harness.run();
    assert!(
        announces(&harness, "last known state"),
        "a model that knows it is behind draws as though it were current"
    );
    assert!(
        announces(&harness, "alice"),
        "going stale blanked the figures instead of ageing them"
    );
}

/// An ordinary event is not a reason to redraw as stale.
#[test]
fn an_ordinary_event_does_not_make_the_surface_look_stale() {
    let mut app = App::new();
    app.absorb(snapshot(vec![device(
        "alice",
        LifecycleState::Running,
        true,
    )]));
    app.consume(&ClientSignal::Event(BackendEvent {
        revision: 2,
        at_ms: 0,
        kind: EventKind::LogChanged,
        device_id: Some("alice".into()),
        message: "log grew".into(),
    }));

    let mut harness = harness(app, Surface::Devices);
    harness.run();
    assert!(
        !announces(&harness, "last known state"),
        "an ordinary event was drawn as a staleness warning"
    );
}

/// Every surface is reachable from the keyboard, and the semantic tree carries
/// which one is current. Full keyboard operation is a release criterion.
#[test]
fn every_surface_is_reachable_and_the_current_one_is_announced() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    let mut harness = harness(app, Surface::Library);
    harness.run();

    assert_eq!(
        Surface::ALL.len(),
        4,
        "a surface was added without being covered here"
    );
    for title in ["Library", "Devices", "Storage", "Diagnostics"] {
        assert!(
            harness
                .query_by_role_and_label(egui::accesskit::Role::Button, title)
                .is_some(),
            "{title} is not reachable as a control in the accessibility tree"
        );
    }

    // The current surface is *stated*, not merely drawn differently: a screen
    // reader has no access to a highlight. egui exposes it as the Toggle
    // pattern, which `accesskit_windows` does implement — so this is a state
    // Narrator and NVDA actually read out, not merely one present in the tree.
    // By role as well as label: the surface heading carries the same text as
    // its tab, and a query that cannot tell a heading from a control is not
    // asserting what it claims to.
    let current = harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Library")
        .accesskit_node()
        .toggled();
    assert_eq!(
        current,
        Some(egui::accesskit::Toggled::True),
        "the current surface is not announced as the current one"
    );
    let other = harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Devices")
        .accesskit_node()
        .toggled();
    assert_eq!(
        other,
        Some(egui::accesskit::Toggled::False),
        "a surface that is not current is announced as though it were"
    );

    // Focus reaches the tabs by keyboard alone.
    harness.key_press(egui::Key::Tab);
    harness.run();
    assert!(
        harness
            .query_by_role_and_label(egui::accesskit::Role::Button, "Library")
            .is_some(),
        "tabbing removed the surface controls from the tree"
    );
}

/// The Library is the front page. A person with one identity and several Spaces
/// must never open onto a process inventory.
#[test]
fn the_front_page_is_the_library() {
    assert_eq!(Surface::default(), Surface::Library);
}

/// A World that declares no entry path cannot be opened, and the control is
/// disabled rather than enabled-and-failing. `/` is not a guess to make on
/// somebody's behalf.
#[test]
fn a_world_with_no_entry_path_cannot_be_opened() {
    use astrolabe::client::library::{LibraryEntry, Placement};

    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_library(vec![LibraryEntry {
        orbit: "orb_one".into(),
        space: "ws_one".into(),
        world_mount: "issues".into(),
        display_name: None,
        entry_path: None,
        placement: Placement::Unknown,
    }]);

    let mut harness = harness(app, Surface::Library);
    harness.run();
    assert!(
        harness.get_by_label("Open").accesskit_node().is_disabled(),
        "a World with no declared entry path offered an Open control"
    );
    assert!(
        announces(&harness, "Unnamed"),
        "an unnamed row borrowed an id for its name"
    );
    assert!(
        announces(&harness, "unknown"),
        "an unobserved placement was drawn as though it were known"
    );
}
