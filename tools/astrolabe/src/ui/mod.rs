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

pub mod caption;
pub mod devices;
pub mod diagnostics;
pub mod geometry;
pub mod header;
pub mod heads;
pub mod library;
pub mod members;
pub mod spaces;
pub mod storage;
pub mod theme;

use egui::{Align, Key, Layout, Modifiers, RichText, ScrollArea, Ui};

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
    /// The Space plane: membership, invites, this actor's machines, custody and
    /// the onboarding gates. Separate from Spaces because those are the things
    /// reachable *before* a Space exists and these are what one says about
    /// itself afterwards.
    Members,
    Devices,
    Heads,
    Storage,
    Diagnostics,
}

impl Surface {
    pub const ALL: [Self; 7] = [
        Self::Library,
        Self::Spaces,
        Self::Members,
        Self::Devices,
        Self::Heads,
        Self::Storage,
        Self::Diagnostics,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Spaces => "Spaces",
            Self::Members => "Members",
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
    pub library: library::Draft,
    pub spaces: spaces::Draft,
    pub devices: devices::Draft,
    pub heads: heads::Draft,
    pub diagnostics: diagnostics::Draft,
    pub members: members::Draft,
    /// Who is allowed to interrupt.
    ///
    /// Interface policy rather than client state: nothing outside this process
    /// can know it and nothing can contradict it, which is the same test the
    /// half-typed drafts above pass. It sits here so that muting stops an
    /// interruption without stopping the observation behind it.
    pub quiet: crate::notify::Quiet,
    /// What the window was asked to do this frame — move, minimise, maximise,
    /// close.
    ///
    /// Output rather than state, and the same shape as the [`Action`]s `draw`
    /// returns: emptied at the top of every frame, filled by the caption, and
    /// carried out by whoever owns the window. It travels here rather than in
    /// the return value because it is not an `Action` — nothing in it reaches
    /// the client, the daemon or the network, and folding it in would make
    /// "everything drawing asked for" one list with two dispatchers.
    pub window: Vec<caption::Ask>,
}

impl Chrome {
    pub fn showing(surface: Surface) -> Self {
        Self {
            surface,
            ..Self::default()
        }
    }
}

/// Put the ladder into the context this interface will be drawn in.
///
/// Called by whoever owns the context — the shell at startup, and the snapshot
/// harness before it renders — rather than from inside `draw`, so the seam is
/// visible and a frame does not pay to rebuild a font table it already has.
/// `all_styles_mut` because corner radii live in visuals, which is per-theme:
/// applying to one style would give the light and dark interfaces different
/// corners.
pub fn install(ctx: &egui::Context) {
    ctx.all_styles_mut(geometry::apply);
}

/// Draw the whole client for one frame, and collect what it was asked to do.
///
/// The page margin lives here rather than in the shell, because breathing room
/// is part of the visual language and not a property of the window: a surface
/// rendered headlessly for a snapshot has to have it too, or the picture is of
/// something nobody ships.
pub fn draw(ui: &mut Ui, app: &App, chrome: &mut Chrome) -> Vec<Action> {
    let mut actions = Vec::new();

    // Cleared here rather than by whoever drains it, so that a shell which
    // forgets to look cannot accumulate a frame's worth of window commands and
    // carry them all out at once, three seconds late.
    chrome.window.clear();

    // The client paints its own page, first, over everything it was given.
    //
    // `eframe::App::ui` hands over a `Ui` with *no background* — its own docs
    // say so and tell you to wrap in a `CentralPanel` — and the window behind it
    // is cleared to eframe's default, a near-black `rgba(12,12,12,180)`. On a
    // dark system that looks deliberate. On a light one the interface renders
    // dark text on near-black and cannot be read at all, which is what it had
    // been doing.
    //
    // Painted here rather than in the shell so a rendered surface has it too:
    // the shell is not the only thing that draws this interface, and a
    // background that only exists in the shell is one no test can see.
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);

    keyboard(ui, chrome, &mut actions);

    // Outside the page margin, because a header inset from the window edge
    // reads as a row of controls that happen to be at the top rather than as
    // chrome. Its contents are inset to the same margin, so the first nav item
    // lines up with the first word of every surface below it.
    header::draw(ui, app, chrome, &mut actions);

    let page = egui::Frame::NONE
        .inner_margin(geometry::page_margin())
        .show(ui, |ui| draw_page(ui, app, chrome))
        .inner;
    actions.extend(page);
    actions
}

