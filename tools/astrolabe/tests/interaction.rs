#![cfg(feature = "egui-ui")]

//! Headless interaction tests, over the same substrate the shell draws with.
//!
//! These run with no display, in CI, on every platform. They assert on the
//! *accessibility tree* rather than on pixels, which is the point: a query for
//! "the button labelled Force stop, and is it enabled" is exactly what a screen
//! reader asks, so a test that passes here is evidence the semantic layer is
//! populated rather than evidence that some pixels were the expected colour.
//!
//! Because drawing returns [`Action`]s rather than calling anything, a click is
//! also assertable: press the control, and read what the surface asked for.
//! That is what makes "Open reaches the World's head" testable with no daemon,
//! no browser and no window.
//!
//! The states covered are the ones that are easy to skip and expensive to get
//! wrong — empty, loading, degraded, and the controls that guard destructive
//! actions.
//!
//! Behind the `egui-ui` feature that carries the interface these test, so a
//! default build — the one the bridge and the Flutter client use — compiles
//! with no egui in the graph at all. CI runs `--all-features`, so nothing is
//! lost while the egui surfaces still carry flows the Dart pages have not
//! taken over.

use astrolabe::client::heads::{McpBinding, McpBindingOutcome};
use astrolabe::client::host::{HostContext, OrbitEntry};
use astrolabe::client::library::{LibraryEntry, Opens, Placement};
use astrolabe::client::space::{DeviceKey, SpaceOp, SpaceRef, SpaceView};
use astrolabe::model::App;
use astrolabe::runtime::{Action, Outcome, Read, Update};
use astrolabe::ui::caption::Ask;
use astrolabe::ui::{Chrome, Surface};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use lait::diagnose::DiagnosisView;
use lait::dto::{MemberDto, WhoamiDto};
use lait_workbench::{
    BackendEvent, Capabilities, ClientSignal, ConnectionSnapshot, DeviceSnapshot,
    EnvironmentSnapshot, EventKind, HeadFacts, HeadKind, LifecycleState, LogEntry, LogLevel,
    LogPage, ObservationHealth, ObservationState, Ownership, SnapshotReason, WorkbenchSnapshot,
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
        home: format!("root/{id}"),
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

/// The model, the interface's own state, and everything the interface has asked
/// for since the harness was built.
///
/// Actions accumulate rather than being replaced, because `Harness::run` draws
/// until the frame settles: a click lands on one frame and the frames after it
/// ask for nothing, so keeping only the last would keep the empty one.
///
/// Everything asked for is also recorded as in flight, exactly as the shell
/// does it. A bench where a control stayed live after its own click would let a
/// repeated frame produce a second action the real client never would — which
/// is the difference between testing the interface and testing the harness.
struct Bench {
    app: App,
    chrome: Chrome,
    actions: Vec<Action>,
    /// What the window was asked to do, for the same reason and in the same
    /// way. The interface clears these every frame — the shell carries them out
    /// on the frame they were asked for — so a bench that read them afterwards
    /// would always read an empty list.
    window: Vec<Ask>,
}

/// Deliberately taller than any surface draws, so a control near the bottom is
/// still inside the clip rect. egui culls interaction outside it, and a click on
/// a widget that fell off a too-small virtual screen registers as nothing at all
/// — which reads exactly like the control being broken.
fn harness(app: App, surface: Surface) -> Harness<'static, Bench> {
    Harness::builder()
        .with_size([1_200.0, 2_000.0])
        .build_ui_state(
            |ui, bench: &mut Bench| {
                let Bench {
                    app,
                    chrome,
                    actions,
                    window,
                } = bench;
                for action in astrolabe::ui::draw(ui, app, chrome) {
                    app.dispatched(&action);
                    actions.push(action);
                }
                window.extend(chrome.window.iter().copied());
            },
            Bench {
                app,
                chrome: Chrome::showing(surface),
                actions: Vec::new(),
                window: Vec::new(),
            },
        )
}

/// Whether any node in the accessibility tree carries `needle`.
///
/// Scans both the node label and its value, because egui puts a control's name
/// in the label and a plain label's text in the value. A test that only looked
/// at one of them would silently pass on a surface that had gone blank.
fn announces(harness: &Harness<'_, Bench>, needle: &str) -> bool {
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

/// Deleting a device's data is confirmed by naming it, and the control stays
/// disabled until the name matches. The supervisor refuses an unconfirmed
/// deletion anyway; asking here is what stops the refusal from being the first
/// time somebody hears about it.
#[test]
fn deleting_a_devices_data_takes_its_name_before_it_takes_anything_else() {
    let mut app = App::new();
    app.absorb(snapshot(vec![device(
        "alice",
        LifecycleState::Stopped,
        true,
    )]));
    let mut harness = harness(app, Surface::Devices);
    harness.run();

    harness.get_by_label("Delete data…").click();
    harness.run();
    assert!(
        announces(&harness, "destroys everything under"),
        "the deletion step does not say what it would destroy"
    );
    assert!(
        harness
            .get_by_label("Delete permanently")
            .accesskit_node()
            .is_disabled(),
        "deletion was offered before it was confirmed"
    );

    // Typed through the model's own draft, which is what the text box writes to.
    harness.state_mut().chrome.devices.confirmation = "alice".into();
    harness.run();
    assert!(
        !harness
            .get_by_label("Delete permanently")
            .accesskit_node()
            .is_disabled(),
        "a correctly named device still refused deletion"
    );

    harness.get_by_label("Delete permanently").click();
    harness.run();
    assert!(
        harness.state().actions.contains(&Action::RemoveDevice {
            id: "alice".into(),
            delete_data: true,
        }),
        "confirming deletion asked for something other than a deletion: {:?}",
        harness.state().actions
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
        7,
        "a surface was added without being covered here"
    );
    for title in [
        "Library",
        "Spaces",
        "Members",
        "Devices",
        "Heads",
        "Storage",
        "Diagnostics",
    ] {
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

/// Changing surface takes one keystroke, not a walk through the tab order.
/// Somebody navigating by keyboard should reach any surface as directly as
/// somebody with a mouse reaches any tab.
#[test]
fn a_surface_is_one_keystroke_away_from_any_other() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    let mut harness = harness(app, Surface::Library);
    harness.run();

    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Num4);
    harness.run();
    assert_eq!(
        harness.state().chrome.surface,
        Surface::Devices,
        "Ctrl+4 did not reach the fourth surface"
    );

    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Num1);
    harness.run();
    assert_eq!(harness.state().chrome.surface, Surface::Library);

    // And the one action worth a key of its own.
    harness.key_press(egui::Key::F5);
    harness.run();
    assert!(
        harness.state().actions.contains(&Action::Refresh),
        "F5 asked for nothing"
    );
}

