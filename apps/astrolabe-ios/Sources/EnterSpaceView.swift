import SwiftUI

/// The join, in three honest stages: read the invite (nothing created), show
/// what it names and ask, then enter and report exactly what happened —
/// admitted, or bootstrapped-but-waiting, which are different facts.
struct EnterSpaceView: View {
    let onJoined: () -> Void
    @Environment(\.dismiss) private var dismiss

    init(onJoined: @escaping () -> Void, initialLink: String? = nil) {
        self.onJoined = onJoined
        _link = State(initialValue: initialLink ?? "")
    }

    enum Stage {
        case pasting
        case invalid(String)
        case confirming(TicketFacts, link: String)
        case joining
        case entered(Entered)
        case refused(String)
    }

    @State private var link = ""
    @State private var stage: Stage = .pasting
    @State private var scanning = false

    var body: some View {
        NavigationStack {
            Group {
                switch stage {
                case .pasting, .invalid:
                    pasteStage
                case .confirming(let facts, let link):
                    confirmStage(facts, link: link)
                case .joining:
                    joiningStage
                case .entered(let entered):
                    enteredStage(entered)
                case .refused(let reason):
                    refusedStage(reason)
                }
            }
            .navigationTitle("Enter a Space")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .interactiveDismissDisabled(isJoining)
        .sheet(isPresented: $scanning) {
            ScanInviteView { value in
                link = value
                switch Core.ticket(value) {
                case .valid(let facts):
                    stage = .confirming(facts, link: value)
                case .invalid(let reason):
                    stage = .invalid(reason)
                }
            }
        }
    }

    private var isJoining: Bool {
        if case .joining = stage { return true }
        return false
    }

    @ViewBuilder private var pasteStage: some View {
        Form {
            Section {
                Button {
                    scanning = true
                } label: {
                    Label("Scan the invite QR", systemImage: "qrcode.viewfinder")
                }
            } footer: {
                Text("Your inviter shows the QR from their Astrolabe.")
            }
            Section {
                TextField("Paste a lait://join/… invite", text: $link, axis: .vertical)
                    .lineLimit(4...8)
                    .font(.caption.monospaced())
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
            } footer: {
                if case .invalid(let reason) = stage {
                    Text("That invite can't be used: \(reason). Ask for a new one — invites expire and spend.")
                        .foregroundStyle(.red)
                } else {
                    Text("Your inviter shares it from their Astrolabe. It is signed and single-use; reading it creates nothing.")
                }
            }
            Button("Read invite") {
                switch Core.ticket(link) {
                case .valid(let facts):
                    stage = .confirming(facts, link: link)
                case .invalid(let reason):
                    stage = .invalid(reason)
                }
            }
            .disabled(link.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    @ViewBuilder private func confirmStage(_ facts: TicketFacts, link: String) -> some View {
        Form {
            Section {
                if !facts.nameHint.isEmpty {
                    LabeledContent("Space", value: facts.nameHint)
                }
                if !facts.hostNickHint.isEmpty {
                    LabeledContent("Invited by", value: facts.hostNickHint)
                }
                LabeledContent("Space id") {
                    Text(facts.spaceId)
                        .font(.caption.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            } header: {
                Text("This invite names")
            } footer: {
                Text("Names are the inviter's labels; the id is the fact. Joining creates this Space's store on this iPhone and admits this device to it — nothing else.")
            }
            Button("Join this Space") {
                stage = .joining
                Task {
                    let outcome = await Task.detached { Core.enter(link) }.value
                    switch outcome {
                    case .entered(let entered): stage = .entered(entered)
                    case .refused(let reason): stage = .refused(reason)
                    }
                }
            }
        }
    }

    @ViewBuilder private var joiningStage: some View {
        VStack(spacing: 14) {
            ProgressView()
            Text("Joining…").font(.headline)
            Text("Bootstrapping the store, then reaching your inviter for admission.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 40)
        }
    }

    @ViewBuilder private func enteredStage(_ entered: Entered) -> some View {
        VStack(spacing: 14) {
            Image(systemName: entered.admitted ? "checkmark.circle" : "clock")
                .font(.system(size: 44, weight: .light))
                .foregroundStyle(entered.admitted ? .green : .orange)
            Text(entered.admitted ? "You're in" : "Joined — waiting on your inviter")
                .font(.headline)
            Text(entered.admitted
                ? "This iPhone is a member. The Space appears under Spaces."
                : (entered.contacted
                    ? "Your inviter answered but admission hasn't landed yet. The Space stays encrypted until it does; it completes on its own."
                    : "Your inviter couldn't be reached. The store is ready; admission completes when they come online."))
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Button("Done") { onJoined() }
                .buttonStyle(.borderedProminent)
        }
    }

    @ViewBuilder private func refusedStage(_ reason: String) -> some View {
        VStack(spacing: 14) {
            Image(systemName: "xmark.circle")
                .font(.system(size: 44, weight: .light))
                .foregroundStyle(.red)
            Text("Couldn't join").font(.headline)
            Text(reason)
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Button("Start over") { stage = .pasting }
        }
    }
}
