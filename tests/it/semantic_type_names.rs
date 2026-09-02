//! The semantic-naming gate, on a real parser.
//!
//! Project-owned identifiers are semantic: no protocol- or storage-generation
//! suffix (`FooV1`, `foo_v1`, `FOO_V1`) may be *declared* anywhere in production
//! sources. Wire formats keep their encoded version **fields** and their
//! versioned signing-domain/ALPN/magic **string contents**; those are data, not
//! names. Byte stability across renames is pinned by the golden fixture suites,
//! which fail if a rename ever changes an encoding.
//!
//! No alias or deprecated suffixed wrapper is permitted: a `type FooV1 = Foo;`
//! shim *declares* a suffixed identifier and fails like any other.
//!
//! This gate parses. The line-oriented prefix scan it replaces could only see
//! sixteen declaration keywords at the start of a line, so it missed fields,
//! locals, parameters, enum variants, and every declaration inside an `impl`
//! block — and it walked six crates, silently exempting an application-call crate,
//! `world-interface`, and both `products/*` packages. Extending the old
//! technique to fields and locals would have produced false positives
//! immediately (`let v1 = …`, a field named `ipv4`); a syntax tree does not
//! have that problem.
//!
//! The TypeScript half runs as its own CI step over `viewer/`; see
//! `viewer/scripts/naming-gate.mjs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every production Rust source: the package `src/**`, each concept crate's
/// `src/**`, and each product package's `src/**`. Tests and fixtures are not
/// production names.
fn production_sources() -> Vec<PathBuf> {
    fn is_test_source(path: &Path) -> bool {
        path.components()
            .any(|component| component.as_os_str() == "internal_tests")
            || path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem == "internal_tests" || stem.ends_with("_tests"))
    }

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") && !is_test_source(&p) {
                out.push(p);
            }
        }
    }
    let root = workspace_root();
    let mut out = Vec::new();
    walk(&root.join("src"), &mut out);
    for group in ["crates", "products"] {
        let Ok(entries) = std::fs::read_dir(root.join(group)) else {
            panic!("{group}/ must exist — the gate's coverage is not optional");
        };
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                walk(&entry.path().join("src"), &mut out);
            }
        }
    }
    out.sort();
    out
}

/// Whether an identifier carries a generation suffix.
///
/// IP-family names (`Ipv4`, `Ipv6Addr`) are not versions: the `V` must be a
/// *trailing* number after a lowercase letter or digit, or a `_v<digits>` tail.
/// A bare `v1`/`V1` is a version name with nothing else to it and counts.
fn versioned_ident(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if let Some(idx) = lower.rfind("_v") {
        let tail = &lower[idx + 2..];
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    if let Some(digits) = lower.strip_prefix('v') {
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    let bytes = name.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b'V' {
            let tail = &bytes[i + 1..];
            if !tail.is_empty()
                && tail.iter().all(|b| b.is_ascii_digit())
                && (bytes[i - 1].is_ascii_lowercase() || bytes[i - 1].is_ascii_digit())
            {
                return true;
            }
        }
    }
    false
}

/// Whether an item is compiled only for tests. An inline `#[cfg(test)] mod`
/// inside `src/` is a test, and the gate is about production names — otherwise
/// a fixture called `v1` in a migration test becomes pressure to rename the
/// thing the test is *about*.
fn test_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if attr.path().is_ident("test") {
            return true;
        }
        if !attr.path().is_ident("cfg") {
            return false;
        }
        if matches!(
            &attr.meta,
            syn::Meta::List(list) if list.tokens.to_string().contains("fault-injection")
        ) {
            return true;
        }
        let mut gated = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("test") {
                gated = true;
            }
            Ok(())
        });
        gated
    })
}

/// Collects every version-suffixed identifier a file *declares*. Declarations
/// are the violation unit; usages follow declarations.
#[derive(Default)]
struct Declarations {
    found: BTreeSet<String>,
    all: BTreeSet<String>,
    prefixed_errors: BTreeSet<String>,
}

impl Declarations {
    fn note(&mut self, ident: &syn::Ident) {
        let name = ident.to_string();
        self.all.insert(name.clone());
        if versioned_ident(&name) {
            self.found.insert(name);
        }
    }

