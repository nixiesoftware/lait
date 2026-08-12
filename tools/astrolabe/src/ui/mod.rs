//! The surfaces. They hold no logic of their own.
//!
//! Every fact drawn here came from the core as a snapshot or a signal, and
//! every action is a call into [`crate::client`]. A surface never computes a
//! figure, never smooths one, and never invents one to look populated.
//!
//! ## Library-first
//!
//! The front page is the Library — the Orbits this device serves and the Worlds
//! mounted in them. Devices is a peer, not the entry: a person with one identity
//! and several Spaces must never open onto a process inventory.

pub mod devices;
pub mod library;
pub mod storage;
pub mod theme;

use egui::{Align, Layout, RichText, Ui};

use crate::model::{App, StaleReason};

/// The surfaces a person can be looking at.
///
/// Ordered as they are drawn. `Library` is first because it is the front page,
/// and that ordering is a product decision rather than an alphabetical accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Surface {
    #[default]
    Library,
    Devices,
    Storage,
    Diagnostics,
}

impl Surface {
    pub const ALL: [Self; 4] = [
        Self::Library,
        Self::Devices,
        Self::Storage,
        Self::Diagnostics,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Devices => "Devices",
            Self::Storage => "Storage",
            Self::Diagnostics => "Diagnostics",
        }
    }
}

/// Draw the whole client for one frame.
pub fn draw(ui: &mut Ui, app: &App, surface: &mut Surface) {
    ui.horizontal(|ui| {
        for candidate in Surface::ALL {
            // `selectable_value` carries the selected state into the semantic
            // tree, so a screen reader hears which surface is current rather
            // than only that three buttons exist.
            ui.selectable_value(surface, candidate, candidate.title());
        }
    });
    ui.separator();

    draw_freshness(ui, app);

    match surface {
        Surface::Library => library::draw(ui, app),
        Surface::Devices => devices::draw(ui, app),
        // The engine read that supplies these is SUB-5. Until it lands the
        // model carries nothing, and the surface says "not measured" rather
        // than drawing zeroes — which is the whole contract this surface has.
        Surface::Storage => storage::draw(ui, app.storage(), app.transfers()),
        Surface::Diagnostics => draw_diagnostics(ui, app),
    }

    draw_failures(ui, app);
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

fn draw_diagnostics(ui: &mut Ui, app: &App) {
    ui.heading("Connections");
    let connections = app.connections();
    if connections.is_empty() {
        // Deliberately hedged. This surface cannot tell "no peers" from "no
        // device is up to have peers", and saying the stronger thing would be
        // the false-disconnection defect wearing a different hat.
        ui.label("No peers observed.");
    }
    for connection in connections {
        ui.horizontal(|ui| {
            ui.label(&connection.peer_nick);
            ui.label(RichText::new(&connection.state).weak());
            if let Some(device) = &connection.target_device_id {
                ui.label(RichText::new(format!("↔ {device}")).weak());
            }
        });
    }

    let degraded: Vec<_> = app.degraded().collect();
    if !degraded.is_empty() {
        ui.separator();
        ui.heading("Degraded observation");
        for device in degraded {
            let since = device
                .observation
                .stale_since_ms
                .map_or_else(|| "unknown".to_owned(), |at| format!("{at} ms"));
            ui.label(
                RichText::new(format!(
                    "{} — figures unchanged since {since}: {}",
                    device.label,
                    device
                        .observation
                        .error
                        .as_deref()
                        .unwrap_or("sampling failed")
                ))
                .color(theme::attention(ui)),
            );
        }
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
