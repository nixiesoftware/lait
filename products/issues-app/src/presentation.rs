//! Issues-owned terminal and JSON presentation.

use std::fmt::Write as _;

use issues::dto::{
    BoardView, CommentAnchorDto, CommentAnchorState, GraphView, InboxEntry, IssueView, Priority,
    Row,
};
use serde_json::Value;
use world_interface::{Failure, Presentation, PresentationFailure, PresentationOptions};

use crate::{IssuesErrorKind, IssuesResponse};

mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const DIM: &str = "\x1b[2m";
    pub const BOLD: &str = "\x1b[1m";
    pub const RED: &str = "\x1b[31m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const CYAN: &str = "\x1b[36m";
}

pub fn present(value: Value, options: PresentationOptions) -> Result<Presentation, Failure> {
    let response: IssuesResponse = serde_json::from_value(value)
        .map_err(|error| Failure::new(format!("decode Issues response: {error}")))?;
    Ok(render(&response, options))
}

pub fn render(response: &IssuesResponse, options: PresentationOptions) -> Presentation {
    if options.json {
        let mut stdout = serde_json::to_string(response).unwrap_or_else(|_| "{}".into());
        stdout.push('\n');
        let (exit_code, failure) = failure(response);
        return Presentation {
            stdout,
            stderr: String::new(),
            exit_code,
            failure,
            failure_message: error_message(response),
        };
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    match response {
        IssuesResponse::Ok { message } => {
            line(&mut stdout, message.as_deref().unwrap_or("ok"));
        }
        IssuesResponse::Ref { reff } => line(&mut stdout, reff),
        IssuesResponse::Issue(view) => render_issue(&mut stdout, view, options.color),
        IssuesResponse::List { rows } => render_rows(&mut stdout, rows, options.color),
        IssuesResponse::Board(board) => render_board(&mut stdout, board, options.color),
        IssuesResponse::Graph(graph) => render_graph(&mut stdout, graph, options.color),
        IssuesResponse::Activity { events, .. } => {
            if events.is_empty() {
                line(
                    &mut stdout,
                    "(no activity yet — it fills as the Space moves: `lait issues new \"...\"`)",
                );
            }
            for event in events {
                let changes = if event.changes.is_empty() {
                    String::new()
                } else {
                    let changes = event
                        .changes
                        .iter()
                        .map(|change| {
                            format!(
                                "{} {}→{}",
                                change.field,
                                change.from.as_deref().unwrap_or("∅"),
                                change.to.as_deref().unwrap_or("∅")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("  {changes}")
                };
                let warning = if event.collision { " ⚠" } else { "" };
                line(
                    &mut stdout,
                    &format!(
                        "{} {} {}{}{}",
                        event.reff, event.actor_nick, event.kind, changes, warning
                    ),
                );
            }
        }
        IssuesResponse::Inbox { entries, unread } => {
            if entries.is_empty() {
                line(
                    &mut stdout,
                    "inbox zero — nothing addressed to you. the backlog is `lait issues ls`.",
                );
            } else {
                for (index, entry) in entries.iter().enumerate() {
                    let mark = if (index as u64) < *unread { "•" } else { " " };
                    let detail = if entry.detail.is_empty() {
                        String::new()
                    } else {
                        format!("  — {}", entry.detail)
                    };
                    line(
                        &mut stdout,
                        &format!(
                            "{} {}  {}  {}{}",
                            paint(options.color, ansi::CYAN, mark),
                            entry.reff,
                            inbox_line_verb(entry),
                            entry.title,
                            detail
                        ),
                    );
                }
                line(
                    &mut stdout,
                    &paint(
                        options.color,
                        ansi::DIM,
                        &format!("({unread} unread — `lait issues inbox --clear` to mark read)"),
                    ),
                );
            }
        }
        IssuesResponse::AccessPlan { assignments } => line(
            &mut stdout,
            &format!("resolved {} authority assignment(s)", assignments.len()),
        ),
        IssuesResponse::Projects { projects } => {
            if projects.is_empty() {
                line(
                    &mut stdout,
                    "(no projects — create one: `lait issues projects add KEY`)",
                );
                line(
                    &mut stdout,
                    &paint(
                        options.color,
                        ansi::DIM,
                        "  just joined? run `lait doctor` to check sync status",
                    ),
                );
            }
            for project in projects {
                line(
                    &mut stdout,
                    &format!("{:<6} {}  ({})", project.key, project.name, project.id),
                );
            }
        }
        IssuesResponse::Updates { updates } => {
            if updates.is_empty() {
                line(
                    &mut stdout,
                    "(no updates yet — post one: `lait issues projects update KEY \"…\"`)",
                );
            }
            for update in updates {
                let health = if update.health.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", update.health.replace('_', " "))
                };
                line(
                    &mut stdout,
                    &format!("{}{health}  {}", update.ts, update.body),
                );
            }
        }
        IssuesResponse::Labels { labels } => {
            if labels.is_empty() {
                line(&mut stdout, "(no labels)");
            }
            for label in labels {
                line(
                    &mut stdout,
                    &format!("{:<16} {}  ({})", label.name, label.color, label.id),
                );
            }
        }
        IssuesResponse::Milestones { milestones } => {
            if milestones.is_empty() {
                line(
                    &mut stdout,
                    "(no milestones — add one: `lait issues milestone new KEY \"…\"`)",
                );
            }
            for milestone in milestones {
                let target = milestone
                    .target_date
                    .map(|target| format!("  → {}", fmt_day(target)))
                    .unwrap_or_default();
                line(
                    &mut stdout,
                    &format!(
                        "{:<24} {}/{}{target}  ({})",
                        milestone.name, milestone.done, milestone.total, milestone.id
                    ),
                );
            }
        }
        IssuesResponse::Cycles { cycles } => {
            if cycles.is_empty() {
                line(
                    &mut stdout,
                    "(no cycles — add one: `lait issues cycle new KEY \"…\"`)",
                );
            }
            for cycle in cycles {
                let window = match (cycle.start, cycle.end) {
                    (0, 0) => String::new(),
                    (start, 0) => format!("  {} →", fmt_day(start)),
                    (0, end) => format!("  → {}", fmt_day(end)),
                    (start, end) => format!("  {} → {}", fmt_day(start), fmt_day(end)),
                };
                line(
                    &mut stdout,
                    &format!(
                        "{:<24} {}/{}{window}  ({})",
                        cycle.name, cycle.done, cycle.total, cycle.id
                    ),
                );
            }
        }
        IssuesResponse::Initiatives { initiatives } => {
            if initiatives.is_empty() {
                line(
                    &mut stdout,
                    "(no initiatives — add one: `lait issues initiative new \"…\"`)",
                );
            }
            for initiative in initiatives {
                let health = if initiative.health.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", initiative.health.replace('_', " "))
                };
                let projects = if initiative.projects.is_empty() {
                    "(no projects)".to_string()
                } else {
                    initiative.projects.join(", ")
                };
                line(
                    &mut stdout,
                    &format!(
                        "{:<24} {}/{}{health}  {}  ({})",
                        initiative.name, initiative.done, initiative.total, projects, initiative.id
                    ),
                );
            }
        }
        IssuesResponse::Teams { teams } => {
            if teams.is_empty() {
                line(
                    &mut stdout,
                    "(no teams — add one: `lait issues team new \"…\" --key T`)",
                );
            }
            for team in teams {
                let projects = if team.projects.is_empty() {
                    String::new()
                } else {
                    format!("  → {}", team.projects.join(", "))
                };
                line(
                    &mut stdout,
                    &format!(
                        "{:<8} {:<20} {} member(s){projects}  ({})",
                        team.key,
                        team.name,
                        team.members.len(),
                        team.id
                    ),
                );
            }
        }
        IssuesResponse::TriageItems { items } => {
            if items.is_empty() {
                line(
                    &mut stdout,
                    "(triage queue is empty — report with `lait issues triage submit \"…\"`)",
                );
            }
            for item in items {
                let state = if item.outcome.is_empty() {
                    "pending".to_string()
                } else if item.reff.is_empty() {
                    item.outcome.clone()
                } else {
                    format!("{} → {}", item.outcome, item.reff)
                };
                line(
                    &mut stdout,
                    &format!("{}  {:<10} {}", item.id, state, item.title),
                );
            }
        }
        IssuesResponse::Attachment { name, mime, .. } => line(
            &mut stdout,
            &format!("attachment {name} ({mime}) — use `lait issues attachment get` to save it"),
        ),
        IssuesResponse::Text { text } => line(&mut stdout, text),
        IssuesResponse::Error { message, .. } => {
            line(&mut stderr, &format!("error: {message}"));
        }
    }

    let (exit_code, failure) = failure(response);
    Presentation {
        stdout,
        stderr,
        exit_code,
        failure,
        failure_message: error_message(response),
    }
}

fn error_message(response: &IssuesResponse) -> Option<String> {
    match response {
        IssuesResponse::Error { message, .. } => Some(message.clone()),
        _ => None,
    }
}

fn failure(response: &IssuesResponse) -> (i32, Option<PresentationFailure>) {
    match response {
        IssuesResponse::Error {
            error_kind: IssuesErrorKind::NotFound,
            ..
        } => (2, Some(PresentationFailure::InvalidRequest)),
        IssuesResponse::Error {
            error_kind: IssuesErrorKind::Denied,
            ..
        } => (1, Some(PresentationFailure::InvalidRequest)),
        IssuesResponse::Error {
            error_kind: IssuesErrorKind::Error | IssuesErrorKind::Retry,
            ..
        } => (1, Some(PresentationFailure::Internal)),
        _ => (0, None),
    }
}

fn render_graph(output: &mut String, graph: &GraphView, color: bool) {
    let row_line = |row: &Row| {
        let handle = row.key_alias.as_deref().unwrap_or(&row.reff);
        format!("{handle}  {}  ({})", row.title, row.status)
    };
    line(output, &paint(color, ansi::BOLD, &graph.reff));
    if let Some(parent) = &graph.parent {
        line(output, &format!("  parent    {}", row_line(parent)));
    }
    for child in &graph.children {
        line(output, &format!("  child     {}", row_line(child)));
    }
    for link in &graph.links {
        let arrow = if link.direction == "out" {
            "→"
        } else {
            "←"
        };
        line(
            output,
            &format!("  {} {arrow}  {}", link.kind, row_line(&link.row)),
        );
    }
    if !graph.blocked_by.is_empty() {
        line(
            output,
            &paint(color, ansi::YELLOW, "  blocked by (open, transitive):"),
        );
        for blocker in &graph.blocked_by {
            line(output, &format!("    ⚠ {}", row_line(blocker)));
        }
    }
    if graph.parent.is_none() && graph.children.is_empty() && graph.links.is_empty() {
        line(
            output,
            "  (no relations — `lait issues link <ref> blocks <ref>` or `lait issues parent <ref> <epic>`)",
        );
    }
}

fn render_rows(output: &mut String, rows: &[Row], color: bool) {
    if rows.is_empty() {
        line(
            output,
            "(no issues here — file one: `lait issues new \"...\"`, or `lait issues ls --all` to include done)",
        );
        return;
    }
    for row in rows {
        let alias = row.key_alias.as_deref().unwrap_or(&row.reff);
        let assignees = if row.assignee_summary.is_empty() {
            String::new()
        } else {
            format!("  {}", row.assignee_summary)
        };
        let provisional = if row.provisional {
            paint(color, ansi::DIM, " (provisional)")
        } else {
            String::new()
        };
        line(
            output,
            &format!(
                "{} {} {:<12} {}{}{}",
                paint(color, ansi::BOLD, &format!("{alias:<10}")),
                priority_badge(row.priority, color),
                row.status,
                row.title,
                assignees,
                provisional
            ),
        );
    }
}

fn render_board(output: &mut String, board: &BoardView, color: bool) {
    line(
        output,
        &format!(
            "{} · {}",
            paint(color, ansi::BOLD, &board.project.key),
            board.project.name
        ),
    );
    for column in &board.columns {
        let header = format!("┌ {} ({}) ", column.state.name, column.rows.len());
        let _ = writeln!(output, "\n{}", paint(color, ansi::CYAN, &header));
        for row in &column.rows {
            let alias = row.key_alias.as_deref().unwrap_or(&row.reff);
            let assignees = if row.assignee_summary.is_empty() {
                String::new()
            } else {
                format!("  {}", row.assignee_summary)
            };
            line(
                output,
                &format!(
                    "│ {:<10} {} {}{}",
                    alias,
                    priority_badge(row.priority, color),
                    row.title,
                    assignees
                ),
            );
        }
    }
}

fn render_issue(output: &mut String, issue: &IssueView, color: bool) {
    let alias = issue.key_alias.as_deref().unwrap_or(&issue.reff);
    line(
        output,
        &format!(
            "{}  {}",
            paint(color, ansi::BOLD, alias),
            paint(color, ansi::BOLD, &issue.title)
        ),
    );
    line(output, &paint(color, ansi::DIM, &"─".repeat(60)));
    line(output, &format!("id:       {}", issue.reff));
    line(
        output,
        &format!("project:  {}", issue.project_key.as_deref().unwrap_or("?")),
    );
    line(output, &format!("status:   {}", issue.status));
    line(output, &format!("priority: {}", issue.priority.as_str()));
    if !issue.assignees.is_empty() {
        let names = issue
            .assignees
            .iter()
            .map(|actor| actor.short())
            .collect::<Vec<_>>()
            .join(", ");
        line(output, &format!("assignees: {names}"));
    }
    if !issue.label_names.is_empty() {
        line(
            output,
            &format!("labels:   {}", issue.label_names.join(", ")),
        );
    }
    if issue.provisional {
        line(output, "(provisional — issue body not yet synced)");
    }
    if !issue.description.is_empty() {
        let _ = writeln!(output, "\n{}", issue.description);
    }
    if !issue.comments.is_empty() {
        let _ = writeln!(output, "\n## Comments ({})", issue.comments.len());
        for comment in &issue.comments {
            let who = comment
                .author_nick
                .clone()
                .unwrap_or_else(|| comment.author.short());
            line(
                output,
                &format!("{} · {}  {}", who, comment.ts, comment.body),
            );
            if let Some(anchor) = &comment.anchor {
                line(output, &format!("  on {}", attachment(anchor)));
            }
        }
    }
    if !issue.corrupt_records.is_empty() {
        let _ = writeln!(
            output,
            "\n## Corrupt records ({})",
            issue.corrupt_records.len()
        );
        for record in &issue.corrupt_records {
            line(output, &format!("{} · {}", record.locus, record.reason));
        }
        line(
            output,
            "(these are stored records that do not conform to the schema; run with --json for the raw values)",
        );
    }
}

/// One comment's attachment, rendered so a lost position reads as a lost
/// position and not as a missing comment.
///
/// `Drifted` is worded about the span rather than about the text. It covers an
/// anchor older than what this replica retains and two ends that resolved out
/// of order, and in both of those the marked words are still on screen — so
/// "the text this marked is gone" would be a claim about the reader's document
/// made from a fact about ours.
fn attachment(anchor: &CommentAnchorDto) -> String {
    let field = &anchor.field;
    match anchor.state {
        CommentAnchorState::At { start, end } if end > start => {
            format!("{field} {start}..{end}")
        }
        CommentAnchorState::At { start, .. } => format!("{field} {start}"),
        CommentAnchorState::Drifted => format!("{field} (the span has no place in the text now)"),
        CommentAnchorState::Unresolved => format!("{field} (position unavailable)"),
    }
}

fn priority_badge(priority: Priority, color: bool) -> String {
    let badge = format!("·{}·", priority.badge());
    let code = match priority {
        Priority::Urgent => ansi::RED,
        Priority::High => ansi::YELLOW,
        Priority::Medium => ansi::CYAN,
        Priority::Low | Priority::None => ansi::DIM,
    };
    paint(color, code, &badge)
}

fn inbox_line_verb(entry: &InboxEntry) -> String {
    let who = entry.actor_nick.clone().unwrap_or_else(|| "someone".into());
    match entry.kind.as_str() {
        "assigned" => format!("{who} assigned you"),
        "comment" | "commented" => format!("{who} commented on"),
        "mentioned" => format!("{who} mentioned you on"),
        _ => format!("{who} moved"),
    }
}

fn fmt_day(timestamp: u64) -> String {
    let days = (timestamp / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

fn line(output: &mut String, value: &str) {
    let _ = writeln!(output, "{value}");
}

fn paint(enabled: bool, code: &str, value: &str) -> String {
    if enabled {
        format!("{code}{value}{}", ansi::RESET)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_errors_keep_typed_exit_semantics() {
        let rendered = render(
            &IssuesResponse::not_found("no issue"),
            PresentationOptions {
                json: true,
                color: false,
            },
        );
        assert_eq!(rendered.exit_code, 2);
        assert_eq!(rendered.failure, Some(PresentationFailure::InvalidRequest));
        assert!(rendered.stdout.contains("\"error_kind\":\"not_found\""));
    }

    #[test]
    fn human_rows_are_owned_and_rendered_by_the_product() {
        let rendered = render(
            &IssuesResponse::List { rows: Vec::new() },
            PresentationOptions {
                json: false,
                color: false,
            },
        );
        assert!(rendered.stdout.contains("no issues here"));
        assert!(rendered.stderr.is_empty());
    }

    #[test]
    fn priority_badges_are_owned_by_the_product_presenter() {
        assert_eq!(priority_badge(Priority::Urgent, false), "·U·");
        let colored = priority_badge(Priority::Urgent, true);
        assert!(colored.contains("·U·") && colored.contains('\u{1b}'));
    }

    fn attached(state: CommentAnchorState) -> issues::dto::CommentDto {
        issues::dto::CommentDto {
            author: issues::ids::ActorId::from_incept_hash(&"a".repeat(64)),
            author_nick: Some("ann".into()),
            ts: 1000,
            body: "this word is wrong".into(),
            id: Some("cmt_00000000000000000000000001".into()),
            parent: None,
            reactions: Vec::new(),
            anchor: Some(CommentAnchorDto {
                field: "description".into(),
                state,
            }),
        }
    }

    fn issue_with(comments: Vec<issues::dto::CommentDto>) -> IssueView {
        let ulid = issues::ids::SystemUlidSource;
        IssueView {
            schema_version: issues::dto::SCHEMA_VERSION,
            reff: "iss_1".into(),
            doc_id: issues::ids::DocId::mint(&ulid),
            space_id: issues::ids::SpaceId::mint(&ulid),
            project_id: issues::ids::ProjectId::mint(&ulid),
            project_key: Some("ENG".into()),
            key_alias: Some("ENG-1".into()),
            title: "fix login race".into(),
            description: "the quick brown fox".into(),
            status: "todo".into(),
            priority: Priority::High,
            assignees: Vec::new(),
            labels: Vec::new(),
            label_names: Vec::new(),
            comments,
            created_by: issues::ids::ActorId::from_incept_hash(&"a".repeat(64)),
            created_at: 1,
            due_date: None,
            estimate: None,
            followers: Vec::new(),
            milestone: None,
            cycle: None,
            attachments: Vec::new(),
            provisional: false,
            corrupt_records: Vec::new(),
        }
    }

    fn rendered_issue(comments: Vec<issues::dto::CommentDto>) -> String {
        render(
            &IssuesResponse::Issue(Box::new(issue_with(comments))),
            PresentationOptions {
                json: false,
                color: false,
            },
        )
        .stdout
    }

    /// A range-attached comment renders its attachment, and each state renders
    /// as the thing it means.
    #[test]
    fn an_attached_comment_renders_where_it_is_attached() {
        let out = rendered_issue(vec![attached(CommentAnchorState::At { start: 4, end: 9 })]);
        assert!(out.contains("on description 4..9"), "{out}");

        let out = rendered_issue(vec![attached(CommentAnchorState::At { start: 4, end: 4 })]);
        assert!(out.contains("on description 4"), "{out}");
        assert!(!out.contains("4..4"), "a caret is a position, not a span");

        // Nothing here may claim the reader's text was deleted: `Drifted` also
        // covers an anchor this replica can no longer place, and `Unresolved`
        // is the absence of an answer rather than a lost one.
        let out = rendered_issue(vec![attached(CommentAnchorState::Drifted)]);
        assert!(
            out.contains("on description (the span has no place"),
            "{out}"
        );

        let out = rendered_issue(vec![attached(CommentAnchorState::Unresolved)]);
        assert!(
            out.contains("on description (position unavailable)"),
            "{out}"
        );
    }

    /// An ordinary comment renders no attachment line at all.
    #[test]
    fn an_unattached_comment_renders_no_attachment() {
        let mut comment = attached(CommentAnchorState::Drifted);
        comment.anchor = None;
        let out = rendered_issue(vec![comment]);
        assert!(out.contains("this word is wrong"));
        assert!(!out.contains("on description"), "{out}");
    }
}
