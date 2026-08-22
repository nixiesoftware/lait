import SwiftUI

/// Founding: the naming ceremony — the mirror of the join read-back. The
/// same panel shape, opposite direction: you AUTHOR the facts instead of an
/// invite filling them, and the consequences are stated plainly before
/// Create. The emblem preview derives from the typed name, so the promise
/// "its colour comes from its name" is true by construction.
struct FoundSpaceView: View {
    let onFounded: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    enum Stage {
        case naming
        case creating
        case refused(String)
    }

    @State private var name = ""
    @State private var nick = ""
    @State private var stage: Stage = .naming

    var body: some View {
        NavigationStack {
            Group {
                switch stage {
                case .naming:
                    namingStage
                case .creating:
                    creatingStage
                case .refused(let reason):
                    refusedStage(reason)
                }
            }
            .navigationTitle("Start a Space")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .interactiveDismissDisabled(isCreating)
    }

    private var isCreating: Bool {
        if case .creating = stage { return true }
        return false
    }

    private var trimmedName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    @ViewBuilder private var namingStage: some View {
        Form {
            Section {
                HStack {
                    Spacer()
                    VStack(spacing: 8) {
                        Text(String(trimmedName.prefix(1)).uppercased())
                            .font(.system(size: 30, weight: .heavy))
                            .foregroundStyle(.white)
                            .frame(width: 68, height: 68)
                            .background(
                                trimmedName.isEmpty
                                    ? AnyShapeStyle(.quaternary)
                                    : AnyShapeStyle(Color.emblem(for: trimmedName)),
                                in: RoundedRectangle(cornerRadius: 10)
                            )
                        Text("Its colour comes from its name")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                    Spacer()
                }
                .listRowBackground(Color.clear)
            }
            Section {
                LabeledContent("Call it") {
                    TextField("Northlight", text: $name)
                        .multilineTextAlignment(.trailing)
                        .fontWeight(.semibold)
                }
                LabeledContent("You appear as") {
                    TextField("Your nick", text: $nick)
                        .multilineTextAlignment(.trailing)
                        .fontWeight(.semibold)
                        .autocorrectionDisabled()
                }
            }
            Section {
                LabeledContent("Who can join") {
                    Text("Only people you invite").fontWeight(.semibold)
                }
                LabeledContent("Where it lives") {
                    Text("This iPhone, for now").fontWeight(.semibold)
                }
                LabeledContent("You'll be") {
                    Text("Its first member, and its admin").fontWeight(.semibold)
                }
            } footer: {
                Text("Nothing leaves this iPhone until you invite someone. Worlds open the moment it exists.")
            }
            Button(trimmedName.isEmpty ? "Create" : "Create \(trimmedName)") {
                create()
            }
            .disabled(trimmedName.isEmpty)
        }
    }

    @ViewBuilder private var creatingStage: some View {
        VStack(spacing: 14) {
            ProgressView()
            Text("Creating…").font(.headline)
            Text("Forming the store on this iPhone.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private func refusedStage(_ reason: String) -> some View {
        VStack(spacing: 14) {
            Image(systemName: "xmark.circle")
                .font(.system(size: 44, weight: .light))
                .foregroundStyle(.red)
            Text("Couldn't create it").font(.headline)
            Text(reason)
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Button("Start over") { stage = .naming }
        }
    }

    private func create() {
        stage = .creating
        let spaceName = trimmedName
        let userNick = nick.trimmingCharacters(in: .whitespacesAndNewlines)
        Task { @MainActor in
            switch await Core.found(name: spaceName, nick: userNick.isEmpty ? nil : userNick) {
            case .founded(let founded):
                onFounded(founded.spaceId)
            case .refused(let reason):
                stage = .refused(reason)
            }
        }
    }
}
