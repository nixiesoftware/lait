//! Issues-owned CLI namespace.

use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, to_value};
use world_interface::{CliInvocation, InterfaceError};

use crate::{encode_call, BoardPos, Filter, IssuesRequest};

pub const LOCAL_FOCUS: &str = "issues.focus";
pub const LOCAL_NEW_START: &str = "issues.new_start";
pub const LOCAL_WORK_STATE: &str = "issues.work_state";
pub const LOCAL_INBOX: &str = "issues.inbox";
pub const LOCAL_ATTACH: &str = "issues.attach";
pub const LOCAL_ATTACHMENT_GET: &str = "issues.attachment_get";
pub const LOCAL_ACCESS: &str = "issues.access";
pub const LOCAL_WORLD_UPGRADE: &str = "issues.world_upgrade";

pub fn command() -> Command {
    Command::new("issues")
        .about("Work with the bundled issue-tracker World.")
        .subcommand(new_command())
        .subcommand(
            leaf("start", "Claim an issue and start working.")
                .arg(pos_opt("reff", "Issue ref (default: current branch)."))
                .arg(flag("no_branch", "Skip the git branch step.").long("no-branch")),
        )
        .subcommand(
            leaf("done", "Finish an issue.")
                .arg(pos_opt("reff", "Issue ref (default: current branch).")),
        )
        .subcommand(
            leaf("stop", "Return an issue to backlog and unassign yourself.")
                .arg(pos_opt("reff", "Issue ref (default: current branch).")),
        )
        .subcommand(
            leaf("inbox", "Things addressed to you.")
                .arg(flag("clear", "Mark everything read after listing.")),
        )
        .subcommand(list_command())
        .subcommand(leaf("board", "Render a project's board.").arg(pos_opt(
            "project",
            "Project key (default: branch, configured, or sole project).",
        )))
        .subcommand(
            leaf("show", "Show a full issue.")
                .arg(pos_opt("reff", "Issue ref (default: current branch).")),
        )
        .subcommand(edit_command())
        .subcommand(move_command())
        .subcommand(
            leaf("assign", "Add or remove assignees.")
                .arg(pos("reff", "Issue ref."))
                .arg(pos_many("who", "Members to assign or unassign."))
                .arg(flag("remove", "Remove instead of add.")),
        )
        .subcommand(
            leaf("label", "Add or remove labels on an issue.")
                .arg(pos("reff", "Issue ref."))
                .arg(
                    pos_many("tokens", "Tokens such as +bug and -wip.")
                        .allow_hyphen_values(true)
                        .trailing_var_arg(true),
                ),
        )
        .subcommand(comment_command())
        .subcommand(
            leaf("react", "Toggle an emoji reaction on a comment.")
                .arg(pos("reff", "Issue ref."))
                .arg(pos("comment", "Comment id."))
                .arg(pos("emoji", "Emoji."))
                .arg(flag("remove", "Remove instead of add.")),
        )
        .subcommand(leaf(
            "world-upgrade",
            "Activate this build's reviewed IssuesWorld implementation.",
        ))
        .subcommand(ref_command("delete", "Delete (tombstone) an issue."))
        .subcommand(ref_command("restore", "Restore a deleted issue.").alias("undelete"))
        .subcommand(ref_command("history", "Show an issue's activity history."))
        .subcommand(link_command("link", "Link two issues."))
        .subcommand(link_command("unlink", "Remove an issue link."))
        .subcommand(
            leaf("parent", "Set or clear an issue's parent.")
                .arg(pos("reff", "Issue ref."))
                .arg(pos_opt("parent", "Parent issue ref."))
                .arg(flag("none", "Clear the parent.")),
        )
        .subcommand(
            ref_command("graph", "Show an issue's graph neighborhood.").aliases(["links", "deps"]),
        )
        .subcommand(projects_command())
        .subcommand(labels_command())
        .subcommand(leaf("follow", "Follow an issue's activity.").arg(pos("reff", "Issue ref.")))
        .subcommand(leaf("unfollow", "Stop following an issue.").arg(pos("reff", "Issue ref.")))
        .subcommand(milestone_command())
        .subcommand(cycle_command())
        .subcommand(initiative_command())
        .subcommand(team_command())
        .subcommand(triage_command())
        .subcommand(
            leaf("attach", "Attach a bounded file to an issue.")
                .arg(pos("reff", "Issue ref."))
                .arg(pos("file", "Path to the file."))
                .arg(option("comment", "Comment id to associate.")),
        )
        .subcommand(attachment_command())
        .subcommand(role_command())
        .subcommand(access_command())
        .subcommand(workflow_command())
        .subcommand(
            leaf("activity", "Space-wide recent transitions.")
                .arg(option("since", "Only events after this sequence.").default_value("0")),
        )
}

