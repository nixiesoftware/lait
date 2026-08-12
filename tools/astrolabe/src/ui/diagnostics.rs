//! What the client can say about the running system.
//!
//! Four things, in the order somebody asks them: what this is, who it can see,
//! what has happened, and what a device wrote down.
//!
//! ## Real fields only
//!
//! Nothing here is inferred and nothing is synthesised to make the page look
//! populated. A field the backend does not answer is drawn as unanswered — the
//! same rule the Storage surface holds, for the same reason, because a topology
//! that invents an edge and a footprint that invents a byte are one defect
//! wearing two coats.
//!
//! ## Logs page, they do not tail
//!
//! The supervisor's log read is a bounded cursor, and this uses it as one. That
//! the renderer now lives in the same process makes an unbounded tail *easier*
//! to write and no less of a way to hold a log file in a frame loop. Following
//! is edge-triggered off the signal stream: the model counts how many times a
//! device's log has been reported to change, and this asks for one page each
//! time that number moves.

use egui::{RichText, Ui};

use lait_workbench::{ConnectionEventKind, LogLevel};

use crate::model::App;
use crate::runtime::Action;

use super::{act, theme};

/// How the timeline and the log are being looked at.
#[derive(Debug, Default)]
pub struct Draft {
    /// Whose log. `None` until a device is chosen — there is no sensible
    /// default, and picking the first would make "the log" mean a different
    /// device depending on what happens to be registered.
    pub device: Option<String>,
    /// Hide anything below this. `None` shows everything.
    pub floor: Option<LogLevel>,
    /// Whether a change to the log is followed. Paused keeps what is on screen
    /// exactly as it is, which is the whole point of pausing.
    pub following: bool,
    /// The log-change count this surface has already asked about.
    pub followed_at: u64,
}

pub fn draw(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    draw_environment(ui, app);
    ui.separator();
    draw_topology(ui, app);
    ui.separator();
    draw_timeline(ui, app, actions);
    ui.separator();
    draw_logs(ui, app, draft, actions);
}

/// What this is: the build, the identity, the managed root, and what this
/// supervisor is allowed to do.
fn draw_environment(ui: &mut Ui, app: &App) {
    ui.heading("This machine");

    match app.snapshot() {
        None => {
            ui.label(RichText::new("Loading…").italics());
        }
        Some(snapshot) => {
            fact(ui, "executable", &snapshot.environment.executable);
            fact(ui, "state root", &snapshot.environment.state_root);
            fact(
                ui,
                "supervisor pid",
                &snapshot.environment.server_pid.to_string(),
            );
            fact(ui, "devices", &app.devices().len().to_string());

            // Capabilities are drawn only when something is *off*. A list of
            // eight "yes" lines is noise; a missing one is the whole reason a
            // control elsewhere is disabled, and that is worth a line.
            let capabilities = &snapshot.capabilities;
            let withheld: Vec<&str> = [
                (capabilities.create_device, "add a device"),
                (capabilities.update_device, "rename a device"),
                (capabilities.remove_device, "remove a device"),
                (capabilities.delete_device_data, "delete a device's data"),
                (capabilities.start, "start"),
                (capabilities.stop, "stop"),
                (capabilities.restart, "restart"),
                (capabilities.force_stop_owned_process, "force-stop"),
            ]
            .into_iter()
            .filter_map(|(allowed, name)| (!allowed).then_some(name))
            .collect();
            if !withheld.is_empty() {
                ui.label(
                    RichText::new(format!("this build cannot: {}", withheld.join(", ")))
                        .color(theme::attention(ui)),
                );
            }
        }
    }

    if let Some(context) = app.context() {
        fact(ui, "build", &context.version);
        fact(ui, "identity", &context.identity_home);
        fact(ui, "Worlds this build hosts", &context.worlds.join(", "));
        fact(ui, "Orbits", &context.orbits.len().to_string());
    }
}

