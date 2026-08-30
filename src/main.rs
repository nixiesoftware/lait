#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! The host launcher. Product Worlds run as separately supervised processes.
//!
//! `lait daemon` is the identity-scoped host; `lait mcp` is the stdio head for
//! an agent; bare `lait` starts the daemon and serves the HTTP head a browser
//! uses. Everything a command used to do is a request one of those three
//! carries, so there is no grammar here to parse — only a mode to pick.
//! `lait install` is the one thing that runs before any of them exist: it
//! writes the always-on service a headless box runs the daemon under.
//!
//! Hand-rolled rather than clap: a handful of argv shapes, all of them already
//! deployed (`daemon_spawn` self-execs `<exe> daemon`, agent configs run
//! `lait mcp`, and `ci/smoke-p0.sh` runs `lait [--orbit X] --port P
//! --json`, and installers verify with `lait --version`), and a parser
//! generator earns nothing against a fixed list of flags.
//!
//! SIGPIPE is deliberately left ignored (Rust's default) in all three modes.
//! Resetting it to `SIG_DFL` was for short-lived, stdout-printing commands, so
//! a closed pipe exited cleanly instead of panicking. Those commands are gone,
//! and every mode below is a long-running service doing socket I/O — for a
//! service the reset is fatal, because a dropped relay or HTTP write would then
//! raise SIGPIPE and *kill the process* instead of returning `EPIPE`.

use std::process::ExitCode;

use lait::config::Selection;

/// What this process is going to be.
enum Mode {
    /// The identity-scoped host, optionally pinned to a self-contained home
    /// or to one of this machine's founded profiles.
    Daemon {
        home: Option<String>,
        /// Which client stack this daemon serves.
        ///
        /// Passed explicitly by whoever spawns it rather than inherited from
        /// the environment, for the reason `--home` is: a daemon and the
        /// client that started it must not be able to disagree about which
        /// identity they are serving, and an ambient variable is exactly how
        /// they would.
        profile: Option<String>,
    },
    /// The stdio MCP head, on whatever Orbit the environment selects.
    Mcp,
    /// Make a new client stack on this machine, and say what it lacks.
    ///
    /// The one act that creates a profile. Every other path *reads* one, and
    /// reading never creates: a name nobody founded refuses and names itself,
    /// because the failure this design replaces resolved an unrecognised name
    /// into a fresh directory holding a fresh keypair and then reported the
    /// empty machine as a healthy one.
    FoundProfile { name: String },
    /// Print this build and exit.
    ///
    /// Not a command coming back. It is the one question that has to be
    /// answerable *before* any process is running — which build did I install,
    /// and is it the tagged release or a dev prerelease — and every documented
    /// consumer asks it this way (`docs/INSTALL.md`'s verification step,
    /// `dev-release.yml`). A running node answers the same question over the
    /// host plane (`Request::HostContext`); this answers it with nothing
    /// running at all.
    Version,
    /// The HTTP head, and the daemon under it.
    Serve {
        json: bool,
        port: Option<String>,
        orbit: Option<String>,
        open: bool,
        /// The self-contained identity directory this head serves, when it is
        /// not the ordinary per-user one. A head is bound to one identity, and
        /// a client that supervises several — Astrolabe's development fleet —
        /// has no other way to say which. Spelled as it is for `daemon`,
        /// because it selects the same thing: the daemon this head starts or
        /// attaches to is the one at that home.
        home: Option<String>,
        /// Which client stack this head and the daemon under it belong to.
        profile: Option<String>,
        /// The one World this head serves.
        ///
        /// `None` falls back to `$LAIT_WORLD`, and then to the sole World this
        /// identity has selected — the same ladder `lait mcp` climbs, resolved
        /// by the same `registry.pin`, so several selected Worlds with no pin
        /// refuses here exactly as it refuses there rather than picking one.
        ///
        /// A head that knows which World it is, is a head a supervisor can
        /// stop *definitively*. The alternative — one head serving everything —
        /// makes "is this World running" unanswerable, and every control built
        /// on the answer speculative.
        world: Option<String>,
    },
    /// Install the channel's proven binary as the always-on service, and
    /// print what the daemon under it has to say.
    ///
    /// Prints for a person at a terminal — a pairing code, or where to look —
    /// and never a readiness line for a parser, which is why `--json` is
    /// refused here rather than inherited from the app arm.
    Install {
        channel: Option<lait::update::feed::Channel>,
        user: bool,
        displays: bool,
        root: Option<String>,
    },
}

