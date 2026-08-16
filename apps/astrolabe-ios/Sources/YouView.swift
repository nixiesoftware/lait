import SwiftUI

/// What a person owns: identity, devices, the Book, and the build itself.
/// Rows that have nothing to show say which kind of nothing.
struct YouView: View {
    let view: IosView

    var body: some View {
        NavigationStack {
            List {
                Section("This iPhone") {
                    switch view.link {
                    case .linked(let deviceName, let did):
                        VStack(alignment: .leading, spacing: 3) {
                            Text(deviceName)
                            Text(did)
                                .font(.caption.monospaced())
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                    case .unlinked:
                        LabeledContent("Identity", value: "not linked")
                    case .unavailable:
                        VStack(alignment: .leading, spacing: 3) {
                            LabeledContent("Identity", value: "not linked")
                            Text("Linking arrives in a later build.")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                Section {
                    LabeledContent("Linked devices", value: linkedAbsence)
                    LabeledContent("People", value: linkedAbsence)
                    LabeledContent("Enrollments", value: "none")
                } footer: {
                    Text("Devices, People, and Enrollments fill in once this iPhone is linked.")
                }
                Section("About") {
                    LabeledContent("Core", value: view.coreVersion)
                    LabeledContent("Keychain", value: keychainText)
                }
            }
            .navigationTitle("You")
        }
    }

    /// Not zero — absent, because unmeasured until a link exists.
    private var linkedAbsence: String {
        if case .linked = view.link { "0" } else { "—" }
    }

    private var keychainText: String {
        switch KeychainProbe.outcome {
        case .roundTripped: "ok"
        case .entropyFailed(let s): "entropy unavailable (\(s))"
        case .writeFailed(let s): "write failed (\(s))"
        case .readFailed(let s): "read failed (\(s))"
        case .mismatch: "mismatch"
        }
    }
}
