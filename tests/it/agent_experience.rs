//! The Agent Experience, end to end (Architecture B): a user sponsors a
//! co-located agent **once**, then the agent acts as **itself** — its work
//! signed and attributed to its own identity, distinct from the human's, in
//! **one store** (one home, one daemon). No second store copy, no restart.
//!
//! This is the automated form of the recorded live acceptance, and it is the
//! single load-bearing proof that a sponsored member holds *write standing*.
//! Everything about agents is allowed to change except this: an agent is a
//! member, not a capability class, so nothing here may pass because the code
//! recognized the word "agent".
//!
//! It drives both real heads over their real protocols — the human through the
//! local app's HTTP surface, the agent through `lait mcp`, which is the one
//! head that can act as somebody other than the identity whose daemon it talks
//! to (`$LAIT_AGENT`). Between them they exercise provisioning (self-inception
//! + sponsorship into the shared store), the contributor grant, the `act_as`
//! selector, per-identity Session docking, and signed attribution.

use crate::head::{temp_root, Head, Mcp};

/// Clean-env entrypoint: a developer's shell `$LAIT_HOME` (their live node) must
/// never leak into a test that spawns a daemon for a temp home.
#[ctor::ctor]
fn scrub_ambient_lait_env() {
    for key in ["LAIT_HOME", "LAIT_STORE", "LAIT_CONFIG_ROOT"] {
        std::env::remove_var(key);
    }
}

#[test]
fn a_sponsored_agent_acts_as_itself_in_one_store() {
    let root = temp_root("axe");
    let config = root.join("cfg");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");

    // The human founds the space and files the first issue (which creates the
    // default project PROJ).
    let head = Head::start(&config, Some(&home));
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": home.display().to_string(),
        "name": "PROJ",
        "nick": "Huginn",
    }));
    assert_eq!(status, 200, "found: {founded}");
    let orbit = head.orbit_for(&home);

    let (status, filed) = head.world(
        &orbit,
        "issues",
        serde_json::json!({ "cmd": "issue_new", "title": "human-filed issue" }),
        false,
    );
    assert_eq!(status, 200, "the human files first: {filed}");

    let (status, human) = head.space(&orbit, serde_json::json!({ "cmd": "whoami" }));
    assert_eq!(status, 200, "whoami: {human}");
    let human_actor = human["actor"].as_str().expect("a human actor").to_string();
    let human_device = human["device"]
        .as_str()
        .expect("a human device")
        .to_string();

    // Sponsor a co-located agent in ONE step.
    let (status, provisioned) = head.space(
        &orbit,
        serde_json::json!({ "cmd": "agent_provision", "name": "scout" }),
    );
    assert_eq!(status, 200, "provision: {provisioned}");

    // whoami AS the agent, through the head an agent actually uses: a DISTINCT
    // identity, a member with write standing, the scoped contributor
    // capabilities, and a sponsor link.
    let mut agent = Mcp::start(&config, &home, Some("scout"));
    let me = agent.call("whoami", serde_json::json!({}));
    let agent_actor = me["actor"].as_str().expect("an agent actor").to_string();
    let agent_device = me["device"].as_str().expect("an agent device").to_string();
    assert_ne!(
        agent_actor, human_actor,
        "the agent must have its OWN actor"
    );
    assert_ne!(
        agent_device, human_device,
        "the agent must sign with its OWN device"
    );
    assert_eq!(
        me["can_write"],
        serde_json::json!(true),
        "a sponsored member writes"
    );
    assert!(
        me["did"]
            .as_str()
            .unwrap_or_default()
            .starts_with("did:key:z6Mk"),
        "the agent exposes a did:key: {}",
        me["did"]
    );
    assert_eq!(
        me["sponsor"].as_str(),
        Some(human_actor.as_str()),
        "the roster renders the sponsor relationship"
    );
    let caps: Vec<String> = me["capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .map(|c| c.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        caps.contains(&"space.issue.read".to_string())
            && caps.contains(&"space.contributor".to_string()),
        "a sponsored member gets the contributor scoped caps (read+write), got {caps:?}"
    );
    // But NEVER membership authority.
    assert!(
        !caps.contains(&"space.admin".to_string()),
        "an agent is not an admin"
    );

    // The agent files an issue AS ITSELF — proving it can both read the catalog
    // (scoped read) and author (write standing) in the one shared store.
    let filed = agent.call(
        "issues_new",
        serde_json::json!({ "title": "agent-filed issue", "project": "PROJ" }),
    );
    assert!(
        filed.to_string().to_uppercase().contains("PROJ-"),
        "the agent must be able to file an issue as itself: {filed}"
    );
    agent.stop();

    // The roster shows both members in the ONE store, the agent sponsored.
    let (status, members) = head.space(&orbit, serde_json::json!({ "cmd": "members" }));
    assert_eq!(status, 200, "members: {members}");
    let rows = members["members"].as_array().cloned().unwrap_or_default();
    let agent_row = rows
        .iter()
        .find(|m| m["key"].as_str() == Some(&agent_actor))
        .unwrap_or_else(|| panic!("the agent is a member in the same store: {members}"));
    assert_eq!(agent_row["sponsor"].as_str(), Some(human_actor.as_str()));
    assert!(rows
        .iter()
        .any(|m| m["key"].as_str() == Some(&human_actor) && m["role"] == "admin"));

    head.stop();
    std::fs::remove_dir_all(&root).ok();
}

