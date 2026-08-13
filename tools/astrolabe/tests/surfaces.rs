//! Every surface, rendered.
//!
//! The interaction tests ask the accessibility tree what is *there*. This asks
//! the renderer what it *looks like*, offscreen, at a fixed size, in both
//! themes — and writes a PNG per surface so a person (or an agent) can look at
//! all of them at once instead of screenshotting one live window one tab at a
//! time.
//!
//! ## Why this is a test and not a script
//!
//! Because it asserts something worth asserting: a surface that renders to a
//! blank frame is a defect, and it is one that no semantic assertion catches —
//! the accessibility tree is perfectly happy about a control that was laid out
//! at zero size or clipped off the edge of its own panel.
//!
//! The images land in `target/surfaces/`, which is build output rather than a
//! committed baseline. Once the visual language stops moving, swapping
//! `render` for `kittest`'s `snapshot` turns this file into the regression gate
//! as well.
//!
//! ## One test, deliberately
//!
//! Every render happens inside a single `#[test]`, in sequence. `cargo nextest`
//! gives each test its own process and would not care, but plain `cargo test`
//! runs them as threads in one process — and two wgpu devices coming up
//! concurrently there is an access violation, not a test failure. One test is
//! the cheapest way to make both runners behave the same.

use astrolabe::client::heads::McpBindingOutcome;
use astrolabe::client::host::{HostContext, OrbitEntry};
use astrolabe::client::library::{LibraryEntry, Placement};
use astrolabe::client::space::{DeviceKey, SpaceRef, SpaceView};
use astrolabe::client::storage::{Missing, StorageFacts};
use astrolabe::model::App;
use astrolabe::runtime::{Outcome, Read, Update};
use astrolabe::ui::{Chrome, Surface};
use egui_kittest::Harness;
use lait::diagnose::DiagnosisView;
use lait::dto::{MemberDto, WhoamiDto};
use lait_workbench::{
    Capabilities, ConnectionSnapshot, DeviceSnapshot, EnvironmentSnapshot, HeadFacts, HeadKind,
    LifecycleState, ObservationHealth, ObservationState, Ownership, WorkbenchSnapshot,
};

/// The real window's inner size, so a rendered surface is laid out at the width
/// it actually gets rather than at whatever the harness defaults to.
const SIZE: [f32; 2] = [1_040.0, 720.0];

fn device(id: &str, state: LifecycleState, owned: bool, degraded: bool) -> DeviceSnapshot {
    DeviceSnapshot {
        id: id.into(),
        label: id.into(),
        home: format!("D:/lait/devices/{id}"),
        log_path: "log".into(),
        state,
        pid: owned.then_some(4_242),
        owned,
        started_at_ms: None,
        last_error: None,
        facts: None,
        observation: if degraded {
            ObservationHealth {
                state: ObservationState::Degraded,
                sampled_at_ms: Some(10),
                stale_since_ms: Some(20),
                error: Some("control channel refused".into()),
            }
        } else {
            ObservationHealth::default()
        },
        image: None,
    }
}

