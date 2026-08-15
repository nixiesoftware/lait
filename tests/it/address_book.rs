//! Process-level address-book contract.
//!
//! The crate and the in-process daemon service already prove persistence and
//! vacant resolve. This file exists because those proofs never start `lait
//! daemon`: they cannot catch the composition that has already been wrong
//! twice on this product — every part correct, the spawn / route / home seam
//! wrong.
//!
//! Two seams, two tests:
//!
//! 1. The identity daemon itself, over the control socket the supervisor
//!    opens. `BookPut` then `BookList`, then the same `BookList` after the
//!    process is gone and a new one has opened the same home.
//! 2. The HTTP host plane a head actually speaks, including the claim that
//!    `BookResolve` of a catalogued-but-vacant Orbit answers `unavailable`
//!    and does not place it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::head::{temp_root, Head};
use lait::control::{ControlRoute, Probe, Request, Response};
use lait::daemon::Client;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_lait")
}

/// A self-contained identity home. Canonicalized on Windows so the daemon's
/// named pipe and the probe agree on the spelling — the same trap
/// `launcher_safety` documents.
fn isolated_home(tag: &str) -> PathBuf {
    let dir = temp_root(tag);
    #[cfg(windows)]
    let dir = lait::config::canonical(&dir);
    dir
}

fn wait_healthy(client: &Client, budget: Duration) {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + budget;
        while !matches!(client.probe().await, Probe::Healthy) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "Lait daemon did not become ready at {}",
                client.home().display()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

fn ask(client: &Client, request: Request) -> Response {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        client
            .request(ControlRoute::Daemon, &request, None)
            .await
            .unwrap_or_else(|error| Response::err(format!("{error:#}")))
    })
}

fn stop_daemon_at(home: &Path, child: &mut lait::daemon_spawn::DaemonChild) {
    let daemon = home.join("daemon");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let client = Client::at(daemon.clone());
        let _ = client
            .request(ControlRoute::Daemon, &Request::Stop, None)
            .await;
        let deadline = Instant::now() + Duration::from_secs(15);
        while !matches!(lait::control::probe(&daemon).await, Probe::Absent) {
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.force_kill_and_wait();
                break;
            }
            Err(_) => break,
        }
    }
}

fn spawn_daemon(home: &Path) -> lait::daemon_spawn::DaemonChild {
    // `--home` is the child's identity. Do not pin `LAIT_*` on this
    // process: `cargo test` shares the env across threads, and a sibling
    // Head would inherit a home it did not ask for.
    let exe = PathBuf::from(bin());
    let log_path = home.join("daemon.log");
    let log = std::fs::File::create(&log_path).expect("daemon log");
    lait::daemon_spawn::spawn(&exe, Some(log), Some(home)).expect("spawn lait daemon")
}

fn cards_named(response: &Response, name: &str) -> usize {
    match response {
        Response::Book(view) => view.cards.iter().filter(|card| card.name == name).count(),
        other => panic!("expected a book, got {other:?}"),
    }
}

/// `BookPut` then `BookList` against a real `lait daemon`, and again after
/// that process is gone and a new one has opened the same home.
#[test]
fn a_real_daemon_puts_a_card_and_keeps_it_across_restart() {
    let home = isolated_home("bookd");
    std::fs::create_dir_all(&home).expect("home");

    let mut child = spawn_daemon(&home);
    let client = Client::at(home.join("daemon"));
    wait_healthy(&client, Duration::from_secs(20));

    let empty = ask(&client, Request::BookList);
    assert_eq!(
        cards_named(&empty, "Ada"),
        0,
        "fresh book is empty: {empty:?}"
    );

    let put = ask(
        &client,
        Request::BookPut {
            card: None,
            name: "Ada".into(),
            note: Some("colleague".into()),
        },
    );
    assert_eq!(
        cards_named(&put, "Ada"),
        1,
        "put should echo the card: {put:?}"
    );

    let listed = ask(&client, Request::BookList);
    assert_eq!(
        cards_named(&listed, "Ada"),
        1,
        "list should see the card: {listed:?}"
    );
    let Response::Book(view) = &listed else {
        panic!("list should return the book: {listed:?}");
    };
    assert_eq!(view.cards[0].note, "colleague");

    // A selector the catalog has never heard of. The answer is coverage,
    // not an error, and it must not be a Card-existence bit.
    let vacant = ask(
        &client,
        Request::BookResolve {
            orbit: "no-such-orbit".into(),
            handles: vec!["dev_not_a_device".into()],
        },
    );
    let Response::BookResolution(resolution) = vacant else {
        panic!("vacant resolve must return a resolution, got {vacant:?}");
    };
    assert!(resolution.hits.is_empty(), "vacant hits: {resolution:?}");
    assert_eq!(
        resolution.coverage.as_deref(),
        Some("unavailable"),
        "vacant coverage: {resolution:?}"
    );

    stop_daemon_at(&home, &mut child);

    let mut child = spawn_daemon(&home);
    let client = Client::at(home.join("daemon"));
    wait_healthy(&client, Duration::from_secs(20));
    let again = ask(&client, Request::BookList);
    assert_eq!(
        cards_named(&again, "Ada"),
        1,
        "the card must survive a new process opening the same home: {again:?}"
    );

    stop_daemon_at(&home, &mut child);
    std::fs::remove_dir_all(&home).ok();
}

