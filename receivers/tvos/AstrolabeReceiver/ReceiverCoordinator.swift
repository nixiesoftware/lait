import Foundation
import Combine
import UIKit

@MainActor
final class ReceiverCoordinator: ObservableObject {
    @Published var screen: ReceiverScreen = .booting
    @Published var transportState = "connecting"
    @Published var sourceState = "none"
    @Published var stale = false

    private let bootstrap: ReceiverBootstrap?
    private let bootstrapFailure: Error?
    private let origin: URL
    private let transport: BoundedTransport
    private let vault = KeychainVault()
    private var credential: ReceiverCredential?
    private var challenge: String?
    private var program: DisplayProgram?
    private var stage: [String: StagedFrame] = [:]
    private var playbackStarted = ProcessInfo.processInfo.systemUptime
    private var elapsedBase = 0
    private var lastProgramDelivery = ProcessInfo.processInfo.systemUptime
    private var lastHealth = ProcessInfo.processInfo.systemUptime
    private var playbackTask: Task<Void, Never>?
    private var staleTask: Task<Void, Never>?
    private var pollingTask: Task<Void, Never>?
    private var programTask: Task<Void, Never>?
    private var started = false

    init() {
        do {
            let bootstrap = try ReceiverBootstrap.load()
            self.bootstrap = bootstrap
            bootstrapFailure = nil
            origin = bootstrap.trust.origin
            transport = BoundedTransport(trust: bootstrap.trust)
        } catch {
            bootstrap = nil
            bootstrapFailure = error
            let refused = URL(string: "https://bootstrap.invalid")!
            origin = refused
            transport = BoundedTransport(trust: .webPKI(origin: refused))
        }
    }

    func start() {
        guard !started else { return }
        started = true
        Task {
            do {
                screen = .booting
                if let bootstrapFailure { throw bootstrapFailure }
                credential = try vault.load()
                if let credential, credential.origin != origin.absoluteString {
                    throw DisplayProtocolV1.refusal("coordinator_changed", "stored identity belongs to another coordinator")
                }
                guard let credential else { try await beginPairing(); return }
                switch credential.mode {
                case "pairing":
                    presentPairing()
                    if credential.userConfirmed == true { pollPairing() }
                case "enrolling":
                    try await finishEnrollment()
                    runProgramLoop()
                case "paired": runProgramLoop()
                default: throw DisplayProtocolV1.refusal("protected_storage", "unknown credential mode")
                }
            } catch { fail(error) }
        }
    }

    func retry() {
        let previousPolling = pollingTask
        playbackTask?.cancel()
        staleTask?.cancel()
        pollingTask?.cancel()
        programTask?.cancel()
        started = false
        Task {
            if let previousPolling { await previousPolling.value }
            start()
        }
    }

    func confirmPairing() {
        guard var credential, credential.mode == "pairing", credential.userConfirmed != true else { return }
        do {
            credential.userConfirmed = true
            try vault.save(credential)
            self.credential = credential
            presentPairing()
            pollPairing()
        } catch { fail(error) }
    }

    func cancelPairing() {
        guard credential?.mode == "pairing" else { return }
        do {
            try vault.clear()
            credential = nil
            let previousPolling = pollingTask
            previousPolling?.cancel()
            Task {
                if let previousPolling { await previousPolling.value }
                do { try await beginPairing() } catch { fail(error) }
            }
        } catch { fail(error) }
    }

    private func fail(_ error: Error) {
        screen = .message(
            eyebrow: "Receiver refused",
            title: "Astrolabe cannot continue safely",
            body: error.localizedDescription,
            retry: true
        )
    }

    private func presentPairing() {
        guard let credential, let words = credential.phrase, let fingerprint = credential.fingerprint else { return }
        screen = .pairing(words: words, fingerprint: fingerprint, confirmed: credential.userConfirmed == true)
    }