    fn note_type(&mut self, ident: &syn::Ident) {
        self.note(ident);
        let name = ident.to_string();
        if name.ends_with("Error") && name != "Error" && name != "WireError" {
            self.prefixed_errors.insert(name);
        }
    }

    /// A binding pattern can introduce several names at once (`let (a, b) = …`).
    fn note_pattern(&mut self, pat: &syn::Pat) {
        match pat {
            syn::Pat::Ident(p) => self.note(&p.ident),
            syn::Pat::Type(p) => self.note_pattern(&p.pat),
            syn::Pat::Reference(p) => self.note_pattern(&p.pat),
            syn::Pat::Tuple(p) => p.elems.iter().for_each(|e| self.note_pattern(e)),
            syn::Pat::TupleStruct(p) => p.elems.iter().for_each(|e| self.note_pattern(e)),
            syn::Pat::Slice(p) => p.elems.iter().for_each(|e| self.note_pattern(e)),
            syn::Pat::Or(p) => p.cases.iter().for_each(|e| self.note_pattern(e)),
            syn::Pat::Struct(p) => p.fields.iter().for_each(|f| self.note_pattern(&f.pat)),
            _ => {}
        }
    }

    fn note_signature(&mut self, sig: &syn::Signature) {
        self.note(&sig.ident);
        for arg in &sig.inputs {
            if let syn::FnArg::Typed(t) = arg {
                self.note_pattern(&t.pat);
            }
        }
    }

    fn note_fields(&mut self, fields: &syn::Fields) {
        if let syn::Fields::Named(named) = fields {
            for field in &named.named {
                if let Some(ident) = &field.ident {
                    self.note(ident);
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for Declarations {
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note_type(&node.ident);
        self.note_fields(&node.fields);
        syn::visit::visit_item_struct(self, node);
    }
    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note_type(&node.ident);
        for variant in &node.variants {
            self.note(&variant.ident);
            self.note_fields(&variant.fields);
        }
        syn::visit::visit_item_enum(self, node);
    }
    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note_type(&node.ident);
        for field in &node.fields.named {
            if let Some(ident) = &field.ident {
                self.note(ident);
            }
        }
        syn::visit::visit_item_union(self, node);
    }
    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note_type(&node.ident);
        syn::visit::visit_item_trait(self, node);
    }
    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note_type(&node.ident);
        syn::visit::visit_item_type(self, node);
    }
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note(&node.ident);
        syn::visit::visit_item_mod(self, node);
    }
    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note(&node.ident);
        syn::visit::visit_item_const(self, node);
    }
    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note(&node.ident);
        syn::visit::visit_item_static(self, node);
    }
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note_signature(&node.sig);
        syn::visit::visit_item_fn(self, node);
    }
    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note_signature(&node.sig);
        syn::visit::visit_impl_item_fn(self, node);
    }
    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note(&node.ident);
        syn::visit::visit_impl_item_const(self, node);
    }
    fn visit_impl_item_type(&mut self, node: &'ast syn::ImplItemType) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note(&node.ident);
        syn::visit::visit_impl_item_type(self, node);
    }
    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note_signature(&node.sig);
        syn::visit::visit_trait_item_fn(self, node);
    }
    fn visit_trait_item_const(&mut self, node: &'ast syn::TraitItemConst) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note(&node.ident);
        syn::visit::visit_trait_item_const(self, node);
    }
    fn visit_trait_item_type(&mut self, node: &'ast syn::TraitItemType) {
        if test_gated(&node.attrs) {
            return;
        }
        self.note(&node.ident);
        syn::visit::visit_trait_item_type(self, node);
    }
    fn visit_local(&mut self, node: &'ast syn::Local) {
        self.note_pattern(&node.pat);
        syn::visit::visit_local(self, node);
    }
    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        for input in &node.inputs {
            self.note_pattern(input);
        }
        syn::visit::visit_expr_closure(self, node);
    }
}

fn versioned_declarations(text: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(text) else {
        return Vec::new();
    };
    let mut declarations = Declarations::default();
    declarations.visit_file(&file);
    declarations.found.into_iter().collect()
}

fn declared_identifiers(text: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(text) else {
        return Vec::new();
    };
    let mut declarations = Declarations::default();
    declarations.visit_file(&file);
    declarations.all.into_iter().collect()
}