/// The host plane a head actually speaks: it carries the book, the Space
/// route refuses it by name, and `BookResolve` of a catalogued-but-vacant
/// Orbit does not place that Orbit.
#[test]
fn the_host_plane_carries_the_book_and_resolve_does_not_place() {
    let root = temp_root("bookh");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project");

    // Shared per-user identity under `config`, same shape as `host_plane`.
    // A self-contained `--home` cannot found into a *different* directory
    // — that store would be "hosted here under its own key".
    let head = Head::start(&config, None);

    let (status, empty) = head.host(serde_json::json!({ "cmd": "book_list" }));
    assert_eq!(status, 200, "book_list: {empty}");
    assert_eq!(empty["kind"], "book", "{empty}");
    assert_eq!(empty["cards"].as_array().map(Vec::len), Some(0), "{empty}");

    let (status, put) = head.host(serde_json::json!({
        "cmd": "book_put",
        "name": "Ada",
        "note": "colleague",
    }));
    assert_eq!(status, 200, "book_put: {put}");
    assert_eq!(put["kind"], "book", "{put}");
    assert_eq!(put["cards"][0]["name"], "Ada", "{put}");

    let orbit = head.found(&project, "Booked");

    // Founding writes a store and a catalog row. The daemon's occupancy
    // slot stays empty — `BookResolve` peeks that slot. `unavailable` is
    // the vacant answer; a resolve that placed would return a snapshot
    // (`coverage` absent) of a live Orbit.
    let (status, resolved) = head.host(serde_json::json!({
        "cmd": "book_resolve",
        "orbit": orbit,
        "handles": [],
    }));
    assert_eq!(status, 200, "book_resolve: {resolved}");
    assert_eq!(resolved["kind"], "book_resolution", "{resolved}");
    assert_eq!(resolved["coverage"], "unavailable", "{resolved}");
    assert_eq!(
        resolved["hits"].as_array().map(Vec::len),
        Some(0),
        "{resolved}"
    );

    // Place, so the Space route has a StationHost to refuse — and so a
    // second resolve can show the other coverage: a snapshot, not absent.
    let (status, info) = head.space(&orbit, serde_json::json!({ "cmd": "status" }));
    assert_eq!(status, 200, "status: {info}");

    let (status, live) = head.host(serde_json::json!({
        "cmd": "book_resolve",
        "orbit": orbit,
        "handles": [],
    }));
    assert_eq!(status, 200, "book_resolve after place: {live}");
    assert_eq!(live["kind"], "book_resolution", "{live}");
    assert!(
        live["coverage"].is_null(),
        "a placed Orbit is a snapshot, not unavailable: {live}"
    );

    let (status, refused) = head.space(&orbit, serde_json::json!({ "cmd": "book_list" }));
    assert_eq!(status, 200, "space book_list: {refused}");
    assert_eq!(refused["kind"], "error", "{refused}");
    let message = refused["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("identity-scoped"),
        "StationHost must name the scope, not a generic refusal: {refused}"
    );

    let (status, listed) = head.host(serde_json::json!({ "cmd": "book_list" }));
    assert_eq!(status, 200, "book_list still: {listed}");
    assert_eq!(listed["cards"][0]["name"], "Ada", "{listed}");

    head.stop();
    std::fs::remove_dir_all(&root).ok();
}

fn suggestions_named(response: &Response, name: &str) -> usize {
    match response {
        Response::Book(view) => view.suggestions.iter().filter(|s| s.name == name).count(),
        other => panic!("expected a book, got {other:?}"),
    }
}

fn suggestion_id(response: &Response, name: &str) -> String {
    match response {
        Response::Book(view) => view
            .suggestions
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.suggestion.clone())
            .unwrap_or_else(|| panic!("no suggestion named {name}")),
        other => panic!("expected a book, got {other:?}"),
    }
}

/// A proposed bundle stages suggestions; nothing enters the book until each
/// one is reviewed. Accept mints the Card and retires the suggestion;
/// dismiss retires it and touches nothing.
#[test]
fn a_bundle_stages_suggestions_and_review_is_the_only_way_in() {
    let home = isolated_home("bookp");
    std::fs::create_dir_all(&home).expect("home");

    let mut child = spawn_daemon(&home);
    let client = Client::at(home.join("daemon"));
    wait_healthy(&client, Duration::from_secs(20));

    let bundle =
        r#"{"version":1,"cards":[{"name":"Grace","note":"met at the works"},{"name":"Edsger"}]}"#;
    let staged = ask(
        &client,
        Request::BookPropose {
            bundle: bundle.to_owned(),
        },
    );
    assert_eq!(suggestions_named(&staged, "Grace"), 1, "{staged:?}");
    assert_eq!(suggestions_named(&staged, "Edsger"), 1);
    assert_eq!(
        cards_named(&staged, "Grace"),
        0,
        "a proposal must not touch the book"
    );

    // Proposing the same file twice stages nothing new.
    let again = ask(
        &client,
        Request::BookPropose {
            bundle: bundle.to_owned(),
        },
    );
    assert_eq!(suggestions_named(&again, "Grace"), 1, "{again:?}");

    let grace = suggestion_id(&staged, "Grace");
    let accepted = ask(&client, Request::BookSuggestAccept { suggestion: grace });
    assert_eq!(cards_named(&accepted, "Grace"), 1, "{accepted:?}");
    assert_eq!(
        suggestions_named(&accepted, "Grace"),
        0,
        "an accepted suggestion retires"
    );

    let edsger = suggestion_id(&accepted, "Edsger");
    let dismissed = ask(&client, Request::BookSuggestDismiss { suggestion: edsger });
    assert_eq!(cards_named(&dismissed, "Edsger"), 0, "{dismissed:?}");
    assert_eq!(suggestions_named(&dismissed, "Edsger"), 0);

    stop_daemon_at(&home, &mut child);
}
