// Clap's data registry uses enum discriminants and validated numeric ranges when
// adapting its declarative command specification.
#![allow(clippy::as_conversions)]

//! Programmatic clap command registry.
//!
//! The CLI command set is defined as **data** — a `Vec<Spec>` built by
//! [`specs`] — instead of a `#[derive(Parser)]` enum. [`build_cli`] turns that
//! data into a `clap::Command` at runtime, so completions (`clap_complete`) and
//! the man page (`clap_mangen`) still generate from the live tree exactly as
//! before. Space/daemon commands build `control::Request`; installed World
//! packages mount and parse their own command namespaces.
//!
//! Why data-driven: a command is now one [`Spec`] entry mapping parsed args to a
//! single [`ClientAction`] (or a `Special` handler), which is the same registry
//! other surfaces (MCP) can derive from instead of re-declaring the command list.
//! The trade vs. the derive macro: `ArgMatches` lookups are keyed by string, so a
//! name typo is a runtime, not compile-time, error — concentrated inside each
//! spec's `to_request` closure and covered by `tests/cli_parse.rs`.

use anyhow::{anyhow, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use clap_complete::Shell;

use crate::{
    client_action::ClientAction,
    control::Request,
    install::{Client, Scope},
};

/// How a resolved leaf command is executed.
#[derive(Clone, Copy)]
pub enum Dispatch {
    /// Build a Space/daemon `Request`, capture its terminal orbital target as a
    /// `ClientAction`, then round-trip and render.
    Action(fn(&ArgMatches) -> Result<Request>),
    /// A command with bespoke handling in `app::run` (spawns a daemon, mints a
    /// key, custom output). The arg reading lives in the matching handler.
    Special(Special),
}

/// The commands `app::run` handles by hand (they do more than one `Request`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Special {
    Init,
    Id,
    Daemon,
    Mcp,
    InstallMcp,
    Serve,
    Invite,
    Join,
    Watch,
    Completions,
    Man,
    Profiles,
    Resume,
    Orbits,
    OrbitsForget,
    OrbitsPrune,
    ConfigGet,
    ConfigSet,
    ConfigUnset,
    ConfigList,
    Context,
    Worlds,
    Rebuild,
    Update,
    /// New-machine side of device enrollment: consume a `device invite` token
    /// and print a consent blob (no daemon, no store — just this identity).
    DeviceAccept,
}

/// One command (or nested group) in the tree.
pub struct Spec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub about: &'static str,
    pub args: Vec<ArgSpec>,
    pub subs: Vec<Spec>,
    /// Require a subcommand (a group with no bare form, e.g. `remote`).
    pub sub_required: bool,
    /// Escape hatch for arg shapes ArgSpec doesn't model (value-enums, etc.).
    pub customize: Option<fn(Command) -> Command>,
    pub dispatch: Dispatch,
    /// A long-running networked service (`daemon`, `mcp`) that must keep Rust's
    /// default SIGPIPE-ignored so a dropped socket returns EPIPE, not a kill.
    pub service: bool,
    /// Help-screen bucket (clap `display_order`): the first screen leads with
    /// the daily loop, registries and node plumbing sink to the bottom.
    pub order: usize,
}

impl Spec {
    /// A leaf command mapping args → one `Request`.
    fn req(
        name: &'static str,
        about: &'static str,
        args: Vec<ArgSpec>,
        f: fn(&ArgMatches) -> Result<Request>,
    ) -> Spec {
        Spec {
            name,
            aliases: &[],
            about,
            args,
            subs: Vec::new(),
            sub_required: false,
            customize: None,
            dispatch: Dispatch::Action(f),
            service: false,
            order: ORDER_DEFAULT,
        }
    }

    /// A leaf command handled by a bespoke `Special` arm.
    fn special(name: &'static str, about: &'static str, args: Vec<ArgSpec>, s: Special) -> Spec {
        Spec {
            name,
            aliases: &[],
            about,
            args,
            subs: Vec::new(),
            sub_required: false,
            customize: None,
            dispatch: Dispatch::Special(s),
            service: false,
            order: ORDER_DEFAULT,
        }
    }

    fn alias(mut self, a: &'static [&'static str]) -> Spec {
        self.aliases = a;
        self
    }
    fn service(mut self) -> Spec {
        self.service = true;
        self
    }
    fn customize(mut self, f: fn(Command) -> Command) -> Spec {
        self.customize = Some(f);
        self
    }
}

/// One argument, modelled declaratively. Every value is a `String`; numerics are
/// parsed in the `to_request` closure (keeps this type free of clap value-parser
/// generics). Exotic parsers (shell/client/scope value-enums) go via `customize`.
pub struct ArgSpec {
    name: &'static str,
    short: Option<char>,
    long: Option<&'static str>,
    help: &'static str,
    action: Act,
    required: bool,
    default: Option<&'static str>,
    value_name: Option<&'static str>,
    allow_hyphen: bool,
    trailing: bool,
    conflicts: &'static [&'static str],
}

