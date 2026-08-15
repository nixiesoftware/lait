//! Devices, and what may be done to them.
//!
//! The controls here encode the ownership boundary directly: a control that
//! cannot be used is disabled and says why, rather than being offered and
//! failing. A person learning a safety rule from an error message has already
//! tried the thing the rule exists to prevent.
//!
//! ## Removal and deletion are drawn as two things
//!
//! Because they are two things. Removal forgets a registration; deletion
//! destroys what the device holds. The supervisor refuses a deletion that is
//! not confirmed by name and not contained beneath the managed root, and this
//! surface asks for the name *before* the click rather than letting the refusal
//! be the first time somebody hears about it.

use egui::{RichText, Ui};

use lait_workbench::{DeviceSnapshot, LifecycleState, ObservationState};

use crate::model::App;
use crate::runtime::Action;

use super::{act, theme};

/// What is half-typed on this surface.
#[derive(Debug, Default)]
pub struct Draft {
    /// The device being added, if the form is open.
    pub new_id: String,
    pub new_label: String,
    /// The device whose deletion is being confirmed, and the name typed so far.
    ///
    /// One at a time, deliberately: a confirmation that stayed armed across
    /// rows would let the wrong one be destroyed by a click meant for another.
    pub deleting: Option<String>,
    pub confirmation: String,
    /// The device being renamed, and the label typed so far. One at a time for
    /// the same reason.
    pub renaming: Option<String>,
    pub label: String,
}

pub fn draw(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("Devices");
    ui.label(
        RichText::new(
            "The development fleet this client supervises. Your own daemon is not \
             one of these — it is the always-running service the Library reads.",
        )
        .color(theme::secondary(ui)),
    );

    if app.is_loading() {
        ui.label(RichText::new("Loading…").italics());
        return;
    }

    let devices = app.devices();
    if devices.is_empty() {
        ui.label("No devices are registered on this machine.");
    }

    for device in devices {
        draw_device(ui, app, device, draft, actions);
    }

    ui.separator();
    draw_add(ui, app, draft, actions);

    // Stops what this client spawned and leaves everything else running — the
    // same boundary the exit policy draws, reachable without leaving. Offered
    // only when there is something owned to stop, so it is never a control that
    // does nothing.
    let owned = app.devices().iter().filter(|device| device.owned).count();
    if let Some(action) = act(
        ui,
        app,
        "Stop everything this client started",
        owned > 0,
        "This client has not started anything.",
        || Action::StopAllOwned,
    ) {
        actions.push(action);
    }
}

fn draw_device(
    ui: &mut Ui,
    app: &App,
    device: &DeviceSnapshot,
    draft: &mut Draft,
    actions: &mut Vec<Action>,
) {
    let running = matches!(
        device.state,
        LifecycleState::Running | LifecycleState::Starting | LifecycleState::External
    );
    let removable = matches!(
        device.state,
        LifecycleState::Stopped | LifecycleState::Failed
    );

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

        let id = device.id.clone();
        for candidate in [
            act(
                ui,
                app,
                "Start",
                !running,
                "This device is already up.",
                || Action::StartDevice(id.clone()),
            ),
            act(
                ui,
                app,
                "Stop",
                running,
                "This device is not running.",
                || Action::StopDevice(id.clone()),
            ),
            act(
                ui,
                app,
                "Restart",
                device.owned,
                "This daemon was not started by this client.",
                || Action::RestartDevice(id.clone()),
            ),
            // Force-stop is offered only for a process this supervisor spawned.
            // Ownership is the safety boundary, and it is drawn as one.
            act(
                ui,
                app,
                "Force stop",
                device.owned,
                "This daemon was not started by this client, so it cannot be force-stopped.",
                || Action::ForceStopDevice(id.clone()),
            ),
            // Removal needs the device stopped. Disabled rather than refused,
            // because the refusal would arrive after the click that meant it.
            act(
                ui,
                app,
                "Remove",
                removable,
                "Stop this device before removing it.",
                || Action::RemoveDevice {
                    id: id.clone(),
                    delete_data: false,
                },
            ),
        ]
        .into_iter()
        .flatten()
        {
            actions.push(candidate);
        }

        if ui
            .button("Rename…")
            .on_hover_text("A label names this device to you; nothing resolves by it.")
            .clicked()
        {
            draft.renaming = Some(device.id.clone());
            draft.label.clone_from(&device.label);
        }

        // Deletion is a separate control that opens a separate step. It is not
        // a checkbox beside Remove, because a checkbox left ticked from the
        // last row is exactly how the wrong store gets destroyed.
        if ui
            .add_enabled(removable, egui::Button::new("Delete data…"))
            .on_disabled_hover_text("Stop this device before deleting what it holds.")
            .clicked()
        {
            draft.deleting = Some(device.id.clone());
            draft.confirmation.clear();
        }
    });

    if draft.renaming.as_deref() == Some(device.id.as_str()) {
        ui.horizontal(|ui| {
            ui.label("New label");
            ui.text_edit_singleline(&mut draft.label);
            let id = device.id.clone();
            let label = draft.label.trim().to_owned();
            let renamed = act(
                ui,
                app,
                "Rename",
                !label.is_empty() && label != device.label,
                "A device needs a label, and this one already has this one.",
                || Action::RenameDevice {
                    id: id.clone(),
                    label: label.clone(),
                },
            );
            if let Some(action) = renamed {
                actions.push(action);
                draft.renaming = None;
            }
            if ui.button("Cancel").clicked() {
                draft.renaming = None;
            }
        });
    }

    if draft.deleting.as_deref() == Some(device.id.as_str()) {
        draw_deletion(ui, app, device, draft, actions);
    }

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

/// The confirmation step, which asks for the device's own name.
///
/// Typing the name is not friction for its own sake: it is the same
/// confirmation the supervisor requires, asked where a person can still change
/// their mind. A dialog with a Yes button is dismissed by muscle memory; a name
/// has to be read off the row first.
fn draw_deletion(
    ui: &mut Ui,
    app: &App,
    device: &DeviceSnapshot,
    draft: &mut Draft,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "Deleting {} destroys everything under {}. Type its name to confirm:",
                device.id, device.home
            ))
            .color(theme::danger(ui)),
        );
        ui.text_edit_singleline(&mut draft.confirmation);

        let named = draft.confirmation.trim() == device.id;
        let id = device.id.clone();
        let confirmed = act(
            ui,
            app,
            "Delete permanently",
            named,
            "Type this device's name to confirm.",
            || Action::RemoveDevice {
                id: id.clone(),
                delete_data: true,
            },
        );
        if let Some(action) = confirmed {
            actions.push(action);
            draft.deleting = None;
            draft.confirmation.clear();
        }
        if ui.button("Cancel").clicked() {
            draft.deleting = None;
            draft.confirmation.clear();
        }
    });
}

fn draw_add(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label("Add a device");
        ui.add(egui::TextEdit::singleline(&mut draft.new_id).hint_text("id"));
        ui.add(egui::TextEdit::singleline(&mut draft.new_label).hint_text("label"));

        let id = draft.new_id.trim().to_owned();
        let label = draft.new_label.trim().to_owned();
        let named = !id.is_empty() && !label.is_empty();
        let added = act(
            ui,
            app,
            "Add",
            named,
            "A device needs an id and a label.",
            || Action::CreateDevice {
                id: id.clone(),
                label: label.clone(),
            },
        );
        if let Some(action) = added {
            actions.push(action);
            draft.new_id.clear();
            draft.new_label.clear();
        }
    });
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
