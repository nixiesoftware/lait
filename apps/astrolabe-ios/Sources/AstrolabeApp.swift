import SwiftUI
import UIKit

@main
struct AstrolabeApp: App {
    var body: some Scene {
        WindowGroup {
            RootView()
        }
    }
}

/// One open World session. Identity is (space, world); everything else is
/// presentation. Transient — a session lives exactly as long as its cover,
/// and the World's tile is the way back. There is no session dock.
struct OpenTab: Equatable, Identifiable {
    let spaceId: String
    let spaceName: String
    let orbitId: String?
    let mount: String
    let worldName: String
    let accent: UInt32?

    var id: String { "\(spaceId)/\(mount)" }
}

/// A bare link with the identity sheet presentation needs. A wrapper rather
/// than a retroactive `Identifiable` on `String` — a global conformance on a
/// standard type is a collision waiting for a second declarer.
struct PresentedLink: Identifiable {
    let link: String
    var id: String { link }
}

/// The one context rule, shared by Home and the switcher so the chooser and
/// the resolver can never disagree: standing gates, presence only colors.
/// Exactly two states fail standing — waiting on admission, and store gone.
enum HomeStanding {
    /// A Space you can act in. Everything lights up.
    case active(SpaceRow)
    /// Spaces exist but this context is not actable — the waiting room,
    /// wearing the Space being waited on.
    case waiting(SpaceRow)
    /// Nothing known. Welcome territory.
    case none

    static func actable(_ space: SpaceRow) -> Bool {
        switch space.status {
        case .admissionPending, .storeMissing: false
        default: true
        }
    }
}

struct RootView: View {
    /// The one projection every surface draws. The initializer is
    /// `launchView`'s only caller: pre-start the read is cheap, and every
    /// later one goes through `Core.view()` off the main thread.
    @State private var view = Core.launchView()
    /// The in-process node's startup, reported honestly: nil while starting.
    @State private var nodeFailure: String?
    /// Navigation memory, not client state.
    @AppStorage("welcomed") private var welcomed = false
    /// The active Space — the context every surface follows. Navigation
    /// memory: the registry stays the truth about what exists.
    @AppStorage("activeSpace") private var activeSpaceId = ""
    /// The open World session — transient, gone when its cover goes.
    @State private var presented: OpenTab?
    @State private var arrivedInvite: PresentedLink?
    /// The bar's four surfaces, left to right. Home is home.
    @State private var selection = Surface.home
    @Environment(\.scenePhase) private var scenePhase
    /// Whether the head was stepped down for a suspension it must now undo.
    /// Launch also lands on `.active`, and that arrival must not race the
    /// startup already running in `.task`.
    @State private var headSteppedDown = false

    /// The four surfaces, in bar order.
    enum Surface: Hashable {
        case home, inbox, chats, you
    }

    /// Context resolution, one rule: the remembered Space keeps the seat even
    /// unactable — silently reseating someone is the account-switcher sin —
    /// else the first actable Space, else the wait, else nothing.
    private var standing: HomeStanding {
        if let remembered = view.spaces.first(where: { $0.spaceId == activeSpaceId }) {
            return HomeStanding.actable(remembered) ? .active(remembered) : .waiting(remembered)
        }
        if let first = view.spaces.first(where: HomeStanding.actable(_:)) {
            return .active(first)
        }
        if let pending = view.spaces.first {
            return .waiting(pending)
        }
        return .none
    }