enum Act {
    Set,
    Append,
    Flag,
}

impl ArgSpec {
    fn base(
        name: &'static str,
        help: &'static str,
        long: Option<&'static str>,
        action: Act,
    ) -> Self {
        ArgSpec {
            name,
            short: None,
            long,
            help,
            action,
            required: false,
            default: None,
            value_name: None,
            allow_hyphen: false,
            trailing: false,
            conflicts: &[],
        }
    }

    /// `--name <v>` (optional value).
    pub fn val(name: &'static str, help: &'static str) -> Self {
        Self::base(name, help, Some(name), Act::Set)
    }
    /// `--name` (boolean).
    pub fn flag(name: &'static str, help: &'static str) -> Self {
        Self::base(name, help, Some(name), Act::Flag)
    }
    /// `--name <v>` repeatable (collected into a `Vec`).
    pub fn multi(name: &'static str, help: &'static str) -> Self {
        Self::base(name, help, Some(name), Act::Append)
    }
    /// A required positional.
    pub fn pos(name: &'static str, help: &'static str) -> Self {
        let mut a = Self::base(name, help, None, Act::Set);
        a.required = true;
        a
    }
    /// An optional positional.
    pub fn pos_opt(name: &'static str, help: &'static str) -> Self {
        Self::base(name, help, None, Act::Set)
    }
    /// A variadic positional (collected into a `Vec`).
    pub fn pos_multi(name: &'static str, help: &'static str) -> Self {
        Self::base(name, help, None, Act::Append)
    }

    pub fn short(mut self, c: char) -> Self {
        self.short = Some(c);
        self
    }
    /// Override the `--long` when it differs from the arg id (kebab vs snake).
    pub fn long(mut self, l: &'static str) -> Self {
        self.long = Some(l);
        self
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn default(mut self, d: &'static str) -> Self {
        self.default = Some(d);
        self
    }
    pub fn value_name(mut self, v: &'static str) -> Self {
        self.value_name = Some(v);
        self
    }
    /// Let a value begin with `-` (so `label ENG-1 -wip` isn't read as a flag).
    pub fn hyphen(mut self) -> Self {
        self.allow_hyphen = true;
        self
    }
    pub fn trailing(mut self) -> Self {
        self.trailing = true;
        self
    }
    pub fn conflicts(mut self, c: &'static [&'static str]) -> Self {
        self.conflicts = c;
        self
    }

    fn is_positional(&self) -> bool {
        self.long.is_none() && self.short.is_none()
    }

    fn to_arg(&self) -> Arg {
        let mut a = Arg::new(self.name).help(self.help);
        if let Some(l) = self.long {
            a = a.long(l);
        }
        if let Some(s) = self.short {
            a = a.short(s);
        }
        match self.action {
            Act::Flag => a = a.action(ArgAction::SetTrue),
            Act::Append => {
                a = a.action(ArgAction::Append);
                if self.is_positional() {
                    a = a.num_args(0..);
                }
            }
            Act::Set => {}
        }
        if self.required {
            a = a.required(true);
        }
        if let Some(d) = self.default {
            a = a.default_value(d);
        }
        if let Some(v) = self.value_name {
            a = a.value_name(v);
        }
        if self.allow_hyphen {
            a = a.allow_hyphen_values(true);
        }
        if self.trailing {
            a = a.trailing_var_arg(true);
        }
        if !self.conflicts.is_empty() {
            a = a.conflicts_with_all(self.conflicts.iter().copied());
        }
        a
    }
}

/// Build the root `clap::Command` from the registry. Fed verbatim to
/// `clap_complete::generate` and `clap_mangen::Man::new`, so completions and the
/// man page stay generated from the live tree.
pub fn build_cli(specs: &[Spec]) -> Command {
    let mut root = Command::new("lait")
        .version(env!("LAIT_VERSION_LONG"))
        .about("Navigate local-first Spaces and the Worlds inside them")
        // No subcommand required: bare `lait` reports the current orbital
        // context. Product focus views live under their own command packages.
        .arg(
            Arg::new("home")
                .long("home")
                .global(true)
                .action(ArgAction::Set)
                .help_heading(GLOBAL_HEADING)
                .help("Select the node's home directory (overrides $LAIT_HOME)."),
        )
        .arg(
            Arg::new("orbit")
                .long("orbit")
                .global(true)
                .action(ArgAction::Set)
                .conflicts_with("home")
                .value_name("SEL")
                .help_heading(GLOBAL_HEADING)
                .help(
                    "Select a local Orbit by name, orb_/ws_ id (or prefix), or path \
                     (see `lait orbits`).",
                ),
        )
        .arg(
            Arg::new("json")
                .long("json")
                .global(true)
                .action(ArgAction::SetTrue)
                .help_heading(GLOBAL_HEADING)
                .help("Emit the versioned JSON DTO instead of human output."),
        )
        .arg(
            Arg::new("yes")
                .short('y')
                .long("yes")
                .global(true)
                .action(ArgAction::SetTrue)
                .help_heading(GLOBAL_HEADING)
                .help("Assume yes: skip confirmation prompts (for scripts and CI)."),
        )
        .arg(
            Arg::new("no_color")
                .long("no-color")
                .global(true)
                .action(ArgAction::SetTrue)
                .help_heading(GLOBAL_HEADING)
                .help("Disable ANSI colours."),
        );
    for s in specs {
        root = root.subcommand(build_sub(s));
    }
    let clients = crate::world::client_packages();
    let clients_valid = clients
        .validate_reserved(
            specs.iter().map(|spec| spec.name),
            std::iter::empty::<&str>(),
        )
        .is_ok();
    if clients_valid {
        for package in clients.packages() {
            root = root.subcommand(package.cli().command().display_order(ORDER_DAILY));
        }
    }
    root
}

