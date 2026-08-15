sub init()
    m.top.functionName = "AstrolabeRun"
end sub

sub AstrolabePublish(model as object)
    if not model.DoesExist("transport") then model.transport = m.transport
    if not model.DoesExist("source") then model.source = "none"
    if not model.DoesExist("stale") then model.stale = false
    m.top.viewModel = model
end sub

function AstrolabeHeader(event as object, wanted as string) as dynamic
    found = invalid
    for each entry in event.GetResponseHeadersArray()
        for each name in entry
            if LCase(name) = LCase(wanted)
                if found <> invalid then return invalid
                found = entry[name]
            end if
        end for
    end for
    return found
end function

function AstrolabeTransfer(path as string, method as string, body as string, headers as object, timeoutMs as integer, targetFile = invalid as dynamic) as dynamic
    transfer = CreateObject("roUrlTransfer")
    transfer.SetPort(m.port)
    transfer.SetCertificatesFile("common:/certs/ca-bundle.crt")
    transfer.EnablePeerVerification(true)
    transfer.EnableHostVerification(true)
    transfer.RetainBodyOnError(true)
    transfer.SetUrl(m.origin + path)
    transfer.AddHeader("Accept", "application/json")
    for each name in headers
        transfer.AddHeader(name, headers[name])
    end for
    issued = false
    if method = "POST"
        transfer.AddHeader("Content-Type", "application/json; charset=utf-8")
        issued = transfer.AsyncPostFromString(body)
    else if targetFile <> invalid
        issued = transfer.AsyncGetToFile(targetFile)
    else
        issued = transfer.AsyncGetToString()
    end if
    if not issued then return invalid

    clock = CreateObject("roTimespan")
    clock.Mark()
    while clock.TotalMilliseconds() < timeoutMs
        event = Wait(100, m.port)
        AstrolabeTickPlayback()
        if event <> invalid and Type(event) = "roUrlEvent" and event.GetSourceIdentity() = transfer.GetIdentity()
            result = { status: event.GetResponseCode(), event: event, body: "" }
            if targetFile = invalid then result.body = event.GetString()
            return result
        end if
    end while
    transfer.AsyncCancel()
    return invalid
end function

function AstrolabeJson(path as string, method as string, value as dynamic, headers = invalid as dynamic, timeoutMs = 30000 as integer) as dynamic
    body = ""
    if value <> invalid then body = FormatJson(value)
    if headers = invalid then headers = {}
    result = AstrolabeTransfer(path, method, body, headers, timeoutMs)
    if result = invalid then return invalid
    bytes = AstrolabeByteArray(result.body)
    if bytes.Count() > 65536 then return invalid
    result.json = ParseJson(result.body)
    return result
end function

function AstrolabeCapabilities() as object
    display = CreateObject("roDeviceInfo").GetDisplaySize()
    width = 1920
    height = 1080
    if display <> invalid
        width = display.w
        height = display.h
    end if
    return {
        protocol_major: 1,
        platform: "roku",
        build: "astrolabe-roku/0.1.0",
        viewport: { width: width, height: height, scale_milli: 1000 },
        image_types: ["image_jpeg", "image_png", "image_webp"],
        max_asset_bytes: 16777216,
        max_staged_bytes: 50331648,
        max_program_items: 16,
        max_staging_horizon_ms: 86400000,
        locale: "en-US",
        accessibility: {
            native_screen_reader: true,
            spoken_summary: true,
            captions: false,
            audio_description: false
        },
        playback: {
            tier: "frame",
            sync_class: "boundary",
            rate_control_probed: false,
            latency_class: "snapshot",
            health_granularity: "full"
        }
    }
end function

function AstrolabeCurrentPlayback() as object
    if m.program = invalid then return { currentIndex: 0, elapsedMs: 0 }
    elapsed = m.elapsedBase + m.playbackClock.TotalMilliseconds()
    if elapsed > 2147483647 then elapsed = 2147483647
    return { currentIndex: m.program.playback.current_index, elapsedMs: elapsed }
