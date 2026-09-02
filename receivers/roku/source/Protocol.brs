function AstrolabeByteArray(text as string) as object
    bytes = CreateObject("roByteArray")
    bytes.FromAsciiString(text)
    return bytes
end function

sub AstrolabePushU32(bytes as object, value as dynamic)
    bytes.Push(Int(value / 16777216) mod 256)
    bytes.Push(Int(value / 65536) mod 256)
    bytes.Push(Int(value / 256) mod 256)
    bytes.Push(value mod 256)
end sub

sub AstrolabePushU64(bytes as object, value as dynamic)
    AstrolabePushU32(bytes, 0)
    AstrolabePushU32(bytes, value)
end sub

sub AstrolabeAppend(bytes as object, value as object)
    for each byte in value
        bytes.Push(byte)
    end for
end sub

sub AstrolabeField(bytes as object, value as object)
    AstrolabePushU32(bytes, value.Count())
    AstrolabeAppend(bytes, value)
end sub

sub AstrolabeTextField(bytes as object, value as string)
    AstrolabeField(bytes, AstrolabeByteArray(value))
end sub

sub AstrolabeOptionalTextField(bytes as object, value as dynamic)
    if value = invalid
        AstrolabeField(bytes, CreateObject("roByteArray"))
    else
        AstrolabeTextField(bytes, value)
    end if
end sub

sub AstrolabeU32Field(bytes as object, value as dynamic)
    encoded = CreateObject("roByteArray")
    AstrolabePushU32(encoded, value)
    AstrolabeField(bytes, encoded)
end sub

sub AstrolabeBooleanField(bytes as object, value as boolean)
    encoded = CreateObject("roByteArray")
    if value
        encoded.Push(1)
    else
        encoded.Push(0)
    end if
    AstrolabeField(bytes, encoded)
end sub

sub AstrolabeOptionalU32Field(bytes as object, value as dynamic)
    if value = invalid
        AstrolabeField(bytes, CreateObject("roByteArray"))
    else
        AstrolabeU32Field(bytes, value)
    end if
end sub

sub AstrolabeOptionalU64Field(bytes as object, value as dynamic)
    if value = invalid
        AstrolabeField(bytes, CreateObject("roByteArray"))
    else
        encoded = CreateObject("roByteArray")
        AstrolabePushU64(encoded, value)
        AstrolabeField(bytes, encoded)
    end if
end sub

function AstrolabeTranscript(domain as string) as object
    bytes = CreateObject("roByteArray")
    AstrolabeTextField(bytes, domain)
    return bytes
end function

function AstrolabeSha256(bytes as object) as string
    ' roEVPDigest has nothing to say about zero bytes; SHA-256 does.
    if bytes.Count() = 0 then return "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    digest = CreateObject("roEVPDigest")
    if digest.Setup("sha256") <> 0 then return ""
    return LCase(digest.Process(bytes))
end function

function AstrolabeHmacSha256(keyHex as string, bytes as object) as string
    key = CreateObject("roByteArray")
    key.FromHexString(keyHex)
    hmac = CreateObject("roHMAC")
    if hmac.Setup("sha256", key) <> 0 then return ""
    result = hmac.Process(bytes)
    if result = invalid then return ""
    return LCase(result.ToHexString())
end function

function AstrolabeIsHex(value as dynamic, characters as integer) as boolean
    if not AstrolabeIsString(value) then return false
    if Len(value) <> characters then return false
    matcher = CreateObject("roRegex", "^[0-9a-f]+$", "")
    return matcher.IsMatch(value)
end function

function AstrolabeIsString(value as dynamic) as boolean
    if value = invalid then return false
    return GetInterface(value, "ifString") <> invalid
end function

function AstrolabeIsArray(value as dynamic) as boolean
    if value = invalid then return false
    return GetInterface(value, "ifArray") <> invalid
end function

