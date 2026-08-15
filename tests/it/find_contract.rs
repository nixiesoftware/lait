//! Package-access and ambient-coordinate gates for generic Find.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::path::PathBuf;

use syn::visit::Visit;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[derive(Default)]
struct Calls(BTreeSet<String>);

impl<'ast> Visit<'ast> for Calls {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.0.insert(node.method.to_string());
        syn::visit::visit_expr_method_call(self, node);
    }
}

#[derive(Default)]
struct Paths(BTreeSet<String>);

impl<'ast> Visit<'ast> for Paths {
    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        self.0.extend(
            node.path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string()),
        );
        syn::visit::visit_type_path(self, node);
    }
}

#[derive(Default)]
struct FindPaths(BTreeSet<String>);

fn collect_find_use(tree: &syn::UseTree, prefix: &mut Vec<String>, found: &mut BTreeSet<String>) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_find_use(&path.tree, prefix, found);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            prefix.push(name.ident.to_string());
            if prefix.iter().any(|segment| segment == "find") {
                found.insert(prefix.join("::"));
            }
            prefix.pop();
        }
        syn::UseTree::Rename(rename) => {
            prefix.push(rename.ident.to_string());
            if prefix.iter().any(|segment| segment == "find") {
                found.insert(prefix.join("::"));
            }
            prefix.pop();
        }
        syn::UseTree::Glob(_) => {
            if prefix.iter().any(|segment| segment == "find") {
                found.insert(format!("{}::*", prefix.join("::")));
            }
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                collect_find_use(item, prefix, found);
            }
        }
    }
}

impl<'ast> Visit<'ast> for FindPaths {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        if node
            .segments
            .iter()
            .position(|segment| segment.ident == "find")
            .is_some_and(|position| position + 1 < node.segments.len())
        {
            self.0.insert(
                node.segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::"),
            );
        }
        syn::visit::visit_path(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        collect_find_use(&node.tree, &mut Vec::new(), &mut self.0);
        syn::visit::visit_item_use(self, node);
    }
}

fn rust_sources(path: &Path) -> Vec<PathBuf> {
    fn walk(path: &Path, files: &mut Vec<PathBuf>) {
        if path.is_file() {
            if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                files.push(path.to_path_buf());
            }
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    walk(path, &mut files);
    files.sort();
    files
}

fn session_methods() -> BTreeMap<String, (bool, BTreeSet<String>, BTreeSet<String>)> {
    let path = workspace_root().join("crates/runtime/src/session.rs");
    let text = std::fs::read_to_string(path).expect("read Session implementation");
    let file = syn::parse_file(&text).expect("parse Session implementation");
    let mut methods = BTreeMap::new();
    for item in file.items {
        let syn::Item::Impl(item_impl) = item else {
            continue;
        };
        let syn::Type::Path(self_ty) = item_impl.self_ty.as_ref() else {
            continue;
        };
        if !self_ty.path.is_ident("Session") {
            continue;
        }
        for item in item_impl.items {
            let syn::ImplItem::Fn(function) = item else {
                continue;
            };
            let mut calls = Calls::default();
            calls.visit_block(&function.block);
            let mut paths = Paths::default();
            paths.visit_signature(&function.sig);
            methods.insert(
                function.sig.ident.to_string(),
                (
                    matches!(function.vis, syn::Visibility::Public(_)),
                    calls.0,
                    paths.0,
                ),
            );
        }
    }
    methods
}

#[test]
fn submit_and_find_share_the_runtime_owned_ambient_prefix() {
    let methods = session_methods();
    let submit = methods.get("submit").expect("Session::submit exists");
    let find = methods.get("find").expect("Session::find exists");

    assert!(submit.1.contains("ambient"), "submit bypassed Ambient");
    assert!(find.1.contains("ambient"), "find bypassed Ambient");
    assert!(find.0, "Session::find is not public");
    for required in ["find", "Query", "Answer", "Failure"] {
        assert!(
            find.2.contains(required),
            "Session::find signature lost `{required}`"
        );
    }
    assert!(
        !find.1.contains("submit") && !find.1.contains("query"),
        "generic Find entered a World semantic callback"
    );
    assert!(
        !methods.get("exec").is_some_and(|method| method.0),
        "Session::exec created a second ambient entrypoint"
    );
}

#[test]
fn world_callbacks_receive_no_find_or_session_facade() {
    let path = workspace_root().join("crates/runtime/src/world.rs");
    let text = std::fs::read_to_string(path).expect("read World contract");
    let file = syn::parse_file(&text).expect("parse World contract");
    let world = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Trait(item_trait) if item_trait.ident == "World" => Some(item_trait),
            _ => None,
        })
        .expect("World trait exists");

    for item in &world.items {
        let syn::TraitItem::Fn(function) = item else {
            continue;
        };
        if function.sig.ident != "submit" && function.sig.ident != "query" {
            continue;
        }
        let mut paths = Paths::default();
        paths.visit_signature(&function.sig);
        for forbidden in ["find", "Session", "Answer", "Grant"] {
            assert!(
                !paths.0.contains(forbidden),
                "World::{} received forbidden `{forbidden}` facade",
                function.sig.ident
            );
        }
    }
}

#[test]
fn client_and_product_wire_cannot_transport_generic_find() {
    let root = workspace_root();
    let hostile = syn::parse_file(
        "use runtime::find::Query as GenericQuery; struct Frame { query: GenericQuery }",
    )
    .unwrap();
    let mut detector = FindPaths::default();
    detector.visit_file(&hostile);
    assert_eq!(detector.0, BTreeSet::from(["runtime::find::Query".into()]));

    let boundaries = [
        root.join("src/serve"),
        root.join("src/mcp.rs"),
        root.join("crates/runtime/src/plane"),
        root.join("products/issues-app/src/protocol.rs"),
        root.join("products/issues-app/src/mcp.rs"),
        root.join("tools/astrolabe/src/api"),
        root.join("tools/astrolabe/src/client"),
    ];
    let mut found = Vec::new();
    for boundary in boundaries {
        for path in rust_sources(&boundary) {
            let text = std::fs::read_to_string(&path).expect("read client wire source");
            let file = syn::parse_file(&text).expect("parse client wire source");
            let mut paths = FindPaths::default();
            paths.visit_file(&file);
            for generic in paths.0 {
                found.push(format!(
                    "{}: `{generic}`",
                    path.strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/")
                ));
            }
        }
    }

    assert!(
        found.is_empty(),
        "generic Find crossed a client/product wire instead of a package-owned semantic API:\n  {}",
        found.join("\n  ")
    );
}
