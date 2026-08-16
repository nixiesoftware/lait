/// The only file that may call the generated bridge.
///
/// One model, two shells: the Rust core owns client state, and everything the
/// interface renders arrives as one whole `IosView` through this file — the
/// Swift analogue of `client.dart`'s rule on desktop, greppable the same way:
/// no other Swift file invokes a generated function. Generated *types* flow
/// outward freely, exactly as `client.dart` re-exports the view classes.
enum Core {
    /// The whole view. Surfaces render it and nothing else.
    static func view() -> IosView {
        clientView()
    }

    /// Bring the in-process node up. Blocks; call off the main thread.
    static func startNode() -> NodeStart {
        nodeStart()
    }

    /// The background transition: the head steps down before suspension
    /// reclaims its listener. Blocks briefly on the drain; call off the main
    /// thread, under a background-task assertion.
    static func background() {
        nodeBackground()
    }

    /// The foreground transition: the head serving again after a wake, with
    /// a fresh announcement every open tab must re-authenticate against.
    /// Blocks; call off the main thread.
    static func foreground() -> NodeStart {
        nodeForeground()
    }

    /// Parse and verify an invite without creating anything.
    static func ticket(_ link: String) -> TicketRead {
        readTicket(link: link)
    }

    /// Join a Space from an invite. Blocks up to the admission deadline;
    /// call off the main thread.
    static func enter(_ link: String) -> EnterOutcome {
        enterSpace(link: link, nick: nil)
    }

    /// Mint a single-use invite for a joined Space. Places the Orbit.
    static func invite(spacePath: String) -> InviteOutcome {
        mintInvite(spacePath: spacePath)
    }

    /// Converge one Space now; the report is the diagnostic, verbatim.
    static func sync(spacePath: String) -> SyncOutcome {
        syncSpace(spacePath: spacePath)
    }
}
