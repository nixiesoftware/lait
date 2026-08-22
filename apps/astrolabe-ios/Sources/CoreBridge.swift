import Foundation

/// The only file that may call the generated bridge.
///
/// One model, two shells: the Rust core owns client state, and everything the
/// interface renders arrives as one whole `IosView` through this file — the
/// Swift analogue of `client.dart`'s rule on desktop. UniFFI emits free
/// functions into the app's own module, so no import gate can hold the rule
/// the way Dart's does; `ci/swift-bridge-guard.sh` holds it instead. Generated
/// *types* flow outward freely, exactly as `client.dart` re-exports the view
/// classes.
///
/// This file also owns the threading contract, so no call site can get it
/// wrong. Every bridge call blocks — the node answers on its own runtime —
/// and two kinds of call must additionally keep their *order*:
///
/// - The lifecycle transitions. A background and a foreground racing on
///   detached tasks can apply in reverse on a fast flip, leaving an active
///   app whose head was paused after it resumed — which one layer up reads as
///   the false-disconnection defect. Their entries below are synchronous
///   *submissions* to one serial queue, deliberately not `async` functions:
///   an async body begins inside a task, and ordering would then hang on task
///   scheduling. A synchronous enqueue makes the serial queue itself the
///   order, and the order is the call order.
/// - The view fetches. `clientView()` is real work — the node asks each
///   Space's Station for membership — so it must leave the main thread, and
///   two fetches must not resolve out of order and publish the stale one
///   last. One serial queue, distinct from the transitions queue so a slow
///   fetch never holds a drain past the suspension deadline.
///
/// The long acts (enter, invite, sync) need neither ordering nor exclusivity,
/// only "not the main thread": each runs on its own detached task.
enum Core {
    /// Lifecycle transitions, strictly in submission order.
    private static let transitions = DispatchQueue(
        label: "com.nixiesoftware.astrolabe.node-transitions", qos: .userInitiated)
    /// View fetches, one at a time, so the newest fetched is the newest applied.
    private static let reads = DispatchQueue(
        label: "com.nixiesoftware.astrolabe.node-reads", qos: .userInitiated)

    /// Bring the in-process node up. The first transition; blocks up to ~30s
    /// on the queue while the shell renders a starting state.
    static func start() async -> NodeStart {
        await on(transitions) { nodeStart() }
    }

    /// The background transition: the head steps down before suspension
    /// reclaims its listener. `whenDrained` runs on the main actor once the
    /// drain finishes — the moment to end the background-task assertion.
    static func background(whenDrained: @escaping @MainActor () -> Void) {
        transitions.async {
            nodeBackground()
            Task { @MainActor in whenDrained() }
        }
    }

    /// The foreground transition: the head serving again after a wake, with a
    /// fresh announcement every open tab must re-authenticate against.
    /// `deliver` runs on the main actor with the outcome.
    static func foreground(deliver: @escaping @MainActor (NodeStart) -> Void) {
        transitions.async {
            let outcome = nodeForeground()
            Task { @MainActor in deliver(outcome) }
        }
    }

    /// The whole view. Surfaces render it and nothing else.
    static func view() async -> IosView {
        await on(reads) { clientView() }
    }

    /// The launch frame's view — the one deliberately synchronous read, for
    /// `RootView`'s state initializer and nothing else. Before the node
    /// starts there is no Station to ask, so this answers from the registry
    /// file alone and is cheap; once the node is up the same call does real
    /// work and must go through `view()`.
    static func launchView() -> IosView {
        clientView()
    }

    /// Parse and verify an invite without creating anything. In-memory only —
    /// the one bridge call that is fine on the main thread.
    static func ticket(_ link: String) -> TicketRead {
        readTicket(link: link)
    }

    /// Join a Space from an invite. Blocks up to the admission deadline.
    static func enter(_ link: String) async -> EnterOutcome {
        await Task.detached { enterSpace(link: link, nick: nil) }.value
    }

    /// Found a Space on this iPhone. Blocks while the store forms.
    static func found(name: String, nick: String?) async -> FoundOutcome {
        await Task.detached { foundSpace(name: name, nick: nick) }.value
    }

    /// Mint a single-use invite for a joined Space. Places the Orbit.
    static func invite(spacePath: String) async -> InviteOutcome {
        await Task.detached { mintInvite(spacePath: spacePath) }.value
    }

    /// Converge one Space now; the report is the diagnostic, verbatim.
    static func sync(spacePath: String) async -> SyncOutcome {
        await Task.detached { syncSpace(spacePath: spacePath) }.value
    }

    private static func on<T>(_ queue: DispatchQueue, _ work: @escaping () -> T) async -> T {
        await withCheckedContinuation { continuation in
            queue.async { continuation.resume(returning: work()) }
        }
    }
}