/// Who each device can see, and why it cannot see the rest.
fn draw_topology(ui: &mut Ui, app: &App) {
    ui.heading("Topology");
    let connections = app.connections();
    if connections.is_empty() {
        // Deliberately hedged. This surface cannot tell "no peers" from "no
        // device is up to have peers", and saying the stronger thing would be
        // the false-disconnection defect wearing a different hat.
        ui.label("No peers observed.");
    }
    for connection in connections {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&connection.peer_nick).strong());
            ui.label(RichText::new(&connection.state).color(theme::secondary(ui)));
            ui.label(
                RichText::new(if connection.online {
                    "online"
                } else {
                    "offline"
                })
                .color(theme::secondary(ui)),
            );
            if !connection.dialable {
                ui.label(RichText::new("not dialable").color(theme::attention(ui)));
            }
            if let Some(blocked) = &connection.blocked_by {
                // The reason, not a state. "Blocked" without it sends somebody
                // to read a log to learn what was already known here.
                ui.label(
                    RichText::new(format!("blocked by {blocked}")).color(theme::attention(ui)),
                );
            }
            if let Some(device) = &connection.target_device_id {
                // Correlated by the Station id a daemon reports for itself,
                // never by nickname — a nick is authored and is not unique.
                ui.label(RichText::new(format!("↔ {device}")).color(theme::secondary(ui)));
            }
            ui.label(
                RichText::new(&connection.space_id)
                    .small()
                    .color(theme::secondary(ui)),
            );
        });
    }

    let degraded: Vec<_> = app.degraded().collect();
    if !degraded.is_empty() {
        ui.label(RichText::new("Some of the above is out of date:").color(theme::attention(ui)));
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

/// What has happened, from the bounded history the supervisor keeps.
fn draw_timeline(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    ui.heading("Timeline");
    ui.horizontal(|ui| {
        if let Some(action) = act(ui, app, "Read events", true, "", || Action::ReadEvents {
            after: None,
        }) {
            actions.push(action);
        }
        if let Some(action) = act(ui, app, "Read transitions", true, "", || {
            Action::ReadTransitions { after: None }
        }) {
            actions.push(action);
        }
    });

    if let Some(page) = app.events() {
        // A history that has already discarded what came before says so. A page
        // that silently began at the oldest surviving revision would look like
        // the beginning of time.
        if page.dropped_before {
            ui.label(
                RichText::new("Earlier events have been discarded from the buffer.")
                    .color(theme::attention(ui)),
            );
        }
        for event in &page.events {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("#{}", event.revision))
                        .small()
                        .monospace(),
                );
                ui.label(RichText::new(format!("{:?}", event.kind)).color(theme::secondary(ui)));
                if let Some(device) = &event.device_id {
                    ui.label(RichText::new(device).color(theme::secondary(ui)));
                }
                ui.label(&event.message);
            });
        }
        if page.has_more {
            let after = page.next_revision;
            if let Some(action) = act(ui, app, "More events", true, "", || Action::ReadEvents {
                after: Some(after),
            }) {
                actions.push(action);
            }
        }
    }

    if let Some(page) = app.transitions() {
        if page.dropped_before {
            ui.label(
                RichText::new("Earlier transitions have been discarded from the buffer.")
                    .color(theme::attention(ui)),
            );
        }
        for event in &page.events {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("#{}", event.revision))
                        .small()
                        .monospace(),
                );
                ui.label(
                    RichText::new(transition_text(event.kind)).color(match event.kind {
                        ConnectionEventKind::Disconnected => theme::attention(ui),
                        _ => theme::secondary(ui),
                    }),
                );
                ui.label(RichText::new(&event.connection.peer_nick).strong());
                ui.label(RichText::new(&event.connection.state).color(theme::secondary(ui)));
            });
        }
    }
}

