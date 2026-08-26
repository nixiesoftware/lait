import SwiftUI

enum EffectFamily: Int, CaseIterable, Identifiable {
    case clouds
    case fabric
    case causticVeil

    var id: Int { rawValue }

    var name: String {
        switch self {
        case .clouds: "Clouds"
        case .fabric: "Fabric"
        case .causticVeil: "Caustics"
        }
    }
}

enum EffectSpectrum: Int, CaseIterable, Identifiable {
    case thermal
    case visible
    case aurora
    case biolume
    case plasma
    case solar

    var id: Int { rawValue }

    var name: String {
        switch self {
        case .thermal: "Thermal"
        case .visible: "Visible"
        case .aurora: "Aurora"
        case .biolume: "Biolume"
        case .plasma: "Plasma"
        case .solar: "Solar"
        }
    }

    var colors: [Color] {
        switch self {
        case .thermal:
            [
                Color(red: 0.12, green: 0.00, blue: 0.02), .red, .orange,
                Color(red: 1.00, green: 0.88, blue: 0.58), .cyan, .blue,
                Color(red: 0.86, green: 0.05, blue: 1.00),
            ]
        case .visible:
            [.red, .orange, .yellow, .green, .cyan, .blue, .purple]
        case .aurora:
            [
                Color(red: 0.01, green: 0.05, blue: 0.16),
                Color(red: 0.00, green: 0.46, blue: 0.32),
                Color(red: 0.16, green: 1.00, blue: 0.66), .cyan,
                Color(red: 0.35, green: 0.28, blue: 1.00),
                Color(red: 0.82, green: 0.18, blue: 1.00),
            ]
        case .biolume:
            [
                Color(red: 0.00, green: 0.05, blue: 0.08),
                Color(red: 0.00, green: 0.30, blue: 0.34),
                Color(red: 0.00, green: 0.82, blue: 0.66),
                Color(red: 0.56, green: 1.00, blue: 0.25),
                Color(red: 0.95, green: 1.00, blue: 0.74), .white,
            ]
        case .plasma:
            [
                Color(red: 0.02, green: 0.00, blue: 0.14),
                Color(red: 0.05, green: 0.14, blue: 0.88), .cyan, .white,
                Color(red: 0.80, green: 0.18, blue: 1.00),
                Color(red: 1.00, green: 0.12, blue: 0.56),
            ]
        case .solar:
            [
                Color(red: 0.10, green: 0.01, blue: 0.00),
                Color(red: 0.66, green: 0.03, blue: 0.00), .orange, .yellow,
                Color(red: 1.00, green: 0.94, blue: 0.68), .white,
                Color(red: 0.62, green: 0.84, blue: 1.00),
            ]
        }
    }
}

enum EffectDither: Int, CaseIterable, Identifiable {
    case none
    case bayer
    case ign

    var id: Int { rawValue }

    var name: String {
        switch self {
        case .none: "None"
        case .bayer: "Bayer"
        case .ign: "IGN"
        }
    }
}

enum EffectPostProcess: Int, CaseIterable, Identifiable {
    case none
    case posterize
    case scanlines
    case prism
    case bleach

    var id: Int { rawValue }

    var name: String {
        switch self {
        case .none: "None"
        case .posterize: "Posterize"
        case .scanlines: "Scanlines"
        case .prism: "Prism"
        case .bleach: "Bleach"
        }
    }
}

enum EffectMorphPad: Int, CaseIterable, Identifiable {
    case form
    case motion
    case light
    case frame
    case finish

    var id: Int { rawValue }

    var name: String {
        switch self {
        case .form: "Form"
        case .motion: "Motion"
        case .light: "Light"
        case .frame: "Frame"
        case .finish: "Finish"
        }
    }

    var xName: String {
        switch self {
        case .form: "Structure"
        case .motion: "Flow"
        case .light: "Softness"
        case .frame: "Precipice"
        case .finish: "Dither"
        }
    }

    var yName: String {
        switch self {
        case .form: "Detail"
        case .motion: "Turbulence"
        case .light: "Glow"
        case .frame: "Energy"
        case .finish: "Post"
        }
    }

    var xLow: String {
        switch self {
        case .form: "Broad"
        case .motion: "Still"
        case .light: "Crisp"
        case .frame: "Narrow"
        case .finish: "Smooth"
        }
    }

    var xHigh: String {
        switch self {
        case .form: "Fine"
        case .motion: "Fast"
        case .light: "Diffuse"
        case .frame: "Deep"
        case .finish: "Textured"
        }
    }

