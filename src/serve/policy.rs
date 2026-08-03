//! What a request may do, and to whose space.
//!
//! `lait serve` exposes both installed World packages and root control. The
//! browser is as privileged as the CLI for the same selected identity; this
//! module classifies only the remaining root-control half.

use crate::control::Request;

/// Whether `req` only reads.
///
/// An **allowlist**, and the direction matters. `Request` fields are add-only,
/// so a verb added after this was written must default to "not a read" — refused
/// for an identity that isn't ours — rather than quietly inherit permission. The
/// match is exhaustive rather than `_ => false` for the same reason: a new variant
/// should fail to *compile* until somebody classifies it, instead of picking a
/// side on its own.
///
pub fn is_read(req: &Request) -> bool {
    match req {
        Request::Members
        | Request::MemberLog
        | Request::DeviceInvite
        | Request::DeviceList
        | Request::Status
        | Request::Diagnose { .. }
        | Request::Id
        | Request::Whoami
        | Request::SeedList
        | Request::Log { .. }
        | Request::Who
        | Request::Live { .. }
        | Request::LiveSubscribe { .. }
        // Declaring what you are looking at is a read, even though peers see the
        // result. Being present is a consequence of looking, and a reader who
        // cannot write is still in the room — Linear shows your cursor in a
        // document you have no permission to edit, for the same reason.
        | Request::Watching { .. }
        | Request::AssignmentList { .. }
        | Request::Hello { .. } => true,

        Request::AgentAdd { .. }
        | Request::AgentProvision { .. }
        | Request::MemberAdd { .. }
        | Request::MemberRemove { .. }
        | Request::MemberSetRole { .. }
        | Request::MemberAlias { .. }
        | Request::KeyRotate
        | Request::InviteRevoke { .. }
        | Request::DeviceAdd { .. }
        | Request::DeviceRevoke { .. }
        | Request::Recover
        | Request::SpaceRecover
        | Request::SpaceElevate { .. }
        | Request::SpaceRecoverApprove { .. }
        | Request::SpaceElevateApprove { .. }
        | Request::SpaceReshare { .. }
        // …and custody, which handles a holder's own key material and a
        // passphrase, so it belongs to the operator at the machine and not to a
        // browser session…
        | Request::SpaceCustodyExport { .. }
        | Request::SpaceCustodyImport { .. }
        // …joining and inviting, which act *as* an identity on the wire…
        | Request::Invite { .. }
        | Request::Join { .. }
        | Request::Connect { .. }
        // …sync drives convergence on the wire (like connect), not a read…
        | Request::Sync
        | Request::SeedAdd { .. }
        | Request::SeedRemove { .. }
        | Request::AssignmentGrant { .. }
        | Request::AssignmentRevoke { .. }
        | Request::WorldActivate { .. }
        // …draining signals, which empties a queue somebody else is waiting to
        // act on — the signals are addressed to that identity, not to whoever
        // has its space open in a browser…
        | Request::Signals
        // …and node control.
        | Request::ConfigReload
        | Request::Stop => false,

        // Not a one-shot at all — see `serve::rpc`, which refuses it with a
        // pointer to the endpoint that streams (`GET /api/events`).
        Request::Subscribe { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_reads() {
        assert!(is_read(&Request::Status));
        assert!(is_read(&Request::Members));
        assert!(is_read(&Request::AssignmentList { actor: None }));
    }

    #[test]
    fn writes_are_not() {
        assert!(!is_read(&Request::KeyRotate));
        assert!(!is_read(&Request::Invite {
            role: None,
            reusable: false,
            ttl_hours: None,
        }));
    }
}
