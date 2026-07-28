//! Argv → `Request` parity guards for the programmatic-clap surface
//! (`src/cmdspec.rs`). The command tree is now data, not a `#[derive(Parser)]`
//! enum, and arg extraction is keyed by string inside each spec's closure — so a
//! renamed arg is a runtime, not compile-time, error. These tests pin the tricky
//! mappings (label +/- tokens, board-position flags, repeated/variadic args,
//! nested namespaces, defaults, and parse-level conflicts) that the derive used
//! to enforce.
//!
//! Comparison is by serde value so we assert the *whole* `Request`, tag and all,
//! without depending on its wire representation.

use issues_app::{BoardPos, Filter, IssuesRequest};
use lait::client_action::ClientPayload;
use lait::cmdspec::{build_cli, parse_to_dispatch, parse_to_request, specs, ParsedCommand};
use lait::control::Request;
use serde_json::to_value;

/// Assert an argv parses to exactly `expected`.
fn parses_to(argv: &[&str], expected: impl ParsedExpected) {
    expected.assert_parse(argv);
}

trait ParsedExpected {
    fn assert_parse(self, argv: &[&str]);
}

impl ParsedExpected for Request {
    fn assert_parse(self, argv: &[&str]) {
        let got = parse_to_request(argv).unwrap_or_else(|e| panic!("parse {argv:?}: {e}"));
        assert_eq!(to_value(got).unwrap(), to_value(self).unwrap());
    }
}

impl ParsedExpected for IssuesRequest {
    fn assert_parse(self, argv: &[&str]) {
        let got = parse_issues(argv).unwrap_or_else(|e| panic!("parse {argv:?}: {e}"));
        assert_eq!(to_value(got).unwrap(), to_value(self).unwrap());
    }
}

fn parse_issues(argv: &[&str]) -> anyhow::Result<IssuesRequest> {
    match parse_to_dispatch(argv)? {
        ParsedCommand::Action(action) => match action.payload() {
            ClientPayload::World(call) => issues_app::decode_call(call).map_err(anyhow::Error::msg),
            ClientPayload::Control(request) => {
                anyhow::bail!("expected Issues World call, got {request:?}")
            }
        },
        ParsedCommand::ProductLocal { operation, .. } => {
            anyhow::bail!("expected Issues World call, got local operation {operation}")
        }
        ParsedCommand::Special { name, .. } => {
            anyhow::bail!("expected Issues World call, got special command {name}")
        }
    }
}

#[test]
fn label_tokens_split_into_add_and_remove() {
    // `+bug` adds, `-wip` removes, a bare token adds — and `-wip` must survive as a
    // value (allow_hyphen_values), not be parsed as an unknown flag.
    parses_to(
        &["lait", "issues", "label", "ENG-1", "+bug", "-wip", "chore"],
        IssuesRequest::Label {
            reff: "ENG-1".into(),
            add: vec!["bug".into(), "chore".into()],
            remove: vec!["wip".into()],
        },
    );
}

#[test]
fn move_position_flags_map_to_boardpos() {
    parses_to(
        &["lait", "issues", "move", "ENG-1", "--top"],
        IssuesRequest::IssueMove {
            reff: "ENG-1".into(),
            project: None,
            pos: Some(BoardPos::Top),
        },
    );
    parses_to(
        &[
            "lait", "issues", "move", "ENG-1", "--before", "ENG-2", "-p", "ENG",
        ],
        IssuesRequest::IssueMove {
            reff: "ENG-1".into(),
            project: Some("ENG".into()),
            pos: Some(BoardPos::Before {
                reff: "ENG-2".into(),
            }),
        },
    );
}

#[test]
fn assign_collects_variadic_who_and_toggles_add() {
    parses_to(
        &[
            "lait", "issues", "assign", "ENG-1", "alice", "bob", "--remove",
        ],
        IssuesRequest::Assign {
            reff: "ENG-1".into(),
            who: vec!["alice".into(), "bob".into()],
            add: false,
        },
    );
    parses_to(
        &["lait", "issues", "assign", "ENG-1", "alice"],
        IssuesRequest::Assign {
            reff: "ENG-1".into(),
            who: vec!["alice".into()],
            add: true,
        },
    );
}