function AstrolabeReceiverBootstrap() as dynamic
    text = ReadAsciiFile("pkg:/receiver-bootstrap.json")
    if Len(text) < 1 or Len(text) > 32768 then return invalid
    bootstrap = ParseJson(text)
    if not AstrolabeExactFields(bootstrap, ["protocol_major", "trust", "certificate_pem", "rendezvous"]) then return invalid
    if bootstrap.protocol_major <> 1 then return invalid
    if bootstrap.rendezvous <> invalid and not AstrolabeIsHex(bootstrap.rendezvous, 32) then return invalid
    trust = bootstrap.trust
    if trust = invalid or not AstrolabeIsString(trust.kind) or not AstrolabeIsString(trust.origin) then return invalid
    if Left(trust.origin, 8) <> "https://" or Len(trust.origin) > 263 then return invalid
    authority = Mid(trust.origin, 9)
    if Len(authority) < 1 then return invalid
    if Instr(1, authority, "/") > 0 or Instr(1, authority, "?") > 0 or Instr(1, authority, "#") > 0 or Instr(1, authority, "@") > 0 then return invalid

    if trust.kind = "web_pki_origin"
        if not AstrolabeExactFields(trust, ["kind", "origin"]) or bootstrap.certificate_pem <> invalid then return invalid
        return {
            origin: trust.origin,
            rendezvous: bootstrap.rendezvous,
            trustKind: trust.kind,
            fingerprint: invalid,
            certificates: "common:/certs/ca-bundle.crt"
        }
    end if
    if trust.kind <> "pinned_certificate" or not AstrolabeExactFields(trust, ["kind", "origin", "sha256"]) then return invalid
    if not AstrolabeIsHex(trust.sha256, 64) or not AstrolabeIsString(bootstrap.certificate_pem) then return invalid
    pem = bootstrap.certificate_pem
    beginMarker = "-----BEGIN CERTIFICATE-----" + Chr(10)
    endMarker = "-----END CERTIFICATE-----" + Chr(10)
    if Len(pem) < 1 or Len(pem) > 16384 or Left(pem, Len(beginMarker)) <> beginMarker then return invalid
    if Right(pem, Len(endMarker)) <> endMarker then return invalid
    encoded = pem.Replace(beginMarker, "").Replace(endMarker, "").Replace(Chr(10), "")
    base64Matcher = CreateObject("roRegex", "^[A-Za-z0-9+/]+={0,2}$", "")
    if Len(encoded) < 4 or Len(encoded) mod 4 <> 0 or not base64Matcher.IsMatch(encoded) then return invalid
    certificate = CreateObject("roByteArray")
    certificate.FromBase64String(encoded)
    if certificate.Count() < 1 then return invalid
    if AstrolabeSha256(certificate) <> trust.sha256 then return invalid
    certificatePath = "tmp:/astrolabe-coordinator-ca.pem"
    if not WriteAsciiFile(certificatePath, pem) then return invalid
    return {
        origin: trust.origin,
        rendezvous: bootstrap.rendezvous,
        trustKind: trust.kind,
        fingerprint: trust.sha256,
        certificates: certificatePath
    }
end function

function AstrolabeRandomHex() as string
    device = CreateObject("roDeviceInfo")
    seed = device.GetRandomUUID() + device.GetRandomUUID()
    return AstrolabeSha256(AstrolabeByteArray(seed))
end function

function AstrolabePairingStatusTag(pollKey as string, pairing as string) as string
    bytes = AstrolabeTranscript("astrolabe-display/pairing-status/v1")
    AstrolabeU32Field(bytes, 1)
    AstrolabeTextField(bytes, pairing)
    return AstrolabeHmacSha256(pollKey, bytes)
end function

function AstrolabePairingCompleteTag(proofKey as string, pairing as string, device as string, challenge as string) as string
    bytes = AstrolabeTranscript("astrolabe-display/pairing-complete/v1")
    AstrolabeU32Field(bytes, 1)
    AstrolabeTextField(bytes, pairing)
    AstrolabeTextField(bytes, device)
    AstrolabeTextField(bytes, challenge)
    return AstrolabeHmacSha256(proofKey, bytes)
