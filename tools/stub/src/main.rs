//! The stub launcher's entry point: claim the installation, apply what is
//! staged, start what is current, and stay for as long as the client runs.
//!
//! Staying is the whole enforcement of "nothing applies under a running
//! client": the claim this process holds *is* the fact that a client is
//! alive here, so a second launch defers its apply without any cooperation
//! from the client itself. It costs one idle process per session and buys an
//! invariant that would otherwise rest on a lock nothing takes.
//!
//! Every decision lives in the library, so the chain test and the unit tests
//! exercise the same code this binary runs.

fn main() {
    let root = match astrolabe_stub::discover_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("astrolabe-stub: the install root could not be discovered: {error}");
            std::process::exit(1);
        }
    };

    // The claim is held until this process exits — through the swap, and
    // through the client's whole run. `None` means something already holds
    // it, which is a deferral and not a failure: the launch still happens.
    let claim = match astrolabe_stub::claim(&root) {
        Ok(Some(claim)) => Some(claim),
        Ok(None) => {
            eprintln!(
                "astrolabe-stub: another client already holds this installation; \
                 any staged release applies at the next launch"
            );
            None
        }
        Err(error) => {
            eprintln!("astrolabe-stub: the installation could not be claimed: {error}");
            None
        }
    };

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    // A request surviving from an earlier session is answered by this very
    // launch; only one written by the client below means "come back".
    let _ = astrolabe_stub::take_relaunch_request(&root);
    let mut answering: Option<String> = None;
    loop {
        // The outcome has already been said (stderr and stub.log) by apply();
        // nothing here may turn a refused update into a refused launch.
        if let Some(claim) = &claim {
            let _ = astrolabe_stub::apply(&root, claim);
        }

        let mut child = match astrolabe_stub::launch_answering(&root, &args, answering.as_deref()) {
            Ok(child) => child,
            Err(error) => {
                eprintln!("astrolabe-stub: the client could not be started: {error}");
                std::process::exit(1);
            }
        };

        match child.wait() {
            Ok(status) => {
                // The client asked for the apply window and exited: loop
                // under the same claim, so whatever staged while it ran is
                // live on the very next start.
                answering = astrolabe_stub::take_relaunch_request(&root);
                if answering.is_some() {
                    continue;
                }
                drop(claim);
                std::process::exit(status.code().unwrap_or(0));
            }
            Err(error) => {
                eprintln!("astrolabe-stub: the client could not be waited on: {error}");
                std::process::exit(1);
            }
        }
    }
}
