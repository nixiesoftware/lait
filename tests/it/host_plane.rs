//! The host plane: the capabilities that run before — or without — an Orbit.
//!
//! Formation, device enrolment, node-local settings, and the catalog used to
//! run in whatever process typed the command, holding the store lock there.
//! They are daemon-scoped requests now (`Request::Host*`), reached through
//! `POST /api/host/rpc` — which is the only way to reach them, because the
//! command surface that used to carry them is gone.

use std::path::Path;

use crate::head::{canonical, daemon_home, temp_root, Head};

/// The cold start: nothing on disk but a config root, and a Space comes out.
///
/// This is the whole reason formation is daemon-scoped. `ControlRoute` has no
/// Orbit form to offer before a store exists, and the daemon is built from an
/// identity directory rather than from a store — so it is the one party that
/// exists early enough to host formation. The proof that it really went that
/// way is the daemon's own pid file: no other process opens the store, so a
/// Space could not have been formed without a daemon coming up first.
#[test]
fn a_fresh_identity_with_no_store_forms_a_space_and_is_then_reachable() {
    let root = temp_root("coldstart");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");

    assert!(
        !config.join("daemon").join("daemon.pid").exists(),
        "test premise: no daemon has ever run for this identity"
    );
    assert!(
        !project.join(".lait").exists(),
        "test premise: no store exists yet"
    );

    let head = Head::start(&config, None);
    let store = project.join(".lait");
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": store.display().to_string(),
        "name": "Cold",
        "nick": "t",
    }));
    assert_eq!(status, 200, "found: {founded}");
    assert_eq!(founded["host"], "founded", "{founded}");
    assert_eq!(founded["name"], "Cold", "{founded}");

    // The daemon formed it: its pid file exists, and the store it wrote is the
    // one the Orbit registry now names.
    assert!(
        config.join("daemon").join("daemon.pid").exists(),
        "formation must run in the identity's daemon"
    );
    assert!(
        lait::orbital::space_store_present(&store),
        "no Space store at {}",
        store.display()
    );

    // …and it is reachable: a Station places into the Orbit that was just
    // formed, and answers as a member of the Space it formed.
    let orbit = head.orbit_for(&store);
    let (status, info) = head.space(&orbit, serde_json::json!({ "cmd": "status" }));
    assert_eq!(status, 200, "status: {info}");
    assert_eq!(info["kind"], "status");
    assert_eq!(
        info["membership"], "admin",
        "the founder must hold ADMIN standing in the Space it just formed: {info}"
    );
    let (status, me) = head.space(&orbit, serde_json::json!({ "cmd": "whoami" }));
    assert_eq!(status, 200, "whoami: {me}");

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// Device enrolment is the one host request with no store anywhere.
///
/// A machine being enrolled holds no membership yet — that is the situation the
/// verb exists for — so it must work with an identity directory and nothing
/// else, and must not mint a store as a side effect of being asked.
#[test]
fn signing_device_consent_needs_no_store_anywhere() {
    let root = temp_root("consent");
    let config = root.join("config");
    let head = Head::start(&config, None);

    let actor = format!("act_{}", "ab".repeat(32));
    let space = format!("ws_{}", "c".repeat(26));
    let (status, signed) = head.host(serde_json::json!({
        "cmd": "host_device_consent",
        "token": format!("{actor} {space}"),
    }));
    assert_eq!(status, 200, "device consent: {signed}");
    let blob = signed["consent"]
        .as_str()
        .expect("a consent blob")
        .to_string();
    assert!(
        !blob.is_empty() && blob.chars().all(|c| c.is_ascii_hexdigit()),
        "consent must be hex, got: {blob}"
    );

    assert!(
        !lait::orbital::space_store_present(&config),
        "enrolment must not create a store under the identity"
    );

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// Settings round-trip, including the one outcome a surface has to tell apart:
/// a key that is set to nothing is a lookup failure, not an empty string.
#[test]
fn settings_round_trip_and_an_unset_key_is_a_lookup_failure() {
    let root = temp_root("config");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let head = Head::start(&config, None);
    head.found(&project, "Cfg");
    let home = canonical(&project.join(".lait"));

    let (status, written) = head.host(serde_json::json!({
        "cmd": "host_config_set",
        "key": "user.nick",
        "value": "moon",
        "global": false,
        "home": home.display().to_string(),
    }));
    assert_eq!(status, 200, "config set: {written}");
    assert_eq!(written["host"], "config_written", "{written}");

    // `get` answers with the value and nothing else — a key/origin/help sentence
    // in that position is a sentence in whatever the caller substitutes it into.
    let (status, got) = head.host(serde_json::json!({
        "cmd": "host_config_get",
        "key": "user.nick",
        "home": home.display().to_string(),
    }));
    assert_eq!(status, 200, "config get: {got}");
    assert_eq!(got, serde_json::json!({ "kind": "text", "text": "moon" }));

    let (status, listed) = head.host(serde_json::json!({
        "cmd": "host_config_list",
        "home": home.display().to_string(),
    }));
    assert_eq!(status, 200, "config list: {listed}");
    assert!(
        listed.to_string().contains("project.default"),
        "the whole key table must come back: {listed}"
    );

    let (_, unset) = head.host(serde_json::json!({
        "cmd": "host_config_unset",
        "key": "user.nick",
        "global": false,
        "home": home.display().to_string(),
    }));
    assert_eq!(unset["host"], "config_written", "{unset}");

    // `project.default` has no built-in, so it is unset at every layer.
    let (_, missing) = head.host(serde_json::json!({
        "cmd": "host_config_get",
        "key": "project.default",
        "home": home.display().to_string(),
    }));
    assert_eq!(
        missing["error_kind"], "not_found",
        "an unset key must resolve to nothing: {missing}"
    );

    // A read names a store, so it is admitted like the write beside it.
    // Unadmitted, `home` is any directory on the machine, and the answer is
    // whatever `<that>/config.json` says — reconnaissance handed to anything
    // holding the loopback token.
    let elsewhere = root.join("not-a-served-store");
    std::fs::create_dir_all(&elsewhere).expect("outside dir");
    std::fs::write(
        elsewhere.join("config.json"),
        r#"{"user":{"nick":"nobody"}}"#,
    )
    .expect("plant a config file");
    for cmd in ["host_config_get", "host_config_list"] {
        let (_, refused) = head.host(serde_json::json!({
            "cmd": cmd,
            "key": "user.nick",
            "home": elsewhere.display().to_string(),
        }));
        assert_eq!(
            refused["kind"], "error",
            "{cmd} must not read a directory this daemon does not serve: {refused}"
        );
    }

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// A store formed under a self-contained `$LAIT_HOME` is the same Orbit on the
/// next request.
///
/// A store's local Orbit id is a digest of the path it is registered under, and
/// every caller derives it from whatever spelling it was handed — so two
/// spellings of one directory are two Orbits. Formation registers the canonical
/// one; if the head kept a raw one, a self-contained catalog would drop the row
/// as belonging to another identity and the store just formed would answer "no
/// such local Orbit" to its own next request. A symlinked temp dir (macOS
/// resolves `/tmp` to `/private/tmp`) is how that happens without anyone trying.
#[test]
fn a_self_contained_home_is_the_same_orbit_on_the_next_request() {
    let root = temp_root("relhome");
    let config = root.join("config");
    let home = root.join("mydata");
    std::fs::create_dir_all(&home).expect("home dir");

    let head = Head::start(&config, Some(&home));
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": home.display().to_string(),
        "name": "Rel",
    }));
    assert_eq!(status, 200, "found: {founded}");
    assert!(
        lait::orbital::space_store_present(&home),
        "no Space store at {}",
        home.display()
    );

    let orbit = head.orbit_for(&home);
    let (status, info) = head.space(&orbit, serde_json::json!({ "cmd": "status" }));
    assert_eq!(
        status, 200,
        "the store this node just formed must still be addressable: {info}"
    );
    assert_eq!(
        info["membership"], "admin",
        "a self-contained home must reach the Orbit it formed: {info}"
    );

    head.stop();
    assert!(
        !daemon_home(&config, Some(&home))
            .join("control.sock")
            .exists()
            || cfg!(windows),
        "the self-contained daemon must have stopped with its head"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The catalog verbs deregister rows and never touch a store.
#[test]
fn forgetting_an_orbit_leaves_its_store_on_disk() {
    let root = temp_root("forget");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let head = Head::start(&config, None);
    head.found(&project, "Keep");
    let store = project.join(".lait");

    let (status, forgotten) = head.host(serde_json::json!({
        "cmd": "host_orbit_forget",
        "selector": store.display().to_string(),
    }));
    assert_eq!(status, 200, "forget: {forgotten}");
    assert_eq!(forgotten["host"], "forgotten", "{forgotten}");
    assert!(
        lait::orbital::space_store_present(&store),
        "forget must deregister, never delete"
    );

    // A selector that matches nothing resolves to nothing.
    let (_, nothing) = head.host(serde_json::json!({
        "cmd": "host_orbit_forget",
        "selector": "ws_nosuchspace",
    }));
    assert_eq!(nothing["error_kind"], "not_found", "{nothing}");

    // Pruning is a no-op now that the row is gone, and still succeeds.
    let (status, pruned) = head.host(serde_json::json!({ "cmd": "host_orbit_prune" }));
    assert_eq!(status, 200, "prune: {pruned}");
    assert_eq!(pruned["host"], "pruned", "{pruned}");

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// Rebuilding no longer races the daemon for the Orbit lock.
///
/// A generation build takes that lock exclusively. Run from a client — which is
/// where it used to run — it was competing with whatever the identity's daemon
/// had open, and "Orbit is active; stop the daemon before rebuilding" was the
/// routine answer to a routine request. Inside the daemon there is nobody left
/// to race: it releases its own placement first.
#[test]
fn rebuilding_does_not_lose_a_lock_race_with_a_live_daemon() {
    let root = temp_root("rebuild");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let head = Head::start(&config, None);
    let orbit = head.found(&project, "Gen");
    // Wake a Station so the daemon really is holding this Orbit open.
    let (status, _) = head.space(&orbit, serde_json::json!({ "cmd": "status" }));
    assert_eq!(status, 200);

    let (_, rebuilt) = head.host(serde_json::json!({
        "cmd": "host_orbit_rebuild",
        "orbit": orbit,
    }));
    // Only the race is asserted. Whether *this* store's journal can be rebuilt
    // is a Mechanics question with its own coverage; what this test owns is that
    // the answer is never "somebody else is holding the lock".
    assert!(
        !rebuilt.to_string().contains("Orbit is active"),
        "the lock race is supposed to be gone: {rebuilt}"
    );

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// The head answers the host plane, and only the host plane.
///
/// Every other `/api` route is `/api/spaces/{id}/…`, which is unanswerable at
/// the moment formation matters. The narrowing is the other half: the daemon
/// route also carries `stop`, and a page able to send that could shut down the
/// server answering it.
#[test]
fn the_host_route_answers_orientation_and_refuses_a_space_request() {
    let root = temp_root("hostroute");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let head = Head::start(&config, None);
    head.found(&project, "Served");

    let (status, reply) = head.host(serde_json::json!({ "cmd": "host_context" }));
    assert_eq!(status, 200, "host_context: {reply}");
    assert_eq!(reply["kind"], "host");
    assert_eq!(reply["host"], "context");
    assert!(
        reply["worlds"]
            .as_array()
            .is_some_and(|worlds| !worlds.is_empty()),
        "orientation must name the Worlds this build hosts: {reply}"
    );

    // A settings read is a host read, so it passes the same gate.
    let (status, body) = head.host(serde_json::json!({ "cmd": "host_config_list" }));
    assert_eq!(status, 200, "host_config_list: {body}");

    // …and a Space-scoped request does not: this endpoint is for the plane that
    // has no space id, not a second door into everything the daemon can do.
    let (status, body) = head.host(serde_json::json!({ "cmd": "stop" }));
    assert_eq!(status, 400, "stop must not be reachable here: {body}");
    assert!(body.to_string().contains("host-plane"), "got: {body}");

    // A settings write names the store layer it lands in, and that path comes
    // from the caller. The daemon admits it against its own catalog first, so
    // `config.json` cannot be written into a directory that is not a store this
    // node serves.
    let (status, reply) = head.host(serde_json::json!({
        "cmd": "host_config_set",
        "key": "user.nick",
        "value": "elsewhere",
        "global": false,
        "home": root.display().to_string(),
    }));
    assert_eq!(status, 200, "host_config_set: {reply}");
    assert_eq!(
        reply["kind"], "error",
        "a settings write outside a served store must be refused: {reply}"
    );
    assert!(
        !root.join("config.json").exists(),
        "the refusal must happen before the write"
    );

    // The credential gate is unchanged for the new route.
    let (status, _) = head.post_raw("/api/host/rpc", "wrong", r#"{"cmd":"host_context"}"#);
    assert_eq!(status, 401, "the host route must sit behind the same gate");

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// `install-mcp` is bootstrapping, so it is a host request too.
///
/// It has no head equivalent by construction: a head cannot write the file that
/// tells an agent how to reach it. The written payload is the deployed contract
/// (`.claude-plugin/plugin.json` declares the same shape), so the entry must
/// stay a bare `lait mcp` off PATH — a pinned absolute path goes stale the
/// moment the binary moves, and a captured `$LAIT_HOME` outlives the shell that
/// set it.
#[test]
fn installing_the_mcp_server_writes_a_portable_entry_into_the_directory_it_was_given() {
    let root = temp_root("installmcp");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let head = Head::start(&config, None);

    // `print` first: nothing on disk, and the contents come back for review.
    let (status, printed) = head.host(serde_json::json!({
        "cmd": "host_install_mcp",
        "client": "claude",
        "name": "lait",
        "print": true,
        "dir": project.display().to_string(),
    }));
    assert_eq!(status, 200, "print: {printed}");
    assert_eq!(printed["host"], "mcp_installed", "{printed}");
    assert!(
        !project.join(".mcp.json").exists(),
        "print must write nothing"
    );
    assert!(
        printed["note"]
            .as_str()
            .is_some_and(|note| note.contains("plugin")),
        "the shadowing caveat has to survive `print` — nobody is reading our \
         stderr: {printed}"
    );

    let (status, written) = head.host(serde_json::json!({
        "cmd": "host_install_mcp",
        "client": "claude",
        "name": "lait",
        "dir": project.display().to_string(),
    }));
    assert_eq!(status, 200, "install: {written}");
    assert_eq!(written["replaced"], false, "{written}");
    assert_eq!(
        written["agent"], "claude",
        "naming the client names the agent"
    );

    // The daemon's own working directory is not the caller's, so the file has to
    // land in the directory the request named.
    let path = Path::new(written["path"].as_str().expect("a path"));
    assert_eq!(path, project.join(".mcp.json"), "wrote {}", path.display());
    let config_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read config")).expect("JSON");
    let entry = &config_json["mcpServers"]["lait"];
    assert_eq!(entry["command"], "lait", "{entry}");
    assert_eq!(entry["args"], serde_json::json!(["mcp"]), "{entry}");
    assert_eq!(entry["env"]["LAIT_AGENT"], "claude", "{entry}");
    assert!(
        !entry.to_string().contains("LAIT_HOME"),
        "the entry must never capture a home: {entry}"
    );

    // A second write updates in place and says so.
    let (_, again) = head.host(serde_json::json!({
        "cmd": "host_install_mcp",
        "client": "claude",
        "name": "lait",
        "dir": project.display().to_string(),
    }));
    assert_eq!(again["replaced"], true, "{again}");

    // A dry run must not become a read. `dir` is caller-directed and
    // deliberately unadmitted — this verb aims at an editor's project, which
    // need not be a store this node serves — so `print` answers with the entry
    // it would write, never with what already sits at that path.
    let mut planted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read config")).expect("JSON");
    planted["authToken"] = serde_json::json!("not-yours");
    std::fs::write(path, planted.to_string()).expect("plant a secret");
    let (status, printed) = head.host(serde_json::json!({
        "cmd": "host_install_mcp",
        "client": "claude",
        "name": "lait",
        "print": true,
        "dir": project.display().to_string(),
    }));
    assert_eq!(status, 200, "print: {printed}");
    assert!(
        !printed.to_string().contains("not-yours"),
        "print must not return the file it would touch: {printed}"
    );

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// The destructive-confirm gate, on the surface that still has one.
///
/// `delete` takes its ref from the git branch when you omit it, so it is the one
/// verb that can tombstone something you never named — it must refuse rather
/// than guess. The terminal's prompt is gone; the browser's is a `409
/// confirm_required` carrying the question the package resolved, and the
/// question has to name what it would destroy, because "delete iss_3f9?" is
/// unanswerable if you don't recall which issue that is.
#[test]
fn deleting_an_issue_needs_confirmation_it_can_actually_ask_for() {
    let root = temp_root("confirm");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let head = Head::start(&config, None);
    let orbit = head.found(&project, "Del");

    let (status, filed) = head.world(
        &orbit,
        "issues",
        serde_json::json!({ "cmd": "issue_new", "title": "keep me" }),
        false,
    );
    assert_eq!(status, 200, "issue_new: {filed}");
    assert_eq!(filed["kind"], "operation", "issue_new: {filed}");
    assert_eq!(filed["receipt"]["phase"], "accepted", "issue_new: {filed}");
    let reff = filed["response"]["reff"]
        .as_str()
        .expect("a ref")
        .to_string();

    let (status, question) = head.world(
        &orbit,
        "issues",
        serde_json::json!({ "cmd": "issue_delete", "reff": reff }),
        false,
    );
    assert_eq!(
        status, 409,
        "an unconfirmed delete must be refused: {question}"
    );
    assert_eq!(question["kind"], "confirm_required", "{question}");
    assert!(
        question["question"]
            .as_str()
            .is_some_and(|q| q.contains("keep me")),
        "the question must name what it would destroy: {question}"
    );

    let (status, listed) = head.world(
        &orbit,
        "issues",
        serde_json::json!({ "cmd": "list", "page": { "limit": 100 } }),
        false,
    );
    assert_eq!(status, 200, "list: {listed}");
    assert!(
        listed.to_string().contains("keep me"),
        "the issue must survive an unconfirmed delete: {listed}"
    );

    // …and answering the question is the way through.
    let (status, deleted) = head.world(
        &orbit,
        "issues",
        serde_json::json!({ "cmd": "issue_delete", "reff": reff }),
        true,
    );
    assert_eq!(status, 200, "confirmed delete: {deleted}");
    let (_, listed) = head.world(
        &orbit,
        "issues",
        serde_json::json!({ "cmd": "list", "page": { "limit": 100 } }),
        false,
    );
    assert!(
        !listed.to_string().contains("keep me"),
        "a confirmed delete must actually delete: {listed}"
    );

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}

/// The daemon can be restarted, and the head it serves survives to stand a
/// fresh one up.
///
/// This closes the loop `host_update` opens. `self_update` replaces the
/// executable by renaming it out from under a live process, so the swap only
/// takes effect at the next start — and with no command surface left, nothing
/// could perform that start. `Request::Stop` is deliberately refused on this
/// route (a page must not be able to kill the server answering it), which is
/// exactly why the restart is its own verb: it names the daemon *under* the
/// server, not the server.
#[test]
fn restarting_the_daemon_leaves_the_head_able_to_stand_a_new_one_up() {
    let root = temp_root("hostrestart");
    let config = root.join("config");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let head = Head::start(&config, None);
    let orbit = head.found(&project, "Restarted");

    let (status, orientation) = head.host(serde_json::json!({ "cmd": "host_context" }));
    assert_eq!(status, 200, "host_context: {orientation}");
    assert!(
        orientation["version"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "a running node must say which build it is: {orientation}"
    );

    let (status, restarting) = head.host(serde_json::json!({ "cmd": "host_restart" }));
    assert_eq!(status, 200, "host_restart: {restarting}");
    assert_eq!(restarting["host"], "restarting", "got: {restarting}");

    // The reply is written before the daemon goes, so wait for it to be gone
    // rather than racing the shutdown with the next request.
    let daemon = daemon_home(&config, None);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        while !matches!(
            lait::control::probe(&daemon).await,
            lait::control::Probe::Absent
        ) {
            assert!(
                std::time::Instant::now() < deadline,
                "the daemon never stopped"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });

    // The next request finds nobody listening and starts one — which is the
    // whole point: a restarted daemon is a *served* daemon again, not an
    // outage the person has to notice.
    let (status, whoami) = head.space(&orbit, serde_json::json!({ "cmd": "whoami" }));
    assert_eq!(status, 200, "after the restart: {whoami}");

    head.stop();
    let _ = std::fs::remove_dir_all(&root);
}