end function

function AstrolabeContext(route as string, method as string, bodySha as string, overrides = invalid as dynamic) as object
    context = {
        method: method,
        route: route,
        device: m.credential.device,
        assignment: invalid,
        program: invalid,
        revision: invalid,
        currentItem: invalid,
        elapsedMs: invalid,
        waitMs: invalid,
        asset: invalid,
        range: invalid,
        challenge: m.challenge,
        bodySha256: bodySha
    }
    if m.program <> invalid
        playback = AstrolabeCurrentPlayback()
        context.assignment = m.program.assignment
        context.program = m.program.program
        context.revision = m.program.revision
        context.currentItem = m.program.items[playback.currentIndex].id
        context.elapsedMs = playback.elapsedMs
    end if
    if overrides <> invalid
        for each name in overrides
            context[name] = overrides[name]
        end for
    end if
    return context
end function

function AstrolabeHeaders(context as object, tag as string) as object
    headers = {
        "Authorization": "Astrolabe-HMAC " + tag,
        "X-Astrolabe-Protocol-Major": "1",
        "X-Astrolabe-Route": context.route,
        "X-Astrolabe-Device": context.device,
        "X-Astrolabe-Challenge": context.challenge,
        "X-Astrolabe-Body-SHA256": context.bodySha256
    }
    optional = {
        "X-Astrolabe-Assignment": context.assignment,
        "X-Astrolabe-Program": context.program,
        "X-Astrolabe-Revision": context.revision,
        "X-Astrolabe-Current-Item": context.currentItem,
        "X-Astrolabe-Elapsed-Ms": context.elapsedMs,
        "X-Astrolabe-Wait-Ms": context.waitMs,
        "X-Astrolabe-Asset": context.asset
    }
    for each name in optional
        if optional[name] <> invalid then headers[name] = optional[name].ToStr()
    end for
    if context.range <> invalid
        headers["X-Astrolabe-Range-Start"] = context.range.start.ToStr()
        headers["X-Astrolabe-Range-Length"] = context.range.length.ToStr()
        lastByte = context.range.start + context.range.length - 1
        headers["Range"] = "bytes=" + context.range.start.ToStr() + "-" + lastByte.ToStr()
    end if
    return headers
end function

function AstrolabeEnsureChallenge() as boolean
    if m.challenge <> invalid then return true
    response = AstrolabeJson("/head/v1/challenges", "POST", {
        protocol_major: 1,
        device: m.credential.device
    })
    if response = invalid or response.status <> 200 or response.json = invalid then return false
    if not AstrolabeExactFields(response.json, ["protocol_major", "challenge", "expires_in_ms"]) then return false
    if response.json.protocol_major <> 1 or not AstrolabeIsHex(response.json.challenge, 64) then return false
    if not AstrolabeIntegerIn(response.json.expires_in_ms, 1, 120000) then return false
    m.challenge = response.json.challenge
    return true
end function

function AstrolabeAuthorizedJson(route as string, method as string, path as string, value as dynamic, overrides = invalid as dynamic, timeoutMs = 30000 as integer) as dynamic
    if not AstrolabeEnsureChallenge() then return invalid
    body = ""
    if value <> invalid then body = FormatJson(value)
    bodySha = AstrolabeSha256(AstrolabeByteArray(body))
    context = AstrolabeContext(route, method, bodySha, overrides)
    headers = AstrolabeHeaders(context, AstrolabeRequestTag(m.credential.proofKey, context))
    m.challenge = invalid
    result = AstrolabeTransfer(path, method, body, headers, timeoutMs)
    if result = invalid
        m.transport = "offline"
        return invalid
    end if
    nextChallenge = AstrolabeHeader(result.event, "X-Astrolabe-Next-Challenge")
    if not AstrolabeIsHex(nextChallenge, 64) then return invalid
    m.challenge = nextChallenge
    m.transport = "online"
    m.lastDelivery.Mark()
    if AstrolabeByteArray(result.body).Count() > 65536 then return invalid
    result.json = ParseJson(result.body)
    return result
