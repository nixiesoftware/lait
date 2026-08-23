//! The product-independence red gate.
//!
//! The navigation shell hosts *Worlds*; it must not know which one. Every
//! reference from `src/**` to a product crate (`issues`, `issues-app`) is a
//! place where the engine has been taught a product's vocabulary, and each one
//! is a thing a second World would have to impersonate.
//!
//! The historical allowlist is now empty. The gate fails for any production
//! reference and also proves the root package has no production dependency
//! edge to a product crate. Test fixtures remain allowed to name products.
//! There are no platform composition exceptions: a platform that cannot run
//! an independent World presents that absence instead of linking a product.
//!
//! It parses rather than greps, for the same reason `semantic_type_names.rs`
//! does: `#[cfg(test)]` fixtures legitimately name a product (a test needs
//! *some* World to test with), and a line scan cannot tell a fixture from
//! a production dependency. Ask for a `Diagnose` and you should not get an
//! answer that depends on the Issues crate; ask for a test fixture and of course
//! you should.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The production Rust that hosts Worlds without being one: the shell
/// (`src/**`) and the clients above it (`tools/astrolabe`,
/// `tools/astrolabe-ios`, `tools/feed`). The product crates themselves and
/// the engine crates are not in scope — a product may name itself, and the
/// engine crates are already product-free by construction.
fn shell_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for scope in [
        "src",
        "tools/astrolabe/src",
        "tools/astrolabe-ios/src",
        "tools/feed/src",
    ] {
        walk(&workspace_root().join(scope), &mut out);
    }
    let test_fixture = workspace_root().join("src/world/test.rs");
    out.retain(|path| path != &test_fixture);
    out.sort();
    out
}

/// Crate roots that are products, not engine — derived from `products/`, not
/// hardcoded, so a second World is covered the day it lands rather than the day
/// someone remembers to update a list.
fn product_crates() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(workspace_root().join("products")) else {
        return out;
    };
    for entry in entries.flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("name") {
                if let Some(name) = rest.split('"').nth(1) {
                    // `lait-issues` is `lait_issues` in a path position, and is
                    // also reachable as the `issues` dependency alias.
                    out.insert(name.replace('-', "_"));
                    if let Some(short) = name.strip_prefix("lait_").or(name.strip_prefix("lait-")) {
                        out.insert(short.replace('-', "_"));
                    }
                }
                break;
            }
        }
    }
    out
}