#[test]
fn new_collects_repeated_short_flags() {
    parses_to(
        &[
            "lait",
            "issues",
            "new",
            "Fix login",
            "-p",
            "ENG",
            "-a",
            "alice",
            "-a",
            "bob",
            "-l",
            "x",
            "-l",
            "y",
            "-P",
            "high",
            "-b",
            "details",
        ],
        IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: "Fix login".into(),
            project: Some("ENG".into()),
            project_hint: None,
            assignees: vec!["alice".into(), "bob".into()],
            priority: Some("high".into()),
            labels: vec!["x".into(), "y".into()],
            body: Some("details".into()),
        },
    );
}

#[test]
fn ls_filter_flags() {
    parses_to(
        &[
            "lait", "issues", "ls", "-p", "ENG", "--mine", "--status", "wip", "--all",
        ],
        IssuesRequest::List {
            project: Some("ENG".into()),
            filter: Filter {
                mine: true,
                status: Some("wip".into()),
                label: None,
                all: true,
            },
        },
    );
}

#[test]
fn activity_since_defaults_to_zero() {
    parses_to(
        &["lait", "issues", "activity"],
        IssuesRequest::Activity { since: 0 },
    );
    parses_to(
        &["lait", "issues", "activity", "--since", "42"],
        IssuesRequest::Activity { since: 42 },
    );
}

#[test]
fn comment_with_inline_body() {
    // (The stdin fallback path is intentionally not exercised — it would block.)
    parses_to(
        &["lait", "issues", "comment", "ENG-1", "looks good"],
        IssuesRequest::Comment {
            reply_to: None,
            reff: "ENG-1".into(),
            body: "looks good".into(),
        },
    );
}

#[test]
fn aliases_resolve_to_the_canonical_command() {
    // `verify` → `doctor`, `seed ls` → `remote ls`, `members alias` → `members name`.
    parses_to(
        &["lait", "verify"],
        Request::Diagnose {
            expected_space: None,
        },
    );
    parses_to(&["lait", "seed", "ls"], Request::SeedList);
    parses_to(
        &["lait", "members", "alias", "abc123", "Alice"],
        Request::MemberAlias {
            who: "abc123".into(),
            name: "Alice".into(),
        },
    );
}

#[test]
fn grouped_commands_bare_form_lists() {
    parses_to(&["lait", "issues", "projects"], IssuesRequest::ProjectList);
    parses_to(&["lait", "issues", "labels"], IssuesRequest::LabelList);
    parses_to(&["lait", "members"], Request::Members);
}

#[test]
fn space_metadata_commands_belong_to_the_issues_namespace() {
    parses_to(
        &["lait", "issues", "rename", "Moon Base"],
        IssuesRequest::SpaceRename {
            name: "Moon Base".into(),
        },
    );
    parses_to(
        &["lait", "issues", "describe", "Orbital work"],
        IssuesRequest::SpaceDescribe {
            description: "Orbital work".into(),
        },
    );

    assert!(parse_to_request(&["lait", "rename", "Moon Base"]).is_err());
    assert!(parse_to_request(&["lait", "describe", "Orbital work"]).is_err());
}

#[test]
fn members_add_reads_admin_and_local_name() {
    parses_to(
        &["lait", "members", "add", "abc", "--admin", "--as", "Alice"],
        Request::MemberAdd {
            who: "abc".into(),
            admin: true,
            as_name: Some("Alice".into()),
        },
    );
}

#[test]
fn board_positional_is_optional() {
    // Bare `lait issues board` parses — the daemon's choose-project chain (sole
    // project / `project.default` / branch hint) supplies the view project.
    // `project_hint` is environment-derived (git branch), so only `project` is
    // pinned here.
    let got =
        parse_issues(&["lait", "issues", "board"]).expect("bare `lait issues board` must parse");
    match got {
        IssuesRequest::Board { project, .. } => assert_eq!(project, None),
        other => panic!("expected Request::Board, got {other:?}"),
    }
    // With an explicit positional the hint is pinned None (an explicit project
    // always wins; no git subprocess runs).
    parses_to(
        &["lait", "issues", "board", "ENG"],
        IssuesRequest::Board {
            project: Some("ENG".into()),
            project_hint: None,
        },
    );
}

#[test]
fn explicit_project_pins_the_hint_to_none() {
    // Under an explicit `-p` the branch-derived project_hint must be None — the
    // daemon must never see a hint that could override an explicit choice.
    parses_to(
        &["lait", "issues", "new", "t", "-p", "ENG"],
        IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: "t".into(),
            project: Some("ENG".into()),
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: None,
        },
    );
}

