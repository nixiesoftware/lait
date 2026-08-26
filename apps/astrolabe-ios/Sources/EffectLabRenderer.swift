import MetalKit
import SwiftUI
import UIKit

struct EffectMetalView: UIViewRepresentable {
    @ObservedObject var model: EffectLabModel

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeUIView(context: Context) -> LabMTKView {
        guard let device = MTLCreateSystemDefaultDevice() else {
            preconditionFailure("Effect Lab requires Metal")
        }

        let view = LabMTKView(frame: .zero, device: device)
        view.colorPixelFormat = .bgra8Unorm
        view.framebufferOnly = true
        view.isPaused = false
        view.enableSetNeedsDisplay = false
        view.isMultipleTouchEnabled = true
        view.preferredFramesPerSecond = model.frameRate
        view.renderScale = CGFloat(model.renderScale)
        view.interactionModel = model
        context.coordinator.renderer = EffectLabRenderer(view: view, initial: model)
        return view
    }

    func updateUIView(_ view: LabMTKView, context: Context) {
        view.preferredFramesPerSecond = model.frameRate
        view.renderScale = CGFloat(model.renderScale)
        view.interactionModel = model
        context.coordinator.renderer?.update(from: model)
    }

    final class Coordinator {
        var renderer: EffectLabRenderer?
    }
}

final class LabMTKView: MTKView {
    weak var interactionModel: EffectLabModel?
    private var primaryTouch: UITouch?
    private var secondaryTouches = Set<ObjectIdentifier>()

    var renderScale: CGFloat = 0.5 {
        didSet { updateDrawableSize() }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        updateDrawableSize()
    }

    private func updateDrawableSize() {
        let displayScale = window?.screen.scale ?? 2
        drawableSize = CGSize(
            width: max(1, bounds.width * displayScale * renderScale),
            height: max(1, bounds.height * displayScale * renderScale)
        )
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        if primaryTouch == nil, let touch = touches.first {
            primaryTouch = touch
            interactionModel?.beginMorph(at: normalizedLocation(of: touch))
        }

        for touch in touches where touch !== primaryTouch {
            secondaryTouches.insert(ObjectIdentifier(touch))
        }
        if !secondaryTouches.isEmpty {
            interactionModel?.setMorphComparison(true)
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let primaryTouch, touches.contains(where: { $0 === primaryTouch }) else { return }
        interactionModel?.updateMorph(at: normalizedLocation(of: primaryTouch))
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        if let primaryTouch, touches.contains(where: { $0 === primaryTouch }) {
            if primaryTouch.tapCount >= 2 {
                interactionModel?.cancelMorph()
                interactionModel?.undoMorph()
            } else {
                interactionModel?.endMorph()
            }
            resetTouches()
            return
        }

        removeSecondaryTouches(touches)
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        if let primaryTouch, touches.contains(where: { $0 === primaryTouch }) {
            interactionModel?.cancelMorph()
            resetTouches()
            return
        }

        removeSecondaryTouches(touches)
    }

    private func removeSecondaryTouches(_ touches: Set<UITouch>) {
        for touch in touches {
            secondaryTouches.remove(ObjectIdentifier(touch))
        }
        if secondaryTouches.isEmpty {
            interactionModel?.setMorphComparison(false)
        }
    }

    private func resetTouches() {
        primaryTouch = nil
        secondaryTouches.removeAll()
    }

    private func normalizedLocation(of touch: UITouch) -> CGPoint {
        let point = touch.location(in: self)
        return CGPoint(
            x: min(max(point.x / max(bounds.width, 1), 0), 1),
            y: min(max(point.y / max(bounds.height, 1), 0), 1)
        )
    }
}

private struct RenderSettings {
    var family: Float
    var spectrum: Float
    var structure: Float
    var detail: Float
    var turbulence: Float
    var softness: Float
    var flow: Float
    var glow: Float
    var energy: Float
    var dither: Float
    var ditherRatio: Float
    var postProcess: Float
    var postAmount: Float
    var background: SIMD3<Float>
    var borderReach: Float
    var seed: Float
    var previewTime: Float
    var isAnimating: Bool
}

final class EffectLabRenderer: NSObject, MTKViewDelegate {
    private let commandQueue: any MTLCommandQueue
    private let pipeline: any MTLRenderPipelineState
    private var settings: RenderSettings
    private var animationTime: Float = 0
    private var lastFrameTime = CACurrentMediaTime()

