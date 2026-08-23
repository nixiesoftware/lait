//! What a World declares about itself: `world.json` (SUB-22).
//!
//! A World is an independently published application. It may serve a web head
//! over the local daemon, launch a native program, offer several of those at
//! once, or offer nothing to a person at all and exist only for an agent. The
//! declaration must therefore say *what it is and how to reach it* rather than
//! assume a shape — the mistake the first cut of the delivery layer made when
//! it named its staging directory after web heads.
//!
//! ## Only what something reads
//!
//! Every field here is one this crate must support forever: the format
//! version makes *adding* one a non-event and removing one a breaking change,
//! so the set stays limited to what is actually consulted: identity,
//! presentation, compatibility, native runners, and typed launch entries.
//!
//! ## The shape is borrowed, not invented
//!
//! Seven shipping systems — Steam, GOG Galaxy, itch.io, AppStream, Desktop
//! Entry, Snap, Homebrew Cask — arrived independently at the same three-axis
//! cut, and none of them uses one field for two axes:
//!
//! * a **catalog kind**, which decides how a thing is shelved and never how it
//!   is launched;
//! * a **list of typed launchables**, which is the load-bearing axis;
//! * **presentation**, orthogonal to both.
//!
//! Two sub-rules recur just as widely and are followed here. Applicability
//! predicates live on the *launchable*, not on the package, because one
//! release routinely offers a Windows entry and a Linux entry. And "no user
//! interface at all" is never a kind: it is the absence of a primary
//! launchable — the same answer `NoDisplay=true`, Snap's `daemon:` without
//! `desktop:`, and Cask's `stage_only` all give.
//!
//! ## Compatibility is a named requirement, never an address
//!
//! An earlier cut keyed a World's artifacts by a derived fingerprint of this
//! build, so a bundle that did not match was *not found*. That is elegant and
//! wrong: the fingerprint covered every schema and every World's
//! implementation, so a change touching none of a publisher's dependencies
//! still invalidated their bundle and forced a republish. Flatpak states
//! `required-flatpak` beside `command` rather than in place of a lookup key,
//! and Steam names a compatibility tool that resolves; [`Requirement`] follows
//! them. A requirement is a named fact and a range over it, evaluated against
//! what the host actually offers, so a bundle keeps working across every
//! change that does not concern it.
//!
//! ## It travels inside the payload
//!
//! `world.json` sits at the bundle root, so it is covered by the artifact
//! digest and inherits the feed's signature for free — and the delivery layer
//! learns nothing about Worlds. GOG's content system v1 put launch information
//! in a server-side document and v2 moved it into the shipped payload; this
//! starts where they ended up.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::artwork_bounds;

/// The manifest format this build understands.
///
/// An unknown *major* is refused by name rather than guessed at: a manifest
/// from the future may mean something this build would misread, and a World
/// that silently half-loads is worse than one that plainly does not.
pub const FORMAT: u32 = 1;

/// The whole of `world.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldManifest {
    /// The declaration format. See [`FORMAT`].
    pub format: u32,
    /// The World's address: reverse-domain, lowercase, immutable forever.
    pub id: String,
    /// This release's version.
    pub version: String,
    /// Stable namespace mounted by HTTP and MCP heads.
    ///
    /// Older format-1 bundles may omit it and fall back to the final id
    /// segment; new publishers should always state it.
    #[serde(default)]
    pub mount: Option<String>,
    /// What to call it in a list.
    ///
    /// Absent falls back to the last segment of the id — which is at least a
    /// real word rather than an address, and is what a World that has not
    /// bothered to name itself deserves to be called.
    #[serde(default)]
    pub name: Option<String>,
    /// One-line Library description.
    #[serde(default)]
    pub tagline: Option<String>,
    /// Preferred semantic implementation version, for passive Library rows.
    #[serde(default)]
    pub implementation_version: Option<u32>,
    /// The square mark a Library row draws, as a path into the bundle.
    ///
    /// Held to the common artwork bounds — a real PNG, square, and within the
    /// size a client draws — at staging, because that is where the bytes are.
    #[serde(default)]
    pub mark: Option<String>,
    /// The frame drawn behind this World's title on a detail surface, as a
    /// path into the bundle.
    ///
    /// Separate from the mark because they are drawn at sizes an order apart:
    /// detail that reads at 200 pixels is mud at 24, and art composed for 24
    /// is four bland shapes at 200.
    #[serde(default)]
    pub hero: Option<String>,
    /// The colour this World is drawn from, packed `0xRRGGBB`.
    ///
    /// A seed, not an asset: a client derives whatever it needs from this one
    /// number, locally.
    #[serde(default)]
    pub accent: Option<u32>,
    /// What the host must offer for this World to run at all.
    #[serde(default)]
    pub requires: Vec<Requirement>,
    /// The independently executable service implementations in this release.
    ///
    /// Kept separate from `launch`: this is how the host runs the World, while
    /// launch entries are the surfaces a person or agent can enter. A web
    /// surface is not a process and a process is not presentation.
    #[serde(default)]
    pub runners: Vec<Runner>,
    /// Everything a person or an agent can start.
    ///
    /// May be empty, and that is a statement rather than an omission: a World
    /// with nothing here offers no way in of its own, which is the honest
    /// declaration for an agent-only surface.
    #[serde(default)]
    pub launch: Vec<Launch>,
}