/// The heading the global flags file under. Without it clap interleaves
/// them with each command's own flags in declaration order (`--home` between
/// `-p` and `-a` on `lait issues new`), so flags that apply *everywhere* read as
/// command-specific noise. One heading separates the two kinds.
const GLOBAL_HEADING: &str = "Global Options";

/// Help buckets (see `Spec.order`). Within a bucket, declaration order holds.
const ORDER_NAV: usize = 5; // context/orbits/worlds: orient before acting
const ORDER_DAILY: usize = 10; // the Issues package and its daily loop
const ORDER_SHARE: usize = 20; // init/join/invite/members/doctor/status
const ORDER_DEFAULT: usize = 30; // registries, settings
const ORDER_NODE: usize = 40; // daemon/remote/mcp/plumbing

fn build_sub(s: &Spec) -> Command {
    let mut c = Command::new(s.name).about(s.about).display_order(s.order);
    for a in s.aliases {
        c = c.alias(*a);
    }
    for a in &s.args {
        c = c.arg(a.to_arg());
    }
    for sub in &s.subs {
        c = c.subcommand(build_sub(sub));
    }
    if s.sub_required {
        c = c.subcommand_required(true).arg_required_else_help(true);
    }
    if let Some(f) = s.customize {
        c = f(c);
    }
    c
}

/// Parse an argv and, when it resolves to a Space/daemon control command, build
/// that `Request`. Product commands deliberately remain opaque World calls and
/// are tested through [`parse_to_dispatch`]. Returns a clap usage error for bad
/// input, or an error naming commands handled outside root control.
pub fn parse_to_request(argv: &[&str]) -> Result<Request> {
    match parse_to_dispatch(argv)? {
        ParsedCommand::Action(action) => match action.payload() {
            crate::client_action::ClientPayload::Control(request) => Ok(request.clone()),
            crate::client_action::ClientPayload::World(call) => Err(anyhow!(
                "World operation `{}` is owned by its client package, not root control",
                call.operation()
            )),
        },
        ParsedCommand::Special { name, .. } => {
            Err(anyhow!("`{name}` is a special-dispatch command"))
        }
        ParsedCommand::ProductLocal { operation, input } => {
            let name = input
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&operation);
            Err(anyhow!("`{name}` is a special-dispatch command"))
        }
    }
}

/// A parsed command line, surfacing `Special` leaves instead of erroring on
/// them — the seam interactive clients dispatch through. The caller decides per
/// `Special` whether
/// it has a native equivalent (start/done/stop, config, orbits, …) or rejects
/// with "CLI-only".
pub enum ParsedCommand {
    Action(ClientAction),
    ProductLocal {
        operation: String,
        input: serde_json::Value,
    },
    Special {
        which: Special,
        /// The leaf's name (for messages) — e.g. "start", "set".
        name: &'static str,
        matches: ArgMatches,
    },
}

/// Like [`parse_to_request`], but classifies rather than rejecting `Special`s.
pub fn parse_to_dispatch(argv: &[&str]) -> Result<ParsedCommand> {
    let specs = specs();
    let cli = build_cli(&specs);
    let m = cli.try_get_matches_from(argv).map_err(|e| anyhow!("{e}"))?;
    if let Some(invocation) = parse_world_invocation(&m)? {
        return Ok(match invocation.into_kind() {
            world_interface::ClientInvocationKind::World(call) => {
                ParsedCommand::Action(ClientAction::world(call))
            }
            world_interface::ClientInvocationKind::Local(local) => ParsedCommand::ProductLocal {
                operation: local.operation,
                input: local.input,
            },
        });
    }
    let (leaf, lm) = resolve(&specs, &m).ok_or_else(|| anyhow!("no subcommand"))?;
    match &leaf.dispatch {
        Dispatch::Action(f) => Ok(ParsedCommand::Action(ClientAction::from_legacy(f(lm)?))),
        Dispatch::Special(s) => Ok(ParsedCommand::Special {
            which: *s,
            name: leaf.name,
            matches: lm.clone(),
        }),
    }
}