    var yLow: String {
        switch self {
        case .form: "Simple"
        case .motion: "Calm"
        case .light: "Dim"
        case .frame: "Dormant"
        case .finish: "Natural"
        }
    }

    var yHigh: String {
        switch self {
        case .form: "Intricate"
        case .motion: "Wild"
        case .light: "Luminous"
        case .frame: "Active"
        case .finish: "Processed"
        }
    }
}

struct EffectPreset: Identifiable {
    let id: String
    let name: String
    let family: EffectFamily
    let structure: Float
    let detail: Float
    let turbulence: Float
    let softness: Float
    let flow: Float
    let glow: Float
    let seed: Float
}

/// The lab's editable state. Everything maps to a compact shader uniform so
/// experimentation never rebuilds the render pipeline or allocates per frame.
@MainActor
final class EffectLabModel: ObservableObject {
    @Published var family = EffectFamily.clouds
    @Published var spectrum = EffectSpectrum.thermal
    @Published var backgroundColor = Color(red: 0.91, green: 0.87, blue: 0.77)
    @Published var borderReach: Float = 0.18
    @Published var structure: Float = 1.45
    @Published var detail: Float = 0.48
    @Published var turbulence: Float = 1.15
    @Published var softness: Float = 0.46
    @Published var flow: Float = 0.12
    @Published var glow: Float = 0.45
    @Published var energy: Float = 0.0
    @Published var dither = EffectDither.none
    @Published var ditherRatio: Float = 0.65
    @Published var postProcess = EffectPostProcess.none
    @Published var postAmount: Float = 0.5
    @Published var seed: Float = 17
    @Published var previewTime: Float = 6
    @Published var isAnimating = true
    @Published var renderScale: Float = 0.5
    @Published var frameRate = 30
    @Published var morphPad = EffectMorphPad.form
    @Published private(set) var isMorphing = false
    @Published private(set) var isComparingMorph = false
    @Published private(set) var morphStart = CGPoint.zero
    @Published private(set) var morphPosition = CGPoint.zero

    private struct MorphSnapshot: Equatable {
        var borderReach: Float
        var structure: Float
        var detail: Float
        var turbulence: Float
        var softness: Float
        var flow: Float
        var glow: Float
        var energy: Float
        var ditherRatio: Float
        var postAmount: Float
    }

    private var morphBaseline: MorphSnapshot?
    private var morphPreview: MorphSnapshot?
    private var morphHistory: [MorphSnapshot] = []

    static let presets = [
        EffectPreset(
            id: "deep-breath", name: "Deep Breath", family: .clouds,
            structure: 1.45, detail: 0.48, turbulence: 1.15, softness: 0.46,
            flow: 0.12, glow: 0.45, seed: 17
        ),
        EffectPreset(
            id: "violet-fabric", name: "Violet Fabric", family: .fabric,
            structure: 1.34, detail: 0.46, turbulence: 1.08, softness: 0.48,
            flow: 0.09, glow: 0.62, seed: 73
        ),
        EffectPreset(
            id: "liquid-light", name: "Liquid Light", family: .causticVeil,
            structure: 1.72, detail: 0.38, turbulence: 1.24, softness: 0.35,
            flow: 0.17, glow: 0.76, seed: 41
        ),
        EffectPreset(
            id: "warm-signal", name: "Warm Signal", family: .clouds,
            structure: 1.75, detail: 0.36, turbulence: 1.55, softness: 0.38,
            flow: 0.16, glow: 0.66, seed: 29
        ),
    ]

    func apply(_ preset: EffectPreset) {
        family = preset.family
        structure = preset.structure
        detail = preset.detail
        turbulence = preset.turbulence
        softness = preset.softness
        flow = preset.flow
        glow = preset.glow
        seed = preset.seed
    }

    func randomize() {
        family = EffectFamily.allCases.randomElement() ?? .clouds
        spectrum = EffectSpectrum.allCases.randomElement() ?? .thermal
        structure = .random(in: 0.75 ... 2.65)
        detail = .random(in: 0.25 ... 0.68)
        turbulence = .random(in: 0.25 ... 2.1)
        softness = .random(in: 0.22 ... 0.72)
        flow = .random(in: 0.04 ... 0.28)
        glow = .random(in: 0.18 ... 0.9)
        seed = .random(in: 1 ... 999)
    }

    var morphXValue: Float {
        switch morphPad {
        case .form: structure
        case .motion: flow
        case .light: softness
        case .frame: borderReach
        case .finish: ditherRatio
        }
    }

    var morphYValue: Float {
        switch morphPad {
        case .form: detail
        case .motion: turbulence
        case .light: glow
        case .frame: energy
        case .finish: postAmount
        }
    }