pub fn parse(matches: &ArgMatches) -> Result<CliInvocation, InterfaceError> {
    let (path, m) = leaf_matches(matches);
    let request = match path.as_slice() {
        [] => return local(LOCAL_FOCUS, json!({})),
        ["new"] => {
            let request = IssuesRequest::IssueNew {
                title: req(m, "title"),
                project: opt(m, "project"),
                project_hint: project_hint(m),
                assignees: many(m, "assignees"),
                priority: opt(m, "priority"),
                labels: many(m, "labels"),
                body: opt(m, "body"),
                due: opt(m, "due"),
                estimate: opt(m, "estimate")
                    .map(|value| {
                        value.parse::<u32>().map_err(|_| {
                            InterfaceError::new("--estimate takes a whole number of points")
                        })
                    })
                    .transpose()?,
            };
            if yes(m, "start") {
                return local(LOCAL_NEW_START, to_value(request).map_err(interface_error)?);
            }
            request
        }
        ["start"] | ["done"] | ["stop"] => {
            return local(
                LOCAL_WORK_STATE,
                json!({
                    "action": path[0],
                    "reff": resolve_reff(m)?,
                    "no_branch": yes(m, "no_branch"),
                }),
            );
        }
        ["inbox"] => {
            return local(LOCAL_INBOX, json!({ "clear": yes(m, "clear") }));
        }
        ["ls"] => IssuesRequest::List {
            project: opt(m, "project"),
            filter: Filter {
                mine: yes(m, "mine"),
                status: opt(m, "status"),
                label: opt(m, "label"),
                all: yes(m, "all"),
            },
        },
        ["board"] => IssuesRequest::Board {
            project: opt(m, "project"),
            project_hint: project_hint(m),
        },
        ["show"] => IssuesRequest::IssueView {
            reff: resolve_reff(m)?,
        },
        ["edit"] => IssuesRequest::IssueEdit {
            reff: resolve_reff(m)?,
            title: opt(m, "title"),
            status: opt(m, "status"),
            priority: opt(m, "priority"),
            description: opt(m, "body"),
            due: opt(m, "due"),
            estimate: opt(m, "estimate"),
        },
        ["move"] => IssuesRequest::IssueMove {
            reff: resolve_reff(m)?,
            project: opt(m, "project"),
            pos: if yes(m, "top") {
                Some(BoardPos::Top)
            } else if yes(m, "bottom") {
                Some(BoardPos::Bottom)
            } else if let Some(reff) = opt(m, "before") {
                Some(BoardPos::Before { reff })
            } else {
                opt(m, "after").map(|reff| BoardPos::After { reff })
            },
        },
        ["assign"] => IssuesRequest::Assign {
            reff: req(m, "reff"),
            who: many(m, "who"),
            add: !yes(m, "remove"),
        },
        ["label"] => {
            let (mut add, mut remove) = (Vec::new(), Vec::new());
            for token in many(m, "tokens") {
                if let Some(label) = token.strip_prefix('+') {
                    add.push(label.to_string());
                } else if let Some(label) = token.strip_prefix('-') {
                    remove.push(label.to_string());
                } else {
                    add.push(token);
                }
            }
            IssuesRequest::Label {
                reff: req(m, "reff"),
                add,
                remove,
            }
        }
        ["comment"] => parse_comment(m)?,
        ["react"] => IssuesRequest::React {
            reff: req(m, "reff"),
            comment: req(m, "comment"),
            emoji: req(m, "emoji"),
            on: !yes(m, "remove"),
        },
        ["world-upgrade"] => return local(LOCAL_WORLD_UPGRADE, json!({})),
        ["delete"] => IssuesRequest::IssueDelete {
            reff: resolve_reff(m)?,
        },
        ["restore"] => IssuesRequest::IssueRestore {
            reff: resolve_reff(m)?,
        },
        ["history"] => IssuesRequest::History {
            reff: resolve_reff(m)?,
        },
        ["link"] | ["unlink"] => {
            let reff = req(m, "reff");
            let first = req(m, "kind_or_target");
            let (kind, target) = match opt(m, "target") {
                Some(target) => (first, target),
                None => ("relates".into(), first),
            };
            if path[0] == "link" {
                IssuesRequest::IssueLink { reff, kind, target }
            } else {
                IssuesRequest::IssueUnlink { reff, kind, target }
            }
        }
        ["parent"] => {
            let reff = req(m, "reff");
            let parent = opt(m, "parent");
            if parent.is_none() && !yes(m, "none") {
                return Err(InterfaceError::new(format!(
                    "give a parent ref, or --none to clear: `lait issues parent {reff} <epic>`"
                )));
            }
            IssuesRequest::IssueParent { reff, parent }
        }
        ["graph"] => IssuesRequest::IssueGraph {
            reff: resolve_reff(m)?,
        },
        ["projects"] | ["projects", "ls"] => IssuesRequest::ProjectList,
        ["projects", "add"] => {
            let key = req(m, "key");
            IssuesRequest::ProjectNew {
                name: opt(m, "name").unwrap_or_else(|| title_case(&key)),
                key,
                color: opt(m, "color"),
            }
        }
        ["projects", "edit"] => IssuesRequest::ProjectEdit {
            project: req(m, "project"),
            name: opt(m, "name"),
            color: opt(m, "color"),
            description: opt(m, "description"),
            lead: opt(m, "lead"),
            start: opt(m, "start"),
            target: opt(m, "target"),
            archived: if yes(m, "archive") {
                Some(true)
            } else if yes(m, "unarchive") {
                Some(false)
            } else {
                None
            },
            team: opt(m, "team"),
        },
        ["projects", "delete"] => IssuesRequest::ProjectDelete {
            project: req(m, "project"),
        },
        ["projects", "update"] => IssuesRequest::ProjectUpdatePost {
            project: req(m, "project"),
            body: req(m, "body"),
            health: opt(m, "health"),
        },
        ["projects", "updates"] => IssuesRequest::ProjectUpdates {
            project: req(m, "project"),
        },
        ["labels"] | ["labels", "ls"] => IssuesRequest::LabelList,
        ["labels", "new"] => IssuesRequest::LabelNew {
            name: req(m, "name"),
            color: opt(m, "color"),
        },
        ["labels", "edit"] => IssuesRequest::LabelEdit {
            label: req(m, "label"),
            name: opt(m, "name"),
            color: opt(m, "color"),
        },
        ["labels", "rm"] => IssuesRequest::LabelDelete {
            label: req(m, "label"),
        },
        ["follow"] | ["unfollow"] => IssuesRequest::Follow {
            reff: req(m, "reff"),
            on: path[0] == "follow",
        },
        ["milestone", verb] => parse_milestone(verb, m)?,
        ["cycle", verb] => parse_cycle(verb, m)?,
        ["initiatives"] | ["initiatives", "ls"] => IssuesRequest::InitiativeList,
        ["initiatives", verb] => parse_initiative(verb, m)?,
        ["teams"] | ["teams", "ls"] => IssuesRequest::TeamList,
        ["teams", verb] => parse_team(verb, m)?,
        ["triage"] | ["triage", "ls"] => IssuesRequest::TriageList,
        ["triage", verb] => parse_triage(verb, m)?,
        ["attach"] => {
            return local(
                LOCAL_ATTACH,
                json!({
                    "reff": req(m, "reff"),
                    "file": req(m, "file"),
                    "comment": opt(m, "comment"),
                }),
            );
        }
        ["attachment", "get"] => {
            return local(
                LOCAL_ATTACHMENT_GET,
                json!({
                    "reff": req(m, "reff"),
                    "id": req(m, "id"),
                    "out": opt(m, "out"),
                }),
            );
        }
        ["attachment", "rm"] => IssuesRequest::Detach {
            reff: req(m, "reff"),
            id: req(m, "id"),
        },
        ["role"] | ["role", "ls"] => IssuesRequest::RoleList,
        ["role", verb] => parse_role(verb, m)?,
        ["access"] | ["access", "ls"] | ["access", "grant"] | ["access", "revoke"] => {
            return local(
                LOCAL_ACCESS,
                json!({
                    "action": path.last().copied().unwrap_or("ls"),
                    "actor": opt(m, "actor"),
                    "role": opt(m, "role"),
                    "project": opt(m, "project"),
                    "grant_id": opt(m, "grant_id"),
                }),
            );
        }
        ["workflow", verb] => parse_workflow(verb, m)?,
        ["activity"] => IssuesRequest::Activity {
            since: req(m, "since")
                .parse()
                .map_err(|_| InterfaceError::new("--since takes an integer sequence"))?,
        },
        _ => {
            return Err(InterfaceError::new(format!(
                "unsupported Issues command path: {}",
                path.join(" ")
            )));
        }
    };
    world(request)
}

