//! Guards for the promises the launcher makes before it becomes a service.
//!
//! lait is not a command surface: three argv shapes, three long-running
//! processes. What survived the carve is everything that was never about a
//! grammar.
//!
//! 1. **Never hold your stdout hostage.** A mode that auto-spawns a daemon must
//!    still let its own stdout hit EOF when it exits: whoever captured it (a
//!    test harness, `npm run dev`'s reader, an MCP client) reads until EOF, so a
//!    daemon left holding the write end hangs the *caller*, not the daemon.
//!    Windows-only in practice — see `process::disinherit_stdio` — but the
//!    promise is platform-independent.
//!
//! 2. **Tell a foreign daemon from an absent one.** A daemon that is listening
//!    but speaks a different wire shape (an older lait still running after an
//!    upgrade) used to be reported as "no daemon" — which spawned a doomed
//!    second daemon over the held lock and waited out a 20s timeout before
//!    blaming the timeout. Detection is at the transport level, so this stays
//!    true across wire changes. And a daemon *ahead* of this build is named,
//!    never replaced.
//!
//! 3. **Report failures in one voice.** A launcher failure is one lowercase
//!    `error:` line with the documented exit code — or, under `--json`, the
//!    versioned DTO on stdout, because that is the line `dev.mjs` is reading.
//!
//! What did NOT survive, stated plainly: the argument-parsing guards
//! (`colliding_leaf_names_do_not_read_each_others_args`, which pinned a clap
//! `ArgMatches` panic when one leaf read another leaf's declared arg). There is
//! no command tree, no leaves, and no `ArgMatches` — the defect it guarded
//! cannot be expressed. The destructive-confirm gate it sat beside moved to
//! `host_plane::deleting_an_issue_needs_confirmation_it_can_actually_ask_for`,
//! which drives the 409 path `serve` answers with.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Clean-env entrypoint for this binary (step 0 of the Agent Experience
/// initiative). A developer's shell may export `$LAIT_HOME` pointing at their
/// live identity-scoped daemon. Scrubbed once at binary load, before any test
/// runs, so spawned test hosts cannot collide with it.
#[ctor::ctor]
fn scrub_ambient_lait_env() {
    for key in ["LAIT_HOME", "LAIT_STORE", "LAIT_CONFIG_ROOT"] {
        std::env::remove_var(key);
    }
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_lait")
}

/// A home that stops whatever daemon is serving it when it goes out of scope.
///
/// These tests run daemons with idle-shutdown disabled, so nothing reclaims one
/// that is merely abandoned — an abandoned daemon here lives until the machine
/// is rebooted, holding a lock, a socket and whatever ports its build binds.
///
/// Two ways one gets abandoned, and neither is a mistake a call site can be
/// trusted to avoid. A panic between the spawn and the stop skips the stop, and
/// every assertion in these tests sits in that gap. Worse, the launcher spawns
/// its daemon *behind* the process a test can see, so killing the child it
/// holds a handle to does not touch it — `takes_over_fake_daemon` did exactly
/// that and leaked one real daemon per run, on the passing path.
///
/// Ownership is the boundary: the home owns the daemon, so unwinding stops it.
struct TmpHome(std::path::PathBuf);

