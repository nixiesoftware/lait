#![cfg(feature = "egui-ui")]

//! Theme, contrast and scaling — tested rather than asserted.
//!
//! Three of the release criteria are about a person being able to *see* the
//! interface, and all three are the kind that stay true by accident until they
//! quietly stop being true. A hard-coded amber reads fine on light and on dark
//! and vanishes against the high-contrast scheme somebody selected precisely
//! because they cannot see it otherwise. A layout that assumes one device pixel
//! per point looks right on the machine it was written on.
//!
//! ## Contrast is measured, not eyeballed
//!
//! The floor is the project's own — `theme::MINIMUM_CONTRAST`, over
//! `theme::contrast`, which is WCAG relative luminance and the standard ratio
//! and is anchored in its own tests against black-on-white and white-on-white.
//! Re-deriving the arithmetic here would be a second implementation to keep in
//! agreement with the first, and the interesting statement is not "this
//! arithmetic is right" but "every scheme the client can be drawn in clears
//! it".
//!
//! Behind the `egui-ui` feature that carries the interface these test, so a
//! default build — the one the bridge and the Flutter client use — compiles
//! with no egui in the graph at all. CI runs `--all-features`, so nothing is
//! lost while the egui surfaces still carry flows the Dart pages have not
//! taken over.

use astrolabe::model::App;
use astrolabe::ui::theme::{contrast, MINIMUM_CONTRAST};
use astrolabe::ui::{Chrome, Surface};
use egui::{Color32, Visuals};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use lait_workbench::{
    Capabilities, DeviceSnapshot, EnvironmentSnapshot, LifecycleState, ObservationHealth,
    WorkbenchSnapshot,
};

/// A scheme like the one Windows' high-contrast themes produce: a background at
/// one end of the range and text at the other, with no midtones to hide in.
fn high_contrast() -> Visuals {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = Color32::BLACK;
    visuals.window_fill = Color32::BLACK;
    visuals.extreme_bg_color = Color32::BLACK;
    visuals.override_text_color = Some(Color32::WHITE);
    visuals.warn_fg_color = Color32::from_rgb(255, 255, 0);
    visuals.error_fg_color = Color32::from_rgb(255, 0, 0);
    visuals
}

/// What the interface asks the visuals for, read back through a real frame.
fn accents(visuals: Visuals) -> (Color32, Color32, Color32, Color32) {
    let context = egui::Context::default();
    context.set_visuals(visuals);
    let mut captured = None;
    let _ = context.run_ui(egui::RawInput::default(), |ui| {
        captured = Some((
            astrolabe::ui::theme::attention(ui),
            astrolabe::ui::theme::danger(ui),
            astrolabe::ui::theme::secondary(ui),
            ui.visuals().panel_fill,
        ));
    });
    captured.expect("a frame drew")
}

/// Every accent has to be legible against the scheme it is drawn on — including
/// the one somebody chose because nothing else was legible.
///
/// The floor is `theme::MINIMUM_CONTRAST` — WCAG's large-text and non-text
/// ratio, which is what these short coloured labels beside ordinary text are.
#[test]
fn every_accent_is_legible_against_light_dark_and_high_contrast() {
    for (name, visuals) in [
        ("light", Visuals::light()),
        ("dark", Visuals::dark()),
        ("high contrast", high_contrast()),
    ] {
        let (attention, danger, secondary, background) = accents(visuals);
        for (what, colour) in [
            ("attention", attention),
            ("danger", danger),
            ("secondary", secondary),
        ] {
            let ratio = contrast(colour, background);
            assert!(
                ratio >= MINIMUM_CONTRAST,
                "the {what} accent is {ratio:.2}:1 against the {name} background, \
                 which is not readable"
            );
        }
    }
}

/// Attention and danger must not be the same colour. They mean different things
/// — "notice this" and "this went wrong" — and a surface that drew them
/// identically would be using colour to say nothing.
#[test]
fn attention_and_danger_stay_distinguishable_in_every_scheme() {
    for (name, visuals) in [
        ("light", Visuals::light()),
        ("dark", Visuals::dark()),
        ("high contrast", high_contrast()),
    ] {
        let (attention, danger, _, _) = accents(visuals);
        assert_ne!(
            attention, danger,
            "attention and danger are the same colour in {name}"
        );
    }
}

/// The window's own chrome answers to the same scheme the pages do.
///
/// Three things have to hold at once, and each of them is a way the caption
/// could look right on the machine it was written on and be unusable elsewhere:
/// the bar has to be distinguishable from the page under it, a control under the
/// pointer has to be distinguishable from the bar, and the close control's mark
/// has to stay readable on the red that appears under it — which is not the bar
/// at all, and is the one background in this client that a hover *replaces*
/// rather than tints.
#[test]
fn the_windows_own_chrome_is_legible_in_every_scheme() {
    for (name, visuals) in [
        ("light", Visuals::light()),
        ("dark", Visuals::dark()),
        ("high contrast", high_contrast()),
    ] {
        let context = egui::Context::default();
        context.set_visuals(visuals);
        let mut captured = None;
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let bar = astrolabe::ui::theme::raised(ui);
            captured = Some((
                ui.visuals().panel_fill,
                bar,
                astrolabe::ui::theme::hovered(bar),
                astrolabe::ui::theme::danger_fill(ui),
                ui.visuals().text_color(),
            ));
        });
        let (page, bar, hovered, danger, ink) = captured.expect("a frame drew");

        assert_ne!(bar, page, "the {name} title bar is the page it sits on");
        assert_ne!(
            hovered, bar,
            "a {name} window control under the pointer looks exactly like the bar"
        );
        assert_ne!(
            danger, bar,
            "the {name} close control's hover looks exactly like the bar"
        );

        // The mark, on whatever is behind it. `legible` is what the caption
        // itself asks for, so this measures the colour that is actually drawn
        // rather than the one the theme happened to answer.
        for (what, behind) in [("the bar", bar), ("a hover", hovered), ("close", danger)] {
            let mark = astrolabe::ui::theme::legible(ink, behind);
            let ratio = contrast(mark, behind);
            assert!(
                ratio >= MINIMUM_CONTRAST,
                "a window control's mark is {ratio:.2}:1 on {what} in {name}"
            );
        }
    }
}