fn draw_page(ui: &mut Ui, app: &App, chrome: &mut Chrome) -> Vec<Action> {
    let mut actions = Vec::new();

    draw_freshness(ui, app);

    // What happened sits at the bottom and the surface takes what is left.
    //
    // Laid out bottom-up *first*, which is the only ordering that works: a
    // scroll area told to fill the space takes all of it, so anything added
    // after it is pushed off the window. The record of what happened being the
    // thing that falls off is the worst possible choice, because it is where a
    // refusal appears.
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        draw_failures(ui, app);
        draw_notices(ui, app);

        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            // The surface scrolls; the bar above it does not. Without this a
            // long surface simply runs off the bottom — and egui culls
            // interaction outside the clip rect, so a control down there stops
            // responding rather than merely being out of view, which reads
            // exactly like it being broken.
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| draw_surface(ui, app, chrome, &mut actions));
        });
    });
    actions
}

fn draw_surface(ui: &mut Ui, app: &App, chrome: &mut Chrome, actions: &mut Vec<Action>) {
    match chrome.surface {
        Surface::Library => library::draw(ui, app, &mut chrome.library, actions),
        Surface::Spaces => spaces::draw(ui, app, &mut chrome.spaces, &mut chrome.quiet, actions),
        Surface::Members => members::draw(ui, app, &mut chrome.members, actions),
        Surface::Devices => devices::draw(ui, app, &mut chrome.devices, actions),
        Surface::Heads => heads::draw(ui, app, &mut chrome.heads, actions),
        // The engine read that supplies these is SUB-5. Until it lands the
        // model carries nothing, and the surface says "not measured" rather
        // than drawing zeroes — which is the whole contract this surface has.
        Surface::Storage => storage::draw(ui, app.storage(), app.transfers()),
        Surface::Diagnostics => diagnostics::draw(ui, app, &mut chrome.diagnostics, actions),
    }
}

/// Every surface, and the one action worth a key of its own, from the keyboard
/// alone.
///
/// Full keyboard operation is a release criterion, and tabbing to a control is
/// only half of it: a person who navigates by keyboard should not have to walk
/// the tab order to change surface. `Ctrl+1` through `Ctrl+7` are the surfaces
/// in the order they are drawn, which is the order the tabs are read out in.
fn keyboard(ui: &Ui, chrome: &mut Chrome, actions: &mut Vec<Action>) {
    const DIGITS: [Key; 7] = [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
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
    // Built oldest-of-the-three first, because the strip is laid out bottom-up
    // and would otherwise put the newest line furthest from the surface it is
    // about.
    let recent: Vec<&crate::model::Notice> = notices.take(3).collect();
    for notice in recent.into_iter().rev() {
        ui.label(RichText::new(&notice.said).color(theme::prose(ui)));
    }
    ui.separator();
}

fn draw_failures(ui: &mut Ui, app: &App) {
    let mut failures = app.failures().peekable();
    if failures.peek().is_none() {
        return;
    }
    for failure in failures {
        ui.label(
            RichText::new(format!("{}: {}", failure.what, failure.error)).color(theme::danger(ui)),
        );
    }
    ui.separator();
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
    dispatchable(
        ui,
        app,
        egui::Button::new(label),
        enabled,
        disabled_because,
        action(),
    )
}

/// The one act on a page, given the weight of one.
///
/// A page with a single obvious thing to do says so with size and colour rather
/// than with position alone — the reference's library page is built around
/// exactly one such control, and this is that control. Everything about
/// dispatch, including being disabled the instant it is clicked, is the same as
/// [`act`]: a primary action that queued four of itself would be worse than a
/// plain one, not better.
pub(crate) fn act_primary(
    ui: &mut Ui,
    app: &App,
    label: &str,
    enabled: bool,
    disabled_because: &str,
    action: impl FnOnce() -> Action,
) -> Option<Action> {
    let fill = theme::accent(ui);
    let button = egui::Button::new(
        RichText::new(label)
            .strong()
            .color(theme::legible(ui.visuals().text_color(), fill)),
    )
    .fill(fill)
    .min_size(egui::vec2(
        geometry::control::xl() * 2.5,
        geometry::control::xl(),
    ));
    dispatchable(ui, app, button, enabled, disabled_because, action())
}

/// What every dispatching control has in common, whatever it looks like.
fn dispatchable(
    ui: &mut Ui,
    app: &App,
    button: egui::Button<'_>,
    enabled: bool,
    disabled_because: &str,
    candidate: Action,
) -> Option<Action> {
    let waiting = app.is_in_flight(&candidate.key());
    let response = ui
        .add_enabled(enabled && !waiting, button)
        .on_disabled_hover_text(if waiting {
            "This is already under way."
        } else {
            disabled_because
        });
    response.clicked().then_some(candidate)
}