end function

function AstrolabeIsProfileId(value as dynamic) as boolean
    if type(value) <> "roString" and type(value) <> "String" then return false
    if Len(value) <> 30 then return false
    if Left(value, 4) <> "prf_" then return false
    for index = 5 to 30
        c = Asc(Mid(value, index, 1))
        if not ((c >= 48 and c <= 57) or (c >= 65 and c <= 86)) then return false
    end for
    return true
end function

function AstrolabeConfirmationPhrase(profile as string, pairing as string, nonce as string) as object
    words = [
        "amber", "anchor", "apple", "beacon", "birch", "cedar", "comet", "coral",
        "delta", "ember", "falcon", "fjord", "garden", "harbor", "hazel", "indigo",
        "juniper", "lantern", "maple", "meadow", "meteor", "olive", "orbit", "pebble",
        "quartz", "river", "saffron", "signal", "spruce", "violet", "willow", "zephyr"
    ]
    ' v2: the phrase commits the identity, not a placement certificate.
    bytes = AstrolabeTranscript("astrolabe-display/confirmation-phrase/v2")
    AstrolabeU32Field(bytes, 1)
    AstrolabeTextField(bytes, profile)
    AstrolabeTextField(bytes, pairing)
    AstrolabeTextField(bytes, nonce)
    digest = CreateObject("roByteArray")
    digest.FromHexString(AstrolabeSha256(bytes))
    phrase = []
    for index = 0 to 5
        phrase.Push(words[digest[index] and 31])
    end for
    return phrase
end function

function AstrolabeRequestTranscript(context as object) as object
    bytes = AstrolabeTranscript("astrolabe-display/request/v1")
    AstrolabeU32Field(bytes, 1)
    AstrolabeTextField(bytes, context.method)
    AstrolabeTextField(bytes, context.route)
    AstrolabeTextField(bytes, context.device)
    AstrolabeOptionalTextField(bytes, context.assignment)
    AstrolabeOptionalTextField(bytes, context.program)
    AstrolabeOptionalTextField(bytes, context.revision)
    AstrolabeOptionalTextField(bytes, context.currentItem)
    AstrolabeOptionalU32Field(bytes, context.elapsedMs)
    AstrolabeOptionalU32Field(bytes, context.waitMs)
    AstrolabeOptionalTextField(bytes, context.asset)
    if context.range = invalid
        AstrolabeOptionalU64Field(bytes, invalid)
        AstrolabeOptionalU32Field(bytes, invalid)
    else
        AstrolabeOptionalU64Field(bytes, context.range.start)
        AstrolabeOptionalU32Field(bytes, context.range.length)
    end if
    AstrolabeTextField(bytes, context.challenge)
    AstrolabeTextField(bytes, context.bodySha256)
    return bytes
end function

function AstrolabeRequestTag(proofKey as string, context as object) as string
    return AstrolabeHmacSha256(proofKey, AstrolabeRequestTranscript(context))
end function

function AstrolabeExactFields(value as dynamic, expected as object) as boolean
    if value = invalid then return false
    if GetInterface(value, "ifAssociativeArray") = invalid then return false
    keys = value.Keys()
    if keys.Count() <> expected.Count() then return false
    for each name in expected
        if not value.DoesExist(name) then return false
    end for
    return true
end function

function AstrolabeIntegerIn(value as dynamic, minimum as dynamic, maximum as dynamic) as boolean
    if value = invalid then return false
    if GetInterface(value, "ifInt") = invalid and GetInterface(value, "ifLongInt") = invalid then return false
    return value >= minimum and value <= maximum
end function

