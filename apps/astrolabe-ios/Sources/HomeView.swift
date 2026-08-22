import SwiftUI

/// Home: the Space is the context, worn by the band — never a folder in a
/// list. Worlds are launcher tiles carrying their own marks; the grid IS the
/// install list. The verbs are the client's own acts. Refresh is the pull
/// gesture. There is no session dock — opening is cheap and the tile is the
/// way back.
///
/// Standing decides the whole page: active lights everything, waiting turns
/// the band amber and holds the gate, and the verbs degrade by what they
/// mean — Scan and Enter are identity-level and survive everything; Invite
/// asserts a membership and is absent without one.
struct HomeView: View {
    let view: IosView
    let standing: HomeStanding
    let onRefresh: () -> Void
    let onOpen: (SpaceRow, BundledWorld) -> Void
    /// The switcher writes the choice; RootView owns the memory.
    let onSelectSpace: (SpaceRow) -> Void
    /// Founding seats the new Space as context; RootView owns that too.
    let onFounded: (String) -> Void

    @State private var switching = false
    @State private var entering = false
    @State private var scanning = false
    @State private var scannedLink: PresentedLink?
    @State private var founding = false
    @State private var inviteLink: PresentedLink?
    @State private var inviteFailure: String?

    var body: some View {
        VStack(spacing: 0) {
            band
            ScrollView {
                VStack(spacing: 16) {
                    switch standing {
                    case .active(let space):
                        worldGrid(space: space, locked: false)
                    case .waiting(let space):
                        worldGrid(space: space, locked: true)
                        waitingLine(space)
                    case .none:
                        emptyState
                    }
                }
                .padding(.horizontal, 20)
                .padding(.top, 16)
            }
        }
        .background(Theme.ground)
        .refreshable {
            if case .active(let space) = standing {
                _ = await Core.sync(spacePath: space.path)
            }
            onRefresh()
        }
        .sheet(isPresented: $switching) {
            SpaceSwitcherView(
                spaces: view.spaces,
                activeSpaceId: contextSpace?.spaceId,
                onSelect: { space in
                    switching = false
                    onSelectSpace(space)
                },
                onEnter: {
                    switching = false
                    entering = true
                },
                onFound: {
                    switching = false
                    founding = true
                }
            )
        }
        .sheet(isPresented: $entering) {
            EnterSpaceView(onJoined: {
                entering = false
                onRefresh()
            })
        }
        .sheet(isPresented: $scanning) {
            ScanInviteView { value in
                // The scan sheet dismisses itself; the read-back follows once
                // the dismissal settles or the present is dropped.
                Task { @MainActor in
                    try? await Task.sleep(for: .seconds(0.45))
                    scannedLink = PresentedLink(link: value)
                }
            }
        }
        .sheet(item: $scannedLink) { scanned in
            EnterSpaceView(onJoined: {
                scannedLink = nil
                onRefresh()
            }, initialLink: scanned.link)
        }
        .sheet(isPresented: $founding) {
            FoundSpaceView(onFounded: { spaceId in
                founding = false
                onFounded(spaceId)
            })
        }
        .sheet(item: $inviteLink) { minted in
            InviteShareView(link: minted.link)
        }
        .alert("Couldn't mint an invite", isPresented: inviteFailedShown) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(inviteFailure ?? "")
        }
    }

    private var contextSpace: SpaceRow? {
        switch standing {
        case .active(let space), .waiting(let space): space
        case .none: nil
        }
    }

    // ---- the band ----

    @ViewBuilder private var band: some View {
        VStack(spacing: 18) {
            if let space = contextSpace {
                Button {
                    switching = true
                } label: {
                    HStack(spacing: 12) {
                        Text(String(space.name.prefix(1)).uppercased())
                            .font(.title3.weight(.heavy))
                            .foregroundStyle(.white)
                            .frame(width: 44, height: 44)
                            .background(.white.opacity(0.18), in: RoundedRectangle(cornerRadius: 7))
                        VStack(alignment: .leading, spacing: 1) {
                            Text("Space")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.white.opacity(0.65))
                            HStack(spacing: 8) {
                                Text(space.name)
                                    .font(.title2.weight(.heavy))
                                    .foregroundStyle(.white)
                                Image(systemName: "chevron.down")
                                    .font(.system(size: 12, weight: .bold))
                                    .foregroundStyle(.white)
                                    .frame(width: 22, height: 22)
                                    .background(.white.opacity(0.2), in: Circle())
                            }
                        }
                        Spacer()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            } else {
                HStack {
                    Text("Astrolabe")
                        .font(.title2.weight(.heavy))
                        .foregroundStyle(.white)
                    Spacer()
                }
            }
            verbs
        }
        .padding(.init(top: 12, leading: 20, bottom: 24, trailing: 20))
        .background {
            // Bleeds behind the status bar so the band owns the top of the
            // screen instead of floating under a strip of ground.
            UnevenRoundedRectangle(bottomLeadingRadius: 28, bottomTrailingRadius: 28)
                .fill(Color.emblem(for: contextSpace?.name ?? ""))
                .ignoresSafeArea(edges: .top)
        }
    }

    @ViewBuilder private var verbs: some View {
        let inviteable = { if case .active = standing { true } else { false } }()
        HStack(spacing: 10) {
            verb("Scan", symbol: "qrcode.viewfinder") {
                scanning = true
            }
            verb("Enter", symbol: "arrow.right.square") {
                entering = true
            }
            // Absent, not disabled, where there is no membership to assert.
            if inviteable {
                verb("Invite", symbol: "person.badge.plus")
            }
        }
    }

    private func verb(_ label: String, symbol: String, action: (() -> Void)? = nil) -> some View {
        Button {
            if let action {
                action()
            } else {
                invite()
            }
        } label: {
            VStack(spacing: 7) {
                Image(systemName: symbol)
                    .font(.system(size: 20, weight: .semibold))
                Text(label)
                    .font(.caption.weight(.semibold))
            }
            .foregroundStyle(.white)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
            .background(.white.opacity(0.14), in: RoundedRectangle(cornerRadius: 16))
        }
        .buttonStyle(.plain)
    }

    // ---- the tiles ----

    @ViewBuilder private func worldGrid(space: SpaceRow, locked: Bool) -> some View {
        LazyVGrid(columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)], spacing: 12) {
            ForEach(view.bundledWorlds, id: \.mount) { world in
                worldTile(world, space: space, locked: locked)
            }
        }
    }

    @ViewBuilder private func worldTile(_ world: BundledWorld, space: SpaceRow, locked: Bool) -> some View {
        let openable = world.openable && !locked
        Button {
            onOpen(space, world)
        } label: {
            VStack(alignment: .leading, spacing: 12) {
                WorldTile(mount: world.mount, accent: world.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text(world.name)
                        .font(.callout.weight(.bold))
                        .foregroundStyle(Theme.ink)
                    // Freshness ("3 new" / "Updated 2 hr ago") lands when the
                    // World can say what changed since you looked; until that
                    // fact exists the slot stays silent rather than invented.
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(16)
            .background(.white, in: RoundedRectangle(cornerRadius: Theme.cardRadius))
            .shadow(color: Theme.cobalt.opacity(0.08), radius: 12, y: 8)
        }
        .buttonStyle(.plain)
        .disabled(!openable)
        .opacity(openable ? 1 : 0.55)
    }

    // ---- the states ----

    /// The wait, stated once and quietly: it needs nothing from you, and it
    /// never owns the page.
    @ViewBuilder private func waitingLine(_ space: SpaceRow) -> some View {
        Text("\(space.name) is waiting on your inviter — it finishes on its own.")
            .font(.footnote)
            .foregroundStyle(Theme.secondary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .padding(.top, 4)
    }

    @ViewBuilder private var emptyState: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("No Spaces on this iPhone")
                .font(.headline)
                .foregroundStyle(Theme.ink)
            Text("Join one you were invited to, or start your own.")
                .font(.subheadline)
                .foregroundStyle(Theme.secondary)
            HStack(spacing: 10) {
                Button("Enter a Space") { entering = true }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.cobalt)
                Button("Start a Space") { founding = true }
                    .buttonStyle(.bordered)
                    .tint(Theme.cobalt)
            }
            .padding(.top, 6)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(.white, in: RoundedRectangle(cornerRadius: Theme.cardRadius))
    }

    // `@MainActor` on the task, explicitly: the writes land on `@State`, and
    // their isolation must not hang on what the SDK infers.
    private func invite() {
        guard case .active(let space) = standing else { return }
        Task { @MainActor in
            switch await Core.invite(spacePath: space.path) {
            case .minted(let link): inviteLink = PresentedLink(link: link)
            case .refused(let reason): inviteFailure = reason
            }
        }
    }

    private var inviteFailedShown: Binding<Bool> {
        Binding(get: { inviteFailure != nil }, set: { if !$0 { inviteFailure = nil } })
    }
}

/// The minted invite, handed to the system share sheet. The ticket is the
/// engine's signed, windowed, single-use capability; nothing is added to it.
struct InviteShareView: View {
    let link: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                Image(systemName: "person.badge.plus")
                    .font(.system(size: 44, weight: .light))
                    .foregroundStyle(.tint)
                Text("Invite minted").font(.headline)
                Text("Single-use, and it expires. Share it with the one person it's for.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                ShareLink(item: link) {
                    Label("Share invite", systemImage: "square.and.arrow.up")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .padding(.horizontal, 24)
            }
            .padding()
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium])
    }
}
