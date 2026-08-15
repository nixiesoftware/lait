import CryptoKit
import Foundation

enum ReceiverTrust {
    case webPKI(origin: URL)
    case pinned(origin: URL, sha256: String)

    var origin: URL {
        switch self {
        case let .webPKI(origin), let .pinned(origin, _): origin
        }
    }

    var fingerprint: String? {
        if case let .pinned(_, sha256) = self { return sha256 }
        return nil
    }
}

struct ReceiverBootstrap {
    let trust: ReceiverTrust
    let rendezvous: String?

    static func load(bundle: Bundle = .main) throws -> ReceiverBootstrap {
        guard let url = bundle.url(forResource: "ReceiverBootstrap", withExtension: "json") else {
            throw DisplayProtocolV1.refusal("missing_bootstrap", "ReceiverBootstrap.json")
        }
        let data = try Data(contentsOf: url, options: [.mappedIfSafe])
        guard (1...(32 * 1024)).contains(data.count) else {
            throw DisplayProtocolV1.refusal("bound_exceeded", "receiver bootstrap")
        }
        let object = try StrictJSON.object(data, name: "receiver bootstrap")
        try StrictJSON.fields(
            object,
            exactly: ["protocol_major", "trust", "certificate_pem", "rendezvous"],
            name: "receiver bootstrap"
        )
        guard StrictJSON.int(object["protocol_major"]) == 1,
              let trustObject = object["trust"] as? [String: Any]
        else { throw DisplayProtocolV1.refusal("unsupported", "receiver bootstrap") }
        let rendezvous: String?
        if object["rendezvous"] is NSNull {
            rendezvous = nil
        } else if let value = object["rendezvous"] as? String, DisplayProtocolV1.isHex(value, count: 32) {
            rendezvous = value
        } else {
            throw DisplayProtocolV1.refusal("invalid_shape", "bootstrap rendezvous")
        }
        guard let kind = trustObject["kind"] as? String,
              let rawOrigin = trustObject["origin"] as? String,
              let origin = exactOrigin(rawOrigin)
        else { throw DisplayProtocolV1.refusal("invalid_origin", "receiver bootstrap") }
        let trust: ReceiverTrust
        if kind == "web_pki_origin" {
            try StrictJSON.fields(trustObject, exactly: ["kind", "origin"], name: "bootstrap trust")
            guard object["certificate_pem"] is NSNull else {
                throw DisplayProtocolV1.refusal("invalid_shape", "Web PKI certificate")
            }
            trust = .webPKI(origin: origin)
        } else if kind == "pinned_certificate" {
            try StrictJSON.fields(trustObject, exactly: ["kind", "origin", "sha256"], name: "bootstrap trust")
            guard let fingerprint = trustObject["sha256"] as? String,
                  DisplayProtocolV1.isHex(fingerprint, count: 64),
                  let pem = object["certificate_pem"] as? String,
                  let certificate = decodeCertificatePEM(pem),
                  SHA256.hash(data: certificate).hex == fingerprint
            else { throw DisplayProtocolV1.refusal("pin_mismatch", "bootstrap certificate") }
            trust = .pinned(origin: origin, sha256: fingerprint)
        } else {
            throw DisplayProtocolV1.refusal("unsupported_trust", "receiver bootstrap")
        }
        return ReceiverBootstrap(trust: trust, rendezvous: rendezvous)
    }

    private static func exactOrigin(_ value: String) -> URL? {
        guard let components = URLComponents(string: value),
              components.scheme == "https", components.host != nil,
              components.user == nil, components.password == nil,
              components.path.isEmpty, components.query == nil, components.fragment == nil,
              let url = components.url, url.absoluteString == value
        else { return nil }
        return url
    }

    private static func decodeCertificatePEM(_ pem: String) -> Data? {
        let begin = "-----BEGIN CERTIFICATE-----\n"
        let end = "-----END CERTIFICATE-----\n"
        guard (1...(16 * 1024)).contains(pem.utf8.count),
              pem.hasPrefix(begin), pem.hasSuffix(end)
        else { return nil }
        let body = pem.dropFirst(begin.count).dropLast(end.count)
        guard body.hasSuffix("\n") else { return nil }
        let lines = body.dropLast().split(separator: "\n", omittingEmptySubsequences: false)
        guard !lines.isEmpty, lines.allSatisfy({ !$0.isEmpty && $0.count <= 64 }) else { return nil }
        return Data(base64Encoded: lines.joined())
    }
}