end function

function AstrolabeAuthorizedAsset(asset as object, program as object) as dynamic
    complete = CreateObject("roByteArray")
    start = 0
    chunkSize = 262144
    while start < asset.encoded_len
        length = chunkSize
        if start + length > asset.encoded_len then length = asset.encoded_len - start
        if not AstrolabeEnsureChallenge() then return invalid
        emptySha = AstrolabeSha256(CreateObject("roByteArray"))
        context = AstrolabeContext("asset", "GET", emptySha, {
            assignment: program.assignment,
            program: program.program,
            revision: program.revision,
            currentItem: invalid,
            elapsedMs: invalid,
            asset: asset.id,
            range: { start: start, length: length }
        })
        headers = AstrolabeHeaders(context, AstrolabeRequestTag(m.credential.proofKey, context))
        headers["Accept"] = AstrolabeMediaType(asset.media_type)
        chunkPath = "tmp:/astrolabe_chunk"
        CreateObject("roFileSystem").Delete(chunkPath)
        m.challenge = invalid
        result = AstrolabeTransfer("/head/v1/assets/" + asset.id, "GET", "", headers, 30000, chunkPath)
        if result = invalid or result.status <> 206 then return invalid
        nextChallenge = AstrolabeHeader(result.event, "X-Astrolabe-Next-Challenge")
        if not AstrolabeIsHex(nextChallenge, 64) then return invalid
        m.challenge = nextChallenge
        contentType = AstrolabeHeader(result.event, "Content-Type")
        if contentType = invalid or LCase(contentType) <> AstrolabeMediaType(asset.media_type) then return invalid
        stat = CreateObject("roFileSystem").Stat(chunkPath)
        if stat = invalid or stat.size <> length then return invalid
        chunk = CreateObject("roByteArray")
        if not chunk.ReadFile(chunkPath) or chunk.Count() <> length then return invalid
        AstrolabeAppend(complete, chunk)
        if complete.Count() > asset.encoded_len or complete.Count() > 16777216 then return invalid
        start = start + length
        m.transport = "online"
        m.lastDelivery.Mark()
    end while
    if complete.Count() <> asset.encoded_len or AstrolabeSha256(complete) <> asset.sha256 then return invalid
    extension = ".bin"
    if asset.media_type = "image_png" then extension = ".png"
    if asset.media_type = "image_jpeg" then extension = ".jpg"
    if asset.media_type = "image_webp" then extension = ".webp"
    finalPath = "tmp:/astrolabe_" + Left(asset.id, 24) + extension
    if not complete.WriteFile(finalPath) then return invalid
    return { uri: finalPath, width: asset.width, height: asset.height, bytes: asset.encoded_len }
end function

function AstrolabeMediaType(kind as string) as string
    if kind = "image_png" then return "image/png"
    if kind = "image_jpeg" then return "image/jpeg"
    if kind = "image_webp" then return "image/webp"
    return "application/octet-stream"
end function

