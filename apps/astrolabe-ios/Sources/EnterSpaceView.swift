import SwiftUI

/// The join, as a dialogue with a fact: the labeled slots sit visibly empty —
/// nothing is claimed before it's known — the invite fills them, and only
/// then does Join light up. Then enter and report exactly what happened:
/// admitted, or bootstrapped-but-waiting, which are different facts.
struct EnterSpaceView: View {
    let onJoined: () -> Void
    @Environment(\.dismiss) private var dismiss

    init(onJoined: @escaping () -> Void, initialLink: String? = nil, startScanning: Bool = false) {
        self.onJoined = onJoined
        _link = State(initialValue: initialLink ?? "")
        _scanning = State(initialValue: startScanning)
        // A delivered link is read on arrival — the sheet opens on the
        // read-back, not on a paste form holding what was already handed over.
        if let initialLink {
            switch Core.ticket(initialLink) {
            case .valid(let facts):
                _stage = State(initialValue: .confirming(facts, link: initialLink))
            case .invalid(let reason):
                _stage = State(initialValue: .invalid(reason))
            }
        }
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
    @State private var scanning: Bool

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
            .navigationTitle("Join a Space")
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
                read(value)
            }
        }
    }

    private var isJoining: Bool {
        if case .joining = stage { return true }
        return false
    }

    private func read(_ link: String) {
        switch Core.ticket(link) {
        case .valid(let facts):
            stage = .confirming(facts, link: link)
        case .invalid(let reason):
            stage = .invalid(reason)
        }
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
                    Text("It is signed and single-use; reading it creates nothing.")
                }
            }
            factsSection(nil)
            Button("Read invite") { read(link) }
                .disabled(link.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    @ViewBuilder private func confirmStage(_ facts: TicketFacts, link: String) -> some View {
        Form {
            Section {
                HStack {
                    Text(link)
                        .font(.caption.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                }
            }
            factsSection(facts)
            Button("Join \(facts.nameHint.isEmpty ? "this Space" : facts.nameHint)") {
                stage = .joining
                Task { @MainActor in
                    switch await Core.enter(link) {
                    case .entered(let entered): stage = .entered(entered)
                    case .refused(let reason): stage = .refused(reason)
                    }
                }
            }
        }
    }

    /// The read-back panel, one shape in both stages: labeled slots, visibly
    /// empty until the invite answers them.
    @ViewBuilder private func factsSection(_ facts: TicketFacts?) -> some View {
        Section {
            LabeledContent("Space") {
                if let facts {
                    if facts.nameHint.isEmpty {
                        Text(facts.spaceId)
                            .font(.caption.monospaced())
                            .lineLimit(1)
                            .truncationMode(.middle)
                    } else {
                        Text(facts.nameHint).fontWeight(.semibold)
                    }
                } else {
                    blankSlot
                }
            }
            factRow(
                "Invited by",
                read: facts != nil,
                text: facts.flatMap { $0.hostNickHint.isEmpty ? nil : $0.hostNickHint }
            )
            factRow("Invite", read: facts != nil, text: facts.flatMap(inviteBound))
            factRow("Joins as", read: facts != nil, text: facts.map { _ in "This iPhone" })
        } header: {
            Text("This invite names")
        } footer: {
            if facts == nil {
                Text("These fill in the moment an invite is read — nothing is claimed before it's known.")
            } else {
                Text("Names are the inviter's labels; the id is the fact. Joining creates this Space's store on this iPhone and admits this device to it — nothing else.")
            }
        }
    }

    /// A labeled slot. Unread: a visible blank, the panel's whole point.
    /// Read but unnamed: a dash — the invite doesn't carry that fact.
    @ViewBuilder private func factRow(_ label: String, read: Bool, text: String?) -> some View {
        LabeledContent(label) {
            if let text {
                Text(text).fontWeight(.semibold)
            } else if read {
                Text("—").foregroundStyle(.tertiary)
            } else {
                blankSlot
            }
        }
    }

    private var blankSlot: some View {
        RoundedRectangle(cornerRadius: 5)
            .fill(.quaternary)
            .frame(width: 96, height: 10)
    }

    /// The invite's own bounds, stated at read time: how many bindings, and
    /// how long the window has left.
    private func inviteBound(_ facts: TicketFacts) -> String? {
        guard let cap = facts.useCap else { return nil }
        let uses = cap == 1 ? "Single use" : "Up to \(cap) uses"
        guard let expires = facts.expiresAt else { return uses }
        let secondsLeft = Double(expires) - Date().timeIntervalSince1970
        if secondsLeft <= 0 { return "\(uses) · expired" }
        let days = Int(secondsLeft / 86_400)
        let window = days >= 1 ? "\(days) day\(days == 1 ? "" : "s") left" : "expires today"
        return "\(uses) · \(window)"
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
                ? "This iPhone is a member. The Space is ready on Home."
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