impl WorldManifest {
    /// What to call this World.
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .unwrap_or_else(|| self.id.rsplit('.').next().unwrap_or(&self.id))
    }

    pub fn mount(&self) -> &str {
        self.mount
            .as_deref()
            .unwrap_or_else(|| self.id.rsplit('.').next().unwrap_or(&self.id))
    }
}

/// One platform-specific World service executable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runner {
    /// Whether this implementation is selected for newly formed Spaces.
    ///
    /// Historical implementations may remain in the same immutable release so
    /// retained publications and explicit migrations can resolve their exact
    /// code. Exactly one applicable runner must be preferred; the host refuses
    /// ambiguity instead of choosing by declaration order.
    #[serde(default)]
    pub preferred: bool,
    /// Where this executable applies. Absent means everywhere.
    #[serde(default)]
    pub when: Option<When>,
    /// Path to the executable, relative to the immutable release root.
    pub program: String,
    /// Arguments passed directly, never through a shell.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory relative to the release root. Absent uses the root.
    #[serde(default)]
    pub cwd: Option<String>,
}

impl Runner {
    pub fn admits(&self, os: &str, arch: &str) -> bool {
        self.when
            .as_ref()
            .is_none_or(|condition| condition.admits(os, arch))
    }
}

/// A fact the host must offer, and the range this World runs against.
///
/// The name is a dotted, host-defined key (`lait.control`,
/// `lait.world.<id>.schema`). The range is semver, so a fact that moves for a
/// reason unrelated to this World leaves it working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// The fact's name.
    pub name: String,
    /// A semver range over it, in `VersionReq` syntax.
    pub range: String,
}

/// Why a requirement was not met. Each arm is a different fact, and only one
/// of them means the World is too new for this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unmet {
    /// The host does not offer this fact at all.
    Unknown {
        /// The fact the World asked for.
        name: String,
    },
    /// The host offers it at a version outside the range.
    OutOfRange {
        /// The fact's name.
        name: String,
        /// What the host offers.
        offered: String,
        /// What the World asked for.
        wanted: String,
    },
    /// The range itself could not be parsed — a publisher's error, and not
    /// something to satisfy by guessing.
    Unreadable {
        /// The fact's name.
        name: String,
        /// The range as written.
        range: String,
    },
}

impl std::fmt::Display for Unmet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { name } => {
                write!(f, "this build offers no `{name}`")
            }
            Self::OutOfRange {
                name,
                offered,
                wanted,
            } => write!(f, "`{name}` is {offered} here and the World needs {wanted}"),
            Self::Unreadable { name, range } => {
                write!(
                    f,
                    "`{name}` asks for {range:?}, which is not a version range"
                )
            }
        }
    }
}

/// Something a person or an agent can start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    /// Stable, publisher-chosen, and the address a deep link uses.
    pub id: String,
    /// Whether a client offers this entry, and how prominently.
    #[serde(default)]
    pub present: Present,
    /// Where this entry applies. Absent means everywhere.
    #[serde(default)]
    pub when: Option<When>,
    /// What starting it actually does.
    pub target: Target,
}

