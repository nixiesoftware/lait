//! A stopped daemon must leave nothing behind in the head that started it.
//!
//! The daemon is spawned as an ordinary child of the head, and on unix a child
//! whose parent never `wait`s becomes a zombie the moment it exits: still
//! listed by `ps`, still answering `kill -0`, and immune to `kill -9` — there
//! is no process left to signal, only a table entry the parent has not
//! collected. It clears when the parent dies, and not before.
//!
//! That is indistinguishable, from outside, from a daemon that refuses to die.
//! It is how this surfaced: SIGTERM "ignored", SIGKILL "ignored", a daemon that
//! reappeared after every kill — all of it a corpse the head never reaped, over
//! a daemon that had in fact exited instantly.
//!
//! So the assertion here is deliberately not "the daemon stopped answering", or
//! "`ps` no longer shows it running". Both were already true of the zombie. It
//! is that the pid is *gone* — reaped — while the head that spawned it is still
//! alive, which is the only state in which the bug could ever have been seen.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::head::{temp_root, Head};

/// The process state `ps` reports, or `None` once the pid is gone entirely.
///
/// `kill -0` cannot answer this: it succeeds for a zombie, which is the case
/// under test. The state letter is the discriminator — `Z` is a corpse nobody
/// has collected.
fn state(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if state.is_empty() {
        None
    } else {
        Some(state)
    }
}

fn pid(config: &Path) -> u32 {
    let path = config.join("daemon").join("daemon.pid");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(pid) = raw.trim().parse::<u32>() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the head never recorded a daemon pid at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_stopped_daemon_is_reaped_while_the_head_that_spawned_it_lives_on() {
    let root = temp_root("daemon-reaping");
    let config = root.join("config");

    // The head spawns the daemon; keeping the head bound for the whole test is
    // the point, because a head that exits first would reparent the daemon to
    // init and hide the defect behind init's reaping.
    let mut head = Head::start(&config, None);
    let pid = pid(&config);
    assert!(
        state(pid).is_some(),
        "the daemon at pid {pid} was not running to begin with"
    );

    // SIGTERM, the ordinary way to stop it. `kill` rather than libc: that is a
    // dev-dependency on apple targets only, and this test is every unix.
    let killed = Command::new("kill")
        .arg(pid.to_string())
        .status()
        .expect("send SIGTERM");
    assert!(killed.success(), "SIGTERM to {pid} failed");

    // Generous, because this is a backstop against a corpse that would persist
    // forever, not a latency budget: a real shutdown here is well under a
    // second, and the watchdog's own deadline is 30s.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = None;
    while Instant::now() < deadline {
        match state(pid) {
            None => {
                // Reaped. The head is still up, which is what makes this mean
                // something — assert it rather than assume it.
                assert!(
                    head.is_running(),
                    "the head exited during the test, so the pid could have been \
                     cleared by init rather than by the head reaping it"
                );
                head.stop();
                return;
            }
            Some(state) => last = Some(state),
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let state = last.unwrap_or_default();
    let zombie = state.starts_with('Z');
    panic!(
        "the daemon at pid {pid} was still in the process table 60s after SIGTERM \
         (state {state:?}){}",
        if zombie {
            " — a zombie: it exited, and the head never reaped it"
        } else {
            " — still alive: it did not honour SIGTERM"
        }
    );
}

/// The harness has to reap too, and the failing path is the one that matters.
///
/// `Head::stop` is an explicit call, so it runs only when a test reaches its
/// end. A test that panics — one failed assertion — never got there, and
/// `Child`'s own drop neither kills nor waits, so the head, the daemon under
/// it, and every World runner beneath survived the test that started them.
///
/// That is worse than the disk it costs. An orphaned daemon holds the display
/// coordinator's fixed port, so the *next* test to want it fails, and orphans
/// its own — one bad assertion becoming a suite-wide cascade that reads as
/// flakiness and points at nothing.
#[test]
fn a_head_dropped_without_being_stopped_is_still_reaped() {
    let root = temp_root("drop-reaping");
    let config = root.join("config");

    let (head_pid, daemon_pid) = {
        let head = Head::start(&config, None);
        let pair = (head.pid(), pid(&config));
        assert!(
            state(pair.0).is_some(),
            "the head at pid {} was not running to begin with",
            pair.0
        );
        pair
        // Dropped here without `stop`, which is exactly what a panicking test
        // does.
    };

    // Gone, not merely stopped: a `Z` here would be the corpse this is about.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = None;
    while Instant::now() < deadline {
        last = state(head_pid);
        if last.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        last.is_none(),
        "a dropped head left pid {head_pid} behind as {last:?}"
    );

    // And the daemon it started went with it, or nothing was gained: the
    // daemon is what holds the ports the next test needs.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = None;
    while Instant::now() < deadline {
        last = state(daemon_pid);
        if last.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        last.is_none(),
        "a dropped head left its daemon at pid {daemon_pid} behind as {last:?}"
    );
}
