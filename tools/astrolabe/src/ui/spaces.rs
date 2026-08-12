//! Founding a Space, entering one from an invite, and this machine's consent.
//!
//! This surface exists because of a timing problem, not a taste one. All three
//! of these happen at the one moment when there is no Space id to put in a path
//! — and therefore when no World head exists to draw the page that would ask for
//! them. The flow this replaces lives in `viewer/src/ui/Welcome.tsx`, which is
//! the very page that cannot exist yet when it is needed.
//!
//! It stays useful afterwards. Founding a second Space, entering another from an
//! invite, and enrolling a new machine are all the same act at a later date, and
//! a surface that disappeared once the first Space existed would make them
//! unreachable.

use egui::{RichText, Ui};

use crate::model::App;
use crate::runtime::Action;

use super::{act, theme};

/// What is half-typed on this surface.
#[derive(Debug, Default)]
pub struct Draft {
    pub found_home: String,
    pub found_name: String,
    pub found_nick: String,
    pub invite: String,
    pub invite_home: String,
    pub invite_nick: String,
    pub consent: String,
    /// The Orbit whose registry entry is being forgotten, and the name typed so
    /// far. Forgetting touches no store, but it is still a thing a person can
    /// do by accident to the wrong row.
    pub forgetting: Option<String>,
    pub confirmation: String,
}

pub fn draw(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    // Where to offer to put a new store. The daemon answers this because a
    // client has no working directory worth defaulting to — and without it
    // every founding form starts with an empty path box.
    if draft.found_home.is_empty() {
        if let Some(context) = app.context() {
            draft.found_home = default_home(&context.spaces_root, "space");
        }
    }
    if draft.invite_home.is_empty() {
        if let Some(context) = app.context() {
            draft.invite_home = default_home(&context.spaces_root, "joined");
        }
    }

    draw_found(ui, app, draft, actions);
    ui.separator();
    draw_enter(ui, app, draft, actions);
    ui.separator();
    draw_consent(ui, app, draft, actions);
    ui.separator();
    draw_registry(ui, app, draft, actions);
}

fn draw_found(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("Found a Space");
    ui.label(
        RichText::new("Nothing is created implicitly. This is the call that ends a fresh install.")
            .color(theme::secondary(ui)),
    );
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut draft.found_name).hint_text("what to call it"));
    });
    ui.horizontal(|ui| {
        ui.label("Store");
        ui.add(egui::TextEdit::singleline(&mut draft.found_home).hint_text("a directory"));
    });
    ui.horizontal(|ui| {
        ui.label("Your nick");
        ui.add(egui::TextEdit::singleline(&mut draft.found_nick).hint_text("optional"));

        let home = draft.found_home.trim().to_owned();
        let name = draft.found_name.trim().to_owned();
        let nick = trimmed(&draft.found_nick);
        let ready = !home.is_empty() && !name.is_empty();
        if let Some(action) = act(
            ui,
            app,
            "Found",
            ready,
            "A new Space needs a name and a directory to live in.",
            || Action::SpaceFound {
                home: home.clone(),
                name: name.clone(),
                nick: nick.clone(),
            },
        ) {
            actions.push(action);
        }
    });
}

fn draw_enter(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("Enter a Space from an invite");
    ui.label(
        RichText::new(
            "An invite in hand must reach a converged Space without opening a browser. \
             This is that path.",
        )
        .color(theme::secondary(ui)),
    );
    ui.horizontal(|ui| {
        ui.label("Invite");
        ui.add(
            egui::TextEdit::singleline(&mut draft.invite).hint_text("a link, or its bare ticket"),
        );
    });
    ui.horizontal(|ui| {
        ui.label("Store");
        ui.add(egui::TextEdit::singleline(&mut draft.invite_home).hint_text("a directory"));
    });
    ui.horizontal(|ui| {
        ui.label("Your nick");
        ui.add(egui::TextEdit::singleline(&mut draft.invite_nick).hint_text("optional"));

        let link = draft.invite.trim().to_owned();
        let home = draft.invite_home.trim().to_owned();
        let nick = trimmed(&draft.invite_nick);
        let ready = !link.is_empty() && !home.is_empty();
        if let Some(action) = act(
            ui,
            app,
            "Enter",
            ready,
            "Entering a Space needs an invite and a directory to bootstrap into.",
            || Action::SpaceEnter {
                link: link.clone(),
                home: home.clone(),
                nick: nick.clone(),
            },
        ) {
            actions.push(action);
        }
    });
}