sub AstrolabePair()
    instance = AstrolabeJson("/head/v1/instance", "GET", invalid)
    if instance = invalid or instance.status <> 200 or instance.json = invalid
        AstrolabePublish({ kind: "message", title: "Coordinator unavailable", body: "Astrolabe will retry from a fresh trust ceremony." })
        return
    end if
    offer = instance.json
    if not AstrolabeExactFields(offer, ["protocol_major", "instance", "label", "trust"]) or offer.protocol_major <> 1
        AstrolabePublish({ kind: "message", title: "Coordinator refused", body: "The endpoint does not speak Astrolabe Display 1." })
        return
    end if
    if not AstrolabeExactFields(offer.trust, ["kind", "origin"]) or offer.trust.kind <> "web_pki_origin" or offer.trust.origin <> m.origin
        AstrolabePublish({ kind: "message", title: "Trust profile unsupported", body: "This Roku build requires the named Web PKI coordinator." })
        return
    end if

    nonce = AstrolabeRandomHex()
    pollKey = AstrolabeRandomHex()
    response = AstrolabeJson("/head/v1/pairings", "POST", {
        protocol_major: 1,
        receiver_nonce: nonce,
        poll_key: pollKey,
        rendezvous: invalid,
        capabilities: AstrolabeCapabilities()
    })
    if response = invalid or response.status <> 200 or response.json = invalid then return
    pairing = response.json
    if not AstrolabeExactFields(pairing, ["protocol_major", "pairing", "expires_in_ms", "confirmation_phrase", "coordinator_fingerprint"]) then return
    if not AstrolabeIsHex(pairing.pairing, 32) or not AstrolabeIsHex(pairing.coordinator_fingerprint, 64) then return
    phrase = AstrolabeConfirmationPhrase(pairing.coordinator_fingerprint, pairing.pairing, nonce)
    if FormatJson(phrase) <> FormatJson(pairing.confirmation_phrase) then return
    m.credential = {
        mode: "pairing",
        origin: m.origin,
        pairing: pairing.pairing,
        receiverNonce: nonce,
        pollKey: pollKey,
        fingerprint: pairing.coordinator_fingerprint,
        phrase: phrase,
        userConfirmed: false
    }
    if not AstrolabeSaveCredential(m.credential) then return
    AstrolabeContinuePairing()
end sub

sub AstrolabeContinuePairing()
    AstrolabePublish({
        kind: "pairing",
        phrase: m.credential.phrase,
        fingerprint: m.credential.fingerprint,
        confirmed: m.credential.userConfirmed
    })
    while m.credential.mode = "pairing" and not m.credential.userConfirmed
        command = m.top.command
        if Left(command, 8) = "confirm:"
            m.credential.userConfirmed = true
            if not AstrolabeSaveCredential(m.credential) then return
            exit while
        else if Left(command, 7) = "cancel:"
            AstrolabeClearCredential()
            m.credential = invalid
            return
        end if
        Sleep(100)
    end while
    if m.credential = invalid then return
    AstrolabePublish({ kind: "pairing", phrase: m.credential.phrase, fingerprint: m.credential.fingerprint, confirmed: true })

    while m.credential.mode = "pairing"
        status = AstrolabeJson("/head/v1/pairings/status", "POST", {
            protocol_major: 1,
            pairing: m.credential.pairing,
            proof: AstrolabePairingStatusTag(m.credential.pollKey, m.credential.pairing)
        })
        if status = invalid
            Sleep(3000)
        else if status.json.kind = "pending"
            delayMs = status.json.retry_after_ms
            if delayMs < 1000 then delayMs = 1000
            if delayMs > 60000 then delayMs = 60000
            Sleep(delayMs)
        else if status.json.kind = "approved"
            if not AstrolabeIsHex(status.json.device, 32) or not AstrolabeIsHex(status.json.proof_key, 64) or not AstrolabeIsHex(status.json.enrollment_challenge, 64) then return
            m.credential = {
                mode: "enrolling",
                origin: m.origin,
                pairing: m.credential.pairing,
                device: status.json.device,
                proofKey: status.json.proof_key,
                enrollmentChallenge: status.json.enrollment_challenge
            }
            if not AstrolabeSaveCredential(m.credential) then return
            AstrolabeFinishEnrollment()
        else
            AstrolabePublish({ kind: "message", title: "Pairing stopped", body: "Approval was rejected or expired. Press Back and relaunch to begin again." })
            return
        end if
    end while
end sub

