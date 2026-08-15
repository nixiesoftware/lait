import SwiftUI

/// First launch: one sentence, one act. The door that exists today is the
/// Space invite — the node's own way in.
struct WelcomeView: View {
    let onDone: () -> Void
    @State private var entering = false

    var body: some View {
        VStack(spacing: 16) {
            Spacer()
            Image(systemName: "safari")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(.tint)
            Text("Astrolabe")
                .font(.largeTitle.weight(.bold))
            Text("Enter a Space from an invite to reach it from this iPhone. An invite admits this device to that Space and nothing else.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Spacer()
            Button {
                entering = true
            } label: {
                Text("Enter a Space")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .padding(.horizontal, 24)
            Button("Continue without a Space", action: onDone)
                .font(.subheadline)
                .padding(.bottom, 24)
        }
        .sheet(isPresented: $entering) {
            EnterSpaceView(onJoined: {
                entering = false
                onDone()
            })
        }
    }
}
