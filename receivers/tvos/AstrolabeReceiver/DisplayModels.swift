import Foundation
import UIKit

struct SourceState: Codable, Equatable {
    let kind: String
    let reasons: [String]?
}

struct DisplayAsset: Codable, Equatable {
    let id: String
    let mediaType: String
    let encodedLen: Int
    let sha256: String
    let width: Int?
    let height: Int?

    enum CodingKeys: String, CodingKey {
        case id
        case mediaType = "media_type"
        case encodedLen = "encoded_len"
        case sha256, width, height
    }
}

struct DisplayScene: Codable, Equatable {
    let kind: String
    let asset: DisplayAsset?
    let manifest: DisplayAsset?
    let `protocol`: String?
    let live: Bool?
    let reason: String?
}

struct DisplayProgramItem: Codable, Equatable {
    let id: String
    let durationMs: Int?
    let sourceState: SourceState
    let scene: DisplayScene
    let spokenSummary: String?

    enum CodingKeys: String, CodingKey {
        case id
        case durationMs = "duration_ms"
        case sourceState = "source_state"
        case scene
        case spokenSummary = "spoken_summary"
    }
}

struct ProgramFreshness: Codable, Equatable {
    let staleAfterMs: Int
    let onStale: String

    enum CodingKeys: String, CodingKey {
        case staleAfterMs = "stale_after_ms"
        case onStale = "on_stale"
    }
}

struct PlaybackCursor: Codable, Equatable {
    let currentIndex: Int
    let elapsedMs: Int
    let cycle: String
    let sync: SyncTarget?

    enum CodingKeys: String, CodingKey {
        case currentIndex = "current_index"
        case elapsedMs = "elapsed_ms"
        case cycle, sync
    }
}

struct SyncTarget: Codable, Equatable {
    let group: String
    let mode: String
    let sampledAtUnixMs: Int64

    enum CodingKeys: String, CodingKey {
        case group, mode
        case sampledAtUnixMs = "sampled_at_unix_ms"
    }
}

struct DisplayProgram: Codable, Equatable {
    let protocolMajor: Int
    let assignment: String
    let program: String
    let revision: String
    let programState: SourceState
    let freshness: ProgramFreshness
    var playback: PlaybackCursor
    let items: [DisplayProgramItem]

    enum CodingKeys: String, CodingKey {
        case protocolMajor = "protocol_major"
        case assignment, program, revision
        case programState = "program_state"
        case freshness, playback, items
    }
}

struct StagedFrame {
    let image: UIImage
    let encodedBytes: Int
    let digest: String
}

struct ReceiverCredential: Codable, Equatable {
    var mode: String
    let origin: String
    var pairing: String?
    var receiverNonce: String?
    var pollKey: String?
    var fingerprint: String?
    var phrase: [String]?
    var userConfirmed: Bool?
    var device: String?
    var proofKey: String?
    var enrollmentChallenge: String?
}

enum ReceiverScreen {
    case booting
    case pairing(words: [String], fingerprint: String, confirmed: Bool)
    case unassigned(device: String)
    case frame(UIImage, summary: String?)
    case message(eyebrow: String, title: String, body: String, retry: Bool)
}

enum DisplayProtocolError: Error, LocalizedError, Equatable {
    case refusal(String, String)

    var errorDescription: String? {
        switch self {
        case let .refusal(code, detail): return "\(code): \(detail)"
        }
    }
}
