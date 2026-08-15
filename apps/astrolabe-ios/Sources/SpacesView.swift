import SwiftUI

/// The library. One row per Space from the node's registry; Worlds open
/// inline behind a disclosure. Listing is passive — nothing here probes,
/// mounts, or connects because you looked at it. Entering and inviting are
/// the two acts, and both are explicit.
struct SpacesView: View {
    let view: IosView
    let onRefresh: () -> Void
    let onOpen: (SpaceRow, SpaceWorldRow) -> Void

    @State private var entering = false
    @State private var inviteLink: String?
    @State private var inviteFailure: String?
    @State private var syncReport: String?

    var body: some View {
        NavigationStack {
            List {
                if view.spaces.isEmpty {
                    emptyState
                } else {
                    ForEach(view.spaces, id: \.spaceId) { space in
                        SpaceSection(space: space, onOpen: onOpen, onInvite: invite, onSync: sync)
                    }
                }
                bundledSection
            }
            .navigationTitle("Spaces")
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        entering = true
                    } label: {
                        Label("Enter a Space", systemImage: "plus")
                    }
                }
            }
            .sheet(isPresented: $entering) {
                EnterSpaceView(onJoined: {
                    entering = false
                    onRefresh()
                })
            }
            .sheet(item: $inviteLink) { link in
                InviteShareView(link: link)
            }
            .alert("Couldn't mint an invite", isPresented: inviteFailedShown) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(inviteFailure ?? "")
            }
            .alert("Sync", isPresented: syncShown) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(syncReport ?? "")
            }
        }
    }

    private func invite(_ space: SpaceRow) {
        Task {
            let outcome = await Task.detached { Core.invite(spacePath: space.path) }.value
            switch outcome {
            case .minted(let link): inviteLink = link
            case .refused(let reason): inviteFailure = reason
            }
        }
    }

    private var inviteFailedShown: Binding<Bool> {
        Binding(get: { inviteFailure != nil }, set: { if !$0 { inviteFailure = nil } })
    }

    private func sync(_ space: SpaceRow) {
        Task {
            let outcome = await Task.detached { Core.sync(spacePath: space.path) }.value
            switch outcome {
            case .report(let message): syncReport = message
            case .refused(let reason): syncReport = reason
            }
        }
    }

    private var syncShown: Binding<Bool> {
        Binding(get: { syncReport != nil }, set: { if !$0 { syncReport = nil } })
    }

    @ViewBuilder private var emptyState: some View {
        Section {
            VStack(alignment: .leading, spacing: 6) {
                Text("No Spaces on this iPhone").font(.headline)
                Text("Enter one from an invite — your inviter shares a lait://join link or QR from their Astrolabe.")
                    .font(.subheadline).foregroundStyle(.secondary)
                Button("Enter a Space") { entering = true }
                    .buttonStyle(.borderedProminent)
                    .padding(.top, 6)
            }
            .padding(.vertical, 6)
        }
    }

    /// Compile-time truth: what this signed build carries.
    @ViewBuilder private var bundledSection: some View {
        Section("This build carries") {
            ForEach(view.bundledWorlds, id: \.mount) { world in
                HStack(spacing: 12) {
                    RoundedRectangle(cornerRadius: 2)
                        .fill(Color(accent: world.accent))
                        .frame(width: 4, height: 32)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(world.name).font(.body.weight(.medium))
                        if let tagline = world.tagline {
                            Text(tagline).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    Spacer()
                    if !world.openable {
                        Text("not openable").font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
        }
    }
}

private struct SpaceSection: View {
    let space: SpaceRow
    let onOpen: (SpaceRow, SpaceWorldRow) -> Void
    let onInvite: (SpaceRow) -> Void
    let onSync: (SpaceRow) -> Void
    @State private var expanded = true

    var body: some View {
        Section {
            DisclosureGroup(isExpanded: $expanded) {
                ForEach(space.worlds, id: \.mount) { world in
                    Button {
                        onOpen(space, world)
                    } label: {
                        HStack(spacing: 12) {
                            RoundedRectangle(cornerRadius: 2)
                                .fill(Color(accent: world.resident ? world.accent : nil))
                                .frame(width: 4, height: 28)
                            Text(world.name).foregroundStyle(.primary)
                            Spacer()
                            if !world.resident {
                                Text("not resident").font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    .disabled(!world.resident)
                }
            } label: {
                VStack(alignment: .leading, spacing: 3) {
                    Text(space.name).font(.body.weight(.semibold))
                    StateChip(status: space.status)
                }
                .contextMenu {
                    Button {
                        onInvite(space)
                    } label: {
                        Label("Invite…", systemImage: "person.badge.plus")
                    }
                    Button {
                        onSync(space)
                    } label: {
                        Label("Sync now", systemImage: "arrow.triangle.2.circlepath")
                    }
                    if let orbit = space.orbitId {
                        Text("orb: \(orbit.prefix(14))…")
                    }
                    Text("id: \(space.spaceId)")
                }
            }
        }
    }
}

/// The minted invite, handed to the system share sheet. The ticket is the
/// engine's signed, windowed, single-use capability; nothing is added to it.
private struct InviteShareView: View {
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

extension String: @retroactive Identifiable {
    public var id: String { self }
}