fn prefixed_error_types(text: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(text) else {
        return Vec::new();
    };
    let mut declarations = Declarations::default();
    declarations.visit_file(&file);
    declarations.prefixed_errors.into_iter().collect()
}

#[derive(Default)]
struct PublicFaultSurfaces(BTreeSet<String>);

impl PublicFaultSurfaces {
    fn note(&mut self, visibility: &syn::Visibility, ident: &syn::Ident) {
        if matches!(visibility, syn::Visibility::Public(_)) {
            let name = ident.to_string();
            let lower = name.to_ascii_lowercase();
            let operational = lower
                .split('_')
                .any(|part| part == "fault" || part == "injector");
            if operational {
                self.0.insert(name);
            }
        }
    }
}

impl<'ast> Visit<'ast> for PublicFaultSurfaces {
    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.ident);
            syn::visit::visit_item_type(self, node);
        }
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.ident);
            syn::visit::visit_item_const(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.sig.ident);
            syn::visit::visit_item_fn(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.sig.ident);
            syn::visit::visit_impl_item_fn(self, node);
        }
    }
}

fn public_fault_surfaces(text: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(text) else {
        return Vec::new();
    };
    let mut surfaces = PublicFaultSurfaces::default();
    surfaces.visit_file(&file);
    surfaces.0.into_iter().collect()
}

fn direct_public_modules(path: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path).expect("read crate root");
    let file = syn::parse_file(&text).expect("parse crate root");
    file.items
        .into_iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module)
                if matches!(module.vis, syn::Visibility::Public(_))
                    && !test_gated(&module.attrs) =>
            {
                Some(module.ident.to_string())
            }
            _ => None,
        })
        .collect()
}

/// Public type declarations in one source file.
///
/// Exec's vocabulary is a public contract, so private parsing helpers and test
/// fixtures are intentionally outside this inventory.
#[derive(Default)]
struct PublicTypes(BTreeSet<String>);

impl PublicTypes {
    fn note(&mut self, visibility: &syn::Visibility, ident: &syn::Ident) {
        if matches!(visibility, syn::Visibility::Public(_)) {
            self.0.insert(ident.to_string());
        }
    }
}

impl<'ast> Visit<'ast> for PublicTypes {
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.ident);
            syn::visit::visit_item_struct(self, node);
        }
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.ident);
            syn::visit::visit_item_enum(self, node);
        }
    }

    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.ident);
            syn::visit::visit_item_union(self, node);
        }
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.ident);
            syn::visit::visit_item_trait(self, node);
        }
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if !test_gated(&node.attrs) {
            self.note(&node.vis, &node.ident);
            syn::visit::visit_item_type(self, node);
        }
    }
}

fn public_type_names(text: &str) -> BTreeSet<String> {
    let file = syn::parse_file(text).expect("parse public type inventory");
    let mut types = PublicTypes::default();
    types.visit_file(&file);
    types.0
}

fn rejected_exec_type(name: &str) -> bool {
    const REJECTED: &[&str] = &[
        "ProviderAdvertisement",
        "ControlTopology",
        "PriorityContext",
    ];
    name.starts_with("Exec") || name.starts_with("Execution") || REJECTED.contains(&name)
}

fn rejected_find_type(name: &str) -> bool {
    const REJECTED: &[&str] = &[
        "Retrieval",
        "Pipeline",
        "QueryPlan",
        "ContextEngine",
        "TraversalProvider",
    ];
    name.starts_with("Find") || REJECTED.iter().any(|rejected| name.starts_with(rejected))
}