    /// Normalized points are in the Metal view's coordinate system, from
    /// top-left (0, 0) to bottom-right (1, 1). Deltas are relative to where
    /// the finger landed, so touching the preview never makes values jump.
    func beginMorph(at point: CGPoint) {
        guard !isMorphing else { return }
        let snapshot = captureMorphSnapshot()
        morphBaseline = snapshot
        morphPreview = snapshot
        morphStart = point
        morphPosition = point
        isMorphing = true
        isComparingMorph = false
    }

    func updateMorph(at point: CGPoint) {
        guard let baseline = morphBaseline else { return }
        morphPosition = point

        // About half a screen of travel covers 60% of a parameter's range.
        // This is fast enough for exploration without sacrificing small moves.
        let xDelta = Float(point.x - morphStart.x) * 1.2
        let yDelta = Float(morphStart.y - point.y) * 1.2
        var preview = baseline

        switch morphPad {
        case .form:
            preview.structure = clamp(baseline.structure + xDelta * 2.5, to: 0.5 ... 3)
            preview.detail = clamp(baseline.detail + yDelta * 0.6, to: 0.15 ... 0.75)
        case .motion:
            preview.flow = clamp(baseline.flow + xDelta * 0.35, to: 0 ... 0.35)
            preview.turbulence = clamp(baseline.turbulence + yDelta * 2.5, to: 0 ... 2.5)
        case .light:
            preview.softness = clamp(baseline.softness + xDelta * 0.7, to: 0.15 ... 0.85)
            preview.glow = clamp(baseline.glow + yDelta, to: 0 ... 1)
        case .frame:
            preview.borderReach = clamp(baseline.borderReach + xDelta * 0.35, to: 0.05 ... 0.40)
            preview.energy = clamp(baseline.energy + yDelta, to: 0 ... 1)
        case .finish:
            preview.ditherRatio = clamp(baseline.ditherRatio + xDelta, to: 0 ... 1)
            preview.postAmount = clamp(baseline.postAmount + yDelta, to: 0 ... 1)
        }

        morphPreview = preview
        if !isComparingMorph {
            applyMorphSnapshot(preview)
        }
    }

    func setMorphComparison(_ comparing: Bool) {
        guard isMorphing, comparing != isComparingMorph else { return }
        isComparingMorph = comparing
        if comparing, let baseline = morphBaseline {
            applyMorphSnapshot(baseline)
        } else if let preview = morphPreview {
            applyMorphSnapshot(preview)
        }
    }

    func endMorph() {
        guard let baseline = morphBaseline else { return }
        if isComparingMorph, let preview = morphPreview {
            applyMorphSnapshot(preview)
        }
        if let preview = morphPreview, preview != baseline {
            morphHistory.append(baseline)
            if morphHistory.count > 30 {
                morphHistory.removeFirst()
            }
        }
        clearMorphSession()
    }

    func cancelMorph() {
        if let baseline = morphBaseline {
            applyMorphSnapshot(baseline)
        }
        clearMorphSession()
    }

    func undoMorph() {
        cancelMorph()
        guard let previous = morphHistory.popLast() else { return }
        applyMorphSnapshot(previous)
    }

    private func captureMorphSnapshot() -> MorphSnapshot {
        MorphSnapshot(
            borderReach: borderReach,
            structure: structure,
            detail: detail,
            turbulence: turbulence,
            softness: softness,
            flow: flow,
            glow: glow,
            energy: energy,
            ditherRatio: ditherRatio,
            postAmount: postAmount
        )
    }

    private func applyMorphSnapshot(_ snapshot: MorphSnapshot) {
        borderReach = snapshot.borderReach
        structure = snapshot.structure
        detail = snapshot.detail
        turbulence = snapshot.turbulence
        softness = snapshot.softness
        flow = snapshot.flow
        glow = snapshot.glow
        energy = snapshot.energy
        ditherRatio = snapshot.ditherRatio
        postAmount = snapshot.postAmount
    }

    private func clearMorphSession() {
        morphBaseline = nil
        morphPreview = nil
        isMorphing = false
        isComparingMorph = false
    }

    private func clamp(_ value: Float, to range: ClosedRange<Float>) -> Float {
        min(max(value, range.lowerBound), range.upperBound)
    }
}

struct EffectUniforms {
    var geometry: SIMD4<Float>
    var shape: SIMD4<Float>
    var motion: SIMD4<Float>
    var finishing: SIMD4<Float>
    var composite: SIMD4<Float>
}
