//! The classification: every control command, placed.
//!
//! Commands are classified by their wire tag (the `cmd` serde tag of the root
//! crate's `control::Request`) because that enum is native-only and this
//! crate must compile for the browser. The root crate's
//! `browser_control_vocabulary` test pins this table against
//! `control::representative_requests()` in both directions, so neither a new
//! wire command nor a phantom entry here survives an ordinary `cargo test`.
//!
//! The placement rule, applied throughout: **a station-scoped reading of the
//! Space or of the backend's own serving state is answerable in principle**
//! (`Answered` once it is, `NotYet` until then); **an act or reading that is
//! the daemon's by nature** — the identity home, the orbit registry, the
//! address book, correspondence, custody ceremonies, display coordination,
//! privileged authority writes, process execution, daemon diagnostics —
//! **is `DaemonOnly`**, and no later slice changes that by implementing
//! harder.

/// How the browser backend disposes of one control command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Answered from the pulled ledger and the composed Session.
    Answered,
    /// The daemon's act by nature; refused with words that say so.
    DaemonOnly,
    /// A station-scoped reading this backend could take but does not yet;
    /// refused with words that promise nothing.
    NotYet,
}

/// Every control command's placement. One entry per wire `cmd` tag.
pub const CLASSIFIED: &[(&str, Disposition)] = &[
    ("agent_add", Disposition::DaemonOnly),
    ("agent_provision", Disposition::DaemonOnly),
    ("assignment_grant", Disposition::DaemonOnly),
    ("assignment_list", Disposition::NotYet),
    ("assignment_revoke", Disposition::DaemonOnly),
    ("book_claim_self", Disposition::DaemonOnly),
    ("book_delete", Disposition::DaemonOnly),
    ("book_get", Disposition::DaemonOnly),
    ("book_link", Disposition::DaemonOnly),
    ("book_list", Disposition::DaemonOnly),
    ("book_lookup", Disposition::DaemonOnly),
    ("book_merge", Disposition::DaemonOnly),
    ("book_migrate", Disposition::DaemonOnly),
    ("book_migrate_status", Disposition::DaemonOnly),
    ("book_propose", Disposition::DaemonOnly),
    ("book_put", Disposition::DaemonOnly),
    ("book_resolve", Disposition::DaemonOnly),
    ("book_set_picture", Disposition::DaemonOnly),
    ("book_suggest_accept", Disposition::DaemonOnly),
    ("book_suggest_dismiss", Disposition::DaemonOnly),
    ("book_unlink", Disposition::DaemonOnly),
    ("config_reload", Disposition::DaemonOnly),
    ("connect", Disposition::DaemonOnly),
    ("coordinates", Disposition::DaemonOnly),
    ("correspond_block", Disposition::DaemonOnly),
    ("correspond_collect", Disposition::DaemonOnly),
    ("correspond_invite", Disposition::DaemonOnly),
    ("correspond_send", Disposition::DaemonOnly),
    ("device_add", Disposition::DaemonOnly),
    ("device_invite", Disposition::DaemonOnly),
    ("device_list", Disposition::NotYet),
    ("device_pair_confirm", Disposition::DaemonOnly),
    ("device_pair_enter", Disposition::DaemonOnly),
    ("device_retire", Disposition::DaemonOnly),
    ("device_revoke", Disposition::DaemonOnly),
    ("diagnose", Disposition::DaemonOnly),
    ("display_assignment_put", Disposition::DaemonOnly),
    ("display_assignment_revoke", Disposition::DaemonOnly),
    ("display_device_revoke", Disposition::DaemonOnly),
    (
        "display_identifier_admit_passphrase",
        Disposition::DaemonOnly,
    ),
    ("display_pairing_approve", Disposition::DaemonOnly),
    ("display_pairing_reject", Disposition::DaemonOnly),
    ("display_present", Disposition::DaemonOnly),
    ("display_rendezvous_mint", Disposition::DaemonOnly),
    ("display_rendezvous_revoke", Disposition::DaemonOnly),
    ("display_status", Disposition::DaemonOnly),
    ("display_surface_choices", Disposition::DaemonOnly),
    ("display_world_receivers", Disposition::DaemonOnly),
    ("find", Disposition::NotYet),
    ("hello", Disposition::DaemonOnly),
    ("host_config_get", Disposition::DaemonOnly),
    ("host_config_list", Disposition::DaemonOnly),
    ("host_config_set", Disposition::DaemonOnly),
    ("host_config_unset", Disposition::DaemonOnly),
    ("host_context", Disposition::DaemonOnly),
    ("host_device_consent", Disposition::DaemonOnly),
    ("host_install_mcp", Disposition::DaemonOnly),
    ("host_orbit_forget", Disposition::DaemonOnly),
    ("host_orbit_prune", Disposition::DaemonOnly),
    ("host_orbit_rebuild", Disposition::DaemonOnly),
    ("host_replica_exclude", Disposition::DaemonOnly),
    ("host_restart", Disposition::DaemonOnly),
    ("host_space_enter", Disposition::DaemonOnly),
    ("host_space_found", Disposition::DaemonOnly),
    ("host_update", Disposition::DaemonOnly),
    ("host_world_update", Disposition::DaemonOnly),
    ("host_world_update_status", Disposition::DaemonOnly),
    ("id", Disposition::NotYet),
    ("invite", Disposition::DaemonOnly),
    ("invite_revoke", Disposition::DaemonOnly),
    ("join", Disposition::DaemonOnly),
    ("key_rotate", Disposition::DaemonOnly),
    ("live", Disposition::NotYet),
    ("live_subscribe", Disposition::NotYet),
    ("log", Disposition::NotYet),
    ("member_add", Disposition::DaemonOnly),
    ("member_log", Disposition::NotYet),
    ("member_remove", Disposition::DaemonOnly),
    ("member_set_role", Disposition::DaemonOnly),
    ("members", Disposition::Answered),
    ("reach_learn", Disposition::DaemonOnly),
    ("reach_resolve", Disposition::DaemonOnly),
    ("reach_share", Disposition::DaemonOnly),
    ("reach_view", Disposition::DaemonOnly),
    ("recover", Disposition::DaemonOnly),
    ("seed_add", Disposition::DaemonOnly),
    ("seed_list", Disposition::DaemonOnly),
    ("seed_remove", Disposition::DaemonOnly),
    ("signals", Disposition::NotYet),
    ("space_custody_export", Disposition::DaemonOnly),
    ("space_custody_import", Disposition::DaemonOnly),
    ("space_elevate", Disposition::DaemonOnly),
    ("space_elevate_approve", Disposition::DaemonOnly),
    ("space_recover", Disposition::DaemonOnly),
    ("space_recover_approve", Disposition::DaemonOnly),
    ("space_reshare", Disposition::DaemonOnly),
    ("sponsor_watch", Disposition::DaemonOnly),
    ("status", Disposition::NotYet),
    ("stop", Disposition::DaemonOnly),
    ("storage", Disposition::NotYet),
    // A stream, not a one-shot — the native head refuses it on its route too;
    // the events lane is the door on every backend.
    ("subscribe", Disposition::DaemonOnly),
    ("sync", Disposition::NotYet),
    ("watching", Disposition::NotYet),
    ("who", Disposition::NotYet),
    ("whoami", Disposition::Answered),
    ("work", Disposition::DaemonOnly),
    ("world_activate", Disposition::DaemonOnly),
    ("worlds_active", Disposition::NotYet),
];

/// Place one command. `None` means the command is not in the vocabulary at
/// all — which the root crate's pinning test makes impossible for any command
/// the wire actually carries, so a live `None` is a frame this backend never
/// promised to understand.
pub fn disposition(cmd: &str) -> Option<Disposition> {
    CLASSIFIED
        .binary_search_by(|(name, _)| (*name).cmp(cmd))
        .ok()
        .map(|index| CLASSIFIED[index].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_nothing_is_placed_twice() {
        // Sorted is what lets `disposition` binary-search; unique is what
        // makes a placement a fact rather than the winner of an ordering.
        for pair in CLASSIFIED.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "{:?} does not sort before {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    #[test]
    fn every_disposition_resolves() {
        for (name, placed) in CLASSIFIED {
            assert_eq!(disposition(name), Some(*placed), "{name}");
        }
        assert_eq!(disposition("no_such_cmd"), None);
    }
}