const USAGE: &str = "lait is not a command surface — it has three host modes and an installer:\n\
     \x20 lait daemon [--home <dir>]      the identity-scoped host\n\
     \x20 lait mcp                        the stdio head for an agent\n\
     \x20 lait [--json] [--port <n>] [--orbit <sel>] [--open] [--home <dir>]\n\
     \x20      [--world <mount>]           one head, one World\n\
     \x20                                 the local app, and the daemon under it\n\
     \x20 lait --version                  which build this is\n\
     \x20 lait install [--user] [--displays] [--channel <stable|test>] [--root <dir>]\n\
     \x20                                 the always-on service, from the release the channel proves\n\
     everything else is a request one of those three carries.";

#[tokio::main]
async fn main() -> ExitCode {
    // Before anything can spawn: our stdio stays ours, and our signals are ours
    // rather than whatever the launching chain happened to be carrying.
    lait::process::disinherit_stdio();
    lait::process::reset_signal_environment();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mode = match Mode::parse(&argv) {
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(1);
        }
    };
    let json = matches!(mode, Mode::Serve { json: true, .. });
    match mode.run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => lait::host_client::report_error(&error, json),
    }
}

impl Mode {
    fn parse(argv: &[String]) -> Result<Mode, String> {
        let mut args = argv.iter().map(String::as_str).peekable();
        let mode = match args.peek().copied() {
            Some("daemon") => {
                args.next();
                let (mut home, mut profile) = (None, None);
                while let Some(flag) = args.next() {
                    match flag {
                        "--home" => home = Some(next(&mut args, "--home")?),
                        "--profile" => profile = Some(next(&mut args, "--profile")?),
                        other => return Err(unknown(other)),
                    }
                }
                Mode::Daemon { home, profile }
            }
            Some("mcp") => {
                args.next();
                if let Some(other) = args.next() {
                    return Err(unknown(other));
                }
                Mode::Mcp
            }
            Some("--version" | "-V") => Mode::Version,
            Some("install") => {
                args.next();
                let (mut user, mut displays) = (false, false);
                let (mut channel, mut root) = (None, None);
                while let Some(flag) = args.next() {
                    match flag {
                        "--user" => user = true,
                        "--displays" => displays = true,
                        "--channel" => {
                            let name = next(&mut args, "--channel")?;
                            channel = Some(lait::update::feed::Channel::parse(&name).ok_or_else(
                                || {
                                    format!(
                                        "--channel must be stable or test, got {name:?}\n{USAGE}"
                                    )
                                },
                            )?);
                        }
                        "--root" => root = Some(next(&mut args, "--root")?),
                        other => return Err(unknown(other)),
                    }
                }
                Mode::Install {
                    channel,
                    user,
                    displays,
                    root,
                }
            }
            _ => {
                let (mut json, mut open) = (false, false);
                let (mut port, mut orbit, mut home) = (None, None, None);
                let (mut world, mut profile) = (None, None);
                let mut found_profile = None;
                while let Some(flag) = args.next() {
                    match flag {
                        "--json" => json = true,
                        "--open" => open = true,
                        "--port" => port = Some(next(&mut args, "--port")?),
                        "--orbit" => orbit = Some(next(&mut args, "--orbit")?),
                        "--home" => home = Some(next(&mut args, "--home")?),
                        "--profile" => profile = Some(next(&mut args, "--profile")?),
                        // Founding is an explicit act, and this is where a
                        // person performs it. Reading a profile never creates
                        // one — a name nobody founded refuses and says so —
                        // so there has to be exactly one way to say "yes,
                        // make this stack", and it says what the new stack
                        // will not have before anything else happens.
                        "--found-profile" => {
                            found_profile = Some(next(&mut args, "--found-profile")?)
                        }
                        // The same pin `lait mcp` takes, on the other head
                        // kind. One head, one World — so stopping a head is a
                        // statement about that World and not about whatever
                        // else happened to share the process.
                        "--world" => world = Some(next(&mut args, "--world")?),
                        other => return Err(unknown(other)),
                    }
                }
                match found_profile {
                    Some(name) => Mode::FoundProfile { name },
                    None => Mode::Serve {
                        json,
                        port,
                        orbit,
                        open,
                        home,
                        profile,
                        world,
                    },
                }
            }
        };
        Ok(mode)
    }