sub AstrolabeFinishEnrollment()
    response = AstrolabeJson("/head/v1/pairings/complete", "POST", {
        protocol_major: 1,
        pairing: m.credential.pairing,
        device: m.credential.device,
        enrollment_challenge: m.credential.enrollmentChallenge,
        proof: AstrolabePairingCompleteTag(m.credential.proofKey, m.credential.pairing, m.credential.device, m.credential.enrollmentChallenge)
    })
    if response = invalid or response.status <> 200 or response.json = invalid then return
    if response.json.kind <> "enrolled" and response.json.kind <> "already_enrolled" then return
    if response.json.device <> m.credential.device or not AstrolabeIsHex(response.json.next_challenge, 64) then return
    m.challenge = response.json.next_challenge
    m.credential = { mode: "paired", origin: m.origin, device: m.credential.device, proofKey: m.credential.proofKey }
    AstrolabeSaveCredential(m.credential)
end sub

function AstrolabeStageProgram(program as object) as dynamic
    if not AstrolabeVerifyProgram(program) then return invalid
    stage = {}
    stagedBytes = 0
    for each item in program.items
        if item.scene.kind = "frame" and not stage.DoesExist(item.scene.asset.id)
            stagedBytes = stagedBytes + item.scene.asset.encoded_len
            if stagedBytes > 50331648 then return invalid
            entry = AstrolabeAuthorizedAsset(item.scene.asset, program)
            if entry = invalid then return invalid
            stage[item.scene.asset.id] = entry
        end if
    end for
    return stage
end function

sub AstrolabeRetireStage(stage as dynamic)
    if stage = invalid then return
    filesystem = CreateObject("roFileSystem")
    for each id in stage
        filesystem.Delete(stage[id].uri)
    end for
end sub

sub AstrolabeAdoptCursor(playback as object)
    if m.program = invalid or playback.current_index < 0 or playback.current_index >= m.program.items.Count() then return
    if playback.cycle <> m.program.playback.cycle then return
    m.program.playback.current_index = playback.current_index
    m.program.playback.elapsed_ms = playback.elapsed_ms
    m.elapsedBase = playback.elapsed_ms
    m.playbackClock.Mark()
    AstrolabeRenderCurrent()
end sub

sub AstrolabeRenderCurrent()
    if m.program = invalid then return
    item = m.program.items[m.program.playback.current_index]
    source = m.program.program_state.kind
    if item.source_state.kind = "unavailable" or source = "unavailable"
        source = "unavailable"
    else if item.source_state.kind = "partial" or source = "partial"
        source = "partial"
    else
        source = "current"
    end if
    stale = m.lastDelivery.TotalMilliseconds() >= m.program.freshness.stale_after_ms
    if stale and m.program.freshness.on_stale = "blank"
        AstrolabePublish({ kind: "message", title: "Coordinator unavailable", body: "The assigned content is no longer eligible to remain on screen.", source: source, stale: true })
        return
    end if
    if item.scene.kind = "frame"
        entry = m.stage[item.scene.asset.id]
        AstrolabePublish({
            kind: "frame",
            uri: entry.uri,
            expectedWidth: entry.width,
            expectedHeight: entry.height,
            spokenSummary: item.spoken_summary,
            source: source,
            stale: stale
        })
    else
        AstrolabePublish({ kind: "message", title: "Receiver-owned state", body: item.scene.reason, source: source, stale: stale })
    end if
end sub

sub AstrolabeTickPlayback()
    if m.program = invalid then return
    item = m.program.items[m.program.playback.current_index]
    if item.duration_ms = invalid then return
    if AstrolabeCurrentPlayback().elapsedMs < item.duration_ms then return
    nextIndex = m.program.playback.current_index + 1
    if nextIndex >= m.program.items.Count()
        if m.program.playback.cycle = "loop"
            nextIndex = 0
        else if m.program.playback.cycle = "blank_at_end"
            AstrolabePublish({ kind: "message", title: "Program complete", body: "Astrolabe is waiting for a newer assigned program." })
            return
        else if m.program.playback.cycle = "poll_at_end"
            AstrolabeRetireStage(m.stage)
            m.program = invalid
            m.stage = {}
            return
        else
            return
        end if
    end if
    m.program.playback.current_index = nextIndex
    m.program.playback.elapsed_ms = 0
    m.elapsedBase = 0
    m.playbackClock.Mark()
    AstrolabeRenderCurrent()