/// A machine with something on it — the state worth looking at, rather than the
/// empty one that flatters every layout.
fn populated() -> App {
    let mut app = App::new();
    app.absorb(WorkbenchSnapshot {
        schema_version: 1,
        revision: 7,
        environment: EnvironmentSnapshot {
            state_root: "C:/Users/you/AppData/Local/Astrolabe/devices".into(),
            executable: "C:/Program Files/Astrolabe/lait.exe".into(),
            server_pid: 4_242,
        },
        capabilities: Capabilities {
            force_stop_owned_process: false,
            ..Capabilities::default()
        },
        devices: vec![
            device("alice", LifecycleState::Running, true, false),
            device("bob", LifecycleState::External, false, true),
            device("seed", LifecycleState::Stopped, true, false),
        ],
        connections: vec![
            ConnectionSnapshot {
                source_device_id: "alice".into(),
                space_id: "ws_38TLCQUD96NG9376CBELI5I5V2".into(),
                peer_id: "peer-1".into(),
                peer_nick: "morgan".into(),
                state: "connected".into(),
                online: true,
                dialable: true,
                blocked_by: None,
                target_device_id: Some("bob".into()),
            },
            ConnectionSnapshot {
                source_device_id: "alice".into(),
                space_id: "ws_38TLCQUD96NG9376CBELI5I5V2".into(),
                peer_id: "peer-2".into(),
                peer_nick: "sam".into(),
                state: "offering".into(),
                online: false,
                dialable: false,
                blocked_by: Some("admission has not landed".into()),
                target_device_id: None,
            },
        ],
    });

    app.absorb_context(HostContext {
        version: "lait 0.7.11".into(),
        identity_home: "C:/Users/you/AppData/Roaming/nixi/lait/config".into(),
        spaces_root: "C:/Users/you/AppData/Roaming/nixi/lait/config/spaces".into(),
        worlds: vec!["com.lait.issues".into()],
        identities: Vec::new(),
        orbits: vec![OrbitEntry {
            space: "ws_38TLCQUD96NG9376CBELI5I5V2".into(),
            name: "ISSUEWORLD".into(),
            path: "C:/Users/you/.../spaces/issueworld-078".into(),
            last_opened: 0,
        }],
    });

    app.absorb_library(vec![
        LibraryEntry {
            orbit: "ws_38TLCQUD96NG9376CBELI5I5V2".into(),
            space: "ws_38TLCQUD96NG9376CBELI5I5V2".into(),
            world_mount: "issues".into(),
            display_name: Some("Issues".into()),
            entry_path: Some("/".into()),
            placement: Placement::Placed,
        },
        LibraryEntry {
            orbit: "ws_7QK2M4RVJ8N3P5T6W9Y1Z0ABCD".into(),
            space: "ws_7QK2M4RVJ8N3P5T6W9Y1Z0ABCD".into(),
            world_mount: String::new(),
            display_name: Some("Work".into()),
            entry_path: None,
            placement: Placement::Vacant,
        },
    ]);

    app.absorb_storage(
        vec![
            StorageFacts {
                orbit: "ws_38TLCQUD96NG9376CBELI5I5V2".into(),
                name: Some("ISSUEWORLD".into()),
                bytes_on_disk: Some(41_943_040),
                object_count: Some(1_284),
                last_verified_ms: Some(1_786_558_069_000),
                missing: None,
            },
            StorageFacts::unmeasured("ws_7QK2M4RVJ8N3P5T6W9Y1Z0ABCD", Missing::NotPlaced),
        ],
        Vec::new(),
    );

    app.absorb_heads(vec![
        HeadFacts {
            id: "identity:default".into(),
            kind: HeadKind::Browser,
            device: None,
            orbit: None,
            identity: "the ordinary identity".into(),
            ownership: Ownership::Owned,
            pid: Some(9_001),
            url: Some("http://127.0.0.1:52713/?token=secret".into()),
        },
        HeadFacts {
            id: "alice-browser".into(),
            kind: HeadKind::Browser,
            device: Some("alice".into()),
            orbit: None,
            identity: "D:/lait/devices/alice".into(),
            ownership: Ownership::External,
            pid: None,
            url: None,
        },
    ]);

    app.apply(Update::Done {
        key: "space.read:ws_38TLCQUD96NG9376CBELI5I5V2".into(),
        outcome: Outcome::Read(Read::Space(Box::new(space_view()))),
    });
    app.apply(Update::Done {
        key: "device.stop:seed".into(),
        outcome: Outcome::Said("seed stopped".into()),
    });
    app
}

fn space_view() -> SpaceView {
    SpaceView {
        space: "ws_38TLCQUD96NG9376CBELI5I5V2".into(),
        standing: WhoamiDto {
            actor: Some("act_86a32a40".into()),
            device: "c3ab2101".into(),
            did: None,
            space: Some("ws_38TLCQUD96NG9376CBELI5I5V2".into()),
            role: "admin".into(),
            member: true,
            can_write: true,
            capabilities: Vec::new(),
            policy_admin: true,
            sponsor: None,
            name: Some("Huginn".into()),
            partial_view: false,
            divergence: Vec::new(),
        },
        members: vec![
            MemberDto {
                key: "act_86a32a40c88b66b026bd7567542e228b".into(),
                role: "admin".into(),
                did: None,
                me: true,
                sponsor: None,
                alias: String::new(),
            },
            MemberDto {
                key: "act_5f1c9d2e77a4b30918ce4471bb2d6633".into(),
                role: "member".into(),
                did: None,
                me: false,
                sponsor: None,
                alias: "Morgan".into(),
            },
            MemberDto {
                key: "act_be74102aa9c5480fa1e6d2338c07bb51".into(),
                role: "member".into(),
                did: None,
                me: false,
                sponsor: Some("act_86a32a40c88b66b026bd7567542e228b".into()),
                alias: "claude".into(),
            },
        ],
        devices: vec![
            DeviceKey {
                id: Some("c3ab2101".repeat(8)),
                line: format!("{} (this device)", "c3ab2101".repeat(8)),
                is_this_device: true,
            },
            DeviceKey {
                id: Some("9f4e77b2".repeat(8)),
                line: "9f4e77b2".repeat(8),
                is_this_device: false,
            },
        ],
        diagnosis: Some(DiagnosisView {
            schema_version: 3,
            gates: Vec::new(),
            blocked_on: None,
            summary: "This node is in and synced.".into(),
        }),
    }
}

