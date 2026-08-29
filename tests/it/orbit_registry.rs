//! What the Orbit registry is allowed to remember, and what it must record.
//!
//! Both tests here exist because the opposite went unnoticed in a release.
//! `orbits::touch` was written, unit-tested, and reachable from nothing, so the
//! picker ordered by a timestamp frozen at founding; and a `projects` snapshot
//! taken at founding outlived every project it named. Neither failed anything.

use std::path::Path;

use crate::head::{stop_daemon, temp_root, Head};

fn registry(config: &Path) -> Vec<serde_json::Value> {
    let raw = std::fs::read_to_string(config.join("spaces.json")).expect("read spaces.json");
    serde_json::from_str(&raw).expect("registry is json")
}

/// Serving a Space records the open.
///
/// The assertion is deliberately against a *backdated* row rather than "is it
/// recent": founding writes `last_opened` too, so a test that only checks for a
/// fresh timestamp passes whether or not the open path does anything at all.
/// That is precisely how the unwired `touch` survived.
#[test]
fn serving_a_space_records_the_open() {
    let root = temp_root("registry-open");
    let config = root.join("config");
    let project = root.join("p");
    std::fs::create_dir_all(&project).unwrap();

    let head = Head::start(&config, None);
    let orbit = head.found(&project, "Drift");
    head.stop();
    stop_daemon(&config, None);

    let mut rows = registry(&config);
    assert_eq!(rows.len(), 1, "one founded Space: {rows:?}");
    rows[0]["last_opened"] = serde_json::json!(1);
    std::fs::write(
        config.join("spaces.json"),
        serde_json::to_string_pretty(&rows).unwrap(),
    )
    .unwrap();

    // Address the Orbit so it is really placed, not merely registered — a
    // Station starts on use, and starting is what has to leave a mark.
    let head = Head::start(&config, None);
    let (status, info) = head.space(&orbit, serde_json::json!({ "cmd": "status" }));
    assert_eq!(status, 200, "status for {orbit}: {info}");
    head.stop();
    stop_daemon(&config, None);

    let rows = registry(&config);
    assert_ne!(
        rows[0]["last_opened"],
        serde_json::json!(1),
        "serving a Space must record the open — the picker orders by this, and \
         the function that writes it once had no callers at all"
    );
}

/// A listed row names where a Space is, never what it contains.
///
/// The wire shape is the guard rail: a content field cannot drift if it is not
/// there to be stored. Adding one back fails here rather than a year later in a
/// picker that quietly serves a founding-day answer.
#[test]
fn a_listed_row_carries_no_contents() {
    let root = temp_root("registry-shape");
    let config = root.join("config");
    let project = root.join("p");
    std::fs::create_dir_all(&project).unwrap();

    let head = Head::start(&config, None);
    head.found(&project, "Shape");
    let (status, listing) = head.get("/api/spaces");
    assert_eq!(status, 200, "spaces: {listing}");

    let row = &listing["spaces"][0];
    assert!(
        row.get("projects").is_none(),
        "a project snapshot is Catalog content and must not be carried here: {row}"
    );
    assert!(
        row.get("name").is_some(),
        "a row still reports a name field, present or null: {row}"
    );
    // The one content a row may carry is a past reading, and a reading never
    // travels without its time -- that is what keeps it from being a cache.
    if let Some(seen) = row.get("seen") {
        assert!(
            seen["observed_at"].as_u64().is_some(),
            "a remembered name says when it was read: {row}"
        );
    }
    head.stop();
    stop_daemon(&config, None);
}

/// A Space nobody has opened since the daemon started is listed by what this
/// device last saw it named -- as a past reading with its time, beside a
/// `name` that is honestly null.
///
/// This is the picker's first-launch defect: the daemon names a Space only
/// from a live Station, listing never places one, and every Orbit not yet
/// clicked showed as its `ws_` id. The record is written by the Station that
/// held the store (start, Catalog change, stop) and read passively; a stopped
/// daemon is exactly the state it exists for.
#[test]
fn an_unopened_space_is_listed_by_what_this_device_last_saw() {
    let root = temp_root("registry-seen");
    let config = root.join("config");
    let project = root.join("p");
    std::fs::create_dir_all(&project).unwrap();

    let head = Head::start(&config, None);
    let orbit = head.found(&project, "Kas");
    // Founding registers the Space; serving it is what reads the name. Address
    // the Orbit so a Station is placed and docks the World, as opening does.
    let (status, info) = head.space(&orbit, serde_json::json!({ "cmd": "status" }));
    assert_eq!(status, 200, "status for {orbit}: {info}");
    let (status, listing) = head.get("/api/spaces");
    assert_eq!(status, 200, "spaces: {listing}");
    let row = &listing["spaces"][0];
    assert_eq!(row["name"], serde_json::json!("Kas"), "live: {row}");
    let space = row["space"].as_str().expect("space id").to_string();
    head.stop();
    stop_daemon(&config, None);

    // The Station that served it left the reading beside its store.
    let record = project
        .join(".lait")
        .join("orbital")
        .join(&space)
        .join("observed-name.json");
    assert!(
        record.is_file(),
        "the Station records the name it read: {}",
        record.display()
    );

    // A fresh daemon has placed nothing. The live name is absent and says
    // why; the observation names the Space and says when.
    let head = Head::start(&config, None);
    let (status, listing) = head.get("/api/spaces");
    assert_eq!(status, 200, "spaces: {listing}");
    let row = &listing["spaces"][0];
    assert_eq!(row["id"], serde_json::json!(orbit), "{row}");
    assert_eq!(
        row["name"],
        serde_json::Value::Null,
        "not a live reading: {row}"
    );
    assert_eq!(row["unnamed"], serde_json::json!("not-probed"), "{row}");
    assert_eq!(row["seen"]["name"], serde_json::json!("Kas"), "{row}");
    assert!(
        row["seen"]["observed_at"]
            .as_u64()
            .is_some_and(|at| at > 1_700_000_000),
        "a reading carries its time: {row}"
    );
    head.stop();
    stop_daemon(&config, None);
}