/// Parse a mounted World package without exposing that package's grammar to the
/// shell registry.
pub fn parse_world_invocation(
    matches: &ArgMatches,
) -> Result<Option<world_interface::ClientInvocation>> {
    let Some((mount, package_matches)) = matches.subcommand() else {
        return Ok(None);
    };
    let registry = crate::world::client_packages();
    let Some(package) = registry.package_for_mount(mount) else {
        return Ok(None);
    };
    package
        .cli()
        .parse(package_matches)
        .map(Some)
        .map_err(anyhow::Error::new)
}

/// The palette's canonical completion source: every visible invocable command
/// as `(full name, about)`, recursively including product-owned groups.
pub fn command_index() -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn visit(prefix: &str, specs: &[Spec], out: &mut Vec<(String, String)>) {
        for spec in specs {
            let name = if prefix.is_empty() {
                spec.name.to_string()
            } else {
                format!("{prefix} {}", spec.name)
            };
            out.push((name.clone(), spec.about.to_string()));
            visit(&name, &spec.subs, out);
        }
    }
    visit("", &specs(), &mut out);
    fn visit_command(prefix: &str, command: &Command, out: &mut Vec<(String, String)>) {
        let name = if prefix.is_empty() {
            command.get_name().to_string()
        } else {
            format!("{prefix} {}", command.get_name())
        };
        out.push((
            name.clone(),
            command
                .get_about()
                .map(ToString::to_string)
                .unwrap_or_default(),
        ));
        for child in command.get_subcommands() {
            visit_command(&name, child, out);
        }
    }
    for package in crate::world::client_packages().packages() {
        visit_command("", &package.cli().command(), &mut out);
    }
    out
}

/// Resolve the invoked matches down to the leaf `Spec` + its `ArgMatches`.
/// Product namespaces may contain their own command groups, so descent is
/// recursive (`issues projects add`). A bare group resolves to the group spec.
pub fn resolve<'a>(specs: &'a [Spec], m: &'a ArgMatches) -> Option<(&'a Spec, &'a ArgMatches)> {
    let (name, sub_m) = m.subcommand()?;
    let spec = specs
        .iter()
        .find(|s| s.name == name || s.aliases.contains(&name))?;
    if sub_m.subcommand().is_some() {
        return resolve(&spec.subs, sub_m);
    }
    Some((spec, sub_m))
}

// ---- ArgMatches readers (all values are String; numerics parsed here) --------

fn opt_str(m: &ArgMatches, id: &str) -> Option<String> {
    m.get_one::<String>(id).cloned()
}
fn req_str(m: &ArgMatches, id: &str) -> String {
    // Required/defaulted at the clap layer, so this is always present.
    m.get_one::<String>(id).cloned().unwrap_or_default()
}
fn flag(m: &ArgMatches, id: &str) -> bool {
    m.get_flag(id)
}
fn multi(m: &ArgMatches, id: &str) -> Vec<String> {
    m.get_many::<String>(id)
        .map(|v| v.cloned().collect())
        .unwrap_or_default()
}
fn u64_arg(m: &ArgMatches, id: &str) -> Result<u64> {
    req_str(m, id)
        .parse::<u64>()
        .map_err(|_| anyhow!("--{id} must be a non-negative integer"))
}

// ---- The registry ------------------------------------------------------------