function AstrolabeValidSyncTarget(value as dynamic) as boolean
    if value = invalid then return true
    if not AstrolabeExactFields(value, ["group", "mode", "sampled_at_unix_ms"]) then return false
    if not AstrolabeIsString(value.group) or Len(value.group) < 1 or Len(value.group) > 64 then return false
    matcher = CreateObject("roRegex", "^[a-z0-9_-]+$", "")
    if not matcher.IsMatch(value.group) then return false
    if not AstrolabeIsString(value.mode) then return false
    if value.mode <> "stay_in_sync" and value.mode <> "positional" then return false
    return AstrolabeIntegerIn(value.sampled_at_unix_ms, 1, 9007199254740991)
end function

function AstrolabeValidApiError(value as dynamic) as boolean
    if not AstrolabeExactFields(value, ["protocol_major", "code", "retry_after_ms", "next_challenge"]) then return false
    if value.protocol_major <> 1 or not AstrolabeIsString(value.code) then return false
    allowed = {
        invalid_request: true,
        authentication_failed: true,
        challenge_expired: true,
        challenge_consumed: true,
        not_enrolled: true,
        unassigned: true,
        revoked: true,
        re_pair_required: true,
        unsupported_protocol: true,
        bound_exceeded: true,
        temporarily_unavailable: true
    }
    if not allowed.DoesExist(value.code) then return false
    if value.retry_after_ms <> invalid and not AstrolabeIntegerIn(value.retry_after_ms, 1, 60000) then return false
    if value.next_challenge <> invalid and not AstrolabeIsHex(value.next_challenge, 64) then return false
    return true
end function

function AstrolabeEncodeSourceState(bytes as object, state as dynamic) as boolean
    if not AstrolabeExactFields(state, ["kind"]) and not AstrolabeExactFields(state, ["kind", "reasons"]) then return false
    if not AstrolabeIsString(state.kind) then return false
    if state.kind = "current" or state.kind = "unavailable"
        if not AstrolabeExactFields(state, ["kind"]) then return false
        AstrolabeTextField(bytes, state.kind)
        return true
    end if
    if state.kind <> "partial" or not AstrolabeExactFields(state, ["kind", "reasons"]) then return false
    if not AstrolabeIsArray(state.reasons) then return false
    if state.reasons.Count() < 1 or state.reasons.Count() > 8 then return false
    allowed = { corrupt_records: true, degraded_source: true, incomplete_projection: true, provisional_data: true }
    previous = ""
    AstrolabeTextField(bytes, "partial")
    AstrolabeU32Field(bytes, state.reasons.Count())
    for each reason in state.reasons
        if not AstrolabeIsString(reason) then return false
        if not allowed.DoesExist(reason) or (previous <> "" and reason <= previous) then return false
        AstrolabeTextField(bytes, reason)
        previous = reason
    end for
    return true
end function

function AstrolabeEncodeAsset(bytes as object, asset as dynamic) as boolean
    if not AstrolabeExactFields(asset, ["id", "media_type", "encoded_len", "sha256", "width", "height"]) then return false
    if not AstrolabeIsHex(asset.id, 64) or not AstrolabeIsHex(asset.sha256, 64) then return false
    if not AstrolabeIsString(asset.media_type) then return false
    if not AstrolabeIntegerIn(asset.encoded_len, 1, 16777216) then return false
    isImage = asset.media_type = "image_jpeg" or asset.media_type = "image_png" or asset.media_type = "image_webp"
    if isImage
        if not AstrolabeIntegerIn(asset.width, 1, 4096) or not AstrolabeIntegerIn(asset.height, 1, 2160) then return false
        if asset.width * asset.height > 8847360 then return false
    else if asset.media_type = "hls_manifest"
        if asset.width <> invalid or asset.height <> invalid then return false
    else
        return false
    end if
    AstrolabeTextField(bytes, asset.media_type)
    AstrolabeU32Field(bytes, asset.encoded_len)
    AstrolabeTextField(bytes, asset.sha256)
    AstrolabeOptionalU32Field(bytes, asset.width)
    AstrolabeOptionalU32Field(bytes, asset.height)
    return true
end function

