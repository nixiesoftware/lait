//! Cross-plan package-access boundary for durable work.
//!
//! Presence and the exact `Session::submit` signature are frozen by Runtime's
//! `public_lifecycle_api` fixture. This gate proves the other half: adding a
//! convenient `Session::exec` or `Session::start` method cannot silently create
//! a second route around a World's semantic callback.

use std::collections::BTreeSet;
use std::path::PathBuf;

use syn::visit::Visit;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[derive(Default)]
struct SessionMethods(BTreeSet<String>);

impl<'ast> Visit<'ast> for SessionMethods {
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let is_session = match node.self_ty.as_ref() {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Session"),
            _ => false,
        };
        if is_session {
            for item in &node.items {
                if let syn::ImplItem::Fn(function) = item {
                    if matches!(function.vis, syn::Visibility::Public(_)) {
                        self.0.insert(function.sig.ident.to_string());
                    }
                }
            }
        }
        syn::visit::visit_item_impl(self, node);
    }
}

#[test]
fn session_has_no_direct_durable_work_shortcut() {
    let path = workspace_root().join("crates/runtime/src/session.rs");
    let text = std::fs::read_to_string(path).expect("read Session implementation");
    let file = syn::parse_file(&text).expect("parse Session implementation");
    let mut methods = SessionMethods::default();
    methods.visit_file(&file);

    assert!(
        methods.0.contains("submit"),
        "Session lost its ordinary durable action entrypoint"
    );
    for shortcut in ["exec", "start"] {
        assert!(
            !methods.0.contains(shortcut),
            "Session::{shortcut} bypasses or duplicates Session::submit"
        );
    }
}

fn public_struct_fields(file: &syn::File, name: &str) -> Vec<String> {
    let item = file.items.iter().find_map(|item| match item {
        syn::Item::Struct(item) if item.ident == name => Some(item),
        _ => None,
    });
    let item = item.unwrap_or_else(|| panic!("missing exec::{name}"));
    let syn::Fields::Named(fields) = &item.fields else {
        panic!("exec::{name} must have named fields");
    };
    fields
        .named
        .iter()
        .map(|field| field.ident.as_ref().expect("named field").to_string())
        .collect()
}

#[test]
fn start_and_try_carry_semantic_intent_not_ambient_coordinates() {
    let path = workspace_root().join("crates/runtime/src/exec.rs");
    let text = std::fs::read_to_string(path).expect("read Exec contract");
    let file = syn::parse_file(&text).expect("parse Exec contract");

    // `target` is the directed-Start coordinate (run-event generation 2): the
    // one Station a Start names to run it. It is a Station address, not a
    // World's semantic intent — World-agnostic infrastructure — so it belongs
    // beside `service`/`resources` and not inside `input`.
    assert_eq!(
        public_struct_fields(&file, "Start"),
        [
            "spec",
            "build",
            "input",
            "parent",
            "source",
            "service",
            "resources",
            "limits",
            "queries",
            "target",
        ]
    );
    // `fence` is gone: the signed Leased event is the Attempt identity now, so
    // the Fence counter it replaced is no longer a field.
    assert_eq!(
        public_struct_fields(&file, "Try"),
        [
            "run",
            "build",
            "offer",
            "resources",
            "enforcement",
            "limits",
            "lease",
            "checkpoint",
        ]
    );
}
