//! Every integration test in this package, as one binary.
//!
//! Cargo compiles each loose `tests/*.rs` into its own executable, and each one
//! statically links the whole dependency graph — iroh, loro, frost, rustls. At
//! 41 files that was 41 links for one `cargo test`, and on a Windows
//! runner linking is most of the wall clock.
//!
//! A directory with a `main.rs` is a SINGLE target, so these are modules now.
//! Test isolation is unchanged: nextest runs every test in its own process
//! regardless of which binary it came from.
//!
//! Add a file here and declare it below; nothing else changes.

mod address_book;
mod agent_experience;
mod authority_history;
mod beacon_convergence;
mod commit_cost_baseline;
mod content_ipc;
mod control_classification;
mod control_plane;
mod control_pool;
mod daemon_reaping;
mod exec_contract;
mod find_contract;
mod frost_interop;
mod guided_join;
mod head;
mod host_plane;
mod issues_comment_anchor;
mod issues_history_contract;
mod issues_policy_designer;
mod issues_reference_perf;
mod lait_daemon;
mod launcher_safety;
mod live_control;
mod mcp_parity;
mod mixed_root_guard;
mod orbit_registry;
mod orbital_admission;
mod orbital_adoption;
mod orbital_boundaries;
mod orbital_catalog;
mod orbital_ceremonies;
mod orbital_clean_break;
mod orbital_concurrent_catalog;
mod orbital_join;
mod orbital_join_iroh;
mod orbital_product_parity;
mod orbital_router;
mod orbital_two_node;
mod product_features;
mod product_independence;
mod product_schema;
mod restart_reconnect;
mod seed_registry;
mod semantic_type_names;
mod signal_is_not_durable;
mod station_lifecycle;
mod viewer_parity;

/// Integration fixtures generally care about the product payload while the
/// operation protocol itself has dedicated parity/continuity coverage. Keep
/// those fixtures honest about the durable acknowledgement before exposing
/// the inner response they historically asserted.
fn accepted_issue_response(response: issues_app::IssuesResponse) -> issues_app::IssuesResponse {
    match response {
        issues_app::IssuesResponse::Operation { receipt, response } => {
            assert_eq!(receipt.phase, issues_app::OperationPhase::Accepted);
            assert_eq!(
                receipt.operation.len(),
                32,
                "Runtime RequestId must remain a 128-bit lowercase hex operation id"
            );
            assert!(
                receipt
                    .operation
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
                "operation id must be canonical lowercase hex"
            );
            *response
        }
        response => response,
    }
}