    @MainActor
    init(view: MTKView, initial model: EffectLabModel) {
        guard let device = view.device,
              let commandQueue = device.makeCommandQueue()
        else {
            preconditionFailure("Effect Lab could not create its Metal command queue")
        }

        let library: any MTLLibrary
        do {
            // Runtime source compilation is deliberate for the development lab:
            // it avoids a workstation Metal-toolchain dependency and is the seam
            // a later Mac-to-iPhone shader hot reload can replace.
            library = try device.makeLibrary(source: EffectLabShaderSource.code, options: nil)
        } catch {
            preconditionFailure("Effect Lab shader compilation failed: \(error)")
        }
        guard let vertex = library.makeFunction(name: "effectLabVertex"),
              let fragment = library.makeFunction(name: "effectLabFragment")
        else {
            preconditionFailure("Effect Lab could not load its Metal functions")
        }

        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "Effect Lab"
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = fragment
        descriptor.colorAttachments[0].pixelFormat = view.colorPixelFormat

        do {
            pipeline = try device.makeRenderPipelineState(descriptor: descriptor)
        } catch {
            preconditionFailure("Effect Lab pipeline failed: \(error)")
        }
        self.commandQueue = commandQueue
        settings = Self.settings(from: model)
        super.init()
        view.delegate = self
    }

    @MainActor
    func update(from model: EffectLabModel) {
        settings = Self.settings(from: model)
    }

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

    func draw(in view: MTKView) {
        autoreleasepool {
            guard view.drawableSize.width > 0,
                  view.drawableSize.height > 0,
                  let pass = view.currentRenderPassDescriptor,
                  let drawable = view.currentDrawable,
                  let commandBuffer = commandQueue.makeCommandBuffer(),
                  let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: pass)
            else { return }

            let now = CACurrentMediaTime()
            let delta = min(Float(now - lastFrameTime), 0.05)
            lastFrameTime = now
            if settings.isAnimating {
                animationTime += delta
            }

            var uniforms = EffectUniforms(
                geometry: SIMD4(
                    Float(view.drawableSize.width),
                    Float(view.drawableSize.height),
                    settings.previewTime + animationTime,
                    settings.seed
                ),
                shape: SIMD4(
                    settings.structure,
                    settings.detail,
                    settings.turbulence,
                    settings.softness
                ),
                motion: SIMD4(
                    settings.flow,
                    settings.glow,
                    settings.energy,
                    settings.family + settings.spectrum * 10
                ),
                finishing: SIMD4(
                    settings.dither,
                    settings.ditherRatio,
                    settings.postProcess,
                    settings.postAmount
                ),
                composite: SIMD4(
                    settings.background.x,
                    settings.background.y,
                    settings.background.z,
                    settings.borderReach
                )
            )

            commandBuffer.label = "Effect Lab frame"
            encoder.label = "Effect Lab full-screen pass"
            encoder.setRenderPipelineState(pipeline)
            encoder.setFragmentBytes(&uniforms, length: MemoryLayout<EffectUniforms>.stride, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
            encoder.endEncoding()
            commandBuffer.present(drawable)
            commandBuffer.commit()
        }
    }

    @MainActor
    private static func settings(from model: EffectLabModel) -> RenderSettings {
        let resolved = UIColor(model.backgroundColor)
        var red: CGFloat = 0.91
        var green: CGFloat = 0.87
        var blue: CGFloat = 0.77
        var alpha: CGFloat = 1
        resolved.getRed(&red, green: &green, blue: &blue, alpha: &alpha)

        return RenderSettings(
            family: Float(model.family.rawValue),
            spectrum: Float(model.spectrum.rawValue),
            structure: model.structure,
            detail: model.detail,
            turbulence: model.turbulence,
            softness: model.softness,
            flow: model.flow,
            glow: model.glow,
            energy: model.energy,
            dither: Float(model.dither.rawValue),
            ditherRatio: model.ditherRatio,
            postProcess: Float(model.postProcess.rawValue),
            postAmount: model.postAmount,
            background: SIMD3(Float(red), Float(green), Float(blue)),
            borderReach: model.borderReach,
            seed: model.seed,
            previewTime: model.previewTime,
            isAnimating: model.isAnimating
        )
    }
}
