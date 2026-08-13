//! Administering one Space: who is in it, which machines speak for you, and
//! what is standing between this node and a converged view.
//!
//! This is the pane that leaves the tracker's Settings page. A Space sits below
//! every World mounted in it, so a World drawing its membership is a layer
//! reaching upwards — and a Space carrying two Worlds would have two Settings
//! pages disagreeing about one roster.
//!
//! ## Authority is native, always
//!
//! Everything here grants, admits, approves or fences. The overlay a World's
//! page carries may *ask* for any of it and may perform none of it: it lives in
//! the DOM of a page a World serves, so a World can draw over it or imitate it.
//! This window is where it happens.

use egui::{RichText, Ui};

use crate::client::space::{SpaceOp, SpaceRef};
use crate::model::App;
use crate::runtime::Action;

use super::{act, theme};

/// What is half-typed on this surface.
#[derive(Debug)]
pub struct Draft {
    /// The Space being administered. `None` until one is chosen — and choosing
    /// is the act that makes reading it acceptable, because reading a Space
    /// means asking it, which places it.
    pub selected: Option<SpaceRef>,
    pub add_who: String,
    pub add_admin: bool,
    /// The member whose removal is being confirmed, and the name typed so far.
    pub removing: Option<String>,
    pub confirmation: String,
    pub alias_for: Option<String>,
    pub alias: String,
    pub invite_role: String,
    pub invite_reusable: bool,
    pub invite_hours: String,
    pub consent: String,
    pub custody_path: String,
    pub custody_passphrase: String,
    pub custody_force: bool,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            selected: None,
            add_who: String::new(),
            add_admin: false,
            removing: None,
            confirmation: String::new(),
            alias_for: None,
            alias: String::new(),
            // The engine's own default, spelled here so the form is not a blank
            // that quietly means something.
            invite_role: "contributor".into(),
            invite_reusable: false,
            invite_hours: "168".into(),
            consent: String::new(),
            custody_path: String::new(),
            custody_passphrase: String::new(),
            custody_force: false,
        }
    }
}

pub fn draw(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("Spaces on this device");
    ui.label(
        RichText::new(
            "A Space sits below every World mounted in it, so this is where it is \
             administered — not on a World's settings page.",
        )
        .color(theme::secondary(ui)),
    );

    let Some(context) = app.context() else {
        ui.label(RichText::new("Loading…").italics());
        return;
    };
    if context.orbits.is_empty() {
        ui.label("This identity has no Spaces yet. Found one, or enter one, on the Spaces tab.");
        return;
    }

    // Choosing is the act. Listing above stays passive; reading below places.
    ui.horizontal(|ui| {
        for orbit in &context.orbits {
            let candidate = SpaceRef {
                space: orbit.space.clone(),
                path: orbit.path.clone(),
            };
            let label = if orbit.name.trim().is_empty() {
                orbit.space.clone()
            } else {
                orbit.name.clone()
            };
            let chosen = draft.selected.as_ref() == Some(&candidate);
            if ui.selectable_label(chosen, label).clicked() && !chosen {
                draft.selected = Some(candidate.clone());
                actions.push(Action::ReadSpace(candidate));
            }
        }
    });

    let Some(at) = draft.selected.clone() else {
        ui.label(
            RichText::new("Choose a Space to administer it. Reading one asks it, which starts it.")
                .color(theme::secondary(ui)),
        );
        return;
    };

    ui.horizontal(|ui| {
        if let Some(action) = act(ui, app, "Re-read this Space", true, "", || {
            Action::ReadSpace(at.clone())
        }) {
            actions.push(action);
        }
    });

    let Some(view) = app.space().filter(|view| view.space == at.space) else {
        ui.label(RichText::new("Asking this Space…").italics());
        return;
    };

    draw_standing(ui, view);
    ui.separator();
    draw_diagnosis(ui, view);
    ui.separator();
    draw_members(ui, app, view, &at, draft, actions);
    ui.separator();
    draw_devices(ui, app, view, &at, draft, actions);
    ui.separator();
    draw_custody(ui, app, &at, draft, actions);
}

