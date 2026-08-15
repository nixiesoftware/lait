import SwiftUI

@main
struct AstrolabeApp: App {
    var body: some Scene {
        WindowGroup {
            RootView()
        }
    }
}

/// One open World session. Identity is (space, world); everything else is
/// presentation. Persisted as navigation memory — restored stale, reconciled
/// when reopened against the live head.
struct OpenTab: Codable, Equatable, Identifiable {
    let spaceId: String
    let spaceName: String
    let orbitId: String?
    let mount: String
    let worldName: String
    let accent: UInt32?

    var id: String { "\(spaceId)/\(mount)" }
}

struct RootView: View {
    /// The one projection every surface draws.
    @State private var view = Core.view()
    /// The in-process node's startup, reported honestly: nil while starting.
    @State private var nodeFailure: String?
    /// Navigation memory, not client state.
    @AppStorage("welcomed") private var welcomed = false
    @AppStorage("openTabs") private var openTabsData = Data()
    @State private var tabs: [OpenTab] = []
    @State private var presented: OpenTab?
    @State private var arrivedInvite: String?
    @State private var selection = 0

    var body: some View {
        TabView(selection: $selection) {
            SpacesView(view: view, onRefresh: refresh, onOpen: open)
                .tabItem { Label("Spaces", systemImage: "circle.grid.2x2") }
                .tag(0)
            TabsView(view: view, tabs: tabs, onSelect: { presented = $0 }, onClose: close)
                .tabItem { Label("Tabs", systemImage: "square.on.square") }
                .tag(1)
            YouView(view: view)
                .tabItem { Label("You", systemImage: "person.crop.circle") }
                .tag(2)
        }
        .task {
            tabs = decodeTabs()
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
        .onOpenURL { url in
            // An invite arrived as a link. The sheet still reads and confirms
            // it — a tapped link is a delivery, never a consent.
            arrivedInvite = url.absoluteString
        }
        .sheet(item: $arrivedInvite) { link in
            EnterSpaceView(onJoined: {
                arrivedInvite = nil
                welcomed = true
                refresh()
            }, initialLink: link)
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

    private func open(space: SpaceRow, world: SpaceWorldRow) {
        let tab = OpenTab(
            spaceId: space.spaceId,
            spaceName: space.name,
            orbitId: space.orbitId,
            mount: world.mount,
            worldName: world.name,
            accent: world.accent
        )
        if !tabs.contains(tab) {
            tabs.append(tab)
            persistTabs()
        }
        presented = tab
    }

    private func close(_ tab: OpenTab) {
        tabs.removeAll { $0.id == tab.id }
        persistTabs()
    }

    private func decodeTabs() -> [OpenTab] {
        (try? JSONDecoder().decode([OpenTab].self, from: openTabsData)) ?? []
    }

    private func persistTabs() {
        openTabsData = (try? JSONEncoder().encode(tabs)) ?? Data()
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