fn world(request: IssuesRequest) -> Result<CliInvocation, InterfaceError> {
    encode_call(&request)
        .map(CliInvocation::World)
        .map_err(interface_error)
}

fn local(operation: &str, input: serde_json::Value) -> Result<CliInvocation, InterfaceError> {
    Ok(CliInvocation::Local {
        operation: operation.to_string(),
        input,
    })
}

fn parse_comment(m: &ArgMatches) -> Result<IssuesRequest, InterfaceError> {
    let (reff, body) = match (opt(m, "reff"), opt(m, "body")) {
        (Some(reff), Some(body)) => (Some(reff), Some(body)),
        (Some(only), None) if looks_like_issue_ref(&only) => (Some(only), None),
        (Some(only), None) => (None, Some(only)),
        _ => (None, None),
    };
    let reff = reff.or_else(infer_ref_from_git_branch).ok_or_else(|| {
        InterfaceError::new("no issue ref given, and none could be inferred from the git branch")
    })?;
    let body = match body {
        Some(body) => body,
        None => {
            use std::io::Read;
            let mut body = String::new();
            std::io::stdin().read_to_string(&mut body).ok();
            body.trim_end().to_string()
        }
    };
    if body.trim().is_empty() {
        return Err(InterfaceError::new(
            "no comment body; pass it as an argument or pipe it on stdin",
        ));
    }
    Ok(IssuesRequest::Comment {
        reff,
        body,
        reply_to: opt(m, "reply_to"),
    })
}