/// How prominently a client offers a launch entry. Closed, because a client
/// has to decide something and an unknown value would have to become one of
/// these anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Present {
    /// The obvious way in. At most one entry should claim it per condition.
    Primary,
    /// Offered, but not the first thing.
    #[default]
    Listed,
    /// Reachable by id, never drawn. A repair step, a diagnostic.
    Hidden,
}

/// Where a launch entry applies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct When {
    /// Operating systems, as Rust spells them (`windows`, `macos`, `linux`).
    /// Empty means every one.
    #[serde(default)]
    pub os: Vec<String>,
    /// Architectures, as Rust spells them (`x86_64`, `aarch64`). Empty means
    /// every one.
    #[serde(default)]
    pub arch: Vec<String>,
}

impl When {
    /// Whether this condition admits the given host.
    pub fn admits(&self, os: &str, arch: &str) -> bool {
        (self.os.is_empty() || self.os.iter().any(|o| o == os))
            && (self.arch.is_empty() || self.arch.iter().any(|a| a == arch))
    }
}

/// What starting an entry does. Tagged, so a target this build cannot perform
/// is refused by name rather than misread as one it can.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Target {
    /// A web surface the local daemon serves over loopback.
    Web {
        /// The path a client opens, absolute and within the bundle.
        path: String,
    },
    /// A program in the bundle the client starts.
    Exec {
        /// Path to the program, relative to the bundle root. Re-proved to be
        /// inside the bundle at launch time, never only at declaration.
        program: String,
        /// Arguments, as a list. Never a shell string: a string means a
        /// quoting dialect, and every launcher that took one now maintains a
        /// parser for it.
        #[serde(default)]
        args: Vec<String>,
        /// The working directory, relative to the bundle root. Stated rather
        /// than inferred from `program`, because programs exist that need
        /// them to differ.
        #[serde(default)]
        cwd: Option<String>,
    },
    /// A URL a client opens in the person's browser.
    Url {
        /// The address.
        url: String,
    },
    /// A target this build does not know how to perform.
    ///
    /// Kept rather than refused at parse time, so one unreadable entry does
    /// not discard a manifest whose other entries are fine. Offering it is
    /// what must not happen.
    #[serde(other)]
    Unsupported,
}