function AstrolabeProgramTranscript(program as dynamic) as dynamic
    if not AstrolabeExactFields(program, ["protocol_major", "assignment", "program", "revision", "program_state", "freshness", "playback", "items"]) then return invalid
    if program.protocol_major <> 1 or not AstrolabeIsHex(program.assignment, 32) or not AstrolabeIsHex(program.program, 32) or not AstrolabeIsHex(program.revision, 64) then return invalid
    if not AstrolabeExactFields(program.freshness, ["stale_after_ms", "on_stale"]) then return invalid
    if not AstrolabeIntegerIn(program.freshness.stale_after_ms, 30001, 86400000) then return invalid
    if not AstrolabeIsString(program.freshness.on_stale) then return invalid
    if program.freshness.on_stale <> "keep_with_native_banner" and program.freshness.on_stale <> "blank" then return invalid
    if not AstrolabeExactFields(program.playback, ["current_index", "elapsed_ms", "cycle", "sync"]) then return invalid
    if not AstrolabeIsString(program.playback.cycle) then return invalid
    if program.playback.cycle <> "loop" and program.playback.cycle <> "hold_last" and program.playback.cycle <> "blank_at_end" and program.playback.cycle <> "poll_at_end" then return invalid
    if not AstrolabeValidSyncTarget(program.playback.sync) then return invalid
    if not AstrolabeIsArray(program.items) then return invalid
    if program.items.Count() < 1 or program.items.Count() > 16 then return invalid
    if not AstrolabeIntegerIn(program.playback.current_index, 0, program.items.Count() - 1) or not AstrolabeIntegerIn(program.playback.elapsed_ms, 0, 4294967295) then return invalid

    bytes = AstrolabeTranscript("astrolabe-display/program-semantics/v2")
    AstrolabeU32Field(bytes, 1)
    AstrolabeTextField(bytes, program.assignment)
    AstrolabeTextField(bytes, program.program)
    if not AstrolabeEncodeSourceState(bytes, program.program_state) then return invalid
    AstrolabeU32Field(bytes, program.freshness.stale_after_ms)
    AstrolabeTextField(bytes, program.freshness.on_stale)
    AstrolabeTextField(bytes, program.playback.cycle)
    AstrolabeBooleanField(bytes, program.playback.sync <> invalid)
    if program.playback.sync <> invalid
        AstrolabeTextField(bytes, program.playback.sync.group)
        AstrolabeTextField(bytes, program.playback.sync.mode)
    end if
    AstrolabeU32Field(bytes, program.items.Count())
    ids = {}
    horizon = 0
    for index = 0 to program.items.Count() - 1
        item = program.items[index]
        if not AstrolabeExactFields(item, ["id", "duration_ms", "source_state", "scene", "spoken_summary"]) then return invalid
        if not AstrolabeIsHex(item.id, 64) or ids.DoesExist(item.id) then return invalid
        ids[item.id] = true
        AstrolabeTextField(bytes, item.id)
        if item.duration_ms = invalid
            if index <> program.items.Count() - 1 or program.playback.cycle <> "hold_last" then return invalid
        else
            if not AstrolabeIntegerIn(item.duration_ms, 250, 86400000) then return invalid
            horizon = horizon + item.duration_ms
            if horizon > 86400000 then return invalid
        end if
        AstrolabeOptionalU32Field(bytes, item.duration_ms)
        if not AstrolabeEncodeSourceState(bytes, item.source_state) then return invalid
        if not AstrolabeExactFields(item.scene, ["kind", "asset"]) and not AstrolabeExactFields(item.scene, ["kind", "manifest", "protocol", "live"]) and not AstrolabeExactFields(item.scene, ["kind", "reason"]) then return invalid
        if not AstrolabeIsString(item.scene.kind) then return invalid
        if item.scene.kind = "frame"
            if not AstrolabeExactFields(item.scene, ["kind", "asset"]) then return invalid
            AstrolabeTextField(bytes, "frame")
            if not AstrolabeEncodeAsset(bytes, item.scene.asset) then return invalid
        else if item.scene.kind = "media"
            if not AstrolabeExactFields(item.scene, ["kind", "manifest", "protocol", "live"]) then return invalid
            ' A stored clip and a live source both reach this receiver as HLS
            ' behind a ticket; `live` says which, and both are playable.
            if item.scene.protocol <> "hls" or (item.scene.live <> true and item.scene.live <> false) then return invalid
            if item.scene.manifest.media_type <> "hls_manifest" then return invalid
            AstrolabeTextField(bytes, "media")
            if not AstrolabeEncodeAsset(bytes, item.scene.manifest) then return invalid
            AstrolabeTextField(bytes, item.scene.protocol)
            AstrolabeBooleanField(bytes, item.scene.live)
        else if item.scene.kind = "blank"
            if not AstrolabeExactFields(item.scene, ["kind", "reason"]) then return invalid
            allowedBlank = { unassigned: true, host_unavailable: true, source_unavailable: true, unsupported: true, revoked: true, program_ended: true }
            if not AstrolabeIsString(item.scene.reason) then return invalid
            if not allowedBlank.DoesExist(item.scene.reason) then return invalid
            AstrolabeTextField(bytes, "blank")
            AstrolabeTextField(bytes, item.scene.reason)
        else
            return invalid
        end if
        if item.spoken_summary <> invalid
            if not AstrolabeIsString(item.spoken_summary) then return invalid
            summaryBytes = AstrolabeByteArray(item.spoken_summary)
            if summaryBytes.Count() < 1 or summaryBytes.Count() > 1024 then return invalid
        end if
        AstrolabeOptionalTextField(bytes, item.spoken_summary)
    end for
    current = program.items[program.playback.current_index]
    if current.duration_ms <> invalid and program.playback.elapsed_ms >= current.duration_ms then return invalid
    return bytes
