//! The surfaces. They hold no logic of their own.
//!
//! Every fact drawn here came from the core as a snapshot or a signal, and
//! every action is an [`Action`] handed back to whoever owns the frame loop. A
//! surface never computes a figure, never smooths one, and never invents one to
//! look populated — and it never writes down what it expects an action to do.
//!
//! ## Why drawing returns actions instead of calling
//!
//! A surface that called the client directly would be doing network work on the
//! frame thread. A surface that called it and *kept the answer* would be a
//! second model of client state. Handing back a list of requests is what makes
//! both impossible rather than discouraged — and it is what lets an interaction
//! test click a control and assert on what was asked for, with no daemon
//! anywhere.
//!
//! ## Library-first
//!
//! The front page is the Library — the Orbits this device serves and the Worlds
//! mounted in them. Devices is a peer, not the entry: a person with one identity
//! and several Spaces must never open onto a process inventory.

pub mod devices;
pub mod diagnostics;
pub mod heads;
pub mod library;
pub mod spaces;
pub mod storage;
pub mod theme;

use egui::{Align, Key, Layout, Modifiers, RichText, Ui};

use crate::model::{App, StaleReason};
use crate::runtime::Action;

/// The surfaces a person can be looking at.
///
/// Ordered as they are drawn. `Library` is first because it is the front page,
/// and that ordering is a product decision rather than an alphabetical accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Surface {
    #[default]
    Library,
    /// The host plane: founding, entering, this machine's consent, and the
    /// Orbit registry. Its own surface because every one of those is reachable
    /// when *no* Space exists, which is precisely when no World head can draw a
    /// page to do it from.
    Spaces,
    Devices,
    Heads,
    Storage,
    Diagnostics,
}

impl Surface {
    pub const ALL: [Self; 6] = [
        Self::Library,
        Self::Spaces,
        Self::Devices,
        Self::Heads,
        Self::Storage,
        Self::Diagnostics,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Spaces => "Spaces",
            Self::Devices => "Devices",
            Self::Heads => "Heads",
            Self::Storage => "Storage",
            Self::Diagnostics => "Diagnostics",
        }
    }
}

/// What the interface itself is holding: which surface is showing, and the
/// half-typed contents of every form on it.
///
/// Drafts are the one thing here that is genuinely UI-local. A partly typed
/// store path is not a model of client state — nothing else in the process can
/// know it, and nothing can contradict it — so keeping it beside the surface
/// that draws it does not make a second model of anything.
#[derive(Debug, Default)]
pub struct Chrome {
    pub surface: Surface,
    pub spaces: spaces::Draft,
    pub devices: devices::Draft,
    pub heads: heads::Draft,
    pub diagnostics: diagnostics::Draft,
}

impl Chrome {
    pub fn showing(surface: Surface) -> Self {
        Self {
            surface,
            ..Self::default()
        }
    }
}

/// Draw the whole client for one frame, and collect what it was asked to do.
pub fn draw(ui: &mut Ui, app: &App, chrome: &mut Chrome) -> Vec<Action> {
    let mut actions = Vec::new();

    keyboard(ui, chrome, &mut actions);

    ui.horizontal(|ui| {
        for candidate in Surface::ALL {
            // `selectable_value` carries the selected state into the semantic
            // tree, so a screen reader hears which surface is current rather
            // than only that six buttons exist.
            ui.selectable_value(&mut chrome.surface, candidate, candidate.title());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Enabled even while a re-read is in flight would let a person
            // queue six of them by clicking a button that looked idle.
            let waiting = app.is_in_flight(&Action::Refresh.key());
            if ui
                .add_enabled(!waiting, egui::Button::new("Refresh"))
                .on_hover_text("Read this machine again (F5)")
                .clicked()
            {
                actions.push(Action::Refresh);
            }
        });
    });
    ui.separator();

    draw_freshness(ui, app);

    match chrome.surface {
        Surface::Library => library::draw(ui, app, &mut actions),
        Surface::Spaces => spaces::draw(ui, app, &mut chrome.spaces, &mut actions),
        Surface::Devices => devices::draw(ui, app, &mut chrome.devices, &mut actions),
        Surface::Heads => heads::draw(ui, app, &mut chrome.heads, &mut actions),
        // The engine read that supplies these is SUB-5. Until it lands the
        // model carries nothing, and the surface says "not measured" rather
        // than drawing zeroes — which is the whole contract this surface has.
        Surface::Storage => storage::draw(ui, app.storage(), app.transfers()),
        Surface::Diagnostics => diagnostics::draw(ui, app, &mut chrome.diagnostics, &mut actions),
    }

    draw_notices(ui, app);
    draw_failures(ui, app);
    actions
}

