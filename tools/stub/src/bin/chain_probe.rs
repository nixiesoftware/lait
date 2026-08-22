//! The reference entry binary for the staged-swap chain test.
//!
//! It plays the client: the chain test copies it into a tree as
//! `astrolabe(.exe)` beside a `version.txt`, and what it announces is the
//! version of the tree it actually ran from — which is how the test knows
//! *which* tree the stub launched, not merely that something launched. The
//! same division as `astrolabe-display-reference`: a reference process the
//! seam tests spawn for real, never shipped to a person.

fn main() {
    let exe = std::env::current_exe().expect("the probe knows its own path");
    let version = std::fs::read_to_string(exe.with_file_name("version.txt"))
        .expect("a version.txt beside the entry binary");
    match std::env::var("CHAIN_PROBE_ANNOUNCE") {
        Ok(out) => {
            // Written aside and renamed in, so a poller never reads a
            // half-written announcement — the same publish-then-point rule
            // as everything else on this path.
            let out = std::path::Path::new(&out);
            let staged = out.join(format!("launched.txt.tmp-{}", std::process::id()));
            std::fs::write(&staged, version.trim()).expect("the announce file writes");
            std::fs::rename(&staged, out.join("launched.txt"))
                .expect("the announce file publishes");
        }
        Err(_) => println!("{}", version.trim()),
    }

    // CHAIN_PROBE_RUNS: append one line per run — the tree that ran and the
    // relaunch env's value (the env *name* comes from the test, which passes
    // the client-side spelling; a line only ever says `env=<v>` if the stub
    // set the same name, which is the weld).
    if let Ok(runs) = std::env::var("CHAIN_PROBE_RUNS") {
        use std::io::Write as _;
        let answered = std::env::var("CHAIN_PROBE_ENV_NAME")
            .ok()
            .and_then(|name| std::env::var(name).ok())
            .unwrap_or_else(|| "-".into());
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(runs)
            .expect("the runs log opens");
        writeln!(log, "{} env={}", version.trim(), answered).expect("the runs log appends");
    }

    // The relaunch rehearsal: playing the client half of the request seam.
    // First run (no marker yet): wait for the gate if one is named — the
    // test stages a release in that window — then write the request exactly
    // as `client::update::request_relaunch` would, and exit. A later run
    // finds the marker and exits plainly, which is how the test tells
    // "relaunched onto the answer" from "spun".
    if let Ok(marker) = std::env::var("CHAIN_PROBE_RELAUNCH_ONCE") {
        let marker = std::path::PathBuf::from(marker);
        if !marker.exists() {
            if let Ok(gate) = std::env::var("CHAIN_PROBE_RELAUNCH_GATE") {
                let gate = std::path::PathBuf::from(gate);
                for _ in 0..200 {
                    if gate.exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            let request = std::env::var("CHAIN_PROBE_REQUEST")
                .expect("a rehearsal names where the request is written");
            let body = std::env::var("CHAIN_PROBE_REQUEST_BODY").unwrap_or_default();
            std::fs::write(&marker, b"asked").expect("the rehearsal marker writes");
            std::fs::write(request, body).expect("the relaunch request writes");
        }
    }
}
