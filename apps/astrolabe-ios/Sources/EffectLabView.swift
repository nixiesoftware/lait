import SwiftUI

struct EffectLabView: View {
    @StateObject private var model = EffectLabModel()
    @State private var inspectorShown = false

    var body: some View {
        ZStack {
            EffectMetalView(model: model)
                .ignoresSafeArea()

            MorphGuideOverlay(model: model)
                .ignoresSafeArea()
                .allowsHitTesting(false)

            VStack(spacing: 12) {
                familyPicker
                Spacer()
                if inspectorShown {
                    inspector
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
                morphControls
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 8)
        }
        .navigationTitle("Effect Lab")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(.hidden, for: .navigationBar)
        .toolbarColorScheme(.dark, for: .navigationBar)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    model.isAnimating.toggle()
                } label: {
                    Label(model.isAnimating ? "Freeze" : "Animate", systemImage: model.isAnimating ? "pause.fill" : "play.fill")
                }

                Button {
                    withAnimation(.snappy) { inspectorShown.toggle() }
                } label: {
                    Label("Controls", systemImage: "slider.horizontal.3")
                }
            }
        }
        .preferredColorScheme(.dark)
    }

    private var morphControls: some View {
        VStack(spacing: 8) {
            Picker("Morph pad", selection: $model.morphPad) {
                ForEach(EffectMorphPad.allCases) { pad in
                    Text(pad.name).tag(pad)
                }
            }
            .pickerStyle(.segmented)

            HStack(spacing: 10) {
                HStack(spacing: 4) {
                    Text(model.morphPad.xLow)
                    Image(systemName: "arrow.left.and.right")
                    Text(model.morphPad.xHigh)
                }

                Spacer(minLength: 4)

                HStack(spacing: 4) {
                    Text(model.morphPad.yLow)
                    Image(systemName: "arrow.down.and.line.horizontal.and.arrow.up")
                    Text(model.morphPad.yHigh)
                }
            }
            .font(.caption2.weight(.medium))
            .foregroundStyle(.secondary)
        }
        .padding(10)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .stroke(.white.opacity(0.12), lineWidth: 0.5)
        }
    }

    private var familyPicker: some View {
        Picker("Technique", selection: $model.family) {
            ForEach(EffectFamily.allCases) { family in
                Text(family.name).tag(family)
            }
        }
        .pickerStyle(.segmented)
        .padding(8)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
    }

    private var inspector: some View {
        VStack(spacing: 0) {
            HStack {
                Menu {
                    ForEach(EffectLabModel.presets) { preset in
                        Button(preset.name) { model.apply(preset) }
                    }
                } label: {
                    Label("Presets", systemImage: "square.stack.3d.up")
                }

                Spacer()

                Button {
                    model.randomize()
                } label: {
                    Label("Remix", systemImage: "dice")
                }
            }
            .font(.subheadline.weight(.semibold))
            .padding(.horizontal, 16)
            .padding(.vertical, 12)

            Divider()

            ScrollView {
                VStack(spacing: 14) {
                    HStack {
                        Text("Background color")
                            .font(.caption.weight(.semibold))
                        Spacer()
                        ColorPicker(
                            "Background color",
                            selection: $model.backgroundColor,
                            supportsOpacity: false
                        )
                        .labelsHidden()
                    }
                    LabRatioSlider(
                        title: "Border precipice",
                        value: $model.borderReach,
                        range: 0.05 ... 0.40
                    )

                    VStack(spacing: 5) {
                        LinearGradient(
                            colors: model.spectrum.colors,
                            startPoint: .leading,
                            endPoint: .trailing
                        )
                        .frame(height: 8)
                        .clipShape(Capsule())

                        HStack {
                            Picker("Spectrum", selection: $model.spectrum) {
                                ForEach(EffectSpectrum.allCases) { spectrum in
                                    Text(spectrum.name).tag(spectrum)
                                }
                            }
                            .pickerStyle(.menu)

                            Spacer()
                            Text("LOW INTENSITY → HIGH REACTIVITY")
                        }
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.secondary)
                    }

                    LabSlider(title: "Structure", value: $model.structure, range: 0.5 ... 3)
                    LabSlider(title: "Detail", value: $model.detail, range: 0.15 ... 0.75)
                    LabSlider(title: "Turbulence", value: $model.turbulence, range: 0 ... 2.5)
                    LabSlider(title: "Softness", value: $model.softness, range: 0.15 ... 0.85)
                    LabSlider(title: "Flow", value: $model.flow, range: 0 ... 0.35)
                    LabSlider(title: "Glow", value: $model.glow, range: 0 ... 1)
                    LabSlider(title: "Energy preview", value: $model.energy, range: 0 ... 1)

                    Divider()

                    VStack(alignment: .leading, spacing: 8) {
                        Text("Dithering")
                            .font(.caption.weight(.semibold))
                        Picker("Dithering", selection: $model.dither) {
                            ForEach(EffectDither.allCases) { dither in
                                Text(dither.name).tag(dither)
                            }
                        }
                        .pickerStyle(.segmented)
                    }
                    if model.dither != .none {
                        LabRatioSlider(title: "Dither ratio", value: $model.ditherRatio)
                    }

                    VStack(alignment: .leading, spacing: 8) {
                        Text("Post-processing")
                            .font(.caption.weight(.semibold))
                        Picker("Post-processing", selection: $model.postProcess) {
                            ForEach(EffectPostProcess.allCases) { effect in
                                Text(effect.name).tag(effect)
                            }
                        }
                        .pickerStyle(.menu)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    if model.postProcess != .none {
                        LabSlider(title: "Post amount", value: $model.postAmount, range: 0 ... 1)
                    }

                    if !model.isAnimating {
                        LabSlider(title: "Time", value: $model.previewTime, range: 0 ... 30)
                    }

                    HStack {
                        Picker("Resolution", selection: $model.renderScale) {
                            Text("½×").tag(Float(0.5))
                            Text("¾×").tag(Float(0.75))
                            Text("1×").tag(Float(1.0))
                        }
                        .pickerStyle(.segmented)

                        Picker("FPS", selection: $model.frameRate) {
                            Text("15").tag(15)
                            Text("30").tag(30)
                            Text("60").tag(60)
                        }
                        .pickerStyle(.segmented)
                    }

                    HStack {
                        Text("Seed")
                        Spacer()
                        TextField("Seed", value: $model.seed, format: .number.precision(.fractionLength(0)))
                            .keyboardType(.numberPad)
                            .multilineTextAlignment(.trailing)
                            .frame(width: 90)
                            .textFieldStyle(.roundedBorder)
                    }
                    .font(.caption)
                }
                .padding(16)
            }
            .frame(maxHeight: 360)
        }
        .foregroundStyle(.primary)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: 22, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 22, style: .continuous)
                .stroke(.white.opacity(0.12), lineWidth: 0.5)
        }
    }
}