/// Every UpperCamel type a product defines, minus any the shell defines itself.
///
/// This is what closes the **laundering** hole. Matching on a name rather than a
/// path means `issues::dto::DirtyProject` and `crate::control::DirtyProject`
/// record the *same* symbol, so re-exporting a product type through a shell
/// module changes nothing the gate sees. Without it, the ratchet rewarded the
/// wrong refactor: relabelling the path made a file look decoupled and demanded
/// its allowlist row be deleted, while the struct stayed typed on a product DTO.
///
/// Subtracting shell-defined names keeps genuine collisions (`Spec`,
/// `StatusProjection`, `Invalid`) from firing. Only types participate: product
/// *function* names like `new`/`parse`/`id` are far too common to match on, so
/// laundering a free function through a shell re-export is a known remaining
/// hole — narrower than the type case, and recorded here rather than pretended
/// away.
fn product_types() -> BTreeSet<String> {
    let defined = |root: PathBuf| -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Ok(ast) = syn::parse_file(&text) else {
                continue;
            };
            for item in &ast.items {
                let name = match item {
                    syn::Item::Struct(i) => Some(i.ident.to_string()),
                    syn::Item::Enum(i) => Some(i.ident.to_string()),
                    syn::Item::Type(i) => Some(i.ident.to_string()),
                    _ => None,
                };
                if let Some(name) = name {
                    names.insert(name);
                }
            }
        }
        names
    };
    let root = workspace_root();
    let mut product = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(root.join("products")) {
        for entry in entries.flatten() {
            product.extend(defined(entry.path().join("src")));
        }
    }
    // Subtract every name the shell or an ENGINE crate also defines. `Body`,
    // `Row`, `State` and `Target` exist in both a product and in
    // `replica`/`runtime`/`clap`; the shell means the engine's, and flagging it
    // would report an ordinary engine type as a product dependency.
    let mut ambient = defined(root.join("src"));
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        for entry in entries.flatten() {
            ambient.extend(defined(entry.path().join("src")));
        }
    }
    product.difference(&ambient).cloned().collect()
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// Whether an item is compiled only for tests. Mirrors `semantic_type_names.rs`:
/// an inline `#[cfg(test)] mod` inside `src/` is a test, and a test may name a
/// product because it has to test against *something*.
/// Whether a cfg predicate can be true or false when `test` itself is false.
/// Unknown target/feature predicates are conservatively allowed either value.
fn cfg_without_test(meta: &syn::Meta) -> (bool, bool) {
    match meta {
        syn::Meta::Path(path) if path.is_ident("test") => (false, true),
        syn::Meta::Path(_) | syn::Meta::NameValue(_) => (true, true),
        syn::Meta::List(list) => {
            let Ok(items) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return (true, true);
            };
            if list.path.is_ident("all") {
                let values: Vec<_> = items.iter().map(cfg_without_test).collect();
                (
                    values.iter().all(|(can_true, _)| *can_true),
                    values.iter().any(|(_, can_false)| *can_false),
                )
            } else if list.path.is_ident("any") {
                let values: Vec<_> = items.iter().map(cfg_without_test).collect();
                (
                    values.iter().any(|(can_true, _)| *can_true),
                    values.iter().all(|(_, can_false)| *can_false),
                )
            } else if list.path.is_ident("not") && items.len() == 1 {
                let (can_true, can_false) = cfg_without_test(&items[0]);
                (can_false, can_true)
            } else {
                (true, true)
            }
        }
    }
}

fn test_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if attr.path().is_ident("test") {
            return true;
        }
        if !attr.path().is_ident("cfg") {
            return false;
        }
        attr.parse_args::<syn::Meta>()
            .is_ok_and(|predicate| !cfg_without_test(&predicate).0)
    })
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

struct Scanner {
    crates: BTreeSet<String>,
    types: BTreeSet<String>,
    /// Names this file imported from a NON-product crate, e.g.
    /// `use axum::body::Body`. A bare `Body` afterwards means axum's, not the
    /// product type that happens to share the name. Collected per file, so the
    /// exclusion is exactly as wide as the import that justifies it — no
    /// hardcoded list to drift.
    shadowed: BTreeSet<String>,
    /// The product symbols this file names. Recorded per **symbol**, not per
    /// file: a file is not a single yes/no, or a phase could add ten new
    /// references to an already-allowlisted file and the gate would stay green.
    hits: BTreeSet<String>,
}

impl Scanner {
    fn new(crates: BTreeSet<String>, types: BTreeSet<String>) -> Self {
        Self {
            crates,
            types,
            shadowed: BTreeSet::new(),
            hits: BTreeSet::new(),
        }
    }

    fn note_ident(&mut self, name: &str) {
        if self.shadowed.contains(name) {
            return;
        }
        if self.types.contains(name) {
            self.hits.insert(name.to_string());
        }
    }

    /// Qualification is the whole test for a *crate* root. A crate reference is
    /// always `issues::…` with at least two segments; a bare `issues` is a local
    /// binding — the shell is full of `let issues: Vec<String>` and
    /// `fn f(issues: &[String])`, because "issues" is also an ordinary English
    /// word for the things being counted. Flagging the single-segment form
    /// reports the *variable* as the *crate*.
    fn note_path(&mut self, path: &syn::Path) {
        let qualified = path.segments.len() >= 2;
        if qualified {
            if let Some(first) = path.segments.first() {
                if self.crates.contains(&first.ident.to_string()) {
                    let leaf = path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_default();
                    self.hits.insert(leaf);
                }
            }
        }
        // The name rule (which is what defeats laundering) applies only where a
        // product type could actually be meant: a bare ident, or a path rooted
        // in this crate. `axum::body::Body` and the enum variant `Cell::Row`
        // both end in a segment a product also happens to name, and neither is
        // a product reference.
        let local = path.segments.len() == 1
            || path.segments.first().is_some_and(|s| {
                matches!(s.ident.to_string().as_str(), "crate" | "self" | "super")
            });
        if local {
            if let Some(last) = path.segments.last() {
                self.note_ident(&last.ident.to_string());
            }
        }
    }