fn draw_consent(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("Enrol this machine");
    ui.label(
        RichText::new(
            "The one request that touches no store: this machine has no membership \
             anywhere yet, which is the whole point of enrolment. Signing produces a \
             blob to hand back to the device that invited it — it grants nothing here.",
        )
        .color(theme::secondary(ui)),
    );
    ui.horizontal(|ui| {
        ui.label("Device invite");
        ui.add(egui::TextEdit::singleline(&mut draft.consent).hint_text("<actor id> <space id>"));

        let token = draft.consent.trim().to_owned();
        if let Some(action) = act(
            ui,
            app,
            "Sign consent",
            !token.is_empty(),
            "Device consent needs the invite token from the device that has the Space.",
            || Action::DeviceConsent {
                token: token.clone(),
            },
        ) {
            actions.push(action);
        }
    });
}

fn draw_registry(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("Registered Orbits");
    let Some(context) = app.context() else {
        ui.label(RichText::new("Loading…").italics());
        return;
    };
    if context.orbits.is_empty() {
        ui.label("This identity has no Orbits registered.");
        return;
    }

    for orbit in &context.orbits {
        ui.horizontal(|ui| {
            // Advisory, and drawn as advisory. The display name is owned by a
            // World today (SUB-1), so the registry's copy may lag a rename.
            if orbit.name.trim().is_empty() {
                ui.label(RichText::new("Unnamed").italics());
            } else {
                ui.label(RichText::new(&orbit.name).strong());
            }
            ui.label(RichText::new(&orbit.path).color(theme::secondary(ui)));

            let space = orbit.space.clone();
            if let Some(action) = act(ui, app, "Rebuild", true, "", || Action::OrbitRebuild {
                orbit: space.clone(),
            }) {
                actions.push(action);
            }
            if ui
                .button("Forget…")
                .on_hover_text("Remove this Orbit's registration. The store on disk is untouched.")
                .clicked()
            {
                draft.forgetting = Some(orbit.space.clone());
                draft.confirmation.clear();
            }
        });

        if draft.forgetting.as_deref() == Some(orbit.space.as_str()) {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Forget {}? Its store stays on disk and can be registered again.",
                        orbit.space
                    ))
                    .color(theme::attention(ui)),
                );
                let space = orbit.space.clone();
                if let Some(action) = act(ui, app, "Forget", true, "", || Action::OrbitForget {
                    space: space.clone(),
                }) {
                    actions.push(action);
                    draft.forgetting = None;
                }
                if ui.button("Keep").clicked() {
                    draft.forgetting = None;
                }
            });
        }
    }
}

/// A place to offer, given where the daemon says new stores go.
///
/// Offered, not chosen: it lands in an editable box, and a person who has an
/// opinion overwrites it. An empty box would make every founding start by
/// asking somebody to invent a path.
fn default_home(spaces_root: &str, leaf: &str) -> String {
    if spaces_root.trim().is_empty() {
        return String::new();
    }
    let separator = if spaces_root.contains('\\') {
        '\\'
    } else {
        '/'
    };
    format!(
        "{}{separator}{leaf}",
        spaces_root.trim_end_matches(['/', '\\'])
    )
}

fn trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offered path is under where the daemon said new stores go, and it is
    /// a suggestion rather than a fact — an empty root produces an empty box
    /// rather than a path rooted at nothing.
    #[test]
    fn a_suggested_store_sits_under_the_root_the_daemon_named() {
        assert_eq!(default_home("D:\\lait", "space"), "D:\\lait\\space");
        assert_eq!(
            default_home("/home/a/.lait", "space"),
            "/home/a/.lait/space"
        );
        assert_eq!(
            default_home("/home/a/.lait/", "space"),
            "/home/a/.lait/space"
        );
        assert_eq!(
            default_home("", "space"),
            "",
            "an unstated root became a path anyway"
        );
    }
}