impl std::ops::Deref for TmpHome {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for TmpHome {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for TmpHome {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TmpHome {
    fn drop(&mut self) {
        stop_daemon(&self.0, Duration::from_secs(5));
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A short-lived home. Kept short on purpose: the control socket lives inside it
/// on unix and `sun_path` caps at 104 bytes (100 here), so a long temp path would
/// silently push the socket to the hashed temp-dir fallback.
fn tmp_home(tag: &str) -> TmpHome {
    let d = std::env::temp_dir().join(format!("lt-{}-{}", tag, std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    // Windows only, and deliberately: these tests hand-build paths the daemon
    // resolves through `Selection`, which canonicalizes. A CI runner's temp dir
    // sits under an 8.3 alias (`RUNNER~1`), so the two spellings name one
    // directory and compare unequal — the daemon binds its socket under one and
    // the probe waits out its deadline on the other. Not done on unix: the
    // control socket lives in here and `sun_path` caps at 104 bytes, which is
    // why this path is short in the first place, and macOS canonicalization
    // prepends `/private` to every temp path.
    #[cfg(windows)]
    let d = lait::config::canonical(&d);
    TmpHome(d)
}

/// The per-test config root. `$LAIT_HOME` isolates the *store*, but the Orbit
/// registry lives under the config root — so without this every space founded
/// here files itself in the developer's real catalog and never leaves.
fn config_root(home: &std::path::Path) -> std::path::PathBuf {
    home.join("cfg")
}

/// Start the local app (the default mode) against `home`, capturing its stdout.
///
/// `--port 0` binds an ephemeral port, so concurrent tests never collide on the
/// default one. `LAIT_IDLE_SECS=0` disables idle-shutdown, so the daemon this
/// starts is *guaranteed* to still be up when the server exits — the race the
/// stdout guard below needs to be deterministic.
fn serve(home: &std::path::Path) -> std::process::Child {
    Command::new(bin())
        .env("LAIT_HOME", home)
        .env("LAIT_CONFIG_ROOT", config_root(home))
        .env("LAIT_IDLE_SECS", "0")
        .env("LAIT_NETWORK", "isolated")
        // A test daemon never hosts the display coordinator: its fixed
        // machine-scoped port would make parallel test daemons — and any real
        // daemon on this machine — mutually exclusive.
        .env("LAIT_DISPLAY", "off")
        .args(["--json", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lait")
}

/// Stop the daemon serving `home`, so it cannot outlive the test.
fn shutdown(home: &std::path::Path) {
    stop_daemon(home, Duration::from_secs(15));
}

/// Ask the daemon serving `home` to stop, and wait `patience` for it to go.
///
/// Best-effort by construction: some of these tests put a *fake* daemon on that
/// socket, which will never answer `Stop` and never become absent. Waiting the
/// full patience on one of those is the cost of not having to know which is
/// which at the call site, so the drop path spends less of it than a test that
/// is deliberately proving a daemon stopped.
fn stop_daemon(home: &std::path::Path, patience: Duration) {
    let daemon_home = home.join("daemon");
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return;
    };
    runtime.block_on(async {
        let client = lait::daemon::Client::at(daemon_home.clone());
        let _ = client
            .request(
                lait::control::ControlRoute::Daemon,
                &lait::control::Request::Stop,
                None,
            )
            .await;
        let deadline = Instant::now() + patience;
        while !matches!(
            lait::control::probe(&daemon_home).await,
            lait::control::Probe::Absent
        ) {
            if Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

/// The daemon a mode spawns must not outlive it *holding its stdout*.
///
/// The default mode is the shape that bites: it stands the daemon up before it
/// serves, and the daemon is still running when the server exits. On Windows
/// `CreateProcess` inherits every inheritable handle, not just the ones in
/// `STARTUPINFO`, so the daemon came up owning a write-end of the server's
/// captured stdout — its own `Stdio::null()` notwithstanding. The server
/// exited, the pipe never closed, and the caller blocked on an EOF that could
/// not arrive. `npm run dev` reads exactly that pipe.
///
/// Waits on the *read*, not the exit: the process exiting was never the broken
/// part. Reading on a thread keeps a regression a 15s failure that says why,
/// rather than a wedged test the runner shoots at 90s with no diagnosis.
#[test]
fn a_spawned_daemon_does_not_hold_our_stdout_open() {
    let home = tmp_home("hold");
    let mut child = serve(&home);

    // Read the readiness line first: it is emitted after `ensure_lait_daemon`,
    // so seeing it proves a daemon was really spawned and is really up.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        tx.send(stdout.read_to_string(&mut s).map(|_| s)).ok();
    });

    // Give the server a moment to print, then end it. Killing (rather than a
    // clean shutdown) is the sharpest form of the question: every handle this
    // process owned is gone, so if stdout still does not reach EOF, somebody
    // else is holding it.
    std::thread::sleep(Duration::from_secs(3));
    let _ = child.kill();
    let _ = child.wait();
    let read = rx.recv_timeout(Duration::from_secs(15));

    // Before any assert: a live daemon is what wedges the reader, and on failure
    // it would otherwise outlive the test and hold the *runner's* pipe too.
    shutdown(&home);

    let read = read.expect(
        "the server exited but its stdout never reached EOF — the daemon it spawned \
         inherited the write end and is holding it open. Whoever captures lait's \
         stdout (`npm run dev`, a test harness, an MCP client) hangs here.",
    );
    let banner = read.expect("reading stdout failed");
    assert!(
        banner.contains("\"token\""),
        "the readiness line must have been emitted (so a daemon was really \
         started before the pipe question was asked); got: {banner:?}",
    );
}

/// `try_wait` must answer *both* ways: a daemon that died is reported dead, and
/// one that is running is not reported dead.
///
/// This is the sensor the spawn wait leans on — "a daemon that has already exited
/// is never going to answer", which is what turns a lock conflict into its own
/// message instead of a 20s timeout blaming the transport. On Windows it is not
/// `std::process::Child::try_wait` but a hand-rolled equivalent (the daemon is
/// spawned through `CreateProcessW` to bound what it inherits), so both answers
/// are pinned here: a false "still running" costs the fast path, and a false
/// "exited" would blame a daemon that is coming up fine.
#[test]
fn a_dead_daemon_is_reported_dead_and_a_live_one_is_not() {
    let exe = std::path::PathBuf::from(bin());
    let home = tmp_home("live");
    let daemon_home = home.join("daemon");
    // Inherited by both daemons this test spawns — a test daemon never hosts
    // the display coordinator (see `serve` above). Process-wide is safe here:
    // nextest runs each test in its own process, and every test wants it off.
    std::env::set_var("LAIT_DISPLAY", "off");
    let mut child = lait::daemon_spawn::spawn(&exe, None, Some(&home)).expect("spawn live daemon");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let client = lait::daemon::Client::at(daemon_home.clone());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while !matches!(client.probe().await, lait::control::Probe::Healthy { .. }) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "Lait daemon did not become ready"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let alive = child.try_wait().expect("try_wait");
    assert!(
        alive.is_none(),
        "a daemon that had just started was reported as exited ({alive:?}) — the \
         spawn wait would abandon a daemon that was coming up fine",
    );

    // Dead: a second host for the same identity loses the process lock and exits
    // immediately. Its stderr must remain wired to the diagnostic log.
    let log_path = home.join("duplicate.log");
    let log = std::fs::File::create(&log_path).expect("create duplicate log");
    let mut duplicate =
        lait::daemon_spawn::spawn(&exe, Some(log), Some(&home)).expect("spawn duplicate");
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match duplicate.try_wait().expect("try_wait duplicate") {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            None => panic!("duplicate Lait daemon was never reported as exited"),
        }
    };
    assert!(!status.success(), "duplicate host must fail: {status}");
    let said = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        said.contains("another lait daemon"),
        "duplicate diagnosis must reach its log; got: {said:?}"
    );

    runtime.block_on(async {
        let client = lait::daemon::Client::at(daemon_home);
        client
            .request(
                lait::control::ControlRoute::Daemon,
                &lait::control::Request::Stop,
                None,
            )
            .await
            .expect("stop Lait daemon");
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            None => panic!("a stopped daemon was never reported as exited"),
        }
    }
}

/// A selector that matches nothing is a not-found, and must answer like one on
/// every channel: prose shape, `--json` DTO, and exit code.
///
/// `--orbit` is the one selector the launcher still resolves, and it resolves it
/// *before* binding a port — so this is also the guard that a startup refusal
/// reaches `dev.mjs` as the first stdout line rather than as silence.
#[test]
fn an_unresolvable_orbit_is_refused_in_one_voice() {
    let home = tmp_home("orbit");
    let run = |args: &[&str]| {
        Command::new(bin())
            .env("LAIT_HOME", &home)
            .env("LAIT_CONFIG_ROOT", config_root(&home))
            .env("LAIT_IDLE_SECS", "0")
            .env("LAIT_DISPLAY", "off")
            .args(args)
            .output()
            .expect("spawn lait")
    };

    let out = run(&["--orbit", "nosuchspace", "--port", "0"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // anyhow's Termination printed `Error:` (capitalised, Debug) while the daemon
    // path printed `error:` — two voices in one binary.
    assert!(
        stderr.starts_with("error:"),
        "errors must use the lowercase `error:` voice; got: {stderr}",
    );
    assert!(
        !stderr.contains("Caused by:"),
        "the cause chain is anyhow's Debug output, not a contract; got: {stderr}",
    );
    // Not-found and ambiguous selectors exit 2; generic termination flattened this to 1.
    assert_eq!(
        out.status.code(),
        Some(2),
        "a selector matching nothing must exit 2; stderr: {stderr}",
    );

    // `--json` is a contract: `dev.mjs` reads the first stdout line and checks
    // `kind === "error"` before it looks for `{token, port}`. Prose on stderr and
    // an empty stdout leaves it waiting for a line that never comes.
    let out = run(&["--json", "--orbit", "nosuchspace", "--port", "0"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {stdout:?}"));
    assert_eq!(v["kind"], "error");
    assert_eq!(
        v["error_kind"], "not_found",
        "the DTO must carry the typed kind, not just prose: {v}",
    );
    assert_eq!(out.status.code(), Some(2));
}

/// An argv that is not one of the three modes is refused, and the refusal says
/// what lait actually is.
///
/// The whole carve in one assertion: `lait issues ls` is not a typo to be
/// corrected, it is a category error, and the message has to name the three
/// processes rather than suggest a nearer subcommand.
#[test]
fn anything_that_is_not_a_mode_is_refused_by_name() {
    let out = Command::new(bin())
        .arg("issues")
        .output()
        .expect("spawn lait");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(out.status.code(), Some(0), "must not succeed: {stderr}");
    assert!(
        stderr.contains("not a command surface"),
        "the refusal must say what lait is; got: {stderr}",
    );
    for mode in ["lait daemon", "lait mcp"] {
        assert!(
            stderr.contains(mode),
            "the refusal must name `{mode}`; got: {stderr}",
        );
    }
}

/// Stand a fake daemon on `home`'s *daemon* control socket, replying `reply` to
/// every request, and start the local app against it. Returns (stderr, exit
/// code, elapsed).
///
/// The daemon socket, not an Orbit's: the launcher's first act in serve mode is
/// to stand the identity's daemon up, and that is where this diagnosis has to
/// happen — before a port is bound and before any Orbit is addressed.
#[cfg(unix)]
fn against_fake_daemon(tag: &str, reply: &'static [u8]) -> (String, Option<i32>, Duration) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let home = tmp_home(tag);
    let daemon_home = home.join("daemon");
    std::fs::create_dir_all(&daemon_home).expect("daemon home");

    let sock = lait::config::socket_path(&daemon_home);
    std::fs::remove_file(&sock).ok();
    let listener = UnixListener::bind(&sock).expect("bind fake daemon");
    let fake = std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut s) = stream else { continue };
            let mut line = String::new();
            BufReader::new(s.try_clone().unwrap())
                .read_line(&mut line)
                .ok();
            s.write_all(reply).ok();
            s.write_all(b"\n").ok();
        }
    });

    let started = Instant::now();
    let out = Command::new(bin())
        .env("LAIT_HOME", &home)
        .env("LAIT_CONFIG_ROOT", config_root(&home))
        .env("LAIT_IDLE_SECS", "0")
        .args(["--port", "0"])
        .output()
        .expect("spawn lait");
    let elapsed = started.elapsed();

    drop(fake);
    std::fs::remove_file(&sock).ok();
    std::fs::remove_dir_all(&home).ok();
    (
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
        elapsed,
    )
}

/// Start the head against a fake daemon and wait for its readiness line.
///
/// The sibling harness above waits for *exit*, which only answers when the
/// launcher refuses. A launcher that takes over and comes up is a service: it
/// never exits, so `output()` would block until the harness killed it. This one
/// reads the one line `--json` promises and then stops the process.
#[cfg(unix)]
fn takes_over_fake_daemon(tag: &str, reply: &'static [u8]) -> (String, Duration) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    let home = tmp_home(tag);
    let daemon_home = home.join("daemon");
    std::fs::create_dir_all(&daemon_home).expect("daemon home");

    let sock = lait::config::socket_path(&daemon_home);
    std::fs::remove_file(&sock).ok();
    let listener = UnixListener::bind(&sock).expect("bind fake daemon");
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut s) = stream else { continue };
            let mut line = String::new();
            BufReader::new(s.try_clone().unwrap())
                .read_line(&mut line)
                .ok();
            s.write_all(reply).ok();
            s.write_all(b"\n").ok();
        }
    });

    let started = Instant::now();
    let mut child = Command::new(bin())
        .env("LAIT_HOME", &home)
        .env("LAIT_CONFIG_ROOT", config_root(&home))
        .env("LAIT_DISPLAY", "off")
        .args(["--json", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lait");

    // On its own thread with a deadline: the point of the test is that this
    // arrives *promptly*, so a hang has to fail the assertion rather than the
    // suite's 90s timeout.
    let mut out = BufReader::new(child.stdout.take().expect("stdout"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = out.read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(30)).unwrap_or_default();
    let elapsed = started.elapsed();

    // Killing the launcher is not killing the daemon. When the launcher takes
    // over it spawns one that this process never holds a handle to, and it
    // outlives everything here unless the home stops it — which `TmpHome` does
    // on the way out of this function.
    child.kill().ok();
    child.wait().ok();
    // The socket stays until `TmpHome` has used it. Unlinking it here is what
    // made the guard silently useless: `Stop` is delivered *through* that path,
    // so removing it first left the taken-over daemon unreachable and immortal.
    // The guard removes the whole home, socket included, after it has stopped.
    (line, elapsed)
}

/// A daemon *behind* this build is taken over promptly — not waited out.
///
/// The reply here is a pre-handshake daemon (v0.4.8): it has no `hello`, so
/// serde rejects the request as an unknown variant, and that rejection is the
/// identification. `control::probe` calls that one `replaceable`, so the
/// launcher replaces it and carries on rather than refusing — which is why this
/// asserts on the readiness line and its sibling below asserts on an exit. The
/// promise both share is the timing: the old path spawned a doomed second daemon
/// over the held lock and polled a full 20s before blaming the timeout.
#[cfg(unix)]
#[test]
fn an_older_daemon_is_taken_over_not_timed_out() {
    let (line, elapsed) = takes_over_fake_daemon(
        "older",
        br#"{"kind":"error","message":"bad request: unknown variant `hello`","error_kind":"error"}"#,
    );

    assert!(
        elapsed < Duration::from_secs(20),
        "an older daemon must be taken over promptly, took {elapsed:?}",
    );
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("readiness line {line:?}: {e}"));
    assert!(
        v.get("token").and_then(|t| t.as_str()).is_some(),
        "the head must come up and announce itself; got: {line}",
    );
    assert!(
        v.get("port").and_then(|p| p.as_u64()).is_some(),
        "the readiness line is what dev.mjs parses; got: {line}",
    );
}

/// The asymmetry, end to end: a daemon *ahead* of this build is named and left
/// alone. Replacing it downgrades the node, and a store already written at a
/// newer `SCHEMA_VERSION` would then refuse to open at all.
///
/// There is no longer any code that could stop it — the interactive "stop it and
/// continue?" repair went with the command surface — so this pins the half that
/// remains and has to keep being true: the refusal names *upgrading* as the way
/// out, and never claims to have stopped anything.
#[cfg(unix)]
#[test]
fn a_newer_daemon_is_named_and_never_replaced() {
    let (stderr, code, _) =
        against_fake_daemon("newer", br#"{"kind":"hello","protocol_version":9000}"#);

    assert_ne!(code, Some(0), "must not proceed; stderr: {stderr}");
    assert!(
        stderr.contains("upgrade lait"),
        "the way out of being behind is to upgrade; got: {stderr}",
    );
    assert!(
        !stderr.contains("stopped it"),
        "must never stop a daemon newer than this build; got: {stderr}",
    );
}

/// `--version` answers with nothing running.
///
/// A binary whose version cannot be read is a support problem the moment two
/// builds are in the field, and every documented consumer asks this way:
/// `docs/INSTALL.md`'s verification step and `dev-release.yml`, whose whole
/// mechanism for telling a dev prerelease from a tagged build is this string.
/// A running node answers the same question over the host plane; this is the
/// answer available before there is a node.
#[test]
fn the_launcher_reports_its_build_without_starting_anything() {
    let out = Command::new(bin())
        .arg("--version")
        .output()
        .expect("spawn lait --version");
    assert!(out.status.success(), "--version must not be an error");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.trim() == format!("lait {}", lait::VERSION),
        "got: {text:?}"
    );
    // Not a usage message: the failure this guards is `--version` falling into
    // the launcher's unknown-argument arm, which exits 1 with the whole USAGE
    // block and leaves installers with no way to verify what they installed.
    assert!(!text.contains("not a command surface"), "got: {text:?}");
}