/// The window has no system title bar, so this client's own three controls are
/// the only way to minimise or close it with a mouse. They are asserted the same
/// way every other control is — press it, read what was asked for — which is
/// what makes a window control testable with no window.
#[test]
fn the_window_is_minimised_and_closed_by_the_clients_own_controls() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    let mut harness = harness(app, Surface::Library);
    harness.run();

    for name in ["Minimise", "Maximise", "Close"] {
        assert!(
            harness
                .query_by_role_and_label(egui::accesskit::Role::Button, name)
                .is_some(),
            "{name} is not a control in the accessibility tree, so a screen \
             reader cannot reach it and the window cannot be {name}d"
        );
    }

    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Minimise")
        .click();
    harness.run();
    assert!(
        harness.state().window.contains(&Ask::Minimise),
        "the minimise control asked for nothing"
    );

    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Close")
        .click();
    harness.run();
    assert!(
        harness.state().window.contains(&Ask::Close),
        "the close control asked for nothing"
    );
}

/// The maximise control says which way it goes, and goes that way.
///
/// It reads the window rather than remembering what it last did, because the
/// window can be maximised by routes this client never sees — `Win`+`↑`, a snap
/// gesture, a double-click on the bar. A remembered flag draws the wrong mark
/// the first time one of those happens and then does the wrong thing once.
#[test]
fn the_maximise_control_reads_the_window_rather_than_remembering() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    let mut harness = harness(app, Surface::Library);

    let maximised = |harness: &mut Harness<'_, Bench>, state: bool| {
        harness
            .input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .maximized = Some(state);
        harness.run();
    };

    maximised(&mut harness, true);
    assert!(
        harness
            .query_by_role_and_label(egui::accesskit::Role::Button, "Restore")
            .is_some(),
        "a maximised window still offers to be maximised"
    );
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Restore")
        .click();
    harness.run();
    assert!(
        harness.state().window.contains(&Ask::Maximise(false)),
        "restoring a maximised window asked for {:?}",
        harness.state().window
    );

    harness.state_mut().window.clear();
    maximised(&mut harness, false);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Maximise")
        .click();
    harness.run();
    assert!(
        harness.state().window.contains(&Ask::Maximise(true)),
        "maximising a restored window asked for {:?}",
        harness.state().window
    );
}

/// The bar is the title bar: dragging it moves the window, and the pills on it
/// still take their own clicks.
///
/// Those are one assertion rather than two. The bar's sense covers every control
/// in it, and egui gives a point to whichever widget claimed it last — so
/// claiming the bar in the wrong order produces a header where the tabs
/// highlight, nothing navigates, and the cause is invisible.
#[test]
fn the_bar_moves_the_window_and_the_tabs_on_it_still_navigate() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    let mut harness = harness(app, Surface::Library);
    harness.run();

    // Inside the bar and inside the page margin, so it is bar and nothing else
    // whatever the pills measure. Picking a gap between two controls instead
    // would make this test a hostage to the width of the word "Diagnostics".
    let handle = egui::pos2(6.0, astrolabe::ui::geometry::bar::lg() / 2.0);
    harness.hover_at(handle);
    harness.run();
    harness.drag_at(handle);
    harness.run();
    // A move, not a press. Handing the platform its modal drag loop the instant
    // a button went down would eat the second half of every double-click.
    harness.hover_at(handle + egui::vec2(40.0, 0.0));
    harness.run();
    assert!(
        harness.state().window.contains(&Ask::Move),
        "dragging the bar asked for {:?}",
        harness.state().window
    );
    harness.drop_at(handle + egui::vec2(40.0, 0.0));
    harness.run();

    harness.state_mut().window.clear();
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Devices")
        .click();
    harness.run();
    assert_eq!(
        harness.state().chrome.surface,
        Surface::Devices,
        "a tab on the title bar stopped navigating, which is what happens when \
         the bar claims the point after the pills do"
    );
    assert!(
        harness.state().window.is_empty(),
        "changing surface also moved the window: {:?}",
        harness.state().window
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
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_library(vec![LibraryEntry {
        orbit: "orb_one".into(),
        space: "ws_one".into(),
        world_mount: "issues".into(),
        display_name: None,
        opens: Opens::Undeclared,
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
        announces(&harness, "could not ask"),
        "an unobserved placement was drawn as though it were known"
    );
}

/// The defect this page shipped with: on a freshly started daemon every row was
/// unopenable, and the disabled hover blamed a World for declaring no entry
/// path.
///
/// A vacant Orbit activates nothing, so it lists no Worlds at all — every row
/// is a *Space* row, and a Space row's destination was never a World's to
/// declare. It opens at its Orbit's own front door, which is exactly what
/// places it.
#[test]
fn a_space_that_is_not_running_is_still_openable() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_library(vec![LibraryEntry {
        orbit: "orb_one".into(),
        space: "ws_one".into(),
        world_mount: String::new(),
        display_name: Some("Work".into()),
        opens: Opens::Front,
        placement: Placement::Vacant,
    }]);

    let mut harness = harness(app, Surface::Library);
    harness.run();
    assert!(
        !harness.get_by_label("Open").accesskit_node().is_disabled(),
        "a Space that is not running refused to be opened, so nothing can ever \
         place it"
    );

    harness.get_by_label("Open").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        vec![Action::OpenWorld {
            orbit: "orb_one".into(),
            entry_path: "/".into(),
        }],
        "opening a Space asked for somewhere other than its head's root"
    );
}