end sub

sub AstrolabeHandleProgramResponse(response as dynamic)
    if response = invalid or response.json = invalid or not response.json.DoesExist("kind") then return
    body = response.json
    if body.kind = "snapshot"
        if not AstrolabeExactFields(body, ["kind", "program"]) then return
        if m.program <> invalid and m.program.revision = body.program.revision
            m.program = body.program
            AstrolabeAdoptCursor(body.program.playback)
            return
        end if
        staged = AstrolabeStageProgram(body.program)
        if staged = invalid
            AstrolabePublish({ kind: "message", title: "Program refused", body: "The assigned program or one of its assets failed verification." })
            return
        end if
        previous = m.stage
        m.stage = staged
        m.program = body.program
        AstrolabeAdoptCursor(body.program.playback)
        AstrolabeRetireStage(previous)
    else if body.kind = "no_change"
        if m.program <> invalid and body.revision = m.program.revision then AstrolabeAdoptCursor(body.playback)
    else if body.kind = "unassigned"
        AstrolabeRetireStage(m.stage)
        m.program = invalid
        m.stage = {}
        AstrolabePublish({ kind: "unassigned", device: m.credential.device })
    else if body.kind = "reset"
        AstrolabeRetireStage(m.stage)
        m.program = invalid
        m.stage = {}
    else if body.kind = "revoked"
        AstrolabeRetireStage(m.stage)
        m.program = invalid
        m.stage = {}
        m.challenge = invalid
        AstrolabePublish({ kind: "message", title: "This display was revoked", body: "Staged content has been cleared." })
    else if body.kind = "re_pair"
        AstrolabeRetireStage(m.stage)
        AstrolabeClearCredential()
        m.credential = invalid
        m.program = invalid
        m.stage = {}
        AstrolabePublish({ kind: "message", title: "Pairing is required again", body: "Coordinator trust or receiver identity changed." })
    end if
end sub

sub AstrolabeProgramLoop()
    capabilities = AstrolabeAuthorizedJson("capabilities", "POST", "/head/v1/capabilities", AstrolabeCapabilities())
    if capabilities = invalid or capabilities.status < 200 or capabilities.status >= 300 then return
    while m.credential <> invalid and m.credential.mode = "paired"
        if m.program = invalid
            response = AstrolabeAuthorizedJson("program_snapshot", "GET", "/head/v1/program", invalid)
        else
            response = AstrolabeAuthorizedJson("program_changes", "GET", "/head/v1/program/changes", invalid, { waitMs: 25000 }, 35000)
        end if
        if response = invalid
            m.transport = "offline"
            AstrolabeTickPlayback()
            Sleep(1000)
        else
            AstrolabeHandleProgramResponse(response)
            if m.program = invalid then Sleep(5000)
        end if
    end while
end sub

sub AstrolabeRun()
    m.origin = "https://nixiesoftware.com"
    m.port = CreateObject("roMessagePort")
    m.transport = "connecting"
    m.challenge = invalid
    m.program = invalid
    m.stage = {}
    m.elapsedBase = 0
    m.playbackClock = CreateObject("roTimespan")
    m.lastDelivery = CreateObject("roTimespan")
    m.playbackClock.Mark()
    m.lastDelivery.Mark()
    AstrolabePublish({ kind: "booting" })
    m.credential = AstrolabeLoadCredential()
    if m.credential <> invalid and m.credential.origin <> m.origin
        AstrolabePublish({ kind: "message", title: "Coordinator changed", body: "Stored receiver identity belongs to a different origin." })
        return
    end if
    if m.credential = invalid
        AstrolabePair()
        if m.credential = invalid then AstrolabePair()
    end if
    if m.credential = invalid then return
    if m.credential.mode = "pairing"
        AstrolabeContinuePairing()
    else if m.credential.mode = "enrolling"
        AstrolabeFinishEnrollment()
    end if
    if m.credential.mode = "paired" then AstrolabeProgramLoop()
end sub