    private func beginPairing() async throws {
        let instance = try await publicJSON(path: "/head/v1/instance", method: "GET", object: nil)
        try StrictJSON.fields(instance, exactly: ["protocol_major", "instance", "label", "trust"], name: "instance")
        guard StrictJSON.int(instance["protocol_major"]) == 1,
              DisplayProtocolV1.isHex(instance["instance"] as? String, count: 32),
              let label = instance["label"] as? String,
              (1...96).contains(label.lengthOfBytes(using: .utf8)),
              label.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }),
              let trust = instance["trust"] as? [String: Any]
        else { throw DisplayProtocolV1.refusal("unsupported", "coordinator instance") }
        let expectedTrust = bootstrap?.trust
        let expectedFields: Set<String> = expectedTrust?.fingerprint == nil
            ? ["kind", "origin"] : ["kind", "origin", "sha256"]
        try StrictJSON.fields(trust, exactly: expectedFields, name: "trust")
        guard trust["origin"] as? String == origin.absoluteString,
              (expectedTrust?.fingerprint == nil
                ? trust["kind"] as? String == "web_pki_origin"
                : trust["kind"] as? String == "pinned_certificate"
                    && trust["sha256"] as? String == expectedTrust?.fingerprint)
        else {
            throw DisplayProtocolV1.refusal("unsupported_trust", "coordinator does not match bootstrap")
        }

        let nonce = try DisplayProtocolV1.randomHex()
        let pollKey = try DisplayProtocolV1.randomHex()
        let response = try await publicJSON(path: "/head/v1/pairings", method: "POST", object: [
            "protocol_major": 1,
            "receiver_nonce": nonce,
            "poll_key": pollKey,
            "rendezvous": (bootstrap?.rendezvous as Any?) ?? NSNull(),
            "capabilities": capabilities(),
        ])
        try StrictJSON.fields(response, exactly: ["protocol_major", "pairing", "expires_in_ms", "confirmation_phrase", "coordinator_fingerprint"], name: "pairing")
        guard let pairing = response["pairing"] as? String,
              let fingerprint = response["coordinator_fingerprint"] as? String,
              let phrase = response["confirmation_phrase"] as? [String],
              let lifetime = StrictJSON.int(response["expires_in_ms"]), (1...600_000).contains(lifetime),
              DisplayProtocolV1.isHex(pairing, count: 32), DisplayProtocolV1.isHex(fingerprint, count: 64),
              phrase == (try DisplayProtocolV1.confirmationPhrase(fingerprint: fingerprint, pairing: pairing, nonce: nonce))
        else { throw DisplayProtocolV1.refusal("pairing_integrity", "confirmation phrase") }
        if let pinned = bootstrap?.trust.fingerprint, fingerprint != pinned {
            throw DisplayProtocolV1.refusal("pairing_integrity", "certificate bootstrap")
        }

        let credential = ReceiverCredential(
            mode: "pairing", origin: origin.absoluteString, pairing: pairing,
            receiverNonce: nonce, pollKey: pollKey, fingerprint: fingerprint,
            phrase: phrase, userConfirmed: false, device: nil, proofKey: nil,
            enrollmentChallenge: nil
        )
        try vault.save(credential)
        self.credential = credential
        presentPairing()
    }

    private func pollPairing() {
        guard pollingTask == nil else { return }
        pollingTask = Task {
            defer { pollingTask = nil }
            do {
                while !Task.isCancelled, let current = credential, current.mode == "pairing" {
                    guard let pairing = current.pairing, let pollKey = current.pollKey else {
                        throw DisplayProtocolV1.refusal("protected_storage", "pairing credential")
                    }
                    let response = try await publicJSON(path: "/head/v1/pairings/status", method: "POST", object: [
                        "protocol_major": 1,
                        "pairing": pairing,
                        "proof": try DisplayProtocolV1.pairingStatusTag(key: pollKey, pairing: pairing),
                    ])
                    try Task.checkCancellation()
                    guard let kind = response["kind"] as? String else { throw DisplayProtocolV1.refusal("invalid_pairing", "status") }
                    if kind == "pending" {
                        try StrictJSON.fields(response, exactly: ["kind", "retry_after_ms"], name: "pending pairing")
                        guard let interval = StrictJSON.int(response["retry_after_ms"]), (1...60_000).contains(interval) else {
                            throw DisplayProtocolV1.refusal("invalid_pairing", "retry interval")
                        }
                        let delay = max(interval, 1_000)
                        try await Task.sleep(for: .milliseconds(delay))
                    } else if kind == "approved" {
                        try StrictJSON.fields(response, exactly: ["kind", "device", "proof_key", "enrollment_challenge"], name: "approved pairing")
                        guard let device = response["device"] as? String,
                              let proofKey = response["proof_key"] as? String,
                              let enrollment = response["enrollment_challenge"] as? String,
                              DisplayProtocolV1.isHex(device, count: 32),
                              DisplayProtocolV1.isHex(proofKey, count: 64),
                              DisplayProtocolV1.isHex(enrollment, count: 64)
                        else { throw DisplayProtocolV1.refusal("invalid_pairing", "approved credential") }
                        let enrolling = ReceiverCredential(
                            mode: "enrolling", origin: origin.absoluteString, pairing: pairing,
                            receiverNonce: nil, pollKey: nil, fingerprint: nil, phrase: nil,
                            userConfirmed: nil, device: device, proofKey: proofKey,
                            enrollmentChallenge: enrollment
                        )
                        try vault.save(enrolling)
                        credential = enrolling
                        try await finishEnrollment()
                        runProgramLoop()
                        return
                    } else if kind == "rejected" {
                        try StrictJSON.fields(response, exactly: ["kind", "reason"], name: "rejected pairing")
                        guard let reason = response["reason"] as? String,
                              ["user_rejected", "controller_unavailable", "policy_refused", "fingerprint_mismatch"].contains(reason)
                        else { throw DisplayProtocolV1.refusal("invalid_pairing", "rejection reason") }
                        screen = .message(eyebrow: "Pairing stopped", title: "Pairing was not approved", body: "Begin a new trust ceremony on this television.", retry: true)
                        return
                    } else if kind == "expired" {
                        try StrictJSON.fields(response, exactly: ["kind"], name: "expired pairing")
                        screen = .message(eyebrow: "Pairing stopped", title: "Pairing was not approved", body: "Begin a new trust ceremony on this television.", retry: true)
                        return
                    } else { throw DisplayProtocolV1.refusal("invalid_pairing", "unknown status") }
                }
            } catch is CancellationError {
            } catch { fail(error) }
        }
    }

    private func finishEnrollment() async throws {
        guard let credential, let pairing = credential.pairing, let device = credential.device,
              let proofKey = credential.proofKey, let enrollment = credential.enrollmentChallenge
        else { throw DisplayProtocolV1.refusal("protected_storage", "enrollment credential") }
        let response = try await publicJSON(path: "/head/v1/pairings/complete", method: "POST", object: [
            "protocol_major": 1,
            "pairing": pairing,
            "device": device,
            "enrollment_challenge": enrollment,
            "proof": try DisplayProtocolV1.pairingCompleteTag(key: proofKey, pairing: pairing, device: device, challenge: enrollment),
        ])
        try StrictJSON.fields(response, exactly: ["kind", "device", "next_challenge"], name: "pairing completion")
        guard let kind = response["kind"] as? String, ["enrolled", "already_enrolled"].contains(kind),
              response["device"] as? String == device,
              let next = response["next_challenge"] as? String, DisplayProtocolV1.isHex(next, count: 64)
        else { throw DisplayProtocolV1.refusal("pairing_integrity", "enrollment completion") }
        challenge = next
        let paired = ReceiverCredential(
            mode: "paired", origin: origin.absoluteString, pairing: nil,
            receiverNonce: nil, pollKey: nil, fingerprint: nil, phrase: nil,
            userConfirmed: nil, device: device, proofKey: proofKey,
            enrollmentChallenge: nil
        )
        try vault.save(paired)
        self.credential = paired
    }

    private func runProgramLoop() {
        screen = .message(eyebrow: "Astrolabe Display", title: "Connecting…", body: "Authenticating this receiver and requesting its complete current program.", retry: false)
        startStaleMonitor()
        programTask?.cancel()
        programTask = Task {
            var backoff = 1_000
            var capabilitiesAccepted = false
            while credential?.mode == "paired", !Task.isCancelled {
                do {
                    if !capabilitiesAccepted {
                        let accepted = try await authorizedJSON(route: "capabilities", method: "POST", path: "/head/v1/capabilities", object: capabilities())
                        try StrictJSON.fields(accepted, exactly: ["kind"], name: "capabilities")
                        guard accepted["kind"] as? String == "accepted" else { throw DisplayProtocolV1.refusal("capability_refused", "coordinator") }
                        capabilitiesAccepted = true
                    }
                    let response: [String: Any]
                    if program == nil {
                        response = try await authorizedJSON(route: "program_snapshot", method: "GET", path: "/head/v1/program", object: nil)
                    } else {
                        response = try await authorizedJSON(
                            route: "program_changes", method: "GET", path: "/head/v1/program/changes",
                            object: nil, overrides: .init(waitMs: 25_000)
                        )
                    }
                    try await handleProgram(response)
                    guard credential?.mode == "paired" else { return }
                    if program != nil, ProcessInfo.processInfo.systemUptime - lastHealth >= 30 {
                        try await reportHealth()
                    }
                    backoff = 1_000
                    if program == nil { try await Task.sleep(for: .seconds(5)) }
                } catch is CancellationError { return }
                catch {
                    transportState = "offline"
                    try? await Task.sleep(for: .milliseconds(backoff))
                    backoff = min(backoff * 2, 30_000)
                }
            }
        }
    }

    private func handleProgram(_ response: [String: Any]) async throws {
        guard let kind = response["kind"] as? String else { throw DisplayProtocolV1.refusal("invalid_program_response", "missing outcome") }
        switch kind {
        case "snapshot":
            try StrictJSON.fields(response, exactly: ["kind", "program"], name: "snapshot")
            guard let object = response["program"] as? [String: Any] else { throw DisplayProtocolV1.refusal("invalid_shape", "program") }
            try StrictJSON.program(object)
            let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
            let next = try JSONDecoder().decode(DisplayProgram.self, from: data)
            try DisplayProtocolV1.verifyProgram(next)
            if program?.revision == next.revision {
                program = next
                guard adoptCursor(next.playback, programDelivery: true) else { throw DisplayProtocolV1.refusal("invalid_cursor", "snapshot") }
                return
            }
            let nextStage = try await stageProgram(next)
            stage = nextStage
            program = next
            guard adoptCursor(next.playback, programDelivery: true) else { throw DisplayProtocolV1.refusal("invalid_cursor", "snapshot") }
        case "no_change":
            try StrictJSON.fields(response, exactly: ["kind", "revision", "playback"], name: "no change")
            guard response["revision"] as? String == program?.revision,
                  let object = response["playback"] as? [String: Any]
            else { throw DisplayProtocolV1.refusal("invalid_revision", "no-change cursor") }
            try StrictJSON.fields(object, exactly: ["current_index", "elapsed_ms", "cycle"], name: "cursor")
            let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
            guard adoptCursor(try JSONDecoder().decode(PlaybackCursor.self, from: data), programDelivery: true) else {
                throw DisplayProtocolV1.refusal("invalid_cursor", "no-change cursor")
            }
        case "unassigned":
            try StrictJSON.fields(response, exactly: ["kind"], name: "unassigned")
            clearProgram()
            if let device = credential?.device { screen = .unassigned(device: device) }
            lastProgramDelivery = ProcessInfo.processInfo.systemUptime
        case "reset":
            try StrictJSON.fields(response, exactly: ["kind", "reason"], name: "reset")
            clearProgram()
        case "revoked":
            try StrictJSON.fields(response, exactly: ["kind"], name: "revoked")
            clearProgram()
            challenge = nil
            credential = nil
            screen = .message(eyebrow: "Receiver access", title: "This display was revoked", body: "Staged content has been cleared. Re-enroll it if access should return.", retry: false)
        case "re_pair":
            try StrictJSON.fields(response, exactly: ["kind"], name: "re-pair")
            clearProgram()
            try vault.clear()
            credential = nil
            challenge = nil
            screen = .message(eyebrow: "Trust changed", title: "Pairing is required again", body: "Coordinator trust or receiver identity changed.", retry: true)
        default: throw DisplayProtocolV1.refusal("unsupported", "program outcome")
        }
    }

    private func stageProgram(_ program: DisplayProgram) async throws -> [String: StagedFrame] {
        var result: [String: StagedFrame] = [:]
        var total = 0
        for item in program.items where item.scene.kind == "frame" {
            guard let asset = item.scene.asset else { throw DisplayProtocolV1.refusal("invalid_shape", "frame asset") }
            if result[asset.id] != nil { continue }
            total += asset.encodedLen
            guard total <= 50_331_648 else { throw DisplayProtocolV1.refusal("bound_exceeded", "staged bytes") }
            result[asset.id] = try await authorizedAsset(asset, program: program)
        }
        return result
    }

    private func authorizedAsset(_ asset: DisplayAsset, program: DisplayProgram) async throws -> StagedFrame {
        let response = try await authorized(
            route: "asset", method: "GET", path: "/head/v1/assets/\(asset.id)", body: Data(),
            maximumBytes: asset.encodedLen,
            overrides: .init(assignment: program.assignment, program: program.program, revision: program.revision, clearPlayback: true, asset: asset.id)
        )
        guard response.status == 200, response.body.count == asset.encodedLen,
              DisplayProtocolV1.sha256(response.body) == asset.sha256,
              response.headers.value(forHTTPHeaderField: "Content-Type")?.lowercased() == mediaType(asset.mediaType),
              let image = UIImage(data: response.body), let cgImage = image.cgImage,
              cgImage.width == asset.width, cgImage.height == asset.height
        else { throw DisplayProtocolV1.refusal("asset_integrity", "frame bytes or dimensions") }
        return StagedFrame(image: image, encodedBytes: response.body.count, digest: asset.sha256)
    }

    @discardableResult
    private func adoptCursor(_ cursor: PlaybackCursor, programDelivery: Bool = false) -> Bool {
        guard var program, program.items.indices.contains(cursor.currentIndex), cursor.elapsedMs >= 0,
              cursor.cycle == program.playback.cycle,
              program.items[cursor.currentIndex].durationMs.map({ cursor.elapsedMs < $0 }) ?? true
        else { return false }
        program.playback = cursor
        self.program = program
        if programDelivery { lastProgramDelivery = ProcessInfo.processInfo.systemUptime }
        elapsedBase = cursor.elapsedMs
        playbackStarted = ProcessInfo.processInfo.systemUptime
        renderCurrent()
        return true
    }

    private func currentPlayback() -> PlaybackCursor? {
        guard let program else { return nil }
        let additional = max(0, Int((ProcessInfo.processInfo.systemUptime - playbackStarted) * 1_000))
        return PlaybackCursor(currentIndex: program.playback.currentIndex, elapsedMs: min(Int(UInt32.max), elapsedBase + additional), cycle: program.playback.cycle)
    }

    private func renderCurrent() {
        playbackTask?.cancel()
        guard let program, let cursor = currentPlayback() else { return }
        let item = program.items[cursor.currentIndex]
        sourceState = sourceKind(program: program, item: item)
        stale = Int((ProcessInfo.processInfo.systemUptime - lastProgramDelivery) * 1_000) >= program.freshness.staleAfterMs
        if stale, program.freshness.onStale == "blank" {
            screen = .message(eyebrow: "Receiver-owned state", title: "Coordinator unavailable", body: "The assigned content is no longer eligible to remain on screen.", retry: false)
            return
        }
        if item.scene.kind == "frame", let asset = item.scene.asset, let frame = stage[asset.id] {
            screen = .frame(frame.image, summary: item.spokenSummary)
        } else if item.scene.kind == "blank" {
            screen = .message(eyebrow: "Receiver-owned state", title: "Assigned blank state", body: item.scene.reason ?? "unsupported", retry: false)
        } else {
            screen = .message(eyebrow: "Receiver refused", title: "Program unsupported", body: "This receiver cannot safely render the assigned scene.", retry: false)
        }
        if let duration = item.durationMs {
            let remaining = max(0, duration - cursor.elapsedMs)
            playbackTask = Task {
                try? await Task.sleep(for: .milliseconds(remaining))
                if !Task.isCancelled { advancePlayback() }
            }
        }
    }

    private func advancePlayback() {
        guard var program else { return }
        var next = program.playback.currentIndex + 1
        if next >= program.items.count {
            switch program.playback.cycle {
            case "loop": next = 0
            case "blank_at_end":
                screen = .message(eyebrow: "Program boundary", title: "Program complete", body: "Astrolabe is waiting for a newer assigned program.", retry: false)
                return
            case "poll_at_end": clearProgram(); return
            default: return
            }
        }
        program.playback = PlaybackCursor(currentIndex: next, elapsedMs: 0, cycle: program.playback.cycle)
        self.program = program
        elapsedBase = 0
        playbackStarted = ProcessInfo.processInfo.systemUptime
        renderCurrent()
    }

    private func startStaleMonitor() {
        staleTask?.cancel()
        staleTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard let program else { continue }
                stale = Int((ProcessInfo.processInfo.systemUptime - lastProgramDelivery) * 1_000) >= program.freshness.staleAfterMs
                if stale, program.freshness.onStale == "blank" {
                    screen = .message(eyebrow: "Receiver-owned state", title: "Coordinator unavailable", body: "The assigned content is no longer eligible to remain on screen.", retry: false)
                }
            }
        }
    }

    private func clearProgram() {
        playbackTask?.cancel()
        program = nil
        stage.removeAll()
        sourceState = "none"
        stale = false
    }

    private struct ContextOverrides {
        var assignment: String?
        var program: String?
        var revision: String?
        var waitMs: Int?
        var clearPlayback = false
        var asset: String?
    }

    private func authorizedJSON(
        route: String, method: String, path: String, object: [String: Any]?, overrides: ContextOverrides = .init()
    ) async throws -> [String: Any] {
        let body = try object.map { try JSONSerialization.data(withJSONObject: $0, options: [.sortedKeys, .withoutEscapingSlashes]) } ?? Data()
        let response = try await authorized(route: route, method: method, path: path, body: body, maximumBytes: 65_536, overrides: overrides)
        guard (200..<300).contains(response.status) else { try handleAPIError(response); throw DisplayProtocolV1.refusal("coordinator_refused", "HTTP \(response.status)") }
        return try StrictJSON.object(response.body, name: "authenticated response")
    }

    private func authorized(
        route: String, method: String, path: String, body: Data, maximumBytes: Int, overrides: ContextOverrides
    ) async throws -> BoundedHTTPResponse {
        try await ensureChallenge()
        guard let credential, let device = credential.device, let proofKey = credential.proofKey, let challenge else {
            throw DisplayProtocolV1.refusal("not_enrolled", "receiver credential")
        }
        let cursor = currentPlayback()
        let context = DisplayProtocolV1.RequestContext(
            method: method, route: route, device: device,
            assignment: overrides.assignment ?? program?.assignment,
            program: overrides.program ?? program?.program,
            revision: overrides.revision ?? program?.revision,
            currentItem: overrides.clearPlayback ? nil : cursor.map { program?.items[$0.currentIndex].id } ?? nil,
            elapsedMs: overrides.clearPlayback ? nil : cursor?.elapsedMs,
            waitMs: overrides.waitMs, asset: overrides.asset,
            rangeStart: nil, rangeLength: nil, challenge: challenge,
            bodySHA256: DisplayProtocolV1.sha256(body)
        )
        var request = URLRequest(url: endpoint(path))
        request.httpMethod = method
        if !body.isEmpty { request.httpBody = body; request.setValue("application/json; charset=utf-8", forHTTPHeaderField: "Content-Type") }
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        applyHeaders(context, tag: try DisplayProtocolV1.requestTag(key: proofKey, context: context), to: &request)
        self.challenge = nil
        do {
            let response = try await transport.send(request, maximumBytes: maximumBytes)
            guard let next = response.headers.value(forHTTPHeaderField: "X-Astrolabe-Next-Challenge"),
                  DisplayProtocolV1.isHex(next, count: 64)
            else { throw DisplayProtocolV1.refusal("invalid_challenge", "authenticated response") }
            self.challenge = next
            transportState = "online"
            return response
        } catch {
            self.challenge = nil
            transportState = "offline"
            throw error
        }
    }

    private func ensureChallenge() async throws {
        if challenge != nil { return }
        guard let device = credential?.device else { throw DisplayProtocolV1.refusal("not_enrolled", "device") }
        let response = try await publicJSON(path: "/head/v1/challenges", method: "POST", object: ["protocol_major": 1, "device": device])
        try StrictJSON.fields(response, exactly: ["protocol_major", "challenge", "expires_in_ms"], name: "challenge")
        guard let challenge = response["challenge"] as? String, DisplayProtocolV1.isHex(challenge, count: 64),
              let lifetime = StrictJSON.int(response["expires_in_ms"]), (1...120_000).contains(lifetime)
        else { throw DisplayProtocolV1.refusal("invalid_challenge", "challenge response") }
        self.challenge = challenge
    }

    private func publicJSON(path: String, method: String, object: [String: Any]?) async throws -> [String: Any] {
        var request = URLRequest(url: endpoint(path))
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let object {
            request.httpBody = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes])
            request.setValue("application/json; charset=utf-8", forHTTPHeaderField: "Content-Type")
        }
        let response = try await transport.send(request, maximumBytes: 65_536)
        guard (200..<300).contains(response.status) else { throw DisplayProtocolV1.refusal("coordinator_refused", "HTTP \(response.status)") }
        return try StrictJSON.object(response.body, name: "public response")
    }

    private func endpoint(_ path: String) -> URL {
        precondition(path.hasPrefix("/head/v1/") || path == "/head/v1/instance")
        return URL(string: path, relativeTo: origin)!.absoluteURL
    }

    private func applyHeaders(_ context: DisplayProtocolV1.RequestContext, tag: String, to request: inout URLRequest) {
        let required = [
            "Authorization": "Astrolabe-HMAC \(tag)",
            "X-Astrolabe-Protocol-Major": "1",
            "X-Astrolabe-Route": context.route,
            "X-Astrolabe-Device": context.device,
            "X-Astrolabe-Challenge": context.challenge,
            "X-Astrolabe-Body-SHA256": context.bodySHA256,
        ]
        required.forEach { request.setValue($1, forHTTPHeaderField: $0) }
        request.setOptional(context.assignment, header: "X-Astrolabe-Assignment")
        request.setOptional(context.program, header: "X-Astrolabe-Program")
        request.setOptional(context.revision, header: "X-Astrolabe-Revision")
        request.setOptional(context.currentItem, header: "X-Astrolabe-Current-Item")
        request.setOptional(context.elapsedMs.map(String.init), header: "X-Astrolabe-Elapsed-Ms")
        request.setOptional(context.waitMs.map(String.init), header: "X-Astrolabe-Wait-Ms")
        request.setOptional(context.asset, header: "X-Astrolabe-Asset")
    }

    private func handleAPIError(_ response: BoundedHTTPResponse) throws {
        let body = try StrictJSON.object(response.body, name: "API error")
        try StrictJSON.fields(body, exactly: ["protocol_major", "code", "retry_after_ms", "next_challenge"], name: "API error")
        let codes = [
            "invalid_request", "authentication_failed", "challenge_expired", "challenge_consumed",
            "not_enrolled", "unassigned", "revoked", "re_pair_required", "unsupported_protocol",
            "bound_exceeded", "temporarily_unavailable",
        ]
        guard StrictJSON.int(body["protocol_major"]) == 1,
              let code = body["code"] as? String, codes.contains(code),
              body["retry_after_ms"] is NSNull || StrictJSON.int(body["retry_after_ms"]).map({ (1...60_000).contains($0) }) == true,
              body["next_challenge"] is NSNull || DisplayProtocolV1.isHex(body["next_challenge"] as? String, count: 64)
        else { throw DisplayProtocolV1.refusal("invalid_api_error", "coordinator") }
        if code == "revoked" {
            clearProgram()
            credential = nil
            screen = .message(eyebrow: "Receiver access", title: "This display was revoked", body: "Staged content has been cleared.", retry: false)
        } else if code == "re_pair_required" {
            clearProgram()
            try vault.clear()
            credential = nil
            screen = .message(eyebrow: "Trust changed", title: "Pairing is required again", body: "Coordinator trust changed.", retry: true)
        }
    }

    private func capabilities() -> [String: Any] {
        let bounds = UIScreen.main.bounds
        return [
            "protocol_major": 1, "platform": "tvos", "build": "astrolabe-tvos/0.1.0",
            "viewport": ["width": min(Int(bounds.width), 4096), "height": min(Int(bounds.height), 2160), "scale_milli": 1000],
            "image_types": ["image_jpeg", "image_png", "image_webp"],
            "max_asset_bytes": 16_777_216, "max_staged_bytes": 50_331_648,
            "max_program_items": 16, "max_staging_horizon_ms": 86_400_000,
            "locale": String(Locale.current.identifier.prefix(35)),
            "accessibility": ["native_screen_reader": true, "spoken_summary": true, "captions": false, "audio_description": false],
            "playback": ["tier": "frame", "sync_class": "boundary", "rate_control_probed": false, "latency_class": "snapshot", "health_granularity": "full"],
        ]
    }

    private func reportHealth() async throws {
        guard let program, let cursor = currentPlayback() else { return }
        let item = program.items[cursor.currentIndex]
        let displayed: Any
        if let asset = item.scene.asset {
            displayed = ["id": asset.id, "sha256": asset.sha256]
        } else {
            displayed = NSNull()
        }
        let stagedBytes = stage.values.reduce(0) { $0 + $1.encodedBytes }
        let response = try await authorizedJSON(route: "health", method: "POST", path: "/head/v1/health", object: [
            "protocol_major": 1,
            "platform": "tvos",
            "build": "astrolabe-tvos/0.1.0",
            "revision": program.revision,
            "current_item": item.id,
            "elapsed_ms": cursor.elapsedMs,
            "last_displayed_asset": displayed,
            "connection": "online",
            "playback": item.scene.kind == "frame" ? "displaying" : "blank",
            "last_error": "none",
            "staged_items": stage.count,
            "staged_bytes": stagedBytes,
            "decode_latency": "unobserved",
            "swap_latency": "unobserved",
            "drift_residual_ms": 0,
            "correction_events": 0,
            "pipeline_unobservable": false,
        ])
        try StrictJSON.fields(response, exactly: ["kind"], name: "health")
        guard response["kind"] as? String == "accepted" else { throw DisplayProtocolV1.refusal("health_refused", "coordinator") }
        lastHealth = ProcessInfo.processInfo.systemUptime
    }

    private func sourceKind(program: DisplayProgram, item: DisplayProgramItem) -> String {
        if program.programState.kind == "unavailable" || item.sourceState.kind == "unavailable" { return "unavailable" }
        if program.programState.kind == "partial" || item.sourceState.kind == "partial" { return "partial" }
        return "current"
    }

    private func mediaType(_ value: String) -> String {
        ["image_png": "image/png", "image_jpeg": "image/jpeg", "image_webp": "image/webp"][value] ?? "application/octet-stream"
    }
}