/// The accent is a *fill*, and everything drawn on it has to survive that.
///
/// This is the trap the rest of the module is built to catch, one layer along:
/// every quiet colour in the client is held to a floor against the **page**,
/// which is the right background for almost everything and the wrong one for
/// the two places a fill replaces it — a primary control's label, and the
/// chosen row of a list. A grey that reads perfectly on the page is the grey
/// that vanishes on the accent.
#[test]
fn what_is_drawn_on_the_accent_is_legible_on_the_accent() {
    for (name, visuals) in [
        ("light", Visuals::light()),
        ("dark", Visuals::dark()),
        ("high contrast", high_contrast()),
    ] {
        let context = egui::Context::default();
        context.set_visuals(visuals);
        let mut captured = None;
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let accent = astrolabe::ui::theme::accent(ui);
            captured = Some((
                ui.visuals().panel_fill,
                accent,
                // The label of the one primary control on a page, and the two
                // states a list row can be in while it is the chosen one.
                [
                    ui.visuals().text_color(),
                    ui.visuals().weak_text_color(),
                    ui.visuals().warn_fg_color,
                ]
                .map(|hue| astrolabe::ui::theme::legible(hue, accent)),
            ));
        });
        let (page, accent, inks) = captured.expect("a frame drew");

        assert_ne!(
            accent, page,
            "the {name} accent is the page, so nothing it fills reads as chosen"
        );
        for ink in inks {
            let ratio = contrast(ink, accent);
            assert!(
                ratio >= MINIMUM_CONTRAST,
                "text on the {name} accent is {ratio:.2}:1"
            );
        }
    }
}

fn snapshot() -> WorkbenchSnapshot {
    WorkbenchSnapshot {
        schema_version: 1,
        revision: 1,
        environment: EnvironmentSnapshot {
            state_root: "root".into(),
            executable: "lait".into(),
            server_pid: 1,
        },
        capabilities: Capabilities::default(),
        devices: vec![DeviceSnapshot {
            id: "alice".into(),
            label: "alice".into(),
            home: "home".into(),
            log_path: "log".into(),
            state: LifecycleState::External,
            pid: None,
            owned: false,
            started_at_ms: None,
            last_error: None,
            facts: None,
            observation: ObservationHealth::default(),
            image: None,
        }],
        connections: Vec::new(),
    }
}

/// The interface is the same interface at any scaling. Every control is still
/// there, still named, and still carrying the state that says whether it may be
/// used — which is what a screen reader reads and what a person on a 200% display
/// depends on.
#[test]
fn the_semantic_tree_survives_a_scaled_display() {
    for scale in [1.0_f32, 1.5, 2.0, 3.0] {
        let mut app = App::new();
        app.absorb(snapshot());
        let mut harness = Harness::builder()
            .with_size([1_200.0, 2_000.0])
            .with_pixels_per_point(scale)
            .build_ui_state(
                |ui, (app, chrome): &mut (App, Chrome)| {
                    astrolabe::ui::draw(ui, app, chrome);
                },
                (app, Chrome::showing(Surface::Devices)),
            );
        harness.run();

        for title in ["Library", "Spaces", "Devices", "Heads"] {
            assert!(
                harness
                    .query_by_role_and_label(egui::accesskit::Role::Button, title)
                    .is_some(),
                "{title} left the accessibility tree at {scale}× scaling"
            );
        }
        // And the safety boundary is still drawn as one. A control that quietly
        // became enabled at a different scaling would be the worst possible
        // scaling defect.
        assert!(
            harness
                .get_by_label("Force stop")
                .accesskit_node()
                .is_disabled(),
            "an external daemon offered a force-stop at {scale}× scaling"
        );
    }
}

/// Both themes draw the same interface. A surface that only exists in one of
/// them is a surface somebody cannot reach.
#[test]
fn both_themes_draw_the_same_interface() {
    let mut trees = Vec::new();
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let mut app = App::new();
        app.absorb(snapshot());
        let mut harness = Harness::builder()
            .with_size([1_200.0, 2_000.0])
            .with_theme(theme)
            .build_ui_state(
                |ui, (app, chrome): &mut (App, Chrome)| {
                    astrolabe::ui::draw(ui, app, chrome);
                },
                (app, Chrome::showing(Surface::Devices)),
            );
        harness.run();
        let mut labels: Vec<String> = harness
            .query_all_by_label_contains("")
            .filter_map(|node| node.accesskit_node().label())
            .collect();
        labels.sort();
        trees.push(labels);
    }
    assert_eq!(
        trees.first(),
        trees.get(1),
        "the light and dark interfaces are not the same interface"
    );
}
