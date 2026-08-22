import SwiftUI

/// One Space at a time, like an account. The sheet lists standings, not
/// machinery: the active Space is checked, a pending one wears the padlock
/// and names the wait, and the bottom carries both ways through — join a
/// Space, or start one. Every door shows both.
struct SpaceSwitcherView: View {
    let spaces: [SpaceRow]
    let activeSpaceId: String?
    let onSelect: (SpaceRow) -> Void
    let onEnter: () -> Void
    let onFound: () -> Void

    var body: some View {
        VStack(spacing: 4) {
            Text("Your Spaces")
                .font(.headline.weight(.heavy))
                .foregroundStyle(Theme.ink)
                .padding(.vertical, 14)
            ForEach(spaces, id: \.spaceId) { space in
                row(space)
                if space.spaceId != spaces.last?.spaceId {
                    Divider().padding(.leading, 74)
                }
            }
            if !spaces.isEmpty {
                Divider().padding(.horizontal, 6).padding(.vertical, 4)
            }
            door(
                "Enter with an invite",
                subtitle: "Scan or paste what someone sent you",
                symbol: "arrow.right.square",
                action: onEnter
            )
            door(
                "Start a new Space",
                subtitle: "Name it, then invite your people",
                symbol: "plus.square",
                action: onFound
            )
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .presentationBackground(.white)
    }

    @ViewBuilder private func row(_ space: SpaceRow) -> some View {
        let selectable = HomeStanding.actable(space)
        Button {
            onSelect(space)
        } label: {
            HStack(spacing: 14) {
                Text(String(space.name.prefix(1)).uppercased())
                    .font(.title3.weight(.heavy))
                    .foregroundStyle(.white)
                    .frame(width: 46, height: 46)
                    .background(Color.emblem(for: space.name), in: RoundedRectangle(cornerRadius: 7))
                VStack(alignment: .leading, spacing: 2) {
                    Text(space.name)
                        .font(.body.weight(.bold))
                        .foregroundStyle(Theme.ink)
                    if let (symbol, line) = subtitle(space.status) {
                        HStack(spacing: 5) {
                            Image(systemName: symbol)
                                .font(.system(size: 11, weight: .semibold))
                            Text(line)
                                .font(.caption.weight(.medium))
                        }
                        .foregroundStyle(Theme.secondary)
                    }
                }
                Spacer()
                if space.spaceId == activeSpaceId {
                    Image(systemName: "checkmark")
                        .font(.system(size: 14, weight: .heavy))
                        .foregroundStyle(.white)
                        .frame(width: 26, height: 26)
                        .background(Theme.cobalt, in: Circle())
                }
            }
            .padding(.init(top: 10, leading: 14, bottom: 10, trailing: 14))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!selectable)
        .opacity(selectable ? 1 : 0.55)
    }

    @ViewBuilder private func door(
        _ title: String, subtitle: String, symbol: String, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 14) {
                Image(systemName: symbol)
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(Theme.cobalt)
                    .frame(width: 46, height: 46)
                    .background(Theme.cobalt.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.body.weight(.bold))
                        .foregroundStyle(Theme.cobalt)
                    Text(subtitle)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.secondary)
                }
                Spacer()
            }
            .padding(.init(top: 8, leading: 14, bottom: 8, trailing: 14))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    /// Standing has glyphs, not sentences: the padlock is admission's, the
    /// cloud the missing store's. Social facts render elsewhere and never
    /// sound functional.
    private func subtitle(_ status: SpaceStatus) -> (String, String)? {
        switch status {
        case .admissionPending: ("lock", "Your inviter opens the gate — waiting")
        case .storeMissing: ("icloud.slash", "Not on this iPhone any more")
        default: nil
        }
    }
}