/// The click that has to work. `Open` asks for the Orbit the pane is *about*
/// and the entry path that row's World declared — not a guess, and not the row
/// the page happened to open on.
///
/// This is the assertion the master–detail layout has to earn: one primary
/// control rather than one per row means the control has to follow the
/// selection, and a page where it silently did not would open the wrong World
/// while looking entirely correct.
#[test]
fn open_asks_to_launch_the_row_the_pane_is_about() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_library(vec![
        LibraryEntry {
            orbit: "orb_one".into(),
            space: "ws_one".into(),
            world_mount: "issues".into(),
            display_name: Some("First".into()),
            opens: Opens::Declared("/".into()),
            placement: Placement::Vacant,
        },
        LibraryEntry {
            orbit: "orb_two".into(),
            space: "ws_two".into(),
            world_mount: "issues".into(),
            display_name: Some("Second".into()),
            opens: Opens::Declared("/spaces/ws_two".into()),
            placement: Placement::Placed,
        },
    ]);

    let mut harness = harness(app, Surface::Library);
    harness.run();

    // Nothing has been chosen, so the pane is about the first row — and the one
    // Open on the page is enabled, because an Orbit that is not running is
    // still openable. Opening is what places it.
    assert!(
        !harness.get_by_label("Open").accesskit_node().is_disabled(),
        "an Orbit that is not running refused to be opened"
    );

    // Choosing a row in the rail is a selection and nothing else: it reads
    // nothing, places nothing, and asks for nothing.
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Second")
        .click();
    harness.run();
    assert!(
        harness.state().actions.is_empty(),
        "choosing a row in the rail did something: {:?}",
        harness.state().actions
    );

    harness.get_by_label("Open").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        vec![Action::OpenWorld {
            orbit: "orb_two".into(),
            entry_path: "/spaces/ws_two".into(),
        }],
        "Open followed the page rather than the selection"
    );
}

/// A control does not stay live during its own action. Four clicks on a Stop
/// button would be one stop and three refusals, and the third refusal is the
/// one a person sees.
#[test]
fn a_control_is_disabled_while_its_own_action_is_in_flight() {
    let mut app = App::new();
    app.absorb(snapshot(vec![
        device("alice", LifecycleState::Running, true),
        device("bob", LifecycleState::Running, true),
    ]));
    app.dispatched(&Action::StopDevice("alice".into()));

    let mut harness = harness(app, Surface::Devices);
    harness.run();
    let stops: Vec<_> = harness.get_all_by_label("Stop").collect();
    assert!(
        stops[0].accesskit_node().is_disabled(),
        "a device whose stop is already under way still offers to stop it again"
    );
    assert!(
        !stops[1].accesskit_node().is_disabled(),
        "one device's action disabled another device's control"
    );
}

/// Founding a Space has to be possible with no World head reachable — which is
/// every fresh install. The form is on screen, and the button is refused until
/// it has what it needs rather than failing after the click.
#[test]
fn a_space_can_be_founded_from_the_client_with_nothing_running() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(HostContext {
        version: "lait 0.0.0".into(),
        identity_home: "home".into(),
        spaces_root: "D:/lait".into(),
        worlds: vec!["issues".into()],
        identities: Vec::new(),
        orbits: Vec::new(),
    });

    let mut harness = harness(app, Surface::Spaces);
    harness.run();
    assert!(
        harness.get_by_label("Found").accesskit_node().is_disabled(),
        "a Space with no name was offered a Found control"
    );
    // The store path is suggested from what the daemon said, so a person is not
    // asked to invent one. Read off the draft the box is bound to: a text
    // field's contents reach the semantic tree as a value on a node with no
    // label, which the announcement scan above cannot see.
    assert!(
        harness
            .state()
            .chrome
            .spaces
            .found_home
            .starts_with("D:/lait"),
        "no store directory was suggested: {:?}",
        harness.state().chrome.spaces.found_home
    );

    harness.state_mut().chrome.spaces.found_name = "Work".into();
    harness.run();
    harness.get_by_label("Found").click();
    harness.run();

    let asked = harness
        .state()
        .actions
        .iter()
        .find_map(|action| match action {
            Action::SpaceFound { home, name, .. } => Some((home.clone(), name.clone())),
            _ => None,
        })
        .expect("founding asked for nothing");
    assert_eq!(asked.1, "Work");
    assert!(
        asked.0.starts_with("D:/lait"),
        "the store went somewhere other than where the daemon said: {}",
        asked.0
    );
}