fn parse_milestone(verb: &str, m: &ArgMatches) -> Result<IssuesRequest, InterfaceError> {
    Ok(match verb {
        "ls" => IssuesRequest::MilestoneList {
            project: req(m, "project"),
        },
        "new" => IssuesRequest::MilestoneSet {
            project: req(m, "project"),
            milestone: None,
            name: Some(req(m, "name")),
            target: opt(m, "target"),
            remove: false,
        },
        "edit" => IssuesRequest::MilestoneSet {
            project: req(m, "project"),
            milestone: opt(m, "milestone"),
            name: opt(m, "name"),
            target: opt(m, "target"),
            remove: false,
        },
        "rm" => IssuesRequest::MilestoneSet {
            project: req(m, "project"),
            milestone: opt(m, "milestone"),
            name: None,
            target: None,
            remove: true,
        },
        "set" => IssuesRequest::IssueMilestone {
            reff: req(m, "reff"),
            milestone: opt(m, "milestone"),
        },
        _ => return Err(InterfaceError::new("unknown milestone command")),
    })
}

fn parse_cycle(verb: &str, m: &ArgMatches) -> Result<IssuesRequest, InterfaceError> {
    Ok(match verb {
        "ls" => IssuesRequest::CycleList {
            project: req(m, "project"),
        },
        "new" => IssuesRequest::CycleSet {
            project: req(m, "project"),
            cycle: None,
            name: Some(req(m, "name")),
            start: opt(m, "start"),
            end: opt(m, "end"),
            remove: false,
        },
        "edit" => IssuesRequest::CycleSet {
            project: req(m, "project"),
            cycle: opt(m, "cycle"),
            name: opt(m, "name"),
            start: opt(m, "start"),
            end: opt(m, "end"),
            remove: false,
        },
        "rm" => IssuesRequest::CycleSet {
            project: req(m, "project"),
            cycle: opt(m, "cycle"),
            name: None,
            start: None,
            end: None,
            remove: true,
        },
        "set" => IssuesRequest::IssueCycle {
            reff: req(m, "reff"),
            cycle: opt(m, "cycle"),
        },
        _ => return Err(InterfaceError::new("unknown cycle command")),
    })
}

fn parse_initiative(verb: &str, m: &ArgMatches) -> Result<IssuesRequest, InterfaceError> {
    Ok(match verb {
        "new" => IssuesRequest::InitiativeSet {
            initiative: None,
            name: Some(req(m, "name")),
            description: opt(m, "description"),
            owner: opt(m, "owner"),
            health: opt(m, "health"),
            target: opt(m, "target"),
            add_projects: vec![],
            remove_projects: vec![],
            remove: false,
        },
        "edit" => IssuesRequest::InitiativeSet {
            initiative: Some(req(m, "initiative")),
            name: opt(m, "name"),
            description: opt(m, "description"),
            owner: opt(m, "owner"),
            health: opt(m, "health"),
            target: opt(m, "target"),
            add_projects: csv(opt(m, "add")),
            remove_projects: csv(opt(m, "remove")),
            remove: false,
        },
        "rm" => IssuesRequest::InitiativeSet {
            initiative: Some(req(m, "initiative")),
            name: None,
            description: None,
            owner: None,
            health: None,
            target: None,
            add_projects: vec![],
            remove_projects: vec![],
            remove: true,
        },
        _ => return Err(InterfaceError::new("unknown initiative command")),
    })
}

fn parse_team(verb: &str, m: &ArgMatches) -> Result<IssuesRequest, InterfaceError> {
    Ok(match verb {
        "new" => IssuesRequest::TeamSet {
            team: None,
            name: Some(req(m, "name")),
            key: opt(m, "key"),
            icon: opt(m, "icon"),
            lead: opt(m, "lead"),
            add_members: vec![],
            remove_members: vec![],
            remove: false,
        },
        "edit" => IssuesRequest::TeamSet {
            team: Some(req(m, "team")),
            name: opt(m, "name"),
            key: None,
            icon: opt(m, "icon"),
            lead: opt(m, "lead"),
            add_members: vec![],
            remove_members: vec![],
            remove: false,
        },
        "add" => IssuesRequest::TeamSet {
            team: Some(req(m, "team")),
            name: None,
            key: None,
            icon: None,
            lead: None,
            add_members: many(m, "who"),
            remove_members: vec![],
            remove: false,
        },
        "remove" => IssuesRequest::TeamSet {
            team: Some(req(m, "team")),
            name: None,
            key: None,
            icon: None,
            lead: None,
            add_members: vec![],
            remove_members: many(m, "who"),
            remove: false,
        },
        "rm" => IssuesRequest::TeamSet {
            team: Some(req(m, "team")),
            name: None,
            key: None,
            icon: None,
            lead: None,
            add_members: vec![],
            remove_members: vec![],
            remove: true,
        },
        _ => return Err(InterfaceError::new("unknown team command")),
    })
}

fn parse_triage(verb: &str, m: &ArgMatches) -> Result<IssuesRequest, InterfaceError> {
    Ok(match verb {
        "submit" => IssuesRequest::TriageSubmit {
            title: req(m, "title"),
            body: opt(m, "body"),
            source: opt(m, "source"),
        },
        "accept" => IssuesRequest::TriageDecide {
            id: req(m, "id"),
            outcome: "accepted".into(),
            project: opt(m, "project"),
            target: None,
            note: opt(m, "note"),
        },
        "decline" => IssuesRequest::TriageDecide {
            id: req(m, "id"),
            outcome: "declined".into(),
            project: None,
            target: None,
            note: opt(m, "note"),
        },
        "dupe" => IssuesRequest::TriageDecide {
            id: req(m, "id"),
            outcome: "duplicate".into(),
            project: None,
            target: opt(m, "reff"),
            note: opt(m, "note"),
        },
        _ => return Err(InterfaceError::new("unknown triage command")),
    })
}