private struct MorphGuideOverlay: View {
    @ObservedObject var model: EffectLabModel

    var body: some View {
        GeometryReader { proxy in
            if model.isMorphing {
                let start = CGPoint(
                    x: model.morphStart.x * proxy.size.width,
                    y: model.morphStart.y * proxy.size.height
                )
                let position = CGPoint(
                    x: model.morphPosition.x * proxy.size.width,
                    y: model.morphPosition.y * proxy.size.height
                )

                ZStack {
                    Path { path in
                        path.move(to: CGPoint(x: 0, y: position.y))
                        path.addLine(to: CGPoint(x: proxy.size.width, y: position.y))
                        path.move(to: CGPoint(x: position.x, y: 0))
                        path.addLine(to: CGPoint(x: position.x, y: proxy.size.height))
                    }
                    .stroke(
                        .white.opacity(0.18),
                        style: StrokeStyle(lineWidth: 0.75, dash: [4, 7])
                    )

                    Path { path in
                        path.move(to: start)
                        path.addLine(to: position)
                    }
                    .stroke(.white.opacity(0.72), lineWidth: 1.5)

                    Circle()
                        .fill(.white.opacity(0.18))
                        .overlay {
                            Circle().stroke(.white.opacity(0.9), lineWidth: 1.5)
                        }
                        .frame(width: 30, height: 30)
                        .position(position)

                    VStack(spacing: 3) {
                        if model.isComparingMorph {
                            Text("BEFORE")
                                .font(.caption2.weight(.black))
                                .foregroundStyle(.yellow)
                        }
                        Text("\(model.morphPad.xName)  \(formatted(model.morphXValue))")
                        Text("\(model.morphPad.yName)  \(formatted(model.morphYValue))")
                    }
                    .font(.caption.monospacedDigit().weight(.semibold))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(.black.opacity(0.58), in: Capsule())
                    .position(
                        x: min(max(position.x, 90), proxy.size.width - 90),
                        y: min(max(position.y - 54, 80), proxy.size.height - 80)
                    )
                }
            }
        }
    }

    private func formatted(_ value: Float) -> String {
        value.formatted(.number.precision(.fractionLength(2)))
    }
}

private struct LabSlider: View {
    let title: String
    @Binding var value: Float
    let range: ClosedRange<Float>

    var body: some View {
        VStack(spacing: 4) {
            HStack {
                Text(title)
                Spacer()
                Text(value.formatted(.number.precision(.fractionLength(2))))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            .font(.caption)
            Slider(value: $value, in: range)
        }
    }
}

private struct LabRatioSlider: View {
    let title: String
    @Binding var value: Float
    var range: ClosedRange<Float> = 0 ... 1

    var body: some View {
        VStack(spacing: 4) {
            HStack {
                Text(title)
                Spacer()
                Text(value, format: .percent.precision(.fractionLength(0)))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            .font(.caption)
            Slider(value: $value, in: range)
        }
    }
}