/// The other half of the acceptance shape: an invite in hand reaches a
/// converged Space without a browser.
#[test]
fn an_invite_is_entered_from_the_client() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(HostContext {
        version: "lait 0.0.0".into(),
        identity_home: "home".into(),
        spaces_root: "D:/lait".into(),
        worlds: Vec::new(),
        identities: Vec::new(),
        orbits: Vec::new(),
    });

    let mut harness = harness(app, Surface::Spaces);
    harness.run();
    assert!(
        harness.get_by_label("Enter").accesskit_node().is_disabled(),
        "entering was offered with no invite"
    );

    harness.state_mut().chrome.spaces.invite = "lait-invite-blob".into();
    harness.run();
    harness.get_by_label("Enter").click();
    harness.run();

    assert!(
        harness.state().actions.iter().any(|action| matches!(
            action,
            Action::SpaceEnter { link, .. } if link == "lait-invite-blob"
        )),
        "entering asked for something other than the invite that was typed"
    );
}

/// Signing this machine's consent is reachable with no membership anywhere,
/// which is the whole point of enrolment.
#[test]
fn this_machine_can_sign_its_consent_before_it_is_a_member_of_anything() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    let mut harness = harness(app, Surface::Spaces);
    harness.run();

    assert!(
        harness
            .get_by_label("Sign consent")
            .accesskit_node()
            .is_disabled(),
        "consent was offered with no invite token"
    );
    harness.state_mut().chrome.spaces.consent = "act_x ws_y".into();
    harness.run();
    harness.get_by_label("Sign consent").click();
    harness.run();

    assert!(
        harness.state().actions.iter().any(|action| matches!(
            action,
            Action::DeviceConsent { token } if token == "act_x ws_y"
        )),
        "signing consent asked for something else"
    );
}

/// A head this client did not start cannot be stopped through it. The same
/// boundary daemons have, drawn the same way.
#[test]
fn a_head_this_client_did_not_start_offers_no_stop() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_heads(vec![
        HeadFacts {
            id: "identity:home".into(),
            kind: HeadKind::Browser,
            device: None,
            orbit: None,
            identity: "home".into(),
            ownership: Ownership::Owned,
            pid: Some(1),
            url: Some("http://127.0.0.1:1/?token=secret".into()),
        },
        HeadFacts {
            id: "alice-browser".into(),
            kind: HeadKind::Browser,
            device: Some("alice".into()),
            orbit: None,
            identity: "root/alice".into(),
            ownership: Ownership::External,
            pid: None,
            url: None,
        },
    ]);

    let mut harness = harness(app, Surface::Heads);
    harness.run();
    let stops: Vec<_> = harness.get_all_by_label("Stop").collect();
    assert_eq!(stops.len(), 2, "one stop control per head");
    assert!(!stops[0].accesskit_node().is_disabled());
    assert!(
        stops[1].accesskit_node().is_disabled(),
        "a head this client did not start offered a stop control"
    );

    // The run credential is never drawn. A token on screen is a token in a
    // screenshot, in a support ticket, and in whatever recorded the window.
    assert!(
        !announces(&harness, "secret"),
        "a head's run credential was drawn on screen"
    );
}

/// An MCP binding is previewed before it is written, and the preview says it
/// is a preview. The file being edited is an agent's, not ours.
#[test]
fn an_mcp_binding_is_previewed_before_anything_is_written() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    let mut harness = harness(app, Surface::Heads);
    harness.run();

    assert!(
        harness
            .get_by_label("Preview")
            .accesskit_node()
            .is_disabled(),
        "a binding with no project directory was offered a preview"
    );
    harness.state_mut().chrome.heads.project = "D:/work".into();
    harness.run();
    harness.get_by_label("Preview").click();
    harness.run();

    let previewed = harness
        .state()
        .actions
        .iter()
        .find_map(|action| match action {
            Action::InstallMcp { binding, preview } => Some((binding.clone(), *preview)),
            _ => None,
        })
        .expect("preview asked for nothing");
    assert!(previewed.1, "Preview asked for a write");
    assert_eq!(previewed.0.project, "D:/work");

    // And what comes back is drawn as a preview rather than as a change.
    harness.state_mut().app.apply(Update::Done {
        key: "mcp.preview".into(),
        outcome: Outcome::Mcp(Box::new(McpBindingOutcome {
            path: "D:/work/.mcp.json".into(),
            detail: "{ \"mcpServers\": {} }".into(),
            note: Some("this entry shadows the bundled plugin".into()),
            replaced: false,
            agent: Some("claude".into()),
            written: false,
        })),
    });
    harness.run();
    assert!(
        announces(&harness, "Would be written to"),
        "a preview was drawn as though the file had changed"
    );
    assert!(
        announces(&harness, "shadows the bundled plugin"),
        "the client-specific caveat was dropped"
    );
}

/// The record of what happened is on screen. An action that worked and left no
/// trace is indistinguishable from one that was never dispatched.
#[test]
fn what_worked_is_drawn_and_not_only_what_failed() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.apply(Update::Done {
        key: "device.stop:alice".into(),
        outcome: Outcome::Said("alice stopped".into()),
    });

    let mut harness = harness(app, Surface::Devices);
    harness.run();
    assert!(
        announces(&harness, "alice stopped"),
        "an action that worked left no trace on screen"
    );
}