    var body: some View {
        TabView(selection: $selection) {
            HomeView(
                view: view,
                standing: standing,
                onRefresh: refresh,
                onOpen: open,
                onSelectSpace: { space in activeSpaceId = space.spaceId },
                onFounded: { spaceId in
                    activeSpaceId = spaceId
                    welcomed = true
                    refresh()
                }
            )
            .tabItem { Label("Home", systemImage: "house") }
            .tag(Surface.home)
            InboxView()
                .tabItem { Label("Inbox", systemImage: "tray") }
                .tag(Surface.inbox)
            ChatsView()
                .tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
                .tag(Surface.chats)
            YouView(view: view)
                .tabItem { Label("You", systemImage: "person.crop.circle") }
                .tag(Surface.you)
        }
        .tint(Theme.cobalt)
        // The visual language is a committed light look; until a dark variant
        // is designed (not inverted), the app pins light so no sheet ever
        // renders ink-on-dark.
        .preferredColorScheme(.light)
        .task {
            switch await Core.start() {
            case .ready:
                await refreshNow()
            case .failed(let reason):
                nodeFailure = reason
            }
            // The view is a projection of moving state — admission landing,
            // stores converging. A slow steady re-read keeps the rows honest
            // without any surface owning a refresh button.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(3))
                await refreshNow()
            }
        }
        .fullScreenCover(item: $presented) { tab in
            WorldTabView(tab: tab, head: view.head)
        }
        .onChange(of: scenePhase) { _, phase in
            // The platform's own guidance for an in-process listener: close
            // before suspension reclaims it, reopen on return. Each is a
            // deliberate transition, not recovery from a mystery.
            switch phase {
            case .background: stepDown()
            case .active: standBackUp()
            default: break
            }
        }
        .onOpenURL { url in
            // An invite arrived as a link. The sheet still reads and confirms
            // it — a tapped link is a delivery, never a consent.
            arrivedInvite = PresentedLink(link: url.absoluteString)
        }
        .sheet(item: $arrivedInvite) { arrived in
            EnterSpaceView(onJoined: {
                arrivedInvite = nil
                welcomed = true
                refresh()
            }, initialLink: arrived.link)
        }
        .fullScreenCover(isPresented: welcomeShown) {
            WelcomeView(
                onDone: { welcomed = true; refresh() },
                onFounded: { spaceId in
                    activeSpaceId = spaceId
                    welcomed = true
                    refresh()
                }
            )
        }
        .alert("Astrolabe couldn't start", isPresented: nodeFailedShown) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(nodeFailure ?? "")
        }
    }

    /// The next projection, fetched off the main thread — it asks each
    /// Space's Station for membership — and applied on it.
    @MainActor
    private func refreshNow() async {
        view = await Core.view()
    }

    /// The synchronous entry the callbacks use.
    func refresh() {
        Task { await refreshNow() }
    }

    /// The background transition: the head drains under a background-task
    /// assertion, so the close finishes before the freeze instead of half of
    /// it being carried into suspension. Submitted synchronously — the
    /// transitions queue is what keeps a fast flip from applying this after
    /// the foreground that follows it.
    private func stepDown() {
        headSteppedDown = true
        let assertion = UIApplication.shared.beginBackgroundTask(withName: "astrolabe-head-drain")
        Core.background {
            UIApplication.shared.endBackgroundTask(assertion)
        }
    }

    /// The foreground transition: only after a step-down — launch also lands
    /// on `.active`, and startup is already running in `.task`. The restarted
    /// head is a new announcement, and `refresh()` is what hands every open
    /// tab the fresh token to re-authenticate with.
    private func standBackUp() {
        guard headSteppedDown else { return }
        headSteppedDown = false
        Core.foreground { outcome in
            switch outcome {
            case .ready:
                nodeFailure = nil
                refresh()
            case .failed(let reason):
                nodeFailure = reason
            }
        }
    }

    private func open(space: SpaceRow, world: BundledWorld) {
        presented = OpenTab(
            spaceId: space.spaceId,
            spaceName: space.name,
            orbitId: space.orbitId,
            mount: world.mount,
            worldName: world.name,
            accent: world.accent
        )
    }

    private var welcomeShown: Binding<Bool> {
        Binding(
            get: { !welcomed && view.spaces.isEmpty },
            set: { shown in if !shown { welcomed = true } }
        )
    }

    private var nodeFailedShown: Binding<Bool> {
        Binding(get: { nodeFailure != nil }, set: { if !$0 { nodeFailure = nil } })
    }
}

/// The per-World accent, derived locally from the compiled-in 0xRRGGBB seed.
extension Color {
    init(accent packed: UInt32?) {
        guard let packed else {
            self = .secondary
            return
        }
        self.init(
            red: Double((packed >> 16) & 0xFF) / 255,
            green: Double((packed >> 8) & 0xFF) / 255,
            blue: Double(packed & 0xFF) / 255
        )
    }
}