#[test]
fn config_set_is_special_dispatch_not_a_request() {
    // `lait config set` is handled by a bespoke `app::run` arm (layered local
    // settings, no daemon round-trip) — parse_to_request must refuse it by
    // naming the special-dispatch leaf, not silently produce a Request.
    let err = parse_to_request(&["lait", "config", "set", "user.nick", "moon"])
        .expect_err("config set must not map to a Request");
    let msg = err.to_string();
    assert!(
        msg.contains("special-dispatch") && msg.contains("set"),
        "error should name the special-dispatch leaf, got: {msg}"
    );
}

#[test]
fn cli_tree_builds_and_validates() {
    // clap panics on a malformed tree (dup ids, bad positionals). Asserts the
    // whole registry — including the `--orbit` global and the `issues`/`config`
    // groups — assembles into a legal Command.
    build_cli(&specs()).debug_assert();
}

#[test]
fn invite_accepts_a_role_and_pass_tuning() {
    // The invite flags parse together: a role selection composes with the
    // reuse/expiry tuning (there is no approval flag — acceptance IS the
    // approval).
    let cli = build_cli(&specs());
    let res = cli.try_get_matches_from([
        "lait",
        "invite",
        "--role",
        "viewer",
        "--reusable",
        "--ttl-hours",
        "24",
    ]);
    assert!(res.is_ok(), "{res:?}");
}

#[test]
fn unknown_subcommand_is_a_usage_error() {
    assert!(parse_to_request(&["lait", "frobnicate"]).is_err());
}

#[test]
fn work_state_verbs_are_special_dispatch() {
    // start/done/stop are Special (branch creation + custom rendering live in
    // app/cli) — parse_to_request must refuse them by name, and the ref must
    // parse as an optional positional.
    for verb in ["start", "done", "stop"] {
        let err = parse_to_request(&["lait", "issues", verb, "ENG-1"])
            .expect_err("work-state verbs are special-dispatch");
        assert!(err.to_string().contains(verb), "{err}");
    }
    // --no-branch is a legal flag on start only.
    let cli = build_cli(&specs());
    assert!(cli
        .try_get_matches_from(["lait", "issues", "start", "ENG-1", "--no-branch"])
        .is_ok());
}

#[test]
fn inbox_parses_with_and_without_clear() {
    for (argv, clear) in [
        (&["lait", "issues", "inbox"][..], false),
        (&["lait", "issues", "inbox", "--clear"][..], true),
    ] {
        match parse_to_dispatch(argv).expect("parse Issues inbox") {
            ParsedCommand::ProductLocal { operation, input } => {
                assert_eq!(operation, issues_app::cli::LOCAL_INBOX);
                assert_eq!(input["clear"], clear);
            }
            _ => panic!("Issues inbox must remain a package-owned host call"),
        }
    }
}

#[test]
fn projects_add_is_key_first_with_defaulted_name() {
    parses_to(
        &["lait", "issues", "projects", "add", "OPS"],
        IssuesRequest::ProjectNew {
            name: "Ops".into(),
            key: "OPS".into(),
            color: None,
        },
    );
    parses_to(
        &["lait", "issues", "projects", "add", "OPS", "Operations"],
        IssuesRequest::ProjectNew {
            name: "Operations".into(),
            key: "OPS".into(),
            color: None,
        },
    );
    // `new` survives as an alias of the SAME shape.
    parses_to(
        &["lait", "issues", "projects", "new", "DSN", "Design"],
        IssuesRequest::ProjectNew {
            name: "Design".into(),
            key: "DSN".into(),
            color: None,
        },
    );
}

#[test]
fn orbital_navigation_uses_one_canonical_vocabulary() {
    // `orbits` names the local path-keyed catalog. Removed migration spellings
    // must remain errors so they cannot quietly become permanent API.
    let cli = build_cli(&specs());
    assert!(cli.clone().try_get_matches_from(["lait", "orbits"]).is_ok());
    for removed in ["orbit", "spaces", "workspaces"] {
        assert!(
            cli.clone().try_get_matches_from(["lait", removed]).is_err(),
            "{removed} must not parse"
        );
    }
    assert!(cli
        .clone()
        .try_get_matches_from(["lait", "--orbit", "demo", "issues", "ls"])
        .is_ok());
    for removed in ["--space", "--workspace", "-w"] {
        assert!(
            cli.clone()
                .try_get_matches_from(["lait", removed, "demo", "issues", "ls"])
                .is_err(),
            "{removed} must not parse"
        );
    }
}

