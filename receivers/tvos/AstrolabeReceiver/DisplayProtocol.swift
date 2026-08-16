import CryptoKit
import Foundation
import Security

enum DisplayProtocolV1 {
    static let major = 1
    static let emptyDigest = SHA256.hash(data: Data()).hex
    static let words = [
        "amber", "anchor", "apple", "beacon", "birch", "cedar", "comet", "coral",
        "delta", "ember", "falcon", "fjord", "garden", "harbor", "hazel", "indigo",
        "juniper", "lantern", "maple", "meadow", "meteor", "olive", "orbit", "pebble",
        "quartz", "river", "saffron", "signal", "spruce", "violet", "willow", "zephyr",
    ]

    struct Transcript {
        private(set) var data = Data()

        init(domain: String) throws { try field(Data(domain.utf8)) }

        mutating func field(_ value: Data) throws {
            guard value.count <= Int(UInt32.max) else { throw refusal("bound_exceeded", "transcript field") }
            u32Bytes(UInt32(value.count)).forEach { data.append($0) }
            data.append(value)
        }

        mutating func text(_ value: String) throws { try field(Data(value.utf8)) }
        mutating func optionalText(_ value: String?) throws { try field(value.map { Data($0.utf8) } ?? Data()) }
        mutating func u32(_ value: Int) throws {
            guard let encoded = UInt32(exactly: value) else { throw refusal("bound_exceeded", "u32") }
            try field(Data(u32Bytes(encoded)))
        }
        mutating func optionalU32(_ value: Int?) throws {
            guard let value else { try field(Data()); return }
            try u32(value)
        }
        mutating func optionalU64(_ value: UInt64?) throws {
            guard let value else { try field(Data()); return }
            try field(Data([
                UInt8((value >> 56) & 0xff), UInt8((value >> 48) & 0xff),
                UInt8((value >> 40) & 0xff), UInt8((value >> 32) & 0xff),
                UInt8((value >> 24) & 0xff), UInt8((value >> 16) & 0xff),
                UInt8((value >> 8) & 0xff), UInt8(value & 0xff),
            ]))
        }
        mutating func boolean(_ value: Bool) throws { try field(Data([value ? 1 : 0])) }
    }

    struct RequestContext {
        let method: String
        let route: String
        let device: String
        let assignment: String?
        let program: String?
        let revision: String?
        let currentItem: String?
        let elapsedMs: Int?
        let waitMs: Int?
        let asset: String?
        let rangeStart: UInt64?
        let rangeLength: Int?
        let challenge: String
        let bodySHA256: String
    }