/// Where a person (or an agent) goes to look at all of them at once.
fn gallery() -> std::path::PathBuf {
    // Anchored to the manifest rather than to the working directory: cargo runs
    // a test from its package root, so a relative `target/` lands in
    // `tools/astrolabe/target/` — a second build directory nobody looks in.
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels below the workspace root")
        .join("target")
        .join("surfaces");
    std::fs::create_dir_all(&directory).expect("a place to write renders");
    directory
}

/// The chrome a render starts from: a Space chosen, a device chosen, a project
/// typed. Those are the states worth looking at — an unchosen surface draws its
/// "choose something" line, which is correct and tells you nothing about the
/// layout.
fn chrome_for(surface: Surface) -> Chrome {
    let mut chrome = Chrome::showing(surface);
    chrome.members.selected = Some(SpaceRef {
        space: "ws_38TLCQUD96NG9376CBELI5I5V2".into(),
        path: "C:/Users/you/.../spaces/issueworld-078".into(),
    });
    chrome.diagnostics.device = Some("alice".into());
    chrome.heads.project = "D:/Documents/projects/lait".into();
    chrome
}

/// Render one state and write it where a person can look.
///
/// Returns the number of distinct colours, because a surface that rendered to
/// one flat fill is a surface that laid nothing out — and no semantic assertion
/// catches that. The accessibility tree is perfectly happy about a control
/// sized to nothing or clipped off the edge of its own panel.
fn render(name: &str, theme: egui::Theme, app: App, chrome: Chrome) -> Result<usize, String> {
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_theme(theme)
        .build_ui_state(
            |ui, (app, chrome): &mut (App, Chrome)| {
                astrolabe::ui::draw(ui, app, chrome);
            },
            (app, chrome),
        );
    // The ladder reaches a rendered surface the same way it reaches the shell,
    // or the picture is of an interface nobody ships.
    astrolabe::ui::install(&harness.ctx);
    harness.run();

    let image = harness.render()?;
    image
        .save(gallery().join(format!("{name}.png")))
        .map_err(|error| error.to_string())?;
    Ok(image
        .pixels()
        .map(|pixel| pixel.0)
        .collect::<std::collections::HashSet<_>>()
        .len())
}

/// Every surface, in both themes, plus the two states that are easy to get
/// wrong and impossible to see from a screenshot of a running client: the
/// machine with nothing on it, and the tallest thing the Heads surface draws.
#[test]
fn every_surface_renders_something_in_both_themes() {
    let mut thin = Vec::new();

    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let suffix = if theme == egui::Theme::Dark {
            "dark"
        } else {
            "light"
        };
        for surface in Surface::ALL {
            let name = format!("{}-{suffix}", surface.title().to_ascii_lowercase());
            match render(&name, theme, populated(), chrome_for(surface)) {
                Ok(colours) if colours < 8 => {
                    thin.push(format!("{name} rendered {colours} distinct colours"));
                }
                Ok(_) => {}
                Err(error) => {
                    // A machine with no usable graphics adapter cannot render,
                    // and that is not this interface being wrong. Said once
                    // rather than fourteen times.
                    eprintln!("no renderer available; skipping surface renders: {error}");
                    return;
                }
            }
        }
    }

    // Loading is not empty, and neither is what a person sees first.
    let _ = render(
        "library-loading",
        egui::Theme::Dark,
        App::new(),
        Chrome::showing(Surface::Library),
    );

    // A returned preview is the tallest thing this client draws, which makes it
    // the one most likely to run off the bottom of its own panel.
    let mut previewed = populated();
    previewed.apply(Update::Done {
        key: "mcp.preview".into(),
        outcome: Outcome::Mcp(Box::new(McpBindingOutcome {
            path: "D:/Documents/projects/lait/.mcp.json".into(),
            detail: "{\n  \"mcpServers\": {\n    \"lait\": {\n      \"command\": \"lait\",\n      \"args\": [\"mcp\"],\n      \"env\": { \"LAIT_AGENT\": \"claude\" }\n    }\n  }\n}\n".into(),
            note: Some(
                "the lait Claude Code plugin already provides an MCP server named 'lait'".into(),
            ),
            replaced: false,
            agent: Some("claude".into()),
            written: false,
        })),
    });
    let _ = render(
        "heads-preview",
        egui::Theme::Dark,
        previewed,
        chrome_for(Surface::Heads),
    );

    assert!(thin.is_empty(), "{thin:#?}");
}
