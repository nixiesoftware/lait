//! Heads: the browser heads this client holds, and the MCP bindings it authors.
//!
//! The two halves are drawn apart because they are not the same kind of thing,
//! and a surface that listed them together would invite the one mistake this
//! whole module exists to prevent: offering to stop a process this client never
//! started and could not prove the identity of.
//!
//! A browser head is ours — spawned here, held here, stoppable here. An MCP head
//! is the agent harness's child; what this client has is the *binding* that
//! points that harness at lait, and the honest thing to draw is the binding.

use egui::{RichText, Ui};

use lait::install::{Client as AgentClient, Scope};
use lait_workbench::{HeadKind, Ownership};

use crate::client::heads::{McpBinding, AGENT_CLIENTS};
use crate::model::App;
use crate::runtime::Action;

use super::{act, theme};

/// What is half-chosen on this surface.
#[derive(Debug)]
pub struct Draft {
    pub agent_client: AgentClient,
    /// `None` takes the agent client's own default scope.
    pub scope: Option<Scope>,
    pub name: String,
    pub agent: String,
    pub no_agent: bool,
    pub project: String,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            agent_client: AgentClient::Claude,
            scope: None,
            // The name lait's own tooling expects. Editable, because a person
            // running two builds needs to tell them apart.
            name: "lait".into(),
            agent: String::new(),
            no_agent: false,
            project: String::new(),
        }
    }
}

pub fn draw(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    draw_browser_heads(ui, app, actions);
    ui.separator();
    draw_mcp(ui, app, draft, actions);
}

fn draw_browser_heads(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    ui.heading("Browser heads");
    ui.label(
        RichText::new(
            "A head is what a World's page is served from. Open starts one for you; \
             this is where you can see it and stop it.",
        )
        .color(theme::secondary(ui)),
    );

    let heads: Vec<_> = app
        .heads()
        .iter()
        .filter(|head| head.kind == HeadKind::Browser)
        .collect();
    if heads.is_empty() {
        ui.label("No head is running.");
    }

    for head in heads {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&head.id).strong());
            ui.label(RichText::new(&head.identity).color(theme::secondary(ui)));
            match head.device.as_deref() {
                Some(device) => {
                    ui.label(RichText::new(format!("device {device}")).color(theme::secondary(ui)));
                }
                None => {
                    ui.label(RichText::new("your own daemon").color(theme::secondary(ui)));
                }
            }
            // The address is deliberately *not* drawn. It carries this run's
            // credential, and a token on screen is a token in a screenshot, in a
            // support ticket, and in whatever recorded the window.
            let id = head.id.clone();
            let owned = head.ownership == Ownership::Owned;
            if let Some(action) = act(
                ui,
                app,
                "Stop",
                owned,
                "This head was not started by this client, so it cannot be stopped here.",
                || Action::StopHead(id.clone()),
            ) {
                actions.push(action);
            }
        });
    }

    if let Some(action) = act(ui, app, "Start a head", true, "", || Action::StartHead) {
        actions.push(action);
    }
}

fn draw_mcp(ui: &mut Ui, app: &App, draft: &mut Draft, actions: &mut Vec<Action>) {
    ui.heading("MCP bindings");
    ui.label(
        RichText::new(
            "An MCP head is the agent harness's own child, so this client can never hold \
             it. What it can do is write the binding that points the harness at lait.",
        )
        .color(theme::secondary(ui)),
    );
    // Said plainly, because the alternative is somebody looking for an Orbit
    // picker that does not exist and concluding the surface is unfinished.
    ui.label(
        RichText::new(
            "The entry resolves `lait` from PATH and finds its Orbit from the project \
             directory at run time. Neither is pinned: a pinned path goes stale when the \
             binary moves, and a captured home outlives the shell that set it.",
        )
        .color(theme::secondary(ui))
        .small(),
    );

    ui.horizontal(|ui| {
        ui.label("Agent");
        for (candidate, label) in AGENT_CLIENTS {
            ui.selectable_value(&mut draft.agent_client, candidate, label);
        }
    });
    ui.horizontal(|ui| {
        ui.label("Scope");
        ui.selectable_value(&mut draft.scope, None, "the agent's default");
        ui.selectable_value(&mut draft.scope, Some(Scope::Project), "this project");
        ui.selectable_value(&mut draft.scope, Some(Scope::User), "this user");
    });
    ui.horizontal(|ui| {
        ui.label("Server name");
        ui.add(egui::TextEdit::singleline(&mut draft.name).hint_text("lait"));
    });
    ui.horizontal(|ui| {
        ui.label("Project");
        ui.add(
            egui::TextEdit::singleline(&mut draft.project)
                .hint_text("the directory a project-scoped config lands in"),
        );
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.no_agent, "Sign as me, not as an agent");
        ui.add_enabled(
            !draft.no_agent,
            egui::TextEdit::singleline(&mut draft.agent)
                .hint_text("agent identity (blank derives one)"),
        )
        .on_disabled_hover_text("This binding signs its work as you.");
    });

    let binding = McpBinding {
        client: draft.agent_client,
        scope: draft.scope,
        name: draft.name.trim().to_owned(),
        agent: (!draft.agent.trim().is_empty()).then(|| draft.agent.trim().to_owned()),
        no_agent: draft.no_agent,
        project: draft.project.trim().to_owned(),
    };
    let ready = !binding.name.is_empty() && !binding.project.is_empty();

    ui.horizontal(|ui| {
        // Preview first, and to its left, because it is the one that touches
        // nothing. The file being edited is an agent's, and a person deserves
        // to see the entry before it merges into a config they did not write.
        if let Some(action) = act(
            ui,
            app,
            "Preview",
            ready,
            "A binding needs a server name and the project directory it belongs to.",
            || Action::InstallMcp {
                binding: Box::new(binding.clone()),
                preview: true,
            },
        ) {
            actions.push(action);
        }
        if let Some(action) = act(
            ui,
            app,
            "Write binding",
            ready,
            "A binding needs a server name and the project directory it belongs to.",
            || Action::InstallMcp {
                binding: Box::new(binding.clone()),
                preview: false,
            },
        ) {
            actions.push(action);
        }
    });

    if let Some(outcome) = app.mcp() {
        ui.separator();
        ui.label(
            RichText::new(if outcome.written {
                format!("Written to {}", outcome.path)
            } else {
                format!("Would be written to {}", outcome.path)
            })
            .strong(),
        );
        if outcome.replaced {
            ui.label(
                RichText::new("An entry under this name already existed and was replaced.")
                    .color(theme::attention(ui)),
            );
        }
        if let Some(agent) = &outcome.agent {
            ui.label(
                RichText::new(format!("Signs its work as '{agent}'.")).color(theme::secondary(ui)),
            );
        }
        if let Some(note) = &outcome.note {
            // Drawn rather than logged: "this entry shadows the bundled plugin"
            // is the whole reason the agent client has to be named.
            ui.label(RichText::new(note).color(theme::attention(ui)));
        }
        ui.label(RichText::new(&outcome.detail).monospace());
    }
}