    /// Walk a macro body's tokens.
    ///
    /// `syn` stores a macro invocation as an opaque `TokenStream` and
    /// `syn::visit` does not descend into it — so before this, a product type
    /// constructed inside `format!`, `matches!`, `tracing::debug!` or
    /// `serde_json::json!` was completely invisible. There are over a thousand
    /// macro call sites under `src/`, and those macros are the ordinary idiom,
    /// so this was the largest false-negative surface by a wide margin and
    /// trivially trippable by accident.
    ///
    /// Works on the stream's textual form rather than `proc_macro2` types, to
    /// avoid a dev-dependency for one traversal. Qualification is recovered the
    /// same way as for paths: a token immediately followed by `::` is a crate
    /// root, a bare one is not.
    fn note_tokens(&mut self, tokens: &str) {
        let parts: Vec<&str> = tokens.split_whitespace().collect();
        for (index, token) in parts.iter().enumerate() {
            let token = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if token.is_empty() {
                continue;
            }
            let after_sep = index
                .checked_sub(1)
                .and_then(|prev| parts.get(prev))
                .is_some_and(|prev| prev.ends_with("::"));
            let local_path = if after_sep {
                let mut root = index;
                while root >= 2 && parts.get(root - 1).is_some_and(|part| part.ends_with("::")) {
                    root -= 2;
                }
                parts.get(root).is_some_and(|part| {
                    matches!(
                        part.trim_matches(|c: char| !c.is_alphanumeric() && c != '_'),
                        "crate" | "self" | "super"
                    )
                })
            } else {
                false
            };
            if !after_sep || local_path {
                self.note_ident(token);
            }
            let qualified = parts
                .get(index.saturating_add(1))
                .is_some_and(|next| next.starts_with("::"));
            if qualified && self.crates.contains(token) {
                self.hits.insert(token.to_string());
            }
        }
    }
}

impl Scanner {
    /// Pre-pass: every name brought in by a `use` rooted at a crate that is not
    /// a product. Those names cannot mean the product type in this file.
    fn collect_shadowed(&mut self, file: &syn::File) {
        fn leaves(tree: &syn::UseTree, out: &mut Vec<String>) {
            match tree {
                syn::UseTree::Path(p) => leaves(&p.tree, out),
                syn::UseTree::Name(n) => out.push(n.ident.to_string()),
                syn::UseTree::Rename(r) => out.push(r.rename.to_string()),
                syn::UseTree::Group(g) => g.items.iter().for_each(|t| leaves(t, out)),
                syn::UseTree::Glob(_) => {}
            }
        }
        for item in &file.items {
            let syn::Item::Use(use_item) = item else {
                continue;
            };
            let root = match &use_item.tree {
                syn::UseTree::Path(p) => p.ident.to_string(),
                _ => continue,
            };
            if self.crates.contains(&root) || matches!(root.as_str(), "crate" | "self" | "super") {
                continue;
            }
            let mut names = Vec::new();
            leaves(&use_item.tree, &mut names);
            self.shadowed.extend(names);
        }
    }
}