/// Installing the MCP binding first is the common order. The agent's first
/// whoami must file a host-plane ask rather than only saying "go to Settings".
#[test]
fn an_unsponsored_agent_asks_the_person_to_sponsor_it() {
    let root = temp_root("ask");
    let config = root.join("cfg");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");

    let head = Head::start(&config, Some(&home));
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": home.display().to_string(),
        "name": "PROJ",
        "nick": "Huginn",
    }));
    assert_eq!(status, 200, "found: {founded}");
    let orbit = head.orbit_for(&home);

    let mut agent = Mcp::start(&config, &home, Some("scout"));
    let reply = agent.call_raw("whoami", serde_json::json!({}));
    assert_eq!(
        reply["result"]["isError"], true,
        "an unsponsored whoami must be a tool error, not a silent empty identity: {reply}"
    );
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("has been requested"),
        "the agent was not told an ask was filed: {text}"
    );

    let waiting = agent.call("wait", serde_json::json!({}));
    assert_eq!(waiting["wait"], "waiting", "{waiting}");
    let heads = waiting["heads"].as_array().cloned().unwrap_or_default();
    assert!(!heads.is_empty(), "{waiting}");
    let again = agent.call("wait", serde_json::json!({ "heads": heads }));
    assert_eq!(again["wait"], "unchanged", "{again}");

    let (status, ctx) = head.host(serde_json::json!({ "cmd": "host_context" }));
    assert_eq!(status, 200, "host_context: {ctx}");
    let asks = ctx["asks"].as_array().cloned().unwrap_or_default();
    assert_eq!(asks.len(), 1, "the ask did not reach the host plane: {ctx}");
    assert_eq!(asks[0]["name"], "scout", "{ctx}");

    let (status, provisioned) = head.space(
        &orbit,
        serde_json::json!({ "cmd": "agent_provision", "name": "scout" }),
    );
    assert_eq!(status, 200, "provision: {provisioned}");

    let granted = agent.call("wait", serde_json::json!({ "heads": heads }));
    assert_eq!(granted["wait"], "granted", "{granted}");

    let me = agent.call("whoami", serde_json::json!({}));
    assert_eq!(me["member"], true, "{me}");
    assert_ne!(me["sponsorship_asked"], true, "{me}");

    let (status, ctx) = head.host(serde_json::json!({ "cmd": "host_context" }));
    assert_eq!(status, 200, "host_context after: {ctx}");
    let leftover = ctx["asks"].as_array().cloned().unwrap_or_default();
    assert!(leftover.is_empty(), "the ask survived approval: {ctx}");

    agent.stop();
    head.stop();
    std::fs::remove_dir_all(&root).ok();
}