fn parse_role(verb: &str, m: &ArgMatches) -> Result<IssuesRequest, InterfaceError> {
    Ok(match verb {
        "show" => IssuesRequest::RoleShow {
            role: req(m, "role"),
        },
        "create" => IssuesRequest::RoleCreate {
            name: req(m, "name"),
            description: opt(m, "description"),
            project: opt(m, "project"),
            capabilities: many(m, "cap"),
        },
        "edit" => {
            let capabilities = many(m, "cap");
            IssuesRequest::RoleEdit {
                role: req(m, "role"),
                expect_revision: req(m, "expect-revision"),
                name: opt(m, "name"),
                description: opt(m, "description"),
                capabilities: (!capabilities.is_empty()).then_some(capabilities),
            }
        }
        "delete" => IssuesRequest::RoleDelete {
            role: req(m, "role"),
            expect_revision: req(m, "expect-revision"),
        },
        "resolve" => IssuesRequest::RoleResolve {
            role: req(m, "role"),
            expect_heads: many(m, "expect-head"),
            body_json: read_file(m, "file")?,
        },
        _ => return Err(InterfaceError::new("unknown role command")),
    })
}

fn parse_workflow(verb: &str, m: &ArgMatches) -> Result<IssuesRequest, InterfaceError> {
    Ok(match verb {
        "show" => IssuesRequest::WorkflowShow {
            project: req(m, "project"),
        },
        "validate" => IssuesRequest::WorkflowValidate {
            body_json: read_file(m, "file")?,
        },
        "set" => IssuesRequest::WorkflowSet {
            project: req(m, "project"),
            expect_heads: many(m, "expect-head"),
            body_json: read_file(m, "file")?,
        },
        _ => return Err(InterfaceError::new("unknown workflow command")),
    })
}

fn new_command() -> Command {
    leaf("new", "Create an issue; echoes the resolved handle.")
        .arg(pos("title", "Issue title."))
        .arg(project_option("Target project."))
        .arg(
            option_many("assignees", "Assign a member (repeatable).")
                .short('a')
                .long("assign"),
        )
        .arg(option("priority", "Priority.").short('P'))
        .arg(
            option_many("labels", "Attach a label (repeatable).")
                .short('l')
                .long("label"),
        )
        .arg(option("body", "Issue body.").short('b'))
        .arg(option("due", "Due date."))
        .arg(option("estimate", "Estimate points.").short('e'))
        .arg(flag("start", "Also start the new issue."))
}

fn list_command() -> Command {
    leaf("ls", "List issue rows.")
        .arg(project_option("Filter to a project."))
        .arg(flag("mine", "Only issues assigned to you."))
        .arg(option("status", "Filter by status."))
        .arg(option("label", "Filter by label."))
        .arg(flag("all", "Include done and archived."))
}

fn edit_command() -> Command {
    leaf("edit", "Patch an issue's fields.")
        .arg(pos_opt("reff", "Issue ref."))
        .arg(option("title", "New title."))
        .arg(option("status", "New status."))
        .arg(option("priority", "New priority."))
        .arg(option("body", "Replace the description.").short('b'))
        .arg(option("due", "Due date, or none."))
        .arg(option("estimate", "Estimate points, or none.").short('e'))
}

fn move_command() -> Command {
    leaf("move", "Set project and/or board position.")
        .arg(pos_opt("reff", "Issue ref."))
        .arg(project_option("Move to project."))
        .arg(flag("top", "Move to top.").conflicts_with_all(["bottom", "before", "after"]))
        .arg(flag("bottom", "Move to bottom.").conflicts_with_all(["top", "before", "after"]))
        .arg(
            option("before", "Place before this ref.")
                .conflicts_with_all(["top", "bottom", "after"]),
        )
        .arg(
            option("after", "Place after this ref.")
                .conflicts_with_all(["top", "bottom", "before"]),
        )
}

fn comment_command() -> Command {
    leaf("comment", "Append an immutable comment.")
        .arg(pos_opt(
            "reff",
            "Issue ref, or body when inferred from git.",
        ))
        .arg(pos_opt("body", "Comment body (omit to read stdin)."))
        .arg(option("reply_to", "Reply to a comment.").long("reply-to"))
}

fn ref_command(name: &'static str, about: &'static str) -> Command {
    leaf(name, about).arg(pos_opt("reff", "Issue ref (default: current branch)."))
}

fn link_command(name: &'static str, about: &'static str) -> Command {
    leaf(name, about)
        .arg(pos("reff", "Issue ref."))
        .arg(pos("kind_or_target", "Link kind, or target ref."))
        .arg(pos_opt("target", "Target issue ref."))
}