impl<'ast> Visit<'ast> for Scanner {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if test_gated(item_attrs(item)) {
            return;
        }
        syn::visit::visit_item(self, item);
    }

    /// `issues::dto::MemberDto` — qualified, so it names the crate.
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.note_path(path);
        syn::visit::visit_path(self, path);
    }

    /// `use issues::dto::{…}` — a use tree is not a `Path`.
    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        let ident = match tree {
            syn::UseTree::Path(p) => Some((&p.ident, true)),
            syn::UseTree::Name(n) => Some((&n.ident, false)),
            syn::UseTree::Rename(r) => Some((&r.ident, false)),
            _ => None,
        };
        if let Some((ident, is_root)) = ident {
            let name = ident.to_string();
            if is_root && self.crates.contains(&name) {
                self.hits.insert(name);
            }
        }
        syn::visit::visit_use_tree(self, tree);
    }

    /// `extern crate issues;`
    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        let name = item.ident.to_string();
        if self.crates.contains(&name) {
            self.hits.insert(name);
        }
        syn::visit::visit_item_extern_crate(self, item);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.note_tokens(&mac.tokens.to_string());
        syn::visit::visit_macro(self, mac);
    }
}

/// Every product symbol named by shell production code, as `(path, symbol)`.
fn scan() -> BTreeSet<(String, String)> {
    let root = workspace_root();
    let crates = product_crates();
    let types = product_types();
    let mut found = BTreeSet::new();
    for file in shell_sources() {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| {
            panic!("read {rel}: {e} — the gate must not skip a file it cannot read")
        });
        let ast = syn::parse_file(&text).unwrap_or_else(|e| {
            panic!("parse {rel}: {e} — an unparseable file must fail loudly, not pass silently")
        });
        let mut scanner = Scanner::new(crates.clone(), types.clone());
        scanner.collect_shadowed(&ast);
        scanner.visit_file(&ast);
        for symbol in scanner.hits {
            found.insert((rel.clone(), symbol));
        }
    }
    found
}

/// Parse the checked-in allowlist: `path<TAB>rule<TAB>phase`, `#` comments.
fn allowlist() -> BTreeSet<(String, String)> {
    let raw =
        std::fs::read_to_string(workspace_root().join("tests/product_independence_allowlist.tsv"))
            .unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| {
            let mut cols = l.split('\t');
            let path = cols.next().unwrap_or("").trim().to_string();
            let rule = cols.next().unwrap_or("").trim().to_string();
            (path, rule)
        })
        .collect()
}

#[test]
fn no_unallowlisted_product_references() {
    let found = scan();
    let allowed = allowlist();
    let new: Vec<_> = found.difference(&allowed).collect();
    let stale: Vec<_> = allowed.difference(&found).collect();
    let mut msg = String::new();
    if !new.is_empty() {
        let _ = writeln!(
            msg,
            "NEW product references in the shell (not in \
             tests/product_independence_allowlist.tsv).\n\
             The shell hosts Worlds; it must not name one. Inject it through the \
             installed-package boundary or move the shared type into an engine \
             crate:"
        );
        for (path, rule) in &new {
            let _ = writeln!(msg, "  {path}\t{rule}");
        }
    }
    if !stale.is_empty() {
        let _ = writeln!(
            msg,
            "STALE allowlist entries (this file is now product-free — prune them, \
             the ratchet only turns one way):"
        );
        for (path, rule) in &stale {
            let _ = writeln!(msg, "  {path}\t{rule}");
        }
    }
    assert!(msg.is_empty(), "\n{msg}");
}

/// The root production dependency graph is product-free. Dev dependencies are
/// permitted solely for explicit test fixtures.
#[test]
fn allowlist_is_empty_when_the_shell_stops_depending_on_products() {
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml"))
        .expect("read the workspace manifest");
    let production = manifest
        .split_once("[dependencies]")
        .map(|(_, rest)| rest.split("\n[").next().unwrap_or(rest))
        .expect("root manifest has a dependencies section");
    let edges: Vec<_> = product_crates()
        .into_iter()
        .filter(|name| production.contains(&name.replace('_', "-")))
        .collect();
    assert!(
        edges.is_empty(),
        "root production product edges remain: {edges:?}"
    );
    let allowed = allowlist();
    assert!(
        allowed.is_empty(),
        "the production shell is product-free, but {} allowlist entries remain",
        allowed.len()
    );
}

