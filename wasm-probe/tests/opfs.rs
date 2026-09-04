//! The OPFS medium against a real browser: sync access handles in a real
//! dedicated worker, real persistence across a medium reopen, and the full
//! Store — indexes, manifest, GC — running on it. This is the slice-3 exit
//! criterion; Node cannot host it (no OPFS), so this binary runs under
//! `wasm-pack test --headless --chrome --test opfs`.

#![cfg(all(target_arch = "wasm32", feature = "probe-journal"))]

use std::sync::Arc;

use journal::{Index, Medium, OpfsMedium, Store};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// Every test gets its own OPFS directory: one browser session serves the
/// whole run, and a leaked lock or leftover pool must not bleed across
/// tests.
fn unique_dir(tag: &str) -> String {
    let mut noise = [0u8; 8];
    let _ = getrandom03::fill(&mut noise);
    format!("probe-{tag}-{}", data_encoding_hex(&noise))
}

fn data_encoding_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[wasm_bindgen_test]
async fn the_pool_medium_round_trips_and_survives_reopen() {
    let dir = unique_dir("medium");
    let medium = OpfsMedium::open(&dir).await.expect("medium opens");
    let (mut writer, read) = medium.open_slot("hot-0").expect("slot from a spare");
    assert_eq!(writer.len(), 0, "a fresh slot is empty at its logical zero");
    writer.append(b"alpha").expect("append");
    writer.append(b"beta").expect("append");
    writer.flush().expect("flush");
    let mut buf = [0u8; 4];
    read.read_at(5, &mut buf).expect("read at offset");
    assert_eq!(&buf, b"beta");
    assert!(
        read.read_at(6, &mut buf).is_err(),
        "a read past the end errors, never short-reads"
    );
    writer.truncate(5).expect("truncate");

    // Everything released; the same directory must come back with the same
    // bytes under the same logical name.
    drop(writer);
    drop(read);
    drop(medium);
    let medium = OpfsMedium::open(&dir).await.expect("medium reopens");
    assert_eq!(
        medium.slot_names().expect("names"),
        vec!["hot-0".to_owned()]
    );
    let (writer, read) = medium.open_slot("hot-0").expect("slot resumes");
    assert_eq!(writer.len(), 5, "the truncated length survived");
    let mut buf = [0u8; 5];
    read.read_at(0, &mut buf).expect("read");
    assert_eq!(&buf, b"alpha");
}

#[wasm_bindgen_test]
async fn a_removed_slot_recycles_into_the_pool() {
    let dir = unique_dir("recycle");
    let medium = OpfsMedium::open(&dir).await.expect("medium opens");
    let (mut writer, read) = medium.open_slot("hot-0").expect("slot");
    writer.append(b"short-lived").expect("append");
    writer.flush().expect("flush");
    drop(writer);
    drop(read);

    medium.remove_slot("hot-0").expect("recycled");
    assert!(medium.slot_names().expect("names").is_empty());
    // The next slot may land on the recycled physical file; its past must
    // be gone either way.
    let (writer, _read) = medium.open_slot("hot-1").expect("slot from pool");
    assert_eq!(writer.len(), 0, "a recycled file carries nothing forward");
}

#[wasm_bindgen_test]
async fn the_full_store_runs_on_real_opfs() {
    let dir = unique_dir("store");
    let medium = OpfsMedium::open(&dir).await.expect("medium opens");
    let mut store = Store::open_on(Arc::new(medium)).expect("store opens");
    let sequence = store
        .commit(&[b"issue-one".to_vec()], &[], Index::NONE, b"meta".to_vec())
        .expect("commit lands");
    store
        .collect_unreachable()
        .expect("compaction runs on OPFS");
    drop(store);

    // A cold reopen: recovery walks real OPFS bytes, elects the compacted
    // generation, and every promise re-verifies.
    let medium = OpfsMedium::open(&dir).await.expect("medium reopens");
    let store = Store::open_on(Arc::new(medium)).expect("store reopens");
    assert_eq!(store.manifest().map(|m| m.sequence), Some(sequence));
    assert_eq!(
        store.caller_meta().expect("meta reads"),
        Some(b"meta".to_vec())
    );
    let required = store.required_objects().expect("required lists");
    assert_eq!(
        store.read_object(&required[0]).expect("object reads"),
        b"issue-one"
    );
}