private extension URLRequest {
    mutating func setOptional(_ value: String?, header: String) {
        if let value { setValue(value, forHTTPHeaderField: header) }
    }
}

enum StrictJSON {
    static func object(_ data: Data, name: String) throws -> [String: Any] {
        guard !data.isEmpty, let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw DisplayProtocolV1.refusal("invalid_shape", name)
        }
        return object
    }

    static func fields(_ object: [String: Any], exactly expected: Set<String>, name: String) throws {
        guard Set(object.keys) == expected else { throw DisplayProtocolV1.refusal("unknown_field", name) }
    }

    static func int(_ value: Any?) -> Int? {
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let result = number.intValue
        return number.doubleValue == Double(result) ? result : nil
    }

    static func program(_ object: [String: Any]) throws {
        try fields(object, exactly: ["protocol_major", "assignment", "program", "revision", "program_state", "freshness", "playback", "items"], name: "program")
        try source(object["program_state"], name: "program source")
        guard let freshness = object["freshness"] as? [String: Any], let playback = object["playback"] as? [String: Any],
              let items = object["items"] as? [[String: Any]]
        else { throw DisplayProtocolV1.refusal("invalid_shape", "program members") }
        try fields(freshness, exactly: ["stale_after_ms", "on_stale"], name: "freshness")
        try fields(playback, exactly: ["current_index", "elapsed_ms", "cycle"], name: "playback")
        for item in items {
            try fields(item, exactly: ["id", "duration_ms", "source_state", "scene", "spoken_summary"], name: "item")
            try source(item["source_state"], name: "item source")
            guard let scene = item["scene"] as? [String: Any], let kind = scene["kind"] as? String else { throw DisplayProtocolV1.refusal("invalid_shape", "scene") }
            if kind == "frame" {
                try fields(scene, exactly: ["kind", "asset"], name: "frame")
                guard let asset = scene["asset"] as? [String: Any] else { throw DisplayProtocolV1.refusal("invalid_shape", "asset") }
                try fields(asset, exactly: ["id", "media_type", "encoded_len", "sha256", "width", "height"], name: "asset")
            } else if kind == "blank" {
                try fields(scene, exactly: ["kind", "reason"], name: "blank")
            } else { throw DisplayProtocolV1.refusal("unsupported", "scene") }
        }
    }

    private static func source(_ value: Any?, name: String) throws {
        guard let state = value as? [String: Any], let kind = state["kind"] as? String else { throw DisplayProtocolV1.refusal("invalid_shape", name) }
        try fields(state, exactly: kind == "partial" ? ["kind", "reasons"] : ["kind"], name: name)
    }
}