/// iOS cannot dynamically install native runners. That limitation is not an
/// excuse for a hidden static composition root: the mobile host must remain
/// product-free and render Worlds as unavailable until an independent delivery
/// contract exists for the platform.
#[test]
fn ios_has_no_product_composition_root() {
    assert!(
        !workspace_root()
            .join("tools/astrolabe-ios/src/worlds.rs")
            .exists(),
        "iOS regained a product composition root instead of an independent boundary"
    );
}

/// The rule fires. A gate that cannot fail is not a gate.
#[test]
fn the_rule_has_teeth() {
    let crates: BTreeSet<String> = ["issues", "issues_app"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let types: BTreeSet<String> = ["DirtyProject", "IssuesWorld", "MemberDto"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let scan = |src: &str| {
        let ast = syn::parse_file(src).expect("sample parses");
        let mut s = Scanner::new(crates.clone(), types.clone());
        s.visit_file(&ast);
        s.hits
    };

    assert!(
        !scan("fn f() { let _ = issues::dto::MemberDto; }").is_empty(),
        "a production path into a product crate must be caught"
    );
    assert!(
        !scan("use issues_app::lifecycle::founder_policy;").is_empty(),
        "a `use` into a product crate must be caught"
    );
    assert!(
        !scan("fn f() -> IssuesWorld { todo!() }").is_empty(),
        "naming a product type must be caught"
    );
    assert!(
        !scan("extern crate issues;").is_empty(),
        "an extern crate declaration must be caught"
    );

    // Regression: macro bodies are opaque TokenStreams and `syn::visit` does not
    // descend into them. `format!`/`matches!`/`tracing::debug!` are the ordinary
    // idiom in this tree, so this was the largest blind spot.
    assert!(
        !scan(r#"fn f() -> String { format!("{:?}", issues_app::IssuesRequest::ProjectList) }"#)
            .is_empty(),
        "a product path inside a macro body must be caught"
    );

    // Regression: the ratchet used to turn BACKWARDS here. Laundering a product
    // type through a shell re-export decouples nothing, but a path-keyed gate
    // called the file clean and demanded its allowlist row be deleted.
    assert_eq!(
        scan("struct S { d: Vec<issues::dto::DirtyProject> }"),
        scan("struct S { d: Vec<crate::control::DirtyProject> }"),
        "re-exporting a product type through a shell module must record the \
         same symbol — otherwise relabelling the path fakes decoupling"
    );

    assert!(
        scan("use mechanics::ids::SpaceId; fn f(_: SpaceId) {}").is_empty(),
        "engine paths must not be flagged"
    );

    // The false positive this gate was born with. "issues" is an ordinary
    // English word for the things the shell counts and watches, so the bare
    // identifier appears all over `serve` and `hosting` as a local, a
    // parameter, and a field shorthand. A single-segment path is a binding.
    assert!(
        scan("fn f(issues: &[String]) { let issues: Vec<String> = issues.to_vec(); }").is_empty(),
        "a local/parameter named `issues` is not a product reference"
    );

    assert!(
        scan("#[cfg(test)] mod t { fn f() { let _ = issues::dto::MemberDto; } }").is_empty(),
        "a #[cfg(test)] fixture may name a product"
    );
    assert!(
        scan("#[cfg(all(test, unix))] mod t { fn f() { let _ = issues::dto::MemberDto; } }")
            .is_empty(),
        "a cfg that requires test is a test-only fixture"
    );
    assert!(
        !scan("#[cfg(not(test))] fn f() { let _ = issues::dto::MemberDto; }").is_empty(),
        "cfg(not(test)) is production code and must not bypass the gate"
    );
    assert!(
        !scan("#[cfg(any(test, unix))] fn f() { let _ = issues::dto::MemberDto; }").is_empty(),
        "a cfg that can compile without test must not bypass the gate"
    );
    assert!(
        !scan("fn f() { tracing::debug!(\"{:?}\", crate::control::DirtyProject); }").is_empty(),
        "a product type laundered through a local path inside a macro must be caught"
    );
}