fn draw_standing(ui: &mut Ui, view: &crate::client::space::SpaceView) {
    ui.heading("You, here");
    ui.horizontal(|ui| {
        ui.label(RichText::new(&view.standing.role).strong());
        if !view.standing.member {
            // A joiner whose admission has not landed is not a member yet, and
            // a surface that drew "viewer" for it would be describing a
            // permission rather than a wait.
            ui.label(RichText::new("not a member of this Space yet").color(theme::attention(ui)));
        }
        if let Some(sponsor) = &view.standing.sponsor {
            ui.label(RichText::new(format!("sponsored by {sponsor}")).color(theme::secondary(ui)));
        }
    });
    if view.standing.partial_view {
        // The figures below are drawn from a view this node knows is
        // incomplete, and saying so is the difference between a short roster
        // and a wrong one.
        ui.label(
            RichText::new("This node's view of the Space is incomplete.")
                .color(theme::attention(ui)),
        );
        for line in &view.standing.divergence {
            ui.label(RichText::new(line).small().color(theme::attention(ui)));
        }
    }
}

fn draw_diagnosis(ui: &mut Ui, view: &crate::client::space::SpaceView) {
    ui.heading("Gates");
    let Some(diagnosis) = &view.diagnosis else {
        // Absent is not "every gate passes". Those are the two answers this
        // whole client spends its effort keeping apart.
        ui.label(
            RichText::new("This Space could not be diagnosed, so nothing below is known.")
                .color(theme::attention(ui)),
        );
        return;
    };
    ui.label(RichText::new(&diagnosis.summary).strong());
    for gate in &diagnosis.gates {
        let blocking = diagnosis.blocked_on.as_deref() == Some(gate.id.as_str());
        ui.horizontal(|ui| {
            ui.label(RichText::new(&gate.label).color(if blocking {
                theme::attention(ui)
            } else {
                theme::secondary(ui)
            }));
            ui.label(RichText::new(format!("{:?}", gate.state)).small());
            if !gate.detail.trim().is_empty() {
                ui.label(
                    RichText::new(&gate.detail)
                        .small()
                        .color(theme::secondary(ui)),
                );
            }
        });
    }
}