end function

function AstrolabeVerifyProgram(program as dynamic) as boolean
    bytes = AstrolabeProgramTranscript(program)
    return bytes <> invalid and AstrolabeSha256(bytes) = program.revision
end function

function AstrolabeConformanceCheck() as boolean
    context = {
        method: "GET",
        route: "program_changes",
        device: "ffeeddccbbaa99887766554433221100",
        assignment: "00112233445566778899aabbccddeeff",
        program: "102132435465768798a9bacbdcedfe0f",
        revision: "5e6875a23ca08a49904655923c15c113e7918c73b1d623a6c3df193ee5fa5ee5",
        currentItem: "0e089d8d262e20aeb998ad2f84300b5023d588746c00c01da9ddb1e146b069e8",
        elapsedMs: 500,
        waitMs: 25000,
        asset: invalid,
        range: invalid,
        challenge: "1111111111111111111111111111111111111111111111111111111111111111",
        bodySha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    }
    requestTag = AstrolabeRequestTag("0000000000000000000000000000000000000000000000000000000000000000", context)
    if requestTag <> "130ed97e77f7751b21fe524e1d48f49f40129342cdfcce26ef3c12ce56a7ff0d" then return false
    completeTag = AstrolabePairingCompleteTag("2222222222222222222222222222222222222222222222222222222222222222", "33333333333333333333333333333333", "ffeeddccbbaa99887766554433221100", "4444444444444444444444444444444444444444444444444444444444444444")
    if completeTag <> "d8a85ed4a54c510ab3b4837ac9152675dfd9169c77fec2bc06ac7a14df077287" then return false
    phrase = AstrolabeConfirmationPhrase("6666666666666666666666666666666666666666666666666666666666666666", "77777777777777777777777777777777", "8888888888888888888888888888888888888888888888888888888888888888")
    return phrase.Count() = 6 and phrase[0] = "juniper" and phrase[1] = "willow" and phrase[2] = "beacon" and phrase[3] = "beacon" and phrase[4] = "signal" and phrase[5] = "juniper"
end function
