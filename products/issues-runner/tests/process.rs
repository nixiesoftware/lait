use std::path::Path;
use std::sync::Arc;

use world_runner::Provenance;
use world_runner::{Instance, Release};
use world_sdk::{remote_exec_package, RemoteClient, RemoteWorld};

#[test]
fn the_shipped_issues_process_exposes_every_host_adapter() {
    let executable = Path::new(env!("CARGO_BIN_EXE_lait-world-issues"));
    let root = executable.parent().expect("runner output directory");
    let program = executable.file_name().expect("runner executable name");
    let release = Release::under(
        root,
        issues::product_world(),
        env!("CARGO_PKG_VERSION"),
        Provenance::Sealed([0x91; 32]),
        Path::new(program),
        Vec::new(),
        None::<&Path>,
    )
    .expect("an immutable Issues release");

    let remote = Arc::new(
        RemoteWorld::connect(Instance::launch(release).expect("runner launches"))
            .expect("semantic adapter connects"),
    );
    let exec = remote_exec_package(Arc::clone(&remote)).expect("Exec adapter connects");
    assert!(
        !exec.specs().is_empty(),
        "Issues publishes its verification spec"
    );
    let client = RemoteClient::connect(remote).expect("client adapter connects");
    assert_eq!(client.declaration().mount, issues_app::MOUNT);
    assert!(
        !client.declaration().tools.is_empty(),
        "JSON-bearing MCP declarations cross the process boundary"
    );
}
