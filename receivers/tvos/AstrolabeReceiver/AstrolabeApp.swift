import SwiftUI

@main
struct AstrolabeReceiverApp: App {
    @StateObject private var receiver = ReceiverCoordinator()

    var body: some Scene {
        WindowGroup {
            ReceiverView(receiver: receiver)
                .task { receiver.start() }
        }
    }
}

struct ReceiverView: View {
    @ObservedObject var receiver: ReceiverCoordinator

    var body: some View {
        ZStack {
            LinearGradient(
                colors: [Color(red: 0.07, green: 0.10, blue: 0.14), Color(red: 0.035, green: 0.05, blue: 0.07)],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            ).ignoresSafeArea()

            content
            VStack {
                trustHeader
                Spacer()
            }
        }
        .preferredColorScheme(.dark)
        .onExitCommand {
            if case .pairing = receiver.screen { receiver.cancelPairing() }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch receiver.screen {
        case .booting:
            panel(eyebrow: "Astrolabe Display", title: "Starting this receiver…", body: "Opening protected device state and contacting the approved coordinator.") {
                ProgressView().controlSize(.large)
            }
        case let .pairing(words, fingerprint, confirmed):
            pairingPanel(words: words, fingerprint: fingerprint, confirmed: confirmed)
        case let .unassigned(device):
            panel(eyebrow: "Receiver enrolled", title: "Ready for an assignment", body: "Choose this display in Astrolabe Displays.\n\n\(device)") { EmptyView() }
        case let .frame(image, summary):
            Color.black.ignoresSafeArea()
                .overlay(Image(uiImage: image).resizable().aspectRatio(contentMode: .fit))
                .accessibilityLabel(summary ?? "Assigned Astrolabe display frame")
        case let .message(eyebrow, title, body, retry):
            panel(eyebrow: eyebrow, title: title, body: body) {
                if retry { Button("Try again") { receiver.retry() }.buttonStyle(.borderedProminent) }
            }
        }
    }

    private var trustHeader: some View {
        HStack(spacing: 18) {
            Image(systemName: "scope")
                .font(.system(size: 38, weight: .semibold))
                .foregroundStyle(Color(red: 0.50, green: 0.84, blue: 0.78))
            VStack(alignment: .leading, spacing: 2) {
                Text("Astrolabe").font(.title2.bold())
                Text("Nixie Solutions LLC").foregroundStyle(.secondary)
            }
            Spacer()
            statusPill(receiver.transportState, color: receiver.transportState == "online" ? .mint : .orange)
            statusPill(receiver.sourceState == "none" ? "No source" : receiver.sourceState.capitalized, color: sourceColor)
            if receiver.stale { statusPill("Stale", color: .orange) }
        }
        .padding(.horizontal, 58)
        .padding(.vertical, 25)
        .background(.black.opacity(0.78))
    }

    private var sourceColor: Color {
        switch receiver.sourceState {
        case "current": return .mint
        case "partial": return .yellow
        case "unavailable": return .red
        default: return .gray
        }
    }

    private func statusPill(_ text: String, color: Color) -> some View {
        Text(text.capitalized)
            .font(.headline)
            .foregroundStyle(color)
            .padding(.horizontal, 20)
            .padding(.vertical, 10)
            .background(.black.opacity(0.4), in: Capsule())
            .overlay(Capsule().stroke(color.opacity(0.55), lineWidth: 1))
    }

    private func panel<Accessory: View>(
        eyebrow: String,
        title: String,
        body: String,
        @ViewBuilder accessory: () -> Accessory
    ) -> some View {
        VStack(spacing: 30) {
            accessory()
            Text(eyebrow.uppercased())
                .font(.headline.weight(.bold))
                .tracking(4)
                .foregroundStyle(Color(red: 0.50, green: 0.84, blue: 0.78))
            Text(title).font(.system(size: 62, weight: .bold)).multilineTextAlignment(.center)
            Text(body).font(.title2).foregroundStyle(.secondary).multilineTextAlignment(.center)
        }
        .frame(maxWidth: 1_250)
        .padding(70)
        .background(Color(red: 0.06, green: 0.09, blue: 0.12).opacity(0.96), in: RoundedRectangle(cornerRadius: 34))
        .overlay(RoundedRectangle(cornerRadius: 34).stroke(.white.opacity(0.14), lineWidth: 1))
        .padding(.top, 100)
    }

    private func pairingPanel(words: [String], fingerprint: String, confirmed: Bool) -> some View {
        VStack(spacing: 28) {
            Text("CONFIRM THIS DISPLAY").font(.headline.bold()).tracking(4).foregroundStyle(.mint)
            Text("Compare these words in Astrolabe").font(.system(size: 58, weight: .bold))
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 18), count: 3), spacing: 18) {
                ForEach(words, id: \.self) { word in
                    Text(word.uppercased()).font(.title2.bold()).frame(maxWidth: .infinity).padding(20)
                        .background(Color.green.opacity(0.12), in: RoundedRectangle(cornerRadius: 16))
                        .overlay(RoundedRectangle(cornerRadius: 16).stroke(.mint.opacity(0.45)))
                }
            }
            Text("Coordinator SHA-256\n\(fingerprint)")
                .font(.system(.body, design: .monospaced)).foregroundStyle(.secondary).multilineTextAlignment(.center)
            if confirmed {
                Text("Confirmed here. Waiting for authenticated approval in Astrolabe…").foregroundStyle(.yellow)
            } else {
                Button("Words match") { receiver.confirmPairing() }.buttonStyle(.borderedProminent)
                Text("Nothing is enrolled until you confirm here and approve in Astrolabe.").foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: 1_300)
        .padding(60)
        .background(Color(red: 0.06, green: 0.09, blue: 0.12).opacity(0.97), in: RoundedRectangle(cornerRadius: 34))
        .overlay(RoundedRectangle(cornerRadius: 34).stroke(.white.opacity(0.14)))
        .padding(.top, 100)
    }
}
