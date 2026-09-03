//! The session/editor lane, end to end in a tab: the Worker-side host answers
//! the exact frame vocabulary `viewer/src/workerSession.ts` speaks — colon
//! tags, camelCase `errorKind`, sid scoping, the operation envelope crossing
//! intact for the client-side unwrap — over the same composed engine the rpc
//! lane proved. The daemon's editor allowlist holds here too, so the session
//! lane cannot become a second, prompt-less RPC surface.
//!
//! Runs under `ci/browser-live-space.sh` (needs the relay + a founded Space).

#![cfg(all(target_arch = "wasm32", feature = "probe-dispatch", issues_runner_wasm))]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::handle::boot;

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const ISSUES_RUNNER: &[u8] = include_bytes!(env!("ISSUES_RUNNER_WASM"));

/// Walk a JSON value for the first string under a `reff` key — how the test
/// learns an issue id without pinning the list projection's shape.
fn first_reff(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(reff)) = map.get("reff") {
                return Some(reff.clone());
            }
            map.values().find_map(first_reff)
        }
        serde_json::Value::Array(items) => items.iter().find_map(first_reff),
        _ => None,
    }
}

#[wasm_bindgen_test]
async fn the_session_lane_answers_the_editor_frames_in_a_tab() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed_hex = option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX");
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");

    let handle = boot(
        relay.to_string(),
        seed_hex.to_string(),
        ticket.to_string(),
        ISSUES_RUNNER.to_vec(),
        "com.lait.issues".to_string(),
        "0.9.5".to_string(),
        "local".to_string(),
        "issues".to_string(),
    )
    .await
    .expect("the engine boots in a tab");

    // Open: exactly one liveness-live event frame, in the client's own wire
    // shape — the colon tag and the sid scope are load-bearing, the client
    // silently drops anything else.
    let opened = handle
        .handle_session(r#"{"lait":"session:open","sid":7}"#)
        .expect("open answers");
    assert!(
        opened.contains(r#""lait":"session:event""#)
            && opened.contains(r#""sid":7"#)
            && opened.contains(r#""liveness":"live""#),
        "open answers the liveness event the client waits on: {opened}"
    );

    // Watch: accepted and silent — a tab has no live plane, and the viewer
    // reads silence on a watched question as "unchanged".
    let watched = handle
        .handle_session(r#"{"lait":"session:watch","sid":7,"question":{"space":"s"}}"#)
        .expect("watch answers");
    assert_eq!(watched, "[]", "a watch owes no frames on this backend");

    // An issue to edit: learned over the rpc lane, the way the page would.
    let list = handle
        .handle_link(r#"{"lait":"rpc","id":1,"verb":"world","request":{"cmd":"list","page":{}}}"#)
        .expect("the list answers");
    let list_json: serde_json::Value = serde_json::from_str(&list).expect("the list decodes");
    let reff = first_reff(&list_json).expect("alice's issues carry a reff");

    // A read inside the allowlist crosses the session lane and resolves the
    // issue's body — the read issues a NESTED callback after its world call,
    // the path the callback-stack discipline in the runner exists for.
    let viewed = handle
        .handle_session(&format!(
            r#"{{"lait":"session:mutate","sid":7,"rid":1,"space":"s","request":{{"cmd":"issue_view","reff":"{reff}"}}}}"#
        ))
        .expect("issue_view answers");
    assert!(
        viewed.contains(r#""lait":"session:reply""#)
            && viewed.contains(r#""rid":1"#)
            && viewed.contains(r#""ok":true"#),
        "the editor read crosses the session lane: {viewed}"
    );

    // A successful editor WRITE crosses with the OPERATION ENVELOPE intact
    // (receipt and all) — the client-side unwrap has something to unwrap. A
    // checkpoint is the editor write with no document precondition: it records
    // one history entry, needing only that the issue exists — whose state its
    // apply reads through the same nested callback.
    let checkpoint = handle
        .handle_session(&format!(
            r#"{{"lait":"session:mutate","sid":7,"rid":2,"space":"s","request":{{"cmd":"issue_text_checkpoint","reff":"{reff}"}}}}"#
        ))
        .expect("the checkpoint answers");
    assert!(
        checkpoint.contains(r#""ok":true"#)
            && checkpoint.contains(r#""kind":"operation""#)
            && checkpoint.contains(r#""receipt""#),
        "the editor write lands with its envelope intact: {checkpoint}"
    );

    // The anti-clobber splice verb crosses the lane and the World adjudicates
    // it: a splice whose `base_len` disagrees with what the World holds is the
    // concurrent-edit fence, refused as a conflict — its diagnostic crossing as
    // clone-safe data proves the splice reaches the World through the session
    // lane (a successful splice needs an editable document; that path is
    // exercised in the World's own tests).
    let fenced = handle
        .handle_session(&format!(
            r#"{{"lait":"session:mutate","sid":7,"rid":3,"space":"s","request":{{"cmd":"issue_text_splice","reff":"{reff}","index":0,"delete":0,"insert":"x","base_len":999999}}}}"#
        ))
        .expect("the fenced splice answers");
    assert!(
        fenced.contains(r#""ok":false"#) && fenced.contains(r#""errorKind""#),
        "a base_len-fenced splice refuses clone-safe through the lane: {fenced}"
    );

    // The allowlist holds: a non-editor request refuses 403 in the clone-safe
    // error shape — camelCase errorKind, the exact field the client rehydrates
    // SocketMutationError from.
    let refused = handle
        .handle_session(
            r#"{"lait":"session:mutate","sid":7,"rid":4,"space":"s","request":{"cmd":"project_list","page":{}}}"#,
        )
        .expect("the refusal answers");
    assert!(
        refused.contains(r#""ok":false"#)
            && refused.contains(r#""status":403"#)
            && refused.contains("editor requests only")
            && refused.contains(r#""errorKind""#),
        "a non-editor request refuses clone-safe: {refused}"
    );

    // sid scoping: a mutate for a session never opened is dropped, never
    // thrown — the client's late-frame contract.
    let late = handle
        .handle_session(
            r#"{"lait":"session:mutate","sid":99,"rid":5,"space":"s","request":{"cmd":"issue_view","reff":"r"}}"#,
        )
        .expect("the late frame answers");
    assert_eq!(late, "[]", "an unknown sid's mutate is dropped");

    // Close, then the closed session's mutate is dropped too.
    let closed = handle
        .handle_session(r#"{"lait":"session:close","sid":7}"#)
        .expect("close answers");
    assert_eq!(closed, "[]");
    let after_close = handle
        .handle_session(&format!(
            r#"{{"lait":"session:mutate","sid":7,"rid":6,"space":"s","request":{{"cmd":"issue_view","reff":"{reff}"}}}}"#
        ))
        .expect("the after-close frame answers");
    assert_eq!(after_close, "[]", "a closed session's mutate is dropped");

    // Convergence reaches the lane: after a re-pull installs peer material,
    // a fresh session still reads the issue — the editor sees converged state.
    let _ = handle.repull().await.expect("the re-pull installs");
    handle
        .handle_session(r#"{"lait":"session:open","sid":8}"#)
        .expect("a fresh session opens");
    let again = handle
        .handle_session(&format!(
            r#"{{"lait":"session:mutate","sid":8,"rid":7,"space":"s","request":{{"cmd":"issue_view","reff":"{reff}"}}}}"#
        ))
        .expect("the view answers after convergence");
    assert!(
        again.contains(r#""ok":true"#),
        "the session lane reads converged state: {again}"
    );
}