const RETIRED: &[&str] = &[
    "LaitDaemon",
    "LaitDaemonClient",
    "ControlRouter",
    "StationPlacement",
    "PlacementHost",
    "MemoryEngine",
    "JournaledStore",
    "ObjectRef",
    "CallerIndex",
    "StoreManifest",
    "RecoveryStatus",
    "RecoveryApproved",
    "DegradedRecoveryHolder",
    "AuthorityConfiguration",
    "AuthorityConfigurationId",
    "AuthorityScheme",
    "AuthorityId",
    "PrincipalId",
    "LeafId",
    "AuthoritySharePackage",
    "WorldCall",
    "WorldReply",
    "WorldCallHandler",
    "CallFailure",
    "CallFailureCode",
    "StationId",
    "StationEpoch",
    "SpaceFormationOptions",
    "EnterOptions",
    "ContactOptions",
    "OrbitObservation",
    "SessionOpen",
    "SessionAccept",
    "SessionRefusal",
    "ProtocolCapability",
    "PlaneWireError",
    "LiveSession",
    "SessionContext",
    "SessionQueue",
    "Fabric",
    "FabricKey",
    "FabricOp",
    "FabricTransactionRequest",
    "FabricCommitReceipt",
    "BodySchema",
    "BodyOp",
    "BodyDescriptor",
    "BodyTransaction",
    "TransactionSigner",
    "ActorPlane",
    "ContactMechanics",
    "PolicyGrant",
    "WorldRegistration",
    "TransientScope",
    concat!("Space", "Bri", "dge"),
    concat!("World", "Bri", "dge"),
    concat!("World", "Bri", "dgeRegistry"),
    concat!("World", "Bri", "dgesBuilder"),
    "OrbitalMechanics",
];

