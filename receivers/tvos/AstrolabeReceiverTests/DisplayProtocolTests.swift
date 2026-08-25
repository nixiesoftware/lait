import XCTest
@testable import AstrolabeReceiver

final class DisplayProtocolTests: XCTestCase {
    private var fixture: [String: Any]!

    override func setUpWithError() throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "conformance", withExtension: "json"))
        fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    }

    func testConfirmationPhraseMatchesRustFixture() throws {
        let value = try XCTUnwrap(fixture["confirmation_phrase"] as? [String: Any])
        XCTAssertEqual(
            try DisplayProtocolV1.confirmationPhrase(
                profile: value["profile"] as! String,
                pairing: value["pairing"] as! String,
                nonce: value["receiver_nonce"] as! String
            ),
            value["words"] as? [String]
        )
    }

    func testPairingCompleteTranscriptMatchesRustFixture() throws {
        let value = try XCTUnwrap(fixture["pairing_complete"] as? [String: String])
        XCTAssertEqual(
            try DisplayProtocolV1.pairingCompleteTag(
                key: value["proof_key_hex"]!, pairing: value["pairing"]!,
                device: value["device"]!, challenge: value["challenge"]!
            ),
            value["authentication_tag"]
        )
    }

    func testProgramChangeRequestMatchesRustFixture() throws {
        let value = try XCTUnwrap(fixture["program_changes_request"] as? [String: Any])
        let context = DisplayProtocolV1.RequestContext(
            method: value["method"] as! String, route: value["route"] as! String,
            device: value["device"] as! String, assignment: value["assignment"] as? String,
            program: value["program"] as? String, revision: value["revision"] as? String,
            currentItem: value["current_item"] as? String,
            elapsedMs: (value["elapsed_ms"] as? NSNumber)?.intValue,
            waitMs: (value["wait_ms"] as? NSNumber)?.intValue,
            asset: nil, rangeStart: nil, rangeLength: nil,
            challenge: value["challenge"] as! String,
            bodySHA256: value["body_sha256"] as! String
        )
        let key = (fixture["fixture_only_keys"] as! [String: String])["proof_key_hex"]!
        XCTAssertEqual(try DisplayProtocolV1.requestTag(key: key, context: context), value["authentication_tag"] as? String)
    }

    func testProgramRevisionMatchesRustFixture() throws {
        let object = try XCTUnwrap(fixture["program"] as? [String: Any])
        try StrictJSON.program(object)
        let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        let program = try JSONDecoder().decode(DisplayProgram.self, from: data)
        XCTAssertNoThrow(try DisplayProtocolV1.verifyProgram(program))
    }

    func testUnknownProgramFieldIsRefused() throws {
        var object = try XCTUnwrap(fixture["program"] as? [String: Any])
        object["world"] = "forbidden"
        XCTAssertThrowsError(try StrictJSON.program(object))
    }
}
