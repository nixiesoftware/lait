//! What a request may do, and to whose space.
//!
//! The local app's HTTP head exposes both installed World packages and root
//! control. There is no second, more privileged surface behind it — the head is
//! how this identity acts — so this module classifies the root-control half:
//! what only reads, and what belongs to the daemon-scoped host plane.

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
        | Request::DisplayStatus
        | Request::MemberLog
        | Request::DeviceInvite
        | Request::DeviceList
        // Which Worlds this Orbit activated. A read of the ACL's own record,
        // and one a Library needs before a person has done anything at all.
        | Request::WorldsActive
        // What the store is holding. A read in the strictest sense: it counts
        // Bodies already in memory and stats files already on disk, and the
        // one thing it could have written — a verification timestamp — is
        // recorded by the placement that verified, not by the asking.
        | Request::Storage
        | Request::Status
        | Request::Diagnose { .. }
        | Request::Id
        | Request::Whoami
        | Request::SeedList
        | Request::Log { .. }
        | Request::Who
        | Request::SponsorWatch { .. }
        | Request::Live { .. }
        | Request::LiveSubscribe { .. }
        | Request::AssignmentList { .. }
        | Request::Find { .. }
        // Host-plane reads: node-local settings and orientation. They sign
        // nothing and change nothing, so they carry the same weight through a
        // browser as through a terminal. A settings read still names a store
        // layer, and is admitted against this daemon's catalog like the write
        // beside it — being a read is not a licence to name a directory.
        | Request::HostConfigList { .. }
        | Request::HostConfigGet { .. }
        | Request::HostWorldUpdateStatus { .. }
        | Request::HostContext
        | Request::Hello { .. }
        | Request::BookList
        | Request::BookGet { .. }
        | Request::BookLookup { .. }
        | Request::BookResolve { .. }
        | Request::BookMigrateStatus => true,

        Request::Work { request, .. } if !request.is_command() => true,

        // Rendering a surface for this machine's own screen commits nothing:
        // no assignment, no receiver, no stored bytes. Its invocation is
        // classified `Query` at the client boundary *and* independently by the
        // trusted runtime before World code runs, which is a stronger guarantee
        // than this allowlist could make on its own.
        Request::DisplayPresent { .. } => true,

        Request::AgentAdd { .. }
        | Request::DisplayPairingApprove { .. }
        | Request::DisplayPairingReject { .. }
        | Request::DisplayAssignmentPut { .. }
        | Request::DisplayAssignmentRevoke { .. }
        | Request::DisplayDeviceRevoke { .. }
        | Request::AgentProvision { .. }
        | Request::MemberAdd { .. }
        | Request::MemberRemove { .. }
        | Request::MemberSetRole { .. }
        | Request::KeyRotate
        | Request::InviteRevoke { .. }
        | Request::DeviceAdd { .. }
        | Request::DeviceRevoke { .. }
        // The recovery-authority ceremonies. Classified, routable, and
        // deliberately **not on any head yet**: each is a multi-party threshold
        // exchange (mint a request, collect approvals from a quorum of
        // holders, then apply), and a one-shot button per step is how a half-run
        // ceremony leaves a Space with an authority nobody can complete. They
        // wait on a flow design, not on a route. Device enrolment and custody
        // export/import — the parts a single operator finishes alone — are on
        // the local app, which is why they are not in this note.
        | Request::Recover
        | Request::SpaceRecover
        | Request::SpaceElevate { .. }
        | Request::SpaceRecoverApprove { .. }
        | Request::SpaceElevateApprove { .. }
        | Request::SpaceReshare { .. }
        // …and custody, which handles a holder's own key material and a
        // passphrase…
        | Request::SpaceCustodyExport { .. }
        | Request::SpaceCustodyImport { .. }
        // …joining and inviting, which act *as* an identity on the wire…
        | Request::Invite { .. }
        | Request::Join { .. }
        | Request::Connect { .. }
        // …sync drives convergence on the wire (like connect), not a read…
        | Request::Sync
        // …declaring what you are looking at *publishes* — carets, a typing
        // flag, presence and the uncommitted text of a preview go into the
        // Space as whoever signs for that Station. Looking is a read; saying
        // "I am here, and this is what I am typing" is not, and on an identity
        // this daemon merely hosts it is a claim made in somebody else's name.
        // The browser's own presence goes through `GET /api/session`, which
        // asks custody before it declares (`serve::socket`)…
        | Request::Watching { .. }
        | Request::SeedAdd { .. }
        | Request::SeedRemove { .. }
        | Request::AssignmentGrant { .. }
        | Request::AssignmentRevoke { .. }
        | Request::WorldActivate { .. }
        // Runtime owns this classification. Product heads independently
        // classify their app vocabulary before it reaches the typed seam.
        | Request::Work { .. }
        // …draining signals, which empties a queue somebody else is waiting to
        // act on — the signals are addressed to that identity, not to whoever
        // has its space open in a browser…
        | Request::Signals
        // …forming or entering a Space, which mints key material and writes a
        // store, and signing device consent, which signs as this identity…
        | Request::HostSpaceFound { .. }
        | Request::HostSpaceEnter { .. }
        | Request::HostDeviceConsent { .. }
        // …settings writes and registry edits, which change what every other
        // caller on this machine subsequently reads…
        | Request::HostConfigSet { .. }
        | Request::HostConfigUnset { .. }
        | Request::HostOrbitForget { .. }
        | Request::HostOrbitPrune
        | Request::HostOrbitRebuild { .. }
        // …writing an agent client's config file, swapping this node's own
        // binary, and stopping the daemon that swap has to outlive…
        | Request::HostInstallMcp { .. }
        | Request::HostUpdate
        | Request::HostWorldUpdate { .. }
        | Request::HostRestart
        // …and node control.
        | Request::ConfigReload
        | Request::Stop
        | Request::BookPut { .. }
        | Request::BookSetPicture { .. }
        | Request::BookDelete { .. }
        | Request::BookLink { .. }
        | Request::BookUnlink { .. }
        | Request::BookMerge { .. }
        | Request::BookClaimSelf { .. }
        | Request::BookMigrate
        | Request::BookPropose { .. }
        | Request::BookSuggestAccept { .. }
        | Request::BookSuggestDismiss { .. } => false,

        // Not a one-shot at all — see `serve::rpc`, which refuses it with a
        // pointer to the endpoint that streams (`GET /api/events`).
        Request::Subscribe { .. } => false,
    }
}