fn draw_members(
    ui: &mut Ui,
    app: &App,
    view: &crate::client::space::SpaceView,
    at: &SpaceRef,
    draft: &mut Draft,
    actions: &mut Vec<Action>,
) {
    ui.heading("Members");
    for member in &view.members {
        ui.horizontal(|ui| {
            // The local petname first when there is one, and the actor id
            // always. A name never selects an authority target — an ambiguous
            // surface offers the canonical id, and this one is never ambiguous
            // because the id is always on the row.
            if member.alias.trim().is_empty() {
                ui.label(RichText::new(&member.key).monospace());
            } else {
                ui.label(RichText::new(&member.alias).strong());
                ui.label(RichText::new(&member.key).small().monospace());
            }
            ui.label(RichText::new(&member.role).color(theme::secondary(ui)));
            if member.me {
                ui.label(RichText::new("you").color(theme::secondary(ui)));
            }
            if let Some(sponsor) = &member.sponsor {
                // An agent is a member, rendered. `sponsor` is what marks it as
                // sponsored, and its standing dies with that sponsor.
                ui.label(
                    RichText::new(format!("agent, sponsored by {sponsor}"))
                        .small()
                        .color(theme::secondary(ui)),
                );
            }

            let who = member.key.clone();
            let promote = member.role != "admin";
            let promoted = act(
                ui,
                app,
                if promote { "Promote" } else { "Demote" },
                !member.me,
                "You cannot change your own role here.",
                || Action::Administer {
                    at: at.clone(),
                    operation: Box::new(SpaceOp::MemberSetRole {
                        who: who.clone(),
                        admin: promote,
                    }),
                },
            );
            if let Some(action) = promoted {
                actions.push(action);
            }

            if ui
                .button("Name locally…")
                .on_hover_text(
                    "A petname you can read. Never broadcast, and never part of the signed ACL.",
                )
                .clicked()
            {
                draft.alias_for = Some(member.key.clone());
                draft.alias.clone_from(&member.alias);
            }

            if ui
                .add_enabled(!member.me, egui::Button::new("Remove…"))
                .on_disabled_hover_text("You cannot remove yourself from a Space here.")
                .clicked()
            {
                draft.removing = Some(member.key.clone());
                draft.confirmation.clear();
            }
        });

        if draft.alias_for.as_deref() == Some(member.key.as_str()) {
            ui.horizontal(|ui| {
                ui.label("Local name");
                ui.text_edit_singleline(&mut draft.alias);
                let who = member.key.clone();
                let name = draft.alias.trim().to_owned();
                let named = act(ui, app, "Save name", true, "", || Action::Administer {
                    at: at.clone(),
                    operation: Box::new(SpaceOp::MemberAlias {
                        who: who.clone(),
                        name: name.clone(),
                    }),
                });
                if let Some(action) = named {
                    actions.push(action);
                    draft.alias_for = None;
                }
                if ui.button("Cancel").clicked() {
                    draft.alias_for = None;
                }
            });
        }

        if draft.removing.as_deref() == Some(member.key.as_str()) {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Removing {} fences them out of this Space. Type their id to confirm:",
                        member.key
                    ))
                    .color(theme::danger(ui)),
                );
                ui.text_edit_singleline(&mut draft.confirmation);
                let who = member.key.clone();
                let named = draft.confirmation.trim() == member.key;
                let confirmed = act(
                    ui,
                    app,
                    "Remove member",
                    named,
                    "Type this member's id to confirm.",
                    || Action::Administer {
                        at: at.clone(),
                        operation: Box::new(SpaceOp::MemberRemove { who: who.clone() }),
                    },
                );
                if let Some(action) = confirmed {
                    actions.push(action);
                    draft.removing = None;
                    draft.confirmation.clear();
                }
                if ui.button("Keep").clicked() {
                    draft.removing = None;
                }
            });
        }
    }

    ui.horizontal(|ui| {
        ui.label("Add");
        ui.add(egui::TextEdit::singleline(&mut draft.add_who).hint_text("an actor id or did:key"));
        ui.checkbox(&mut draft.add_admin, "as an administrator");
        let who = draft.add_who.trim().to_owned();
        let added = act(
            ui,
            app,
            "Add member",
            !who.is_empty(),
            "Naming who is the whole of this request.",
            || Action::Administer {
                at: at.clone(),
                operation: Box::new(SpaceOp::MemberAdd {
                    who: who.clone(),
                    admin: draft.add_admin,
                }),
            },
        );
        if let Some(action) = added {
            actions.push(action);
            draft.add_who.clear();
        }
    });

    ui.horizontal(|ui| {
        ui.label("Invite");
        for role in ["viewer", "contributor", "administrator"] {
            ui.selectable_value(&mut draft.invite_role, role.to_owned(), role);
        }
        ui.checkbox(&mut draft.invite_reusable, "admits a team");
        ui.add(egui::TextEdit::singleline(&mut draft.invite_hours).desired_width(48.0));
        ui.label(RichText::new("hours").color(theme::secondary(ui)));

        // An unparseable lifetime is refused rather than silently defaulted: a
        // person who typed "seven" meant something, and a week is not a safe
        // guess about what.
        let hours = draft.invite_hours.trim().parse::<u64>().ok();
        let role = draft.invite_role.clone();
        let reusable = draft.invite_reusable;
        let minted = act(
            ui,
            app,
            "Mint an invite",
            hours.is_some_and(|hours| hours > 0),
            "An invite needs a lifetime in whole hours.",
            || Action::Administer {
                at: at.clone(),
                operation: Box::new(SpaceOp::Invite {
                    role: role.clone(),
                    reusable,
                    ttl_hours: hours.unwrap_or(168),
                }),
            },
        );
        if let Some(action) = minted {
            actions.push(action);
        }
    });
}