/// A binding is a value the surface composes from what was typed, and the type
/// is what keeps that honest. Kept here because it is the shape both the
/// preview and the write travel as.
#[test]
fn a_binding_carries_what_the_surface_was_told_and_nothing_else() {
    let binding = McpBinding {
        client: lait::install::Client::Cursor,
        scope: Some(lait::install::Scope::User),
        name: "lait-dev".into(),
        agent: Some("cursor".into()),
        no_agent: false,
        project: "D:/work".into(),
    };
    assert_eq!(binding.name, "lait-dev");
    assert_eq!(binding.client, lait::install::Client::Cursor);
}

/// A registered Orbit is listed with the name the registry holds, and that name
/// is drawn as advisory rather than as truth: a Space's display name is owned
/// by a World today, so the registry's copy may lag a rename (SUB-1).
#[test]
fn a_registered_orbit_is_listed_with_a_way_to_forget_it() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(HostContext {
        version: "lait 0.0.0".into(),
        identity_home: "home".into(),
        spaces_root: "D:/lait".into(),
        worlds: Vec::new(),
        identities: Vec::new(),
        orbits: vec![OrbitEntry {
            space: "ws_one".into(),
            name: "Work".into(),
            path: "D:/lait/work".into(),
            last_opened: 0,
        }],
    });

    let mut harness = harness(app, Surface::Spaces);
    harness.run();
    assert!(announces(&harness, "Work"));
    harness.get_by_label("Forget…").click();
    harness.run();
    assert!(
        announces(&harness, "Its store stays on disk"),
        "forgetting did not say that it leaves the data alone"
    );

    harness.get_by_label("Forget").click();
    harness.run();
    assert!(
        harness.state().actions.iter().any(|action| matches!(
            action,
            Action::OrbitForget { space } if space == "ws_one"
        )),
        "forgetting asked for something else"
    );
}

/// The environment page is real fields only. Nothing is inferred and nothing is
/// synthesised to make it look populated — and a capability this build does not
/// have is named, because it is the reason a control elsewhere is disabled.
#[test]
fn the_environment_page_draws_what_the_backend_answered_and_says_what_it_cannot_do() {
    let mut app = App::new();
    let mut reading = snapshot(vec![device("alice", LifecycleState::Running, true)]);
    reading.environment = EnvironmentSnapshot {
        state_root: "D:/state".into(),
        executable: "D:/bin/lait.exe".into(),
        server_pid: 4242,
    };
    reading.capabilities = Capabilities {
        force_stop_owned_process: false,
        ..Capabilities::default()
    };
    app.absorb(reading);

    let mut harness = harness(app, Surface::Diagnostics);
    harness.run();
    assert!(announces(&harness, "D:/bin/lait.exe"));
    assert!(announces(&harness, "D:/state"));
    assert!(announces(&harness, "4242"));
    assert!(
        announces(&harness, "force-stop"),
        "a capability this build does not have was not named"
    );
}

/// A peer that cannot be reached says why. "Blocked" with no reason sends
/// somebody to read a log to learn what was already known here.
#[test]
fn a_blocked_peer_names_what_is_blocking_it() {
    let mut app = App::new();
    let mut reading = snapshot(vec![device("alice", LifecycleState::Running, true)]);
    reading.connections = vec![ConnectionSnapshot {
        source_device_id: "alice".into(),
        space_id: "ws_one".into(),
        peer_id: "peer".into(),
        peer_nick: "bob".into(),
        state: "offering".into(),
        online: false,
        dialable: false,
        blocked_by: Some("admission has not landed".into()),
        target_device_id: None,
    }];
    app.absorb(reading);

    let mut harness = harness(app, Surface::Diagnostics);
    harness.run();
    assert!(announces(&harness, "bob"));
    assert!(
        announces(&harness, "admission has not landed"),
        "a blocked peer was drawn as blocked and not as why"
    );
    assert!(announces(&harness, "not dialable"));
}

/// Following a log asks once per reported change, not once per frame. The
/// alternative is a request storm against a supervisor in the same process,
/// which is exactly the mistake that being in the same process makes easy.
#[test]
fn following_a_log_asks_once_per_change_rather_than_once_per_frame() {
    let mut app = App::new();
    app.absorb(snapshot(vec![device(
        "alice",
        LifecycleState::Running,
        true,
    )]));

    let mut harness = harness(app, Surface::Diagnostics);
    harness.state_mut().chrome.diagnostics.device = Some("alice".into());
    harness.state_mut().chrome.diagnostics.following = true;
    harness.run();
    harness.run();
    harness.run();
    let asked = |bench: &Bench| {
        bench
            .actions
            .iter()
            .filter(|action| matches!(action, Action::ReadLogs { .. }))
            .count()
    };
    assert_eq!(
        asked(harness.state()),
        1,
        "following a log asked once per frame: {:?}",
        harness.state().actions
    );

    answer(&mut harness, 10);
    harness.run();
    harness.run();
    assert_eq!(
        asked(harness.state()),
        1,
        "an answered read was asked for again with nothing having changed"
    );

    // One more change, one more read.
    grew(&mut harness, "alice", 2);
    harness.run();
    harness.run();
    assert_eq!(
        asked(harness.state()),
        2,
        "a change to the log was not followed"
    );

    // And a change to somebody else's log is not this device's business.
    answer(&mut harness, 20);
    grew(&mut harness, "bob", 3);
    harness.run();
    assert_eq!(
        asked(harness.state()),
        2,
        "another device's log change triggered a read"
    );
}

