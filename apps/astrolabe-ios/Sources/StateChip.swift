import SwiftUI

/// The one state vocabulary, rendered. A measured fact gets a solid dot; an
/// unmeasured absence gets an outlined one — the difference is the point.
struct StateChip: View {
    enum Kind {
        case measured(Color)
        case unmeasured(Color)
    }

    let kind: Kind
    let text: String

    var body: some View {
        HStack(spacing: 5) {
            switch kind {
            case .measured(let color):
                Circle().fill(color).frame(width: 7, height: 7)
            case .unmeasured(let color):
                Circle().strokeBorder(color, lineWidth: 1.5).frame(width: 8, height: 8)
            }
            Text(text)
        }
        .font(.caption)
        .foregroundStyle(tint)
    }

    private var tint: Color {
        switch kind {
        case .measured(let color), .unmeasured(let color): color
        }
    }
}

extension StateChip {
    init(status: SpaceStatus) {
        switch status {
        case .serving(let provider):
            self.init(kind: .measured(.green), text: "serving via \(provider)")
        case .servingLocally:
            self.init(kind: .measured(.green), text: "serving locally")
        case .admissionPending:
            self.init(kind: .unmeasured(.orange), text: "waiting for admission")
        case .notRunning:
            self.init(kind: .measured(.secondary), text: "not running anywhere")
        case .couldNotBeAsked:
            self.init(kind: .unmeasured(.orange), text: "couldn't be asked")
        case .storeMissing:
            self.init(kind: .unmeasured(.red), text: "store missing")
        }
    }

}