    async fn run(self) -> anyhow::Result<()> {
        match self {
            Mode::Version => {
                println!("lait {}", lait::VERSION);
                Ok(())
            }
            Mode::FoundProfile { name } => {
                let parsed: lait::config::ProfileName = name.parse()?;
                let founded = lait::config::profile::found(&parsed)?;
                println!(
                    "founded profile {parsed} — its own device, Spaces and Worlds\n  config {}\n  state  {}",
                    founded.profile.config_root()?.display(),
                    founded
                        .profile
                        .state_root()
                        .map(|root| root.display().to_string())
                        .unwrap_or_default(),
                );
                // Said before anything runs under it. A new stack is a new
                // device to every peer, and a person who is not told that
                // reads an empty Library as a broken one.
                println!("\nit starts with:");
                for lacks in founded.lacks {
                    println!("  - {}", lacks.says());
                }
                println!("\nrun it with:  lait --profile {parsed}");
                Ok(())
            }
            Mode::Daemon { home, profile } => {
                // To **stderr**, because that is the only stream a spawned
                // daemon still owns. `daemon_spawn` hands the log file to
                // stderr and nulls stdout, and `fmt()` writes to stdout by
                // default — so every line this subscriber produced went to the
                // null device, and `daemon.log` could only ever receive a panic
                // or a spawn refusal printed before this ran.
                //
                // It failed silently and it failed for everything: sixty-odd
                // `warn!`/`error!` sites across the tree, including the
                // implementation-drift check that names its own remedy and the
                // store watchdog. Reading an empty `daemon.log` looked like a
                // quiet node, which is the most misleading possible answer — the
                // log the error messages point operators at was structurally
                // incapable of holding anything.
                tracing_subscriber::fmt()
                    .with_writer(std::io::stderr)
                    .with_env_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "lait=info,warn".into()),
                    )
                    .init();
                // Fixed before anything resolves a root, so every later read
                // in this process answers for the same stack.
                establish_profile(profile.as_deref(), home.is_some())?;
                let selection = Selection {
                    identity: home.map(std::path::PathBuf::from),
                    store: None,
                    profile: None,
                };
                let identity = selection.identity_dir()?;
                let installation = lait::world::installed::load(
                    &lait::serve::head::installations_root(&identity),
                )?;
                // And in the daemon, which is what the host plane and every
                // display resolve through. A World that only some of this
                // device's registries know about is a World that works until
                // somebody reaches it by a route nobody tested.
                let (packages, clients, refused) = lait::world::installed::load_local(
                    &identity,
                    installation.packages,
                    installation.clients,
                );
                for reason in &refused {
                    tracing::warn!(%reason, "a local World was not loaded");
                }
                lait::daemon::run_lait_daemon(packages, clients, selection).await
            }
            Mode::Mcp => {
                // This mode takes no flags — an editor spawns it and owns its
                // environment — so the stack can only come from `$LAIT_PROFILE`.
                // Established here anyway, and with the same refusals every
                // other entry point gets: a profile named beside a
                // self-contained home is a contradiction wherever it appears,
                // and this was the one path where the two would both have been
                // honoured. The store still decides which stack an agent binds
                // (`bind_for_agent`); this only settles the fallback.
                establish_profile(None, std::env::var_os("LAIT_HOME").is_some())?;
                let selection = Selection::default();
                // The one mode that needs a store before it can speak: its tools
                // address an Orbit. `resolve_for_agent` adds the registry's
                // sole-Orbit fallback — an agent config cannot cd — and still
                // creates nothing implicitly, so a miss stays a refusal that
                // names what exists instead.
                let bound =
                    selection.bind_for_agent().map_err(|error| match error
                        .downcast_ref::<lait::config::NoStoreHere>()
                    {
                        Some(_) => anyhow::anyhow!(lait::host_client::no_store_here()),
                        None => error,
                    })?;
                // The stack that registered this store, not the one the editor
                // happened to launch this process in. `lait mcp` takes no
                // flags and its environment is the editor's, so the store on
                // disk is the only thing here that reliably says which
                // identity this session belongs to — and signing with the
                // wrong one would have every write refused as a permission
                // problem rather than named as the wrong device.
                if let Some(profile) = bound.profile.clone() {
                    tracing::debug!(profile = %profile.label(), "binding this agent to the stack that owns its store");
                }
                let home = bound.store.clone();
                lait::mcp::run_mcp(&home, bound.selection()).await
            }
            Mode::Serve {
                json,
                port,
                orbit,
                open,
                home,
                profile,
                world,
            } => {
                // The head had no subscriber at all, so every `warn!` and
                // `error!` reached by this process was a no-op — including the
                // one a linked World logs to say it is *not* serving the
                // installed release, which is the safety half of that seam.
                //
                // To **stderr**, for the reason the daemon arm spells out and
                // one more: `--json` writes a readiness line to stdout that
                // callers parse, and a log line landing in the middle of it
                // would break every one of them.
                //
                // `try_init` rather than `init`: this is the launcher, it may
                // run beside anything, and a second subscriber is not worth a
                // panic on the way up.
                let _ = tracing_subscriber::fmt()
                    .with_writer(std::io::stderr)
                    .with_env_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "lait=warn,warn".into()),
                    )
                    .try_init();
                establish_profile(profile.as_deref(), home.is_some())?;
                let port = match port {
                    Some(p) => p.parse::<u16>().map_err(|_| {
                        anyhow::anyhow!("--port must be a number 0-65535, got {p:?}")
                    })?,
                    None => lait::serve::DEFAULT_PORT,
                };
                let store = match orbit {
                    Some(selector) => Some(lait::orbits::select(&selector).map_err(|error| {
                        // A selector matching nothing resolves to nothing: exit
                        // `2`, the same answer a missing ref gets.
                        lait::host_client::Failure::not_found(format!("{error}"))
                    })?),
                    None => None,
                };
                let selection = Selection {
                    identity: home.map(std::path::PathBuf::from),
                    store,
                    profile: None,
                };
                // Resolved before the listener binds, so a build that cannot
                // say which World this head is refuses to be one — rather than
                // coming up, announcing an address, and answering for whatever
                // mount a request happens to name.
                let world =
                    world.or_else(|| std::env::var("LAIT_WORLD").ok().filter(|s| !s.is_empty()));
                lait::serve::run(port, open, json, selection, world).await
            }
            Mode::Install {
                channel,
                user,
                displays,
                root,
            } => {
                // The feed's refusals are `warn!`s on the way to an error
                // that names only the last link; to stderr so they are not
                // lost, and so stdout stays the person's two lines.
                let _ = tracing_subscriber::fmt()
                    .with_writer(std::io::stderr)
                    .with_env_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "lait=warn,warn".into()),
                    )
                    .try_init();
                use lait::update::service;
                // Blocking HTTP and archive work; the reactor is this process's
                // and nothing else is on it, but a worker thread costs nothing.
                let plan = tokio::task::spawn_blocking(move || {
                    service::plan(channel, user, displays, root.map(std::path::PathBuf::from))
                })
                .await
                .map_err(|error| anyhow::anyhow!("the install plan panicked: {error}"))??;
                service::apply(&plan)?;
                println!(
                    "Installed {} as {} under {}",
                    plan.version,
                    service::unit_label(plan.user),
                    plan.root.display()
                );
                let tail = service::tail(&plan.root, plan.user).await?;
                println!("{tail}");
                Ok(())
            }
        }
    }
}