/// One device's log, a page at a time.
fn draw_logs(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("Logs");

    ui.horizontal(|ui| {
        ui.label("Device");
        for device in app.devices() {
            ui.selectable_value(&mut draft.device, Some(device.id.clone()), &device.label);
        }
    });

    let Some(device) = draft.device.clone() else {
        ui.label("Choose a device to read its log.");
        return;
    };

    ui.horizontal(|ui| {
        ui.label("Show");
        ui.selectable_value(&mut draft.floor, None, "everything");
        ui.selectable_value(&mut draft.floor, Some(LogLevel::Info), "info and above");
        ui.selectable_value(
            &mut draft.floor,
            Some(LogLevel::Warn),
            "warnings and errors",
        );
        ui.selectable_value(&mut draft.floor, Some(LogLevel::Error), "errors");

        ui.checkbox(&mut draft.following, "Follow");
        let cursor = app
            .logs()
            .filter(|page| page.device_id == device)
            .map(|page| page.next_cursor);
        let asked = act(ui, app, "Read now", true, "", || Action::ReadLogs {
            device: device.clone(),
            cursor,
        });
        if let Some(action) = asked {
            actions.push(action);
            draft.followed_at = app.log_changes(&device);
        }
    });

    // Edge-triggered: one read per reported change, and one to begin with.
    // Paused keeps what is on screen exactly as it is, which is what pausing is
    // for. The in-flight guard is what makes "one read" true rather than
    // aspirational — without it every frame between asking and being answered
    // asks again.
    let changes = app.log_changes(&device);
    let unread = app.logs().is_none_or(|page| page.device_id != device);
    let waiting = app.is_in_flight(&format!("logs:{device}"));
    if draft.following && !waiting && (unread || changes != draft.followed_at) {
        draft.followed_at = changes;
        let cursor = app
            .logs()
            .filter(|page| page.device_id == device)
            .map(|page| page.next_cursor);
        actions.push(Action::ReadLogs {
            device: device.clone(),
            cursor,
        });
    }

    let Some(page) = app.logs().filter(|page| page.device_id == device) else {
        ui.label("This device's log has not been read yet.");
        return;
    };

    if page.reset {
        // The file was rotated or truncated under us, so what was on screen is
        // not the beginning of this one. Said rather than smoothed over.
        ui.label(
            RichText::new("The log file was replaced; this page starts a new one.")
                .color(theme::attention(ui)),
        );
    }

    let mut shown = 0_usize;
    for entry in &page.entries {
        if !passes(entry.level, draft.floor) {
            continue;
        }
        shown = shown.saturating_add(1);
        ui.horizontal(|ui| {
            if let Some(timestamp) = &entry.timestamp {
                ui.label(RichText::new(timestamp).small().monospace());
            }
            ui.label(RichText::new(level_text(entry.level)).color(level_colour(ui, entry.level)));
            if let Some(target) = &entry.target {
                ui.label(RichText::new(target).small().color(theme::secondary(ui)));
            }
            ui.label(RichText::new(&entry.message).monospace());
            if entry.truncated {
                ui.label(
                    RichText::new("(truncated)")
                        .small()
                        .color(theme::attention(ui)),
                );
            }
        });
    }

    if shown == 0 && !page.entries.is_empty() {
        // The difference between "this device logged nothing" and "the filter
        // hid all of it" is one a person cannot see, and only one of them means
        // something is wrong.
        ui.label(
            RichText::new(format!(
                "{} line(s) on this page are below the level you chose.",
                page.entries.len()
            ))
            .color(theme::secondary(ui)),
        );
    }
    if page.entries.is_empty() {
        ui.label("Nothing in this page of the log.");
    }
}

/// Whether a line survives the floor.
///
/// `Unknown` always survives. A line whose level could not be parsed is a line
/// this filter has no opinion about, and hiding it would make a filter into a
/// way to lose the one message that did not fit the format.
const fn passes(level: LogLevel, floor: Option<LogLevel>) -> bool {
    match floor {
        None => true,
        Some(floor) => matches!(level, LogLevel::Unknown) || rank(level) >= rank(floor),
    }
}

const fn rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Info => 2,
        LogLevel::Warn => 3,
        LogLevel::Error => 4,
        // Never compared — `passes` admits it before rank is consulted.
        LogLevel::Unknown => 0,
    }
}

const fn level_text(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
        LogLevel::Unknown => "?",
    }
}

fn level_colour(ui: &Ui, level: LogLevel) -> egui::Color32 {
    match level {
        LogLevel::Error => theme::danger(ui),
        LogLevel::Warn => theme::attention(ui),
        _ => theme::secondary(ui),
    }
}

const fn transition_text(kind: ConnectionEventKind) -> &'static str {
    match kind {
        ConnectionEventKind::Connected => "connected",
        ConnectionEventKind::Changed => "changed",
        ConnectionEventKind::Disconnected => "disconnected",
    }
}

/// One named fact. Absent is drawn as absent — an empty value line says "this
/// was answered with nothing", which is different from the line not being there.
fn fact(ui: &mut Ui, name: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(name).color(theme::secondary(ui)));
        if value.trim().is_empty() {
            ui.label(
                RichText::new("not answered")
                    .italics()
                    .color(theme::attention(ui)),
            );
        } else {
            ui.label(RichText::new(value).monospace());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filter must not be a way to lose the one line that did not parse.
    #[test]
    fn a_level_filter_hides_what_is_below_it_and_never_what_it_cannot_rank() {
        assert!(passes(LogLevel::Error, Some(LogLevel::Warn)));
        assert!(passes(LogLevel::Warn, Some(LogLevel::Warn)));
        assert!(!passes(LogLevel::Info, Some(LogLevel::Warn)));
        assert!(!passes(LogLevel::Trace, Some(LogLevel::Info)));
        assert!(passes(LogLevel::Trace, None));
        assert!(
            passes(LogLevel::Unknown, Some(LogLevel::Error)),
            "a line whose level could not be read was hidden by a filter with no opinion about it"
        );
    }
}
