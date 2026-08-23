import SwiftUI

@main
struct AstrolabeApp: App {
    var body: some Scene {
        WindowGroup {
            RootView()
        }
    }
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
    @State private var arrivedInvite: PresentedLink?
    /// The bar's five surfaces, left to right. Spaces is home.
    @State private var selection = Surface.spaces

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
            SpacesView(view: view, onRefresh: refresh)
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
