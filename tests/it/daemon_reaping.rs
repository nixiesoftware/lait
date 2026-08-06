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