fn projects_command() -> Command {
    group("projects", "Manage the project registry.")
        .subcommand(
            leaf("add", "Create a project.")
                .alias("new")
                .arg(pos("key", "Short project key."))
                .arg(pos_opt("name", "Project name."))
                .arg(option("color", "Project color.")),
        )
        .subcommand(
            leaf("edit", "Edit a project's overview.")
                .arg(pos("project", "Project ref."))
                .arg(option("name", "New name."))
                .arg(option("color", "New color."))
                .arg(option("description", "Overview markdown."))
                .arg(option("lead", "Lead actor."))
                .arg(option("start", "Start date."))
                .arg(option("target", "Target date."))
                .arg(option("team", "Owning team."))
                .arg(flag("archive", "Archive the project.").conflicts_with("unarchive"))
                .arg(flag("unarchive", "Restore the project.")),
        )
        .subcommand(leaf("delete", "Delete an empty project.").arg(pos("project", "Project ref.")))
        .subcommand(leaf("ls", "List projects."))
        .subcommand(
            leaf("update", "Post a project status update.")
                .arg(pos("project", "Project ref."))
                .arg(pos("body", "Update text."))
                .arg(option("health", "Health label.")),
        )
        .subcommand(
            leaf("updates", "Show project status updates.").arg(pos("project", "Project ref.")),
        )
}

fn labels_command() -> Command {
    group("labels", "Manage the label registry.")
        .subcommand(
            leaf("new", "Create a label.")
                .arg(pos("name", "Label name."))
                .arg(option("color", "Label color.")),
        )
        .subcommand(
            leaf("edit", "Edit a label.")
                .arg(pos("label", "Label ref."))
                .arg(option("name", "New name."))
                .arg(option("color", "New color.")),
        )
        .subcommand(
            leaf("rm", "Delete a label.")
                .alias("delete")
                .arg(pos("label", "Label ref.")),
        )
        .subcommand(leaf("ls", "List labels."))
}

fn milestone_command() -> Command {
    group_required("milestone", "Manage project milestones.")
        .subcommand(leaf("ls", "List milestones.").arg(pos("project", "Project ref.")))
        .subcommand(
            leaf("new", "Create a milestone.")
                .arg(pos("project", "Project ref."))
                .arg(pos("name", "Milestone name."))
                .arg(option("target", "Target date.")),
        )
        .subcommand(
            leaf("edit", "Edit a milestone.")
                .arg(pos("project", "Project ref."))
                .arg(pos("milestone", "Milestone ref."))
                .arg(option("name", "New name."))
                .arg(option("target", "Target date.")),
        )
        .subcommand(
            leaf("rm", "Remove a milestone.")
                .arg(pos("project", "Project ref."))
                .arg(pos("milestone", "Milestone ref.")),
        )
        .subcommand(
            leaf("set", "Set an issue's milestone.")
                .arg(pos("reff", "Issue ref."))
                .arg(pos("milestone", "Milestone ref or none.")),
        )
}

fn cycle_command() -> Command {
    group_required("cycle", "Manage project cycles.")
        .subcommand(leaf("ls", "List cycles.").arg(pos("project", "Project ref.")))
        .subcommand(
            leaf("new", "Create a cycle.")
                .arg(pos("project", "Project ref."))
                .arg(pos("name", "Cycle name."))
                .arg(option("start", "Start date."))
                .arg(option("end", "End date.")),
        )
        .subcommand(
            leaf("edit", "Edit a cycle.")
                .arg(pos("project", "Project ref."))
                .arg(pos("cycle", "Cycle ref."))
                .arg(option("name", "New name."))
                .arg(option("start", "Start date."))
                .arg(option("end", "End date.")),
        )
        .subcommand(
            leaf("rm", "Remove a cycle.")
                .arg(pos("project", "Project ref."))
                .arg(pos("cycle", "Cycle ref.")),
        )
        .subcommand(
            leaf("set", "Schedule an issue into a cycle.")
                .arg(pos("reff", "Issue ref."))
                .arg(pos("cycle", "Cycle ref or none.")),
        )
}

fn initiative_command() -> Command {
    group("initiatives", "Manage initiatives.")
        .alias("initiative")
        .subcommand(
            leaf("new", "Create an initiative.")
                .arg(pos("name", "Initiative name."))
                .arg(option("description", "Description."))
                .arg(option("owner", "Owner actor."))
                .arg(option("health", "Health label."))
                .arg(option("target", "Target date.")),
        )
        .subcommand(
            leaf("edit", "Edit an initiative.")
                .arg(pos("initiative", "Initiative ref."))
                .arg(option("name", "New name."))
                .arg(option("description", "New description."))
                .arg(option("owner", "Owner actor."))
                .arg(option("health", "Health label."))
                .arg(option("target", "Target date."))
                .arg(option("add", "Comma-separated projects to add."))
                .arg(option("remove", "Comma-separated projects to remove.")),
        )
        .subcommand(leaf("rm", "Remove an initiative.").arg(pos("initiative", "Initiative ref.")))
        .subcommand(leaf("ls", "List initiatives."))
}

