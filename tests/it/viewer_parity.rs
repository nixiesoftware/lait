//! Viewer / installed-interface parity guard.
//!
//! The local app's head accepts both the Issues-owned application protocol and
//! root Space/daemon control. `viewer/src/types.ts` is a hand-written TypeScript
//! union of those installed interfaces, so this test checks it against both
//! canonical Rust schemas.
//!
//! What makes that a correctness problem rather than a tidiness one: nothing in
//! `src/` uses `deny_unknown_fields`, and it should not: add-only fields are what
//! lets a newer client talk to a daemon that is stale across an update. So a
//! field the TS invents is **silently dropped**, and the daemon does something
//! plausible with the rest.
//!
//! The sharpest instance, and the reason for the direction of the assert below:
//! `assign` takes `add: bool`, defaulted **true** (`#[serde(default = "default_true")]`).
//! A client that sends `{cmd:"assign", reff, who, remove:true}` gets `remove`
//! dropped and `add` defaulted — it *adds* the assignee the user asked to remove.
//! No error. Wrong action. That is what an unchecked mirror buys you.
//!
//! This is the "check" half of "generate/check, don't hand-maintain twice" — the
//! same rule `mcp_parity.rs` states and enforces for the MCP surface. Generating
//! `types.ts` outright (ts-rs) and diffing a regeneration in CI, exactly as the
//! `viewer` job already does for the asset bundle, is the better end state; this is
//! the cheap half that makes the current file honest in the meantime.
//!
//! **Scope, and its blind spot.** This guards the *write* surface — Request field
//! *names* — and nothing more. It does not check *Response* shapes, and it does not
//! check the *semantics* of a field (what value the daemon puts there). That gap is
//! not hypothetical: durable history changed `ActivityEvent.actor`/`actor_nick`
//! semantics under the viewer and this test was blind to it.
//!
//! **There is no behavioural pin for that class.** This comment used to name one —
//! `replica.rs::tests::history_is_the_contract_the_viewer_reads` — and no such test
//! exists anywhere in the tree; grepping the name finds this line and nothing else.
//! A pointer to a guard that is not there is worse than no pointer, because it
//! tells a reader the class is covered.
//!
//! What that leaves unguarded is live right now: `ActivityEvent.actor` is populated
//! from the committing *device* while the viewer resolves display names by *actor*,
//! so the lookup misses on every row and every author renders as a hex prefix in a
//! colour derived from that hex — a different colour from the same person's roster
//! chip. The field is named `actor` and does not carry one.

use std::collections::{BTreeMap, BTreeSet};

/// Field names the installed Rust interfaces declare for each `cmd`, keyed by
/// tag value. Inbox is a product-owned host capability: the viewer sends
/// `clear`, while the shell supplies the local watermark to the World request.
fn rust_request_fields() -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for schema in [
        serde_json::to_value(schemars::schema_for!(lait::control::Request))
            .expect("root control schema to json"),
        serde_json::to_value(schemars::schema_for!(issues_app::IssuesRequest))
            .expect("Issues schema to json"),
    ] {
        let variants = schema
            .get("oneOf")
            .and_then(|value| value.as_array())
            .expect("internally-tagged request enum schema is a oneOf");
        for variant in variants {
            let Some(props) = variant
                .get("properties")
                .and_then(|value| value.as_object())
            else {
                continue;
            };
            let Some(cmd) = props
                .get("cmd")
                .and_then(|value| value.get("const"))
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            let fields = props
                .keys()
                .filter(|key| key.as_str() != "cmd")
                .cloned()
                .collect();
            assert!(
                out.insert(cmd.to_string(), fields).is_none(),
                "root control and Issues both declare `{cmd}`"
            );
        }
    }
    out.insert("inbox".into(), BTreeSet::from(["clear".into()]));
    out.insert("access_list".into(), BTreeSet::from(["actor".into()]));
    out.insert(
        "access_grant".into(),
        BTreeSet::from(["actor".into(), "role".into(), "project".into()]),
    );
    out.insert(
        "access_revoke".into(),
        BTreeSet::from(["grant_id".into(), "grant_ids".into()]),
    );

    out
}

