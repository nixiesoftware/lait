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

//! The launcher. lait is not a command surface — it is three processes.
//!
//! `lait daemon` is the identity-scoped host; `lait mcp` is the stdio head for
//! an agent; bare `lait` starts the daemon and serves the HTTP head a browser
//! uses. Everything a command used to do is a request one of those three
//! carries, so there is no grammar here to parse — only a mode to pick.
//!
//! Hand-rolled rather than clap: a handful of argv shapes, all of them already
//! deployed (`daemon_spawn` self-execs `<exe> daemon`, agent configs run
//! `lait mcp`, `viewer/scripts/dev.mjs` runs `lait [--orbit X] --port P
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
    /// The identity-scoped host, optionally pinned to a self-contained home.
    Daemon { home: Option<String> },
    /// The stdio MCP head, on whatever Orbit the environment selects.
    Mcp,
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
    },
}

const USAGE: &str = "lait is not a command surface — it is three processes:\n\
     \x20 lait daemon [--home <dir>]      the identity-scoped host\n\
     \x20 lait mcp                        the stdio head for an agent\n\
     \x20 lait [--json] [--port <n>] [--orbit <sel>] [--open]\n\
     \x20                                 the local app, and the daemon under it\n\
     \x20 lait --version                  which build this is\n\
     everything else is a request one of those three carries.";

#[tokio::main]
async fn main() -> ExitCode {
    // Before anything can spawn: our stdio stays ours.
    lait::process::disinherit_stdio();
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
                let mut home = None;
                while let Some(flag) = args.next() {
                    match flag {
                        // A deprecated no-op: the daemon is always-on, and the
                        // flag is still in `Dockerfile`'s CMD.
                        "--seed" => {}
                        "--home" => home = Some(next(&mut args, "--home")?),
                        other => return Err(unknown(other)),
                    }
                }
                Mode::Daemon { home }
            }
            Some("mcp") => {
                args.next();
                if let Some(other) = args.next() {
                    return Err(unknown(other));
                }
                Mode::Mcp
            }
            Some("--version" | "-V") => Mode::Version,
            _ => {
                let (mut json, mut open) = (false, false);
                let (mut port, mut orbit) = (None, None);
                while let Some(flag) = args.next() {
                    match flag {
                        "--json" => json = true,
                        "--open" => open = true,
                        "--port" => port = Some(next(&mut args, "--port")?),
                        "--orbit" => orbit = Some(next(&mut args, "--orbit")?),
                        other => return Err(unknown(other)),
                    }
                }
                Mode::Serve {
                    json,
                    port,
                    orbit,
                    open,
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
            Mode::Daemon { home } => {
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
                let selection = Selection {
                    identity: home.map(std::path::PathBuf::from),
                    store: None,
                };
                lait::daemon::run_lait_daemon(lait::world::packages(), selection).await
            }
            Mode::Mcp => {
                let selection = Selection::default();
                // The one mode that needs a store before it can speak: its tools
                // address an Orbit. Nothing is created implicitly, so a bare
                // directory is a refusal that names what exists instead.
                let home = selection.resolve_existing_store().map_err(|error| {
                    match error.downcast_ref::<lait::config::NoStoreHere>() {
                        Some(_) => anyhow::anyhow!(lait::host_client::no_store_here()),
                        None => error,
                    }
                })?;
                lait::mcp::run_mcp(&home, selection).await
            }
            Mode::Serve {
                json,
                port,
                orbit,
                open,
            } => {
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
                    identity: None,
                    store,
                };
                lait::serve::run(port, open, json, selection).await
            }
        }
    }
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