/// Land a page for `alice`, which is what clears the read from flight.
fn answer(harness: &mut Harness<'_, Bench>, next_cursor: u64) {
    harness.state_mut().app.apply(Update::Done {
        key: "logs:alice".into(),
        outcome: Outcome::Read(Read::Logs(Box::new(LogPage {
            schema_version: 1,
            device_id: "alice".into(),
            file_size: next_cursor,
            next_cursor,
            reset: false,
            has_more: false,
            entries: Vec::new(),
        }))),
    });
}

fn grew(harness: &mut Harness<'_, Bench>, device: &str, revision: u64) {
    harness
        .state_mut()
        .app
        .consume(&ClientSignal::Event(BackendEvent {
            revision,
            at_ms: 0,
            kind: EventKind::LogChanged,
            device_id: Some(device.to_owned()),
            message: "log grew".into(),
        }));
}

/// Paused means paused. What is on screen stays exactly as it is, which is the
/// whole point of being able to stop a log that is moving.
#[test]
fn a_paused_log_is_not_re_read_when_it_changes() {
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

    let mut harness = harness(app, Surface::Diagnostics);
    harness.state_mut().chrome.diagnostics.device = Some("alice".into());
    harness.state_mut().chrome.diagnostics.following = false;
    harness.run();
    harness.run();
    assert!(
        !harness
            .state()
            .actions
            .iter()
            .any(|action| matches!(action, Action::ReadLogs { .. })),
        "a paused log was re-read anyway"
    );
}

/// A log page that begins a new file says so. Otherwise what was on screen a
/// moment ago silently stops being the start of what is on screen now.
#[test]
fn a_rotated_log_says_the_file_was_replaced() {
    let mut app = App::new();
    app.absorb(snapshot(vec![device(
        "alice",
        LifecycleState::Running,
        true,
    )]));
    app.apply(Update::Done {
        key: "logs:alice".into(),
        outcome: Outcome::Read(Read::Logs(Box::new(LogPage {
            schema_version: 1,
            device_id: "alice".into(),
            file_size: 10,
            next_cursor: 10,
            reset: true,
            has_more: false,
            entries: vec![LogEntry {
                cursor: 0,
                timestamp: Some("12:00:00".into()),
                level: LogLevel::Error,
                target: Some("lait".into()),
                message: "the store lock was held".into(),
                truncated: false,
            }],
        }))),
    });

    let mut harness = harness(app, Surface::Diagnostics);
    harness.state_mut().chrome.diagnostics.device = Some("alice".into());
    harness.run();
    assert!(
        announces(&harness, "log file was replaced"),
        "a rotated log was drawn as a continuation"
    );
    assert!(announces(&harness, "the store lock was held"));
}

/// Storage says which kind of absent a missing figure is. A Space that is not
/// running and one nobody could ask are different facts about the machine, and
/// only one of them is worth acting on.
#[test]
fn storage_says_whether_anybody_could_have_measured() {
    use astrolabe::client::storage::{Missing, StorageFacts};

    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_storage(
        vec![
            StorageFacts::unmeasured("orb_one", Missing::NotPlaced),
            StorageFacts::unmeasured("orb_two", Missing::Unreachable),
        ],
        Vec::new(),
    );

    let mut harness = harness(app, Surface::Storage);
    harness.run();
    assert!(announces(&harness, "not running"));
    assert!(announces(&harness, "could not be asked"));
    assert!(
        announces(&harness, "SUB-3"),
        "an empty transfer lane did not say why it is empty"
    );
}

/// Renaming is safe at any lifecycle state — a label names the device to a
/// person and nothing resolves by it — and it is still a deliberate step
/// rather than an always-live text box beside a Force stop button.
#[test]
fn a_device_is_renamed_through_a_step_of_its_own() {
    let mut app = App::new();
    app.absorb(snapshot(vec![device(
        "alice",
        LifecycleState::Running,
        true,
    )]));
    let mut harness = harness(app, Surface::Devices);
    harness.run();

    harness.get_by_label("Rename…").click();
    harness.run();
    assert!(
        harness
            .get_by_label("Rename")
            .accesskit_node()
            .is_disabled(),
        "renaming to the name it already has was offered"
    );

    harness.state_mut().chrome.devices.label = "the spare box".into();
    harness.run();
    harness.get_by_label("Rename").click();
    harness.run();
    assert!(
        harness.state().actions.contains(&Action::RenameDevice {
            id: "alice".into(),
            label: "the spare box".into(),
        }),
        "renaming asked for something else: {:?}",
        harness.state().actions
    );
}

/// Stopping everything this client owns is offered only when there is
/// something owned to stop, so it is never a control that does nothing.
#[test]
fn stopping_everything_owned_is_offered_only_when_there_is_something_to_stop() {
    let mut nothing_owned = App::new();
    nothing_owned.absorb(snapshot(vec![device(
        "bob",
        LifecycleState::External,
        false,
    )]));
    let mut external = harness(nothing_owned, Surface::Devices);
    external.run();
    assert!(
        external
            .get_by_label("Stop everything this client started")
            .accesskit_node()
            .is_disabled(),
        "a client that started nothing offered to stop everything it started"
    );

    let mut app = App::new();
    app.absorb(snapshot(vec![device(
        "alice",
        LifecycleState::Running,
        true,
    )]));
    let mut harness = harness(app, Surface::Devices);
    harness.run();
    harness
        .get_by_label("Stop everything this client started")
        .click();
    harness.run();
    assert!(harness.state().actions.contains(&Action::StopAllOwned));
}