fn draw_devices(
    ui: &mut Ui,
    app: &App,
    view: &crate::client::space::SpaceView,
    at: &SpaceRef,
    draft: &mut Draft,
    actions: &mut Vec<Action>,
) {
    ui.heading("Your devices");
    ui.label(
        RichText::new(
            "The machines that speak for you in this Space. Revoking one rotates the \
             key to fence it.",
        )
        .color(theme::secondary(ui)),
    );

    for device in &view.devices {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&device.line).monospace());
            if device.is_this_device {
                ui.label(RichText::new("this machine").color(theme::secondary(ui)));
            }
            // Offered only for a line that resolved to a device id. The plane
            // answers this as prose, and a Revoke pointed at a sentence would
            // be a control that cannot work.
            let id = device.id.clone();
            let revoked = act(
                ui,
                app,
                "Revoke",
                id.is_some() && !device.is_this_device,
                if device.is_this_device {
                    "Revoking the machine you are using would fence you out of your own Space."
                } else {
                    "This line is not a device id."
                },
                || Action::Administer {
                    at: at.clone(),
                    operation: Box::new(SpaceOp::DeviceRevoke {
                        device: id.clone().unwrap_or_default(),
                    }),
                },
            );
            if let Some(action) = revoked {
                actions.push(action);
            }
        });
    }

    ui.horizontal(|ui| {
        if let Some(action) = act(ui, app, "Enrol another machine", true, "", || {
            Action::Administer {
                at: at.clone(),
                operation: Box::new(SpaceOp::DeviceInvite),
            }
        }) {
            actions.push(action);
        }
        ui.label(
            RichText::new("Hand the token to the other machine's Spaces tab.")
                .small()
                .color(theme::secondary(ui)),
        );
    });

    ui.horizontal(|ui| {
        ui.label("Consent from that machine");
        ui.add(egui::TextEdit::singleline(&mut draft.consent).hint_text("the blob it signed"));
        let consent = draft.consent.trim().to_owned();
        let added = act(
            ui,
            app,
            "Add the device",
            !consent.is_empty(),
            "Adding a device needs the consent blob it produced.",
            || Action::Administer {
                at: at.clone(),
                operation: Box::new(SpaceOp::DeviceAdd {
                    consent: consent.clone(),
                }),
            },
        );
        if let Some(action) = added {
            actions.push(action);
            draft.consent.clear();
        }
    });
}

fn draw_custody(
    ui: &mut Ui,
    app: &App,
    at: &SpaceRef,
    draft: &mut Draft,
    actions: &mut Vec<Action>,
) {
    ui.heading("Custody");
    ui.label(
        RichText::new(
            "Export this device's recovery share as a passphrase-protected package, or \
             restore one. An export is verified by reopening it before it is attested.",
        )
        .color(theme::secondary(ui)),
    );
    ui.horizontal(|ui| {
        ui.label("Package");
        ui.add(egui::TextEdit::singleline(&mut draft.custody_path).hint_text("a file path"));
        ui.label("Passphrase");
        ui.add(
            egui::TextEdit::singleline(&mut draft.custody_passphrase)
                .password(true)
                .hint_text("a passphrase"),
        );
    });
    ui.horizontal(|ui| {
        let path = draft.custody_path.trim().to_owned();
        let passphrase = draft.custody_passphrase.clone();
        let ready = !path.is_empty() && !passphrase.is_empty();
        let exported = act(
            ui,
            app,
            "Export",
            ready,
            "An export needs somewhere to write and a passphrase to protect it.",
            || Action::Administer {
                at: at.clone(),
                operation: Box::new(SpaceOp::CustodyExport {
                    path: path.clone(),
                    passphrase: passphrase.clone(),
                }),
            },
        );
        if let Some(action) = exported {
            actions.push(action);
        }

        // Overwriting a readable share is refused unless it is asked for, and
        // the asking is a separate tick rather than a flag inside the button.
        ui.checkbox(&mut draft.custody_force, "replace a readable share");
        let force = draft.custody_force;
        let imported = act(
            ui,
            app,
            "Restore",
            ready,
            "A restore needs the package and its passphrase.",
            || Action::Administer {
                at: at.clone(),
                operation: Box::new(SpaceOp::CustodyImport {
                    path: path.clone(),
                    passphrase: passphrase.clone(),
                    force,
                }),
            },
        );
        if let Some(action) = imported {
            actions.push(action);
        }
    });
}
