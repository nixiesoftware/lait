//! The one property reliable signals must have, enforced where it can be.
//!
//! A signal is delivered or it fails loudly, and it is durable in no other
//! sense: nothing journaled, nothing replayed after a restart, nothing that
//! becomes activity. Two of those three are the same mechanism seen twice —
//! `StationCore::with_replica` is the only route to the Replica writer, and
//! `Broadcaster::publish` is the only route to the Observation ring, which
//! `SpaceBridge::frame_for` turns into `activity_advanced` for any Observation
//! carrying scopes. The third, surviving a restart, is a consequence rather
//! than a mechanism: `Orbit::activate` builds a fresh `StationCore` and reads
//! nothing signal-shaped from disk.
//!
//! **Why a parser and not privacy.** `Broadcaster::publish` is `pub(crate)` and
//! `signal.rs` lives inside that crate, so `pub(crate)` stops nothing;
//! `StationCore::with_replica` is outright `pub`. The three legitimate
//! `publish` call sites are all durable-commit or authority-advance. A fourth,
//! added from signal code, would journal nothing and still emit an Observation
//! that becomes activity — and it would be one line, in a file whose tests all
//! still pass.
//!
//! So the gate reads the source. It lands before the transport it guards, so
//! there is never a window in which the rule is only a comment.

use std::path::Path;

use syn::visit::Visit;

/// Names that reach durable state or the Observation ring.
///
/// Deliberately coarse. A false positive here costs a rename; a false negative
/// costs the property this whole module exists to have.
const FORBIDDEN: &[&str] = &[
    "StationCore",
    "with_replica",
    "Broadcaster",
    "publish",
    "Replica",
    "Journal",
    "commit_action",
    "note_authority_advanced",
];

#[derive(Default)]
struct Durability {
    found: Vec<String>,
}

impl<'ast> Visit<'ast> for Durability {
    fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
        let name = segment.ident.to_string();
        if FORBIDDEN.contains(&name.as_str()) {
            self.found.push(name);
        }
        syn::visit::visit_path_segment(self, segment);
    }

    /// A method call is not a path segment, so `core.with_replica(..)` would
    /// slip past the visitor above. This is the shape the rule is most likely
    /// to be broken in.
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let name = call.method.to_string();
        if FORBIDDEN.contains(&name.as_str()) {
            self.found.push(name);
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        if let syn::UseTree::Name(name) = tree {
            let ident = name.ident.to_string();
            if FORBIDDEN.contains(&ident.as_str()) {
                self.found.push(ident);
            }
        }
        syn::visit::visit_use_tree(self, tree);
    }

    // `std::fs` anywhere in this module is durable state by another route.
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let rendered = quote_path(&item.tree);
        if rendered.starts_with("fs") || rendered.contains("std :: fs") {
            self.found.push("std::fs".into());
        }
        syn::visit::visit_item_use(self, item);
    }
}

fn quote_path(tree: &syn::UseTree) -> String {
    match tree {
        syn::UseTree::Path(path) => format!("{} :: {}", path.ident, quote_path(&path.tree)),
        syn::UseTree::Name(name) => name.ident.to_string(),
        syn::UseTree::Rename(rename) => rename.ident.to_string(),
        syn::UseTree::Glob(_) => "*".into(),
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .map(quote_path)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn offenders(source: &str) -> Vec<String> {
    let parsed = syn::parse_file(source).expect("signal.rs parses");
    let mut visitor = Durability::default();
    visitor.visit_file(&parsed);
    visitor.found.sort();
    visitor.found.dedup();
    visitor.found
}

#[test]
fn the_signal_module_cannot_reach_durable_state() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/runtime/src/signal.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let found = offenders(&source);
    assert!(
        found.is_empty(),
        "crates/runtime/src/signal.rs names {found:?}.\n\
         A reliable signal is never journaled, never replayed after a restart, and never \n\
         emitted as activity. `with_replica` reaches the Replica writer and `publish` reaches \n\
         the Observation ring, which becomes `activity_advanced` for anything carrying scopes.\n\
         If a signal genuinely needs durable state, it is not a signal."
    );
}

#[test]
fn the_gate_can_see_what_it_claims_to_see() {
    // The negative control, and the reason it is not optional.
    //
    // A parser that silently stopped matching — a syn upgrade changing a visit
    // method, a refactor moving the call behind an alias — would keep passing
    // forever while guarding nothing. The only way to know it still works is to
    // show it rejecting something.
    let violation = r#"
        use crate::session::StationCore;
        pub fn send(core: &StationCore) {
            let _ = core.with_replica(|replica| Ok(replica.frontier()));
        }
    "#;
    let found = offenders(violation);
    assert!(
        found.contains(&"StationCore".to_string()),
        "the visitor did not see a type it is supposed to reject: {found:?}"
    );
    assert!(
        found.contains(&"with_replica".to_string()),
        "the visitor did not see a call it is supposed to reject: {found:?}"
    );

    // And the ring, by its own route.
    let ring = r#"
        pub fn shout(broadcaster: &Broadcaster) {
            broadcaster.publish(vec![], frontier, false);
        }
    "#;
    assert!(
        offenders(ring).contains(&"publish".to_string()),
        "the visitor did not see the Observation ring"
    );

    // A file that is genuinely clean must still pass, or the gate is a
    // tautology that rejects everything.
    let clean = r#"
        pub struct Declaration { pub selector: u16 }
        pub fn declarations() -> Vec<Declaration> { Vec::new() }
    "#;
    assert!(offenders(clean).is_empty());
}