#[test]
fn compatibility_source_vocabulary_is_absent() {
    let root = workspace_root();
    let mut found = Vec::new();
    for file in production_sources() {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let lower = text.to_ascii_lowercase();
        // The word list is data, not vocabulary. It carries 2048 ordinary
        // English nouns and one of them is the banned term — a word a person
        // says out loud as part of their address, with no architectural claim
        // anywhere near it. Exempting the file rather than editing the list is
        // the only honest option: the list is reproduced verbatim, its 2048
        // entries are what the address keyspace is measured against, and a
        // codebase's naming discipline has no business reaching into English.
        if lower.contains(concat!("bri", "dge")) && rel != "crates/directory/src/words.rs" {
            found.push(format!("{rel}: retired boundary vocabulary"));
        }
        if lower.contains("#[path") || lower.contains("#[ path") {
            found.push(format!("{rel}: compatibility path module"));
        }
        if (rel.starts_with("crates/runtime/src/") || rel.starts_with("crates/replica/src/"))
            && lower.contains("issues")
        {
            found.push(format!("{rel}: product vocabulary"));
        }
    }
    assert!(
        found.is_empty(),
        "retired compatibility vocabulary in production sources:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn production_boundaries_expose_no_fault_injectors() {
    let root = workspace_root();
    let mut found = Vec::new();
    for file in production_sources() {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for name in public_fault_surfaces(&text) {
            found.push(format!("{rel}: `{name}`"));
        }
    }
    assert!(
        found.is_empty(),
        "fault injection escaped a production boundary:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn concept_crates_expose_only_their_semantic_namespaces() {
    let root = workspace_root();
    let expected: &[(&str, &[&str])] = &[
        (
            "correspondence",
            // The carrier seam is the crate root — `Carrier`, `Sealed`, `Missed`.
            // Two kinds of module hang off it, and both are legitimate:
            //
            // Contractors — `mem` (in-process, for tests) and `post` (the hosted
            // `lait-post` over HTTP). A new one here should be another contractor.
            //
            // The plane's own concepts — `letter` (what crosses: an invitation or
            // a message, sealed and signed), `mailbox` (the local inbox of opened
            // letters), and `watch` (noticing an arrival on a timer). These are the
            // correspondence domain, the same way `actor`/`egress`/`kinship` are
            // mechanics' domain — a concept crate names its concepts. What must not
            // appear is a *delivery-mechanism* opinion leaking up from a contractor
            // into the seam, which is the thing the seam exists not to have.
            //
            // `plane` joins them: the reach itself — announcing, learning,
            // resolving and sending — which moved down here from the client so
            // that products and the daemon are callers rather than owners. It is
            // a concept of this crate for the same reason the others are, and
            // it names no delivery mechanism.
            &["letter", "mailbox", "mem", "plane", "post", "watch"],
        ),
        ("fabric", &[]),
        ("journal", &[]),
        (
            "mechanics",
            &[
                "actor",
                "assignment",
                "authorization",
                // An append-only log that can prove its own memory: signed
                // heads, inclusion paths, consistency paths, and the reader's
                // monotonic ratchet over them. Public because it is the
                // artifact grammar *both* sides of that check share — the
                // registrar in `directory` mints heads and proofs, the daemon
                // in `display` pins one and asks every later head to prove it
                // extends the pin. A second copy of the shape is exactly how a
                // mirror gets to serve two readers two worlds, which is the
                // defect this module exists to make impossible.
                "chronicle",
                // Whose key is about to be spent on the way out. Public because
                // the crate that carries correspondence has to take the witness
                // as an argument — that is the whole enforcement mechanism, and a
                // witness type nobody outside can name cannot be an argument.
                //
                // Deliberately not folded into `authorization`: that module
                // answers whether an act is permitted, and this one answers whose
                // signature would make it. A grant can be revoked; a signature
                // made under somebody else's identity cannot be recalled, so the
                // two are not degrees of one question.
                "egress",
                "ids",
                // The Space-less relation plane: mutual device links, signed
                // audience-scoped avowals, and the projection that commits to
                // its log head. Public because it is a boundary others cross —
                // the address book resolves attested names against it, and it
                // is what the directory publishes. Deliberately not folded into
                // `actor`: that plane is per-Space by construction, and this
                // one is Space-less by construction.
                "kinship",
                "membership",
                // The balanced PAKE the device-pairing ceremony spends.
                // Public because a password-authenticated exchange is a
                // mechanic, not a daemon detail — and naming it here is
                // what keeps a second one from growing somewhere else.
                "pake",
                "policy",
                "recovery",
                // Owner-only, crash-safe storage for device-local secret
                // material. Public so process-level adapters (the display
                // coordinator's receiver proof keys) share Runtime custody's
                // DACL/DPAPI boundary instead of growing a second secret store.
                "secretfs",
                "space",
                "station",
                // Wall-clock time, with a freeze seam for tests. Public because
                // `lait` and `lait-issues-app` both need it and `mechanics` is
                // the crate they share — it replaced four private copies of the
                // same `now_secs()`. tokio's clock covers `Instant` and not
                // `SystemTime`, which is why this exists separately.
                "wallclock",
            ],
        ),
        (
            "replica",
            &[
                "body",
                "content",
                "convergence",
                "frontier",
                "manifest",
                "receipt",
                "transaction",
            ],
        ),
        (
            "runtime",
            &[
                "beacon",
                // The daemon's own Station machinery composed with no daemon
                // around it — a browser tab's engine over a pulled Space.
                // wasm32-only in cfg, but the name is part of the public
                // vocabulary either way.
                "browser",
                // The bounded description of what one accepted operation did,
                // which the product surfaces need in order to render an
                // outcome without re-reading the store.
                "change",
                "coordinates",
                // World-declared durable Runs and their Attempts. The module
                // supplies the context; its own naming gate below prevents
                // public types from repeating Exec or Execution.
                "exec",
                // Bounded World-declared reads. Find owns the generic Query,
                // Grant, vocabulary references, and work ceilings; product
                // meanings remain in their Worlds.
                "find",
                // Immutable Orbit materializations. The namespace keeps the
                // lifecycle sharp: Generation -> Build -> Verification ->
                // Activation, without prefix-stuttering every public type.
                // The identity-scoped correspondence dial tone
                // (`lait/correspondence/1`) — the mailbox's transport seam,
                // routed without a Space by design.
                "correspondence",
                "generation",
                "neighbor",
                "plane",
                // The exception, and deliberately a visible one: every other
                // name here is a concept this engine is *about*, and this one
                // is a mechanism — one policy for reading state a panicked
                // thread may have left half-written. It is public because the
                // locks it governs are held in three crates and the whole
                // point is that they answer alike; a second copy is how the
                // previous idiom ended up with five spellings and no
                // documentation. If this list grows a second mechanism, that
                // is the signal to give them a crate of their own instead.
                "poison",
                // The immutable read image an exact query is answered from.
                // Public because a caller pins one and asks again later; the
                // identity travels with every product page and receipt.
                "publication",
                "signal",
                "transient",
                "world",
            ],
        ),
    ];

    for (package, allowed) in expected {
        let found = direct_public_modules(&root.join("crates").join(package).join("src/lib.rs"));
        let allowed: BTreeSet<String> = allowed.iter().map(|name| (*name).to_owned()).collect();
        assert_eq!(found, allowed, "{package} public module allowlist changed");
    }
}

#[test]
fn exec_public_types_do_not_stutter_or_reintroduce_rejected_nouns() {
    let root = workspace_root();
    let mut found = Vec::new();
    for file in production_sources() {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if rel != "crates/runtime/src/exec.rs" && !rel.starts_with("crates/runtime/src/exec/") {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read runtime::exec source");
        for name in public_type_names(&text) {
            if rejected_exec_type(&name) {
                found.push(format!("{rel}: `{name}`"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "runtime::exec public types repeat their module or reintroduce rejected nouns:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn exec_naming_gate_has_teeth() {
    let bad = r#"
        pub struct ExecSpec;
        pub enum ExecutionRun { Began }
        pub type ProviderAdvertisement = ();
        struct ExecPrivateHelper;
        pub struct Spec;
    "#;
    let found: Vec<String> = public_type_names(bad)
        .into_iter()
        .filter(|name| rejected_exec_type(name))
        .collect();
    assert_eq!(found, ["ExecSpec", "ExecutionRun", "ProviderAdvertisement"]);
}

#[test]
fn find_public_types_use_only_the_contextual_vocabulary() {
    let root = workspace_root();
    let mut found = Vec::new();
    for file in production_sources() {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if rel != "crates/runtime/src/find.rs" && !rel.starts_with("crates/runtime/src/find/") {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read runtime::find source");
        for name in public_type_names(&text) {
            if rejected_find_type(&name) {
                found.push(format!("{rel}: `{name}`"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "runtime::find public types repeat their module or reintroduce rejected nouns:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn find_naming_gate_has_teeth() {
    let bad = r#"
        pub struct FindQuery;
        pub enum Retrieval { Exact }
        pub type Pipeline = ();
        pub struct QueryPlan;
        pub struct ContextEngine;
        pub trait TraversalProvider {}
        struct FindPrivateHelper;
        pub struct Query;
    "#;
    let found: Vec<String> = public_type_names(bad)
        .into_iter()
        .filter(|name| rejected_find_type(name))
        .collect();
    assert_eq!(
        found,
        [
            "ContextEngine",
            "FindQuery",
            "Pipeline",
            "QueryPlan",
            "Retrieval",
            "TraversalProvider"
        ]
    );
}

#[test]
fn retired_declarations_and_module_prefix_stutter_are_absent() {
    let root = workspace_root();
    let mut found = Vec::new();
    for file in production_sources() {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let stem = file
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let prefix = match stem {
            "fabric" => Some("Fabric"),
            "body" => Some("Body"),
            "transaction" => Some("Transaction"),
            "world" => Some("World"),
            "station" => Some("Station"),
            "plane" | "planes" => Some("Plane"),
            _ => None,
        };
        for name in declared_identifiers(&text) {
            if RETIRED.contains(&name.as_str()) {
                found.push(format!("{rel}: retired `{name}`"));
            }
            if let Some(prefix) = prefix {
                if name.starts_with(prefix) && name != prefix {
                    found.push(format!("{rel}: module-prefix stutter `{name}`"));
                }
            }
            let product_issue = name == "Issue"
                || name
                    .strip_prefix("Issue")
                    .and_then(|tail| tail.chars().next())
                    .is_some_and(char::is_uppercase);
            if (rel.starts_with("crates/runtime/src/") || rel.starts_with("crates/replica/src/"))
                && product_issue
            {
                found.push(format!("{rel}: product vocabulary `{name}`"));
            }
        }
        for name in prefixed_error_types(&text) {
            found.push(format!(
                "{rel}: prefixed error bag `{name}`; qualify Failure, Invalid, Refusal, or Interruption by its owner"
            ));
        }
    }
    assert!(
        found.is_empty(),
        "retired or non-semantic production declarations:\n  {}",
        found.join("\n  ")
    );
}

/// Deliberate exemptions, as `path<TAB>identifier<TAB>justification`. An entry
/// that no longer matches anything is itself a failure: a stale exemption reads
/// as coverage it no longer provides.
fn allowlist() -> Vec<(String, String)> {
    let path = workspace_root().join("tests/semantic-name-allowlist.tsv");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let file = parts.next()?.trim().to_string();
            let ident = parts.next()?.trim().to_string();
            let justification = parts.next().unwrap_or("").trim();
            assert!(
                !justification.is_empty(),
                "allowlist entry `{file} {ident}` carries no justification"
            );
            Some((file, ident))
        })
        .collect()
}

fn violations() -> Vec<(String, String)> {
    let root = workspace_root();
    let mut out = Vec::new();
    for file in production_sources() {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for name in versioned_declarations(&text) {
            out.push((rel.clone(), name));
        }
    }
    out
}

#[test]
fn no_production_identifier_carries_a_version_suffix() {
    let allowed = allowlist();
    let found: Vec<String> = violations()
        .into_iter()
        .filter(|entry| !allowed.contains(entry))
        .map(|(file, name)| format!("{file}: `{name}`"))
        .collect();
    assert!(
        found.is_empty(),
        "version-suffixed identifier declarations in production sources:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn every_allowlist_entry_still_applies() {
    let found = violations();
    let stale: Vec<String> = allowlist()
        .into_iter()
        .filter(|entry| !found.contains(entry))
        .map(|(file, name)| format!("{file}: `{name}`"))
        .collect();
    assert!(
        stale.is_empty(),
        "allowlist entries that no longer match anything — delete them:\n  {}",
        stale.join("\n  ")
    );
}

#[test]
fn the_gate_covers_every_production_package() {
    let root = workspace_root();
    let sources = production_sources();
    let covered = |needle: &str| {
        sources.iter().any(|p| {
            p.to_string_lossy()
                .replace('\\', "/")
                .contains(&format!("/{needle}/src/"))
        })
    };
    let mut missing = Vec::new();
    for group in ["crates", "products"] {
        for entry in std::fs::read_dir(root.join(group))
            .expect("group dir")
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().join("src").is_dir() && !covered(&name) {
                missing.push(format!("{group}/{name}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "packages the gate does not scan: {missing:?}"
    );
    assert!(
        sources.iter().any(|p| p.starts_with(root.join("src"))),
        "the root package's src/ must be scanned"
    );
}

#[test]
fn the_detector_has_teeth() {
    // Every declaration position the old line scanner could not reach.
    for sample in [
        "pub struct BodyTransactionV1 { field: u8 }",
        "struct Payload { payload_v2: u8 }",
        "pub enum Frame { HeaderV10 }",
        "pub enum Frame { Header { len_v2: u8 } }",
        "pub type SignedCoordinatesV1 = ();",
        "type ShimV1 = Real;",
        "fn decode_v1() {}",
        "fn decode(bytes_v2: &[u8]) {}",
        "pub const PRESENCE_ALPN_V1: &[u8] = b\"x\";",
        "static TABLE_V3: u8 = 0;",
        "mod wire_v1;",
        "impl T { fn read_v1(&self) {} }",
        "impl T { const LIMIT_V2: u8 = 0; }",
        "trait T { fn write_v1(&self); }",
        "fn f() { let parsed_v1 = 0; }",
        "fn f() { let (a, frame_v2) = (0, 0); }",
        "fn f() { let cb = |arg_v1: u8| arg_v1; }",
        "fn f() { let v1 = 0; }",
    ] {
        assert!(
            !versioned_declarations(sample).is_empty(),
            "detector missed: {sample}"
        );
    }
    // Semantic names, IP families, and versioned *string contents* are fine.
    for sample in [
        "pub struct Transaction { format_version: u8 }",
        "pub struct SignedCoordinates;",
        "pub struct Ipv4Header { ipv6: u8 }",
        "fn f() { use std::net::Ipv6Addr; }",
        "const DOMAIN: &str = \"lait.coordinates.v1\";",
        "fn f() { let alpn = b\"lait/contact/2\"; }",
        "fn f(protocol_version: u32) {}",
        "pub const CONTENT_FORMAT_VERSION: u8 = 1;",
        // Inline test modules inside src/ are tests, not production names.
        "#[cfg(test)] mod tests { fn t() { let v1 = 0; } }",
        "#[test] fn a_v1_file_migrates() { let v1 = 0; }",
    ] {
        assert!(
            versioned_declarations(sample).is_empty(),
            "false positive: {sample}"
        );
    }
}