fn team_command() -> Command {
    group("teams", "Manage teams.")
        .alias("team")
        .subcommand(
            leaf("new", "Create a team.")
                .arg(pos("name", "Team name."))
                .arg(option("key", "Team key."))
                .arg(option("icon", "Team icon."))
                .arg(option("lead", "Lead actor.")),
        )
        .subcommand(
            leaf("edit", "Edit a team.")
                .arg(pos("team", "Team ref."))
                .arg(option("name", "New name."))
                .arg(option("icon", "New icon."))
                .arg(option("lead", "Lead actor.")),
        )
        .subcommand(
            leaf("add", "Add team members.")
                .arg(pos("team", "Team ref."))
                .arg(pos_many("who", "Actor keys.")),
        )
        .subcommand(
            leaf("remove", "Remove team members.")
                .arg(pos("team", "Team ref."))
                .arg(pos_many("who", "Actor keys.")),
        )
        .subcommand(leaf("rm", "Remove a team.").arg(pos("team", "Team ref.")))
        .subcommand(leaf("ls", "List teams."))
}

fn triage_command() -> Command {
    group("triage", "Manage the intake queue.")
        .subcommand(
            leaf("submit", "Submit work for triage.")
                .arg(pos("title", "Title."))
                .arg(option("body", "Details."))
                .arg(option("source", "Source.")),
        )
        .subcommand(
            leaf("accept", "Accept triage work into a project.")
                .arg(pos("id", "Triage id."))
                .arg(option("project", "Target project.").short('p'))
                .arg(option("note", "Review note.")),
        )
        .subcommand(
            leaf("decline", "Decline triage work.")
                .arg(pos("id", "Triage id."))
                .arg(option("note", "Review note.")),
        )
        .subcommand(
            leaf("dupe", "Mark triage work as a duplicate.")
                .arg(pos("id", "Triage id."))
                .arg(pos("reff", "Existing issue ref."))
                .arg(option("note", "Review note.")),
        )
        .subcommand(leaf("ls", "List triage work."))
}

fn attachment_command() -> Command {
    group_required("attachment", "Fetch or remove attachments.")
        .subcommand(
            leaf("get", "Save an attachment to disk.")
                .arg(pos("reff", "Issue ref."))
                .arg(pos("id", "Attachment id."))
                .arg(option("out", "Output path.")),
        )
        .subcommand(
            leaf("rm", "Remove an attachment.")
                .arg(pos("reff", "Issue ref."))
                .arg(pos("id", "Attachment id.")),
        )
}

fn role_command() -> Command {
    group("role", "Author product roles.")
        .subcommand(leaf("show", "Show one role.").arg(pos("role", "Role id.")))
        .subcommand(
            leaf("create", "Create a custom role.")
                .arg(pos("name", "Display name."))
                .arg(option_many("cap", "Capability id.").required(true))
                .arg(option("project", "Project scope.").short('p'))
                .arg(option("description", "Description.")),
        )
        .subcommand(
            leaf("edit", "Edit a custom role.")
                .arg(pos("role", "Role id."))
                .arg(option("expect-revision", "Expected revision.").required(true))
                .arg(option("name", "New name."))
                .arg(option("description", "New description."))
                .arg(option_many("cap", "Replacement capabilities.")),
        )
        .subcommand(
            leaf("delete", "Delete a custom role.")
                .arg(pos("role", "Role id."))
                .arg(option("expect-revision", "Expected revision.").required(true)),
        )
        .subcommand(
            leaf("resolve", "Resolve concurrent role heads.")
                .arg(pos("role", "Role id."))
                .arg(option_many("expect-head", "Expected head.").required(true))
                .arg(option("file", "Canonical JSON body.").required(true)),
        )
        .subcommand(leaf("ls", "List roles."))
}

fn access_command() -> Command {
    group("access", "Manage effective scoped assignments.")
        .subcommand(
            leaf("grant", "Grant a role to an actor.")
                .arg(pos("actor", "Actor id or petname."))
                .arg(option("role", "Role to grant.").required(true))
                .arg(option("project", "Project scope.").short('p')),
        )
        .subcommand(leaf("revoke", "Revoke an assignment.").arg(pos("grant_id", "Grant id.")))
        .subcommand(
            leaf("ls", "List effective assignments.").arg(option("actor", "Filter to one actor.")),
        )
}

fn workflow_command() -> Command {
    group_required("workflow", "Author deterministic workflow gates.")
        .subcommand(leaf("show", "Show a project's workflow.").arg(pos("project", "Project ref.")))
        .subcommand(
            leaf("validate", "Validate a workflow body.")
                .arg(option("file", "Canonical JSON body.").required(true)),
        )
        .subcommand(
            leaf("set", "Replace a project's workflow.")
                .arg(pos("project", "Project ref."))
                .arg(option_many("expect-head", "Expected head.").required(true))
                .arg(option("file", "Canonical JSON body.").required(true)),
        )
}

fn leaf(name: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about)
}

fn group(name: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about)
}

fn group_required(name: &'static str, about: &'static str) -> Command {
    group(name, about)
        .subcommand_required(true)
        .arg_required_else_help(true)
}

fn pos(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name)
        .help(help)
        .required(true)
        .action(ArgAction::Set)
}

fn pos_opt(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name).help(help).action(ArgAction::Set)
}