/// Every surface, and the one action worth a key of its own, from the keyboard
/// alone.
///
/// Full keyboard operation is a release criterion, and tabbing to a control is
/// only half of it: a person who navigates by keyboard should not have to walk
/// the tab order to change surface. `Ctrl+1` through `Ctrl+6` are the surfaces
/// in the order they are drawn, which is the order the tabs are read out in.
fn keyboard(ui: &Ui, chrome: &mut Chrome, actions: &mut Vec<Action>) {
    const DIGITS: [Key; 6] = [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
    ];

    ui.input_mut(|input| {
        for (index, key) in DIGITS.iter().enumerate() {
            if input.consume_key(Modifiers::CTRL, *key) {
                if let Some(surface) = Surface::ALL.get(index) {
                    chrome.surface = *surface;
                }
            }
        }
        if input.consume_key(Modifiers::NONE, Key::F5) {
            actions.push(Action::Refresh);
        }
    });
}

/// Say plainly when what is on screen is not current.
///
/// A stale surface that looks identical to a fresh one is the defect this
/// exists to prevent — the figures are still the best available, and a person
/// deciding something on them deserves to know how old they are.
fn draw_freshness(ui: &mut Ui, app: &App) {
    match app.stale() {
        None => {}
        Some(StaleReason::NeverLoaded) => {
            ui.label(RichText::new("Loading…").italics());
        }
        Some(StaleReason::Signalled(reason)) => {
            ui.label(
                RichText::new(format!("Showing the last known state — {reason}"))
                    .color(theme::attention(ui)),
            );
        }
    }
}

/// What worked, most recent first.
///
/// Bounded and unobtrusive, but present: an action that succeeded and left no
/// trace on screen is indistinguishable from one that was never dispatched, and
/// "did that do anything" is the question a client with no record cannot answer.
fn draw_notices(ui: &mut Ui, app: &App) {
    let mut notices = app.notices().peekable();
    if notices.peek().is_none() {
        return;
    }
    ui.separator();
    for notice in notices.take(3) {
        ui.label(RichText::new(&notice.said).color(theme::secondary(ui)));
    }
}

fn draw_failures(ui: &mut Ui, app: &App) {
    let mut failures = app.failures().peekable();
    if failures.peek().is_none() {
        return;
    }
    ui.separator();
    ui.with_layout(Layout::top_down(Align::Min), |ui| {
        for failure in failures {
            ui.label(
                RichText::new(format!("{}: {}", failure.what, failure.error))
                    .color(theme::danger(ui)),
            );
        }
    });
}

/// A control that dispatches one action, disabled while that action is in
/// flight and while `enabled` says it cannot be used.
///
/// Written once because the rule it encodes is the same everywhere and easy to
/// forget in one place: a control that stays live during its own action lets a
/// person ask for the same thing four times, and the fourth refusal is the one
/// they see.
pub(crate) fn act(
    ui: &mut Ui,
    app: &App,
    label: &str,
    enabled: bool,
    disabled_because: &str,
    action: impl FnOnce() -> Action,
) -> Option<Action> {
    let candidate = action();
    let waiting = app.is_in_flight(&candidate.key());
    let response = ui
        .add_enabled(enabled && !waiting, egui::Button::new(label))
        .on_disabled_hover_text(if waiting {
            "This is already under way."
        } else {
            disabled_because
        });
    response.clicked().then_some(candidate)
}