impl WorldManifest {
    /// Parse and check a `world.json`.
    ///
    /// Structural rules only — whether the host satisfies it is
    /// [`Self::unmet`], and whether the files it names exist is the caller's,
    /// at the moment it uses them.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|error| format!("world.json: {error}"))?;
        if manifest.format != FORMAT {
            return Err(format!(
                "world.json declares format {} and this build reads {FORMAT}",
                manifest.format
            ));
        }
        if manifest.id.is_empty()
            || !manifest.id.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
            })
        {
            return Err(format!(
                "world.json id {:?} is not a lowercase reverse-domain address",
                manifest.id
            ));
        }
        if manifest.mount().is_empty()
            || !manifest.mount().bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
        {
            return Err(format!(
                "world.json mount {:?} is invalid",
                manifest.mount()
            ));
        }
        if manifest
            .tagline
            .as_ref()
            .is_some_and(|tagline| tagline.trim().is_empty() || tagline.chars().count() > 96)
        {
            return Err("world.json tagline must be 1..=96 characters".to_string());
        }
        if manifest.implementation_version == Some(0) {
            return Err("world.json implementation_version must be non-zero".to_string());
        }
        for (kind, path) in [("mark", &manifest.mark), ("hero", &manifest.hero)] {
            let Some(path) = path else { continue };
            if path.is_empty() || path.starts_with('/') || path.contains("..") {
                return Err(format!(
                    "world.json points its {kind} at {path:?}, which is not a path inside \
                     the bundle"
                ));
            }
        }
        for runner in &manifest.runners {
            validate_bundle_path("runner program", &runner.program)?;
            if let Some(cwd) = &runner.cwd {
                validate_bundle_path("runner working directory", cwd)?;
            }
        }
        let applicable = manifest
            .runners
            .iter()
            .filter(|runner| runner.admits(std::env::consts::OS, std::env::consts::ARCH));
        let preferred = applicable.clone().filter(|runner| runner.preferred).count();
        let total = applicable.count();
        if total > 0 && preferred != 1 {
            return Err(format!(
                "world.json declares {preferred} preferred runners for this platform; exactly one is required"
            ));
        }
        let mut seen = BTreeMap::new();
        for entry in &manifest.launch {
            if entry.id.is_empty() {
                return Err("a launch entry has no id".to_string());
            }
            if seen.insert(entry.id.clone(), ()).is_some() {
                return Err(format!("two launch entries share the id {:?}", entry.id));
            }
            if let Target::Web { path } = &entry.target {
                if !path.starts_with('/') || path.contains("..") {
                    return Err(format!(
                        "launch {:?} opens {path:?}, which is not an absolute path inside the bundle",
                        entry.id
                    ));
                }
            }
            if let Target::Exec { program, .. } = &entry.target {
                if program.is_empty() || program.starts_with('/') || program.contains("..") {
                    return Err(format!(
                        "launch {:?} runs {program:?}, which is not a path inside the bundle",
                        entry.id
                    ));
                }
            }
        }
        Ok(manifest)
    }

    /// Which of this World's requirements the host does not meet.
    ///
    /// Empty means it runs here. Every unmet requirement is returned rather
    /// than the first, because a publisher fixing one at a time is a
    /// publisher making several releases to learn one answer.
    pub fn unmet(&self, offers: &BTreeMap<String, semver::Version>) -> Vec<Unmet> {
        self.requires
            .iter()
            .filter_map(|requirement| {
                let Ok(range) = semver::VersionReq::parse(&requirement.range) else {
                    return Some(Unmet::Unreadable {
                        name: requirement.name.clone(),
                        range: requirement.range.clone(),
                    });
                };
                let Some(offered) = offers.get(&requirement.name) else {
                    return Some(Unmet::Unknown {
                        name: requirement.name.clone(),
                    });
                };
                (!range.matches(offered)).then(|| Unmet::OutOfRange {
                    name: requirement.name.clone(),
                    offered: offered.to_string(),
                    wanted: requirement.range.clone(),
                })
            })
            .collect()
    }

    /// The entries a client should offer on this host, most prominent first.
    pub fn offerable(&self, os: &str, arch: &str) -> Vec<&Launch> {
        let mut entries: Vec<&Launch> = self
            .launch
            .iter()
            .filter(|entry| entry.present != Present::Hidden)
            .filter(|entry| !matches!(entry.target, Target::Unsupported))
            .filter(|entry| entry.when.as_ref().is_none_or(|when| when.admits(os, arch)))
            .collect();
        entries.sort_by_key(|entry| match entry.present {
            Present::Primary => 0,
            Present::Listed => 1,
            Present::Hidden => 2,
        });
        entries
    }
}