    static func randomHex() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw refusal("secure_random", "SecRandomCopyBytes failed")
        }
        return Data(bytes).hex
    }

    static func sha256(_ data: Data) -> String { SHA256.hash(data: data).hex }

    static func hmac(keyHex: String, data: Data) throws -> String {
        let keyData = try hexData(keyHex, bytes: 32)
        return HMAC<SHA256>.authenticationCode(for: data, using: SymmetricKey(data: keyData)).hex
    }

    static func pairingStatusTag(key: String, pairing: String) throws -> String {
        var transcript = try Transcript(domain: "astrolabe-display/pairing-status/v1")
        try transcript.u32(major)
        try transcript.text(pairing)
        return try hmac(keyHex: key, data: transcript.data)
    }

    static func pairingCompleteTag(key: String, pairing: String, device: String, challenge: String) throws -> String {
        var transcript = try Transcript(domain: "astrolabe-display/pairing-complete/v1")
        try transcript.u32(major)
        try transcript.text(pairing)
        try transcript.text(device)
        try transcript.text(challenge)
        return try hmac(keyHex: key, data: transcript.data)
    }

    static func confirmationPhrase(fingerprint: String, pairing: String, nonce: String) throws -> [String] {
        var transcript = try Transcript(domain: "astrolabe-display/confirmation-phrase/v1")
        try transcript.u32(major)
        try transcript.text(fingerprint)
        try transcript.text(pairing)
        try transcript.text(nonce)
        return SHA256.hash(data: transcript.data).prefix(6).map { words[Int($0 & 0x1f)] }
    }

    static func requestTag(key: String, context: RequestContext) throws -> String {
        var transcript = try Transcript(domain: "astrolabe-display/request/v1")
        try transcript.u32(major)
        try transcript.text(context.method)
        try transcript.text(context.route)
        try transcript.text(context.device)
        try transcript.optionalText(context.assignment)
        try transcript.optionalText(context.program)
        try transcript.optionalText(context.revision)
        try transcript.optionalText(context.currentItem)
        try transcript.optionalU32(context.elapsedMs)
        try transcript.optionalU32(context.waitMs)
        try transcript.optionalText(context.asset)
        try transcript.optionalU64(context.rangeStart)
        try transcript.optionalU32(context.rangeLength)
        try transcript.text(context.challenge)
        try transcript.text(context.bodySHA256)
        return try hmac(keyHex: key, data: transcript.data)
    }

    static func verifyProgram(_ program: DisplayProgram) throws {
        let transcript = try programTranscript(program)
        guard sha256(transcript) == program.revision else { throw refusal("integrity", "program revision") }
    }

    static func programTranscript(_ program: DisplayProgram) throws -> Data {
        guard program.protocolMajor == major,
              isHex(program.assignment, count: 32), isHex(program.program, count: 32),
              isHex(program.revision, count: 64), !program.items.isEmpty, program.items.count <= 16,
              (30_001...86_400_000).contains(program.freshness.staleAfterMs),
              ["keep_with_native_banner", "blank"].contains(program.freshness.onStale),
              ["loop", "hold_last", "blank_at_end", "poll_at_end"].contains(program.playback.cycle),
              program.items.indices.contains(program.playback.currentIndex),
              (0...Int(UInt32.max)).contains(program.playback.elapsedMs)
        else { throw refusal("invalid_shape", "program envelope") }

        if let sync = program.playback.sync {
            let bytes = Array(sync.group.utf8)
            guard !bytes.isEmpty, bytes.count <= 64,
                  bytes.allSatisfy({ (48...57).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95 }),
                  ["stay_in_sync", "positional"].contains(sync.mode), sync.sampledAtUnixMs > 0
            else { throw refusal("invalid_shape", "sync target") }
        }

        var transcript = try Transcript(domain: "astrolabe-display/program-semantics/v2")
        try transcript.u32(major)
        try transcript.text(program.assignment)
        try transcript.text(program.program)
        try encodeSource(program.programState, into: &transcript)
        try transcript.u32(program.freshness.staleAfterMs)
        try transcript.text(program.freshness.onStale)
        try transcript.text(program.playback.cycle)
        try transcript.boolean(program.playback.sync != nil)
        if let sync = program.playback.sync {
            try transcript.text(sync.group)
            try transcript.text(sync.mode)
        }
        try transcript.u32(program.items.count)

        var seen = Set<String>()
        var horizon = 0
        for (index, item) in program.items.enumerated() {
            guard isHex(item.id, count: 64), seen.insert(item.id).inserted else {
                throw refusal("invalid_shape", "program item id")
            }
            if let duration = item.durationMs {
                guard (250...86_400_000).contains(duration) else { throw refusal("bound_exceeded", "item duration") }
                horizon += duration
                guard horizon <= 86_400_000 else { throw refusal("bound_exceeded", "staging horizon") }
            } else if index != program.items.count - 1 || program.playback.cycle != "hold_last" {
                throw refusal("invalid_shape", "open-ended item")
            }
            try transcript.text(item.id)
            try transcript.optionalU32(item.durationMs)
            try encodeSource(item.sourceState, into: &transcript)
            switch item.scene.kind {
            case "frame":
                guard let asset = item.scene.asset else { throw refusal("invalid_shape", "frame asset") }
                try transcript.text("frame")
                try encodeAsset(asset, into: &transcript)
            case "blank":
                guard let reason = item.scene.reason,
                      ["unassigned", "host_unavailable", "source_unavailable", "unsupported", "revoked", "program_ended"].contains(reason)
                else { throw refusal("invalid_shape", "blank reason") }
                try transcript.text("blank")
                try transcript.text(reason)
            default:
                throw refusal("unsupported", "tvOS frame receiver does not accept media scenes")
            }
            if let summary = item.spokenSummary {
                guard !summary.isEmpty, Data(summary.utf8).count <= 1024 else { throw refusal("bound_exceeded", "spoken summary") }
            }
            try transcript.optionalText(item.spokenSummary)
        }
        let current = program.items[program.playback.currentIndex]
        if let duration = current.durationMs, program.playback.elapsedMs >= duration {
            throw refusal("invalid_shape", "playback cursor")
        }
        return transcript.data
    }

    private static func encodeSource(_ state: SourceState, into transcript: inout Transcript) throws {
        switch state.kind {
        case "current", "unavailable":
            guard state.reasons == nil else { throw refusal("unknown_field", "source state") }
            try transcript.text(state.kind)
        case "partial":
            guard let reasons = state.reasons, (1...8).contains(reasons.count),
                  reasons == reasons.sorted(), Set(reasons).count == reasons.count,
                  reasons.allSatisfy({ ["corrupt_records", "degraded_source", "incomplete_projection", "provisional_data"].contains($0) })
            else { throw refusal("invalid_shape", "partial source state") }
            try transcript.text("partial")
            try transcript.u32(reasons.count)
            for reason in reasons { try transcript.text(reason) }
        default: throw refusal("unsupported", "source state")
        }
    }

    private static func encodeAsset(_ asset: DisplayAsset, into transcript: inout Transcript) throws {
        guard isHex(asset.id, count: 64), isHex(asset.sha256, count: 64),
              ["image_jpeg", "image_png", "image_webp"].contains(asset.mediaType),
              (1...16_777_216).contains(asset.encodedLen),
              let width = asset.width, let height = asset.height,
              (1...4096).contains(width), (1...2160).contains(height), width * height <= 8_847_360
        else { throw refusal("invalid_shape", "frame asset") }
        try transcript.text(asset.mediaType)
        try transcript.u32(asset.encodedLen)
        try transcript.text(asset.sha256)
        try transcript.optionalU32(width)
        try transcript.optionalU32(height)
    }

    static func isHex(_ value: String?, count: Int) -> Bool {
        guard let value, value.utf8.count == count else { return false }
        return value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }

    static func refusal(_ code: String, _ detail: String) -> DisplayProtocolError { .refusal(code, detail) }
}

private func u32Bytes(_ value: UInt32) -> [UInt8] {
    [UInt8((value >> 24) & 0xff), UInt8((value >> 16) & 0xff), UInt8((value >> 8) & 0xff), UInt8(value & 0xff)]
}

private func hexData(_ value: String, bytes: Int) throws -> Data {
    guard DisplayProtocolV1.isHex(value, count: bytes * 2) else { throw DisplayProtocolV1.refusal("invalid_encoding", "hex") }
    var output = Data(capacity: bytes)
    var index = value.startIndex
    for _ in 0..<bytes {
        let next = value.index(index, offsetBy: 2)
        guard let byte = UInt8(value[index..<next], radix: 16) else { throw DisplayProtocolV1.refusal("invalid_encoding", "hex") }
        output.append(byte)
        index = next
    }
    return output
}

extension Sequence where Element == UInt8 {
    var hex: String { map { String(format: "%02x", $0) }.joined() }
}