fn standing(role: &str, me: bool) -> WhoamiDto {
    WhoamiDto {
        actor: Some("act_me".into()),
        device: "0".repeat(64),
        did: None,
        space: Some("ws_one".into()),
        role: role.into(),
        member: me,
        can_write: me,
        capabilities: Vec::new(),
        policy_admin: role == "admin",
        sponsor: None,
        name: None,
        partial_view: false,
        divergence: Vec::new(),
    }
}

fn member(key: &str, role: &str, me: bool) -> MemberDto {
    MemberDto {
        key: key.into(),
        role: role.into(),
        did: None,
        me,
        sponsor: None,
        alias: String::new(),
    }
}

fn space_view(devices: Vec<DeviceKey>, diagnosis: Option<DiagnosisView>) -> SpaceView {
    SpaceView {
        space: "ws_one".into(),
        standing: standing("admin", true),
        members: vec![
            member("act_me", "admin", true),
            member("act_them", "member", false),
        ],
        devices,
        diagnosis,
    }
}

fn one_orbit() -> HostContext {
    HostContext {
        version: "lait 0.0.0".into(),
        identity_home: "home".into(),
        spaces_root: "D:/lait".into(),
        worlds: Vec::new(),
        identities: Vec::new(),
        orbits: vec![OrbitEntry {
            space: "ws_one".into(),
            name: "Work".into(),
            path: "D:/lait/work".into(),
            last_opened: 0,
        }],
    }
}

/// Listing Spaces is passive; choosing one is the act that asks. The read has
/// to be caused by the click and not by the page being drawn — otherwise
/// opening Members would place every Orbit this device serves.
#[test]
fn a_space_is_asked_only_once_somebody_chooses_it() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(one_orbit());

    let mut harness = harness(app, Surface::Members);
    harness.run();
    harness.run();
    assert!(
        !harness
            .state()
            .actions
            .iter()
            .any(|action| matches!(action, Action::ReadSpace(_))),
        "drawing the list of Spaces asked one of them a question"
    );
    assert!(announces(&harness, "Work"));

    harness.get_by_label("Work").click();
    harness.run();
    assert!(
        harness.state().actions.iter().any(|action| matches!(
            action,
            Action::ReadSpace(at) if at.space == "ws_one" && at.path == "D:/lait/work"
        )),
        "choosing a Space did not ask it: {:?}",
        harness.state().actions
    );
}

/// You cannot remove yourself from a Space, or change your own role, from here.
/// Both are refused before the click rather than after it.
#[test]
fn a_person_cannot_fence_themselves_out_of_their_own_space() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(one_orbit());
    app.apply(Update::Done {
        key: "space.read:ws_one".into(),
        outcome: Outcome::Read(Read::Space(Box::new(space_view(Vec::new(), None)))),
    });

    let mut harness = harness(app, Surface::Members);
    harness.state_mut().chrome.members.selected = Some(SpaceRef {
        space: "ws_one".into(),
        path: "D:/lait/work".into(),
    });
    harness.run();

    let removals: Vec<_> = harness.get_all_by_label("Remove…").collect();
    assert_eq!(removals.len(), 2, "one removal control per member");
    assert!(
        removals[0].accesskit_node().is_disabled(),
        "a person was offered a control that removes themselves"
    );
    assert!(!removals[1].accesskit_node().is_disabled());

    let roles: Vec<_> = harness.get_all_by_label_contains("mote").collect();
    assert!(
        roles[0].accesskit_node().is_disabled(),
        "a person was offered a control that changes their own role"
    );
}

/// Removing a member is confirmed by their id. The engine will refuse an
/// unauthorised removal anyway; asking here is what stops a click from being
/// the first time somebody thinks about it.
#[test]
fn removing_a_member_takes_their_id_first() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(one_orbit());
    app.apply(Update::Done {
        key: "space.read:ws_one".into(),
        outcome: Outcome::Read(Read::Space(Box::new(space_view(Vec::new(), None)))),
    });

    let mut harness = harness(app, Surface::Members);
    harness.state_mut().chrome.members.selected = Some(SpaceRef {
        space: "ws_one".into(),
        path: "D:/lait/work".into(),
    });
    harness.run();

    let removals: Vec<_> = harness.get_all_by_label("Remove…").collect();
    removals[1].click();
    harness.run();
    assert!(announces(&harness, "fences them out"));
    assert!(
        harness
            .get_by_label("Remove member")
            .accesskit_node()
            .is_disabled(),
        "removal was offered before it was confirmed"
    );

    harness.state_mut().chrome.members.confirmation = "act_them".into();
    harness.run();
    harness.get_by_label("Remove member").click();
    harness.run();
    assert!(
        harness.state().actions.iter().any(|action| matches!(
            action,
            Action::Administer { operation, .. }
                if matches!(&**operation, SpaceOp::MemberRemove { who } if who == "act_them")
        )),
        "confirming a removal asked for something else"
    );
}