#[test]
fn bare_invocation_parses_as_navigation_context() {
    // Bare `lait` (with global flags allowed) has no product command. app::run
    // renders the selected identity/Orbit/Space/World context.
    let cli = build_cli(&specs());
    let m = cli
        .clone()
        .try_get_matches_from(["lait"])
        .expect("bare lait parses");
    assert!(m.subcommand().is_none());
    assert!(cli.try_get_matches_from(["lait", "--json"]).is_ok());
}

#[test]
fn context_and_world_catalog_are_navigation_commands() {
    let cli = build_cli(&specs());
    assert!(cli
        .clone()
        .try_get_matches_from(["lait", "context"])
        .is_ok());
    assert!(cli.clone().try_get_matches_from(["lait", "worlds"]).is_ok());
    assert!(cli.try_get_matches_from(["lait", "world"]).is_err());
}

#[test]
fn issues_namespace_is_the_only_product_entry() {
    let expected = IssuesRequest::IssueView {
        reff: "ENG-1".into(),
    };
    parses_to(&["lait", "issues", "show", "ENG-1"], expected);

    // Product-owned groups may nest under the World namespace.
    parses_to(
        &["lait", "issues", "projects", "add", "OPS"],
        IssuesRequest::ProjectNew {
            name: "Ops".into(),
            key: "OPS".into(),
            color: None,
        },
    );

    let cli = build_cli(&specs());
    assert!(cli
        .clone()
        .try_get_matches_from(["lait", "show", "ENG-1"])
        .is_err());
    let root_names: Vec<_> = cli
        .get_subcommands()
        .map(|command| command.get_name())
        .collect();
    let issues = cli
        .get_subcommands()
        .find(|command| command.get_name() == "issues")
        .expect("visible Issues namespace");
    assert!(!issues.is_hide_set());
    let issue_names: Vec<_> = issues
        .get_subcommands()
        .map(|command| command.get_name())
        .collect();
    for product_command in [
        "new",
        "start",
        "done",
        "stop",
        "inbox",
        "ls",
        "show",
        "edit",
        "move",
        "assign",
        "label",
        "comment",
        "react",
        "world-upgrade",
        "delete",
        "restore",
        "link",
        "unlink",
        "parent",
        "graph",
        "history",
        "board",
        "projects",
        "labels",
        "follow",
        "unfollow",
        "milestone",
        "cycle",
        "initiatives",
        "teams",
        "triage",
        "attach",
        "attachment",
        "role",
        "access",
        "workflow",
        "activity",
    ] {
        assert!(
            issue_names.contains(&product_command),
            "{product_command} must live under the Issues namespace"
        );
        assert!(
            !root_names.contains(&product_command),
            "{product_command} must not exist at the root"
        );
    }
}

#[test]
fn root_help_presents_navigation_then_product_namespaces() {
    let mut cli = build_cli(&specs());
    let help = cli.render_long_help().to_string();
    for visible in ["context", "orbits", "worlds", "serve", "issues"] {
        assert!(
            help.contains(visible),
            "root help should expose {visible}:\n{help}"
        );
    }
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("show ")),
        "flat Issues verbs must stay out of root help:\n{help}"
    );
    assert!(
        help.find("context").unwrap() < help.find("issues").unwrap(),
        "navigation should precede product namespaces:\n{help}"
    );
}

#[test]
fn daemon_off_switch_is_shutdown() {
    // `stop` now belongs to the work loop; the daemon's off-switch is `shutdown`.
    parses_to(&["lait", "shutdown"], Request::Stop);
}

#[test]
fn comment_single_arg_is_the_body_with_explicit_ref_still_working() {
    // Two positionals: ref + body, exactly as typed.
    parses_to(
        &["lait", "issues", "comment", "ENG-1", "found it"],
        IssuesRequest::Comment {
            reply_to: None,
            reff: "ENG-1".into(),
            body: "found it".into(),
        },
    );
    // One positional: it's the BODY; the ref must come from the git branch.
    // Off a KEY-n branch that inference fails with a teaching error (this test
    // runs on arbitrary branches, so pin only the failure shape).
    match parse_issues(&["lait", "issues", "comment", "just a body, no ref"]) {
        Ok(IssuesRequest::Comment { reff, body, .. }) => {
            // On a KEY-n branch the inference kicks in — body must be intact.
            assert_eq!(body, "just a body, no ref");
            assert!(reff.contains('-'), "inferred ref is a KEY-n: {reff}");
        }
        Ok(other) => panic!("wrong request: {other:?}"),
        Err(e) => assert!(
            e.to_string().contains("inferred from the git branch"),
            "teaching error expected, got: {e}"
        ),
    }
}
