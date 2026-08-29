use std::path::Path;
use std::sync::Arc;

use world_runner::Provenance;
use world_runner::{Instance, Release};
use world_sdk::{RemoteClient, RemoteWorld};

#[test]
fn the_shipped_signage_process_exposes_its_host_adapters() {
    let executable = Path::new(env!("CARGO_BIN_EXE_lait-world-signage"));
    let root = executable.parent().expect("runner output directory");
    let program = executable.file_name().expect("runner executable name");
    let release = Release::under(
        root,
        signage::product_world(),
        env!("CARGO_PKG_VERSION"),
        Provenance::Sealed([0x92; 32]),
        Path::new(program),
        Vec::new(),
        None::<&Path>,
    )
    .expect("an immutable Signage release");

    let remote = Arc::new(
        RemoteWorld::connect(Instance::launch(release).expect("runner launches"))
            .expect("semantic adapter connects"),
    );
    let client = RemoteClient::connect(remote).expect("client adapter connects");
    assert_eq!(client.declaration().mount, signage_app::MOUNT);
}
