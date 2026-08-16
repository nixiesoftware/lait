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
/// and reopening from Spaces is the way back.
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

struct RootView: View {
    /// The one projection every surface draws.
    @State private var view = Core.view()
    /// The in-process node's startup, reported honestly: nil while starting.
    @State private var nodeFailure: String?
    /// Navigation memory, not client state.
    @AppStorage("welcomed") private var welcomed = false
    @State private var presented: OpenTab?
    @State private var arrivedInvite: PresentedLink?
    /// The bar's five surfaces, left to right. Spaces is home.
    @State private var selection = Surface.spaces
    @Environment(\.scenePhase) private var scenePhase
    /// Whether the head was stepped down for a suspension it must now undo.
    /// Launch also lands on `.active`, and that arrival must not race the
    /// startup already running in `.task`.
    @State private var headSteppedDown = false

    /// The five surfaces, in bar order.
    enum Surface: Hashable {
        case inbox, chats, spaces, library, you
    }

    var body: some View {
        TabView(selection: $selection) {
            InboxView()
                .tabItem { Label("Inbox", systemImage: "tray") }
                .tag(Surface.inbox)
            ChatsView()
                .tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
                .tag(Surface.chats)
            SpacesView(view: view, onRefresh: refresh, onOpen: open)
                .tabItem { Label("Spaces", systemImage: "circle.grid.2x2") }
                .tag(Surface.spaces)
            LibraryView(view: view)
                .tabItem { Label("Library", systemImage: "books.vertical") }
                .tag(Surface.library)
            YouView(view: view)
                .tabItem { Label("You", systemImage: "person.crop.circle") }
                .tag(Surface.you)
        }
        .task {
            let outcome = await Task.detached { Core.startNode() }.value
            switch outcome {
            case .ready:
                refresh()
            case .failed(let reason):
                nodeFailure = reason
            }
            // The view is a projection of moving state — admission landing,
            // stores converging. A slow steady re-read keeps the rows honest
            // without any surface owning a refresh button.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(3))
                refresh()
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
            WelcomeView(onDone: { welcomed = true; refresh() })
        }
        .alert("The node could not start", isPresented: nodeFailedShown) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(nodeFailure ?? "")
        }
    }

    func refresh() {
        view = Core.view()
    }

    /// The background transition: the head drains under a background-task
    /// assertion, so the close finishes before the freeze instead of half of
    /// it being carried into suspension.
    private func stepDown() {
        headSteppedDown = true
        let assertion = UIApplication.shared.beginBackgroundTask(withName: "astrolabe-head-drain")
        Task.detached {
            Core.background()
            await MainActor.run {
                UIApplication.shared.endBackgroundTask(assertion)
            }
        }
    }

    /// The foreground transition: only after a step-down — launch also lands
    /// on `.active`, and startup is already running in `.task`. The restarted
    /// head is a new announcement, and `refresh()` is what hands every open
    /// tab the fresh token to re-authenticate with.
    private func standBackUp() {
        guard headSteppedDown else { return }
        headSteppedDown = false
        Task {
            let outcome = await Task.detached { Core.foreground() }.value
            switch outcome {
            case .ready:
                nodeFailure = nil
                refresh()
            case .failed(let reason):
                nodeFailure = reason
            }
        }
    }

    private func open(space: SpaceRow, world: SpaceWorldRow) {
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
