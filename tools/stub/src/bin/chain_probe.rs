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
}