/// The full CLI surface as data. Built once per invocation in `app::run`.
pub fn specs() -> Vec<Spec> {
    use ArgSpec as A;
    let mut v = vec![
        // ---- space founding ----
        Spec::special(
            "init",
            "Found a new space here (mints the genesis; seeds a first project).",
            vec![
                A::val("name", "Space display name (default: this directory's name)."),
                A::val("nick", "Display nickname (sugar for `lait config set user.nick`)."),
            ],
            Special::Init,
        ),
        Spec {
            subs: vec![
                Spec::req(
                    "add",
                    "Add a member (admin-only). Seals the space key to them.",
                    vec![
                        A::pos(
                            "who",
                            "@me, a local name, a key id-prefix, or a 64-hex key.",
                        ),
                        A::flag("admin", "Grant admin."),
                        A::val("as_name", "Attach a local name as you add them.")
                            .long("as")
                            .value_name("NAME"),
                    ],
                    |m| {
                        Ok(Request::MemberAdd {
                            who: req_str(m, "who"),
                            admin: flag(m, "admin"),
                            as_name: opt_str(m, "as_name"),
                        })
                    },
                ),
                Spec::req(
                    "remove",
                    "Remove a member (admin-only) and rotate the space key.",
                    vec![A::pos("who", "A who-ref.")],
                    |m| {
                        Ok(Request::MemberRemove {
                            who: req_str(m, "who"),
                        })
                    },
                ),
                Spec::req(
                    "promote",
                    "Grant an existing member admin standing (admin-only).",
                    vec![A::pos(
                        "who",
                        "An actor id (full or unique act_ prefix) or a device id.",
                    )],
                    |m| {
                        Ok(Request::MemberSetRole {
                            who: req_str(m, "who"),
                            admin: true,
                        })
                    },
                ),
                Spec::req(
                    "demote",
                    "Reduce an admin to a plain member (admin-only; the last \
                     admin cannot be demoted).",
                    vec![A::pos(
                        "who",
                        "An actor id (full or unique act_ prefix) or a device id.",
                    )],
                    |m| {
                        Ok(Request::MemberSetRole {
                            who: req_str(m, "who"),
                            admin: false,
                        })
                    },
                ),
                Spec::req(
                    "name",
                    "Set (or clear) a local name for a member/key.",
                    vec![
                        A::pos("who", "A key id-prefix, a full key, or an existing name."),
                        A::pos_opt("name", "The name to assign (omit or \"\" to clear).")
                            .default(""),
                    ],
                    |m| {
                        Ok(Request::MemberAlias {
                            who: req_str(m, "who"),
                            name: req_str(m, "name"),
                        })
                    },
                )
                .alias(&["alias"]),
                Spec::req(
                    "agent",
                    "Sponsor an agent (any member). It can read/write but not manage \
                     membership or delete; its standing dies with you. Pass `--new <name>` \
                     to provision a co-located agent in one step (mint + self-incept + \
                     sponsor); then act as it with `lait --as <name> …`. Or pass an existing \
                     agent's 64-hex key to sponsor a key already known here.",
                    vec![
                        A::pos_opt("key", "An existing agent's 64-hex ed25519 public key."),
                        A::val("new", "Provision a new co-located agent with this local name."),
                    ],
                    |m| match (opt_str(m, "new"), opt_str(m, "key")) {
                        (Some(name), _) => Ok(Request::AgentProvision { name }),
                        (None, Some(key)) => Ok(Request::AgentAdd { key }),
                        (None, None) => Err(anyhow::anyhow!(
                            "pass an agent's key to sponsor, or `--new <name>` to provision one"
                        )),
                    },
                ),
                Spec::req(
                    "log",
                    "The membership audit log: the signed ACL DAG in causal order, \
                     with each op's authorization verdict.",
                    vec![],
                    |_| Ok(Request::MemberLog),
                )
                .alias(&["history"]),
                Spec::req(
                    "rotate-key",
                    "Rotate the space key (admin-only).",
                    vec![],
                    |_| Ok(Request::KeyRotate),
                ),
                Spec::req("ls", "List members.", vec![], |_| Ok(Request::Members)),
            ],
            ..Spec::req(
                "members",
                "Manage space membership through the signed ACL. `members` lists.",
                vec![],
                |_| Ok(Request::Members),
            )
        },
        Spec {
            subs: vec![
                Spec::req(
                    "invite",
                    "Print a token to enroll another device into your actor.",
                    vec![],
                    |_| Ok(Request::DeviceInvite),
                ),
                Spec::special(
                    "accept",
                    "On a new machine: consume a `device invite` token and print a \
                     consent blob to hand back for `device add`.",
                    vec![A::pos("token", "The token from `lait device invite`.")],
                    Special::DeviceAccept,
                ),
                Spec::req(
                    "add",
                    "Add a device to your actor from its consent blob, sealing it \
                     the space key.",
                    vec![A::pos("consent", "The blob from `device accept`.")],
                    |m| {
                        Ok(Request::DeviceAdd {
                            consent: req_str(m, "consent"),
                        })
                    },
                ),
                Spec::req(
                    "revoke",
                    "Revoke a device from your actor and rotate the key to fence it.",
                    vec![A::pos("device", "The device's 64-hex key.")],
                    |m| {
                        Ok(Request::DeviceRevoke {
                            device: req_str(m, "device"),
                        })
                    },
                ),
                Spec::req("ls", "List your actor's devices.", vec![], |_| {
                    Ok(Request::DeviceList)
                }),
            ],
            ..Spec::req(
                "device",
                "Manage the devices of your actor (multi-device identity).",
                vec![],
                |_| Ok(Request::DeviceList),
            )
        },
        Spec::req(
            "recover",
            "Recover your actor with the offline recovery key: reset the device \
             set to this device (content access re-seals once a peer syncs).",
            vec![],
            |_| Ok(Request::Recover),
        ),
        Spec::req(
            "recover-space",
            "Break-glass: re-root the WHOLE space to this device using the \
             offline space recovery keys (threshold K-of-N), when the admins \
             are lost or compromised. Sync from a surviving peer first. Under a \
             group key, repeat on each holder until the threshold co-signs.",
            vec![],
            |_| Ok(Request::SpaceRecover),
        )
        .alias(&["recover-workspace"]),
        Spec::req(
            "recover-approve",
            "Co-sign a pending break-glass recovery as a holder of the group \
             recovery key. You must name who you expect it to re-root to (`--to`); \
             a request that re-roots elsewhere is refused before your share is used.",
            vec![
                A::pos(
                    "session",
                    "The recovery session id (from the initiator's `recover-space`).",
                ),
                A::multi(
                    "to",
                    "The actor id you expect the space to re-root to (repeatable).",
                )
                .required(),
            ],
            |m| {
                Ok(Request::SpaceRecoverApprove {
                    session: req_str(m, "session"),
                    expect: multi(m, "to"),
                })
            },
        ),
        Spec::req(
            "elevate-approve",
            "Co-sign a proposed change to the recovery arrangement, as a holder \
             of the current group key. You must name the proposal you expect \
             (`--proposal`); a request authorizing a different one is refused \
             before your share is used.",
            vec![
                A::pos(
                    "session",
                    "The request id (from the proposer's `elevate-recovery`).",
                ),
                A::val(
                    "proposal",
                    "The proposal id you expect this to authorize.",
                )
                .required(),
            ],
            |m| {
                Ok(Request::SpaceElevateApprove {
                    session: req_str(m, "session"),
                    proposal: req_str(m, "proposal"),
                })
            },
        ),
        Spec::req(
            "custody-export",
            "Export your share of the group recovery key as a portable, \
             passphrase-protected package, and verify it by reopening it. An \
             all-holders arrangement will NOT install until every custodian has \
             done this — a share that only your Windows account can open is one \
             profile loss from gone. Store the file where the passphrase cannot \
             also be found.",
            vec![
                A::pos("path", "Where to write the package."),
                A::val("passphrase", "Passphrase protecting the package (min 12 chars).")
                    .required(),
            ],
            |m| {
                Ok(Request::SpaceCustodyExport {
                    path: req_str(m, "path"),
                    passphrase: req_str(m, "passphrase"),
                })
            },
        ),
        Spec::req(
            "custody-import",
            "Restore your share of the group recovery key from a package written \
             by `custody-export` — after losing the account or machine that held \
             it. Refuses to overwrite a share this device can already read unless \
             you pass `--force`.",
            vec![
                A::pos("path", "The package to restore from."),
                A::val("passphrase", "The passphrase the package was written with.").required(),
                A::flag("force", "Replace a share this device can already read."),
            ],
            |m| {
                Ok(Request::SpaceCustodyImport {
                    path: req_str(m, "path"),
                    passphrase: req_str(m, "passphrase"),
                    force: flag(m, "force"),
                })
            },
        ),
        Spec::req(
            "elevate-recovery",
            "Elevate the space recovery authority from your solo bootstrap key \
             to a K-of-N group key (dealer-free FROST DKG), sharing the recovery \
             burden with co-founders. Run where space-recovery.key lives; the \
             co-founders must already be admitted members.",
            vec![
                A::pos_multi(
                    "cofounders",
                    "Co-founder device keys to share the recovery authority with.",
                ),
                A::val(
                    "threshold",
                    "Signatures required to recover (K). Defaults to all holders (N-of-N).",
                )
                .default("0"),
            ],
            |m| {
                Ok(Request::SpaceElevate {
                    cofounders: multi(m, "cofounders"),
                    k: u64_arg(m, "threshold")? as u16,
                })
            },
        ),
        Spec::req(
            "reshare-recovery",
            "Reshare the group recovery key onto a new K-of-N arrangement \
             WITHOUT changing the key — replace or add holders. The current \
             holders authorize it (`elevate-approve`) and then threshold-sign \
             the installation. Note resharing is not a revocation: a removed \
             holder's old share still exists; to revoke, rotate the key with \
             `elevate-recovery` instead.",
            vec![
                A::pos_multi(
                    "participants",
                    "The COMPLETE new holder set (device keys), replacing the current one.",
                ),
                A::val(
                    "threshold",
                    "Signatures required to recover (K). Defaults to all holders (N-of-N).",
                )
                .default("0"),
            ],
            |m| {
                Ok(Request::SpaceReshare {
                    participants: multi(m, "participants"),
                    k: u64_arg(m, "threshold")? as u16,
                })
            },
        ),
        Spec::special(
            "serve",
            // `--json` is a global flag, so it needs no entry here — but it needs
            // *saying*, because the token is the reason anyone scripts this and a
            // long-running command that prints a machine line first is unusual
            // enough to be worth one clause.
            "Open your spaces in a browser (local, loopback-only). --json prints {url, token, port}, then serves.",
            vec![
                A::val("port", "Port to bind on 127.0.0.1 (default 7717)."),
                A::flag("open", "Open the URL in your default browser."),
            ],
            Special::Serve,
        ),
        Spec::req(
            "doctor",
            "Guided-join verifier: diagnose why you can't get to work yet.",
            vec![],
            |_| {
                Ok(Request::Diagnose {
                    expected_space: None,
                })
            },
        )
        .alias(&["verify"]),
        Spec {
            subs: vec![
                Spec::special(
                    "ls",
                    "List known local Orbits with status (default).",
                    vec![],
                    Special::Orbits,
                ),
                Spec::special(
                    "forget",
                    "Deregister an Orbit (registry only — never touches its store).",
                    vec![A::pos(
                        "sel",
                        "A store path, orb_/ws_ id, or unique id prefix.",
                    )],
                    Special::OrbitsForget,
                ),
                Spec::special(
                    "prune",
                    "Drop Orbit entries whose store no longer exists on disk.",
                    vec![],
                    Special::OrbitsPrune,
                ),
            ],
            ..Spec::special(
                "orbits",
                "Every local Orbit: id, Space, origin, status, projects, and path.",
                vec![],
                Special::Orbits,
            )
        },
        Spec::special(
            "context",
            "Show the identity, Orbit, Space, and installed Worlds selected here.",
            vec![],
            Special::Context,
        ),
        Spec::special(
            "worlds",
            "List the semantic World packages installed in this Lait application.",
            vec![],
            Special::Worlds,
        ),
        Spec::special(
            "rebuild",
            "Build, verify, and atomically activate the current representation as a new Orbit generation.",
            vec![],
            Special::Rebuild,
        ),
        Spec {
            subs: vec![
                Spec::special(
                    "get",
                    "Print a key's effective value (store layer wins over global).",
                    vec![A::pos("key", "Config key (see `lait config ls`).")],
                    Special::ConfigGet,
                ),
                Spec::special(
                    "set",
                    "Set a key. Store layer by default; --global for the machine layer.",
                    vec![
                        A::pos("key", "Config key (e.g. user.nick, project.default)."),
                        A::pos("value", "The value."),
                        A::flag("global", "Write the global layer instead of this store's."),
                    ],
                    Special::ConfigSet,
                ),
                Spec::special(
                    "unset",
                    "Remove a key from a layer.",
                    vec![
                        A::pos("key", "Config key."),
                        A::flag("global", "Remove from the global layer instead."),
                    ],
                    Special::ConfigUnset,
                ),
                Spec::special(
                    "ls",
                    "List effective settings, annotated with their origin layer (default).",
                    vec![],
                    Special::ConfigList,
                ),
            ],
            ..Spec::special(
                "config",
                "Get/set layered local settings (global + per-store; store wins).",
                vec![],
                Special::ConfigList,
            )
        },
        Spec::special("id", "Print our endpoint id.", vec![], Special::Id),
        Spec::special(
            "daemon",
            "Run the identity-scoped Lait daemon in the foreground.",
            vec![A::flag(
                "seed",
                "Deprecated compatibility flag; the Lait daemon is always-on.",
            )],
            Special::Daemon,
        )
        .service(),
        Spec::special(
            "mcp",
            "Run the MCP server over stdio (for agents).",
            vec![],
            Special::Mcp,
        )
        .service(),
        Spec::special(
            "install-mcp",
            "Register lait's MCP server with an agent's config.",
            vec![A::flag("print", "Print the config instead of writing it.")],
            Special::InstallMcp,
        )
        .customize(|c| {
            // Required, not defaulted: the written entry lands in a different
            // file per client, and for Claude Code it can shadow the bundled
            // plugin's own server. A default silently picked that consequence.
            c.arg(
                Arg::new("client")
                    .long("client")
                    .value_parser(clap::value_parser!(Client))
                    .required(true)
                    .help("Target agent client (required — the config differs per client)."),
            )
            .arg(
                Arg::new("scope")
                    .long("scope")
                    .value_parser(clap::value_parser!(Scope))
                    .help("Config scope (user/project)."),
            )
            .arg(
                Arg::new("name")
                    .long("name")
                    .default_value("lait")
                    .help("Server name in the client config."),
            )
            .arg(
                Arg::new("agent")
                    .long("agent")
                    .help(
                        "Sponsored agent identity to sign the agent's work as \
                         (provision with `lait members agent --new <name>`).",
                    ),
            )
        }),
        Spec::req("status", "Show node and space status.", vec![], |_| {
            Ok(Request::Status)
        }),
        Spec {
            subs: vec![Spec::req(
                "revoke",
                "Revoke an invite so it can no longer admit anyone (admin only).",
                vec![A::pos(
                    "invite",
                    "The invite ticket, or its 32-hex nonce.",
                )],
                |m| {
                    Ok(Request::InviteRevoke {
                        invite: req_str(m, "invite"),
                    })
                },
            )],
            ..Spec::special(
                "invite",
                "Print a base32 ticket (+ QR) others use to join your space.",
                vec![
                    A::val(
                        "email",
                        "Open your mail client with a prefilled invite to this address.",
                    ),
                    A::val(
                        "role",
                        "The role the invite admits as: viewer | contributor | administrator.",
                    )
                    .long("role")
                    .value_name("ROLE"),
                    A::flag(
                        "reusable",
                        "Let one ticket admit your whole team until it expires.",
                    ),
                    A::val(
                        "ttl_hours",
                        "Hours until the pass expires (default 168 = 7 days).",
                    )
                    .long("ttl-hours")
                    .value_name("HOURS"),
                ],
                Special::Invite,
            )
        },
        Spec::special(
            "join",
            "Join a space from an invite link (creates the store here, or at --dir).",
            vec![
                A::pos("ticket", "The invite link / ticket from `lait invite`."),
                A::val("nick", "Set your display name as you join."),
                A::val("dir", "Create the joined space's store under this directory."),
            ],
            Special::Join,
        ),
        Spec::req(
            "connect",
            "Nudge the daemon to contact a peer now (a station id, or an invite link \
             whose host to reach). Joining a new space is `lait join`.",
            vec![A::pos(
                "target",
                "A station/device id, or an invite link for this space.",
            )],
            |m| {
                Ok(Request::Connect {
                    ticket: req_str(m, "target"),
                })
            },
        ),
        Spec {
            subs: vec![
                Spec::req(
                    "add",
                    "Pin a remote for this space (an invite link for it, or an endpoint id).",
                    vec![A::pos("target", "An invite link or an endpoint id.")],
                    |m| {
                        Ok(Request::SeedAdd {
                            arg: req_str(m, "target"),
                        })
                    },
                ),
                Spec::req(
                    "ls",
                    "List pinned remotes and reachability.",
                    vec![],
                    |_| Ok(Request::SeedList),
                ),
                Spec::req(
                    "rm",
                    "Unpin a remote by endpoint id (or prefix) or name.",
                    vec![A::pos("who", "Endpoint id (or prefix) or name to unpin.")],
                    |m| {
                        Ok(Request::SeedRemove {
                            who: req_str(m, "who"),
                        })
                    },
                ),
            ],
            sub_required: true,
            ..Spec::req(
                "remote",
                "Manage pinned remotes (always-on peers your node always dials).",
                vec![],
                |_| Ok(Request::SeedList),
            )
            .alias(&["seed"])
        },
        Spec::req(
            "log",
            "Print presence/system events (optionally only after --since).",
            vec![A::val("since", "Only events after this seq.").default("0")],
            |m| {
                Ok(Request::Log {
                    since: u64_arg(m, "since")?,
                })
            },
        ),
        Spec::special(
            "watch",
            "Follow presence events like a notification stream.",
            vec![
                A::val("since", "Start after this seq."),
                A::val("exec", "Run a hook command per event."),
                A::flag("notify", "Emit a desktop notification per event."),
            ],
            Special::Watch,
        ),
        Spec::req("who", "List peers and their online status.", vec![], |_| {
            Ok(Request::Who)
        }),
        Spec::req(
            "whoami",
            "Show who you are in this space: actor, did:key, role, capabilities, \
             sponsor, and whether your view is complete — in one shot.",
            vec![],
            |_| Ok(Request::Whoami),
        ),
        Spec::req(
            "sync",
            "Converge now and report whether your view is complete — names any \
             missing epoch key loudly instead of silently showing fewer issues.",
            vec![],
            |_| Ok(Request::Sync),
        ),
        Spec::special(
            "profiles",
            "List your profiles — each a separate private identity.",
            vec![],
            Special::Profiles,
        )
        .alias(&["agents"]),
        Spec::special(
            "resume",
            "Switch to (or create) a named profile for this session.",
            vec![A::pos("name", "Profile name.")],
            Special::Resume,
        ),
        Spec::special(
            "update",
            "Update lait in place from the latest GitHub release.",
            vec![],
            Special::Update,
        ),
        // `stop` the word belongs to the work loop (put an issue down); the
        // daemon's off-switch is `shutdown`.
        Spec::req("shutdown", "Stop the running daemon.", vec![], |_| {
            Ok(Request::Stop)
        }),
        Spec::special(
            "completions",
            "Print shell completions to stdout for the given shell.",
            vec![],
            Special::Completions,
        )
        .customize(|c| {
            c.arg(
                Arg::new("shell")
                    .value_parser(clap::value_parser!(Shell))
                    .required(true)
                    .help("bash, zsh, fish, powershell, or elvish."),
            )
        }),
        Spec::special(
            "man",
            "Render the lait(1) man page (roff) to stdout.",
            vec![],
            Special::Man,
        ),
    ];
    // Help buckets in one greppable place: navigation leads, product namespaces
    // follow, then sharing/settings, with node plumbing at the bottom. Within a
    // bucket, declaration order holds.
    for s in &mut v {
        s.order = match s.name {
            "new" | "start" | "done" | "stop" | "inbox" | "show" | "board" | "ls" | "edit"
            | "move" | "assign" | "label" | "comment" | "react" | "delete" | "restore" | "link"
            | "unlink" | "parent" | "graph" | "history" | "follow" | "unfollow" | "attach"
            | "attachment" | "activity" => ORDER_DAILY,
            "context" | "orbits" | "worlds" | "serve" => ORDER_NAV,
            "init" | "join" | "invite" | "members" | "doctor" | "status" | "who" => ORDER_SHARE,
            "projects" | "labels" | "milestone" | "cycle" | "initiatives" | "teams" | "triage"
            | "role" | "access" | "workflow" | "config" | "profiles" | "resume" => ORDER_DEFAULT,
            _ => ORDER_NODE,
        };
    }

    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_tree_builds_and_validates() {
        // clap panics on a malformed tree (dup ids, bad positionals); this asserts
        // the whole registry assembles into a legal Command.
        build_cli(&specs()).debug_assert();
    }
}