/// Whether `req` belongs to the daemon-scoped host plane.
///
/// The gate on `POST /api/host/rpc`. That route exists because formation has no
/// space id yet, not because the daemon route is a second door into everything
/// the daemon can do — `Stop` is daemon-scoped too, and a page that could send
/// it would be able to shut down the server it is talking to.
///
/// What passing this gate grants, stated plainly, because it is wider than the
/// Space routes and reading the list does not make that obvious. Founding into a
/// caller-named directory, signing device consent, writing an agent client's
/// config file and setting a global key are all here, and nothing narrower
/// stands behind them: this is the only surface that can do any of it, and its
/// loopback token stands for the whole identity. Where each request may point is
/// *not* uniform, and the split is the part a new variant has to be classified
/// against:
///
/// * **A caller-named directory, by design.** `HostSpaceFound` creates the
///   directory it is given and writes a store into it. It cannot address an
///   *existing* store that way — that is the whole situation it exists for — so
///   it is not admitted against the catalog, and a caller holding this token can
///   therefore create a directory anywhere this process can write.
/// * **An existing store, admitted first.** `HostConfigSet`/`HostConfigUnset`
///   and `HostConfigList`/`HostConfigGet` name a store layer, and
///   `HostOrbitRebuild` names an Orbit; every one of them goes through
///   `orbits::bootstrap::admit`, which refuses a path this daemon does not serve
///   and a store whose Station signs with a key this daemon merely hosts.
/// * **A caller-named directory, checked for custody.** `HostSpaceEnter` writes
///   `<home>/config.json` and then drives `Connect` from the Station that signs
///   for that home, and `HostSpaceFound` plants a store and a `config.json` in
///   whatever directory it is handed, so
///   `orbits::bootstrap::admit_formation_target` runs on both before either
///   writes. It asks only whose key the directory holds — not whether a Space is
///   in it yet, because a provisioned agent home holds its seed first and would
///   otherwise read as blank. A directory holding neither a Space nor a foreign
///   identity passes, which is what keeps founding into `spaces_root()` or any
///   blank browser-proposed path working.
/// * **A caller-named directory this daemon may not even own.**
///   `HostInstallMcp` writes an MCP client config, and its `dir` is an editor's
///   project directory, which need not hold a store — so it is not admitted and
///   must not become a way to *read*. Note what `dir` does **not** control:
///   under `scope: user` (and for Windsurf under either scope) it is ignored
///   entirely and a fixed file in the daemon user's home is rewritten —
///   `~/.claude.json`, `~/.cursor/mcp.json`,
///   `~/.codeium/windsurf/mcp_config.json`. `print` answers with the entry that
///   would be written, never with the contents of the file at that path.
/// * **No path at all.** Device consent mints and signs with this identity's own
///   seed; the registry, update, restart and context requests name no directory.
///
/// A `Host*` variant added later inherits nothing from this list. If it carries a
/// path that addresses an existing store, it goes through `admit` before it
/// touches the filesystem.
///
/// Exhaustive for the same reason [`is_read`] is: a variant added later must be
/// classified before it can reach a route, rather than inherit one by default.
pub fn is_host_plane(req: &Request) -> bool {
    match req {
        Request::HostSpaceFound { .. }
        | Request::HostSpaceEnter { .. }
        | Request::HostDeviceConsent { .. }
        | Request::HostConfigList { .. }
        | Request::HostConfigGet { .. }
        | Request::HostConfigSet { .. }
        | Request::HostConfigUnset { .. }
        | Request::HostOrbitForget { .. }
        | Request::HostOrbitPrune
        | Request::HostOrbitRebuild { .. }
        | Request::HostInstallMcp { .. }
        | Request::HostUpdate
        | Request::HostWorldUpdate { .. }
        | Request::HostWorldUpdateStatus { .. }
        // …and the restart that makes an update take effect. Admitting it here
        // and not `Stop` is the whole distinction: this one names the daemon
        // *under* the server, which survives to stand a fresh one up.
        | Request::HostRestart
        | Request::HostContext
        | Request::BookList
        | Request::BookGet { .. }
        | Request::BookPut { .. }
        | Request::BookSetPicture { .. }
        | Request::BookDelete { .. }
        | Request::BookLink { .. }
        | Request::BookUnlink { .. }
        | Request::BookMerge { .. }
        | Request::BookClaimSelf { .. }
        | Request::BookLookup { .. }
        | Request::BookResolve { .. }
        | Request::BookMigrateStatus
        | Request::BookMigrate
        | Request::BookPropose { .. }
        | Request::BookSuggestAccept { .. }
        | Request::BookSuggestDismiss { .. } => true,

        Request::MemberAdd { .. }
        // Display coordination is daemon-scoped but intentionally reserved for
        // the native Astrolabe controller, not the browser host-RPC surface.
        | Request::DisplayStatus
        | Request::DisplayPairingApprove { .. }
        | Request::DisplayPairingReject { .. }
        | Request::DisplayAssignmentPut { .. }
        | Request::DisplayAssignmentRevoke { .. }
        | Request::DisplayDeviceRevoke { .. }
        | Request::DisplayPresent { .. }
        | Request::MemberRemove { .. }
        | Request::MemberSetRole { .. }
        | Request::Members
        | Request::MemberLog
        // Orbit-routed, not host-routed: it reads one Space's activation record
        // and therefore needs a Space to read.
        | Request::WorldsActive
        | Request::AgentAdd { .. }
        | Request::AgentProvision { .. }
        | Request::KeyRotate
        | Request::InviteRevoke { .. }
        | Request::DeviceInvite
        | Request::DeviceAdd { .. }
        | Request::DeviceRevoke { .. }
        | Request::DeviceList
        | Request::SpaceRecover
        | Request::SpaceElevate { .. }
        | Request::SpaceRecoverApprove { .. }
        | Request::SpaceElevateApprove { .. }
        | Request::SpaceReshare { .. }
        | Request::SpaceCustodyExport { .. }
        | Request::SpaceCustodyImport { .. }
        | Request::Recover
        | Request::AssignmentList { .. }
        | Request::AssignmentGrant { .. }
        | Request::AssignmentRevoke { .. }
        | Request::WorldActivate { .. }
        | Request::Work { .. }
        | Request::Find { .. }
        | Request::Subscribe { .. }
        | Request::Status
        // Orbit-routed, not host-routed, for the same reason `WorldsActive` is:
        // it reads one Space's own store and therefore needs a Space to read.
        // A daemon-scoped total across every Orbit on the machine would be a
        // different question, and not one this answers.
        | Request::Storage
        | Request::Diagnose { .. }
        | Request::Id
        | Request::Whoami
        | Request::SponsorWatch { .. }
        | Request::Sync
        | Request::Invite { .. }
        | Request::Join { .. }
        | Request::Connect { .. }
        | Request::SeedAdd { .. }
        | Request::SeedList
        | Request::SeedRemove { .. }
        | Request::Log { .. }
        | Request::Who
        | Request::Watching { .. }
        | Request::Live { .. }
        | Request::LiveSubscribe { .. }
        | Request::Signals
        | Request::ConfigReload
        | Request::Stop
        | Request::Hello { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_route_takes_the_host_plane_and_nothing_else() {
        assert!(is_host_plane(&Request::HostContext));
        assert!(is_host_plane(&Request::HostOrbitPrune));
        // The one that matters: daemon-scoped, and not the host plane. A page
        // that could send it would be able to stop the server serving it.
        assert!(!is_host_plane(&Request::Stop));
        assert!(!is_host_plane(&Request::Status));
    }

    #[test]
    fn reads_are_reads() {
        assert!(is_read(&Request::Status));
        assert!(is_read(&Request::Members));
        assert!(is_read(&Request::AssignmentList { actor: None }));
    }

    #[test]
    fn runtime_work_classifies_its_typed_operation_not_its_transport_envelope() {
        let world = replica::body::WorldId::parse("com.example.work").unwrap();
        let run = runtime::exec::RunId::from_bytes([0; 16]);
        assert!(is_read(&Request::Work {
            request: runtime::exec::WorkRequest::Inspect {
                world: world.clone(),
                run,
            },
            operation: String::new(),
        }));
        assert!(!is_read(&Request::Work {
            request: runtime::exec::WorkRequest::Cancel { world, run },
            operation: String::new(),
        }));
    }

    /// Asking what a Space is storing reads one Space, so it takes the Space
    /// route and not the daemon-scoped one.
    #[test]
    fn a_storage_read_is_a_read_and_belongs_to_a_space_not_to_the_host_plane() {
        assert!(is_read(&Request::Storage));
        assert!(!is_host_plane(&Request::Storage));
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

    /// The book is identity-scoped: every Book verb rides the host plane, its
    /// mutations are writes, and none of it belongs to a Space route. Named
    /// here so a future read-only credential tier inherits a classified list
    /// rather than a guess.
    #[test]
    fn the_address_book_is_host_plane_and_its_mutations_are_writes() {
        assert!(is_host_plane(&Request::BookList));
        assert!(is_host_plane(&Request::BookResolve {
            orbit: String::new(),
            handles: Vec::new(),
        }));
        assert!(is_host_plane(&Request::BookMigrate));
        assert!(!is_read(&Request::BookPut {
            card: None,
            name: String::new(),
            note: None,
        }));
        assert!(!is_read(&Request::BookDelete {
            card: String::new(),
        }));
        assert!(!is_read(&Request::BookMerge {
            from: String::new(),
            into: String::new(),
        }));
        assert!(!is_read(&Request::BookMigrate));
        assert!(!is_read(&Request::BookPropose {
            bundle: String::new()
        }));
        assert!(!is_read(&Request::BookSuggestAccept {
            suggestion: String::new()
        }));
        assert!(is_host_plane(&Request::BookPropose {
            bundle: String::new()
        }));
        // BookGet and BookLookup stay reads; BookList is *labelled* a read but
        // runs demand-driven alias import today — kept honest on the issue.
        assert!(is_read(&Request::BookGet {
            card: String::new(),
        }));
        assert!(is_read(&Request::BookLookup {
            handle: String::new(),
        }));
    }
}
