//! Devices, and what may be done to them.
//!
//! The controls here encode the ownership boundary directly: a control that
//! cannot be used is disabled and says why, rather than being offered and
//! failing. A person learning a safety rule from an error message has already
//! tried the thing the rule exists to prevent.

use egui::{RichText, Ui};

use lait_workbench::{DeviceSnapshot, LifecycleState, ObservationState};

use crate::model::App;

use super::theme;

pub fn draw(ui: &mut Ui, app: &App) {
    ui.heading("Devices");

    if app.is_loading() {
        ui.label(RichText::new("Loading…").italics());
        return;
    }

    let devices = app.devices();
    if devices.is_empty() {
        ui.label("No devices are registered on this machine.");
        return;
    }

    for device in devices {
        draw_device(ui, device);
    }
}

fn draw_device(ui: &mut Ui, device: &DeviceSnapshot) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(&device.label).strong());
        ui.label(RichText::new(state_text(device.state)).color(theme::secondary(ui)));

        if device.observation.state == ObservationState::Degraded {
            ui.label(RichText::new("stale").color(theme::attention(ui)))
                .on_hover_text(
                    device
                        .observation
                        .error
                        .as_deref()
                        .unwrap_or("this device could not be sampled"),
                );
        }

        // Force-stop is offered only for a process this supervisor spawned.
        // Ownership is the safety boundary, and it is drawn as one.
        ui.add_enabled(device.owned, egui::Button::new("Force stop"))
            .on_disabled_hover_text(
                "This daemon was not started by this client, so it cannot be force-stopped.",
            );

        // Removal needs the device stopped. Disabled rather than refused,
        // because the refusal would arrive after the click that meant it.
        let removable = matches!(
            device.state,
            LifecycleState::Stopped | LifecycleState::Failed
        );
        ui.add_enabled(removable, egui::Button::new("Remove"))
            .on_disabled_hover_text("Stop this device before removing it.");
    });

    if let Some(image) = &device.image {
        // What it is *actually* running. A staged run may outlive the tree that
        // produced it, so the source path alone would be a claim about a file
        // that may no longer exist.
        ui.label(
            RichText::new(format!(
                "image {} — staged from {}",
                image.fingerprint, image.source_path
            ))
            .small()
            .color(theme::secondary(ui)),
        );
    }
}

const fn state_text(state: LifecycleState) -> &'static str {
    match state {
        LifecycleState::Stopped => "stopped",
        LifecycleState::Starting => "starting",
        LifecycleState::Running => "running",
        LifecycleState::Stopping => "stopping",
        LifecycleState::External => "external",
        LifecycleState::Failed => "failed",
    }
}