/// Field names `types.ts` declares for each `cmd`.
///
/// The union is one variant per line — `| { cmd: "issue_new"; title: string; … }` —
/// which is regular enough to read without a TS parser. If that ever stops being
/// true this test fails loudly (it asserts it found the union at all) rather than
/// quietly checking nothing, which is the failure mode of a scraper.
fn ts_request_fields(src: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for line in src.lines() {
        let line = line.trim();
        if !line.starts_with("| { cmd:") {
            continue;
        }
        let Some(rest) = line.strip_prefix("| {") else {
            continue;
        };
        // `;` before `}`: a variant line ends `};`, and stripping the brace first
        // leaves it stranded on the final field.
        let body = rest
            .trim()
            .trim_end_matches(';')
            .trim_end_matches('}')
            .trim();

        let mut cmd = None;
        let mut fields = BTreeSet::new();
        for part in body.split(';') {
            let part = part.trim();
            let Some((name, value)) = part.split_once(':') else {
                continue;
            };
            // `foo?: string` — the `?` is TS optionality, not part of the name.
            let name = name.trim().trim_end_matches('?').trim();
            if name == "cmd" {
                cmd = Some(value.trim().trim_matches('"').to_string());
            } else if !name.is_empty() {
                fields.insert(name.to_string());
            }
        }
        if let Some(cmd) = cmd {
            out.insert(cmd, fields);
        }
    }
    out
}

fn types_ts() -> Option<String> {
    // `viewer/` is excluded from the published crate, so a consumer running
    // `cargo test` on a downloaded lait has nothing to check. Skip rather than
    // fail: this guard is for the repo, not for them.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("viewer")
        .join("src")
        .join("types.ts");
    std::fs::read_to_string(path).ok()
}

/// Every field the TS client declares must exist on the Rust variant.
///
/// This direction is the one that bites. A TS field Rust doesn't have is accepted
/// by the daemon and ignored — the request still runs, with that field's intent
/// silently discarded.
///
/// The reverse is *not* asserted: `types.ts` deliberately omits fields (and whole
/// verbs) the browser does not expose, and the add-only field rule means a Rust
/// field the client never sends is exactly what forward compatibility looks like.
#[test]
fn the_ts_client_invents_no_fields() {
    let Some(src) = types_ts() else {
        return;
    };
    let rust = rust_request_fields();
    let ts = ts_request_fields(&src);

    assert!(
        ts.len() > 20,
        "parsed only {} Request variants out of types.ts — the union's shape changed \
         and this guard is now checking nothing, which is worse than not having it",
        ts.len(),
    );

    for (cmd, ts_fields) in &ts {
        let Some(rust_fields) = rust.get(cmd) else {
            panic!(
                "types.ts declares `cmd: \"{cmd}\"`, which no installed Rust \
                 interface accepts"
            );
        };
        for field in ts_fields {
            assert!(
                rust_fields.contains(field),
                "types.ts declares `{field}` on `{cmd}`, but its installed Rust interface does not.\n\
                 serde ignores unknown fields deliberately for forward compatibility, so this \
                 would be dropped in flight and the command would run without it.\n\
                 Rust has: {rust_fields:?}",
            );
        }
    }
}

/// The specific trap that motivated this file, pinned by name.
///
/// `assign`'s bool is `add`, defaulted true — so an invented `remove: true` doesn't
/// merely fail to remove, it *adds*. If someone "fixes" types.ts to match the CLI's
/// `--remove` flag, this says why that's wrong before the wrong assignee does.
#[test]
fn assign_is_add_not_remove() {
    let rust = rust_request_fields();
    let fields = rust.get("assign").expect("assign variant");
    assert!(fields.contains("add"), "assign's flag is `add`: {fields:?}");
    assert!(
        !fields.contains("remove"),
        "assign has no `remove` — clearing an assignee is `add: false`",
    );

    if let Some(src) = types_ts() {
        let ts = ts_request_fields(&src);
        let ts_assign = ts.get("assign").expect("types.ts declares assign");
        assert!(
            !ts_assign.contains("remove"),
            "types.ts declares `remove` on assign; serde would drop it and default \
             `add` to true, adding the assignee the user asked to remove",
        );
    }
}