/// Fix this process's client stack before anything reads a root.
///
/// One call, at the top of each mode that can be told which stack it serves.
/// A refusal here — a value that is not a name, a profile nobody founded, a
/// profile named alongside `--home` — ends the process saying so, which is the
/// whole point: the failure this replaces resolved an unrecognised name to a
/// freshly created directory and then reported the empty machine as healthy.
fn establish_profile(profile: Option<&str>, self_contained_home: bool) -> anyhow::Result<()> {
    let selected = lait::config::Profile::select(profile, self_contained_home)?;
    if let Some(name) = selected.name() {
        tracing::info!(profile = %name, "serving a profile of this machine");
    }
    lait::config::profile::establish(selected);
    Ok(())
}

/// The value that follows a flag, or a message naming the flag that wanted one.
fn next<'a>(args: &mut impl Iterator<Item = &'a str>, flag: &str) -> Result<String, String> {
    args.next()
        .map(ToString::to_string)
        .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
}

fn unknown(arg: &str) -> String {
    format!("unrecognized argument `{arg}`\n{USAGE}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Mode, String> {
        Mode::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    /// A head is bound to one identity, and a supervisor running several — the
    /// development fleet — has no other way to say which. Without this the
    /// spawn is rejected by the launcher and the head dies before it can
    /// announce an address, which reads as "the head failed to come up" rather
    /// than as "the flag does not exist".
    #[test]
    fn the_app_accepts_the_home_that_selects_which_identity_it_serves() {
        let Ok(Mode::Serve { home, json, .. }) = parse(&["--json", "--port", "0", "--home", "d"])
        else {
            panic!("the app mode refused --home");
        };
        assert_eq!(home.as_deref(), Some("d"));
        assert!(json);

        let Ok(Mode::Serve { home, .. }) = parse(&["--json"]) else {
            panic!("the ordinary app launch stopped parsing");
        };
        assert_eq!(
            home, None,
            "an unstated identity became a selection rather than the per-user one"
        );
    }

    /// The three modes stay three. A flag that wanders between them is how a
    /// spawn ends up silently rejected by the process it was aimed at.
    #[test]
    fn the_three_modes_keep_their_own_flags() {
        assert!(
            matches!(parse(&["daemon", "--home", "d"]), Ok(Mode::Daemon { .. })),
            "the daemon lost its --home"
        );
        assert!(
            parse(&["daemon", "--json"]).is_err(),
            "the daemon accepted a flag that belongs to the app"
        );
        assert!(
            parse(&["mcp", "--home", "d"]).is_err(),
            "the MCP head accepted a flag it has no use for"
        );
        assert!(parse(&["--nonsense"]).is_err());

        // The stack a process serves is named on its argv, never inherited.
        // The daemon must accept it because that is how a client tells the
        // daemon it spawns which identity to serve — an ambient variable
        // would let the two disagree.
        assert!(
            matches!(
                parse(&["daemon", "--profile", "dev"]),
                Ok(Mode::Daemon {
                    profile: Some(_),
                    ..
                })
            ),
            "the daemon cannot be told which stack it serves"
        );
        assert!(
            matches!(
                parse(&["--profile", "dev"]),
                Ok(Mode::Serve {
                    profile: Some(_),
                    ..
                })
            ),
            "the app cannot be told which stack it is"
        );
        // And `mcp` still takes none. Its stack comes from the store it
        // finds, because an editor owns its environment and the store on disk
        // is the only thing that reliably says which identity a repository
        // belongs to.
        assert!(
            parse(&["mcp", "--profile", "dev"]).is_err(),
            "the MCP head grew a flag; its stack must come from the store it binds"
        );
        assert!(
            matches!(
                parse(&["--found-profile", "dev"]),
                Ok(Mode::FoundProfile { .. })
            ),
            "there is no way to found a profile, so every read of one refuses forever"
        );
    }

    /// `--seed` was a no-op kept because a container CMD spelled it. The
    /// container is gone, and a flag that parses to nothing is a flag a unit
    /// file can carry for years without anybody noticing it does nothing.
    #[test]
    fn the_daemon_no_longer_accepts_the_seed_flag_that_did_nothing() {
        assert!(
            parse(&["daemon", "--seed"]).is_err(),
            "the daemon still swallows --seed"
        );
    }

    /// The install line prints for a person at a terminal — a pairing code,
    /// or where to look — and never a readiness line for a parser. Refused
    /// by the mode's own parser, so it cannot inherit `--json` from the app
    /// arm by accident.
    #[test]
    fn the_install_line_refuses_json() {
        assert!(
            parse(&["install", "--json"]).is_err(),
            "install accepted --json, which belongs to the app"
        );
    }

    /// The installer's flags are its own, and a channel is one of the two the
    /// feed serves — a typo here would follow nothing and install stable.
    #[test]
    fn the_install_line_takes_its_own_flags() {
        let Ok(Mode::Install {
            channel,
            user,
            displays,
            root,
        }) = parse(&["install", "--user", "--channel", "test"])
        else {
            panic!("install --user --channel test did not parse");
        };
        assert_eq!(channel, Some(lait::update::feed::Channel::Test));
        assert!(user);
        assert!(!displays, "the display coordinator is off unless asked");
        assert_eq!(root, None);

        let Ok(Mode::Install { root, displays, .. }) =
            parse(&["install", "--displays", "--root", "/srv/lait"])
        else {
            panic!("install --displays --root did not parse");
        };
        assert!(displays);
        assert_eq!(root.as_deref(), Some("/srv/lait"));

        assert!(
            parse(&["install", "--channel", "nightly"]).is_err(),
            "a channel the feed does not serve was accepted"
        );
        assert!(parse(&["install", "--home", "d"]).is_err());
    }
}