fn pos_many(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name)
        .help(help)
        .required(true)
        .action(ArgAction::Append)
        .num_args(1..)
}

fn option(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name).long(name).help(help).action(ArgAction::Set)
}

fn option_many(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .help(help)
        .action(ArgAction::Append)
}

fn flag(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .help(help)
        .action(ArgAction::SetTrue)
}

fn project_option(help: &'static str) -> Arg {
    option("project", help).short('p').value_name("PROJECT")
}

fn leaf_matches(matches: &ArgMatches) -> (Vec<&str>, &ArgMatches) {
    let mut path = Vec::new();
    let mut current = matches;
    while let Some((name, next)) = current.subcommand() {
        path.push(name);
        current = next;
    }
    (path, current)
}

fn opt(m: &ArgMatches, id: &str) -> Option<String> {
    m.try_get_one::<String>(id).ok().flatten().cloned()
}

fn req(m: &ArgMatches, id: &str) -> String {
    opt(m, id).unwrap_or_default()
}

fn many(m: &ArgMatches, id: &str) -> Vec<String> {
    m.try_get_many::<String>(id)
        .ok()
        .flatten()
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

fn yes(m: &ArgMatches, id: &str) -> bool {
    m.try_get_one::<bool>(id)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

fn csv(value: Option<String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn read_file(m: &ArgMatches, id: &str) -> Result<String, InterfaceError> {
    let path = req(m, id);
    std::fs::read_to_string(&path)
        .map_err(|error| InterfaceError::new(format!("read {path}: {error}")))
}

fn title_case(key: &str) -> String {
    let lower = key.to_ascii_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn project_hint(m: &ArgMatches) -> Option<String> {
    opt(m, "project")
        .is_none()
        .then(infer_project_key)?
        .or(None)
}

fn resolve_reff(m: &ArgMatches) -> Result<String, InterfaceError> {
    opt(m, "reff")
        .or_else(infer_ref_from_git_branch)
        .ok_or_else(|| {
            InterfaceError::new(
                "no issue ref given, and none could be inferred from the git branch",
            )
        })
}

fn infer_ref_from_git_branch() -> Option<String> {
    parse_key_n(&git_branch()?)
}

fn infer_project_key() -> Option<String> {
    parse_key_n(&git_branch()?).and_then(|reff| reff.split_once('-').map(|(key, _)| key.into()))
}

fn git_branch() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|branch| !branch.is_empty())
}

fn parse_key_n(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_alphabetic() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            if index < bytes.len() && bytes[index] == b'-' {
                let mut end = index + 1;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > index + 1 {
                    return Some(format!(
                        "{}-{}",
                        value[start..index].to_ascii_uppercase(),
                        &value[index + 1..end]
                    ));
                }
            }
        } else {
            index += 1;
        }
    }
    None
}

fn looks_like_issue_ref(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return false;
    }
    if value.starts_with("iss_") {
        return true;
    }
    match value.split_once('-') {
        Some((key, number)) => {
            !key.is_empty()
                && key.chars().all(|c| c.is_ascii_alphabetic())
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

fn interface_error(error: impl std::fmt::Display) -> InterfaceError {
    InterfaceError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(argv: &[&str]) -> CliInvocation {
        let matches = command().try_get_matches_from(argv).unwrap();
        parse(&matches).unwrap()
    }

    fn request(argv: &[&str]) -> IssuesRequest {
        match invocation(argv) {
            CliInvocation::World(call) => crate::decode_call(&call).unwrap(),
            CliInvocation::Local { operation, .. } => panic!("local invocation {operation}"),
        }
    }

    #[test]
    fn command_tree_is_valid() {
        command().debug_assert();
    }

    #[test]
    fn daily_commands_emit_typed_world_calls() {
        assert!(matches!(
            request(&["issues", "show", "ENG-1"]),
            IssuesRequest::IssueView { reff } if reff == "ENG-1"
        ));
        assert!(matches!(
            request(&["issues", "move", "ENG-1", "--before", "ENG-2"]),
            IssuesRequest::IssueMove {
                pos: Some(BoardPos::Before { reff }),
                ..
            } if reff == "ENG-2"
        ));
    }

    #[test]
    fn host_capabilities_are_explicit_local_operations() {
        assert!(matches!(
            invocation(&["issues", "inbox", "--clear"]),
            CliInvocation::Local { operation, .. } if operation == LOCAL_INBOX
        ));
        assert!(matches!(
            invocation(&["issues", "access", "ls"]),
            CliInvocation::Local { operation, .. } if operation == LOCAL_ACCESS
        ));
    }

    #[test]
    fn branch_reference_inference_is_product_owned() {
        assert_eq!(parse_key_n("eng-142-fix-login").as_deref(), Some("ENG-142"));
        assert_eq!(parse_key_n("feature/eng-142-x").as_deref(), Some("ENG-142"));
        assert_eq!(parse_key_n("release/v0.4.5"), None);
        assert!(looks_like_issue_ref("BEACON-7"));
        assert!(looks_like_issue_ref("iss_01JU74CAT"));
        assert!(!looks_like_issue_ref("fix ENG-7 now"));
    }
}
