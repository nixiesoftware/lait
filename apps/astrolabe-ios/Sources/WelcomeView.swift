import SwiftUI

/// First launch: two doors, both real. A Space is where your people and
/// your Worlds live — join one you were invited to, or start your own.
/// "Look around first" stays quiet underneath; nothing is created by
/// looking.
struct WelcomeView: View {
    let onDone: () -> Void
    let onFounded: (String) -> Void
    @State private var entering = false
    @State private var founding = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Spacer()
            HStack(spacing: 6) {
                RoundedRectangle(cornerRadius: 5).fill(Theme.cobalt)
                    .frame(width: 34, height: 34)
                RoundedRectangle(cornerRadius: 5).fill(Color(accent: 0x4C6EF5).opacity(0.55))
                    .frame(width: 34, height: 34)
                RoundedRectangle(cornerRadius: 5).fill(Color(red: 0.894, green: 0.910, blue: 0.949))
                    .frame(width: 34, height: 34)
            }
            .padding(.bottom, 12)
            Text("Astrolabe")
                .font(.system(size: 34, weight: .heavy))
                .foregroundStyle(Theme.ink)
            Text("A Space is where your people and your Worlds live. Join one you were invited to, or start your own.")
                .font(.subheadline)
                .foregroundStyle(Theme.secondary)
            Spacer()
            door(
                "I have an invite",
                subtitle: "Scan the QR or paste the link",
                symbol: "arrow.right.square",
                prominent: true
            ) { entering = true }
            door(
                "Start a Space",
                subtitle: "Name it, then invite your people",
                symbol: "plus.square",
                prominent: false
            ) { founding = true }
            Button("Look around first", action: onDone)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Theme.secondary)
                .frame(maxWidth: .infinity)
                .padding(.top, 8)
        }
        .padding(.init(top: 0, leading: 24, bottom: 40, trailing: 24))
        .background(Theme.ground)
        .sheet(isPresented: $entering) {
            EnterSpaceView(onJoined: {
                entering = false
                onDone()
            })
        }
        .sheet(isPresented: $founding) {
            FoundSpaceView(onFounded: { spaceId in
                founding = false
                onFounded(spaceId)
            })
        }
    }

    @ViewBuilder private func door(
        _ title: String, subtitle: String, symbol: String, prominent: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 14) {
                Image(systemName: symbol)
                    .font(.system(size: 20, weight: .semibold))
                    .foregroundStyle(prominent ? .white : Theme.cobalt)
                    .frame(width: 42, height: 42)
                    .background(
                        prominent ? Color.white.opacity(0.18) : Theme.cobalt.opacity(0.08),
                        in: RoundedRectangle(cornerRadius: 13)
                    )
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.headline.weight(.heavy))
                        .foregroundStyle(prominent ? .white : Theme.ink)
                    Text(subtitle)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(prominent ? .white.opacity(0.7) : Theme.secondary)
                }
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.system(size: 14, weight: .bold))
                    .foregroundStyle(prominent ? .white.opacity(0.8) : Color(red: 0.769, green: 0.800, blue: 0.878))
            }
            .padding(18)
            .background(
                prominent ? Theme.cobalt : Color.white,
                in: RoundedRectangle(cornerRadius: Theme.cardRadius)
            )
            .shadow(
                color: prominent ? Theme.cobalt.opacity(0.3) : Theme.cobalt.opacity(0.08),
                radius: prominent ? 14 : 12, y: 8
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}