fn validate_bundle_path(kind: &str, path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains("..")
        || std::path::Path::new(path).is_absolute()
    {
        return Err(format!(
            "world.json declares {kind} {path:?}, which is not a path inside the bundle"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(json: serde_json::Value) -> Result<WorldManifest, String> {
        WorldManifest::parse(json.to_string().as_bytes())
    }

    fn minimal() -> serde_json::Value {
        serde_json::json!({
            "format": 1,
            "id": "world.lait.issues",
            "version": "0.8.0",
        })
    }

    #[test]
    fn a_world_that_offers_no_way_in_is_a_statement_rather_than_an_omission() {
        let parsed = manifest(minimal()).expect("a manifest with no launch entries is valid");
        assert!(parsed.launch.is_empty());
        assert!(
            parsed.offerable("linux", "x86_64").is_empty(),
            "a World with nothing to launch offered something"
        );
    }

    #[test]
    fn a_manifest_from_a_future_format_is_refused_by_name() {
        let mut doc = minimal();
        doc["format"] = serde_json::json!(2);
        let error = manifest(doc).expect_err("an unknown format must refuse");
        assert!(
            error.contains("format 2") && error.contains("reads 1"),
            "{error}"
        );
    }

    #[test]
    fn artwork_must_be_named_as_a_path_inside_the_bundle() {
        for kind in ["mark", "hero"] {
            for path in ["/etc/passwd", "../escape.png", ""] {
                let mut doc = minimal();
                doc[kind] = serde_json::json!(path);
                let error = manifest(doc).expect_err("an escaping artwork path must refuse");
                assert!(
                    error.contains("inside the bundle"),
                    "{kind} {path:?}: {error}"
                );
            }
        }
        let mut doc = minimal();
        doc["mark"] = serde_json::json!("art/mark.png");
        doc["hero"] = serde_json::json!("art/hero.png");
        doc["accent"] = serde_json::json!(0x31_7A_6D);
        let parsed = manifest(doc).expect("artwork inside the bundle is valid");
        assert_eq!(parsed.mark.as_deref(), Some("art/mark.png"));
        assert_eq!(parsed.accent, Some(0x31_7A_6D));
    }

    /// A World that has not named itself is called something readable rather
    /// than being drawn as an address.
    #[test]
    fn a_world_without_a_name_falls_back_to_the_last_segment_of_its_id() {
        let unnamed = manifest(minimal()).expect("a manifest without a name is valid");
        assert_eq!(unnamed.display_name(), "issues");

        let mut doc = minimal();
        doc["name"] = serde_json::json!("Issues");
        assert_eq!(manifest(doc).expect("valid").display_name(), "Issues");
    }

    #[test]
    fn a_web_target_may_not_address_outside_the_bundle() {
        for path in ["../escape", "relative", "/ok/../../escape"] {
            let mut doc = minimal();
            doc["launch"] = serde_json::json!([{
                "id": "web", "target": { "type": "web", "path": path }
            }]);
            let error = manifest(doc).expect_err("an escaping path must refuse");
            assert!(error.contains("inside the bundle"), "{path}: {error}");
        }
    }

    #[test]
    fn an_exec_target_may_not_address_outside_the_bundle() {
        for program in ["/usr/bin/anything", "../escape", ""] {
            let mut doc = minimal();
            doc["launch"] = serde_json::json!([{
                "id": "run", "target": { "type": "exec", "program": program }
            }]);
            let error = manifest(doc).expect_err("an escaping program must refuse");
            assert!(error.contains("inside the bundle"), "{program:?}: {error}");
        }
    }

    /// One unreadable entry must not discard the entries around it — but it
    /// must never be offered either.
    #[test]
    fn an_unsupported_target_is_kept_and_never_offered() {
        let mut doc = minimal();
        doc["launch"] = serde_json::json!([
            { "id": "future", "target": { "type": "holotape", "reel": 3 } },
            { "id": "web", "present": "primary", "target": { "type": "web", "path": "/" } },
        ]);
        let parsed = manifest(doc).expect("an unknown target does not discard the manifest");
        assert_eq!(parsed.launch.len(), 2);
        let offered = parsed.offerable("macos", "aarch64");
        assert_eq!(offered.len(), 1, "an unperformable target was offered");
        assert_eq!(offered[0].id, "web");
    }

    #[test]
    fn conditions_live_on_the_entry_so_one_release_serves_every_host() {
        let mut doc = minimal();
        doc["launch"] = serde_json::json!([
            { "id": "win", "when": { "os": ["windows"] },
              "target": { "type": "exec", "program": "bin/world.exe" } },
            { "id": "nix", "when": { "os": ["linux", "macos"] },
              "target": { "type": "exec", "program": "bin/world" } },
            { "id": "web", "target": { "type": "web", "path": "/" } },
        ]);
        let parsed = manifest(doc).expect("a multi-host manifest is valid");
        let names = |os, arch| {
            parsed
                .offerable(os, arch)
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("windows", "x86_64"), vec!["win", "web"]);
        assert_eq!(names("linux", "x86_64"), vec!["nix", "web"]);
        assert_eq!(names("macos", "aarch64"), vec!["nix", "web"]);
    }

    #[test]
    fn one_applicable_runner_must_be_the_formation_default() {
        let mut doc = minimal();
        doc["runners"] = serde_json::json!([
            { "program": "bin/current" },
            { "program": "bin/migrator" }
        ]);
        let error = manifest(doc).expect_err("an ambiguous runner set must refuse");
        assert!(error.contains("exactly one"), "{error}");

        let mut doc = minimal();
        doc["runners"] = serde_json::json!([
            { "program": "bin/current", "preferred": true },
            { "program": "bin/migrator" }
        ]);
        let parsed = manifest(doc).expect("one preferred runner is unambiguous");
        assert_eq!(parsed.runners.len(), 2);
        assert!(parsed.runners[0].preferred);
    }

    #[test]
    fn the_primary_entry_is_offered_first_and_hidden_ones_never() {
        let mut doc = minimal();
        doc["launch"] = serde_json::json!([
            { "id": "repair", "present": "hidden", "target": { "type": "web", "path": "/repair" } },
            { "id": "docs", "target": { "type": "url", "url": "https://example.invalid" } },
            { "id": "app", "present": "primary", "target": { "type": "web", "path": "/" } },
        ]);
        let parsed = manifest(doc).expect("valid");
        let offered = parsed.offerable("linux", "x86_64");
        assert_eq!(
            offered.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["app", "docs"]
        );
    }

    #[test]
    fn two_launch_entries_may_not_share_an_id() {
        let mut doc = minimal();
        doc["launch"] = serde_json::json!([
            { "id": "app", "target": { "type": "web", "path": "/" } },
            { "id": "app", "target": { "type": "web", "path": "/other" } },
        ]);
        let error = manifest(doc).expect_err("a duplicate id must refuse");
        assert!(error.contains("share the id"), "{error}");
    }

    /// The correction the whole requirement model exists for: a fact moving
    /// for a reason unrelated to a World must leave that World working.
    #[test]
    fn a_requirement_survives_changes_it_does_not_name() {
        let mut doc = minimal();
        doc["requires"] = serde_json::json!([
            { "name": "lait.control", "range": ">=13, <14" },
        ]);
        let parsed = manifest(doc).expect("valid");

        let offers = |control: &str, schema: &str| {
            BTreeMap::from([
                (
                    "lait.control".to_string(),
                    semver::Version::parse(control).unwrap(),
                ),
                (
                    "lait.world.issues.schema".to_string(),
                    semver::Version::parse(schema).unwrap(),
                ),
            ])
        };
        assert!(
            parsed.unmet(&offers("13.0.0", "3.0.0")).is_empty(),
            "a satisfied requirement was reported unmet"
        );
        // An unrelated schema bump. Under the old fingerprint every published
        // bundle became unfetchable here; under a named range nothing moves.
        assert!(
            parsed.unmet(&offers("13.0.0", "4.0.0")).is_empty(),
            "an unrelated fact moved and took this World down with it"
        );
        // The fact it does name, moving out of range.
        assert_eq!(
            parsed.unmet(&offers("14.0.0", "3.0.0")),
            vec![Unmet::OutOfRange {
                name: "lait.control".into(),
                offered: "14.0.0".into(),
                wanted: ">=13, <14".into(),
            }]
        );
    }

    #[test]
    fn every_unmet_requirement_is_reported_and_each_kind_says_which_it_is() {
        let mut doc = minimal();
        doc["requires"] = serde_json::json!([
            { "name": "lait.control", "range": ">=99" },
            { "name": "lait.nonexistent", "range": ">=1" },
            { "name": "lait.mangled", "range": "not a range" },
        ]);
        let parsed = manifest(doc).expect("valid");
        let offers = BTreeMap::from([(
            "lait.control".to_string(),
            semver::Version::parse("13.0.0").unwrap(),
        )]);
        let unmet = parsed.unmet(&offers);
        assert_eq!(unmet.len(), 3, "only some unmet requirements were reported");
        assert!(matches!(unmet[0], Unmet::OutOfRange { .. }));
        assert!(matches!(unmet[1], Unmet::Unknown { .. }));
        assert!(matches!(unmet[2], Unmet::Unreadable { .. }));
        // Each says which kind of "no" it is; a client rendering them must be
        // able to tell "too old" from "never heard of it".
        let said = unmet.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(said[0].contains("is 13.0.0 here"), "{said:?}");
        assert!(said[1].contains("offers no"), "{said:?}");
        assert!(said[2].contains("not a version range"), "{said:?}");
    }
}
