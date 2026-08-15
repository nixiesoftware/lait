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
    if value = invalid or Len(value) <> characters then return false
    matcher = CreateObject("roRegex", "^[0-9a-f]+$", "")
    return matcher.IsMatch(value)
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

function AstrolabeConfirmationPhrase(fingerprint as string, pairing as string, nonce as string) as object
    words = [
        "amber", "anchor", "apple", "beacon", "birch", "cedar", "comet", "coral",
        "delta", "ember", "falcon", "fjord", "garden", "harbor", "hazel", "indigo",
        "juniper", "lantern", "maple", "meadow", "meteor", "olive", "orbit", "pebble",
        "quartz", "river", "saffron", "signal", "spruce", "violet", "willow", "zephyr"
    ]
    bytes = AstrolabeTranscript("astrolabe-display/confirmation-phrase/v1")
    AstrolabeU32Field(bytes, 1)
    AstrolabeTextField(bytes, fingerprint)
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
    keys = value.Keys()
    if keys.Count() <> expected.Count() then return false
    for each name in expected
        if not value.DoesExist(name) then return false
    end for
    return true
end function

function AstrolabeIntegerIn(value as dynamic, minimum as dynamic, maximum as dynamic) as boolean
    if value = invalid then return false
    return value = Int(value) and value >= minimum and value <= maximum
end function

function AstrolabeEncodeSourceState(bytes as object, state as dynamic) as boolean
    if state = invalid or not state.DoesExist("kind") then return false
    if state.kind = "current" or state.kind = "unavailable"
        if not AstrolabeExactFields(state, ["kind"]) then return false
        AstrolabeTextField(bytes, state.kind)
        return true
    end if
    if state.kind <> "partial" or not AstrolabeExactFields(state, ["kind", "reasons"]) then return false
    if state.reasons.Count() < 1 or state.reasons.Count() > 8 then return false
    allowed = { data_missing: true, source_timeout: true, access_limited: true, render_degraded: true }
    previous = ""
    AstrolabeTextField(bytes, "partial")
    AstrolabeU32Field(bytes, state.reasons.Count())
    for each reason in state.reasons
        if not allowed.DoesExist(reason) or (previous <> "" and reason <= previous) then return false
        AstrolabeTextField(bytes, reason)
        previous = reason
    end for
    return true
end function

function AstrolabeEncodeAsset(bytes as object, asset as dynamic) as boolean
    if not AstrolabeExactFields(asset, ["id", "media_type", "encoded_len", "sha256", "width", "height"]) then return false
    if not AstrolabeIsHex(asset.id, 64) or not AstrolabeIsHex(asset.sha256, 64) then return false
    if asset.media_type <> "image_jpeg" and asset.media_type <> "image_png" and asset.media_type <> "image_webp" then return false
    if not AstrolabeIntegerIn(asset.encoded_len, 1, 16777216) then return false
    if not AstrolabeIntegerIn(asset.width, 1, 4096) or not AstrolabeIntegerIn(asset.height, 1, 2160) then return false
    if asset.width * asset.height > 8847360 then return false
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
    if program.freshness.on_stale <> "keep_with_native_banner" and program.freshness.on_stale <> "blank" then return invalid
    if not AstrolabeExactFields(program.playback, ["current_index", "elapsed_ms", "cycle"]) then return invalid
    if program.playback.cycle <> "loop" and program.playback.cycle <> "hold_last" and program.playback.cycle <> "blank_at_end" and program.playback.cycle <> "poll_at_end" then return invalid
    if program.items.Count() < 1 or program.items.Count() > 16 then return invalid
    if not AstrolabeIntegerIn(program.playback.current_index, 0, program.items.Count() - 1) or not AstrolabeIntegerIn(program.playback.elapsed_ms, 0, 2147483647) then return invalid

    bytes = AstrolabeTranscript("astrolabe-display/program-semantics/v1")
    AstrolabeU32Field(bytes, 1)
    AstrolabeTextField(bytes, program.assignment)
    AstrolabeTextField(bytes, program.program)
    if not AstrolabeEncodeSourceState(bytes, program.program_state) then return invalid
    AstrolabeU32Field(bytes, program.freshness.stale_after_ms)
    AstrolabeTextField(bytes, program.freshness.on_stale)
    AstrolabeTextField(bytes, program.playback.cycle)
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
        if item.scene = invalid or not item.scene.DoesExist("kind") then return invalid
        if item.scene.kind = "frame"
            if not AstrolabeExactFields(item.scene, ["kind", "asset"]) then return invalid
            AstrolabeTextField(bytes, "frame")
            if not AstrolabeEncodeAsset(bytes, item.scene.asset) then return invalid
        else if item.scene.kind = "blank"
            if not AstrolabeExactFields(item.scene, ["kind", "reason"]) then return invalid
            allowedBlank = { unassigned: true, host_unavailable: true, source_unavailable: true, unsupported: true, revoked: true, program_ended: true }
            if not allowedBlank.DoesExist(item.scene.reason) then return invalid
            AstrolabeTextField(bytes, "blank")
            AstrolabeTextField(bytes, item.scene.reason)
        else
            return invalid
        end if
        if item.spoken_summary <> invalid
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