/// The Space plane answers the device list as prose. A line that is not a
/// device id offers nothing to revoke, and the machine answering is never
/// offered its own revocation.
#[test]
fn a_device_line_offers_revocation_only_when_it_is_a_device_that_is_not_this_one() {
    let id = "a".repeat(64);
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(one_orbit());
    app.apply(Update::Done {
        key: "space.read:ws_one".into(),
        outcome: Outcome::Read(Read::Space(Box::new(space_view(
            vec![
                DeviceKey {
                    id: Some(id.clone()),
                    line: format!("{id} (this device)"),
                    is_this_device: true,
                },
                DeviceKey {
                    id: Some("b".repeat(64)),
                    line: "b".repeat(64),
                    is_this_device: false,
                },
                DeviceKey {
                    id: None,
                    line: "no devices".into(),
                    is_this_device: false,
                },
            ],
            None,
        )))),
    });

    let mut harness = harness(app, Surface::Members);
    harness.state_mut().chrome.members.selected = Some(SpaceRef {
        space: "ws_one".into(),
        path: "D:/lait/work".into(),
    });
    harness.run();

    let revocations: Vec<_> = harness.get_all_by_label("Revoke").collect();
    assert_eq!(revocations.len(), 3, "one revocation control per line");
    assert!(
        revocations[0].accesskit_node().is_disabled(),
        "the machine in use offered to fence itself out"
    );
    assert!(!revocations[1].accesskit_node().is_disabled());
    assert!(
        revocations[2].accesskit_node().is_disabled(),
        "a line that is not a device id offered a revocation"
    );
}

/// A diagnosis that could not be taken is absent, and absent is not "every
/// gate passes". Those are the two answers this client spends its effort
/// keeping apart.
#[test]
fn a_space_that_could_not_be_diagnosed_does_not_read_as_healthy() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(one_orbit());
    app.apply(Update::Done {
        key: "space.read:ws_one".into(),
        outcome: Outcome::Read(Read::Space(Box::new(space_view(Vec::new(), None)))),
    });

    let mut harness = harness(app, Surface::Members);
    harness.state_mut().chrome.members.selected = Some(SpaceRef {
        space: "ws_one".into(),
        path: "D:/lait/work".into(),
    });
    harness.run();
    assert!(
        announces(&harness, "could not be diagnosed"),
        "an undiagnosed Space was drawn as one with nothing wrong"
    );
}

/// An invite lifetime that is not a number of hours is refused rather than
/// silently defaulted. Somebody who typed something meant something, and a
/// week is not a safe guess about what.
#[test]
fn an_invite_with_an_unreadable_lifetime_is_refused_rather_than_defaulted() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(one_orbit());
    app.apply(Update::Done {
        key: "space.read:ws_one".into(),
        outcome: Outcome::Read(Read::Space(Box::new(space_view(Vec::new(), None)))),
    });

    let mut harness = harness(app, Surface::Members);
    harness.state_mut().chrome.members.selected = Some(SpaceRef {
        space: "ws_one".into(),
        path: "D:/lait/work".into(),
    });
    harness.state_mut().chrome.members.invite_hours = "a week".into();
    harness.run();
    assert!(
        harness
            .get_by_label("Mint an invite")
            .accesskit_node()
            .is_disabled(),
        "an invite with an unreadable lifetime was offered anyway"
    );

    harness.state_mut().chrome.members.invite_hours = "48".into();
    harness.run();
    harness.get_by_label("Mint an invite").click();
    harness.run();
    assert!(
        harness.state().actions.iter().any(|action| matches!(
            action,
            Action::Administer { operation, .. }
                if matches!(&**operation, SpaceOp::Invite { ttl_hours: 48, .. })
        )),
        "minting asked for a different lifetime than the one typed"
    );
}

/// A node whose view of the Space is incomplete says so. A short roster and a
/// wrong one look identical, and only one of them is a reason to wait.
#[test]
fn an_incomplete_view_is_drawn_as_incomplete_rather_than_as_a_short_roster() {
    let mut view = space_view(Vec::new(), None);
    view.standing.partial_view = true;
    view.standing.divergence = vec!["epoch 3 has not arrived".into()];

    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(one_orbit());
    app.apply(Update::Done {
        key: "space.read:ws_one".into(),
        outcome: Outcome::Read(Read::Space(Box::new(view))),
    });

    let mut harness = harness(app, Surface::Members);
    harness.state_mut().chrome.members.selected = Some(SpaceRef {
        space: "ws_one".into(),
        path: "D:/lait/work".into(),
    });
    harness.run();
    assert!(announces(&harness, "view of the Space is incomplete"));
    assert!(announces(&harness, "epoch 3 has not arrived"));
}

/// Quiet is honestly global: while it is on, a per-Space mute is not a control
/// that does anything, and it says so rather than looking live.
#[test]
fn a_globally_quiet_client_offers_no_per_space_muting() {
    let mut app = App::new();
    app.absorb(snapshot(Vec::new()));
    app.absorb_context(one_orbit());

    let mut harness = harness(app, Surface::Spaces);
    harness.run();
    assert!(
        !harness
            .get_by_label("Mute Work")
            .accesskit_node()
            .is_disabled(),
        "a Space could not be muted while the client was not quiet"
    );

    harness.state_mut().chrome.quiet.everything = true;
    harness.run();
    assert!(
        harness
            .get_by_label("Mute Work")
            .accesskit_node()
            .is_disabled(),
        "a per-Space mute stayed live while the whole client was quiet"
    );
    // And the gap is named rather than left to be discovered by not hearing
    // about a comment on your work.
    assert!(announces(&harness, "SUB-6"));
}
